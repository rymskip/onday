//! The MCP tools. One browser per process; the engine can change at runtime.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use onday::{
    Browser, BrowserContext, ClickOptions, ContextOptions, DialogPolicy, DriverSource,
    ElementState, Engine, Fulfill, InterceptedRequest, LaunchOptions, Locator, MouseButton, Page,
    Protocol, ProtocolPreference, RouteAction, ScreenshotOptions, WaitUntil,
};
use rmcp::{
    ErrorData as McpError, ServerHandler, handler::server::wrapper::Parameters, model::*, tool,
    tool_handler, tool_router,
};
use serde_json::value::RawValue;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::params::*;
use crate::session::{Session, SessionDir};

const INSTRUCTIONS: &str = "Browser automation through onday (WebDriver BiDi, WebDriver Classic fallback).
- This server process is one session with its own browser, profile and logs under the session directory (see browser_status), created by the first browser tool. Other sessions in the same directory are independent.
- browser_launch switches engine (chromium, firefox, webkit) at any time; other tools launch the default engine on first use.
- Target elements with a ref from browser_snapshot (e.g. \"e12\") or a selector: CSS, text=…, role=button[name=\"Save\"], testid=…, label=…, chained with >>.
- Iframes, same- or cross-origin, appear in the snapshot under their iframe entry, with refs like \"f1e3\". A selector enters a frame when a >> step follows one: iframe[title=\"Editor\"] >> testid=save. Without that step a selector only searches the top document.
- Actions auto-wait for the element to be visible, stable, enabled and unobscured, then use real pointer and key input.
- Mutating tools answer with the page URL, any open dialog (\"Modal state\"), new console messages and a fresh snapshot.
- Console and network logs stream into the session's logs/ directory.";

struct Running {
    browser: Browser,
    context: BrowserContext,
    current: usize,
    console_seen: u64,
    network_seen: u64,
    streamed: HashSet<String>,
    headless: bool,
}

#[derive(Clone)]
pub struct OndayServer {
    config: Arc<Config>,
    session: Arc<Session>,
    state: Arc<Mutex<Option<Running>>>,
}

fn failure(error: anyhow::Error) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(format!(
        "{error:#}"
    ))]))
}

fn reply(outcome: Result<String>) -> Result<CallToolResult, McpError> {
    match outcome {
        Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
        Err(error) => failure(error),
    }
}

