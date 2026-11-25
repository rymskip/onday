# refactor(api)!: expose no WebDriver client type

- **Date:** 2026-11-23 12:09
- **Commit:** 523cd61
- **Area:** API

## Problem

onday handed out thirtyfour in its public API:
- a `pub use thirtyfour` and the thirtyfour prelude
- `WebDriverExt` on a raw `WebDriver`
- `Browser::driver`, `Page::driver`, `Page::driver_on_tab` and the `bidi` accessors
- thirtyfour's `Cookie` and `PageLoadStrategy`
- `&WebDriver` taken by the WebAuthn and warm functions

A project using onday therefore had to depend on the same thirtyfour, and reached around
onday whenever it needed one of those. Two Firefox failures surfaced while covering the
replacements:
- A `goto` issued right after `go_back` never settled.
- Removing a virtual authenticator failed with `Expected "authenticatorId" to be a string`.

## Cause

- The raw layer predates the Playwright-style API and was never retired.
- Firefox sends `browsingContext.navigationStarted` without the `url` the spec requires when
  a history traversal starts. onday decoded it with thirtyfour's type, which requires the
  field, so the start was dropped. When the next `goto` was superseded by that navigation,
  its commit and load were ignored as belonging to an unknown navigation.
- thirtyfour 0.38.3 maps a Firefox version to a geckodriver through an embedded table that
  ends at 0.36.0. Firefox cannot read the snake-case authenticator id geckodriver 0.36.0
  sends, which was fixed in 0.37.

## Introduced

4f4099b (2026-10-21, "0.4.0: act on the page as Playwright does, and type into a field like a
person") added the Playwright-style API beside the raw layer.

## Fix

- **Own types:** `Cookie` and `SameSite`, `PageLoadStrategy`, and `Authenticator`.
- **Page methods:** `Page::add_virtual_authenticator`, `remove_virtual_authenticator`,
  `minimize_window`, `reload_until` and `wait_for_network_idle`.
  - Network idle counts only fetch and XHR requests, read from the request's
    `initiatorType`, which `NetworkEntry` now carries.
- **Warm:** `onday::warm` takes a `&Page`.
- **Drivers:**
  - `install_driver` resolves the driver matching a local browser, and `cached_drivers`
    lists the downloaded ones.
  - Downloads go to `$ONDAY_DRIVERS_DIR`, or `<user cache>/onday/drivers`.
  - Firefox resolves the newest geckodriver.
- **`Browser::debugger_address`** for a tool that attaches over the DevTools protocol.
- **Removed:** the raw accessors become crate-private, and `WebDriverExt` and its helpers are
  deleted.
- **Firefox navigations:** a navigation start decodes without a URL.

## Verified

- `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean.
- `cargo test --workspace --all-features` passed: 22 library tests and 10 MCP tests. They
  include new tests for the URL-less navigation start and for counting only fetch and XHR as
  open.
- `cargo test -p onday --test browser -- --ignored` passed all 10 browser tests on Chromium
  and Firefox, over BiDi and Classic. New coverage:
  - a cookie round trip in onday's type
  - an authenticator on each of two tabs
  - network idle failing on an open fetch, naming it, and then settling
- `cargo test -p onday --test flood -- --ignored` passed both flood tests.
