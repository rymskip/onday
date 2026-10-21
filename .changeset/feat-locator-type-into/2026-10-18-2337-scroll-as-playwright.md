# fix(locator): scroll before an action as Playwright does over BiDi

- **Date:** 2026-10-18 23:37
- **Commit:** 6529dd2
- **Area:** Locator

## Problem

An application's e2e typed into an input low in a fixed dialog and timed out after 60 seconds with the input "outside the viewport", every run, though a screenshot showed it scrolled into view.

## Cause

onday scrolled differently from Playwright. Before each probe its own prescroll scrolled the target into its box, then nudged it clear of any fixed or sticky bar over its centre. The input's floating label sat over that centre, so the hit landed on the label, and walking up from it found the dialog itself, which is fixed: the nudge took the dialog for a bar and scrolled the input back out of it on every attempt. Playwright has no such nudge. Over BiDi each of its attempts scrolls the target to the centre, then to end, centre and start in turn while the target stays out of view or something else takes the hit; that cycle is how it clears a sticky bar.

## Introduced

1f060d1 (2026-09-17), `Frames, lazy session directory, and headed-by-default MCP`, added the prescroll and its nudge. It stayed harmless for typing until 4c16a73 (2026-10-15), `fix(locator): check the hit when typing into a field, and re-scroll as Playwright does when it misses`, made `type_into` require the hit, which the nudged target never passed.

## Fix

Every attempt scrolls the target by Playwright's sequence (centre, end, centre, start) before probing it, moving to the next alignment when the target is outside the viewport or the hit misses. The prescroll, its script and the public `SCROLL_TO_TARGET_VIA_SCROLLBAR` are gone; `scroll_into_view` and the Classic click and type helpers scroll to the centre as Playwright does.

## Verified

A new Chromium scenario types into an input low in a fixed dialog under its floating label; on the old prescroll it failed with `<html> intercepts pointer events`, and it passes now, as do the existing scroll cases (a drop target taller than its box, frames, the sticky header). `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, and `cargo test -p onday --test browser chromium_over -- --ignored`, 2 passed.
