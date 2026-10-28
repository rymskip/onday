//! The MCP tools. One browser per process; the engine can change at runtime.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use onday::{
    Browser, BrowserContext, ClickOptions, ContextOptions, DialogPolicy, DriverSource,
    ElementState, Engine, Fulfill, Health, InterceptedRequest, LaunchOptions, Locator, MouseButton,
    Page, Protocol, ProtocolPreference, RouteAction, ScreenshotOptions, WaitUntil,
};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde_json::value::RawValue;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::params::{
    ButtonParam, ClickParams, ConsoleParams, DialogParams, DragParams, EngineParam, EvaluateParams,
    FieldKind, FillFormParams, LaunchParams, NavigateParams, NetworkParams, PressKeyParams,
    ResizeParams, RouteKind, RouteParams, ScreenshotParams, SelectParams, SnapshotParams,
    TabAction, TabsParams, TargetOnlyParams, TargetParams, TypeParams, UnrouteParams, UploadParams,
    WaitParams,
};
use crate::session::{Session, SessionDir};

const INSTRUCTIONS: &str = "Browser automation through onday (WebDriver BiDi, WebDriver Classic fallback).
- This server process is one session with its own browser, profile and logs under the session directory (see browser_status), created by the first browser tool. Other sessions in the same directory are independent.
- browser_launch switches engine (chromium, firefox, webkit) at any time; other tools launch the default engine on first use.
- Target elements with a ref from browser_snapshot (e.g. \"e12\") or a selector: CSS, text=…, role=button[name=\"Save\"], testid=…, label=…, chained with >>.
- Iframes, same- or cross-origin, appear in the snapshot under their iframe entry, with refs like \"f1e3\". A selector enters a frame when a >> step follows one: iframe[title=\"Editor\"] >> testid=save. Without that step a selector only searches the top document.
- Actions auto-wait for the element to be visible, stable, enabled and unobscured, then use real pointer and key input.
- Mutating tools answer with the page URL, any open dialog (\"Modal state\"), new console messages and a fresh snapshot.
- Console and network logs stream into the session's logs/ directory.
- If the browser closes or crashes outside onday, the next tool fails fast and releases it. The session, its directory and profile stay; retrying the tool launches a new browser with the same settings. browser_status reports the browser's health.";

struct Running {
    browser: Browser,
    context: BrowserContext,
    current: usize,
    console_seen: u64,
    network_seen: u64,
    streamed: HashSet<String>,
}

/// How the browser launches. Kept across relaunches, so a browser that closed
/// comes back the way browser_launch last set it up.
#[derive(Clone)]
struct LaunchSettings {
    engine: Engine,
    headless: bool,
    viewport: (u32, u32),
    /// Overrides the configured binary for `engine`.
    executable: Option<PathBuf>,
    classic: bool,
}

impl std::fmt::Display for LaunchSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}, {}x{}{})",
            self.engine,
            if self.headless { "headless" } else { "headed" },
            self.viewport.0,
            self.viewport.1,
            if self.classic { ", Classic" } else { "" }
        )
    }
}

struct BrowserState {
    running: Option<Running>,
    settings: LaunchSettings,
    /// Why the last browser was released, when it closed outside onday.
    vanished: Option<String>,
}

#[derive(Clone)]
pub struct OndayServer {
    config: Arc<Config>,
    session: Arc<Session>,
    state: Arc<Mutex<BrowserState>>,
}

impl OndayServer {
    async fn reply(&self, outcome: Result<String>) -> Result<CallToolResult, McpError> {
        match outcome {
            Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
            Err(error) => self.failure(error).await,
        }
    }

    /// Report a failed tool call. A browser that closed outside onday is released,
    /// and the agent is told what survives and how to carry on.
    async fn failure(&self, error: anyhow::Error) -> Result<CallToolResult, McpError> {
        let text = match self.release_if_gone().await {
            None => format!("{error:#}"),
            Some(settings) => {
                let kept = if self.config.isolated {
                    "keeps its directory and logs (its profile is throwaway, so cookies and logins do not carry over)"
                } else {
                    "keeps its directory, logs and browser profile, so cookies and logins carry over"
                };
                format!(
                    "The browser was closed outside onday (its window was closed or it crashed), so this call did not run.\n\
                     The onday session {id} is still running and {kept}. Retry the call to open a new {settings} browser.\n\
                     Tabs, page state, snapshot refs and routes from the closed browser are gone: take a fresh browser_snapshot before using refs.\n\
                     Detail: {error:#}",
                    id = self.session.id,
                )
            }
        };
        Ok(CallToolResult::error(vec![ContentBlock::text(text)]))
    }