impl OndayServer {
    pub fn new(config: Config, session: Session) -> Self {
        OndayServer {
            config: Arc::new(config),
            session: Arc::new(session),
            state: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn shutdown(&self) {
        if let Some(running) = self.state.lock().await.take()
            && let Err(error) = running.browser.close().await
        {
            tracing::warn!("closing the browser on shutdown failed: {error:#}");
        }
    }

    // ── session plumbing ───────────────────────────────────────────────────

    async fn launch(
        &self,
        engine: Engine,
        headless: bool,
        viewport: (u32, u32),
        executable: Option<std::path::PathBuf>,
        classic: bool,
    ) -> Result<Running> {
        let dir = self.session.dir().await?;
        let mut options = LaunchOptions::new(engine)
            .headless(headless)
            .window_size(viewport.0, viewport.1)
            .driver_log(dir.driver_log())
            .protocol(if classic {
                ProtocolPreference::Classic
            } else {
                ProtocolPreference::Auto
            });
        if self.config.no_sandbox {
            options = options.no_sandbox();
        }
        if let Some(executable) = executable.or_else(|| self.config.executable(engine)) {
            options = options.executable(executable);
        }
        options.driver = match (&self.config.webdriver_url, self.config.driver(engine)) {
            (Some(url), _) => DriverSource::Remote(url.clone()),
            (None, Some(path)) => DriverSource::Binary(path),
            (None, None) => DriverSource::Managed,
        };
        if !self.config.isolated {
            let profile = dir.profile(engine);
            std::fs::create_dir_all(&profile)
                .with_context(|| format!("create {}", profile.display()))?;
            options = options.user_data_dir(profile);
        }
        let browser = Browser::launch(options).await?;
        let context = browser
            .default_context(ContextOptions {
                viewport: Some(viewport),
                dialog_policy: DialogPolicy::Leave,
                downloads_dir: Some(dir.downloads()),
                ..ContextOptions::default()
            })
            .await?;
        let page = context.page().await?;
        page.set_default_timeout(Duration::from_secs(self.config.action_timeout));
        dir.write_metadata(Some(engine), Some(browser.protocol().to_string()))?;
        tracing::info!("launched {engine} over {}", browser.protocol());
        let mut running = Running {
            browser,
            context,
            current: 0,
            console_seen: 0,
            network_seen: 0,
            streamed: HashSet::new(),
            headless,
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

    /// The current page, launching the default engine first if needed.
    async fn page(&self) -> Result<Page> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            let running = self
                .launch(
                    self.config.engine,
                    self.config.headless,
                    self.config.viewport,
                    None,
                    self.config.classic,
                )
                .await?;
            *state = Some(running);
        }
        let running = state.as_mut().context("no browser")?;
        let dir = self.session.dir().await?;
        let pages = running.context.pages().await?;
        if pages.is_empty() {
            let page = running.context.new_page().await?;
            page.set_default_timeout(Duration::from_secs(self.config.action_timeout));
            running.current = 0;
            Self::stream_logs(&dir, running, &page);
            return Ok(page);
        }
        if running.current >= pages.len() {
            running.current = pages.len() - 1;
        }
        let page = pages[running.current].clone();
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
        let Some(running) = state.as_mut() else {
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
        Parameters(p): Parameters<LaunchParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let mut state = self.state.lock().await;
            let previous_headless = state.as_ref().map(|running| running.headless);
            if let Some(running) = state.take() {
                running
                    .browser
                    .close()
                    .await
                    .context("close the running browser")?;
            }
            let engine = match p.engine {
                Some(EngineParam::Chromium) => Engine::Chromium,
                Some(EngineParam::Firefox) => Engine::Firefox,
                Some(EngineParam::Webkit) => Engine::Webkit,
                None => self.config.engine,
            };
            let headless = p
                .headless
                .or(previous_headless)
                .unwrap_or(self.config.headless);
            let viewport = (
                p.width.unwrap_or(self.config.viewport.0),
                p.height.unwrap_or(self.config.viewport.1),
            );
            let classic = p.classic.unwrap_or(self.config.classic);
            let running = self
                .launch(
                    engine,
                    headless,
                    viewport,
                    p.executable_path.map(Into::into),
                    classic,
                )
                .await?;
            let summary = format!(
                "Launched {engine} over {} ({}, {}x{})",
                running.browser.protocol(),
                if headless { "headless" } else { "headed" },
                viewport.0,
                viewport.1
            );
            *state = Some(running);
            Ok(summary)
        }
        .await;
        reply(outcome)
    }

    #[tool(
        name = "browser_status",
        description = "Session id and directory, engine, protocol, tabs, routes and log paths."
    )]
    async fn browser_status(&self) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let root = self.session.root.display();
            let mut out = format!("Session: {}\nDirectory: {root}\n", self.session.id);
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
            let state = self.state.lock().await;
            match state.as_ref() {
                None => out.push_str("Browser: not running (the next browser tool launches it)\n"),
                Some(running) => {
                    writeln!(
                        out,
                        "Browser: {} over {} ({})",
                        running.browser.engine(),
                        running.browser.protocol(),
                        if running.headless { "headless" } else { "headed" }
                    )
                    .context("format")?;
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
        reply(outcome)
    }

    #[tool(
        name = "browser_navigate",
        description = "Open a URL in the current tab."
    )]
    async fn browser_navigate(
        &self,
        Parameters(p): Parameters<NavigateParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.goto(&p.url, WaitUntil::Load).await?;
                Ok(String::new())
            })
            .await,
        )
    }

    #[tool(
        name = "browser_navigate_back",
        description = "Go back in the current tab's history."
    )]
    async fn browser_navigate_back(&self) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.go_back().await?;
                Ok(String::new())
            })
            .await,
        )
    }

    #[tool(
        name = "browser_navigate_forward",
        description = "Go forward in the current tab's history."
    )]
    async fn browser_navigate_forward(&self) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.go_forward().await?;
                Ok(String::new())
            })
            .await,
        )
    }

    #[tool(name = "browser_reload", description = "Reload the current tab.")]
    async fn browser_reload(&self) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.reload().await?;
                Ok(String::new())
            })
            .await,
        )
    }

    #[tool(
        name = "browser_snapshot",
        description = "Accessibility snapshot of the page (or of one ref's subtree) with [ref=…] labels for targeting elements. Prefer it to screenshots for acting on the page."
    )]
    async fn browser_snapshot(
        &self,
        Parameters(p): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            match p.reference {
                None => self.report(&page, true).await,
                Some(reference) => {
                    let tree = page.get_by_ref(&reference).snapshot().await?;
                    Ok(format!("```yaml\n{tree}\n```"))
                }
            }
        }
        .await;
        reply(outcome)
    }

    #[tool(
        name = "browser_click",
        description = "Click an element with real pointer input once it is actionable."
    )]
    async fn browser_click(
        &self,
        Parameters(p): Parameters<ClickParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let locator = Self::locate(&page, &p.target)?;
                locator
                    .click_with(ClickOptions {
                        button: match p.button {
                            ButtonParam::Left => MouseButton::Left,
                            ButtonParam::Middle => MouseButton::Middle,
                            ButtonParam::Right => MouseButton::Right,
                        },
                        click_count: if p.double_click { 2 } else { 1 },
                        modifiers: p.modifiers,
                        ..ClickOptions::default()
                    })
                    .await?;
                Ok(format!("Clicked {}", describe(&p.target)))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_hover",
        description = "Move the pointer over an element."
    )]
    async fn browser_hover(
        &self,
        Parameters(p): Parameters<TargetOnlyParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                Self::locate(&page, &p.target)?.hover().await?;
                Ok(format!("Hovered {}", describe(&p.target)))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_type",
        description = "Enter text into an input, textarea or contenteditable. Replaces the value unless slowly is set."
    )]
    async fn browser_type(
        &self,
        Parameters(p): Parameters<TypeParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let locator = Self::locate(&page, &p.target)?;
                if p.slowly {
                    locator.press_sequentially(&p.text).await?;
                } else {
                    locator.fill(&p.text).await?;
                }
                if p.submit {
                    locator.press("Enter").await?;
                }
                Ok(format!("Typed into {}", describe(&p.target)))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_fill_form",
        description = "Fill several fields: textboxes, checkboxes, radios, comboboxes and sliders."
    )]
    async fn browser_fill_form(
        &self,
        Parameters(p): Parameters<FillFormParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                for field in &p.fields {
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
                Ok(format!("Filled {} field(s)", p.fields.len()))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_select_option",
        description = "Select options of a <select> by value or label."
    )]
    async fn browser_select_option(
        &self,
        Parameters(p): Parameters<SelectParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let values: Vec<&str> = p.values.iter().map(String::as_str).collect();
                let chosen = Self::locate(&page, &p.target)?
                    .select_option(&values)
                    .await?;
                Ok(format!("Selected {chosen:?}"))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_press_key",
        description = "Press a key or chord in the focused element, e.g. Enter, Escape, ArrowDown, Control+A."
    )]
    async fn browser_press_key(
        &self,
        Parameters(p): Parameters<PressKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.keyboard().press(&p.key).await?;
                Ok(format!("Pressed {}", p.key))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_drag",
        description = "Drag one element onto another with real pointer moves. HTML5 drag-and-drop the driver cannot complete, including drops into another frame, fires the drag events in the pages instead."
    )]
    async fn browser_drag(
        &self,
        Parameters(p): Parameters<DragParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let start = Self::locate(
                    &page,
                    &TargetParams {
                        reference: p.start_ref,
                        selector: p.start_selector,
                        element: None,
                    },
                )?;
                let end = Self::locate(
                    &page,
                    &TargetParams {
                        reference: p.end_ref,
                        selector: p.end_selector,
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
    }

    #[tool(
        name = "browser_file_upload",
        description = "Set the files of a file input (it may be hidden)."
    )]
    async fn browser_file_upload(
        &self,
        Parameters(p): Parameters<UploadParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let locator = match (&p.target.reference, &p.target.selector) {
                    (None, None) => page.locator("input[type=file]"),
                    _ => Self::locate(&page, &p.target)?,
                };
                locator.set_input_files(&p.paths).await?;
                Ok(format!("Attached {} file(s)", p.paths.len()))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_handle_dialog",
        description = "Accept or dismiss the open alert, confirm or prompt dialog."
    )]
    async fn browser_handle_dialog(
        &self,
        Parameters(p): Parameters<DialogParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.handle_dialog(p.accept, p.prompt_text).await?;
                Ok(if p.accept {
                    "Accepted the dialog".to_string()
                } else {
                    "Dismissed the dialog".to_string()
                })
            })
            .await,
        )
    }

    #[tool(
        name = "browser_wait_for",
        description = "Wait for text to appear or disappear, for a selector to become visible, or for a number of seconds."
    )]
    async fn browser_wait_for(
        &self,
        Parameters(p): Parameters<WaitParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                let timeout = Duration::from_secs_f64(p.timeout.unwrap_or(30.0));
                if let Some(seconds) = p.time {
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                }
                if let Some(text) = &p.text {
                    page.get_by_text(text)
                        .first()
                        .wait_for(ElementState::Visible, timeout)
                        .await?;
                }
                if let Some(text) = &p.text_gone {
                    page.get_by_text(text)
                        .first()
                        .wait_for(ElementState::Hidden, timeout)
                        .await?;
                }
                if let Some(selector) = &p.selector {
                    page.locator(selector.clone())
                        .first()
                        .wait_for(ElementState::Visible, timeout)
                        .await?;
                }
                Ok("Waited".to_string())
            })
            .await,
        )
    }

    #[tool(
        name = "browser_evaluate",
        description = "Run a JS expression or function in the page and return its JSON result. Read-only inspection; use the input tools to act."
    )]
    async fn browser_evaluate(
        &self,
        Parameters(p): Parameters<EvaluateParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let result: Box<RawValue> = match (&p.target.reference, &p.target.selector) {
                (None, None) => page.evaluate(&p.function).await?,
                _ => Self::locate(&page, &p.target)?.eval_on(&p.function).await?,
            };
            Ok(format!("```json\n{}\n```", result.get()))
        }
        .await;
        reply(outcome)
    }

    #[tool(
        name = "browser_take_screenshot",
        description = "PNG of the viewport, the full page, or one element; saved under the session's screenshots/ and returned as an image."
    )]
    async fn browser_take_screenshot(
        &self,
        Parameters(p): Parameters<ScreenshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let png = match (&p.target.reference, &p.target.selector) {
                (None, None) => {
                    page.screenshot(ScreenshotOptions {
                        full_page: p.full_page,
                    })
                    .await?
                }
                _ => Self::locate(&page, &p.target)?.screenshot().await?,
            };
            let name = match p.filename {
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
            Err(error) => failure(error),
        }
    }

    #[tool(
        name = "browser_console_messages",
        description = "Console messages and uncaught errors of the current tab."
    )]
    async fn browser_console_messages(
        &self,
        Parameters(p): Parameters<ConsoleParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let floor = level_rank(p.level.as_deref().unwrap_or("debug"));
            let mut out = String::new();
            for message in page.console_messages(0).await? {
                let wanted = level_rank(&message.level) >= floor
                    && p.filter
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
        reply(outcome)
    }

    #[tool(
        name = "browser_network_requests",
        description = "Requests made by the current tab with status and timing."
    )]
    async fn browser_network_requests(
        &self,
        Parameters(p): Parameters<NetworkParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let mut out = String::new();
            for entry in page.network_requests(0).await? {
                if p.filter
                    .as_deref()
                    .is_some_and(|filter| !entry.url.contains(filter))
                {
                    continue;
                }
                let failed =
                    entry.error.is_some() || entry.status.is_some_and(|status| status >= 400);
                if p.failures_only && !failed {
                    continue;
                }
                let outcome = match (&entry.status, &entry.error) {
                    (Some(status), _) => status.to_string(),
                    (None, Some(error)) => format!("FAILED {error}"),
                    (None, None) => "pending".to_string(),
                };
                let duration = entry
                    .duration_ms
                    .map(|ms| format!(" {ms}ms"))
                    .unwrap_or_default();
                writeln!(out, "{} {} -> {outcome}{duration}", entry.method, entry.url)
                    .context("format")?;
                if p.headers {
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
        reply(outcome)
    }

    #[tool(
        name = "browser_route",
        description = "Intercept requests matching a URL glob: block them, answer them with a fixed response, or let them continue (BiDi sessions)."
    )]
    async fn browser_route(
        &self,
        Parameters(p): Parameters<RouteParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            let page = self.page().await?;
            let action = match p.action {
                RouteKind::Block => RouteAction::Abort,
                RouteKind::Continue => RouteAction::Continue,
                RouteKind::Fulfill => RouteAction::Fulfill(Fulfill::new(
                    p.status.unwrap_or(200),
                    p.content_type.as_deref().unwrap_or("application/json"),
                    p.body.clone().unwrap_or_default(),
                )),
            };
            let handler = move |request: &InterceptedRequest| {
                tracing::debug!("route answered {} {}", request.method, request.url);
                action.clone()
            };
            page.route(&p.pattern, Arc::new(handler)).await?;
            Ok(format!("Routing {}", p.pattern))
        }
        .await;
        reply(outcome)
    }

    #[tool(
        name = "browser_unroute",
        description = "Remove a route added with browser_route."
    )]
    async fn browser_unroute(
        &self,
        Parameters(p): Parameters<UnrouteParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            self.page().await?.unroute(&p.pattern).await?;
            Ok(format!("Removed route {}", p.pattern))
        }
        .await;
        reply(outcome)
    }

    #[tool(
        name = "browser_tabs",
        description = "List, open, select or close tabs."
    )]
    async fn browser_tabs(
        &self,
        Parameters(p): Parameters<TabsParams>,
    ) -> Result<CallToolResult, McpError> {
        let outcome = async {
            self.page().await?;
            let mut state = self.state.lock().await;
            let running = state.as_mut().context("no browser")?;
            match p.action {
                TabAction::List => {}
                TabAction::New => {
                    let page = running.context.new_page().await?;
                    if let Some(url) = &p.url {
                        page.goto(url, WaitUntil::Load).await?;
                    }
                    running.current = running.context.pages().await?.len().saturating_sub(1);
                    running.console_seen = 0;
                    running.network_seen = 0;
                }
                TabAction::Select => {
                    let index = p.index.context("select needs an index")?;
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
                    let index = p.index.unwrap_or(running.current);
                    let page = pages
                        .get(index)
                        .with_context(|| format!("no tab {index}"))?;
                    page.close().await?;
                    if running.current >= index && running.current > 0 {
                        running.current -= 1;
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
        reply(outcome)
    }

    #[tool(name = "browser_resize", description = "Resize the viewport.")]
    async fn browser_resize(
        &self,
        Parameters(p): Parameters<ResizeParams>,
    ) -> Result<CallToolResult, McpError> {
        reply(
            self.act(|page| async move {
                page.set_viewport(p.width, p.height).await?;
                Ok(format!("Viewport is {}x{}", p.width, p.height))
            })
            .await,
        )
    }

    #[tool(
        name = "browser_close",
        description = "Close the browser. The next browser tool starts it again."
    )]
    async fn browser_close(&self) -> Result<CallToolResult, McpError> {
        let outcome = async {
            match self.state.lock().await.take() {
                Some(running) => {
                    running.browser.close().await?;
                    Ok("Closed the browser".to_string())
                }
                None => Ok("The browser was not running".to_string()),
            }
        }
        .await;
        reply(outcome)
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
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
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
