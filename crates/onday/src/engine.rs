//! Browser engines and the WebDriver capabilities that launch them.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thirtyfour::{Capabilities, CapabilitiesHelper, ChromiumLikeCapabilities, DesiredCapabilities};

use crate::launch::{LaunchOptions, ProtocolPreference};

/// A browser engine. Each maps to the stock browser and its WebDriver server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Chrome, Chromium or Chrome for Testing, driven by chromedriver.
    #[default]
    Chromium,
    /// Firefox, driven by geckodriver.
    Firefox,
    /// Safari (safaridriver) on macOS; WebKitGTK (WebKitWebDriver) elsewhere.
    Webkit,
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::Chromium, Engine::Firefox, Engine::Webkit];

    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Chromium => "chromium",
            Engine::Firefox => "firefox",
            Engine::Webkit => "webkit",
        }
    }

    /// Capabilities for a new session of this engine.
    pub fn capabilities(self, options: &LaunchOptions) -> Result<Capabilities> {
        let mut caps = match self {
            Engine::Chromium => chromium_capabilities(options)?,
            Engine::Firefox => firefox_capabilities(options)?,
            Engine::Webkit => webkit_capabilities(options)?,
        };
        caps.set_page_load_strategy(options.page_load_strategy.clone())
            .context("set page load strategy")?;
        // The W3C key; thirtyfour's helper only writes the legacy one.
        caps.set("unhandledPromptBehavior", options.prompt_behavior.as_str())
            .context("set prompt behaviour")?;
        if options.accept_insecure_certs {
            caps.accept_insecure_certs(true)
                .context("accept insecure certs")?;
        }
        if options.protocol == ProtocolPreference::Auto {
            caps.enable_bidi().context("request webSocketUrl")?;
        }
        Ok(caps)
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Engine {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "chromium" | "chrome" | "chrome-for-testing" | "edge" => Ok(Engine::Chromium),
            "firefox" | "gecko" => Ok(Engine::Firefox),
            "webkit" | "safari" => Ok(Engine::Webkit),
            other => bail!("unknown engine {other:?}; expected chromium, firefox or webkit"),
        }
    }
}

fn chromium_capabilities(options: &LaunchOptions) -> Result<Capabilities> {
    let mut caps = DesiredCapabilities::chrome();
    if options.headless {
        caps.add_arg("--headless=new").context("headless arg")?;
    }
    if options.no_sandbox {
        caps.add_arg("--no-sandbox").context("no-sandbox arg")?;
    }
    // Small /dev/shm on containers and servers crashes the renderer; spill to /tmp.
    caps.add_arg("--disable-dev-shm-usage")
        .context("dev-shm arg")?;
    if let Some((width, height)) = options.window_size {
        caps.add_arg(&format!("--window-size={width},{height}"))
            .context("window-size arg")?;
    }
    if let Some(dir) = &options.user_data_dir {
        caps.add_arg(&format!("--user-data-dir={}", dir.display()))
            .context("user-data-dir arg")?;
    }
    if let Some(binary) = &options.executable {
        caps.set_binary(&binary.to_string_lossy())
            .context("chrome binary")?;
    }
    for arg in &options.args {
        caps.add_arg(arg)
            .with_context(|| format!("chrome arg {arg}"))?;
    }
    Ok(caps.into())
}

fn firefox_capabilities(options: &LaunchOptions) -> Result<Capabilities> {
    let mut caps = DesiredCapabilities::firefox();
    if options.headless {
        caps.set_headless().context("headless arg")?;
    }
    if let Some((width, height)) = options.window_size {
        caps.add_arg(&format!("--width={width}"))
            .context("width arg")?;
        caps.add_arg(&format!("--height={height}"))
            .context("height arg")?;
    }
    if let Some(dir) = &options.user_data_dir {
        caps.add_arg("-profile").context("profile arg")?;
        caps.add_arg(&dir.to_string_lossy())
            .context("profile path arg")?;
    }
    if let Some(binary) = &options.executable {
        caps.set_firefox_binary(&binary.to_string_lossy())
            .context("firefox binary")?;
    }
    for arg in &options.args {
        caps.add_arg(arg)
            .with_context(|| format!("firefox arg {arg}"))?;
    }
    Ok(caps.into())
}

#[derive(Serialize)]
struct WebkitGtkOptions {
    binary: String,
    args: Vec<String>,
}

fn webkit_capabilities(options: &LaunchOptions) -> Result<Capabilities> {
    if cfg!(target_os = "macos") {
        return Ok(DesiredCapabilities::safari().into());
    }
    let binary = options
        .executable
        .clone()
        .unwrap_or_else(|| "MiniBrowser".into());
    let browser_name = binary
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "MiniBrowser".to_string());
    let mut args = vec!["--automation".to_string()];
    if options.headless {
        args.push("--headless".to_string());
    }
    args.extend(options.args.iter().cloned());
    let mut caps = Capabilities::new();
    caps.set("browserName", browser_name)
        .context("webkit browserName")?;
    caps.set(
        "webkitgtk:browserOptions",
        WebkitGtkOptions {
            binary: binary.to_string_lossy().into_owned(),
            args,
        },
    )
    .context("webkitgtk options")?;
    Ok(caps)
}
