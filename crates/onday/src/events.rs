//! Console, network and dialog history per page, fed by BiDi event streams or,
//! on WebDriver Classic, by the in-page capture script.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use thirtyfour::bidi::events::{
    ContextCreated, ContextDestroyed, UserPromptClosed, UserPromptOpened,
};
use thirtyfour::bidi::{BidiEvent, BrowsingContextId};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::channel::BidiChannel;
use crate::proto::{
    BeforeRequestSentEvent, FetchErrorEvent, LogEntryEvent, ResponseCompletedEvent, WireHeader,
};
use crate::route::RouteTable;

const CONSOLE_CAP: usize = 5000;
const NETWORK_CAP: usize = 5000;

/// One console message or uncaught error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsoleMessage {
    /// Monotonic per page, so callers can ask for "everything after N".
    pub seq: u64,
    /// `debug`, `info`, `warn` or `error`.
    pub level: String,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub timestamp: u64,
    /// `url:line:column` of the call site, when the browser reports one.
    pub location: Option<String>,
}

/// An HTTP header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl From<&WireHeader> for Header {
    fn from(header: &WireHeader) -> Self {
        Header {
            name: header.name.clone(),
            value: header.value.as_text(),
        }
    }
}

/// One request and, once it settles, its response or failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkEntry {
    pub seq: u64,
    pub id: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub mime_type: Option<String>,
    pub error: Option<String>,
    pub started: u64,
    pub duration_ms: Option<u64>,
    pub request_headers: Vec<Header>,
    pub response_headers: Vec<Header>,
}

impl NetworkEntry {
    pub fn is_settled(&self) -> bool {
        self.status.is_some() || self.error.is_some()
    }
}

/// A JavaScript dialog waiting for an answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dialog {
    /// `alert`, `confirm`, `prompt`, `beforeunload`, or `unknown` on Classic.
    pub kind: String,
    pub message: String,
    pub default_value: Option<String>,
}

/// What a page does with a dialog nobody is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DialogPolicy {
    /// Dismiss it, as Playwright does.
    #[default]
    Dismiss,
    /// Accept it.
    Accept,
    /// Leave it open until answered through [`Page::handle_dialog`](crate::Page::handle_dialog).
    Leave,
}

#[derive(Debug, Default)]
struct History {
    console: VecDeque<ConsoleMessage>,
    network: VecDeque<NetworkEntry>,
    next_console: u64,
    next_network: u64,
}

/// The event history and live channels of one page.
#[derive(Debug)]
pub(crate) struct PageEvents {
    history: Mutex<History>,
    dialog: Mutex<Option<Dialog>>,
    pub(crate) policy: Mutex<DialogPolicy>,
    pub(crate) console_tx: broadcast::Sender<ConsoleMessage>,
    pub(crate) network_tx: broadcast::Sender<NetworkEntry>,
    pub(crate) dialog_tx: broadcast::Sender<Dialog>,
    pub(crate) routes: RouteTable,
}

impl PageEvents {
    pub(crate) fn new(policy: DialogPolicy) -> Arc<Self> {
        Arc::new(PageEvents {
            history: Mutex::new(History::default()),
            dialog: Mutex::new(None),
            policy: Mutex::new(policy),
            console_tx: broadcast::channel(1024).0,
            network_tx: broadcast::channel(1024).0,
            dialog_tx: broadcast::channel(16).0,
            routes: RouteTable::default(),
        })
    }

    fn lock_history(&self) -> std::sync::MutexGuard<'_, History> {
        self.history
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn push_console(
        &self,
        level: String,
        text: String,
        timestamp: u64,
        location: Option<String>,
    ) {
        let message = {
            let mut history = self.lock_history();
            history.next_console += 1;
            let message = ConsoleMessage {
                seq: history.next_console,
                level,
                text,
                timestamp,
                location,
            };
            history.console.push_back(message.clone());
            if history.console.len() > CONSOLE_CAP {
                history.console.pop_front();
            }
            message
        };
        // No receivers is the common case and not an error.
        if self.console_tx.receiver_count() > 0 {
            self.console_tx.send(message).ok();
        }
    }

    pub(crate) fn request_started(
        &self,
        id: String,
        method: String,
        url: String,
        started: u64,
        headers: Vec<Header>,
    ) {
        let mut history = self.lock_history();
        if history
            .network
            .iter()
            .any(|entry| entry.id == id && !entry.is_settled())
        {
            return;
        }
        history.next_network += 1;
        let seq = history.next_network;
        history.network.push_back(NetworkEntry {
            seq,
            id,
            method,
            url,
            status: None,
            status_text: None,
            mime_type: None,
            error: None,
            started,
            duration_ms: None,
            request_headers: headers,
            response_headers: Vec::new(),
        });
        if history.network.len() > NETWORK_CAP {
            history.network.pop_front();
        }
    }

