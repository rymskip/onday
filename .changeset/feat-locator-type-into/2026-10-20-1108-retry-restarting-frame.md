# fix(locator): retry a pointer action whose target's frame restarts

- **Date:** 2026-10-20 11:08
- **Commit:** c11ad3f
- **Area:** Locator

## Problem

An application's e2e failed clicking into Stripe's card number field: `resolve iframe[title="Secure card number input frame"] >> input[name="cardnumber"]: the page runtime was gone again right after it was installed`.

## Cause

A payment field's iframe replaces its document while it starts. A click resolving into it can install onday's runtime in one document and call into it after the next has replaced it. `aim_pointer` returned that error at once, so the whole action failed, where onday's own `wait_one` and `actionable` loops, and Playwright's `retryWithProgressAndTimeouts`, take it as another reason to try again.

## Introduced

1886433 (2026-10-19, feat(locator): take pointer actions through Playwright's retry and hit-target guard) wrote `aim_pointer` with its errors propagated.

## Fix

`aim_pointer` records an attempt's error as a retry. As Playwright's `isNonRetriableError` does for a closed session, only a browser that is gone ends the action, and a strictness violation still fails it as before.

## Verified

A new scenario clicks into a frame that replaces its document twenty times before it settles, then types into it. `cargo test -p onday --test browser chromium_over -- --ignored` passed both Chromium sessions. With the retry taken back out, the BiDi session failed at that click with `Cannot find context with specified id`.
