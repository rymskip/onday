//! Bounded BiDi commands, and a liveness probe that tells a vanished browser from a
//! slow one. A driver can keep the BiDi socket open after its browser dies, so a
//! reply may never come; every command races a watchdog and a budget instead.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use thirtyfour::WebDriver;
use thirtyfour::bidi::{BiDi, BidiCommand};
use thirtyfour::error::{WebDriverError, WebDriverErrorInner};

use crate::page::DEFAULT_TIMEOUT;

/// How long a liveness probe may take before the browser counts as unresponsive.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often a command still waiting for its reply probes the browser.
const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Whether the browser behind a session still answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Alive,
    /// The driver did not answer the probe in time; the browser may only be busy.
    Unresponsive,
    /// The browser closed or crashed; the session cannot be used again.
    Gone(String),
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Health::Alive => f.write_str("alive"),
            Health::Unresponsive => f.write_str("unresponsive"),
            Health::Gone(reason) => write!(f, "gone ({reason})"),
        }
    }
}

/// A command failed because the browser closed or crashed outside onday.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserGone {
    pub reason: String,
}

impl fmt::Display for BrowserGone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the browser is gone: {}", self.reason)
    }
}

impl std::error::Error for BrowserGone {}

/// Probes a session's browser over WebDriver Classic and remembers when it is gone.
pub(crate) struct Liveness {
    driver: WebDriver,
    gone: Mutex<Option<String>>,
}

impl Liveness {
    pub(crate) fn new(driver: WebDriver) -> Arc<Liveness> {
        Arc::new(Liveness {
            driver,
            gone: Mutex::new(None),
        })
    }

    /// Why the browser is gone, if a probe or a command found out.
    pub(crate) fn gone(&self) -> Option<String> {
        self.gone
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn mark_gone(&self, reason: String) {
        let mut gone = self
            .gone
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if gone.is_none() {
            tracing::warn!("the browser is gone: {reason}");
            *gone = Some(reason);
        }
    }

    /// Ask the driver for the browser's windows: none, or an error, means it is gone.
    pub(crate) async fn probe(&self) -> Health {
        if let Some(reason) = self.gone() {
            return Health::Gone(reason);
        }
        let health = match tokio::time::timeout(PROBE_TIMEOUT, self.driver.windows()).await {
            Err(_) => Health::Unresponsive,
            Ok(Ok(windows)) if windows.is_empty() => {
                let reason = "no windows left".to_string();
                self.mark_gone(reason.clone());
                Health::Gone(reason)
            }
            Ok(Ok(windows)) => {
                tracing::trace!("probe: {} window(s) open", windows.len());
                Health::Alive
            }
            Ok(Err(error)) => match vanished(&error) {
                Some(reason) => {
                    self.mark_gone(reason.clone());
                    Health::Gone(reason)
                }
                None => {
                    tracing::warn!(
                        "the liveness probe failed without proving the browser gone: {error}"
                    );
                    Health::Unresponsive
                }
            },
        };
        tracing::debug!("probed the browser: {health}");
        health
    }

    /// Resolves with the reason once a periodic probe finds the browser gone.
    async fn watch(&self) -> String {
        loop {
            tokio::time::sleep(PROBE_INTERVAL).await;
            match self.probe().await {
                Health::Gone(reason) => return reason,
                Health::Unresponsive => {
                    tracing::warn!("the driver did not answer a probe within {PROBE_TIMEOUT:?}")
                }
                Health::Alive => {}
            }
        }
    }
}

/// A session's BiDi connection, with every command bounded.
#[derive(Clone)]
pub(crate) struct BidiChannel {
    bidi: BiDi,
    liveness: Arc<Liveness>,
}

impl BidiChannel {
    pub(crate) fn new(bidi: BiDi, liveness: Arc<Liveness>) -> BidiChannel {
        BidiChannel { bidi, liveness }
    }

    /// The unbounded thirtyfour connection.
    pub(crate) fn raw(&self) -> &BiDi {
        &self.bidi
    }

    pub(crate) async fn send<C: BidiCommand>(&self, command: C) -> Result<C::Returns> {
        self.send_within(command, DEFAULT_TIMEOUT).await
    }

    /// Send `command`, failing once the browser is found gone or `budget` runs out.
    pub(crate) async fn send_within<C: BidiCommand>(
        &self,
        command: C,
        budget: Duration,
    ) -> Result<C::Returns> {
        if let Some(reason) = self.liveness.gone() {
            tracing::debug!("refusing {} to a vanished browser: {reason}", C::METHOD);
            return Err(BrowserGone { reason }).context(C::METHOD);
        }
        tracing::trace!("sending {} (budget {budget:?})", C::METHOD);
        tokio::select! {
            reply = self.bidi.send(command) => match reply {
                Ok(returns) => Ok(returns),
                Err(error) if error.is_session_ended() => {
                    let reason = format!("its WebDriver session ended: {}", error.message);
                    self.liveness.mark_gone(reason.clone());
                    Err(BrowserGone { reason }).context(C::METHOD)
                }
                // A missing context may be one closed tab or a browser that went away,
                // and an unknown error covers the socket dropping with it; only the
                // driver can tell which.
                Err(error) if error.is_no_such() || error.error == "unknown error" => {
                    match self.liveness.probe().await {
                        Health::Gone(reason) => Err(BrowserGone { reason }).context(C::METHOD),
                        Health::Alive | Health::Unresponsive => Err(error).context(C::METHOD),
                    }
                }
                Err(error) => Err(error).context(C::METHOD),
            },
            reason = self.liveness.watch() => {
                tracing::warn!("abandoning {}: the browser is gone", C::METHOD);
                Err(BrowserGone { reason }).context(C::METHOD)
            }
            () = tokio::time::sleep(budget) => {
                tracing::warn!("{} got no answer within {budget:?}", C::METHOD);
                Err(anyhow!("the browser did not answer within {budget:?}")).context(C::METHOD)
            }
        }
    }
}

/// Why a failed probe proves the browser gone, or `None` when it does not.
fn vanished(error: &WebDriverError) -> Option<String> {
    match error.as_inner() {
        WebDriverErrorInner::InvalidSessionId(info) => Some(format!(
            "its WebDriver session ended: {}",
            info.value.message
        )),
        WebDriverErrorInner::NoSuchWindow(info) => {
            Some(format!("its window closed: {}", info.value.message))
        }
        // chromedriver reports a dead browser as "disconnected" or "not reachable".
        WebDriverErrorInner::UnknownError(info)
            if info.value.message.contains("disconnected")
                || info.value.message.contains("not reachable") =>
        {
            Some(info.value.message.clone())
        }
        WebDriverErrorInner::RequestFailed(detail) | WebDriverErrorInner::HttpError(detail) => {
            Some(format!("the driver is unreachable: {detail}"))
        }
        _ => None,
    }
}
