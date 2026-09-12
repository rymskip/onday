//! Browser automation over WebDriver BiDi, falling back to WebDriver Classic.
//!
//! ```no_run
//! # async fn run() -> anyhow::Result<()> {
//! use onday::prelude::*;
//!
//! let browser = Browser::launch(LaunchOptions::new(Engine::Firefox)).await?;
//! let page = browser.default_context(ContextOptions::default()).await?.page().await?;
//! page.goto("https://example.com", WaitUntil::Load).await?;
//! page.get_by_role("link", Some("More information")).click().await?;
//! expect_page(&page).to_have_url("**/iana.org/**").await?;
//! browser.close().await?;
//! # Ok(()) }
//! ```

pub mod browser;
pub mod context;
pub mod dom;
pub mod engine;
pub mod events;
pub mod expect;
pub mod hooks;
pub mod js;
pub mod keys;
pub mod launch;
pub mod locator;
pub mod page;
pub mod poll;
pub mod route;
pub mod warm;
pub mod webauthn;

mod proto;

pub use browser::Browser;
pub use context::{BrowserContext, ContextOptions};
pub use dom::{WebDriverExt, escape_attr, testid_selector};
pub use engine::Engine;
pub use events::{ConsoleMessage, Dialog, DialogPolicy, Header, NetworkEntry};
pub use expect::{LocatorAssertions, PageAssertions, expect, expect_page};
pub use hooks::{AppHooks, DefaultHooks};
pub use launch::{DriverSource, LaunchOptions, PromptBehavior, Protocol, ProtocolPreference};
pub use locator::{ClickOptions, ElementInfo, ElementState, Locator, StrictModeViolation};
pub use page::{Keyboard, Mouse, MouseButton, Page, ScreenshotOptions, WaitUntil};
pub use poll::{Backoff, poll_until, poll_until_blocking, poll_until_ok};
pub use route::{Fulfill, InterceptedRequest, RouteAction, RouteHandler};
pub use thirtyfour;
pub use webauthn::{
    VirtualAuthenticatorOptions, add_virtual_authenticator, remove_virtual_authenticator,
};

/// Common imports: the Playwright-style API, the raw-driver extension trait and
/// the thirtyfour prelude.
pub mod prelude {
    pub use crate::{
        Browser, BrowserContext, ClickOptions, ContextOptions, DialogPolicy, Engine, LaunchOptions,
        Locator, Page, WaitUntil, WebDriverExt, escape_attr, expect, expect_page, testid_selector,
    };
    pub use thirtyfour::prelude::*;
}
