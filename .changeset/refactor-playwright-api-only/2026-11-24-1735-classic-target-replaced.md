# fix(locator): aim again when a Classic click's target is replaced before input

- **Date:** 2026-11-24 17:35
- **Commit:** this commit
- **Area:** locator

## Problem

`firefox_over_classic` failed its click into a frame that keeps restarting its document:
"click iframe#restart >> #restart-input: execute script: Element is stale".

## Cause

Aiming retries a frame that replaces its document, but the dispatch that follows did not. Over
WebDriver Classic a pointer action first resolves its target by script to find the move's origin,
and a restart between aiming and that script left the element stale, which ended the click.

## Introduced

0101a06 (2026-11-19, "fix(locator): scroll only what a person could scroll (#1)") added the
scripted origin lookup to the Classic pointer, after 4f4099b (2026-10-21, "0.4.0: act on the page
as Playwright does, and type into a field like a person") gave aiming its retry.

## Fix

A stale target in the origin lookup is reported as `TargetReplaced`. No input has been sent at that
point, so `click` and `hover` record it and aim again; every other dispatch error still ends the
action.

## Verified

`cargo clippy --workspace --all-targets --all-features -- -D warnings` was clean and
`cargo test -p onday --test browser -- --ignored` passed 10 of 10, `firefox_over_classic` included.
