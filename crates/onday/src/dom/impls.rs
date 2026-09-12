//! [`WebDriverExt`] for a live thirtyfour session.

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use std::time::{Duration, Instant};
use thirtyfour::prelude::*;
use tracing::debug;

use super::wait::wait_until_interactable;
use super::*;
use crate::js;
use crate::poll::{Backoff, poll_until, poll_until_ok};

async fn script_bool(driver: &WebDriver, script: &str) -> bool {
    match driver.execute(script, Vec::new()).await {
        Ok(returned) => returned.convert::<bool>().unwrap_or(false),
        Err(_) => false,
    }
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
        let present = poll_until(
            || script_bool(self, &exists_script),
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
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
        let escaped_selector = escape_js_single(selector);
        let prescroll_script = format!(
            r#"const el = document.querySelector('{escaped_selector}');
if (!el) return;
{SCROLL_TO_TARGET_VIA_SCROLLBAR}"#
        );
        loop {
            self.wait_for_selector(selector, timeout).await?;
            // Scroll first, then settle: the scroll itself moves the target.
            if let Err(e) = self.execute(&prescroll_script, Vec::new()).await {
                debug!("prescroll before click failed for {selector}: {e:#}");
            }
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
        let found = poll_until(
            || script_bool(self, &script),
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
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
        let hidden = poll_until(
            || script_bool(self, &script),
            timeout,
            Backoff::FAST,
            |_| {},
        )
        .await;
        if hidden {
            return Ok(());
        }
        bail!("text \"{}\" still visible after {:?}", text, timeout)
    }

    async fn expect_hidden(&self, selector: &str, timeout: Duration) -> Result<()> {
        let hidden = poll_until(
            || async { !self.selector_exists(selector).await.unwrap_or(false) },
            timeout,
            Backoff::FAST,
            |_| {},
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
            |_| {},
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
        self.wait_for_selector(selector, timeout).await?;
        // Settle before acquiring the handle, or a re-render leaves it stale.
        wait_until_interactable(self, selector, timeout).await?;
        let element = self
            .find(By::Css(selector))
            .await
            .with_context(|| format!("failed to re-find selector after settling: {}", selector))?;
        // Clicking focuses inputs and focusable non-inputs alike.
        element
            .click()
            .await
            .with_context(|| format!("failed to focus selector: {}", selector))?;
        // Clear through the native value setter so framework bindings see the
        // change; non-inputs have no value slot and skip it.
        let escaped = escape_js_single(selector);
        let is_form_input = self
            .execute(
                format!(
                    r#"const el = document.querySelector('{escaped}');
if (!el) throw new Error('type_into_selector: element not found: {escaped}');
const isFormInput = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA';
if (isFormInput) {{
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, 'value').set;
    setter.call(el, '');
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
}}
return isFormInput;"#,
                ),
                Vec::new(),
            )
            .await
            .with_context(|| format!("clear {selector}"))?
            .convert::<bool>()
            .with_context(|| format!("read whether {selector} is a form input"))?;
        if !text.is_empty() {
            if is_form_input {
                element
                    .send_keys(text)
                    .await
                    .with_context(|| format!("failed to send keys to {}", selector))?;
            } else {
                // `send_keys` rejects non-inputs; send through the focused element instead.
                self.action_chain()
                    .send_keys(text)
                    .perform()
                    .await
                    .with_context(|| format!("failed to action-send keys to {}", selector))?;
            }
        }
        Ok(())
    }
}
