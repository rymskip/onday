# feat(page): reach a tab's context, each match of a locator, and the raw driver on that tab

- **Date:** 2026-10-12 18:22
- **Commit:** b2df5c2
- **Area:** Page, Locator

## Problem

A caller holding only a `Page` could not open a second tab, find the tab a link opened, or touch the cookies: those live on the `BrowserContext`, and a page did not say which one it belongs to. Two `Page` values could not be compared, so "the tab that was not there before" had no expression.

A locator matching several elements gave their count and their texts, but no way to read an attribute of each or act on each in turn.

Over BiDi the raw driver kept acting on the session's first window. A Classic-only command, WebAuthn's virtual authenticator for one, attached to that window however many tabs had been opened since, and failed outright once the first tab was closed.

## Cause

`Page` kept its context private. `Locator` had `count`, `nth` and `all_inner_texts` only. `Page::driver` returns the driver as it is, and nothing switched its window for a BiDi page: `classic_window` refuses a page that is not Classic.

## Introduced

Not a regression: the API never had these.

## Fix

`Page::context` returns the page's `BrowserContext`, and `Page` is `PartialEq` on its tab. `Locator::all` returns one locator per match in document order. `Page::driver_on_tab` switches the session's window to the tab, using the BiDi context id as the window handle, before handing the driver out.

## Verified

The browser scenario opens a sibling tab from the page's context and requires both listed, asks for the driver on each tab and requires it on that tab's URL, and requires two locators for the two elements sharing a testid. It passed on Chromium and Firefox over BiDi and over Classic, six tests in all. Clippy with warnings denied exited 0.
