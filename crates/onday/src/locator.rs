//! Lazy element locators with auto-waiting actions.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::keys::parse_chord;
use crate::page::{ElementRef, JsArg, MouseButton, Page, abbreviate, quote};
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

#[derive(Debug, Deserialize)]
struct Hit {
    ok: bool,
    reason: String,
    x: i64,
    y: i64,
    outside: bool,
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

    pub fn get_by_role(&self, role: &str, name: Option<&str>) -> Locator {
        match name {
            Some(name) => self.locator(&format!("role={role}[name={}]", quote(name))),
            None => self.locator(&format!("role={role}")),
        }
    }

    // ── resolution ─────────────────────────────────────────────────────────

    async fn resolve_all(&self) -> Result<Vec<ElementRef>> {
        let attribute = self.page.hooks().test_id_attribute().to_string();
        self.page
            .call_elements(
                "(lib, selector, attr) => lib.resolve(selector, attr)",
                vec![JsArg::Str(self.selector.clone()), JsArg::Str(attribute)],
            )
            .await
            .with_context(|| format!("resolve {}", self.selector))
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
            .call_json(
                "(lib, el) => lib.describe(el)",
                vec![JsArg::El(element.clone())],
            )
            .await
    }

    /// Wait for exactly one match.
    async fn wait_one(&self, timeout: Duration) -> Result<ElementRef> {
        let start = Instant::now();
        let mut gap = Backoff::FAST.start;
        loop {
            match self.resolve_strict().await {
                Ok(Some(element)) => return Ok(element),
                Ok(None) => {}
                Err(error) if is_strictness(&error) => return Err(error),
                Err(error) => {
                    tracing::debug!("resolving {} failed, retrying: {error:#}", self.selector)
                }
            }
            if start.elapsed() > timeout {
                bail!(
                    "no element matches {} after {:?}",
                    self.selector,
                    start.elapsed()
                );
            }
            tokio::time::sleep(gap).await;
            gap = Backoff::FAST.next_gap(gap);
        }
    }

    fn timeout(&self, explicit: Option<Duration>) -> Duration {
        explicit.unwrap_or_else(|| self.page.default_timeout())
    }

