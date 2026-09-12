//! Conventions of the application under test that the generic layer defers to.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

/// Application conventions the framework consults instead of hard-coding them.
///
/// Implement it for the app under test and pass it through
/// [`ContextOptions::hooks`](crate::ContextOptions) or [`install_global`].
pub trait AppHooks: Send + Sync + 'static {
    /// Attribute read by `testid=` selectors and the `*_testid` helpers.
    fn test_id_attribute(&self) -> &str {
        "data-testid"
    }

    /// Script bodies run at document start in every page of a context.
    fn init_scripts(&self) -> Vec<Cow<'static, str>> {
        Vec::new()
    }

    /// Function body returning `true` once the app has hydrated; drives `WaitUntil::Ready`.
    fn ready_script(&self) -> Option<Cow<'static, str>> {
        None
    }

    /// Selector for ancestors whose descendants only become opaque under a real
    /// hover; the opacity gate is skipped beneath them.
    fn hover_reveal_ancestor(&self) -> Option<&str> {
        None
    }

    /// JS `(method, url) => boolean` marking fetches counted as in-flight writes.
    fn is_tracked_write_js(&self) -> Option<&str> {
        None
    }
}

/// The conventions used when the app installs none.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultHooks;

impl AppHooks for DefaultHooks {}

static GLOBAL: OnceLock<Arc<dyn AppHooks>> = OnceLock::new();

/// Install the process-wide hooks used by raw-driver helpers
/// ([`WebDriverExt`](crate::dom::WebDriverExt)). The first install wins.
pub fn install_global(hooks: Arc<dyn AppHooks>) -> bool {
    GLOBAL.set(hooks).is_ok()
}

/// The process-wide hooks, or [`DefaultHooks`] when none were installed.
pub fn global() -> Arc<dyn AppHooks> {
    GLOBAL
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(DefaultHooks))
}