    pub(crate) fn request_settled(
        &self,
        id: &str,
        finished: u64,
        settle: impl FnOnce(&mut NetworkEntry),
    ) {
        let settled = {
            let mut history = self.lock_history();
            let entry = history
                .network
                .iter_mut()
                .rev()
                .find(|entry| entry.id == id && !entry.is_settled());
            entry.map(|entry| {
                settle(entry);
                entry.duration_ms = Some(finished.saturating_sub(entry.started));
                entry.clone()
            })
        };
        if let Some(entry) = settled
            && self.network_tx.receiver_count() > 0
        {
            self.network_tx.send(entry).ok();
        }
    }

    /// Record a request the capture script saw complete in one piece.
    pub(crate) fn push_network(&self, mut entry: NetworkEntry) {
        let mut history = self.lock_history();
        history.next_network += 1;
        entry.seq = history.next_network;
        history.network.push_back(entry.clone());
        if history.network.len() > NETWORK_CAP {
            history.network.pop_front();
        }
        drop(history);
        if self.network_tx.receiver_count() > 0 {
            self.network_tx.send(entry).ok();
        }
    }

    pub(crate) fn console_after(&self, seq: u64) -> Vec<ConsoleMessage> {
        self.lock_history()
            .console
            .iter()
            .filter(|message| message.seq > seq)
            .cloned()
            .collect()
    }

    pub(crate) fn network_after(&self, seq: u64) -> Vec<NetworkEntry> {
        self.lock_history()
            .network
            .iter()
            .filter(|entry| entry.seq > seq)
            .cloned()
            .collect()
    }

    pub(crate) fn set_dialog(&self, dialog: Option<Dialog>) {
        let opened = dialog.clone();
        *self
            .dialog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = dialog;
        if let Some(dialog) = opened
            && self.dialog_tx.receiver_count() > 0
        {
            self.dialog_tx.send(dialog).ok();
        }
    }

