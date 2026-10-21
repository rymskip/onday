# fix(locator): drop only where the target takes the pointer, as Playwright does

- **Date:** 2026-10-18 11:58
- **Commit:** ef2d74a
- **Area:** Locator

## Problem

A drag onto a drop zone clipped by its own scroll box reported success over BiDi without the drop landing, and over Classic failed with "did not accept the drop". The fixture's event log showed the pointer released below the box's visible part, over another element, with the box never scrolled.

## Cause

`drag_to` checked the source's hit but not the target's, and `hitPoint` accepted an ancestor of the target as a hit. The aim point, the target's centre clipped only to the window, lay where the box hid the target and the page behind answered, and an ancestor answering counted as reaching the target.

## Introduced

e8050d3 (2026-09-12), "feat(onday): WebDriver BiDi browser automation and an MCP server", which wrote both the unchecked drop target and the ancestor rule in `hitPoint`.

## Fix

The drop target's hit is checked, so a miss scrolls again with the next alignment, as Playwright's drag does; and `hitPoint` accepts only the target or something inside it, as Playwright's `expectHitTarget` does. A new browser scenario drags onto a 1200px drop zone in a 300px box.

## Verified

`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, and `cargo test -p onday --test browser -- --ignored chromium`: 4 passed, the drops into same-origin and cross-origin frames among them. The new scenario failed with the scroll fix alone and with the target's hit checked but ancestors still accepted, and passes with both.
