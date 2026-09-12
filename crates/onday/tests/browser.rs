//! End-to-end scenario against a local fixture server, per engine and protocol.
//!
//! Needs real browsers, so the tests are ignored by default:
//! `cargo test -p onday --test browser -- --ignored`. Browser binaries come from
//! `ONDAY_CHROMIUM_EXECUTABLE` / `ONDAY_FIREFOX_EXECUTABLE` when the driver cannot
//! discover them.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use onday::prelude::*;
use onday::{
    AppHooks, DialogPolicy, Fulfill, Protocol, ProtocolPreference, RouteAction, ScreenshotOptions,
    StrictModeViolation,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const INDEX: &str = include_str!("fixture/index.html");
const SECOND: &str = include_str!("fixture/second.html");

async fn serve() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the fixture server")?;
    let address = listener.local_addr().context("fixture address")?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&request);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, kind, body) = match path.as_str() {
                    "/" => ("200 OK", "text/html; charset=utf-8", INDEX.to_string()),
                    "/second" => ("200 OK", "text/html; charset=utf-8", SECOND.to_string()),
                    "/api/data" => (
                        "200 OK",
                        "application/json",
                        r#"{"value":"real"}"#.to_string(),
                    ),
                    _ => ("404 Not Found", "text/plain", "missing".to_string()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                if let Err(error) = stream.write_all(response.as_bytes()).await {
                    eprintln!("fixture write failed: {error}");
                }
            });
        }
    });
    Ok(format!("http://{address}"))
}

struct FixtureHooks;

impl AppHooks for FixtureHooks {
    fn ready_script(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("return window.__appReady === true;"))
    }

    fn hover_reveal_ancestor(&self) -> Option<&str> {
        Some(".group")
    }
}

fn launch_options(engine: Engine, protocol: ProtocolPreference) -> LaunchOptions {
    let mut options = LaunchOptions::new(engine)
        .protocol(protocol)
        .no_sandbox()
        .window_size(1280, 900);
    let variable = match engine {
        Engine::Chromium => "ONDAY_CHROMIUM_EXECUTABLE",
        Engine::Firefox => "ONDAY_FIREFOX_EXECUTABLE",
        Engine::Webkit => "ONDAY_WEBKIT_EXECUTABLE",
    };
    if let Ok(path) = std::env::var(variable) {
        options = options.executable(path);
    }
    options
}

fn ref_of(snapshot: &str, needle: &str) -> Result<String> {
    let line = snapshot
        .lines()
        .find(|line| line.contains(needle))
        .with_context(|| format!("no snapshot line contains {needle}:\n{snapshot}"))?;
    let start = line.find("[ref=").context("line has no ref")? + 5;
    let end = line[start..].find(']').context("unterminated ref")? + start;
    Ok(line[start..end].to_string())
}

