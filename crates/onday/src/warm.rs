//! Route warming: drive a real navigation through each URL so a dev server
//! transforms the route's module graph before anything timed runs. Readiness is
//! a caller-supplied JS predicate rather than the `load` event, so sessions can
//! use the `none` page-load strategy.

use crate::{Page, WaitUntil};
use anyhow::{Context, Result};
use std::time::{Duration, Instant};

/// Progress while warming one route, so callers own their log formatting.
pub enum WarmEvent {
    /// Navigation committed; time spent in `goto`.
    Navigated(Duration),
    /// Still waiting; elapsed time within the current slice, roughly every 5s.
    Waiting(Duration),
    /// A slice passed without readiness; about to reload. Total time so far.
    Reloading(Duration),
    /// Ready; total time spent on the route.
    Ready(Duration),
}

/// Poll `ready_script` (a function body returning a boolean) until it returns
/// `true` or `timeout` elapses.
pub async fn wait_for_ready<F>(
    page: &Page,
    ready_script: &str,
    timeout: Duration,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(Duration),
{
    let start = Instant::now();
    let predicate = format!("() => {{ {ready_script} }}");
    let held = crate::poll::poll_until_reporting(
        || async {
            match page.evaluate::<bool>(&predicate).await {
                Ok(ready) => ready,
                Err(error) => {
                    tracing::debug!("ready predicate failed: {error:#}");
                    false
                }
            }
        },
        timeout,
        crate::poll::Backoff::FAST,
        &mut on_progress,
    )
    .await;
    if held {
        return Ok(());
    }
    anyhow::bail!("ready predicate did not hold after {:?}", start.elapsed());
}

/// Navigate to `url`, then wait for `ready_script` in `slice`-sized windows,
/// reloading between them, up to `budget`. A dependency re-optimization can
/// strand a page unhydrated; waiting never recovers that, a reload does.
pub async fn warm_route<F>(
    page: &Page,
    url: &str,
    ready_script: &str,
    budget: Duration,
    slice: Duration,
    mut on_event: F,
) -> Result<()>
where
    F: FnMut(WarmEvent),
{
    let route_start = Instant::now();
    // Stamp the outgoing document so a poll cannot read its readiness after
    // `goto` returns early. Fails harmlessly when there is no document yet.
    let stamped = page
        .evaluate::<bool>("() => { window.__warmPending = true; return true; }")
        .await
        .is_ok();
    page.goto(url, WaitUntil::Commit)
        .await
        .with_context(|| format!("warm navigate failed for {url}"))?;
    on_event(WarmEvent::Navigated(route_start.elapsed()));

    let guarded = if stamped {
        format!("if (window.__warmPending === true) {{ return false; }} {ready_script}")
    } else {
        ready_script.to_string()
    };

    while route_start.elapsed() < budget {
        let wait = budget.saturating_sub(route_start.elapsed()).min(slice);
        let reached = wait_for_ready(page, &guarded, wait, |elapsed| {
            on_event(WarmEvent::Waiting(elapsed));
        })
        .await
        .is_ok();
        if reached {
            on_event(WarmEvent::Ready(route_start.elapsed()));
            return Ok(());
        }
        on_event(WarmEvent::Reloading(route_start.elapsed()));
        page.reload_until(WaitUntil::Commit)
            .await
            .with_context(|| format!("warm reload failed for {url}"))?;
    }

    anyhow::bail!(
        "route warm timed out for {url} after {:.0}s (including reload retries)",
        route_start.elapsed().as_secs_f32()
    )
}
