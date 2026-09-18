//! Lazy element locators with auto-waiting actions.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::frame::Frame;
use crate::keys::parse_chord;
use crate::page::{
    Anchor, ClickPoint, ElementRef, Hit, JsArg, MouseButton, Page, Resolution, abbreviate, quote,
};
use crate::poll::Backoff;
use crate::proto::{KeyAction, PointerAction};

/// Options for [`Locator::click_with`].
#[derive(Debug, Clone, Default)]
pub struct ClickOptions {
    pub button: MouseButton,
    /// 2 for a double click.
    pub click_count: u32,
    /// Key names held during the click, e.g. `Shift`.
    pub modifiers: Vec<String>,
    /// Skip the actionability checks and click wherever the element's centre is.
    pub force: bool,
    pub timeout: Option<Duration>,
}

/// Element states [`Locator::wait_for`] can wait on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementState {
    Attached,
    Detached,
    Visible,
    Hidden,
}

/// What the page runtime reports about one element.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ElementInfo {
    pub tag: String,
    pub text: String,
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub visible: bool,
    pub enabled: bool,
    pub editable: bool,
    pub rect: [f64; 4],
    pub description: String,
}

#[derive(Debug, Deserialize)]
struct Probe {
    ok: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    rect: Option<[f64; 4]>,
}

/// What firing `dragstart` in the source's page produced.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DragStart {
    started: bool,
    items: Vec<(String, String)>,
    effect_allowed: String,
}

/// Pointer moves in a drag, spread over this many animation frames.
const DRAG_STEPS: i64 = 10;

fn pointer_move((x, y): (i64, i64), duration: Option<u64>) -> PointerAction {
    PointerAction::PointerMove {
        x,
        y,
        duration,
        origin: "viewport",
    }
}

/// Moves along the straight line from `from` to `to`, at the given steps out of
/// [`DRAG_STEPS`].
fn glide(
    from: (i64, i64),
    to: (i64, i64),
    steps: impl Iterator<Item = i64>,
) -> impl Iterator<Item = PointerAction> {
    steps.map(move |step| {
        pointer_move(
            (
                from.0 + (to.0 - from.0) * step / DRAG_STEPS,
                from.1 + (to.1 - from.1) * step / DRAG_STEPS,
            ),
            Some(16),
        )
    })
}

/// An actionable element and its centre, in its own frame's viewport and in the
/// top-level one.
struct Aim {
    element: ElementRef,
    local: (i64, i64),
    top: (i64, i64),
}

impl Aim {
    fn anchor(&self) -> Anchor<'_> {
        Anchor {
            element: &self.element,
            x: self.local.0,
            y: self.local.1,
        }
    }
}

/// Why an element is not actionable yet.
enum Pending {
    /// Its box changed since the last probe.
    Moving(String),
    /// Something else stands in the way.
    Blocked(String),
}

#[derive(Debug, Clone, Copy)]
struct Checks {
    enabled: bool,
    hit: bool,
    force: bool,
}

/// A selector bound to a page. Nothing is resolved until an action or query runs.
#[derive(Debug, Clone)]
pub struct Locator {
    page: Page,
    selector: String,
}

impl Locator {
    pub(crate) fn new(page: Page, selector: String) -> Locator {
        Locator { page, selector }
    }