    /// Resolve, scroll, and wait until the element is settled and would receive a
    /// pointer event at its centre. Returns the element and that point.
    async fn actionable(
        &self,
        action: &str,
        checks: Checks,
        timeout: Duration,
    ) -> Result<(ElementRef, i64, i64)> {
        let start = Instant::now();
        let hover = self
            .page
            .hooks()
            .hover_reveal_ancestor()
            .map(str::to_string);
        let mut reason = String::from("no element matches");
        let mut last_rect: Option<[f64; 4]> = None;
        let mut gap = Backoff::FAST.start;
        loop {
            if start.elapsed() > timeout {
                bail!(
                    "{action} {}: {reason} (after {:?})",
                    self.selector,
                    start.elapsed()
                );
            }
            match self.attempt(checks, hover.as_deref(), &mut last_rect).await {
                Ok(Ok(found)) => return Ok(found),
                Ok(Err(why)) => reason = why,
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
    ) -> Result<std::result::Result<(ElementRef, i64, i64), String>> {
        let Some(element) = self.resolve_strict().await? else {
            *last_rect = None;
            return Ok(Err("no element matches".to_string()));
        };
        let el = JsArg::El(element.clone());
        if !checks.force {
            let scrolled: Result<bool> = self
                .page
                .call_json(
                    "(lib, el) => { lib.prescroll(el); return true; }",
                    vec![el.clone()],
                )
                .await;
            if let Err(error) = scrolled {
                tracing::debug!("prescroll of {} failed: {error:#}", self.selector);
            }
            let probe: Probe = self
                .page
                .call_json(
                    "(lib, el, hover, enabled) => lib.probe(el, hover, enabled)",
                    vec![
                        el.clone(),
                        hover.map_or(JsArg::Null, |selector| JsArg::Str(selector.to_string())),
                        JsArg::Bool(checks.enabled),
                    ],
                )
                .await?;
            if !probe.ok {
                *last_rect = None;
                return Ok(Err(probe
                    .reason
                    .unwrap_or_else(|| "not actionable".to_string())));
            }
            // The element must hold still across two probes: a click aimed at a
            // moving target lands on whatever slides into its place.
            if *last_rect != probe.rect {
                let why = format!("still moving ({:?} -> {:?})", last_rect, probe.rect);
                *last_rect = probe.rect;
                return Ok(Err(why));
            }
        }
        let hit: Hit = self
            .page
            .call_json("(lib, el) => lib.hitPoint(el)", vec![el.clone()])
            .await?;
        if hit.outside {
            let scrolled: Result<bool> = self
                .page
                .call_json(
                    "(lib, el) => { lib.scrollIntoViewIfNeeded(el); return true; }",
                    vec![el],
                )
                .await;
            if let Err(error) = scrolled {
                tracing::debug!("scrolling {} into view failed: {error:#}", self.selector);
            }
            *last_rect = None;
            return Ok(Err(hit.reason));
        }
        if checks.hit && !hit.ok && !checks.force {
            *last_rect = None;
            return Ok(Err(hit.reason));
        }
        Ok(Ok((element, hit.x, hit.y)))
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
            let (_, x, y) = self.actionable("click", checks, remaining).await?;
            match self
                .page
                .click_at(x, y, options.button, options.click_count.max(1), &modifiers)
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
        let (_, x, y) = self.actionable("hover", checks, self.timeout(None)).await?;
        self.page.mouse().move_to(x, y).await
    }

    /// Replace the element's value with `text`, typing it as key events.
    pub async fn fill(&self, text: &str) -> Result<()> {
        let checks = Checks {
            enabled: true,
            hit: false,
            force: false,
        };
        let (element, _, _) = self.actionable("fill", checks, self.timeout(None)).await?;
        let el = JsArg::El(element);
        let direct: bool = self
            .page
            .call_json(
                "(lib, el, value) => lib.fillDirect(el, value)",
                vec![el.clone(), JsArg::Str(text.to_string())],
            )
            .await
            .with_context(|| format!("fill {}", self.selector))?;
        if direct {
            return Ok(());
        }
        let kind: String = self
            .page
            .call_json("(lib, el) => lib.clearForTyping(el)", vec![el])
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
            .keyboard()
            .type_text(text)
            .await
            .with_context(|| format!("type into {}", self.selector))
    }

    pub async fn clear(&self) -> Result<()> {
        self.fill("").await
    }

    /// Focus the element and type `text` without clearing it first.
    pub async fn press_sequentially(&self, text: &str) -> Result<()> {
        self.focus().await?;
        self.page.keyboard().type_text(text).await
    }

    /// Focus the element and press a key or chord.
    pub async fn press(&self, combo: &str) -> Result<()> {
        self.focus().await?;
        self.page.keyboard().press(combo).await
    }

    pub async fn focus(&self) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        let focused: bool = self
            .page
            .call_json(
                "(lib, el) => { el.focus(); return true; }",
                vec![JsArg::El(element)],
            )
            .await
            .with_context(|| format!("focus {}", self.selector))?;
        if !focused {
            bail!("{} could not be focused", self.selector);
        }
        Ok(())
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
        let (element, _, _) = self
            .actionable("select", checks, self.timeout(None))
            .await?;
        self.page
            .call_json(
                "(lib, el, values) => lib.selectOptions(el, values)",
                vec![
                    JsArg::El(element),
                    JsArg::Strs(values.iter().map(|value| value.to_string()).collect()),
                ],
            )
            .await
            .with_context(|| format!("select {values:?} in {}", self.selector))
    }

    /// Set the files of an `<input type=file>`; it may be hidden.
    pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page.set_files(&element, paths.to_vec()).await
    }