    /// Drop the running browser if it closed or crashed, reaping its driver, and
    /// answer the settings its replacement launches with.
    async fn release_if_gone(&self) -> Option<LaunchSettings> {
        let mut state = self.state.lock().await;
        let Health::Gone(reason) = state.running.as_ref()?.browser.health().await else {
            return None;
        };
        tracing::warn!("releasing the vanished browser: {reason}");
        if let Some(running) = state.running.take()
            && let Err(error) = running.browser.close().await
        {
            tracing::warn!("releasing the vanished browser failed: {error:#}");
        }
        state.vanished = Some(reason);
        Some(state.settings.clone())
    }

    pub fn new(config: Config, session: Session) -> Self {
        let settings = LaunchSettings {
            engine: config.engine,
            headless: config.headless,
            viewport: config.viewport,
            executable: None,
            classic: config.classic,
        };
        OndayServer {
            config: Arc::new(config),
            session: Arc::new(session),
            state: Arc::new(Mutex::new(BrowserState {
                running: None,
                settings,
                vanished: None,
            })),
        }
    }

    pub async fn shutdown(&self) {
        if let Some(running) = self.state.lock().await.running.take()
            && let Err(error) = running.browser.close().await
        {
            tracing::warn!("closing the browser on shutdown failed: {error:#}");
        }
    }

    // ── session plumbing ───────────────────────────────────────────────────

    async fn launch(&self, settings: &LaunchSettings) -> Result<Running> {
        let engine = settings.engine;
        let dir = self.session.dir().await?;
        let mut options = LaunchOptions::new(engine)
            .headless(settings.headless)
            .window_size(settings.viewport.0, settings.viewport.1)
            .driver_log(dir.driver_log())
            .protocol(if settings.classic {
                ProtocolPreference::Classic
            } else {
                ProtocolPreference::Auto
            });
        if self.config.no_sandbox {
            options = options.no_sandbox();
        }
        if let Some(executable) = settings
            .executable
            .clone()
            .or_else(|| self.config.executable(engine))
        {
            options = options.executable(executable);
        }
        options.driver = match (&self.config.webdriver_url, self.config.driver(engine)) {
            (Some(url), _) => DriverSource::Remote(url.clone()),
            (None, Some(path)) => DriverSource::Binary(path),
            (None, None) => DriverSource::Managed,
        };
        if !self.config.isolated {
            let profile = dir.profile(engine);
            // A browser relaunched in this session keeps the profile its first launch made.
            if let Some(source) = self.session.profile_from()
                && !profile.exists()
            {
                source.copy_to(engine, &profile)?;
                tracing::info!("started the {engine} profile from session {}", source.id());
            }
            std::fs::create_dir_all(&profile)
                .with_context(|| format!("create {}", profile.display()))?;
            options = options.user_data_dir(profile);
        }
        let browser = Browser::launch(options).await?;
        let context = browser
            .default_context(ContextOptions {
                viewport: Some(settings.viewport),
                dialog_policy: DialogPolicy::Leave,
                downloads_dir: Some(dir.downloads()),
                ..ContextOptions::default()
            })
            .await?;
        let page = context.page().await?;
        page.set_default_timeout(Duration::from_secs(self.config.action_timeout));
        dir.write_metadata(Some(engine), Some(browser.protocol().to_string()))?;
        tracing::info!("launched {settings} over {}", browser.protocol());
        let mut running = Running {
            browser,
            context,
            current: 0,
            console_seen: 0,
            network_seen: 0,
            streamed: HashSet::new(),
        };
        Self::stream_logs(&dir, &mut running, &page);
        Ok(running)
    }

