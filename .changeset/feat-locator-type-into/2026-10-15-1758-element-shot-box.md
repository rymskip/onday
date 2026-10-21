# fix(page): capture an element over BiDi as a box of the document, as Playwright does

- **Date:** 2026-10-15 17:58
- **Commit:** eb9fff7
- **Area:** Page

## Problem

Through the MCP, `browser_take_screenshot` of a map inside an app's modal form returned a picture of the page behind the dialog: the right size, the wrong place.

## Cause

Over BiDi, an element in the top frame was captured with an element clip, which leaves the browser to place the element. Chromium places an element inside a fixed container that scrolls on its own somewhere other than where it is drawn. Elements in frames already used a measured box and were unaffected.

## Introduced

e8050d3 (2026-09-12), "feat(onday): WebDriver BiDi browser automation and an MCP server", which captured top-frame elements with an element clip.

## Fix

Every BiDi element shot measures the element's box, moves it by its frames' origin and the page's scroll, and captures it as a box of the document, which is what Playwright's `bidiPage.takeScreenshot` sends for an element. The element clip variant, now unused, is gone.

## Verified

`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features` and `cargo test -p onday --test browser -- --ignored` (6 passed). The new `/shot` fixture holds a solid target 1500px down a fixed dialog that scrolls on its own, with the page behind scrolled 500px; the scenario screenshots the target and checks its middle pixel is its color. Before the fix Chromium over BiDi returned the dialog's black there.
