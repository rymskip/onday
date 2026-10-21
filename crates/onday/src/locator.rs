//! Lazy element locators with auto-waiting actions.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::frame::Frame;
use crate::keys::parse_chord;
use crate::page::{
    Anchor, ClickPoint, ElementRef, Hit, JsArg, MouseButton, Page, Resolution, abbreviate, quote,
};
use crate::poll::{Backoff, poll_until_ok};
use crate::proto::{KeyAction, PointerAction};
use crate::typing::{TypingState, typed_in_order};

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

/// An element's box in its frame's viewport, in whole CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct BoundingBox {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
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

/// Whether the browser ran a drag of its own, by its trusted events.
#[derive(Debug, Clone, Copy, Deserialize)]
struct NativeDrag {
    started: bool,
    ended: bool,
}

impl NativeDrag {
    fn describe(self) -> &'static str {
        if self.started {
            "a native drag started and had not ended 500ms after the release"
        } else {
            "no native drag started"
        }
    }
}

/// What a drop fired in the target's page came to.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DropOutcome {
    effect: String,
    /// What lay under the point and refused the drop, when it was refused.
    refused_by: Option<String>,
}

/// What firing `dragstart` in the source's page produced.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DragStart {
    started: bool,
    items: Vec<(String, String)>,
    effect_allowed: String,
}

fn pointer_move((horizontal, vertical): (i64, i64)) -> PointerAction {
    PointerAction::Move {
        horizontal,
        vertical,
        origin: "viewport",
    }
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
            horizontal: self.local.0,
            vertical: self.local.1,
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

/// Where each attempt scrolls its target, in turn, as Playwright's retries do:
/// first only as far as it is out of view (`None`), then forced to end, centre
/// and start, which is how a sticky bar over the target is cleared. Leaving a
/// visible target where it is keeps it under a pointer that revealed it.
const SCROLL_ALIGNMENTS: [Option<&str>; 4] = [None, Some("end"), Some("center"), Some("start")];

/// The alignment for attempt `turn`, as the argument `lib.scrollAligned` reads.
fn alignment(turn: usize) -> JsArg {
    match SCROLL_ALIGNMENTS
        .iter()
        .cycle()
        .nth(turn)
        .copied()
        .flatten()
    {
        Some(block) => JsArg::Str(block.to_string()),
        None => JsArg::Null,
    }
}

/// Playwright's waits before a pointer action's second, third and later attempts.
const RETRY_WAITS: [Duration; 5] = [
    Duration::ZERO,
    Duration::from_millis(20),
    Duration::from_millis(100),
    Duration::from_millis(100),
    Duration::from_millis(500),
];

/// Which of a pointer action's events the page guards, as Playwright's
/// hit-target interceptor kinds.
#[derive(Debug, Clone, Copy)]
enum Guard {
    Hover,
    Mouse,
    /// Only the aimed point is checked: the dragged element often stays under
    /// the cursor through the drop.
    Drop,
}

impl Guard {
    fn kind(self) -> &'static str {
        match self {
            Self::Hover => "hover",
            Self::Mouse => "mouse",
            Self::Drop => "drag",
        }
    }
}

/// A pointer action as Playwright's `_retryPointerAction` takes it.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    /// Whether the target must also be enabled and not inert.
    enabled: bool,
    force: bool,
    guard: Guard,
    /// Whether a hit on the label bound to the target counts: onday's typing
    /// click only, with no Playwright counterpart.
    follow_label: bool,
}

/// Why a pointer action's attempts were abandoned, each reason once and in
/// order, as Playwright's call log reports them when the action times out.
#[derive(Debug, Default)]
struct AttemptLog(Vec<String>);

impl AttemptLog {
    fn record(&mut self, why: String) {
        if !self.0.contains(&why) {
            self.0.push(why);
        }
    }
}

impl std::fmt::Display for AttemptLog {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return formatter.write_str("not attempted");
        }
        formatter.write_str(&self.0.join("; "))
    }
}

/// One attempt at a pointer action, up to its input.
enum Attempt {
    Aimed(Aim),
    /// Why the attempt was abandoned; the action retries.
    Retry(String),
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
    /// How long this locator's actions and waits may take, when not the page's default.
    timeout: Option<Duration>,
}

