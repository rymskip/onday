//! A page (tab): navigation, scripting, input, screenshots, events and dialogs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use thirtyfour::bidi::modules::browsing_context::{
    Activate, Close, GetTree, HandleUserPrompt, Navigate, ReadinessState, Reload, SetViewport,
    TraverseHistory, Viewport,
};
use thirtyfour::bidi::modules::network::{AddIntercept, InterceptPhase, RemoveIntercept};
use thirtyfour::bidi::modules::script::Target;
use thirtyfour::bidi::{BiDi, BrowsingContextId, NodeId};
use thirtyfour::{WebDriver, WebElement, WindowHandle};
use tokio::sync::broadcast;

use crate::context::ContextInner;
use crate::events::{ConsoleMessage, Dialog, DialogPolicy, NetworkEntry, PageEvents};
use crate::hooks::AppHooks;
use crate::js;
use crate::keys::{Chord, key_value, parse_chord};
use crate::launch::Protocol;
use crate::locator::Locator;
use crate::poll::{Backoff, poll_until};
use crate::proto::{
    CallFunction, CallResult, CaptureScreenshot, KeyAction, LocalValue, PerformActions,
    PointerAction, PointerParameters, RemoteValue, ScreenshotClip, SharedRef, SourceActions,
    WheelAction,
};
use crate::route::RouteHandler;

/// Default budget for actions and waits.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageTarget {
    Bidi(BrowsingContextId),
    Classic(WindowHandle),
}

/// A DOM element handle in the page's protocol.
#[derive(Debug, Clone)]
pub(crate) enum ElementRef {
    Bidi(String),
    Classic(WebElement),
}

/// An argument passed into page scripts.
#[derive(Debug, Clone)]
pub(crate) enum JsArg {
    Str(String),
    Bool(bool),
    Null,
    Strs(Vec<String>),
    El(ElementRef),
}

impl JsArg {
    fn to_local(&self) -> Result<LocalValue> {
        Ok(match self {
            JsArg::Str(value) => LocalValue::String {
                value: value.clone(),
            },
            JsArg::Bool(value) => LocalValue::Boolean { value: *value },
            JsArg::Null => LocalValue::Null,
            JsArg::Strs(values) => LocalValue::Array {
                value: values
                    .iter()
                    .map(|value| LocalValue::String {
                        value: value.clone(),
                    })
                    .collect(),
            },
            JsArg::El(ElementRef::Bidi(shared_id)) => LocalValue::Shared(SharedRef {
                shared_id: shared_id.clone(),
            }),
            JsArg::El(ElementRef::Classic(_)) => {
                bail!("a Classic element handle cannot cross into a BiDi call")
            }
        })
    }

    fn to_classic(&self) -> Result<serde_json::Value> {
        match self {
            JsArg::Str(value) => serde_json::to_value(value).context("serialize script argument"),
            JsArg::Bool(value) => serde_json::to_value(value).context("serialize script argument"),
            JsArg::Null => serde_json::to_value(()).context("serialize script argument"),
            JsArg::Strs(values) => {
                serde_json::to_value(values).context("serialize script argument")
            }
            JsArg::El(ElementRef::Classic(element)) => {
                element.to_json().context("serialize element argument")
            }
            JsArg::El(ElementRef::Bidi(_)) => {
                bail!("a BiDi element handle cannot cross into a Classic call")
            }
        }
    }
}

/// When a navigation counts as finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitUntil {
    /// The new document has been committed.
    Commit,
    /// `DOMContentLoaded` fired.
    DomContentLoaded,
    /// `load` fired.
    #[default]
    Load,
    /// The app's [`AppHooks::ready_script`] holds; falls back to `Load` without one.
    Ready,
}

/// Mouse buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        }
    }
}

/// What to capture.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenshotOptions {
    /// The whole scrollable document rather than the viewport (BiDi only).
    pub full_page: bool,
}

pub(crate) struct PageInner {
    pub(crate) context: Arc<ContextInner>,
    pub(crate) target: PageTarget,
    pub(crate) events: Arc<PageEvents>,
    timeout: Mutex<Duration>,
    closed: AtomicBool,
}

