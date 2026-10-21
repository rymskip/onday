# fix(locator): scroll a visible target only when needed on the first attempt

- **Date:** 2026-10-20 12:10
- **Commit:** 7d21aff
- **Area:** Locator

## Problem

An application's e2e failed clicking a button in a message's hover bar: `<svg> from <button> subtree intercepts pointer events; detached from the document; no element matches`, for a full minute, though the message was on the page.

## Cause

The button sits in an actions bar a message shows only while the pointer is over it. Every pointer action scrolled its target to the viewport's centre on its first attempt, even one already in view, so the click's own scroll moved the message out from under the pointer and the bar, button and all, unmounted. Anything a scroll takes away behaves the same, such as a menu that closes when the page scrolls. Playwright on Chromium scrolls only a target out of view on its first attempt, through `DOM.scrollIntoViewIfNeeded`, and forces end, centre and start only on retries. The port had taken its first alignment from Playwright's BiDi backend, which serves Firefox and always centres. onday's `scrollIntoViewIfNeeded` centred unconditionally for the same reason.

## Introduced

1886433 (2026-10-19, feat(locator): take pointer actions through Playwright's retry and hit-target guard) set the first alignment to centre.

## Fix

The first attempt scrolls only as far as the target is out of view, and the retries keep forcing end, centre and start. `scrollIntoViewIfNeeded` uses the browser's own `Element.scrollIntoViewIfNeeded(true)` where it has one, as Chromium's protocol does, and centres where it does not, as Playwright's BiDi backend does.

## Verified

A new scenario clicks an item, visible below the viewport's centre, of a menu that any scroll closes, which a first-attempt scroll takes away as it did that bar. `cargo test -p onday --test browser chromium_over -- --ignored` passed both Chromium sessions. With the first alignment set back to centre, the BiDi session failed at that click with `no element matches`.
