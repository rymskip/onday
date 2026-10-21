# fix(page): run the runtime in a sandbox the page cannot reach

- **Date:** 2026-10-14 17:22
- **Commit:** 56348d7
- **Area:** Page

## Problem

On a page with Stripe's split card fields, under Chromium over BiDi, every action inside the expiry and CVC frames failed with `script.callFunction: the browser did not answer within 30s`, as did a full snapshot of the page. Playwright typed into the same fields of the same page at once.

## Cause

Every BiDi call ran in the frame's own world (`sandbox: None`). Probing each frame of that page over BiDi with `() => 1 + 1`: chromedriver never answered a call into the own world of 8 of its 15 frames (Stripe's controller, its expiry and CVC frames, the link modal, the invisible hCaptcha frame and two link buttons), awaited or not, and answered the same call into a sandbox of every one of them at once. Over DevTools every frame's own world answered, so the loss is chromedriver's. Playwright never meets it because it runs its injected script in a utility world and only a caller's script in the page's.

The page's world also left the runtime open to the page: a page that replaces `JSON.stringify` breaks every runtime call.

## Introduced

1f060d1 (2026-09-17), "Frames, lazy session directory, and headed-by-default MCP", which added BiDi calls into frames, in the frame's own world.

## Fix

A BiDi call names its world. The runtime's installation and calls, selector resolution, the frame lookup, and onday's own reads of the document (title, content, viewport, the error-page check, `Locator::get_attribute`) run in an `onday` sandbox. A caller's `evaluate`, `eval_on` and `wait_for_function`, and the readiness check that reads the app's globals, stay in the page's world. Node references carry across, so elements resolved in the sandbox are handed to either. Classic has no sandbox and is unchanged.

## Verified

`cargo test -p onday --test browser chromium -- --ignored`: all four Chromium scenarios pass. The BiDi scenario now has a page replace `JSON.stringify` and `Array.isArray`, then clicks and reads through onday, which failed on reading the title before onday's own reads moved to the sandbox. Clippy with `-D warnings` and the unit tests pass. The Stripe page was not rerun through onday after the fix.
