# fix(locator): press the drag source before aiming the target, as Playwright does

- **Date:** 2026-10-19 14:29
- **Commit:** e905d65
- **Area:** Locator

## Problem

An application's e2e dragged a calendar event from its sidebar onto a grid slot and, now and then, a different event moved: the page opened its "Recurring event reschedule" dialog for a recurring card in the grid, and onday reported that the slot did not accept the drop.

## Cause

`drag_to` aimed the source, then aimed the target, and only then moved onto the source and pressed. Aiming the target scrolls it into view, so the point measured for the source was stale by the time of the press, and the press landed on whatever the scroll brought under it: in the calendar, the grip of a grid card, which the page drags with pointer events of its own. Playwright never aims the target before the press: it scrolls to the source, moves and presses, then scrolls to the target, moves and releases. Over Classic each move is anchored to an element the driver scrolls into view, and the move onto the target was still anchored to the source.

## Introduced

e8050d3 (2026-09-12), `feat(onday): WebDriver BiDi browser automation and an MCP server`, aimed both ends before the press. 09929cc (2026-10-18), `fix(locator): drag in Playwright's steps, one move onto the target`, re-aimed the target after the press but kept the aim before it.

## Fix

Before the press only the target's frame is resolved, which does not scroll. The source is aimed, moved onto and pressed; the target is aimed after that, and the move and release over Classic are anchored to the target.

## Verified

A new Chromium scenario drags from the top of a long page onto a drop zone the page scrolls to, with a decoy where the source stood before the scroll. On the old order it failed with `found "pressed the decoy"`; it passes now over BiDi and Classic. `cargo test -p onday --test browser chromium_over -- --ignored`, 2 passed; `cargo fmt --all --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.
