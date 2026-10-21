# fix(locator): finish a drag whose page re-renders its source on drop

- **Date:** 2026-10-18 12:17
- **Commit:** 1e23ff6
- **Area:** Locator

## Problem

In an application's e2e, a sidebar card dragged onto a grid slot was rescheduled, yet the drag failed with "did not accept the drop". Over Classic the same kind of drag failed with "stale element reference".

## Cause

`watchDrag` counted a native drag as delivered only on a trusted `dragend` at the window. The page re-rendered the dragged card on drop, detaching it, and a detached element's `dragend` never reaches the window. onday took the completed drop for a failed one and replayed it with synthetic events, whose drop hit the event the real drop had placed there. The synthetic path also ended the drag through the source element's handle, stale once the page removed it.

## Introduced

e8050d3 (2026-09-12), "feat(onday): WebDriver BiDi browser automation and an MCP server", which wrote `watchDrag` and the synthetic drag's `endDrag` call.

## Fix

`watchDrag` also counts a trusted `drop` as delivery, and the synthetic drag ends in the source's frame, where the drag state lives, instead of on its handle. The tall-target fixture's drop now removes the source.

## Verified

With the fixture change and the old `watchDrag`, both Chromium scenarios failed: "did not accept the drop" over BiDi and "stale element reference" over Classic. With the fix: `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, and `cargo test -p onday --test browser -- --ignored chromium`, 4 passed.
