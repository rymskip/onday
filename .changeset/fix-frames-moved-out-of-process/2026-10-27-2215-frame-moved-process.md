# fix(page): restore a frame whose document chromedriver lost when it moved process

- **Date:** 2026-10-27 22:15
- **Commit:** d91bdd0
- **Area:** Page

## Problem

Typing into Stripe's split card fields on the first page of a fresh Chromium browser hung for 30 s on the expiry or CVC field: `script.callFunction: the browser did not answer within 30s`. The card number field worked, and later pages in the same browser worked. Moving the runtime into a sandbox (56348d7) had stopped it once, but chromedriver 156 hangs in the sandbox too.

## Cause

The fault is in chromedriver's bundled BiDi layer (chromium-bidi). A frame that starts blank in its page's process and then loads from another site moves to a process of its own. On that move the layer adds listeners on the frame's new DevTools session but keeps the ones on the page's session. Shortly after, the page's session reports the frame's blank document destroyed. Execution context ids are only unique per session, and in a fresh browser the new process numbers its contexts from the same small numbers. When the blank document's id equals the one the frame's real document got in its new process, the stale listener resets the frame's default-realm promise and deletes the real document's realm. Every later `script.callFunction` addressed to the frame, in its own world or any sandbox, and every `input.performActions` sent to it (which reads the platform in a hidden sandbox first) waits on that promise forever. No DevTools command is sent at all.

A verbose chromedriver log of the hang showed it directly. The card frames' new default contexts were ids 3, 4 and 5 on their own sessions. The page's session then destroyed its contexts 4, 5 and 6, and the two frames whose new id matched (expiry 4, CVC 5) never answered again. The card number frame (3) kept working. chromium-bidi fixed it upstream on 2026-10-27 in 2ade9ec, "Only apply frame and session events from their owner", which drops a context's listeners when it changes target. No chromedriver release carries that fix yet.

## Introduced

1f060d1 (2026-09-17), "Frames, lazy session directory, and headed-by-default MCP", which first sent BiDi calls into frames. The driver's fault predates onday.

## Fix

Calls, pointer and key input, and file uploads into a child frame first ask the driver for the frame's realms (`script.getRealms`). When it lists no page-world realm, onday restarts DevTools' Runtime domain in the frame's own session (`goog:cdp.getSession`, then `Runtime.disable` and `Runtime.enable` through `goog:cdp.sendCommand`). That makes the frame report its existing documents again, and the layer's live listener restores its record. A frame still loading gets up to 5 s to report its document, after which the call proceeds and waits as before. Drivers that answer `goog:cdp` with `unknown command` are remembered per page and keep the old path.

## Verified

- A new scenario, `chromium_types_into_frames_that_moved_process`, mounts eight frames that start blank and then load from a site no other fixture uses, then types into each. Before the fix it failed three runs out of three on `card-1`. With the fix it passed three runs out of three, and a run at debug level logged the restore for seven of the eight frames.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean, and `cargo test --workspace` passes 24 tests.
- `cargo test -p onday --test browser chromium -- --ignored` passes all five Chromium scenarios, and the Chromium flood test passes.
- The Firefox scenarios did not run: no geckodriver is installed on this machine.
