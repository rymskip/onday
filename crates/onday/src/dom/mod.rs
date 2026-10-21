//! Polling, selector and click helpers on a raw `thirtyfour::WebDriver` (Classic).
//!
//! Click modes:
//! - `click_*`: WebDriver element click, retried while the driver reports it
//!   intercepted, stale or not interactable; the default.
//! - `js_click_*`: `HTMLElement.click()`, for targets whose ancestors swallow pointer events.
//! - `dispatch_pointer_click`: synthetic `pointerdown`/`pointerup`/`click`, for
//!   triggers that open on `pointerdown`.

mod impls;
mod runtime;
mod wait;

use anyhow::Result;
use serde::de::DeserializeOwned;
use std::time::Duration;
use thirtyfour::prelude::WebElement;

pub use wait::wait_until_interactable;

/// Escape a string for a double-quoted HTML attribute value inside a CSS selector.
pub fn escape_attr(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// CSS selector for the test-id attribute named by the global [`AppHooks`](crate::AppHooks).
pub fn testid_selector(testid: &str) -> String {
    format!(
        r#"[{}="{}"]"#,
        crate::hooks::global().test_id_attribute(),
        escape_attr(testid)
    )
}

/// DOM helpers on `WebDriver`. Import through `onday::prelude`.
pub trait WebDriverExt {
    /// Evaluate an expression, or call a function, and decode its JSON-able result.
    fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> impl Future<Output = Result<T>>;

    /// Whether at least one element matches `selector` right now.
    fn selector_exists(&self, selector: &str) -> impl Future<Output = Result<bool>>;

    /// Poll until an element matches `selector` and its handle can be acquired.
    fn wait_for_selector(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<WebElement>>;

    fn wait_for_testid(
        &self,
        testid: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<WebElement>>;

    /// WebDriver element click once the element has settled, retried while transient.
    fn click_selector(&self, selector: &str, timeout: Duration)
    -> impl Future<Output = Result<()>>;

    fn click_testid(&self, testid: &str, timeout: Duration) -> impl Future<Output = Result<()>>;

    /// `HTMLElement.click()` in the page, bypassing pointer-event pipelines.
    fn js_click_selector(&self, selector: &str) -> impl Future<Output = Result<()>>;

    fn js_click_testid(&self, testid: &str) -> impl Future<Output = Result<()>>;

    /// Dispatch `pointerdown`/`pointerup`/`click` synchronously on the element.
    fn dispatch_pointer_click(
        &self,
        testid: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<()>>;

    /// Poll until `document.body.innerText` contains `text`.
    fn wait_for_text(&self, text: &str, timeout: Duration) -> impl Future<Output = Result<()>>;

    /// Poll until `document.body.innerText` no longer contains `text`.
    fn expect_text_hidden(&self, text: &str, timeout: Duration)
    -> impl Future<Output = Result<()>>;

    /// Poll until no element matches `selector`.
    fn expect_hidden(&self, selector: &str, timeout: Duration) -> impl Future<Output = Result<()>>;

    /// Poll until the current URL contains `path`.
    fn expect_url_contains(
        &self,
        path: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<()>>;

    /// Poll until the element has size, is visible and opaque, and holds still.
    /// Viewport visibility is not required; the click scrolls on its own.
    fn wait_for_interactable(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<()>>;

    /// Type `text` into an input, textarea or contenteditable the way a person does:
    /// pointer click at its center, confirm focus landed there, clear with the
    /// browser's select-all and one Backspace, type, then require every typed
    /// character in the field in order, so masks pass.
    /// Native-picker and valueless input types are refused.
    fn type_into_testid(
        &self,
        testid: &str,
        text: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<()>>;

    fn type_into_selector(
        &self,
        selector: &str,
        text: &str,
        timeout: Duration,
    ) -> impl Future<Output = Result<()>>;
}

/// Escape a string for a single-quoted JS string literal.
fn escape_js_single(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