    /// Write a BiDi page's console and network events to the session logs as they happen.
    fn stream_logs(dir: &Arc<SessionDir>, running: &mut Running, page: &Page) {
        let Some(id) = page.context_id() else {
            return;
        };
        if !running.streamed.insert(id.as_str().to_string()) {
            return;
        }
        let mut console = page.on_console();
        let console_dir = dir.clone();
        tokio::spawn(async move {
            while let Ok(message) = console.recv().await {
                console_dir.log_console(&message);
            }
        });
        let mut network = page.on_network();
        let dir = dir.clone();
        tokio::spawn(async move {
            while let Ok(entry) = network.recv().await {
                dir.log_network(&entry);
            }
        });
    }

    /// The current page, launching the browser first if none is running.
    async fn page(&self) -> Result<Page> {
        let mut state = self.state.lock().await;
        if state.running.is_none() {
            let running = self.launch(&state.settings).await?;
            state.running = Some(running);
            state.vanished = None;
        }
        let running = state.running.as_mut().context("no browser")?;
        let dir = self.session.dir().await?;
        let pages = running.context.pages().await?;
        let Some(last) = pages.last() else {
            let page = running.context.new_page().await?;
            page.set_default_timeout(Duration::from_secs(self.config.action_timeout));
            running.current = 0;
            Self::stream_logs(&dir, running, &page);
            return Ok(page);
        };
        // The tab the session was on may have closed; the last one takes its place.
        let page = match pages.get(running.current) {
            Some(page) => page.clone(),
            None => {
                running.current = pages.len().saturating_sub(1);
                last.clone()
            }
        };
        page.set_default_timeout(Duration::from_secs(self.config.action_timeout));
        Self::stream_logs(&dir, running, &page);
        Ok(page)
    }

    fn locate(page: &Page, target: &TargetParams) -> Result<Locator> {
        match (&target.reference, &target.selector) {
            (Some(reference), None) => Ok(page.get_by_ref(reference)),
            (None, Some(selector)) => Ok(page.locator(selector.clone())),
            (Some(_), Some(_)) => bail!("pass either ref or selector, not both"),
            (None, None) => bail!("pass a ref from browser_snapshot or a selector"),
        }
    }

    /// URL, dialog, new console output and (unless a dialog blocks it) a snapshot.
    async fn report(&self, page: &Page, snapshot: bool) -> Result<String> {
        let mut out = String::new();
        let dialog = page.dialog().await?;
        match (&dialog, page.url().await) {
            (_, Ok(url)) => {
                writeln!(out, "### Page\n- URL: {url}").context("format")?;
                if dialog.is_none()
                    && let Ok(title) = page.title().await
                {
                    writeln!(out, "- Title: {title}").context("format")?;
                }
            }
            (_, Err(error)) => {
                writeln!(out, "### Page\n- URL unavailable: {error:#}").context("format")?
            }
        }
        if let Some(dialog) = &dialog {
            writeln!(
                out,
                "\n### Modal state\n- [{} dialog] {:?}: answer it with browser_handle_dialog before anything else",
                dialog.kind, dialog.message
            )
            .context("format")?;
        }
        if dialog.is_none() {
            let fresh = self.fresh_console(page).await?;
            if !fresh.is_empty() {
                out.push_str("\n### New console messages\n");
                for message in fresh {
                    writeln!(out, "- [{}] {}", message.level, message.text).context("format")?;
                }
            }
        }
        if snapshot && dialog.is_none() {
            let tree = self.snapshot_with_retry(page).await?;
            let dir = self.session.dir().await?;
            std::fs::write(dir.snapshots().join("latest.yml"), &tree)
                .context("save the snapshot")?;
            write!(out, "\n### Snapshot\n```yaml\n{tree}\n```\n").context("format")?;
        }
        Ok(out)
    }

