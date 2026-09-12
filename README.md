# onday

Browser automation for Rust over WebDriver BiDi, with a WebDriver Classic fallback,
plus an MCP server built on it.

- **`onday`**: a library with a Playwright-style API. `Browser`, `BrowserContext`,
  `Page`, `Locator` and `expect` drive stock Chrome, Firefox and Safari/WebKitGTK through
  their own WebDriver servers.
- **`onday_mcp`**: an MCP server for coding agents. You can switch the engine at runtime,
  and each server process is one isolated session, so any number of them can share a
  working directory.

## Protocols

Every session asks for BiDi (`webSocketUrl: true`). If the driver doesn't offer it
(Safari and WebKitGTK today), the session uses WebDriver Classic. The API is the same
either way; `page.protocol()` tells you which one you got.

| | BiDi (Chrome, Firefox) | Classic fallback |
|---|---|---|
| Contexts | isolated user contexts in one browser | one WebDriver session each |
| Console, network | live event streams | in-page capture, read on demand |
| Init scripts | preload scripts at document start | replayed before each command |
| Request interception | `page.route` | not available |
| Screenshots | viewport, full page, element | viewport, element |

## Library

```rust
use onday::prelude::*;

let browser = Browser::launch(LaunchOptions::new(Engine::Firefox)).await?;
let page = browser.default_context(ContextOptions::default()).await?.page().await?;
page.goto("https://example.com", WaitUntil::Load).await?;
page.get_by_role("link", Some("More information")).click().await?;
expect_page(&page).to_have_url("**/iana.org/**").await?;
browser.close().await?;
```

Every action waits until its element is actionable before acting:

1. The element is resolved fresh.
2. It is scrolled into view through its nearest scrollable ancestor.
3. The action waits for the element to be sized, visible, opaque and enabled, and to
   hold still across two probes.
4. It checks that nothing else would receive the pointer at the click point.
5. It sends real pointer and key input.

Selectors are CSS by default. There are also `text=`, `role=button[name="Save"]`,
`testid=`, `label=`, `placeholder=`, `ref=` (from an aria snapshot) and `nth=`, and any
of them can be chained with `>>`.

`AppHooks` lets an application supply its own conventions: the test-id attribute, init
scripts, a readiness predicate for `WaitUntil::Ready`, hover-reveal containers, and which
fetches count as in-flight writes.

`onday::dom::WebDriverExt` offers the same waiting helpers on a raw `thirtyfour::WebDriver`.

## MCP server

```sh
cargo install --path crates/onday_mcp
onday_mcp --help
```

Each process owns `.onday/<session>/`:

- `session.json`
- `lock`: an OS file lock, released when the process dies.
- `profile-<engine>/`
- `logs/{console,network,driver,mcp}.log`
- `screenshots/`, `downloads/`, `snapshots/`

`--session` (or `ONDAY_SESSION`) names the session; otherwise the name is generated.

The tools follow playwright-mcp's names, plus:

- `browser_launch` switches engine at runtime.
- `browser_route` and `browser_unroute` intercept requests.
- `browser_status` reports the session.

Tools that target an element accept a snapshot `ref` or a `selector`.

## Tests

```sh
cargo test                       # unit tests
ONDAY_CHROMIUM_EXECUTABLE=/path/to/chrome ONDAY_FIREFOX_EXECUTABLE=/path/to/firefox \
  cargo test -p onday --test browser -- --ignored   # real browsers, BiDi and Classic
```
