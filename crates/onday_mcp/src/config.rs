//! Command-line and environment configuration.

use std::path::PathBuf;

use clap::Parser;
use clap::builder::FalseyValueParser;
use onday::Engine;

/// A browser MCP server. Each process is one session with its own directory,
/// driver and browser, so many can run side by side in one working directory.
#[derive(Debug, Clone, Parser)]
#[command(name = "onday_mcp", version)]
pub struct Config {
    /// Session id; the session's state lives in `<output-dir>/<session>/`.
    /// Generated when unset.
    #[arg(long, env = "ONDAY_SESSION")]
    pub session: Option<String>,

    /// Directory holding every session's state.
    #[arg(long, env = "ONDAY_OUTPUT_DIR", default_value = ".onday")]
    pub output_dir: PathBuf,

    /// Engine used until `browser_launch` picks another: chromium, firefox or webkit.
    #[arg(long, env = "ONDAY_ENGINE", default_value = "chromium")]
    pub engine: Engine,

    /// Show the browser window.
    #[arg(long, env = "ONDAY_HEADED", value_parser = FalseyValueParser::new())]
    pub headed: bool,

    /// Viewport as WIDTHxHEIGHT.
    #[arg(long, env = "ONDAY_VIEWPORT", default_value = "1280x800", value_parser = parse_viewport)]
    pub viewport: (u32, u32),

    /// Use a throwaway profile instead of `<session>/profile-<engine>/`.
    #[arg(long, env = "ONDAY_ISOLATED", value_parser = FalseyValueParser::new())]
    pub isolated: bool,

    /// Speak WebDriver Classic only, never BiDi.
    #[arg(long, env = "ONDAY_CLASSIC", value_parser = FalseyValueParser::new())]
    pub classic: bool,

    /// Pass --no-sandbox to Chromium (needed where unprivileged user namespaces are denied).
    #[arg(long, env = "ONDAY_NO_SANDBOX", value_parser = FalseyValueParser::new())]
    pub no_sandbox: bool,

    /// Seconds an action waits for its element to become actionable.
    #[arg(long, env = "ONDAY_ACTION_TIMEOUT", default_value_t = 15)]
    pub action_timeout: u64,

    #[arg(long, env = "ONDAY_CHROMIUM_EXECUTABLE")]
    pub chromium_executable: Option<PathBuf>,

    #[arg(long, env = "ONDAY_FIREFOX_EXECUTABLE")]
    pub firefox_executable: Option<PathBuf>,

    #[arg(long, env = "ONDAY_WEBKIT_EXECUTABLE")]
    pub webkit_executable: Option<PathBuf>,

    /// chromedriver binary; resolved or downloaded when unset.
    #[arg(long, env = "ONDAY_CHROMIUM_DRIVER")]
    pub chromium_driver: Option<PathBuf>,

    /// geckodriver binary; resolved or downloaded when unset.
    #[arg(long, env = "ONDAY_FIREFOX_DRIVER")]
    pub firefox_driver: Option<PathBuf>,

    /// safaridriver or WebKitWebDriver binary.
    #[arg(long, env = "ONDAY_WEBKIT_DRIVER")]
    pub webkit_driver: Option<PathBuf>,

    /// Connect to this WebDriver server instead of starting a driver.
    #[arg(long, env = "ONDAY_WEBDRIVER_URL")]
    pub webdriver_url: Option<String>,
}

impl Config {
    pub fn executable(&self, engine: Engine) -> Option<PathBuf> {
        match engine {
            Engine::Chromium => self.chromium_executable.clone(),
            Engine::Firefox => self.firefox_executable.clone(),
            Engine::Webkit => self.webkit_executable.clone(),
        }
    }

    pub fn driver(&self, engine: Engine) -> Option<PathBuf> {
        match engine {
            Engine::Chromium => self.chromium_driver.clone(),
            Engine::Firefox => self.firefox_driver.clone(),
            Engine::Webkit => self.webkit_driver.clone(),
        }
    }
}

fn parse_viewport(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got {value:?}"))?;
    let width = width
        .trim()
        .parse()
        .map_err(|error| format!("width {width:?}: {error}"))?;
    let height = height
        .trim()
        .parse()
        .map_err(|error| format!("height {height:?}: {error}"))?;
    Ok((width, height))
}
