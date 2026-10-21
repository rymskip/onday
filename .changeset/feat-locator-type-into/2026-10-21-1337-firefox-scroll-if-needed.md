# fix(locator): leave a visible target unscrolled on Firefox too

- **Date:** 2026-10-21 13:37
- **Commit:** 6cf2156
- **Area:** Locator runtime, browser scenarios

## Problem

`firefox_over_classic` failed clicking an item of a menu that any scroll closes, with `stale element reference`. `firefox_over_bidi` failed earlier, on the console scenario: the warning carried one frame where the test wanted at least three.

## Cause

`scrollIntoViewIfNeeded` called the browser's own `Element.scrollIntoViewIfNeeded` and, where there is none, centred the target regardless. Firefox has none, so a target already in view was scrolled, the scroll closed the menu, and the click met a removed element.

Firefox's BiDi `log.entryAdded` sends only the top-most frame for a console warning, as its `log.sys.mjs` says (Bugzilla 1944136). onday keeps every frame it is sent; the scenario asked for frames Firefox never sends.

## Introduced

7d21aff (2026-10-20, fix(locator): scroll a visible target only when needed on the first attempt) added the menu scenario and kept the centring fallback; it ran on Chromium only. 6d2e5e6 (2026-10-19, feat(events): keep a console message's whole stack) added the three-frame check, also run on Chromium only.

## Fix

Where the browser has no native call, the runtime checks whether the target lies wholly inside the viewport and every ancestor that clips it, and leaves it where it is if so; otherwise it centres it, as Chromium's call does. The console scenario requires the whole stack on Chromium and the top-most frame on Firefox.

## Verified

`cargo test -p onday --test browser -- --ignored --test-threads=1`: 6 passed, Chromium and Firefox over BiDi and over Classic, the menu and console scenarios among them. `cargo test -p onday --test flood -- --ignored --test-threads=1`: both passed; all 1,202 requests settled 4.2 s into the flood on Chromium (slowest command 1.43 s) and 7.4 s on Firefox (slowest command 3.23 s). `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` and `cargo test --workspace --all-features --locked` (24 passed) passed.