/// A tab in a [`BrowserContext`](crate::BrowserContext).
#[derive(Clone)]
pub struct Page {
    pub(crate) inner: Arc<PageInner>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("target", &self.inner.target)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct Drained {
    console: Vec<CapturedConsole>,
    network: Vec<CapturedRequest>,
}

#[derive(Debug, Deserialize)]
struct CapturedConsole {
    level: String,
    text: String,
    time: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapturedRequest {
    method: String,
    url: String,
    status: Option<u16>,
    error: Option<String>,
    duration_ms: u64,
    time: u64,
}

impl Page {
    pub(crate) async fn attach(context: Arc<ContextInner>, target: PageTarget) -> Result<Page> {
        let events = PageEvents::new(context.options.dialog_policy);
        if let (PageTarget::Bidi(id), Some(hub)) = (&target, &context.session.hub) {
            hub.register(id.clone(), &events);
        }
        let page = Page {
            inner: Arc::new(PageInner {
                context,
                target,
                events,
                timeout: Mutex::new(DEFAULT_TIMEOUT),
                closed: AtomicBool::new(false),
            }),
        };
        if let Some((width, height)) = page.inner.context.options.viewport {
            page.set_viewport(width, height).await?;
        }
        Ok(page)
    }

    // ── accessors ──────────────────────────────────────────────────────────

    pub fn protocol(&self) -> Protocol {
        self.inner.context.session.protocol()
    }

    pub fn hooks(&self) -> &Arc<dyn AppHooks> {
        &self.inner.context.hooks
    }

    /// The raw thirtyfour driver behind this page.
    pub fn driver(&self) -> &WebDriver {
        &self.inner.context.session.driver
    }

    /// The raw BiDi connection, when negotiated.
    pub fn bidi(&self) -> Option<&BiDi> {
        self.inner.context.session.bidi.as_ref()
    }

    /// The BiDi browsing context id of this page.
    pub fn context_id(&self) -> Option<&BrowsingContextId> {
        match &self.inner.target {
            PageTarget::Bidi(id) => Some(id),
            PageTarget::Classic(_) => None,
        }
    }

    pub fn default_timeout(&self) -> Duration {
        *self
            .inner
            .timeout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_default_timeout(&self, timeout: Duration) {
        *self
            .inner
            .timeout
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = timeout;
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    // ── locators ───────────────────────────────────────────────────────────

    /// A lazy locator for `selector` (CSS, or `text=`, `role=`, `testid=`, `ref=` … joined by `>>`).
    pub fn locator(&self, selector: impl Into<String>) -> Locator {
        Locator::new(self.clone(), selector.into())
    }

    pub fn get_by_test_id(&self, id: &str) -> Locator {
        self.locator(format!("testid={}", quote(id)))
    }

    pub fn get_by_text(&self, text: &str) -> Locator {
        self.locator(format!("text={text}"))
    }

    pub fn get_by_exact_text(&self, text: &str) -> Locator {
        self.locator(format!("text={}", quote(text)))
    }

    pub fn get_by_role(&self, role: &str, name: Option<&str>) -> Locator {
        match name {
            Some(name) => self.locator(format!("role={role}[name={}]", quote(name))),
            None => self.locator(format!("role={role}")),
        }
    }

    pub fn get_by_label(&self, text: &str) -> Locator {
        self.locator(format!("label={text}"))
    }

    pub fn get_by_placeholder(&self, text: &str) -> Locator {
        self.locator(format!("placeholder={text}"))
    }

    /// The element a snapshot labelled `[ref=…]`.
    pub fn get_by_ref(&self, reference: &str) -> Locator {
        self.locator(format!("ref={reference}"))
    }

    // ── navigation ─────────────────────────────────────────────────────────

    /// Navigate to `url` (relative URLs resolve against the context's `base_url`).
    pub async fn goto(&self, url: &str, wait: WaitUntil) -> Result<()> {
        let url = match (&self.inner.context.options.base_url, url.contains("://")) {
            (Some(base), false) => format!(
                "{}/{}",
                base.trim_end_matches('/'),
                url.trim_start_matches('/')
            ),
            _ => url.to_string(),
        };
        let ready = match wait {
            WaitUntil::Ready => self.hooks().ready_script(),
            _ => None,
        };
        if ready.is_some() {
            self.stamp_navigation().await;
        }
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let readiness = match (wait, &ready) {
                    (WaitUntil::Commit, _) | (WaitUntil::Ready, Some(_)) => ReadinessState::None,
                    (WaitUntil::DomContentLoaded, _) => ReadinessState::Interactive,
                    (WaitUntil::Load, _) | (WaitUntil::Ready, None) => ReadinessState::Complete,
                };
                self.bidi_handle()?
                    .send(Navigate {
                        context: context.clone(),
                        url: url.clone(),
                        wait: Some(readiness),
                    })
                    .await
                    .with_context(|| format!("navigate to {url}"))?;
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                self.driver()
                    .goto(url.as_str())
                    .await
                    .with_context(|| format!("navigate to {url}"))?;
                drop(window);
            }
        }
        if let Some(script) = ready {
            self.wait_for_ready(&script, self.default_timeout()).await?;
        }
        Ok(())
    }