fn mock_data(request: &onday::InterceptedRequest) -> RouteAction {
    if request.method == "GET" {
        RouteAction::Fulfill(Fulfill::json(200, r#"{"value":"mocked"}"#))
    } else {
        RouteAction::Continue
    }
}

async fn status_is(page: &Page, expected: &str) -> Result<()> {
    expect(&page.locator("#status"))
        .to_have_text(expected)
        .await
}

async fn scenario(engine: Engine, preference: ProtocolPreference) -> Result<()> {
    let installed = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    if let Err(error) = installed {
        eprintln!("tracing already installed: {error}");
    }
    let base = serve().await?;
    let browser = Browser::launch(launch_options(engine, preference)).await?;
    let expected = if preference == ProtocolPreference::Classic {
        Protocol::Classic
    } else {
        Protocol::Bidi
    };
    ensure!(
        browser.protocol() == expected,
        "negotiated {} instead of {expected}",
        browser.protocol()
    );
    let result = exercise(&browser, &base).await;
    let closed = browser.close().await;
    result?;
    closed
}

async fn exercise(browser: &Browser, base: &str) -> Result<()> {
    let bidi = browser.protocol() == Protocol::Bidi;
    let context = browser
        .default_context(ContextOptions {
            hooks: Some(Arc::new(FixtureHooks)),
            dialog_policy: DialogPolicy::Leave,
            ..ContextOptions::default()
        })
        .await?;
    context
        .add_init_script("window.__initRan = (window.__initRan || 0) + 1;")
        .await?;
    let page = context.page().await?;
    page.set_default_timeout(Duration::from_secs(10));
    page.goto(base, WaitUntil::Load).await?;
    expect_page(&page).to_have_title("Onday fixture").await?;
    ensure!(
        page.evaluate::<u32>("window.__initRan").await? >= 1,
        "init script did not run"
    );

    // Raw-driver evaluation takes expressions, async IIFEs and statement one-liners.
    let driver = page.driver();
    let title: String = driver.evaluate("document.title").await?;
    ensure!(title == "Onday fixture", "document.title gave {title:?}");
    let awaited: Option<String> = driver
        .evaluate(
            "(async () => { await new Promise(r => setTimeout(r, 10)); return 'awaited'; })()",
        )
        .await?;
    ensure!(
        awaited.as_deref() == Some("awaited"),
        "async IIFE gave {awaited:?}"
    );
    let defined: String = driver
        .evaluate("Object.defineProperty(window, '__ondayProbe', { value: 1 });\n")
        .await?;
    ensure!(
        defined == "[object Window]",
        "unserializable results fall back to String(): {defined:?}"
    );
    let probe: u32 = driver.evaluate("window.__ondayProbe").await?;
    ensure!(probe == 1, "the statement did not run");
    let nothing: Option<String> = driver.evaluate("null").await?;
    ensure!(nothing.is_none(), "null should decode as None");

    // Clicks, auto-waiting and strictness.
    page.get_by_test_id("increment").click().await?;
    page.get_by_role("button", Some("Increment"))
        .click()
        .await?;
    expect(&page.get_by_test_id("count"))
        .to_have_text("2")
        .await?;
    match page.get_by_test_id("dup").click().await {
        Err(error) if error.downcast_ref::<StrictModeViolation>().is_some() => {}
        other => bail!("expected a strict-mode violation, got {other:?}"),
    }
    page.get_by_test_id("dup").first().click().await?;
    page.locator("#late-btn").click().await?;
    status_is(&page, "late").await?;
    page.locator("#fade").click().await?;
    status_is(&page, "faded-click").await?;
    page.locator("#slider").click().await?;
    status_is(&page, "slid").await?;
    page.locator(".reveal").click().await?;
    status_is(&page, "revealed").await?;
    page.get_by_text("Bottom button").click().await?;
    status_is(&page, "bottom").await?;

    // Forms.
    page.get_by_label("Email").fill("ada@example.com").await?;
    expect(&page.locator("#email"))
        .to_have_value("ada@example.com")
        .await?;
    page.get_by_placeholder("you@example.com")
        .fill("grace@example.com")
        .await?;
    expect(&page.locator("#email"))
        .to_have_value("grace@example.com")
        .await?;
    page.locator("#agree").check().await?;
    expect(&page.locator("#agree")).to_be_checked().await?;
    let chosen = page
        .get_by_label("Flavor")
        .select_option(&["Chocolate"])
        .await?;
    ensure!(chosen == vec!["choc".to_string()], "selected {chosen:?}");
    page.locator("#when").fill("2026-09-11").await?;
    expect(&page.locator("#when"))
        .to_have_value("2026-09-11")
        .await?;
    page.get_by_role("textbox", Some("Editor"))
        .fill("hello")
        .await?;
    expect(&page.locator("#editor"))
        .to_have_text("hello")
        .await?;
    page.locator("#email").press("Control+A").await?;
    page.keyboard().press("Backspace").await?;
    expect(&page.locator("#email")).to_have_value("").await?;

    // Snapshot refs.
    let snapshot = page.snapshot().await?;
    ensure!(
        snapshot.contains("heading \"Fixture heading\" [level=1]"),
        "snapshot:\n{snapshot}"
    );
    let increment = ref_of(&snapshot, "button \"Increment\"")?;
    page.get_by_ref(&increment).click().await?;
    expect(&page.get_by_test_id("count"))
        .to_have_text("3")
        .await?;

    // Dialogs.
    page.locator("#alert-btn").click().await?;
    let dialog = onday::poll_until_ok(
        || async { page.dialog().await },
        Duration::from_secs(5),
        onday::Backoff::FAST,
    )
    .await
    .map_err(|last| anyhow::anyhow!("no dialog opened: {last:?}"))?;
    ensure!(
        dialog.message == "Proceed?",
        "dialog said {:?}",
        dialog.message
    );
    page.handle_dialog(true, None).await?;
    status_is(&page, "confirmed").await?;

    // Routing, network and console.
    if bidi {
        page.route("**/api/data", Arc::new(mock_data)).await?;
        page.locator("#fetch-btn").click().await?;
        status_is(&page, "data:mocked").await?;
        page.unroute("**/api/data").await?;
    } else {
        let refused = page.route("**/api/data", Arc::new(mock_data)).await;
        ensure!(refused.is_err(), "Classic sessions cannot intercept");
    }
    page.locator("#fetch-btn").click().await?;
    status_is(&page, "data:real").await?;
    page.evaluate::<bool>("(() => { console.warn('late warning'); return true; })()")
        .await?;
    let settled = onday::poll_until(
        || async {
            page.network_requests(0).await.is_ok_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.url.ends_with("/api/data") && entry.status == Some(200))
            })
        },
        Duration::from_secs(5),
        onday::Backoff::FAST,
        |_| {},
    )
    .await;
    ensure!(
        settled,
        "network history: {:?}",
        page.network_requests(0).await?
    );
    // Console events arrive asynchronously, so wait for the one just logged.
    let logged = onday::poll_until(
        || async {
            page.console_messages(0).await.is_ok_and(|messages| {
                messages
                    .iter()
                    .any(|message| message.text.contains("late warning"))
            })
        },
        Duration::from_secs(5),
        onday::Backoff::FAST,
        |_| {},
    )
    .await;
    let console = page.console_messages(0).await?;
    ensure!(logged, "console: {console:?}");
    if bidi {
        ensure!(
            console
                .iter()
                .any(|message| message.text.contains("fixture loaded")),
            "console: {console:?}"
        );
    }

    // Screenshots.
    let full = page
        .screenshot(ScreenshotOptions { full_page: bidi })
        .await?;
    ensure!(full.starts_with(b"\x89PNG"), "not a PNG");
    let element = page.locator("h1").screenshot().await?;
    ensure!(element.starts_with(b"\x89PNG"), "element shot is not a PNG");

    // Navigation with the ready hook, history.
    page.goto(&format!("{base}/second"), WaitUntil::Ready)
        .await?;
    expect(&page.locator("h1"))
        .to_have_text("Second page")
        .await?;
    page.go_back().await?;
    expect_page(&page).to_have_url(&format!("{base}/")).await?;

    // Isolation.
    context
        .add_cookies(vec![{
            let mut cookie = onday::thirtyfour::Cookie::new("flavor", "choc");
            cookie.domain = Some("127.0.0.1".to_string());
            cookie.path = Some("/".to_string());
            cookie
        }])
        .await?;
    ensure!(
        context
            .cookies()
            .await?
            .iter()
            .any(|cookie| cookie.name == "flavor"),
        "cookie missing"
    );
    let other = browser.new_context(ContextOptions::default()).await?;
    let other_page = other.page().await?;
    other_page.goto(base, WaitUntil::Load).await?;
    ensure!(
        !other
            .cookies()
            .await?
            .iter()
            .any(|cookie| cookie.name == "flavor"),
        "cookie leaked across contexts"
    );
    other.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome; run with --ignored"]
async fn chromium_over_bidi() -> Result<()> {
    scenario(Engine::Chromium, ProtocolPreference::Auto).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome; run with --ignored"]
async fn chromium_over_classic() -> Result<()> {
    scenario(Engine::Chromium, ProtocolPreference::Classic).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Firefox; run with --ignored"]
async fn firefox_over_bidi() -> Result<()> {
    scenario(Engine::Firefox, ProtocolPreference::Auto).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Firefox; run with --ignored"]
async fn firefox_over_classic() -> Result<()> {
    scenario(Engine::Firefox, ProtocolPreference::Classic).await
}
