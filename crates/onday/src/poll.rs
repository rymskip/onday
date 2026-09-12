//! Adaptive polling for "wait until this holds" conditions.
//!
//! The gap starts below a frame and grows, so an already-true condition costs
//! one probe and a slow one settles into a cheap heartbeat.

use anyhow::Result;
use std::future::Future;
use std::time::{Duration, Instant};

/// How the gap between probes grows while a condition has not yet held.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub start: Duration,
    pub factor: f64,
    pub max: Duration,
}

impl Backoff {
    /// In-page conditions: first re-probe inside a frame, settling near 2.5 probes a second.
    pub const FAST: Backoff = Backoff {
        start: Duration::from_millis(15),
        factor: 1.6,
        max: Duration::from_millis(400),
    };

    /// Process start-up conditions, where each probe is a connect or a subprocess.
    pub const SERVICE: Backoff = Backoff {
        start: Duration::from_millis(25),
        factor: 1.5,
        max: Duration::from_millis(500),
    };

    /// The gap that follows `current`, for retry loops that are not a predicate.
    pub fn next_gap(&self, current: Duration) -> Duration {
        current.mul_f64(self.factor).min(self.max)
    }
}

/// Poll `probe` until it reports `true` or `budget` elapses; returns whether it held.
///
/// `on_progress` fires roughly every 5s with the elapsed time.
pub async fn poll_until<F, Fut>(
    mut probe: F,
    budget: Duration,
    backoff: Backoff,
    mut on_progress: impl FnMut(Duration),
) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = Instant::now();
    let mut gap = backoff.start;
    let mut next_report = Duration::from_secs(5);
    loop {
        if probe().await {
            return true;
        }
        let elapsed = start.elapsed();
        if elapsed >= next_report {
            on_progress(elapsed);
            next_report = elapsed + Duration::from_secs(5);
        }
        // Checked after the probe so a zero budget still tries once.
        if elapsed > budget {
            return false;
        }
        tokio::time::sleep(gap).await;
        gap = backoff.next_gap(gap);
    }
}

/// [`poll_until`] for a fallible probe. Errors count as "not yet"; the last one
/// is returned when the budget runs out.
pub async fn poll_until_ok<F, Fut, T>(
    mut probe: F,
    budget: Duration,
    backoff: Backoff,
) -> Result<T, Option<anyhow::Error>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    let start = Instant::now();
    let mut gap = backoff.start;
    let mut last_error: Option<anyhow::Error> = None;
    loop {
        match probe().await {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(e) => last_error = Some(e),
        }
        if start.elapsed() > budget {
            return Err(last_error);
        }
        tokio::time::sleep(gap).await;
        gap = backoff.next_gap(gap);
    }
}

/// Blocking [`poll_until`], for waits that run outside an async runtime.
pub fn poll_until_blocking(
    mut probe: impl FnMut() -> bool,
    budget: Duration,
    backoff: Backoff,
) -> bool {
    let start = Instant::now();
    let mut gap = backoff.start;
    loop {
        if probe() {
            return true;
        }
        if start.elapsed() > budget {
            return false;
        }
        std::thread::sleep(gap);
        gap = backoff.next_gap(gap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_is_capped() {
        let b = Backoff::FAST;
        let mut gap = b.start;
        for _ in 0..100 {
            gap = b.next_gap(gap);
        }
        assert_eq!(gap, b.max);
    }

    #[test]
    fn an_already_true_condition_costs_one_probe() {
        let mut probes = 0;
        let held = poll_until_blocking(
            || {
                probes += 1;
                true
            },
            Duration::ZERO,
            Backoff::FAST,
        );
        assert!(held);
        assert_eq!(probes, 1);
    }

    #[test]
    fn a_never_true_condition_gives_up_after_the_budget() {
        let start = Instant::now();
        let held = poll_until_blocking(|| false, Duration::from_millis(60), Backoff::FAST);
        assert!(!held);
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