    pub(crate) fn dialog(&self) -> Option<Dialog> {
        self.dialog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn policy(&self) -> DialogPolicy {
        *self
            .policy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Routes session-wide BiDi events to the page (top-level context) they belong to.
pub(crate) struct EventHub {
    bidi: BidiChannel,
    state: Mutex<HubState>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) created_tx: broadcast::Sender<ContextCreated>,
}

#[derive(Default)]
struct HubState {
    pages: HashMap<BrowsingContextId, Weak<PageEvents>>,
    parents: HashMap<BrowsingContextId, BrowsingContextId>,
}

impl EventHub {
    pub(crate) async fn start(channel: BidiChannel) -> anyhow::Result<Arc<EventHub>> {
        use anyhow::Context as _;
        let bidi = channel.raw().clone();
        let hub = Arc::new(EventHub {
            bidi: channel,
            state: Mutex::new(HubState::default()),
            tasks: Mutex::new(Vec::new()),
            created_tx: broadcast::channel(64).0,
        });
        let mut tasks = Vec::new();
        tasks.push(spawn_stream::<LogEntryEvent>(
            &hub,
            bidi.subscribe().await.context("subscribe log.entryAdded")?,
            |hub, event| hub.on_log(event),
        ));
        tasks.push(spawn_stream::<BeforeRequestSentEvent>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe network.beforeRequestSent")?,
            |hub, event| hub.on_request(event),
        ));
        tasks.push(spawn_stream::<ResponseCompletedEvent>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe network.responseCompleted")?,
            |hub, event| hub.on_response(event),
        ));
        tasks.push(spawn_stream::<FetchErrorEvent>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe network.fetchError")?,
            |hub, event| hub.on_fetch_error(event),
        ));
        tasks.push(spawn_stream::<UserPromptOpened>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe browsingContext.userPromptOpened")?,
            |hub, event| hub.on_prompt_opened(event),
        ));
        tasks.push(spawn_stream::<UserPromptClosed>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe browsingContext.userPromptClosed")?,
            |hub, event| hub.on_prompt_closed(event),
        ));
        tasks.push(spawn_stream::<ContextCreated>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe browsingContext.contextCreated")?,
            |hub, event| hub.on_context_created(event),
        ));
        tasks.push(spawn_stream::<ContextDestroyed>(
            &hub,
            bidi.subscribe()
                .await
                .context("subscribe browsingContext.contextDestroyed")?,
            |hub, event| hub.on_context_destroyed(event),
        ));
        *hub.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = tasks;
        Ok(hub)
    }

    pub(crate) fn register(&self, context: BrowsingContextId, events: &Arc<PageEvents>) {
        self.lock().pages.insert(context, Arc::downgrade(events));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn page_for(
        &self,
        context: Option<&BrowsingContextId>,
    ) -> Option<(BrowsingContextId, Arc<PageEvents>)> {
        let state = self.lock();
        let mut current = context?.clone();
        for _ in 0..32 {
            if let Some(events) = state.pages.get(&current).and_then(Weak::upgrade) {
                return Some((current, events));
            }
            current = state.parents.get(&current)?.clone();
        }
        None
    }

    fn on_log(self: &Arc<Self>, event: LogEntryEvent) {
        let Some((_, page)) = self.page_for(event.source.context.as_ref()) else {
            return;
        };
        let location = event
            .stack_trace
            .as_ref()
            .and_then(|trace| trace.call_frames.first())
            .map(|frame| {
                format!(
                    "{}:{}:{}",
                    frame.url,
                    frame.line_number + 1,
                    frame.column_number + 1
                )
            });
        let mut text = event.text.unwrap_or_default();
        if event.kind == "javascript" && !text.starts_with("Uncaught") {
            text = format!("Uncaught {text}");
        }
        page.push_console(event.level, text, event.timestamp, location);
    }

    fn on_request(self: &Arc<Self>, event: BeforeRequestSentEvent) {
        let Some((context, page)) = self.page_for(event.context.as_ref()) else {
            return;
        };
        page.request_started(
            event.request.request.as_str().to_string(),
            event.request.method.clone(),
            event.request.url.clone(),
            event.timestamp,
            event.request.headers.iter().map(Header::from).collect(),
        );
        if event.is_blocked {
            crate::route::dispatch(self.bidi.clone(), context, page, event);
        }
    }

    fn on_response(self: &Arc<Self>, event: ResponseCompletedEvent) {
        let Some((_, page)) = self.page_for(event.context.as_ref()) else {
            return;
        };
        let response = event.response;
        page.request_settled(event.request.request.as_str(), event.timestamp, |entry| {
            entry.status = Some(response.status);
            entry.status_text = Some(response.status_text);
            entry.mime_type = response.mime_type;
            entry.response_headers = response.headers.iter().map(Header::from).collect();
        });
    }

    fn on_fetch_error(self: &Arc<Self>, event: FetchErrorEvent) {
        let Some((_, page)) = self.page_for(event.context.as_ref()) else {
            return;
        };
        let error = event.error_text;
        page.request_settled(event.request.request.as_str(), event.timestamp, |entry| {
            entry.error = Some(error);
        });
    }

    fn on_prompt_opened(self: &Arc<Self>, event: UserPromptOpened) {
        let Some((_, page)) = self.page_for(Some(&event.context)) else {
            return;
        };
        page.set_dialog(Some(Dialog {
            kind: event.prompt_type.clone(),
            message: event.message.clone(),
            default_value: event.default_value.clone(),
        }));
        let accept = match page.policy() {
            DialogPolicy::Leave => return,
            DialogPolicy::Accept => true,
            DialogPolicy::Dismiss => false,
        };
        let bidi = self.bidi.clone();
        tokio::spawn(async move {
            let answered = bidi
                .send(
                    thirtyfour::bidi::modules::browsing_context::HandleUserPrompt {
                        context: event.context,
                        accept: Some(accept),
                        user_text: None,
                    },
                )
                .await;
            if let Err(error) = answered {
                tracing::debug!("auto-answering a dialog failed: {error:#}");
            }
        });
    }

    fn on_prompt_closed(self: &Arc<Self>, event: UserPromptClosed) {
        if let Some((_, page)) = self.page_for(Some(&event.context)) {
            page.set_dialog(None);
        }
    }

    fn on_context_created(self: &Arc<Self>, event: ContextCreated) {
        if let Some(parent) = &event.0.parent {
            self.lock()
                .parents
                .insert(event.0.context.clone(), parent.clone());
        }
        if self.created_tx.receiver_count() > 0 {
            self.created_tx.send(event).ok();
        }
    }

    fn on_context_destroyed(self: &Arc<Self>, event: ContextDestroyed) {
        let mut state = self.lock();
        state.parents.remove(&event.0.context);
        state.pages.remove(&event.0.context);
    }
}

impl Drop for EventHub {
    fn drop(&mut self) {
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

fn spawn_stream<E: BidiEvent>(
    hub: &Arc<EventHub>,
    mut stream: thirtyfour::bidi::EventStream<E>,
    handle: fn(&Arc<EventHub>, E),
) -> JoinHandle<()> {
    let weak = Arc::downgrade(hub);
    tokio::spawn(async move {
        while let Some(event) = stream.next().await {
            let Some(hub) = weak.upgrade() else {
                return;
            };
            handle(&hub, event);
        }
    })
}
