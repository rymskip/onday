# fix(locator): scroll only what a person could scroll

- **Date:** 2026-11-19 12:00
- **Commit:** e9ae7bd
- **Area:** Locator, Page

## Problem

In an application's e2e, a click into a side drawer shifted the whole drawer sideways, though the drawer clips sideways with `overflow-x: hidden` and no person can scroll it that way. A blurred decoration in its header made it far wider inside than it shows, so the shift was over a thousand pixels, and the click then landed on a screen no person could see.

## Cause

Each retry of a pointer action scrolled the target with `scrollIntoView({ block, inline: block })` at `end`, `center` and `start` in turn, as Playwright's forced scroll options do. The browser's `scrollIntoView` and `scrollIntoViewIfNeeded` scroll every box around the target, a box that clips with `overflow: hidden` or `clip` included, and so does a page an open modal has locked. Frame reveals scrolled the window the same way, and Classic's wheel fallback scrolled the window when nothing under the pointer scrolled.

Aiming had the matching fault: it pressed at the centre of the target cut to the viewport only, so a target a clipping box cut off was aimed at what lay beneath the box, and only a forced scroll of the box made the aim land. Over Classic, a press was offset from the target itself, which chromedriver scrolls fully into view before it moves, through a clipping box too.

## Introduced

4f4099b (2026-10-21), "0.4.0: act on the page as Playwright does, and type into a field like a person", which added the retry alignments, the native `scrollIntoViewIfNeeded`, the frame reveal and the Classic wheel fallback. Offsetting Classic presses from the target dates from 1f060d1 (2026-09-17), "Frames, lazy session directory, and headed-by-default MCP".

## Fix

Every scroll onday makes puts back, on each axis, the offset of every box around the target that clips without scrolling, and of the page when its overflow is locked. A target only such a scroll would reveal stays out of reach and the action fails rather than act on it. Aiming presses at the middle of the part of the target a person sees, cut by every box that clips it, and reports a target no part of which shows as hidden by that box. A Classic press is offset from the nearest of the target and its ancestors that shows whole and holds the aimed point, which no driver scrolls. Classic's wheel fallback moves only the axes a person's wheel could.

## Verified

`cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features` and `cargo test -p onday --test browser -- --ignored` (7 passed) in Chromium and Firefox over BiDi and Classic. The new `/locked` fixture, a drawer that clips sideways under a cover the first press raises, failed before the fix with the drawer shifted 220px over 5 scrolls; after it the drawer never shifts, and a target past its clipped edge fails the click. The `#clipped` field now types through its visible strip with its clipping box unscrolled after the press, which failed by 30px over Classic in Chromium until presses were offset from a box shown whole.
