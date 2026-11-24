# feat(mcp): open a tab in a named container

- **Date:** 2026-11-23 23:11
- **Commit:** this commit
- **Area:** MCP

## Problem

The MCP server kept every tab in the browser's one context, so two signed-in users could not be
driven side by side: switching user meant signing out and back in, or a second browser.

## Cause

`browser_tabs` only opened tabs in the default context, though the library already creates
isolated contexts (`Browser::new_context`, a BiDi user context, which Firefox keeps as a
container).

## Introduced

None: the server never offered more than one context.

## Fix

`browser_tabs` with action `new` takes a `container` name: the tab opens in that container, a
context with its own cookies and storage, created the first time it is named. Every tab is listed
with its container and selected or closed by its listed number. The current tab is held as its
container and its place there, so tabs opening or closing in one container do not move another's.
A new tab is brought to the front: Firefox does not paint a background tab, so a screenshot of one
showed a frame from before its last change.

## Verified

`cargo clippy -p onday_mcp --all-targets --all-features -- -D warnings` is clean and
`cargo test -p onday_mcp` passed (13), the listing's new tests among them.
Through the installed server, Firefox over BiDi opened a tab in container `karen` and one in
`admin`; each signed in as a different user, and each tab's user menu named its own user while the
other stayed signed in.