    /// Console messages the agent has not seen yet; Classic pages log them here too.
    async fn fresh_console(&self, page: &Page) -> Result<Vec<onday::ConsoleMessage>> {
        let mut state = self.state.lock().await;
        let Some(running) = state.running.as_mut() else {
            return Ok(Vec::new());
        };
        let fresh = page.console_messages(running.console_seen).await?;
        if let Some(last) = fresh.last() {
            running.console_seen = last.seq;
        }
        if page.protocol() == Protocol::Classic {
            let dir = self.session.dir().await?;
            for message in &fresh {
                dir.log_console(message);
            }
            let requests = page.network_requests(running.network_seen).await?;
            if let Some(last) = requests.last() {
                running.network_seen = last.seq;
            }
            for entry in &requests {
                dir.log_network(entry);
            }
        }
        Ok(fresh)
    }

    /// A navigation in flight can tear down the document mid-snapshot; retry briefly.
    async fn snapshot_with_retry(&self, page: &Page) -> Result<String> {
        let mut last = None;
        for _ in 0..6 {
            match page.snapshot().await {
                Ok(tree) => return Ok(tree),
                Err(error) => last = Some(error),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(last.context("snapshot never ran")?)
    }

    async fn act<F, Fut>(&self, action: F) -> Result<String>
    where
        F: FnOnce(Page) -> Fut,
        Fut: std::future::Future<Output = Result<String>>,
    {
        let page = self.page().await?;
        let summary = action(page.clone()).await?;
        let report = self.report(&page, true).await?;
        Ok(if summary.is_empty() {
            report
        } else {
            format!("{summary}\n\n{report}")
        })
    }
}

#[tool_router]
impl OndayServer {
    #[tool(
        name = "browser_launch",
        description = "Start the browser, or restart it with another engine (chromium, firefox, webkit) without restarting the server. Closes the running browser and its tabs."
    )]
    async fn browser_launch(
        &self,
        Parameters(params): Parameters<LaunchParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let mut state = self.state.lock().await;
            if let Some(running) = state.running.take() {
                running
                    .browser
                    .close()
                    .await
                    .context("close the running browser")?;
            }
            let current = state.settings.clone();
            let engine = match params.engine {
                Some(EngineParam::Chromium) => Engine::Chromium,
                Some(EngineParam::Firefox) => Engine::Firefox,
                Some(EngineParam::Webkit) => Engine::Webkit,
                None => current.engine,
            };
            let settings = LaunchSettings {
                engine,
                headless: params.headless.unwrap_or(current.headless),
                viewport: (
                    params.width.unwrap_or(current.viewport.0),
                    params.height.unwrap_or(current.viewport.1),
                ),
                executable: match params.executable_path {
                    Some(path) => Some(path.into()),
                    None if engine == current.engine => current.executable,
                    None => None,
                },
                classic: params.classic.unwrap_or(current.classic),
            };
            let running = self.launch(&settings).await?;
            let summary = format!("Launched {settings} over {}", running.browser.protocol());
            state.settings = settings;
            state.vanished = None;
            state.running = Some(running);
            Ok(summary)
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_status",
        description = "Session id and directory, engine, protocol, browser health, tabs, routes and log paths. Never waits on the browser."
    )]
    async fn browser_status(&self) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let root = self.session.root.display();
            let mut out = format!("Session: {}\nDirectory: {root}\n", self.session.id);
            if let Some(source) = self.session.profile_from() {
                writeln!(out, "Profile started from session: {}", source.id()).context("format")?;
            }
            match self.session.opened() {
                Some(dir) => writeln!(
                    out,
                    "Logs: {root}/logs/{{console,network,driver,mcp}}.log\nScreenshots: {}\nLock holder pid: {}",
                    dir.screenshots().display(),
                    dir.holder()?.trim()
                )
                .context("format")?,
                None => out.push_str("Directory not created yet: the first browser tool creates it\n"),
            }
            let Ok(state) = self.state.try_lock() else {
                out.push_str("Browser: busy (another tool call is using it)\n");
                return Ok(out);
            };
            match (&state.running, &state.vanished) {
                (None, None) => writeln!(
                    out,
                    "Browser: not running (the next browser tool launches {})",
                    state.settings
                )
                .context("format")?,
                (None, Some(reason)) => writeln!(
                    out,
                    "Browser: not running; the last one was closed outside onday ({reason}). The session is unaffected: the next browser tool launches a new {}",
                    state.settings
                )
                .context("format")?,
                (Some(running), _) => {
                    let health = running.browser.health().await;
                    writeln!(
                        out,
                        "Browser: {} over {}\nHealth: {health}",
                        state.settings,
                        running.browser.protocol()
                    )
                    .context("format")?;
                    if health != Health::Alive {
                        return Ok(out);
                    }
                    let pages = running.context.pages().await?;
                    for (index, page) in pages.iter().enumerate() {
                        let marker = if index == running.current { "*" } else { " " };
                        let url = page.url().await.unwrap_or_else(|error| format!("({error:#})"));
                        writeln!(out, "{marker} tab {index}: {url}").context("format")?;
                        for pattern in page.routes() {
                            writeln!(out, "    route {pattern}").context("format")?;
                        }
                    }
                }
            }
            Ok(out)
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_navigate",
        description = "Open a URL in the current tab."
    )]
    async fn browser_navigate(
        &self,
        Parameters(params): Parameters<NavigateParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.goto(&params.url, WaitUntil::Load).await?;
                Ok(String::new())
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_navigate_back",
        description = "Go back in the current tab's history."
    )]
    async fn browser_navigate_back(&self) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.go_back().await?;
                Ok(String::new())
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_navigate_forward",
        description = "Go forward in the current tab's history."
    )]
    async fn browser_navigate_forward(&self) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.go_forward().await?;
                Ok(String::new())
            })
            .await,
        )
        .await
    }

    #[tool(name = "browser_reload", description = "Reload the current tab.")]
    async fn browser_reload(&self) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.reload().await?;
                Ok(String::new())
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_snapshot",
        description = "Accessibility snapshot of the page (or of one ref's subtree) with [ref=…] labels for targeting elements. Prefer it to screenshots for acting on the page."
    )]
    async fn browser_snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            match params.reference {
                None => self.report(&page, true).await,
                Some(reference) => {
                    let tree = page.get_by_ref(&reference).snapshot().await?;
                    Ok(format!("```yaml\n{tree}\n```"))
                }
            }
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_click",
        description = "Click an element with real pointer input once it is actionable."
    )]
    async fn browser_click(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let locator = Self::locate(&page, &params.target)?;
                locator
                    .click_with(ClickOptions {
                        button: match params.button {
                            ButtonParam::Left => MouseButton::Left,
                            ButtonParam::Middle => MouseButton::Middle,
                            ButtonParam::Right => MouseButton::Right,
                        },
                        click_count: if params.double_click { 2 } else { 1 },
                        modifiers: params.modifiers,
                        ..ClickOptions::default()
                    })
                    .await?;
                Ok(format!("Clicked {}", describe(&params.target)))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_hover",
        description = "Move the pointer over an element."
    )]
    async fn browser_hover(
        &self,
        Parameters(params): Parameters<TargetOnlyParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                Self::locate(&page, &params.target)?.hover().await?;
                Ok(format!("Hovered {}", describe(&params.target)))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_type",
        description = "Enter text into an input, textarea or contenteditable. Replaces the value unless slowly is set."
    )]
    async fn browser_type(
        &self,
        Parameters(params): Parameters<TypeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let locator = Self::locate(&page, &params.target)?;
                if params.slowly {
                    locator.press_sequentially(&params.text).await?;
                } else {
                    locator.fill(&params.text).await?;
                }
                if params.submit {
                    locator.press("Enter").await?;
                }
                Ok(format!("Typed into {}", describe(&params.target)))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_fill_form",
        description = "Fill several fields: textboxes, checkboxes, radios, comboboxes and sliders."
    )]
    async fn browser_fill_form(
        &self,
        Parameters(params): Parameters<FillFormParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                for field in &params.fields {
                    let locator = Self::locate(&page, &field.target)?;
                    match field.kind {
                        Some(FieldKind::Checkbox) | Some(FieldKind::Radio) => {
                            locator.set_checked(field.value == "true").await?
                        }
                        Some(FieldKind::Combobox) if locator.info().await?.tag == "select" => {
                            locator
                                .select_option(&[field.value.as_str()])
                                .await
                                .map(|_| ())?
                        }
                        _ => locator.fill(&field.value).await?,
                    }
                }
                Ok(format!("Filled {} field(s)", params.fields.len()))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_select_option",
        description = "Select options of a <select> by value or label."
    )]
    async fn browser_select_option(
        &self,
        Parameters(params): Parameters<SelectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let values: Vec<&str> = params.values.iter().map(String::as_str).collect();
                let chosen = Self::locate(&page, &params.target)?
                    .select_option(&values)
                    .await?;
                Ok(format!("Selected {chosen:?}"))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_press_key",
        description = "Press a key or chord in the focused element, e.g. Enter, Escape, ArrowDown, Control+A."
    )]
    async fn browser_press_key(
        &self,
        Parameters(params): Parameters<PressKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.keyboard().press(&params.key).await?;
                Ok(format!("Pressed {}", params.key))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_drag",
        description = "Drag one element onto another with real pointer moves. HTML5 drag-and-drop the driver cannot complete, including drops into another frame, fires the drag events in the pages instead."
    )]
    async fn browser_drag(
        &self,
        Parameters(params): Parameters<DragParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let start = Self::locate(
                    &page,
                    &TargetParams {
                        reference: params.start_ref,
                        selector: params.start_selector,
                        element: None,
                    },
                )?;
                let end = Self::locate(
                    &page,
                    &TargetParams {
                        reference: params.end_ref,
                        selector: params.end_selector,
                        element: None,
                    },
                )?;
                start.drag_to(&end).await?;
                Ok(format!(
                    "Dragged {} onto {}",
                    start.selector(),
                    end.selector()
                ))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_file_upload",
        description = "Set the files of a file input (it may be hidden)."
    )]
    async fn browser_file_upload(
        &self,
        Parameters(params): Parameters<UploadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let locator = match (&params.target.reference, &params.target.selector) {
                    (None, None) => page.locator("input[type=file]"),
                    _ => Self::locate(&page, &params.target)?,
                };
                locator.set_input_files(&params.paths).await?;
                Ok(format!("Attached {} file(s)", params.paths.len()))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_handle_dialog",
        description = "Accept or dismiss the open alert, confirm or prompt dialog."
    )]
    async fn browser_handle_dialog(
        &self,
        Parameters(params): Parameters<DialogParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.handle_dialog(params.accept, params.prompt_text)
                    .await?;
                Ok(if params.accept {
                    "Accepted the dialog".to_string()
                } else {
                    "Dismissed the dialog".to_string()
                })
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_wait_for",
        description = "Wait for text to appear or disappear, for a selector to become visible, or for a number of seconds."
    )]
    async fn browser_wait_for(
        &self,
        Parameters(params): Parameters<WaitParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                let timeout = Duration::from_secs_f64(params.timeout.unwrap_or(30.0));
                if let Some(seconds) = params.time {
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                }
                if let Some(text) = &params.text {
                    page.get_by_text(text)
                        .first()
                        .with_timeout(timeout)
                        .wait_for(ElementState::Visible)
                        .await?;
                }
                if let Some(text) = &params.text_gone {
                    page.get_by_text(text)
                        .first()
                        .with_timeout(timeout)
                        .wait_for(ElementState::Hidden)
                        .await?;
                }
                if let Some(selector) = &params.selector {
                    page.locator(selector.clone())
                        .first()
                        .with_timeout(timeout)
                        .wait_for(ElementState::Visible)
                        .await?;
                }
                Ok("Waited".to_string())
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_evaluate",
        description = "Run a JS expression or function in the page and return its JSON result. Read-only inspection; use the input tools to act."
    )]
    async fn browser_evaluate(
        &self,
        Parameters(params): Parameters<EvaluateParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let result: Box<RawValue> = match (&params.target.reference, &params.target.selector) {
                (None, None) => page.evaluate(&params.function).await?,
                _ => {
                    Self::locate(&page, &params.target)?
                        .eval_on(&params.function)
                        .await?
                }
            };
            Ok(format!("```json\n{}\n```", result.get()))
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_take_screenshot",
        description = "PNG of the viewport, the full page, or one element; saved under the session's screenshots/ and returned as an image."
    )]
    async fn browser_take_screenshot(
        &self,
        Parameters(params): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let png = match (&params.target.reference, &params.target.selector) {
                (None, None) => {
                    page.screenshot(ScreenshotOptions {
                        full_page: params.full_page,
                    })
                    .await?
                }
                _ => Self::locate(&page, &params.target)?.screenshot().await?,
            };
            let name = match params.filename {
                Some(name) => sanitize_filename(&name)?,
                None => format!("{}.png", timestamp_millis()),
            };
            let path = self.session.dir().await?.screenshots().join(name);
            std::fs::write(&path, &png).with_context(|| format!("save {}", path.display()))?;
            Ok((path, png))
        }
        .await;
        match outcome {
            Ok((path, png)) => Ok(CallToolResult::success(vec![
                ContentBlock::text(format!("Saved {}", path.display())),
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(png),
                    "image/png",
                ),
            ])),
            Err(error) => self.failure(error).await,
        }
    }

    #[tool(
        name = "browser_console_messages",
        description = "Console messages and uncaught errors of the current tab."
    )]
    async fn browser_console_messages(
        &self,
        Parameters(params): Parameters<ConsoleParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let floor = level_rank(params.level.as_deref().unwrap_or("debug"));
            let mut out = String::new();
            for message in page.console_messages(0).await? {
                let wanted = level_rank(&message.level) >= floor
                    && params
                        .filter
                        .as_deref()
                        .is_none_or(|filter| message.text.contains(filter));
                if wanted {
                    let at = message
                        .location
                        .as_deref()
                        .map(|at| format!(" ({at})"))
                        .unwrap_or_default();
                    writeln!(out, "[{}] {}{at}", message.level, message.text).context("format")?;
                }
            }
            Ok(if out.is_empty() {
                "No console messages".to_string()
            } else {
                out
            })
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_network_requests",
        description = "Requests made by the current tab with status and timing."
    )]
    async fn browser_network_requests(
        &self,
        Parameters(params): Parameters<NetworkParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let mut out = String::new();
            for entry in page.network_requests(0).await? {
                if params
                    .filter
                    .as_deref()
                    .is_some_and(|filter| !entry.url.contains(filter))
                {
                    continue;
                }
                let failed =
                    entry.error.is_some() || entry.status.is_some_and(|status| status >= 400);
                if params.failures_only && !failed {
                    continue;
                }
                let outcome = match (&entry.status, &entry.error) {
                    (Some(status), _) => status.to_string(),
                    (None, Some(error)) => format!("FAILED {error}"),
                    (None, None) => "pending".to_string(),
                };
                writeln!(
                    out,
                    "{} {} {} -> {outcome}{}",
                    entry.started,
                    entry.method,
                    entry.url,
                    crate::session::settled_note(&entry)
                )
                .context("format")?;
                if params.headers {
                    for header in &entry.request_headers {
                        writeln!(out, "    > {}: {}", header.name, header.value)
                            .context("format")?;
                    }
                    for header in &entry.response_headers {
                        writeln!(out, "    < {}: {}", header.name, header.value)
                            .context("format")?;
                    }
                }
            }
            Ok(if out.is_empty() {
                "No requests".to_string()
            } else {
                out
            })
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_route",
        description = "Intercept requests matching a URL glob: block them, answer them with a fixed response, or let them continue (BiDi sessions)."
    )]
    async fn browser_route(
        &self,
        Parameters(params): Parameters<RouteParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let action = match params.action {
                RouteKind::Block => RouteAction::Abort,
                RouteKind::Continue => RouteAction::Continue,
                RouteKind::Fulfill => RouteAction::Fulfill(Fulfill::new(
                    params.status.unwrap_or(200),
                    params.content_type.as_deref().unwrap_or("application/json"),
                    params.body.clone().unwrap_or_default(),
                )),
            };
            let handler = move |request: &InterceptedRequest| {
                tracing::debug!("route answered {} {}", request.method, request.url);
                action.clone()
            };
            page.route(&params.pattern, Arc::new(handler)).await?;
            Ok(format!("Routing {}", params.pattern))
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_unroute",
        description = "Remove a route added with browser_route."
    )]
    async fn browser_unroute(
        &self,
        Parameters(params): Parameters<UnrouteParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            self.page().await?.unroute(&params.pattern).await?;
            Ok(format!("Removed route {}", params.pattern))
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(
        name = "browser_tabs",
        description = "List, open, select or close tabs."
    )]
    async fn browser_tabs(
        &self,
        Parameters(params): Parameters<TabsParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            self.page().await?;
            let mut state = self.state.lock().await;
            let running = state.running.as_mut().context("no browser")?;
            match params.action {
                TabAction::List => {}
                TabAction::New => {
                    let page = running.context.new_page().await?;
                    if let Some(url) = &params.url {
                        page.goto(url, WaitUntil::Load).await?;
                    }
                    running.current = running.context.pages().await?.len().saturating_sub(1);
                    running.console_seen = 0;
                    running.network_seen = 0;
                }
                TabAction::Select => {
                    let index = params.index.context("select needs an index")?;
                    let pages = running.context.pages().await?;
                    let page = pages
                        .get(index)
                        .with_context(|| format!("no tab {index}"))?;
                    page.bring_to_front().await?;
                    running.current = index;
                    running.console_seen = page
                        .console_messages(0)
                        .await?
                        .last()
                        .map_or(0, |message| message.seq);
                    running.network_seen = 0;
                }
                TabAction::Close => {
                    let pages = running.context.pages().await?;
                    let index = params.index.unwrap_or(running.current);
                    let page = pages
                        .get(index)
                        .with_context(|| format!("no tab {index}"))?;
                    page.close().await?;
                    if running.current >= index && running.current > 0 {
                        running.current = running.current.saturating_sub(1);
                    }
                }
            }
            let mut out = String::new();
            for (index, page) in running.context.pages().await?.iter().enumerate() {
                let marker = if index == running.current { "*" } else { " " };
                let url = page
                    .url()
                    .await
                    .unwrap_or_else(|error| format!("({error:#})"));
                writeln!(out, "{marker} {index}: {url}").context("format")?;
            }
            Ok(out)
        }
        .await;
        self.reply(outcome).await
    }

    #[tool(name = "browser_resize", description = "Resize the viewport.")]
    async fn browser_resize(
        &self,
        Parameters(params): Parameters<ResizeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.reply(
            self.act(|page| async move {
                page.set_viewport(params.width, params.height).await?;
                Ok(format!("Viewport is {}x{}", params.width, params.height))
            })
            .await,
        )
        .await
    }

    #[tool(
        name = "browser_close",
        description = "Close the browser. The next browser tool starts it again."
    )]
    async fn browser_close(&self) -> Result<CallToolResult, McpError> {
        let outcome = async {
            match self.state.lock().await.running.take() {
                Some(running) => {
                    running.browser.close().await?;
                    Ok("Closed the browser".to_string())
                }
                None => Ok("The browser was not running".to_string()),
            }
        }
        .await;
        self.reply(outcome).await
    }
}

#[tool_handler]
impl ServerHandler for OndayServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("onday", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

fn describe(target: &TargetParams) -> String {
    match (&target.element, &target.reference, &target.selector) {
        (Some(element), Some(reference), _) => format!("{element} [ref={reference}]"),
        (Some(element), None, Some(selector)) => format!("{element} ({selector})"),
        (None, Some(reference), _) => format!("[ref={reference}]"),
        (None, None, Some(selector)) => selector.clone(),
        (Some(element), None, None) => element.clone(),
        (None, None, None) => "the element".to_string(),
    }
}

fn level_rank(level: &str) -> u8 {
    match level {
        "error" => 3,
        "warn" | "warning" => 2,
        "info" | "log" => 1,
        _ => 0,
    }
}

fn sanitize_filename(name: &str) -> Result<String> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        });
    if !valid {
        bail!("screenshot filename {name:?} must be letters, digits, '-', '_' or '.'");
    }
    Ok(if name.ends_with(".png") {
        name.to_string()
    } else {
        format!("{name}.png")
    })
}

fn timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}