    /// Mark the current document so a readiness poll cannot read it after navigation starts.
    async fn stamp_navigation(&self) {
        let stamped = self
            .evaluate::<bool>("(() => { window.__ondayNavPending = true; return true; })()")
            .await;
        if let Err(error) = stamped {
            tracing::debug!("no document to stamp before navigating: {error:#}");
        }
    }

    async fn wait_for_ready(&self, script: &str, timeout: Duration) -> Result<()> {
        let guarded =
            format!("() => {{ if (window.__ondayNavPending === true) return false; {script} }}");
        let start = Instant::now();
        let held = poll_until(
            || async { self.evaluate::<bool>(&guarded).await.unwrap_or(false) },
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
        if !held {
            bail!("the app did not report ready within {:?}", start.elapsed());
        }
        Ok(())
    }

    pub async fn reload(&self) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(Reload {
                        context: context.clone(),
                        ignore_cache: None,
                        wait: Some(ReadinessState::Complete),
                    })
                    .await
                    .context("browsingContext.reload")?;
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                self.driver().refresh().await.context("reload")?;
                drop(window);
            }
        }
        Ok(())
    }

    pub async fn go_back(&self) -> Result<()> {
        self.traverse(-1).await
    }

    pub async fn go_forward(&self) -> Result<()> {
        self.traverse(1).await
    }

