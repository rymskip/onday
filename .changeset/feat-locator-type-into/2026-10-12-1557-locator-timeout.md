# feat(locator): let a locator carry its own timeout, and wait on it

- **Date:** 2026-10-12 15:57
- **Commit:** 92988e1
- **Area:** Locator

## Problem

One call could not ask for a budget of its own. A click had its options and a wait took the budget as an argument, but typing, hovering, checking and the rest always used the page's default, and a wait could not use the default without being handed it.

## Cause

The budget was a property of the page and, case by case, of an action's arguments. The locator held none.

## Introduced

e8050d3 (2026-09-12), which gave `wait_for` its timeout argument and the other actions none.

## Fix

`Locator::with_timeout` returns the same locator with its own budget. Every action and wait on it uses that budget, and every locator refined from it keeps it. `wait_for` takes its budget the same way.

This changes `wait_for`'s signature: `wait_for(state, timeout)` is `with_timeout(timeout).wait_for(state)`, or `wait_for(state)` for the page's default. The MCP server's three calls are updated.

## Verified

The browser scenario waits on a visible field with the page's default, and on an element that never renders with a 300 ms budget, which has to fail within three seconds. It passed on Chromium and Firefox over BiDi and over Classic, with `cargo test --workspace` and clippy as above.
