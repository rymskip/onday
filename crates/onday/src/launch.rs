//! How a browser is started: engine, flags, and where its WebDriver server comes from.

use std::path::PathBuf;
use thirtyfour::PageLoadStrategy;

use crate::engine::Engine;

/// Which protocol a session should speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProtocolPreference {
    /// WebDriver BiDi when the driver offers it, WebDriver Classic otherwise.
    #[default]
    Auto,
    /// WebDriver Classic only.
    Classic,
}

/// The protocol a session ended up speaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Bidi,
    Classic,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Protocol::Bidi => "bidi",
            Protocol::Classic => "classic",
        })
    }
}

/// What the driver does with a dialog no command expected (W3C `unhandledPromptBehavior`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptBehavior {
    /// Leave it open so it can be observed and answered; commands fail until then.
    #[default]
    Ignore,
    Accept,
    Dismiss,
    AcceptAndNotify,
    DismissAndNotify,
}

impl PromptBehavior {
    pub fn as_str(self) -> &'static str {
        match self {
            PromptBehavior::Ignore => "ignore",
            PromptBehavior::Accept => "accept",
            PromptBehavior::Dismiss => "dismiss",
            PromptBehavior::AcceptAndNotify => "accept and notify",
            PromptBehavior::DismissAndNotify => "dismiss and notify",
        }
    }
}

/// Where the WebDriver server comes from.
#[derive(Debug, Clone, Default)]
pub enum DriverSource {
    /// Resolve (downloading if needed) and spawn the engine's driver.
    #[default]
    Managed,
    /// Spawn this driver binary.
    Binary(PathBuf),
    /// Connect to a driver or grid already listening at this URL.
    Remote(String),
}

/// Everything needed to start a browser.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub engine: Engine,
    pub headless: bool,
    /// Browser binary; the driver's discovery is used when unset.
    pub executable: Option<PathBuf>,
    /// Extra browser command-line arguments.
    pub args: Vec<String>,
    /// Persistent profile directory; a throwaway profile when unset.
    pub user_data_dir: Option<PathBuf>,
    pub window_size: Option<(u32, u32)>,
    pub no_sandbox: bool,
    pub accept_insecure_certs: bool,
    pub page_load_strategy: PageLoadStrategy,
    pub prompt_behavior: PromptBehavior,
    pub driver: DriverSource,
    pub protocol: ProtocolPreference,
    /// File that receives the driver's stdout and stderr.
    pub driver_log: Option<PathBuf>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        LaunchOptions {
            engine: Engine::default(),
            headless: true,
            executable: None,
            args: Vec::new(),
            user_data_dir: None,
            window_size: None,
            no_sandbox: false,
            accept_insecure_certs: false,
            page_load_strategy: PageLoadStrategy::Normal,
            prompt_behavior: PromptBehavior::Ignore,
            driver: DriverSource::Managed,
            protocol: ProtocolPreference::Auto,
            driver_log: None,
        }
    }
}

impl LaunchOptions {
    pub fn new(engine: Engine) -> Self {
        LaunchOptions {
            engine,
            ..LaunchOptions::default()
        }
    }

    pub fn headed(mut self) -> Self {
        self.headless = false;
        self
    }

    pub fn headless(mut self, headless: bool) -> Self {
        self.headless = headless;
        self
    }

    pub fn executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.executable = Some(path.into());
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn user_data_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.user_data_dir = Some(path.into());
        self
    }

    pub fn window_size(mut self, width: u32, height: u32) -> Self {
        self.window_size = Some((width, height));
        self
    }

    pub fn no_sandbox(mut self) -> Self {
        self.no_sandbox = true;
        self
    }

    pub fn accept_insecure_certs(mut self) -> Self {
        self.accept_insecure_certs = true;
        self
    }

    pub fn page_load_strategy(mut self, strategy: PageLoadStrategy) -> Self {
        self.page_load_strategy = strategy;
        self
    }

    pub fn prompt_behavior(mut self, behavior: PromptBehavior) -> Self {
        self.prompt_behavior = behavior;
        self
    }

    pub fn driver(mut self, source: DriverSource) -> Self {
        self.driver = source;
        self
    }

    pub fn protocol(mut self, preference: ProtocolPreference) -> Self {
        self.protocol = preference;
        self
    }

    pub fn driver_log(mut self, path: impl Into<PathBuf>) -> Self {
        self.driver_log = Some(path.into());
        self
    }
}
