//! Waiting until an element is interactable, not merely present.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thirtyfour::prelude::*;

use crate::js;
use crate::poll::{Backoff, poll_until};

#[derive(Debug, Deserialize)]
struct Probe {
    ok: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    rect: Option<[f64; 4]>,
}

const PROBE: &str = "(lib, selector, hover) => {
    const el = document.querySelector(selector);
    if (!el) return { ok: false, reason: 'not found' };
    return lib.probe(el, hover, false);
}";

async fn probe(driver: &WebDriver, selector: &str, hover: Option<&str>) -> Result<Probe> {
    let hover = serde_json::to_value(hover).context("serialize hover selector")?;
    let selector = serde_json::to_value(selector).context("serialize selector")?;
    let text = driver
        .execute(
            js::classic_body(&js::json_call(PROBE, "")),
            vec![selector, hover],
        )
        .await
        .context("run the interactability probe")?
        .convert::<String>()
        .context("read the probe result")?;
    serde_json::from_str(&text).with_context(|| format!("decode probe result {text}"))
}

/// Poll until the element is settled: sized, visible, opaque (outside the hooks'
/// hover-reveal ancestors), and at the same rect across two consecutive probes.
/// A target still moving takes the click at coordinates it has already left.
pub async fn wait_until_interactable(
    driver: &WebDriver,
    selector: &str,
    timeout: Duration,
) -> Result<()> {
    let start = Instant::now();
    let hooks = crate::hooks::global();
    let hover = hooks.hover_reveal_ancestor();
    // The last "why not" is the diagnostic; by the time a re-probe runs the page has moved on.
    let last_reason = Mutex::new(String::from("probe never ran"));
    let last_rect = Mutex::new(None::<[f64; 4]>);
    let interactable = poll_until(
        || async {
            let reason = match probe(driver, selector, hover).await {
                Ok(probe) if probe.ok => {
                    let mut previous = last_rect
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if *previous == probe.rect {
                        return true;
                    }
                    let reason = format!("still moving ({:?} -> {:?})", *previous, probe.rect);
                    *previous = probe.rect;
                    reason
                }
                Ok(probe) => probe.reason.unwrap_or_else(|| "unknown".to_string()),
                Err(error) => format!("probe failed: {error:#}"),
            };
            *last_reason
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = reason;
            false
        },
        timeout,
        Backoff::FAST,
        |_| {},
    )
    .await;
    if interactable {
        return Ok(());
    }
    bail!(
        "element {} never became interactable (last reason: {}, elapsed {:?})",
        selector,
        last_reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        start.elapsed()
    );
}
