//! [`WebDriverExt`] for a live thirtyfour session.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::time::{Duration, Instant};
use thirtyfour::prelude::*;
use tracing::debug;

use super::runtime::call_lib;
use super::wait::wait_until_interactable;
use super::{WebDriverExt, escape_js_single, testid_selector, typed_in_order};
use crate::js;
use crate::poll::{Backoff, poll_until, poll_until_ok};

async fn script_bool(driver: &WebDriver, script: &str) -> bool {
    match driver.execute(script, Vec::new()).await {
        Ok(returned) => returned.convert::<bool>().unwrap_or_else(|error| {
            debug!("script returned a non-boolean: {error:#}");
            false
        }),
        Err(error) => {
            debug!("script failed: {error:#}");
            false
        }
    }
}

const PRESCROLL: &str = "(lib, selector) => {
    const el = document.querySelector(selector);
    if (el) lib.prescroll(el);
    return true;
}";

/// Scroll the target into view clear of sticky overlays. Best effort: the settle
/// and the click that follow report a target that is still out of reach.
async fn prescroll(driver: &WebDriver, selector: &str) {
    let scrolled = match serde_json::to_value(selector) {
        Ok(selector) => call_lib::<bool>(driver, PRESCROLL, vec![selector]).await,
        Err(error) => Err(error).context("serialize selector"),
    };
    if let Err(error) = scrolled {
        debug!("prescroll failed for {selector}: {error:#}");
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TypingKind {
    Input,
    Textarea,
    Editable,
    Native,
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TypingState {
    kind: TypingKind,
    #[serde(rename = "type")]
    input_type: Option<String>,
    text: String,
    focused: bool,
    on_top: String,
    active: String,
}

const TYPING_STATE: &str = "(lib, selector) => {
    const el = document.querySelector(selector);
    if (!el) throw new Error('element not found: ' + selector);
    return lib.typingState(el);
}";

/// The browser's select-all modifier: Command on macOS, Control elsewhere. Read from
/// the session rather than the host, so a remote browser gets its own platform's key.
fn select_all_modifier(driver: &WebDriver) -> Result<Key> {
    let platform = driver
        .handle()
        .capabilities()
        .get("platformName")
        .and_then(|name| name.as_str())
        .context("the session reported no platformName to pick the select-all modifier")?;
    Ok(if platform.to_ascii_lowercase().starts_with("mac") {
        Key::Meta
    } else {
        Key::Control
    })
}

async fn typing_state(driver: &WebDriver, selector: &str) -> Result<TypingState> {
    let selector_value = serde_json::to_value(selector).context("serialize selector")?;
    call_lib(driver, TYPING_STATE, vec![selector_value])
        .await
        .with_context(|| format!("read the typing state of {selector}"))
}

impl WebDriverExt for WebDriver {
    async fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        let text = self
            .execute(js::classic_body(&js::user_call(expression, "")), Vec::new())
            .await
            .context("JavaScript evaluation failed")?
            .convert::<String>()
            .context("read the evaluation result")?;
        serde_json::from_str(&text).with_context(|| format!("decode evaluation result {text}"))
    }

    async fn selector_exists(&self, selector: &str) -> Result<bool> {
        let elements = self
            .find_all(By::Css(selector))
            .await
            .with_context(|| format!("query {selector}"))?;
        Ok(!elements.is_empty())
    }

    async fn wait_for_selector(&self, selector: &str, timeout: Duration) -> Result<WebElement> {
        let start = Instant::now();
        let escaped = escape_js_single(selector);
        // Existence is checked in JS first, which avoids driver interactability errors mid-hydration.
        let exists_script = format!("return document.querySelector('{escaped}') !== null");
        let present =
            poll_until(|| script_bool(self, &exists_script), timeout, Backoff::FAST).await;
        if !present {
            bail!(
                "timed out waiting for selector: {} (after {:?})",
                selector,
                start.elapsed()
            );
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        poll_until_ok(
            || async {
                Ok(self
                    .find_all(By::Css(selector))
                    .await
                    .with_context(|| format!("query {selector}"))?
                    .into_iter()
                    .next())
            },
            remaining,
            Backoff::FAST,
        )
        .await
        .map_err(|last| {
            let message = format!(
                "element exists in DOM but WebDriver cannot acquire handle for: {selector}"
            );
            match last {
                Some(e) => e.context(message),
                None => anyhow::anyhow!(message),
            }
        })
    }

    async fn wait_for_testid(&self, testid: &str, timeout: Duration) -> Result<WebElement> {
        self.wait_for_selector(&testid_selector(testid), timeout)
            .await
    }

    async fn click_selector(&self, selector: &str, timeout: Duration) -> Result<()> {
        // Stale, not-interactable and intercepted are transient during a re-render,
        // so the settle-find-click cycle is retried until the budget runs out.
        let start = Instant::now();
        loop {
            self.wait_for_selector(selector, timeout).await?;
            // Scroll first, then settle: the scroll itself moves the target.
            prescroll(self, selector).await;
            wait_until_interactable(self, selector, timeout).await?;
            let element = self.find(By::Css(selector)).await.with_context(|| {
                format!("failed to re-find selector after settling: {}", selector)
            })?;
            match element.click().await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let msg = err.to_string();
                    let transient = msg.contains("stale element")
                        || msg.contains("not interactable")
                        || msg.contains("intercepted");
                    if transient && start.elapsed() < timeout {
                        // Slower than the read-only polls: a click reported as
                        // intercepted may still have landed, so let the page settle.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        continue;
                    }
                    return Err(err)
                        .with_context(|| format!("failed to click selector: {}", selector));
                }
            }
        }
    }

    async fn click_testid(&self, testid: &str, timeout: Duration) -> Result<()> {
        self.click_selector(&testid_selector(testid), timeout).await
    }

    async fn js_click_selector(&self, selector: &str) -> Result<()> {
        let escaped = escape_js_single(selector);
        self.execute(
            format!(
                r#"const el = document.querySelector('{escaped}');
if (!el) throw new Error('Element not found for js_click: {escaped}');
el.click();"#
            ),
            Vec::new(),
        )
        .await
        .with_context(|| format!("failed to js_click selector: {}", selector))?;
        Ok(())
    }

    async fn js_click_testid(&self, testid: &str) -> Result<()> {
        self.js_click_selector(&testid_selector(testid)).await
    }

    async fn dispatch_pointer_click(&self, testid: &str, timeout: Duration) -> Result<()> {
        self.wait_for_testid(testid, timeout).await?;
        let selector = escape_js_single(&testid_selector(testid));
        self.execute(
            format!(
                r#"const el = document.querySelector('{selector}');
if (!el) throw new Error('Trigger {selector} not found');
el.dispatchEvent(new PointerEvent('pointerdown', {{ bubbles: true, cancelable: true }}));
el.dispatchEvent(new PointerEvent('pointerup', {{ bubbles: true, cancelable: true }}));
el.dispatchEvent(new MouseEvent('click', {{ bubbles: true, cancelable: true }}));"#
            ),
            Vec::new(),
        )
        .await
        .with_context(|| format!("failed to dispatch pointer click on testid: {}", testid))?;
        Ok(())
    }

    async fn wait_for_text(&self, text: &str, timeout: Duration) -> Result<()> {
        let script = format!(
            "return !!(document.body && document.body.innerText.includes({}))",
            js::js_single_quoted(text)
        );
        let found = poll_until(|| script_bool(self, &script), timeout, Backoff::FAST).await;
        if found {
            return Ok(());
        }
        bail!("timed out waiting for text: \"{}\"", text);
    }

    async fn expect_text_hidden(&self, text: &str, timeout: Duration) -> Result<()> {
        let script = format!(
            "return !(document.body && document.body.innerText.includes({}))",
            js::js_single_quoted(text)
        );
        let hidden = poll_until(|| script_bool(self, &script), timeout, Backoff::FAST).await;
        if hidden {
            return Ok(());
        }
        bail!("text \"{}\" still visible after {:?}", text, timeout)
    }

    async fn expect_hidden(&self, selector: &str, timeout: Duration) -> Result<()> {
        let hidden = poll_until(
            || async {
                match self.selector_exists(selector).await {
                    Ok(exists) => !exists,
                    Err(error) => {
                        debug!("checking whether {selector} is gone failed: {error:#}");
                        false
                    }
                }
            },
            timeout,
            Backoff::FAST,
        )
        .await;
        if hidden {
            return Ok(());
        }
        bail!("element still visible after timeout: {}", selector);
    }

    async fn expect_url_contains(&self, path: &str, timeout: Duration) -> Result<()> {
        let arrived = poll_until(
            || async {
                self.current_url()
                    .await
                    .is_ok_and(|url| url.as_str().contains(path))
            },
            timeout,
            Backoff::FAST,
        )
        .await;
        if arrived {
            return Ok(());
        }
        let url = self.current_url().await.context("read the current URL")?;
        bail!("URL {} does not contain {}", url, path);
    }

    async fn wait_for_interactable(&self, selector: &str, timeout: Duration) -> Result<()> {
        wait_until_interactable(self, selector, timeout).await
    }

    async fn type_into_testid(&self, testid: &str, text: &str, timeout: Duration) -> Result<()> {
        self.type_into_selector(&testid_selector(testid), text, timeout)
            .await
    }

    async fn type_into_selector(
        &self,
        selector: &str,
        text: &str,
        timeout: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let remaining = || timeout.saturating_sub(start.elapsed());
        self.wait_for_selector(selector, timeout).await?;
        prescroll(self, selector).await;
        // Settle before acquiring the handle, or a re-render leaves it stale.
        wait_until_interactable(self, selector, remaining()).await?;
        let before = typing_state(self, selector).await?;
        match before.kind {
            TypingKind::Native => bail!(
                "{selector} is an <input type={}>: text typing does not apply to that input type",
                before.input_type.as_deref().unwrap_or_default()
            ),
            TypingKind::Other => {
                bail!("{selector} is not an input, textarea or contenteditable element")
            }
            TypingKind::Input | TypingKind::Textarea | TypingKind::Editable => {}
        }
        let element = self
            .find(By::Css(selector))
            .await
            .with_context(|| format!("failed to re-find selector after settling: {}", selector))?;
        // A pointer click lands on whatever is on top, as a person's does; a label
        // over the field forwards focus to it, and the focus check catches the rest.
        self.action_chain()
            .move_to_element_center(&element)
            .click()
            .perform()
            .await
            .with_context(|| format!("pointer click on {selector}"))?;
        let focused = typing_state(self, selector).await?;
        if !focused.focused {
            bail!(
                "{selector} did not take focus: the click landed on {}, focus is on {}",
                before.on_top,
                focused.active
            );
        }
        if !focused.text.is_empty() {
            // One selection delete, so a mask sees a single edit and an input handler fires once.
            let modifier = select_all_modifier(self)?;
            self.action_chain()
                .key_down(modifier.clone())
                .key_down('a')
                .key_up('a')
                .key_up(modifier)
                .key_down(Key::Backspace)
                .key_up(Key::Backspace)
                .perform()
                .await
                .with_context(|| format!("clear {selector}"))?;
            let cleared = typing_state(self, selector).await?;
            if !cleared.text.is_empty() {
                bail!("clearing {selector} left {:?}", cleared.text);
            }
        }
        if !text.is_empty() {
            self.action_chain()
                .send_keys(text)
                .perform()
                .await
                .with_context(|| format!("type into {selector}"))?;
        }
        poll_until_ok(
            || async {
                let landed = typing_state(self, selector).await?.text;
                if typed_in_order(text, &landed) {
                    return Ok(Some(()));
                }
                Err(anyhow!("{selector} holds {landed:?} after typing {text:?}"))
            },
            remaining(),
            Backoff::FAST,
        )
        .await
        .map_err(|last| {
            last.unwrap_or_else(|| anyhow!("{selector}: the typed value was never read back"))
        })
    }
}