    async fn traverse(&self, delta: i32) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(TraverseHistory {
                        context: context.clone(),
                        delta,
                    })
                    .await
                    .context("browsingContext.traverseHistory")?;
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                if delta < 0 {
                    self.driver().back().await.context("go back")?;
                } else {
                    self.driver().forward().await.context("go forward")?;
                }
                drop(window);
            }
        }
        Ok(())
    }

    pub async fn url(&self) -> Result<String> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let tree = self
                    .bidi_handle()?
                    .send(GetTree {
                        max_depth: Some(0),
                        root: Some(context.clone()),
                    })
                    .await
                    .context("browsingContext.getTree")?;
                tree.contexts
                    .into_iter()
                    .next()
                    .map(|info| info.url)
                    .context("the page's browsing context is gone")
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                let url = self.driver().current_url().await.context("read the URL")?;
                drop(window);
                Ok(url.to_string())
            }
        }
    }

    pub async fn title(&self) -> Result<String> {
        self.evaluate("document.title").await
    }

    /// The document's serialized HTML.
    pub async fn content(&self) -> Result<String> {
        self.evaluate("document.documentElement ? document.documentElement.outerHTML : ''")
            .await
    }

    /// Poll until the URL matches `glob` (see [`glob_matches`](crate::route::glob_matches)).
    pub async fn wait_for_url(&self, glob: &str, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let matched = poll_until(
            || async {
                self.url()
                    .await
                    .is_ok_and(|url| crate::route::glob_matches(glob, &url))
            },
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
        if matched {
            return Ok(());
        }
        let url = self.url().await.unwrap_or_default();
        bail!(
            "URL {url} did not match {glob} within {:?}",
            start.elapsed()
        )
    }

    // ── scripting ──────────────────────────────────────────────────────────

    /// Evaluate an expression, or call a function, and deserialize its JSON-able result.
    pub async fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        self.evaluate_with(expression, Vec::new()).await
    }

    pub(crate) async fn evaluate_with<T: DeserializeOwned>(
        &self,
        expression: &str,
        args: Vec<JsArg>,
    ) -> Result<T> {
        let text = self
            .call_text(|prelude| js::user_call(expression, prelude), args)
            .await
            .with_context(|| format!("evaluate {}", abbreviate(expression)))?;
        serde_json::from_str(&text)
            .with_context(|| format!("decode the result of {}: {text}", abbreviate(expression)))
    }

    /// Poll until `expression` is truthy.
    pub async fn wait_for_function(&self, expression: &str, timeout: Duration) -> Result<()> {
        let check = format!(
            "(async () => {{ const v = await (\n{expression}\n); return !!(typeof v === 'function' ? await v() : v); }})()"
        );
        let start = Instant::now();
        let held = poll_until(
            || async { self.evaluate::<bool>(&check).await.unwrap_or(false) },
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
        if !held {
            bail!(
                "{} stayed falsy for {:?}",
                abbreviate(expression),
                start.elapsed()
            );
        }
        Ok(())
    }

    /// Wait until no tracked write (see [`AppHooks::is_tracked_write_js`]) is in flight.
    pub async fn wait_for_writes(&self, timeout: Duration) -> Result<()> {
        self.wait_for_function("!window.__ondayWritesInFlight", timeout)
            .await
    }

    /// Call `func(lib, ...args)` in the page runtime and decode its JSON result.
    pub(crate) async fn call_json<T: DeserializeOwned>(
        &self,
        func: &str,
        args: Vec<JsArg>,
    ) -> Result<T> {
        let text = self
            .call_text(|prelude| js::json_call(func, prelude), args)
            .await?;
        serde_json::from_str(&text).with_context(|| format!("decode page runtime result {text}"))
    }

    async fn call_text(
        &self,
        declare: impl Fn(&str) -> String,
        args: Vec<JsArg>,
    ) -> Result<String> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let arguments = args
                    .iter()
                    .map(JsArg::to_local)
                    .collect::<Result<Vec<_>>>()?;
                let result = self
                    .bidi_handle()?
                    .send(CallFunction {
                        function_declaration: declare(""),
                        await_promise: true,
                        target: Target::Context {
                            context: context.clone(),
                            sandbox: None,
                        },
                        arguments,
                        user_activation: Some(true),
                    })
                    .await
                    .context("script.callFunction")?;
                match result {
                    CallResult::Success {
                        result: RemoteValue::String { value },
                    } => Ok(value),
                    CallResult::Success { result } => {
                        bail!("expected JSON text from the page, got {result:?}")
                    }
                    CallResult::Exception { exception_details } => bail!(
                        "page script threw at line {}: {}",
                        exception_details.line_number.unwrap_or(0),
                        exception_details.text
                    ),
                }
            }
            PageTarget::Classic(_) => {
                let arguments = args
                    .iter()
                    .map(JsArg::to_classic)
                    .collect::<Result<Vec<_>>>()?;
                let window = self.classic_window().await?;
                let prelude = self.classic_prelude();
                let returned = self
                    .driver()
                    .execute(js::classic_body(&declare(&prelude)), arguments)
                    .await
                    .context("execute script")?;
                drop(window);
                returned
                    .convert::<String>()
                    .context("read the script result")
            }
        }
    }

    /// Call `func(lib, ...args)` in the page runtime and return the elements it yields.
    pub(crate) async fn call_elements(
        &self,
        func: &str,
        args: Vec<JsArg>,
    ) -> Result<Vec<ElementRef>> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let arguments = args
                    .iter()
                    .map(JsArg::to_local)
                    .collect::<Result<Vec<_>>>()?;
                let result = self
                    .bidi_handle()?
                    .send(CallFunction {
                        function_declaration: js::elements_call(func, ""),
                        await_promise: true,
                        target: Target::Context {
                            context: context.clone(),
                            sandbox: None,
                        },
                        arguments,
                        user_activation: None,
                    })
                    .await
                    .context("script.callFunction")?;
                match result {
                    CallResult::Success {
                        result: RemoteValue::Array { value },
                    } => value
                        .into_iter()
                        .map(|item| match item {
                            RemoteValue::Node {
                                shared_id: Some(shared_id),
                            } => Ok(ElementRef::Bidi(shared_id)),
                            other => Err(anyhow!("expected an element, got {other:?}")),
                        })
                        .collect(),
                    CallResult::Success { result } => {
                        bail!("expected an element array, got {result:?}")
                    }
                    CallResult::Exception { exception_details } => bail!(
                        "page script threw at line {}: {}",
                        exception_details.line_number.unwrap_or(0),
                        exception_details.text
                    ),
                }
            }
            PageTarget::Classic(_) => {
                let arguments = args
                    .iter()
                    .map(JsArg::to_classic)
                    .collect::<Result<Vec<_>>>()?;
                let window = self.classic_window().await?;
                let prelude = self.classic_prelude();
                let returned = self
                    .driver()
                    .execute(
                        js::classic_body(&js::elements_call(func, &prelude)),
                        arguments,
                    )
                    .await
                    .context("execute script")?;
                drop(window);
                Ok(returned
                    .elements()
                    .context("read the returned elements")?
                    .into_iter()
                    .map(ElementRef::Classic)
                    .collect())
            }
        }
    }

    /// Classic has no preload scripts: every call first ensures the runtime, the
    /// capture hooks and the init scripts are in the current document.
    fn classic_prelude(&self) -> String {
        let mut scripts = self.inner.context.init_scripts();
        scripts.push("window.__onday.installCapture();".to_string());
        format!("{}\n{}", js::runtime_guard(), js::init_prelude(&scripts))
    }

    // ── aria snapshot ──────────────────────────────────────────────────────

    /// An accessibility-tree snapshot with `[ref=…]` labels usable in `ref=` selectors.
    pub async fn snapshot(&self) -> Result<String> {
        self.call_json("(lib) => lib.snapshot(null, 5000)", Vec::new())
            .await
            .context("take an aria snapshot")
    }

    // ── input ──────────────────────────────────────────────────────────────

    pub fn keyboard(&self) -> Keyboard<'_> {
        Keyboard { page: self }
    }

    pub fn mouse(&self) -> Mouse<'_> {
        Mouse { page: self }
    }

    /// Dispatch input over BiDi. A dialog opened by the input blocks the command
    /// until it is answered, so a dialog appearing also counts as completion.
    async fn perform(
        &self,
        context: &BrowsingContextId,
        source: SourceActions,
        what: &str,
    ) -> Result<()> {
        let mut dialogs = self.inner.events.dialog_tx.subscribe();
        if self.inner.events.dialog().is_some() {
            bail!("a dialog is open; answer it before sending {what}");
        }
        let command = self.bidi_handle()?.send(PerformActions {
            context: context.clone(),
            actions: vec![source],
        });
        tokio::select! {
            sent = command => {
                sent.with_context(|| format!("input.performActions ({what})"))?;
                Ok(())
            }
            opened = dialogs.recv() => {
                if let Ok(dialog) = opened {
                    tracing::debug!("{what} opened a {} dialog", dialog.kind);
                }
                Ok(())
            }
        }
    }

    pub(crate) async fn pointer(&self, actions: Vec<PointerAction>) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let source = SourceActions::Pointer {
                    id: "onday-mouse".to_string(),
                    parameters: PointerParameters {
                        pointer_type: "mouse",
                    },
                    actions,
                };
                self.perform(context, source, "pointer").await
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                let mut chain = self.driver().action_chain();
                for action in actions {
                    chain = match action {
                        PointerAction::PointerMove { x, y, .. } => chain.move_to(x, y),
                        PointerAction::PointerDown { button: 0 } => chain.click_and_hold(),
                        PointerAction::PointerUp { button: 0 } => chain.release(),
                        PointerAction::Pause { duration } => {
                            chain.perform().await.context("perform pointer actions")?;
                            tokio::time::sleep(Duration::from_millis(duration)).await;
                            self.driver().action_chain()
                        }
                        PointerAction::PointerDown { button }
                        | PointerAction::PointerUp { button } => {
                            bail!("mouse button {button} needs a BiDi session")
                        }
                    };
                }
                chain.perform().await.context("perform pointer actions")?;
                drop(window);
                Ok(())
            }
        }
    }

    pub(crate) async fn click_at(
        &self,
        x: i64,
        y: i64,
        button: MouseButton,
        count: u32,
        modifiers: &[String],
    ) -> Result<()> {
        if let PageTarget::Classic(_) = &self.inner.target
            && button != MouseButton::Left
        {
            if button == MouseButton::Right && count == 1 && modifiers.is_empty() {
                let window = self.classic_window().await?;
                self.driver()
                    .action_chain()
                    .move_to(x, y)
                    .context_click()
                    .perform()
                    .await
                    .context("right-click")?;
                drop(window);
                return Ok(());
            }
            bail!("{button:?}-button clicks need a BiDi session");
        }
        self.keys(
            modifiers
                .iter()
                .map(|key| KeyAction::KeyDown { value: key.clone() })
                .collect(),
        )
        .await?;
        let mut actions = vec![PointerAction::PointerMove {
            x,
            y,
            duration: None,
            origin: "viewport",
        }];
        for _ in 0..count.max(1) {
            actions.push(PointerAction::PointerDown {
                button: button.code(),
            });
            actions.push(PointerAction::PointerUp {
                button: button.code(),
            });
        }
        let clicked = self.pointer(actions).await;
        self.keys(
            modifiers
                .iter()
                .rev()
                .map(|key| KeyAction::KeyUp { value: key.clone() })
                .collect(),
        )
        .await?;
        clicked
    }

    pub(crate) async fn keys(&self, actions: Vec<KeyAction>) -> Result<()> {
        if actions.is_empty() {
            return Ok(());
        }
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let source = SourceActions::Key {
                    id: "onday-keyboard".to_string(),
                    actions,
                };
                self.perform(context, source, "keys").await
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                let mut chain = self.driver().action_chain();
                for action in actions {
                    chain = match action {
                        KeyAction::KeyDown { value } => chain.key_down(single_char(&value)?),
                        KeyAction::KeyUp { value } => chain.key_up(single_char(&value)?),
                    };
                }
                chain.perform().await.context("perform key actions")?;
                drop(window);
                Ok(())
            }
        }
    }

    pub(crate) async fn wheel(&self, x: i64, y: i64, delta_x: i64, delta_y: i64) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let source = SourceActions::Wheel {
                    id: "onday-wheel".to_string(),
                    actions: vec![WheelAction::Scroll {
                        x,
                        y,
                        delta_x,
                        delta_y,
                        origin: "viewport",
                    }],
                };
                self.perform(context, source, "wheel").await
            }
            PageTarget::Classic(_) => self
                .evaluate::<bool>(&format!(
                    "(() => {{ window.scrollBy({delta_x}, {delta_y}); return true; }})()"
                ))
                .await
                .map(|_| ()),
        }
    }

    pub(crate) async fn set_files(&self, element: &ElementRef, files: Vec<String>) -> Result<()> {
        match (element, &self.inner.target) {
            (ElementRef::Bidi(shared_id), PageTarget::Bidi(context)) => {
                self.bidi_handle()?
                    .input()
                    .set_files(context.clone(), &NodeId::from(shared_id.clone()), files)
                    .await
                    .context("input.setFiles")?;
                Ok(())
            }
            (ElementRef::Classic(element), PageTarget::Classic(_)) => {
                let window = self.classic_window().await?;
                element
                    .send_keys(files.join("\n"))
                    .await
                    .context("send file paths to the input")?;
                drop(window);
                Ok(())
            }
            _ => bail!("element handle does not belong to this page's protocol"),
        }
    }

    // ── screenshots ────────────────────────────────────────────────────────

    /// PNG bytes of the viewport or, with `full_page`, the whole document.
    pub async fn screenshot(&self, options: ScreenshotOptions) -> Result<Vec<u8>> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                let shot = self
                    .bidi_handle()?
                    .send(CaptureScreenshot {
                        context: context.clone(),
                        origin: if options.full_page {
                            "document"
                        } else {
                            "viewport"
                        },
                        clip: None,
                    })
                    .await
                    .context("browsingContext.captureScreenshot")?;
                decode_png(&shot.data)
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                let png = self
                    .driver()
                    .screenshot_as_png()
                    .await
                    .context("take a screenshot")?;
                drop(window);
                Ok(png)
            }
        }
    }

    pub(crate) async fn element_screenshot(&self, element: &ElementRef) -> Result<Vec<u8>> {
        match (element, &self.inner.target) {
            (ElementRef::Bidi(shared_id), PageTarget::Bidi(context)) => {
                let shot = self
                    .bidi_handle()?
                    .send(CaptureScreenshot {
                        context: context.clone(),
                        origin: "document",
                        clip: Some(ScreenshotClip::Element {
                            element: SharedRef {
                                shared_id: shared_id.clone(),
                            },
                        }),
                    })
                    .await
                    .context("browsingContext.captureScreenshot (element)")?;
                decode_png(&shot.data)
            }
            (ElementRef::Classic(element), PageTarget::Classic(_)) => {
                let window = self.classic_window().await?;
                let png = element
                    .screenshot_as_png()
                    .await
                    .context("screenshot the element")?;
                drop(window);
                Ok(png)
            }
            _ => bail!("element handle does not belong to this page's protocol"),
        }
    }

    // ── viewport and lifecycle ─────────────────────────────────────────────

    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(SetViewport {
                        context: context.clone(),
                        viewport: Some(Viewport { width, height }),
                        device_pixel_ratio: None,
                    })
                    .await
                    .context("browsingContext.setViewport")?;
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                self.driver()
                    .set_window_rect(0, 0, width, height)
                    .await
                    .context("size the window")?;
                drop(window);
            }
        }
        Ok(())
    }

    /// The viewport's CSS size.
    pub async fn viewport(&self) -> Result<(u32, u32)> {
        self.evaluate("[window.innerWidth, window.innerHeight]")
            .await
    }

    pub async fn bring_to_front(&self) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(Activate {
                        context: context.clone(),
                    })
                    .await
                    .context("browsingContext.activate")?;
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                drop(window);
            }
        }
        Ok(())
    }

    pub async fn close(&self) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(Close {
                        context: context.clone(),
                        prompt_unload: None,
                    })
                    .await
                    .context("browsingContext.close")?;
            }
            PageTarget::Classic(handle) => {
                let mut current = self.classic_window().await?;
                self.driver()
                    .close_window()
                    .await
                    .context("close the window")?;
                *current = None;
                drop(current);
                if let Some(next) = self
                    .driver()
                    .windows()
                    .await
                    .context("list windows")?
                    .into_iter()
                    .find(|other| other != handle)
                {
                    self.driver()
                        .switch_to_window(next)
                        .await
                        .context("switch to a remaining window")?;
                }
            }
        }
        self.inner.closed.store(true, Ordering::SeqCst);
        Ok(())
    }

    // ── dialogs ────────────────────────────────────────────────────────────

    pub fn set_dialog_policy(&self, policy: DialogPolicy) {
        *self
            .inner
            .events
            .policy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = policy;
    }

    /// The dialog waiting for an answer, if any.
    pub async fn dialog(&self) -> Result<Option<Dialog>> {
        match &self.inner.target {
            PageTarget::Bidi(_) => Ok(self.inner.events.dialog()),
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                let text = self.driver().get_alert_text().await;
                drop(window);
                Ok(text.ok().map(|message| Dialog {
                    kind: "unknown".to_string(),
                    message,
                    default_value: None,
                }))
            }
        }
    }

    /// Accept or dismiss the open dialog, typing `prompt_text` into a prompt first.
    pub async fn handle_dialog(&self, accept: bool, prompt_text: Option<String>) -> Result<()> {
        match &self.inner.target {
            PageTarget::Bidi(context) => {
                self.bidi_handle()?
                    .send(HandleUserPrompt {
                        context: context.clone(),
                        accept: Some(accept),
                        user_text: prompt_text,
                    })
                    .await
                    .context("browsingContext.handleUserPrompt")?;
                self.inner.events.set_dialog(None);
            }
            PageTarget::Classic(_) => {
                let window = self.classic_window().await?;
                if let Some(text) = prompt_text {
                    self.driver()
                        .send_alert_text(text)
                        .await
                        .context("type into the prompt")?;
                }
                if accept {
                    self.driver()
                        .accept_alert()
                        .await
                        .context("accept the dialog")?;
                } else {
                    self.driver()
                        .dismiss_alert()
                        .await
                        .context("dismiss the dialog")?;
                }
                drop(window);
            }
        }
        Ok(())
    }

    pub fn on_dialog(&self) -> broadcast::Receiver<Dialog> {
        self.inner.events.dialog_tx.subscribe()
    }

    // ── console and network ────────────────────────────────────────────────

    /// Console messages newer than `seq` (0 for all retained).
    pub async fn console_messages(&self, after: u64) -> Result<Vec<ConsoleMessage>> {
        self.sync_capture().await?;
        Ok(self.inner.events.console_after(after))
    }

    /// Requests newer than `seq` (0 for all retained).
    pub async fn network_requests(&self, after: u64) -> Result<Vec<NetworkEntry>> {
        self.sync_capture().await?;
        Ok(self.inner.events.network_after(after))
    }

    /// Live console messages (BiDi); Classic pages deliver on each history read.
    pub fn on_console(&self) -> broadcast::Receiver<ConsoleMessage> {
        self.inner.events.console_tx.subscribe()
    }

    /// Live settled requests (BiDi); Classic pages deliver on each history read.
    pub fn on_network(&self) -> broadcast::Receiver<NetworkEntry> {
        self.inner.events.network_tx.subscribe()
    }

    async fn sync_capture(&self) -> Result<()> {
        if let PageTarget::Bidi(_) = &self.inner.target {
            return Ok(());
        }
        let drained: Drained = self
            .call_json("(lib) => lib.drainCapture()", Vec::new())
            .await
            .context("drain the page's console and network capture")?;
        for message in drained.console {
            self.inner
                .events
                .push_console(message.level, message.text, message.time, None);
        }
        for request in drained.network {
            self.inner.events.push_network(NetworkEntry {
                seq: 0,
                id: format!("capture-{}", request.time),
                method: request.method,
                url: request.url,
                status: request.status,
                status_text: None,
                mime_type: None,
                error: request.error,
                started: request.time,
                duration_ms: Some(request.duration_ms),
                request_headers: Vec::new(),
                response_headers: Vec::new(),
            });
        }
        Ok(())
    }

    // ── routing ────────────────────────────────────────────────────────────

    /// Intercept requests whose URL matches `glob` (BiDi only).
    pub async fn route(&self, glob: &str, handler: RouteHandler) -> Result<()> {
        let PageTarget::Bidi(context) = &self.inner.target else {
            bail!("request interception needs a BiDi session");
        };
        if self.inner.events.routes.add(glob.to_string(), handler) {
            let intercept = self
                .bidi_handle()?
                .send(AddIntercept {
                    phases: vec![InterceptPhase::BeforeRequestSent],
                    contexts: vec![context.clone()],
                    url_patterns: None,
                })
                .await
                .context("network.addIntercept")?;
            self.inner.events.routes.set_intercept(intercept.intercept);
        }
        Ok(())
    }

    pub async fn unroute(&self, glob: &str) -> Result<()> {
        if let Some(intercept) = self.inner.events.routes.remove(glob) {
            self.bidi_handle()?
                .send(RemoveIntercept { intercept })
                .await
                .context("network.removeIntercept")?;
        }
        Ok(())
    }

    pub fn routes(&self) -> Vec<String> {
        self.inner.events.routes.patterns()
    }

    // ── plumbing ───────────────────────────────────────────────────────────

    fn bidi_handle(&self) -> Result<&BiDi> {
        self.bidi()
            .context("this page's session has no BiDi connection")
    }

    /// Switch the Classic session to this page's window; held for one command.
    pub(crate) async fn classic_window(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<WindowHandle>>> {
        let PageTarget::Classic(handle) = &self.inner.target else {
            bail!("not a Classic page");
        };
        let mut current = self.inner.context.session.classic_window.lock().await;
        if current.as_ref() != Some(handle) {
            self.driver()
                .switch_to_window(handle.clone())
                .await
                .context("switch to the page's window")?;
            *current = Some(handle.clone());
        }
        Ok(current)
    }
}