    /// Drag this element onto `target` with real pointer moves.
    pub async fn drag_to(&self, target: &Locator) -> Result<()> {
        let source_checks = Checks {
            enabled: false,
            hit: true,
            force: false,
        };
        let (_, from_x, from_y) = self
            .actionable("drag", source_checks, self.timeout(None))
            .await?;
        let target_checks = Checks {
            enabled: false,
            hit: false,
            force: false,
        };
        let (_, to_x, to_y) = target
            .actionable("drop onto", target_checks, self.timeout(None))
            .await?;
        let mut actions = vec![
            PointerAction::PointerMove {
                x: from_x,
                y: from_y,
                duration: None,
                origin: "viewport",
            },
            PointerAction::PointerDown { button: 0 },
            PointerAction::Pause { duration: 50 },
        ];
        let steps = 10;
        for step in 1..=steps {
            actions.push(PointerAction::PointerMove {
                x: from_x + (to_x - from_x) * step / steps,
                y: from_y + (to_y - from_y) * step / steps,
                duration: Some(16),
                origin: "viewport",
            });
        }
        actions.push(PointerAction::PointerUp { button: 0 });
        self.page
            .pointer(actions)
            .await
            .with_context(|| format!("drag {} to {}", self.selector, target.selector))
    }

    pub async fn scroll_into_view(&self) -> Result<()> {
        let element = self.wait_one(self.timeout(None)).await?;
        let scrolled: bool = self
            .page
            .call_json(
                "(lib, el) => { lib.prescroll(el); if (lib.hitPoint(el).outside) lib.scrollIntoViewIfNeeded(el); return true; }",
                vec![JsArg::El(element)],
            )
            .await?;
        if !scrolled {
            bail!("could not scroll {} into view", self.selector);
        }
        Ok(())
    }

    /// PNG of the element.
    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        self.scroll_into_view().await?;
        let element = self.wait_one(self.timeout(None)).await?;
        self.page.element_screenshot(&element).await
    }

    /// Press `key` while this element has focus, as a single key event pair.
    pub async fn dispatch_key(&self, key: &str) -> Result<()> {
        self.focus().await?;
        let chord = parse_chord(key)?;
        self.page
            .keys(vec![
                KeyAction::KeyDown {
                    value: chord.key.clone(),
                },
                KeyAction::KeyUp { value: chord.key },
            ])
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
            .call_json("(lib, el) => lib.info(el)", vec![JsArg::El(element)])
            .await
            .with_context(|| format!("inspect {}", self.selector))
    }

    pub async fn is_visible(&self) -> Result<bool> {
        match self.resolve_strict().await? {
            None => Ok(false),
            Some(element) => {
                self.page
                    .call_json("(lib, el) => lib.isVisible(el)", vec![JsArg::El(element)])
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
                "(el, name) => el.getAttribute(name)",
                vec![JsArg::El(element), JsArg::Str(name.to_string())],
            )
            .await
    }

    /// Text of every match.
    pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
        let mut texts = Vec::new();
        for element in self.resolve_all().await? {
            let info: ElementInfo = self
                .page
                .call_json("(lib, el) => lib.info(el)", vec![JsArg::El(element)])
                .await?;
            texts.push(info.text);
        }
        Ok(texts)
    }

    /// Call `function(element)` in the page and decode its result.
    pub async fn eval_on<T: DeserializeOwned>(&self, function: &str) -> Result<T> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .evaluate_with(function, vec![JsArg::El(element)])
            .await
            .with_context(|| format!("evaluate {} on {}", abbreviate(function), self.selector))
    }

    /// Aria snapshot rooted at this element.
    pub async fn snapshot(&self) -> Result<String> {
        let element = self.wait_one(self.timeout(None)).await?;
        self.page
            .call_json(
                "(lib, el) => lib.snapshot(el, 5000)",
                vec![JsArg::El(element)],
            )
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
