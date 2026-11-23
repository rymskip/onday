//! The WebDriver servers onday downloads, as `playwright install` does for browsers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use thirtyfour::manager::{Status, WebDriverManager, WebDriverManagerBuilder};

use crate::engine::Engine;
use crate::launch::LaunchOptions;

/// Where downloaded drivers live: `$ONDAY_DRIVERS_DIR`, else `<user cache>/onday/drivers`.
pub fn drivers_dir() -> PathBuf {
    std::env::var_os("ONDAY_DRIVERS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("onday")
                .join("drivers")
        })
}

/// The driver binaries already downloaded for `engine`, one per version and platform.
pub fn cached_drivers(engine: Engine) -> Vec<PathBuf> {
    let Some(binary) = driver_binary(engine) else {
        return Vec::new();
    };
    let Ok(versions) = std::fs::read_dir(drivers_dir().join(binary)) else {
        return Vec::new();
    };
    versions
        .flatten()
        .filter_map(|version| std::fs::read_dir(version.path()).ok())
        .flat_map(|platforms| platforms.flatten())
        .map(|platform| platform.path().join(binary))
        .filter(|candidate| candidate.is_file())
        .collect()
}

/// The driver matching the local browser (`executable`, or the one the driver
/// discovers), downloaded first when the cache lacks it. It opens and closes
/// one headless session, which is how the version is resolved.
pub async fn install_driver(engine: Engine, executable: Option<&Path>) -> Result<PathBuf> {
    if driver_binary(engine).is_none() {
        bail!("{engine} has no driver onday downloads; install its WebDriver server");
    }
    let mut options = LaunchOptions::new(engine).headless(true).no_sandbox();
    if let Some(path) = executable {
        options = options.executable(path);
    }
    let caps = engine.capabilities(&options)?;
    let spawned: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let seen = spawned.clone();
    let manager = manager_for(engine)
        .on_status(move |status| {
            if let Status::DriverProcessSpawned { binary, .. } = status {
                *seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(binary.clone());
            }
        })
        .build();
    let session = manager
        .launch(caps)
        .await
        .with_context(|| format!("resolve the {engine} driver"))?;
    session
        .quit()
        .await
        .context("close the session that resolved the driver")?;
    let binary = spawned
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    binary.with_context(|| format!("the driver manager started no {engine} driver"))
}

/// A driver manager keeping its downloads in [`drivers_dir`]. Chromium gets the
/// chromedriver matching the local browser. Firefox gets the newest geckodriver,
/// which supports every Firefox geckodriver still targets: thirtyfour's own
/// compatibility table lags the releases. Offline, the newest cached one serves.
pub(crate) fn manager_for(engine: Engine) -> WebDriverManagerBuilder {
    let builder = WebDriverManager::builder().cache_dir(drivers_dir());
    match engine {
        Engine::Firefox => builder.latest(),
        Engine::Chromium | Engine::Webkit => builder,
    }
}

fn driver_binary(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Chromium => Some("chromedriver"),
        Engine::Firefox => Some("geckodriver"),
        Engine::Webkit => None,
    }
}
