# fix(locator): check an action's target holds still before scrolling it, as Playwright does

- **Date:** 2026-10-19 15:04
- **Commit:** 6e47771
- **Area:** Locator

## Problem

In an application's e2e a click on a header button gave up after 60 seconds reporting the button still moving between two places 323 pixels apart, after a dialog had covered it for a while.

## Cause

Each attempt scrolled the target with its turn's alignment and only then probed its box, comparing it with the previous attempt's. A miss moves the next attempt to the next alignment, so the box read after each scroll differed from the last one, and a page that puts its scroll back between attempts never reads as still either. Playwright waits for the element to be visible and stable where the page leaves it, then scrolls it into view, then aims and checks the hit.

## Introduced

6529dd2 (2026-10-18), `fix(locator): scroll before an action as Playwright does over BiDi`, moved the scroll ahead of the probe.

## Fix

Each attempt probes visibility and stability first, with no scroll of its own between probes, then scrolls with its turn's alignment, then aims and checks the hit.

## Verified

`cargo test -p onday --test browser chromium_over -- --ignored`, 2 passed, covering the sticky header, the drop target taller than its box, the fixed dialog and the drag the page scrolls to. `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean. No scenario reproduces a page that resets its own scroll between attempts.