/// Keyboard input to the focused element.
pub struct Keyboard<'a> {
    page: &'a Page,
}

impl Keyboard<'_> {
    /// Press a key or chord such as `Enter` or `Control+A`.
    pub async fn press(&self, combo: &str) -> Result<()> {
        let Chord { modifiers, key } = parse_chord(combo)?;
        let mut actions: Vec<KeyAction> = modifiers
            .iter()
            .map(|value| KeyAction::KeyDown {
                value: value.clone(),
            })
            .collect();
        actions.push(KeyAction::KeyDown { value: key.clone() });
        actions.push(KeyAction::KeyUp { value: key });
        actions.extend(modifiers.iter().rev().map(|value| KeyAction::KeyUp {
            value: value.clone(),
        }));
        self.page.keys(actions).await
    }

    /// Type text a character at a time.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        let actions = text
            .chars()
            .flat_map(|c| {
                [
                    KeyAction::KeyDown {
                        value: c.to_string(),
                    },
                    KeyAction::KeyUp {
                        value: c.to_string(),
                    },
                ]
            })
            .collect();
        self.page.keys(actions).await
    }

    pub async fn down(&self, key: &str) -> Result<()> {
        self.page
            .keys(vec![KeyAction::KeyDown {
                value: key_value(key)?,
            }])
            .await
    }

    pub async fn up(&self, key: &str) -> Result<()> {
        self.page
            .keys(vec![KeyAction::KeyUp {
                value: key_value(key)?,
            }])
            .await
    }
}

/// Pointer input in viewport coordinates.
pub struct Mouse<'a> {
    page: &'a Page,
}

impl Mouse<'_> {
    pub async fn click(&self, x: i64, y: i64, button: MouseButton) -> Result<()> {
        self.page.click_at(x, y, button, 1, &[]).await
    }

    pub async fn move_to(&self, x: i64, y: i64) -> Result<()> {
        self.page
            .pointer(vec![PointerAction::PointerMove {
                x,
                y,
                duration: None,
                origin: "viewport",
            }])
            .await
    }

    pub async fn wheel(&self, delta_x: i64, delta_y: i64) -> Result<()> {
        self.page.wheel(0, 0, delta_x, delta_y).await
    }
}

fn single_char(value: &str) -> Result<char> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => bail!("{value:?} is not a single key"),
    }
}

fn decode_png(data: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("decode the screenshot")
}

pub(crate) fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

pub(crate) fn abbreviate(script: &str) -> String {
    let flat = script.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(80).collect::<String>())
    } else {
        flat
    }
}