    pub fn selector(&self) -> &str {
        &self.selector
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    // ── refinement ─────────────────────────────────────────────────────────

    /// Descendants of this locator's elements matching `selector`.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(
            self.page.clone(),
            format!("{} >> {selector}", self.selector),
        )
    }

    pub fn nth(&self, index: i64) -> Locator {
        self.locator(&format!("nth={index}"))
    }

    pub fn first(&self) -> Locator {
        self.nth(0)
    }

    pub fn last(&self) -> Locator {
        self.nth(-1)
    }

    /// Keep elements whose text contains `text` (case-insensitive).
    pub fn filter_has_text(&self, text: &str) -> Locator {
        self.locator(&format!("has-text={text}"))
    }

    pub fn visible_only(&self) -> Locator {
        self.locator("visible=true")
    }

    pub fn get_by_test_id(&self, id: &str) -> Locator {
        self.locator(&format!("testid={}", quote(id)))
    }

    pub fn get_by_text(&self, text: &str) -> Locator {
        self.locator(&format!("text={text}"))
    }

    pub fn get_by_label(&self, text: &str) -> Locator {
        self.locator(&format!("label={text}"))
    }

    pub fn get_by_role(&self, role: &str, name: Option<&str>) -> Locator {
        match name {
            Some(name) => self.locator(&format!("role={role}[name={}]", quote(name))),
            None => self.locator(&format!("role={role}")),
        }
    }

    // ── resolution ─────────────────────────────────────────────────────────

    /// Every match, following the selector into frames as it steps through them.
    async fn resolve_all(&self) -> Result<Vec<ElementRef>> {
        let mut frame = Frame::top();
        let mut selector = self.selector.clone();
        loop {
            let resolution = self
                .page
                .resolve(&frame, &selector)
                .await
                .with_context(|| format!("resolve {}", self.selector))?;
            match resolution {
                Resolution::Elements(elements) => return Ok(elements),
                Resolution::Enter { mut frames, rest } => {
                    if frames.len() > 1 {
                        let mut matches = Vec::new();
                        for element in frames.iter().take(5) {
                            matches.push(self.describe(element).await?);
                        }
                        return Err(StrictModeViolation {
                            selector: self.selector.clone(),
                            count: frames.len(),
                            matches,
                        }
                        .into());
                    }
                    frame = self.page.content_frame(&frames.remove(0)).await?;
                    selector = rest;
                }
                Resolution::Frame { prefix, rest } => {
                    frame = self.page.frame_by_prefix(&prefix)?;
                    selector = rest;
                }
            }
        }
    }

    /// The single matching element, if any; more than one is an error.
    async fn resolve_strict(&self) -> Result<Option<ElementRef>> {
        let mut elements = self.resolve_all().await?;
        match elements.len() {
            0 => Ok(None),
            1 => Ok(elements.pop()),
            count => {
                let mut matches = Vec::new();
                for element in elements.iter().take(5) {
                    matches.push(
                        self.describe(element)
                            .await
                            .unwrap_or_else(|error| format!("{error:#}")),
                    );
                }
                Err(StrictModeViolation {
                    selector: self.selector.clone(),
                    count,
                    matches,
                }
                .into())
            }
        }
    }

    async fn describe(&self, element: &ElementRef) -> Result<String> {
        self.page
            .call_on(element, "(lib, el) => lib.describe(el)", Vec::new())
            .await
    }

    /// Wait for exactly one match.
    async fn wait_one(&self, timeout: Duration) -> Result<ElementRef> {
        let start = Instant::now();
        let mut gap = Backoff::FAST.start;
        loop {
            let reason = match self.resolve_strict().await {
                Ok(Some(element)) => return Ok(element),
                Ok(None) => "no element matches".to_string(),
                Err(error) if is_strictness(&error) => return Err(error),
                Err(error) => format!("{error:#}"),
            };
            if start.elapsed() > timeout {
                bail!("{}: {reason} (after {:?})", self.selector, start.elapsed());
            }
            tokio::time::sleep(gap).await;
            gap = Backoff::FAST.next_gap(gap);
        }
    }

    fn timeout(&self, explicit: Option<Duration>) -> Duration {
        explicit.unwrap_or_else(|| self.page.default_timeout())
    }

    /// Resolve, scroll, and wait until the element is settled and would receive a
    /// pointer event at its centre.
    async fn actionable(&self, action: &str, checks: Checks, timeout: Duration) -> Result<Aim> {
        let start = Instant::now();
        let hover = self
            .page
            .hooks()
            .hover_reveal_ancestor()
            .map(str::to_string);
        let mut reason = String::from("no element matches");
        // A block resets the stability check, so the final reason alone would read
        // "still moving" and hide what actually stood in the way.
        let mut blocked: Option<String> = None;
        let mut last_rect: Option<[f64; 4]> = None;
        let mut gap = Backoff::FAST.start;
        loop {
            if start.elapsed() > timeout {
                let before = blocked
                    .filter(|blocked| *blocked != reason)
                    .map(|blocked| format!("; before that: {blocked}"))
                    .unwrap_or_default();
                bail!(
                    "{action} {}: {reason}{before} (after {:?})",
                    self.selector,
                    start.elapsed()
                );
            }
            match self.attempt(checks, hover.as_deref(), &mut last_rect).await {
                Ok(Ok(found)) => return Ok(found),
                Ok(Err(Pending::Moving(why))) => reason = why,
                Ok(Err(Pending::Blocked(why))) => {
                    reason = why.clone();
                    blocked = Some(why);
                }
                Err(error) if is_strictness(&error) => return Err(error),
                Err(error) => reason = format!("{error:#}"),
            }
            tokio::time::sleep(gap).await;
            gap = Backoff::FAST.next_gap(gap);
        }
    }

    async fn attempt(
        &self,
        checks: Checks,
        hover: Option<&str>,
        last_rect: &mut Option<[f64; 4]>,
    ) -> Result<std::result::Result<Aim, Pending>> {
        let Some(element) = self.resolve_strict().await? else {
            *last_rect = None;
            return Ok(Err(Pending::Blocked("no element matches".to_string())));
        };
        if !checks.force {
            let scrolled: Result<bool> = self
                .page
                .call_on(
                    &element,
                    "(lib, el) => { lib.prescroll(el); return true; }",
                    Vec::new(),
                )
                .await;
            if let Err(error) = scrolled {
                tracing::debug!("prescroll of {} failed: {error:#}", self.selector);
            }
            let probe: Probe = self
                .page
                .call_on(
                    &element,
                    "(lib, el, hover, enabled) => lib.probe(el, hover, enabled)",
                    vec![
                        hover.map_or(JsArg::Null, |selector| JsArg::Str(selector.to_string())),
                        JsArg::Bool(checks.enabled),
                    ],
                )
                .await?;
            if !probe.ok {
                *last_rect = None;
                return Ok(Err(Pending::Blocked(
                    probe.reason.unwrap_or_else(|| "not actionable".to_string()),
                )));
            }
            // The element must hold still across two probes: a click aimed at a
            // moving target lands on whatever slides into its place.
            if *last_rect != probe.rect {
                let why = format!("still moving ({:?} -> {:?})", last_rect, probe.rect);
                *last_rect = probe.rect;
                return Ok(Err(Pending::Moving(why)));
            }
        }
        let hit: Hit = self
            .page
            .call_on(&element, "(lib, el) => lib.hitPoint(el)", Vec::new())
            .await?;
        if hit.outside {
            let scrolled: Result<bool> = self
                .page
                .call_on(
                    &element,
                    "(lib, el) => { lib.scrollIntoViewIfNeeded(el); return true; }",
                    Vec::new(),
                )
                .await;
            if let Err(error) = scrolled {
                tracing::debug!("scrolling {} into view failed: {error:#}", self.selector);
            }
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        let local = (hit.x, hit.y);
        let hit = self.page.lift(&element.frame, hit).await?;
        if hit.outside {
            if let Err(error) = self.page.reveal(&element.frame, local.0, local.1).await {
                tracing::debug!("revealing the frame of {} failed: {error:#}", self.selector);
            }
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        if checks.hit && !hit.ok && !checks.force {
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        Ok(Ok(Aim {
            element,
            local,
            top: (hit.x, hit.y),
        }))
    }

    // ── actions ────────────────────────────────────────────────────────────

    pub async fn click(&self) -> Result<()> {
        self.click_with(ClickOptions::default()).await
    }

    pub async fn dblclick(&self) -> Result<()> {
        self.click_with(ClickOptions {
            click_count: 2,
            ..ClickOptions::default()
        })
        .await
    }

    pub async fn click_with(&self, options: ClickOptions) -> Result<()> {
        let timeout = self.timeout(options.timeout);
        let start = Instant::now();
        let modifiers = options
            .modifiers
            .iter()
            .map(|name| parse_chord(name).map(|chord| chord.key))
            .collect::<Result<Vec<_>>>()?;
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            let checks = Checks {
                enabled: true,
                hit: true,
                force: options.force,
            };
            let aim = self.actionable("click", checks, remaining).await?;
            let (x, y) = aim.local;
            match self
                .page
                .click_at(
                    ClickPoint {
                        frame: &aim.element.frame,
                        anchor: Some(aim.anchor()),
                        x,
                        y,
                    },
                    options.button,
                    options.click_count.max(1),
                    &modifiers,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) if start.elapsed() < timeout => {
                    tracing::debug!("click on {} failed, retrying: {error:#}", self.selector);
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("click {}", self.selector));
                }
            }
        }
    }

    pub async fn hover(&self) -> Result<()> {
        let checks = Checks {
            enabled: false,
            hit: true,
            force: false,
        };
        let aim = self.actionable("hover", checks, self.timeout(None)).await?;
        let (x, y) = aim.local;
        self.page
            .pointer(
                &aim.element.frame,
                Some(aim.anchor()),
                vec![PointerAction::PointerMove {
                    x,
                    y,
                    duration: None,
                    origin: "viewport",
                }],
            )
            .await
    }

    /// Replace the element's value with `text`, typing it as key events.
    pub async fn fill(&self, text: &str) -> Result<()> {
        let checks = Checks {
            enabled: true,
            hit: false,
            force: false,
        };
        let element = self
            .actionable("fill", checks, self.timeout(None))
            .await?
            .element;
        let direct: bool = self
            .page
            .call_on(
                &element,
                "(lib, el, value) => lib.fillDirect(el, value)",
                vec![JsArg::Str(text.to_string())],
            )
            .await
            .with_context(|| format!("fill {}", self.selector))?;
        if direct {
            return Ok(());
        }
        let kind: String = self
            .page
            .call_on(&element, "(lib, el) => lib.clearForTyping(el)", Vec::new())
            .await
            .with_context(|| format!("clear {}", self.selector))?;
        if kind == "other" {
            bail!(
                "{} is not an input, textarea or contenteditable element",
                self.selector
            );
        }
        if text.is_empty() {
            return Ok(());
        }
        self.page
            .keyboard_in(element.frame)
            .type_text(text)
            .await
            .with_context(|| format!("type into {}", self.selector))
    }

    pub async fn clear(&self) -> Result<()> {
        self.fill("").await
    }

    /// Focus the element and type `text` without clearing it first.
    pub async fn press_sequentially(&self, text: &str) -> Result<()> {
        let frame = self.focused().await?;
        self.page.keyboard_in(frame).type_text(text).await
    }

    /// Focus the element and press a key or chord.
    pub async fn press(&self, combo: &str) -> Result<()> {
        let frame = self.focused().await?;
        self.page.keyboard_in(frame).press(combo).await
    }

    pub async fn focus(&self) -> Result<()> {
        self.focused().await.map(|_| ())
    }

    /// Focus the element; returns its frame, where key input must go.
    async fn focused(&self) -> Result<Frame> {
        let element = self.wait_one(self.timeout(None)).await?;
        let focused: bool = self
            .page
            .call_on(
                &element,
                "(lib, el) => { el.focus(); return true; }",
                Vec::new(),
            )
            .await
            .with_context(|| format!("focus {}", self.selector))?;
        if !focused {
            bail!("{} could not be focused", self.selector);
        }
        Ok(element.frame)
    }

    pub async fn check(&self) -> Result<()> {
        self.set_checked(true).await
    }

    pub async fn uncheck(&self) -> Result<()> {
        self.set_checked(false).await
    }

    pub async fn set_checked(&self, checked: bool) -> Result<()> {
        if self.info().await?.checked == Some(checked) {
            return Ok(());
        }
        self.click().await?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if self.info().await?.checked == Some(checked) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bail!(
            "clicking {} did not make it {}",
            self.selector,
            if checked { "checked" } else { "unchecked" }
        )
    }

    /// Select `<option>`s by value or label; returns the selected values.
    pub async fn select_option(&self, values: &[&str]) -> Result<Vec<String>> {
        let checks = Checks {
            enabled: true,
            hit: false,
            force: false,
        };
        let element = self
            .actionable("select", checks, self.timeout(None))
            .await?
            .element;
        self.page
            .call_on(
                &element,
                "(lib, el, values) => lib.selectOptions(el, values)",
                vec![JsArg::Strs(
                    values.iter().map(|value| value.to_string()).collect(),
                )],
            )
            .await
            .with_context(|| format!("select {values:?} in {}", self.selector))
    }

    /// Set the files of an `<input type=file>`; it may be hidden.
    pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page.set_files(&element, paths.to_vec()).await
    }

    /// Drag this element onto `target` with real pointer moves. An HTML5 drag the
    /// driver cannot perform natively, including every drag between frames, is
    /// carried out by firing the drag events in the pages.
    pub async fn drag_to(&self, target: &Locator) -> Result<()> {
        let source_checks = Checks {
            enabled: false,
            hit: true,
            force: false,
        };
        let from = self
            .actionable("drag", source_checks, self.timeout(None))
            .await?;
        let target_checks = Checks {
            enabled: false,
            hit: false,
            force: false,
        };
        let to = target
            .actionable("drop onto", target_checks, self.timeout(None))
            .await?;
        let html5: bool = self
            .page
            .call_on(
                &from.element,
                "(lib, el) => !!lib.dragSource(el)",
                Vec::new(),
            )
            .await
            .with_context(|| format!("inspect {} as a drag source", self.selector))?;
        let same_frame = from.element.frame == to.element.frame;
        if html5 && !same_frame {
            return self.drop_by_events(&from, &to, target).await;
        }
        if html5 {
            self.page
                .call_on::<()>(&from.element, "(lib) => lib.watchDrag()", Vec::new())
                .await
                .context("watch for a native drag")?;
        }
        // Across frames a real mouse stays one gesture in top-level coordinates.
        let (frame, anchor, start, end) = if same_frame {
            (
                from.element.frame.clone(),
                Some(from.anchor()),
                from.local,
                to.local,
            )
        } else {
            (Frame::top(), None, from.top, to.top)
        };
        let mut actions = vec![
            pointer_move(start, None),
            PointerAction::PointerDown { button: 0 },
            PointerAction::Pause { duration: 50 },
        ];
        actions.extend(glide(start, end, 1..=DRAG_STEPS));
        actions.push(PointerAction::PointerUp { button: 0 });
        self.page
            .pointer(&frame, anchor, actions)
            .await
            .with_context(|| format!("drag {} to {}", self.selector, target.selector))?;
        if !html5 {
            return Ok(());
        }
        let native: bool = self
            .page
            .call_on(
                &from.element,
                "(lib) => lib.nativeDragEnded(500)",
                Vec::new(),
            )
            .await
            .context("check for a native drag")?;
        if native {
            return Ok(());
        }
        self.drop_by_events(&from, &to, target).await
    }

    async fn drop_by_events(&self, from: &Aim, to: &Aim, target: &Locator) -> Result<()> {
        let started: DragStart = self
            .page
            .call_on(
                &from.element,
                "(lib, el, x, y) => lib.startDrag(el, x, y)",
                vec![JsArg::Int(from.local.0), JsArg::Int(from.local.1)],
            )
            .await
            .with_context(|| format!("start dragging {}", self.selector))?;
        if !started.started {
            bail!("{} cancelled its dragstart", self.selector);
        }
        let items = serde_json::to_string(&started.items).context("carry the drag data")?;
        let effect: Result<String> = self
            .page
            .call_on(
                &to.element,
                "(lib, el, x, y, items, allowed) => lib.dropAt(el, x, y, JSON.parse(items), allowed)",
                vec![
                    JsArg::Int(to.local.0),
                    JsArg::Int(to.local.1),
                    JsArg::Str(items),
                    JsArg::Str(started.effect_allowed),
                ],
            )
            .await
            .with_context(|| format!("drop onto {}", target.selector));
        let dropped = effect
            .as_ref()
            .map_or_else(|_| "none".to_string(), Clone::clone);
        let ended: Result<()> = self
            .page
            .call_on(
                &from.element,
                "(lib, el, x, y, effect) => lib.endDrag(x, y, effect)",
                vec![
                    JsArg::Int(from.local.0),
                    JsArg::Int(from.local.1),
                    JsArg::Str(dropped),
                ],
            )
            .await;
        let effect = effect?;
        ended.with_context(|| format!("end the drag of {}", self.selector))?;
        if effect == "none" {
            bail!("{} did not accept the drop", target.selector);
        }
        Ok(())
    }

    pub async fn scroll_into_view(&self) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        let hit: Hit = self
            .page
            .call_on(
                &element,
                "(lib, el) => { lib.prescroll(el); if (lib.hitPoint(el).outside) lib.scrollIntoViewIfNeeded(el); return lib.hitPoint(el); }",
                Vec::new(),
            )
            .await
            .with_context(|| format!("scroll {} into view", self.selector))?;
        self.page
            .reveal(&element.frame, hit.x, hit.y)
            .await
            .with_context(|| format!("scroll the frame of {} into view", self.selector))
    }

    /// PNG of the element.
    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        self.scroll_into_view().await?;
        let element = self.wait_one(self.timeout(None)).await?;
        self.page.element_screenshot(&element).await
    }

    /// Press `key` while this element has focus, as a single key event pair.
    pub async fn dispatch_key(&self, key: &str) -> Result<()> {
        let frame = self.focused().await?;
        let chord = parse_chord(key)?;
        self.page
            .keys(
                &frame,
                vec![
                    KeyAction::KeyDown {
                        value: chord.key.clone(),
                    },
                    KeyAction::KeyUp { value: chord.key },
                ],
            )
            .await
    }

    // ── queries ────────────────────────────────────────────────────────────

    pub async fn count(&self) -> Result<usize> {
        Ok(self.resolve_all().await?.len())
    }

    /// Everything the runtime knows about the (single) element; waits for it to exist.
    pub async fn info(&self) -> Result<ElementInfo> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .call_on(&element, "(lib, el) => lib.info(el)", Vec::new())
            .await
            .with_context(|| format!("inspect {}", self.selector))
    }

    pub async fn is_visible(&self) -> Result<bool> {
        match self.resolve_strict().await? {
            None => Ok(false),
            Some(element) => {
                self.page
                    .call_on(&element, "(lib, el) => lib.isVisible(el)", Vec::new())
                    .await
            }
        }
    }

    pub async fn is_enabled(&self) -> Result<bool> {
        Ok(self.info().await?.enabled)
    }

    pub async fn is_checked(&self) -> Result<bool> {
        Ok(self.info().await?.checked.unwrap_or(false))
    }

    pub async fn inner_text(&self) -> Result<String> {
        Ok(self.info().await?.text)
    }

    pub async fn text_content(&self) -> Result<String> {
        self.eval_on("el => el.textContent || ''").await
    }

    pub async fn input_value(&self) -> Result<String> {
        self.info()
            .await?
            .value
            .with_context(|| format!("{} has no value", self.selector))
    }

    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .evaluate_with(
                &element.frame,
                "(el, name) => el.getAttribute(name)",
                vec![JsArg::El(element.clone()), JsArg::Str(name.to_string())],
            )
            .await
    }

    /// Text of every match.
    pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
        let mut texts = Vec::new();
        for element in self.resolve_all().await? {
            let info: ElementInfo = self
                .page
                .call_on(&element, "(lib, el) => lib.info(el)", Vec::new())
                .await?;
            texts.push(info.text);
        }
        Ok(texts)
    }

    /// Call `function(element)` in the page and decode its result.
    pub async fn eval_on<T: DeserializeOwned>(&self, function: &str) -> Result<T> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .evaluate_with(&element.frame, function, vec![JsArg::El(element.clone())])
            .await
            .with_context(|| format!("evaluate {} on {}", abbreviate(function), self.selector))
    }

    /// Aria snapshot rooted at this element.
    pub async fn snapshot(&self) -> Result<String> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .render_snapshot(&element.frame.clone(), Some(element))
            .await
    }

    /// Wait for the element to reach `state`.
    pub async fn wait_for(&self, state: ElementState, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        let mut gap = Backoff::FAST.start;
        loop {
            let reached = match state {
                ElementState::Attached => self.count().await.is_ok_and(|count| count > 0),
                ElementState::Detached => self.count().await.is_ok_and(|count| count == 0),
                ElementState::Visible => self.first().is_visible().await.unwrap_or(false),
                ElementState::Hidden => !self.first().is_visible().await.unwrap_or(true),
            };
            if reached {
                return Ok(());
            }
            if start.elapsed() > timeout {
                bail!(
                    "{} did not become {state:?} within {:?}",
                    self.selector,
                    start.elapsed()
                );
            }
            tokio::time::sleep(gap).await;
            gap = Backoff::FAST.next_gap(gap);
        }
    }
}

/// A locator used for a single-element operation matched several elements.
#[derive(Debug)]
pub struct StrictModeViolation {
    pub selector: String,
    pub count: usize,
    pub matches: Vec<String>,
}

impl std::fmt::Display for StrictModeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "strict mode: {} matched {} elements ({}); refine it or use .first()/.nth()",
            self.selector,
            self.count,
            self.matches.join(", ")
        )
    }
}

impl std::error::Error for StrictModeViolation {}

fn is_strictness(error: &anyhow::Error) -> bool {
    error.downcast_ref::<StrictModeViolation>().is_some()
}
