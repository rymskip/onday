# feat(locator): take pointer actions through Playwright's retry and hit-target guard

- **Date:** 2026-10-19 15:37
- **Commit:** 1886433
- **Area:** Locator

## Problem

An application's e2e hovered one chat message and the actions bar opened on the message above it: between aiming and moving the pointer the timeline shifted, and the hover landed on whatever slid under the point. onday reported the hover done.

## Cause

onday's pointer actions were not Playwright's. They checked the hit only when they aimed, never during the input; checked stability across separate attempts rather than over animation frames; advanced the scroll alignment only on a miss; aimed at the middle of the bounding box; checked the hit against the exact element; and counted transparency as invisible. Playwright's `_retryPointerAction` and `_performPointerAction` differ on each point, and its hit-target interceptor blocks a gesture whose first event lands off the target so the action retries.

## Introduced

e8050d3 (2026-09-12), `feat(onday): WebDriver BiDi browser automation and an MCP server`, wrote the pointer actions with their own checks and no guard.

## Fix

Click, hover, the typing click and drag share one attempt, ported from Playwright: stable over animation frames (`_checkElementIsStable`, one frame over BiDi), visible as `isElementVisible`, and for a click enabled (`getAriaDisabled`) and not inert; scrolled with the attempt's alignment (centre, end, centre, start), advancing on every retry; aimed at the middle of the first client rect in the viewport (`_clickablePoint`); the hit checked with `expectHitTarget` against the target retargeted to its button or link; and the guard raised (`setupHitTargetInterceptor`). Attempts wait 0, 20, 100, 100 then 500 ms. A blocked drag press is pressed again with no release, and a drop is checked only at its point, as in Playwright.

Not ported: `fill` and `select` keep onday's own checks, which still count transparency as invisible, and the typing click, which has no Playwright counterpart, still counts a hit on the field's bound label.

## Verified

A new Chromium scenario clicks a button the page covers on its first press: without the guard the click was lost (`found "waiting"`), and it passes now. `cargo test -p onday --test browser chromium_over -- --ignored`, 2 passed over BiDi and Classic, covering the floating label, the sticky header, the fixed dialog and the drags. `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.
