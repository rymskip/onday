# feat(locator): type into a field like a person and check what it holds

- **Date:** 2026-10-12 15:54
- **Commit:** 5eed46d
- **Area:** Locator

## Problem

A locator had no way to type into a field the way a person does. `fill` clears the field from script before it types, and nothing checks afterwards what the field holds.

## Cause

173965d (2026-10-01) gave the raw session `type_into_selector`: a pointer click to focus, select-all and one Backspace, the keys, then every typed character required in the field in order. It was written against the WebDriver session and never reached `Locator`.

## Introduced

Not a regression: 173965d added the behavior to one of the two ways of driving a page.

## Fix

`Locator::type_into` does the same through the page's own pointer and keyboard. The field's state type and the typed-in-order check moved to `typing.rs`, which both paths use, with their unit tests.

## Verified

`cargo test -p onday --test browser -- --ignored --test-threads=1 chromium_over firefox_over`: the typing scenario (a floating label, a wrapped label, a phone mask, an input counter, a number, a textarea, a contenteditable, a field under a toast, a date input) runs once through the raw session and once through a locator, and passed on Chromium and Firefox over BiDi and over Classic.

`cargo clippy --workspace --all-targets --all-features -- -D warnings` exited 0.