impl Locator {
    pub(crate) fn new(page: Page, selector: String) -> Locator {
        Locator {
            page,
            selector,
            timeout: None,
        }
    }

    /// The same locator with `timeout` as the budget of its actions and waits,
    /// in place of the page's default. Locators refined from it keep it.
    pub fn with_timeout(&self, timeout: Duration) -> Locator {
        Locator {
            page: self.page.clone(),
            selector: self.selector.clone(),
            timeout: Some(timeout),
        }
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
        Locator {
            page: self.page.clone(),
            selector: format!("{} >> {selector}", self.selector),
            timeout: self.timeout,
        }
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
        explicit
            .or(self.timeout)
            .unwrap_or_else(|| self.page.default_timeout())
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
        let mut scroll_turn = 0usize;
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
            match self
                .attempt(checks, hover.as_deref(), &mut last_rect, &mut scroll_turn)
                .await
            {
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
        scroll_turn: &mut usize,
    ) -> Result<std::result::Result<Aim, Pending>> {
        let Some(element) = self.resolve_strict().await? else {
            *last_rect = None;
            return Ok(Err(Pending::Blocked("no element matches".to_string())));
        };
        if !checks.force {
            // As Playwright's actions do: visible and stable first, where the
            // page leaves it, and only then scrolled. Measured after this
            // action's own scroll, a page that puts its scroll back between
            // attempts would read as an element that never holds still.
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
            let scrolled: Result<bool> = self
                .page
                .call_on(
                    &element,
                    "(lib, el, block) => { lib.scrollAligned(el, block); return true; }",
                    vec![alignment(*scroll_turn)],
                )
                .await;
            if let Err(error) = scrolled {
                tracing::debug!("scrolling {} into view failed: {error:#}", self.selector);
            }
        }
        let hit: Hit = self
            .page
            .call_on(&element, "(lib, el) => lib.hitPoint(el)", Vec::new())
            .await?;
        if hit.outside {
            *scroll_turn = scroll_turn.saturating_add(1);
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        let local = (hit.horizontal, hit.vertical);
        let hit = self.page.lift(&element.frame, hit).await?;
        if hit.outside {
            if let Err(error) = self.page.reveal(&element.frame, local.0, local.1).await {
                tracing::debug!("revealing the frame of {} failed: {error:#}", self.selector);
            }
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        if checks.hit && !hit.ok && !checks.force {
            // The next attempt scrolls with the next alignment: a sticky bar or a
            // box clipping the element can cover where this one left it.
            *scroll_turn = scroll_turn.saturating_add(1);
            *last_rect = None;
            return Ok(Err(Pending::Blocked(hit.reason)));
        }
        Ok(Ok(Aim {
            element,
            local,
            top: (hit.horizontal, hit.vertical),
        }))
    }

    /// Before the attempt numbered `retry`: Playwright's wait, or the timeout
    /// naming why the last attempt was abandoned.
    async fn before_attempt(
        &self,
        action: &str,
        retry: usize,
        started: Instant,
        timeout: Duration,
        last: &AttemptLog,
    ) -> Result<()> {
        if started.elapsed() > timeout {
            bail!(
                "{action} {}: {last} (after {:?})",
                self.selector,
                started.elapsed()
            );
        }
        if let Some(index) = retry.checked_sub(1) {
            let wait = RETRY_WAITS
                .get(index)
                .or(RETRY_WAITS.last())
                .copied()
                .unwrap_or_default();
            tokio::time::sleep(wait).await;
        }
        Ok(())
    }

    /// One attempt as Playwright's `_performPointerAction`, up to the input: the
    /// target stable and visible (and enabled), scrolled with this attempt's
    /// alignment, its clickable point found, the hit there checked and the
    /// page's guard raised.
    /// One attempt at aiming, where an error is another reason to try again:
    /// as Playwright's retry loop does, only a strictness violation or a
    /// browser that is gone ends the action. A frame that replaces its
    /// document while the action resolves into it, as a card-field iframe does
    /// while it starts, is retried until the action's timeout.
    async fn aim_pointer(&self, pointer: Pointer, retry: usize) -> Result<Attempt> {
        match self.aim_pointer_once(pointer, retry).await {
            Ok(attempt) => Ok(attempt),
            Err(error) if is_strictness(&error) || is_browser_gone(&error) => Err(error),
            Err(error) => Ok(Attempt::Retry(format!("{error:#}"))),
        }
    }

    async fn aim_pointer_once(&self, pointer: Pointer, retry: usize) -> Result<Attempt> {
        let Some(element) = self.resolve_strict().await? else {
            return Ok(Attempt::Retry("no element matches".to_string()));
        };
        if !pointer.force {
            let state: String = self
                .page
                .call_on(
                    &element,
                    "(lib, el, enabled) => lib.pointerStates(el, enabled)",
                    vec![JsArg::Bool(pointer.enabled)],
                )
                .await?;
            match state.as_str() {
                "ok" => {}
                "notconnected" => {
                    return Ok(Attempt::Retry(
                        "element is not attached to the DOM".to_string(),
                    ));
                }
                missing => return Ok(Attempt::Retry(format!("element is not {missing}"))),
            }
        }
        if let Err(error) = self
            .page
            .call_on::<bool>(
                &element,
                "(lib, el, block) => { lib.scrollAligned(el, block); return true; }",
                vec![alignment(retry)],
            )
            .await
        {
            return Ok(Attempt::Retry(format!(
                "scrolling it into view failed: {error:#}"
            )));
        }
        let point: Hit = self
            .page
            .call_on(&element, "(lib, el) => lib.clickablePoint(el)", Vec::new())
            .await?;
        if !point.ok {
            return Ok(Attempt::Retry(format!("element is {}", point.reason)));
        }
        let local = (point.horizontal, point.vertical);
        let top = self.page.lift(&element.frame, point).await?;
        if top.outside {
            if let Err(error) = self.page.reveal(&element.frame, local.0, local.1).await {
                tracing::debug!("revealing the frame of {} failed: {error:#}", self.selector);
            }
            return Ok(Attempt::Retry(top.reason));
        }
        if !pointer.force {
            let guarded: String = self
                .page
                .call_on(
                    &element,
                    "(lib, el, kind, x, y, followLabel) => lib.guardHits(el, kind, x, y, followLabel)",
                    vec![
                        JsArg::Str(pointer.guard.kind().to_string()),
                        JsArg::Int(local.0),
                        JsArg::Int(local.1),
                        JsArg::Bool(pointer.follow_label),
                    ],
                )
                .await?;
            if guarded != "done" {
                return Ok(Attempt::Retry(guarded));
            }
        }
        Ok(Attempt::Aimed(Aim {
            element,
            local,
            top: (top.horizontal, top.vertical),
        }))
    }

    /// What the guard raised for `aim` found once the input ran: `Some` names
    /// what took the gesture. An action that navigates leaves no page to ask,
    /// which counts as a hit, as in Playwright.
    async fn lift_guard(&self, aim: &Aim, pointer: Pointer) -> Option<String> {
        if pointer.force {
            return None;
        }
        match self
            .page
            .call_json::<String>(&aim.element.frame, "(lib) => lib.endHitGuard()", Vec::new())
            .await
        {
            Ok(found) => (found != "done").then_some(found),
            Err(error) => {
                tracing::debug!(
                    "reading the hit guard for {} failed: {error:#}",
                    self.selector
                );
                None
            }
        }
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
        let started = Instant::now();
        let modifiers = options
            .modifiers
            .iter()
            .map(|name| parse_chord(name).map(|chord| chord.key))
            .collect::<Result<Vec<_>>>()?;
        let pointer = Pointer {
            enabled: true,
            force: options.force,
            guard: Guard::Mouse,
            follow_label: false,
        };
        let mut last = AttemptLog::default();
        for retry in 0.. {
            self.before_attempt("click", retry, started, timeout, &last)
                .await?;
            let aim = match self.aim_pointer(pointer, retry).await? {
                Attempt::Aimed(aim) => aim,
                Attempt::Retry(why) => {
                    last.record(why);
                    continue;
                }
            };
            let (horizontal, vertical) = aim.local;
            let clicked = self
                .page
                .click_at(
                    ClickPoint {
                        frame: &aim.element.frame,
                        anchor: Some(aim.anchor()),
                        horizontal,
                        vertical,
                    },
                    options.button,
                    options.click_count.max(1),
                    &modifiers,
                )
                .await;
            let missed = self.lift_guard(&aim, pointer).await;
            clicked.with_context(|| format!("click {}", self.selector))?;
            match missed {
                None => return Ok(()),
                Some(why) => last.record(why),
            }
        }
        bail!("click {}: {last}", self.selector)
    }

    pub async fn hover(&self) -> Result<()> {
        let timeout = self.timeout(None);
        let started = Instant::now();
        let pointer = Pointer {
            enabled: false,
            force: false,
            guard: Guard::Hover,
            follow_label: false,
        };
        let mut last = AttemptLog::default();
        for retry in 0.. {
            self.before_attempt("hover", retry, started, timeout, &last)
                .await?;
            let aim = match self.aim_pointer(pointer, retry).await? {
                Attempt::Aimed(aim) => aim,
                Attempt::Retry(why) => {
                    last.record(why);
                    continue;
                }
            };
            let (horizontal, vertical) = aim.local;
            let moved = self
                .page
                .pointer(
                    &aim.element.frame,
                    Some(aim.anchor()),
                    vec![pointer_move((horizontal, vertical))],
                )
                .await;
            let missed = self.lift_guard(&aim, pointer).await;
            moved.with_context(|| format!("hover {}", self.selector))?;
            match missed {
                None => return Ok(()),
                Some(why) => last.record(why),
            }
        }
        bail!("hover {}: {last}", self.selector)
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

    /// Replace what the field holds with `text` the way a person does: a pointer
    /// click at its centre, a check that focus landed there, select-all and one
    /// Backspace, the keys, then every typed character required in the field in
    /// order, so a mask passes. Native-picker and valueless inputs are refused.
    pub async fn type_into(&self, text: &str) -> Result<()> {
        let timeout = self.timeout(None);
        let start = Instant::now();
        // A pointer action, so the click must land on the field, as Playwright's
        // hit-target check requires; a label bound to it counts as the field.
        let pointer = Pointer {
            enabled: true,
            force: false,
            guard: Guard::Mouse,
            follow_label: true,
        };
        let mut last = AttemptLog::default();
        let mut found = None;
        for retry in 0.. {
            self.before_attempt("type into", retry, start, timeout, &last)
                .await?;
            let aim = match self.aim_pointer(pointer, retry).await? {
                Attempt::Aimed(aim) => aim,
                Attempt::Retry(why) => {
                    last.record(why);
                    continue;
                }
            };
            let before = self.typing_state(timeout).await?;
            before.ensure_typable(&self.selector)?;
            let (horizontal, vertical) = aim.local;
            let clicked = self
                .page
                .click_at(
                    ClickPoint {
                        frame: &aim.element.frame,
                        anchor: Some(aim.anchor()),
                        horizontal,
                        vertical,
                    },
                    MouseButton::Left,
                    1,
                    &[],
                )
                .await;
            let missed = self.lift_guard(&aim, pointer).await;
            clicked.with_context(|| format!("pointer click on {}", self.selector))?;
            match missed {
                None => {
                    found = Some((aim, before));
                    break;
                }
                Some(why) => last.record(why),
            }
        }
        let Some((aim, before)) = found else {
            bail!("type into {}: {last}", self.selector);
        };
        let focused = self.typing_state(timeout).await?;
        if !focused.focused {
            bail!(
                "{} did not take focus: the click landed on {}, focus is on {}",
                self.selector,
                before.on_top,
                focused.active
            );
        }
        let keyboard = self.page.keyboard_in(aim.element.frame.clone());
        if !focused.text.is_empty() {
            // One selection delete, so a mask sees a single edit and an input handler fires once.
            keyboard
                .press("ControlOrMeta+a")
                .await
                .with_context(|| format!("select what {} holds", self.selector))?;
            keyboard
                .press("Backspace")
                .await
                .with_context(|| format!("clear {}", self.selector))?;
            let cleared = self.typing_state(timeout).await?;
            if !cleared.text.is_empty() {
                bail!("clearing {} left {:?}", self.selector, cleared.text);
            }
        }
        if !text.is_empty() {
            keyboard
                .type_text(text)
                .await
                .with_context(|| format!("type into {}", self.selector))?;
        }
        poll_until_ok(
            || async {
                let landed = self.typing_state(timeout).await?.text;
                if typed_in_order(text, &landed) {
                    return Ok(Some(()));
                }
                Err(anyhow!(
                    "{} holds {landed:?} after typing {text:?}",
                    self.selector
                ))
            },
            timeout.saturating_sub(start.elapsed()),
            Backoff::FAST,
        )
        .await
        .map_err(|last| {
            last.unwrap_or_else(|| {
                anyhow!("{}: the typed value was never read back", self.selector)
            })
        })
    }

    /// Re-resolved on every read: typing may re-render the field.
    async fn typing_state(&self, timeout: Duration) -> Result<TypingState> {
        let element = self.wait_one(timeout).await?;
        self.page
            .call_on(&element, "(lib, el) => lib.typingState(el)", Vec::new())
            .await
            .with_context(|| format!("read the typing state of {}", self.selector))
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
        let timeout = self.timeout(None);
        let started = Instant::now();
        let target_frame = target.wait_one(target.timeout(None)).await?.frame;
        // As Playwright's drag: the source pressed in one retried action, guarded
        // as a click, and the target aimed only once the button is down (pressing
        // can move it), its point checked before the release.
        let press = Pointer {
            enabled: false,
            force: false,
            guard: Guard::Mouse,
            follow_label: false,
        };
        let drop = Pointer {
            enabled: false,
            force: false,
            guard: Guard::Drop,
            follow_label: false,
        };
        let mut last = AttemptLog::default();
        let mut pressed = None;
        for retry in 0.. {
            self.before_attempt("drag", retry, started, timeout, &last)
                .await?;
            let from = match self.aim_pointer(press, retry).await? {
                Attempt::Aimed(aim) => aim,
                Attempt::Retry(why) => {
                    last.record(why);
                    continue;
                }
            };
            let html5: bool = self
                .page
                .call_on(
                    &from.element,
                    "(lib, el) => !!lib.dragSource(el)",
                    Vec::new(),
                )
                .await
                .with_context(|| format!("inspect {} as a drag source", self.selector))?;
            let same_frame = from.element.frame == target_frame;
            if html5 && !same_frame {
                // No driver carries a drag between frames: the drag events are
                // fired in the pages instead, with no press to guard.
                self.lift_guard(&from, press).await;
                let to = target.aim_drop(drop, started, timeout).await?;
                return self.drop_by_events(&from, &to, target, None).await;
            }
            if html5 {
                self.page
                    .call_on::<()>(&from.element, "(lib) => lib.watchDrag()", Vec::new())
                    .await
                    .context("watch for a native drag")?;
            }
            // Across frames a real mouse stays one gesture in top-level coordinates.
            let (frame, anchor, start) = if same_frame {
                (from.element.frame.clone(), Some(from.anchor()), from.local)
            } else {
                (Frame::top(), None, from.top)
            };
            self.page
                .pointer(&frame, anchor, vec![pointer_move(start)])
                .await
                .with_context(|| format!("move onto {}", self.selector))?;
            self.page
                .pointer(&frame, anchor, vec![PointerAction::Down { button: 0 }])
                .await
                .with_context(|| format!("press on {}", self.selector))?;
            match self.lift_guard(&from, press).await {
                None => {
                    pressed = Some((from, html5, same_frame, frame));
                    break;
                }
                Some(why) => last.record(why),
            }
        }
        let Some((from, html5, same_frame, frame)) = pressed else {
            bail!("drag {}: {last}", self.selector);
        };
        let to = target.aim_drop(drop, started, timeout).await?;
        // Classic input is anchored to the element it heads for, which the
        // driver scrolls into view: anchored to the source, the move would scroll
        // the source back and leave the target where it was aimed.
        let (end, anchor) = if same_frame {
            (to.local, Some(to.anchor()))
        } else {
            (to.top, None)
        };
        self.page
            .pointer(&frame, anchor, vec![pointer_move(end)])
            .await
            .with_context(|| format!("move onto {}", target.selector))?;
        self.page
            .pointer(&frame, anchor, vec![PointerAction::Up { button: 0 }])
            .await
            .with_context(|| format!("release on {}", target.selector))?;
        if !html5 {
            return Ok(());
        }
        let native: NativeDrag = self
            .page
            .call_on(&from.element, "(lib) => lib.nativeDrag(500)", Vec::new())
            .await
            .context("check for a native drag")?;
        if native.ended {
            return Ok(());
        }
        self.drop_by_events(&from, &to, target, Some(native)).await
    }

    /// The drop target aimed as Playwright's `move and up` aims it, retried
    /// until `timeout` from `started`.
    async fn aim_drop(&self, drop: Pointer, started: Instant, timeout: Duration) -> Result<Aim> {
        let mut last = AttemptLog::default();
        for retry in 0.. {
            self.before_attempt("drop onto", retry, started, timeout, &last)
                .await?;
            match self.aim_pointer(drop, retry).await? {
                Attempt::Aimed(aim) => {
                    self.lift_guard(&aim, drop).await;
                    return Ok(aim);
                }
                Attempt::Retry(why) => last.record(why),
            }
        }
        bail!("drop onto {}: {last}", self.selector)
    }

    /// Fire the drag in the pages. `native` is what the browser's own drag came
    /// to, when one was attempted, for the error should this drop be refused.
    async fn drop_by_events(
        &self,
        from: &Aim,
        to: &Aim,
        target: &Locator,
        native: Option<NativeDrag>,
    ) -> Result<()> {
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
        let outcome: Result<DropOutcome> = self
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
        let dropped = outcome
            .as_ref()
            .map_or_else(|_| "none".to_string(), |outcome| outcome.effect.clone());
        // In the source's frame, not on the source: a page that re-renders its
        // source on drop leaves that handle stale, and the drag it ends is kept
        // on the frame's window.
        let ended: Result<()> = self
            .page
            .call_json(
                &from.element.frame,
                "(lib, x, y, effect) => lib.endDrag(x, y, effect)",
                vec![
                    JsArg::Int(from.local.0),
                    JsArg::Int(from.local.1),
                    JsArg::Str(dropped),
                ],
            )
            .await;
        let outcome = outcome?;
        ended.with_context(|| format!("end the drag of {}", self.selector))?;
        if outcome.effect == "none" {
            let refused_by = outcome
                .refused_by
                .unwrap_or_else(|| "the element there".to_string());
            let native = native.map_or("", NativeDrag::describe);
            bail!(
                "{} did not accept the drop: {refused_by} at ({}, {}) refused it; {native}",
                target.selector,
                to.local.0,
                to.local.1
            );
        }
        Ok(())
    }

    pub async fn scroll_into_view(&self) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        let hit: Hit = self
            .page
            .call_on(
                &element,
                "(lib, el) => { lib.scrollIntoViewIfNeeded(el); return lib.hitPoint(el); }",
                Vec::new(),
            )
            .await
            .with_context(|| format!("scroll {} into view", self.selector))?;
        self.page
            .reveal(&element.frame, hit.horizontal, hit.vertical)
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

    /// One locator per match, in document order, for when each match needs its
    /// own read or action. The list is as long as the matches were when asked.
    pub async fn all(&self) -> Result<Vec<Locator>> {
        (0..self.count().await?)
            .map(|index| -> Result<Locator> { Ok(self.nth(i64::try_from(index)?)) })
            .collect()
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
            .inspect_with(
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

    /// Where the element is in its frame's viewport, rounded to whole pixels:
    /// what a pointer aimed with [`Mouse`](crate::Mouse) needs.
    pub async fn bounding_box(&self) -> Result<BoundingBox> {
        self.eval_on(
            "(el) => { const box = el.getBoundingClientRect(); return { \
             left: Math.round(box.left), top: Math.round(box.top), \
             width: Math.round(box.width), height: Math.round(box.height) }; }",
        )
        .await
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
    pub async fn wait_for(&self, state: ElementState) -> Result<()> {
        let timeout = self.timeout(None);
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

fn is_browser_gone(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::channel::BrowserGone>()
        .is_some()
}
