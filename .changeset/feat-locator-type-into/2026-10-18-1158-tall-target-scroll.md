# fix(locator): scroll a target taller than its box once, as Playwright does

- **Date:** 2026-10-18 11:58
- **Commit:** b8d65e0
- **Area:** Locator

## Problem

A drag onto a drop zone taller than the box that scrolls it timed out with "still moving": the zone's position flipped between y = -2532 and y = 140 on every probe, for a drop zone 3362px tall in a 689px box.

## Cause

The prescroll in `scroll_to_target.js` aligned whichever edge of the target lay outside its scroll box. For a target taller than the box, aligning the top pushed the bottom out and the next probe aligned the bottom, so the target never held still across two probes.

## Introduced

e8050d3 (2026-09-12), "feat(onday): WebDriver BiDi browser automation and an MCP server", which wrote the prescroll's edge alignment.

## Fix

The prescroll follows `scrollIntoView({ block: 'nearest' })`, the rule Playwright's scroll-if-needed keeps: a target that spans the box is left where it is, and one taller than the box is aligned once.

## Verified

`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, and `cargo test -p onday --test browser -- --ignored chromium`: 4 passed. The onday MCP's drag onto that drop zone reproduced the flip before the fix.
