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
    AppHooks, BrowserGone, DialogPolicy, Fulfill, Health, Protocol, ProtocolPreference,
    RouteAction, ScreenshotOptions, StrictModeViolation,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const INDEX: &str = include_str!("fixture/index.html");
const SECOND: &str = include_str!("fixture/second.html");
const FRAME: &str = include_str!("fixture/frame.html");
const SHOT: &str = include_str!("fixture/shot.html");
const TALL: &str = include_str!("fixture/tall.html");
const FAR: &str = include_str!("fixture/far.html");
const COVER: &str = include_str!("fixture/cover.html");
const DIALOG: &str = include_str!("fixture/dialog.html");
const RESTART: &str = include_str!("fixture/restart.html");
const REVEAL: &str = include_str!("fixture/reveal.html");
const RESTART_FRAME: &str = include_str!("fixture/restart-frame.html");

/// The fixture served from `127.0.0.1`, embedding a frame served from `localhost`: a
/// different site, so the browser treats that frame as cross-origin.
async fn serve() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the fixture server")?;
    let remote = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the cross-origin fixture server")?;
    let remote_origin = format!(
        "http://localhost:{}",
        remote.local_addr().context("cross-origin address")?.port()
    );
    let address = listener.local_addr().context("fixture address")?;
    let index = Arc::new(INDEX.replace("__CROSS_ORIGIN__", &remote_origin));
    let remote_origin = Arc::new(remote_origin);
    tokio::spawn(accept(listener, index.clone(), remote_origin.clone()));
    tokio::spawn(accept(remote, index, remote_origin));
    Ok(format!("http://{address}"))
}

async fn accept(listener: tokio::net::TcpListener, index: Arc<String>, remote_origin: Arc<String>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let index = index.clone();
        let remote_origin = remote_origin.clone();
        tokio::spawn(async move {
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => request.extend(buffer.iter().take(read)),
                }
            }
            let head = String::from_utf8_lossy(&request);
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let location = match path.as_str() {
                "/redirect" => Some("/second".to_string()),
                "/redirect-remote" => Some(format!("{remote_origin}/second")),
                _ => None,
            };
            if let Some(location) = location {
                let response = format!(
                    "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                );
                if let Err(error) = stream.write_all(response.as_bytes()).await {
                    eprintln!("fixture write failed: {error}");
                }
                return;
            }
            // The restarting frame carries how many restarts are left in its query.
            let page = path.split('?').next().unwrap_or("/");
            let (status, kind, body) = match page {
                "/" => ("200 OK", "text/html; charset=utf-8", index.to_string()),
                "/restart" => ("200 OK", "text/html; charset=utf-8", RESTART.to_string()),
                "/reveal" => ("200 OK", "text/html; charset=utf-8", REVEAL.to_string()),
                "/restart-frame" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    RESTART_FRAME.to_string(),
                ),
                "/second" => ("200 OK", "text/html; charset=utf-8", SECOND.to_string()),
                "/frame" => ("200 OK", "text/html; charset=utf-8", FRAME.to_string()),
                "/shot" => ("200 OK", "text/html; charset=utf-8", SHOT.to_string()),
                "/tall" => ("200 OK", "text/html; charset=utf-8", TALL.to_string()),
                "/far" => ("200 OK", "text/html; charset=utf-8", FAR.to_string()),
                "/cover" => ("200 OK", "text/html; charset=utf-8", COVER.to_string()),
                "/dialog" => ("200 OK", "text/html; charset=utf-8", DIALOG.to_string()),
                "/script-redirect" => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    "<!doctype html><title>Leaving</title><script>location.replace('/second')</script>"
                        .to_string(),
                ),
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
    let (_, after) = line.split_once("[ref=").context("line has no ref")?;
    let (reference, _) = after.split_once(']').context("unterminated ref")?;
    Ok(reference.to_string())
}

/// The ref of the first line containing `needle` below the line containing `anchor`.
fn ref_after(snapshot: &str, anchor: &str, needle: &str) -> Result<String> {
    let (_, below) = snapshot
        .split_once(anchor)
        .with_context(|| format!("no snapshot line contains {anchor}:\n{snapshot}"))?;
    ref_of(below, needle)
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

fn install_tracing() {
    let installed = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    if let Err(error) = installed {
        eprintln!("tracing already installed: {error}");
    }
}

async fn scenario(engine: Engine, preference: ProtocolPreference) -> Result<()> {
    install_tracing();
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
    redirects(&page, base).await?;
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

    // Over BiDi the runtime lives in its own sandbox, out of reach of whatever the
    // page does to its globals. Classic has no sandbox, so this holds for BiDi only.
    if bidi {
        page.evaluate::<Option<String>>(
            "(() => { setTimeout(() => { JSON.stringify = () => { throw new Error('page broke JSON'); }; \
             Array.isArray = () => false; document.title = 'sabotaged'; }, 0); return null; })()",
        )
        .await?;
        expect_page(&page).to_have_title("sabotaged").await?;
        page.get_by_test_id("increment").click().await?;
        expect(&page.get_by_test_id("count"))
            .to_have_text("1")
            .await?;
        page.goto(base, WaitUntil::Load).await?;
        expect_page(&page).to_have_title("Onday fixture").await?;
    }

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
    page.locator("#email").press("ControlOrMeta+A").await?;
    page.keyboard().press("Backspace").await?;
    expect(&page.locator("#email")).to_have_value("").await?;
    // A wait takes its budget from the locator, or from the page when the
    // locator names none.
    page.locator("#email")
        .wait_for(onday::ElementState::Visible)
        .await?;
    let started = std::time::Instant::now();
    let absent = page
        .locator("#never-rendered")
        .with_timeout(Duration::from_millis(300))
        .first()
        .wait_for(onday::ElementState::Attached)
        .await
        .err()
        .context("waiting for an element that never renders should fail")?;
    ensure!(
        started.elapsed() < Duration::from_secs(3),
        "a 300 ms wait took {:?}: {absent:#}",
        started.elapsed()
    );
    // A stroke presses once, moves while held and releases once.
    let pad = page.locator("#pad").bounding_box().await?;
    let (across, down) = (pad.left.saturating_add(20), pad.top.saturating_add(20));
    page.mouse()
        .stroke(&[
            (across, down),
            (across.saturating_add(60), down.saturating_add(20)),
            (across.saturating_add(120), down.saturating_add(10)),
        ])
        .await?;
    let stroke: Vec<String> = page.evaluate("window.__stroke ?? []").await?;
    ensure!(
        stroke.first().map(String::as_str) == Some("down")
            && stroke.last().map(String::as_str) == Some("up")
            && stroke.iter().filter(|event| *event == "move").count() >= 2,
        "the pad saw {stroke:?} for a three-point stroke"
    );
    // A page reaches its sibling tabs through its own context.
    let sibling = page.context().new_page().await?;
    let open = page.context().pages().await?;
    ensure!(
        open.len() == 2 && open.contains(&page) && open.contains(&sibling),
        "a tab opened from the page's context is not listed beside it"
    );
    // The raw driver follows whichever tab asks for it.
    let on_sibling = sibling.driver_on_tab().await?.current_url().await?;
    ensure!(
        on_sibling.as_str() == "about:blank",
        "the driver asked for the new tab is on {on_sibling}"
    );
    let on_page = page.driver_on_tab().await?.current_url().await?;
    ensure!(
        on_page.as_str() == page.url().await?,
        "the driver asked for the first tab is on {on_page}"
    );
    sibling.close().await?;
    // Every match gets a locator of its own.
    let duplicates = page.get_by_test_id("dup").all().await?;
    ensure!(
        duplicates.len() == 2,
        "two buttons share the testid, {} locators came back",
        duplicates.len()
    );
    for duplicate in &duplicates {
        expect(duplicate).to_have_text("Duplicate").await?;
    }
    // The wheel moves the scroller under the pointer and leaves the page be.
    page.locator("#scroller").scroll_into_view().await?;
    let scroller = page.locator("#scroller").bounding_box().await?;
    let page_scroll: i64 = page.evaluate("Math.round(window.scrollY)").await?;
    page.mouse()
        .wheel_at(
            scroller.left.saturating_add(scroller.width / 2),
            scroller.top.saturating_add(scroller.height / 2),
            0,
            120,
        )
        .await?;
    page.wait_for_function(
        "document.getElementById('scroller').scrollTop > 0",
        Duration::from_secs(5),
    )
    .await
    .context("the wheel over the scroller did not scroll it")?;
    let page_scroll_after: i64 = page.evaluate("Math.round(window.scrollY)").await?;
    ensure!(
        page_scroll_after == page_scroll,
        "the wheel over the scroller moved the page from {page_scroll} to {page_scroll_after}"
    );
    typing(&page, Typist::Driver).await?;
    typing(&page, Typist::Locator).await?;

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

    // Same-origin frames: snapshot refs and selectors reach inside, and only on request.
    let canvas = page.locator(r#"iframe[title="Canvas"]"#);
    let frame_status = canvas.locator("#frame-status");
    expect(&frame_status).to_have_text("frame idle").await?;
    let snapshot = page.snapshot().await?;
    let action = ref_of(&snapshot, "button \"Frame action\"")?;
    ensure!(
        action.starts_with('f'),
        "frame ref {action} lacks its frame prefix:\n{snapshot}"
    );
    page.get_by_ref(&action).click().await?;
    expect(&frame_status).to_have_text("component 1").await?;
    canvas.get_by_test_id("component-link").click().await?;
    expect(&frame_status).to_have_text("component 2").await?;
    canvas.get_by_label("Frame input").fill("inside").await?;
    expect(&canvas.locator("#frame-input"))
        .to_have_value("inside")
        .await?;
    page.get_by_test_id("component-link").click().await?;
    status_is(&page, "parent-component").await?;
    expect(&frame_status).to_have_text("component 2").await?;
    match page
        .locator("iframe >> testid=component-link")
        .click()
        .await
    {
        Err(error) if error.downcast_ref::<StrictModeViolation>().is_some() => {}
        other => bail!("entering two frames should be a strict-mode violation, got {other:?}"),
    }
    let framed = ref_of(&snapshot, "iframe \"Canvas\"")?;
    let inner = page.get_by_ref(&framed).snapshot().await?;
    ensure!(
        inner.contains("button \"Frame action\""),
        "iframe subtree snapshot:\n{inner}"
    );
    // Cross-origin frames work the same way.
    let remote = page.locator(r#"iframe[title="Remote"]"#);
    let remote_status = remote.locator("#frame-status");
    expect(&remote_status).to_have_text("frame idle").await?;
    let snapshot = page.snapshot().await?;
    let remote_action = ref_after(&snapshot, "iframe \"Remote\"", "button \"Frame action\"")?;
    page.get_by_ref(&remote_action).click().await?;
    expect(&remote_status).to_have_text("component 1").await?;
    remote.get_by_test_id("component-link").click().await?;
    expect(&remote_status).to_have_text("component 2").await?;
    remote.get_by_label("Frame input").fill("across").await?;
    expect(&remote.locator("#frame-input"))
        .to_have_value("across")
        .await?;
    let remote_shot = remote.get_by_test_id("component-link").screenshot().await?;
    ensure!(
        remote_shot.starts_with(b"\x89PNG"),
        "cross-origin element shot is not a PNG"
    );
    expect(&frame_status).to_have_text("component 2").await?;

    let shot = canvas.get_by_test_id("component-link").screenshot().await?;
    ensure!(
        shot.starts_with(b"\x89PNG"),
        "framed element shot is not a PNG"
    );

    // Drag and drop within the page and into frames of either origin.
    let source = page.get_by_test_id("drag-source");
    source.drag_to(&page.locator("#drop-zone")).await?;
    expect(&page.locator("#drop-zone"))
        .to_have_text("dropped payload")
        .await?;
    source.drag_to(&canvas.locator("#frame-drop")).await?;
    expect(&frame_status)
        .to_have_text("dropped payload")
        .await?;
    source.drag_to(&remote.locator("#frame-drop")).await?;
    expect(&remote_status)
        .to_have_text("dropped payload")
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
    // Through a wrapper, as a dev server's console forwarding does: the first
    // frame is the wrapper and the caller is further down the stack.
    page.evaluate::<bool>(
        "(() => { const forward = (...args) => console.warn(...args); \
         function report() { forward('late warning'); } report(); return true; })()",
    )
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
        let late = console
            .iter()
            .find(|message| message.text.contains("late warning"))
            .context("the late warning is in the console")?;
        // Firefox sends only the top-most frame for a console warning
        // (Bugzilla 1944136); Chromium sends the whole stack.
        let frames = if browser.engine() == Engine::Firefox {
            1
        } else {
            3
        };
        ensure!(
            late.stack.len() >= frames && late.location.as_ref() == late.stack.first(),
            "the warning keeps every frame the browser sends, the wrapper first: {late:?}"
        );
    }

    // Screenshots.
    let full = page
        .screenshot(ScreenshotOptions { full_page: bidi })
        .await?;
    ensure!(full.starts_with(b"\x89PNG"), "not a PNG");
    let element = page.locator("h1").screenshot().await?;
    ensure!(element.starts_with(b"\x89PNG"), "element shot is not a PNG");
    element_shot_is_the_element(&page, base).await?;
    drop_onto_a_target_taller_than_its_box(&page, base).await?;
    drag_onto_a_target_the_page_scrolls_to(&page, base).await?;
    click_retried_when_its_press_lands_on_a_cover(&page, base).await?;
    click_into_a_frame_that_restarts_its_document(&page, base).await?;
    click_an_item_of_a_menu_scrolling_closes(&page, base).await?;
    type_into_an_input_low_in_a_fixed_dialog(&page, base).await?;
    if bidi {
        inspect_runs_under_pristine_globals(&page, base).await?;
    }

    // Navigation with the ready hook, history. The page asks before it is left,
    // which the driver answers itself, so no dialog may stay on record after.
    page.evaluate::<bool>(
        "(() => { addEventListener('beforeunload', (event) => { event.preventDefault(); event.returnValue = ''; }); return true; })()",
    )
    .await?;
    page.goto(&format!("{base}/second"), WaitUntil::Ready)
        .await?;
    expect(&page.locator("h1"))
        .to_have_text("Second page")
        .await?;
    ensure!(
        page.dialog().await?.is_none(),
        "the left page's beforeunload prompt is still on record"
    );
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

/// `goto` settles on wherever a redirect lands, not on the URL it was given.
async fn redirects(page: &Page, base: &str) -> Result<()> {
    for path in ["/redirect", "/redirect-remote", "/script-redirect"] {
        let started = std::time::Instant::now();
        step(
            &format!("following {path}"),
            page.goto(&format!("{base}{path}"), WaitUntil::Load),
        )
        .await?;
        let landed = page.url().await?;
        tracing::info!("{path} landed on {landed} after {:?}", started.elapsed());
        ensure!(landed.ends_with("/second"), "{path} landed on {landed}");
    }
    step(
        "following /script-redirect to DOMContentLoaded",
        page.goto(
            &format!("{base}/script-redirect"),
            WaitUntil::DomContentLoaded,
        ),
    )
    .await?;
    let landed = page.url().await?;
    ensure!(
        landed.ends_with("/second"),
        "/script-redirect to DOMContentLoaded landed on {landed}"
    );
    // A same-document navigation has no load event of its own.
    step(
        "following a fragment",
        page.goto(&format!("{base}/second#below"), WaitUntil::Load),
    )
    .await?;
    let landed = page.url().await?;
    ensure!(
        landed.ends_with("#below"),
        "the fragment landed on {landed}"
    );
    // A navigation that never commits fails at once instead of waiting out the timeout,
    // both refused by the server and blocked by the browser, which Chromium over
    // Classic reports as done while showing its error page.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind a port to release")?
        .local_addr()
        .context("released port address")?;
    for unreachable in [
        format!("http://{closed}/"),
        "http://127.0.0.1:1/".to_string(),
    ] {
        let started = std::time::Instant::now();
        let navigated = page.goto(&unreachable, WaitUntil::Load).await;
        ensure!(navigated.is_err(), "navigating to {unreachable} succeeded");
        ensure!(
            started.elapsed() < Duration::from_secs(5),
            "navigating to {unreachable} took {:?} to fail",
            started.elapsed()
        );
    }
    Ok(())
}

/// `WebDriverExt::type_into_selector` acts like a person and checks what the field holds.
/// The two ways to type into a field.
#[derive(Debug, Clone, Copy)]
enum Typist {
    /// The raw session's `type_into_selector`.
    Driver,
    /// A locator's `type_into`.
    Locator,
}

impl Typist {
    async fn type_into(self, page: &Page, selector: &str, text: &str) -> Result<()> {
        match self {
            Typist::Driver => {
                page.driver()
                    .type_into_selector(selector, text, Duration::from_secs(10))
                    .await
            }
            Typist::Locator => page.locator(selector).type_into(text).await,
        }
    }
}

async fn typing(page: &Page, typist: Typist) -> Result<()> {
    // A label covering the input forwards the pointer click's focus to it.
    typist.type_into(page, "#float", "new").await?;
    expect(&page.locator("#float")).to_have_value("new").await?;
    typist.type_into(page, "#wrapped", "Grace").await?;
    expect(&page.locator("#wrapped"))
        .to_have_value("Grace")
        .await?;
    // A mask reformats both the prefill and the typed digits.
    typist.type_into(page, "#phone", "5551234567").await?;
    expect(&page.locator("#phone"))
        .to_have_value("(555) 123-4567")
        .await?;
    // One input event for the clear, one per typed key.
    let inputs_before: u32 = page.evaluate("window.__searchInputs || 0").await?;
    typist.type_into(page, "#search", "ab").await?;
    expect(&page.locator("#search")).to_have_value("ab").await?;
    let inputs: u32 = page.evaluate("window.__searchInputs").await?;
    let typed = inputs.saturating_sub(inputs_before);
    ensure!(typed == 3, "#search saw {typed} input events, expected 3");
    typist.type_into(page, "#qty", "40").await?;
    expect(&page.locator("#qty")).to_have_value("40").await?;
    typist.type_into(page, "#notes", "only line").await?;
    expect(&page.locator("#notes"))
        .to_have_value("only line")
        .await?;
    typist.type_into(page, "#editor", "typed").await?;
    expect(&page.locator("#editor"))
        .to_have_text("typed")
        .await?;
    let covered = typist
        .type_into(page, "#covered", "x")
        .await
        .err()
        .context("typing into a field under a toast should fail")?;
    ensure!(
        format!("{covered:#}").contains(r#"id="toast""#),
        "the focus error should name the toast: {covered:#}"
    );
    // A box that clips its overflow, which no scrollbar reaches, hides part of
    // the field and leaves what lies beneath the box under its centre. The
    // locator's hit check refuses that point and scrolls the field clear.
    if let Typist::Locator = typist {
        typist.type_into(page, "#clipped", "seen").await?;
        expect(&page.locator("#clipped"))
            .to_have_value("seen")
            .await?;
    }
    let dated = typist
        .type_into(page, "#when", "10012026")
        .await
        .err()
        .context("typing into a date input should fail")?;
    ensure!(
        format!("{dated:#}").contains("text typing does not apply"),
        "the date refusal should say typing does not apply: {dated:#}"
    );
    Ok(())
}

/// Run one step of a test, failing with its name instead of hanging.
async fn step<T>(what: &str, future: impl Future<Output = Result<T>>) -> Result<T> {
    let started = std::time::Instant::now();
    tracing::info!("step: {what}");
    let outcome = tokio::time::timeout(Duration::from_secs(30), future)
        .await
        .with_context(|| format!("{what} hung"))?
        .with_context(|| format!("{what} failed"));
    match &outcome {
        Ok(_) => tracing::info!("step: {what} done in {:?}", started.elapsed()),
        Err(error) => tracing::error!("step: {error:#} after {:?}", started.elapsed()),
    }
    outcome
}

/// One plain HTTP GET, returning the body once `Content-Length` bytes arrived.
async fn http_get(address: &str, path: &str) -> Result<String> {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .with_context(|| format!("connect to {address}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .with_context(|| format!("send GET {path}"))?;
    let mut response = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .with_context(|| format!("read GET {path}"))?;
        response.extend(buffer.iter().take(read));
        let text = String::from_utf8_lossy(&response);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            });
            if read == 0 || length.is_some_and(|length| body.len() >= length) {
                return Ok(body.to_string());
            }
        } else if read == 0 {
            bail!("GET {path} closed before its headers: {text:?}");
        }
    }
}

#[derive(serde::Deserialize)]
struct DevToolsTarget {
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

/// What disappears behind onday's back.
#[derive(Debug, Clone, Copy)]
enum Vanish {
    /// The tabs, as when a person closes the window.
    Tabs,
    /// Every DevTools target, the driver's own BiDi tab included, as in a crash.
    Everything,
}

/// Close DevTools targets through the browser's own debugging endpoint.
async fn close_targets(browser: &Browser, vanish: Vanish) -> Result<()> {
    let capabilities = browser.driver().handle().capabilities();
    let address = capabilities
        .get("goog:chromeOptions")
        .and_then(|options| options.get("debuggerAddress"))
        .and_then(|address| address.as_str())
        .context("chromedriver reported no debuggerAddress")?
        .to_string();
    let listing = http_get(&address, "/json/list").await?;
    let targets: Vec<DevToolsTarget> =
        serde_json::from_str(&listing).with_context(|| format!("parse /json/list: {listing}"))?;
    for target in &targets {
        let chosen = match vanish {
            Vanish::Tabs => target.kind == "page",
            Vanish::Everything => true,
        };
        if !chosen {
            continue;
        }
        match (
            http_get(&address, &format!("/json/close/{}", target.id)).await,
            vanish,
        ) {
            (Ok(answer), _) => {
                tracing::info!("closed {} {}: {}", target.kind, target.id, answer.trim())
            }
            // Closing the browser's own UI takes the whole process down with it.
            (Err(error), Vanish::Everything) => {
                tracing::info!(
                    "the browser exited while closing {}: {error:#}",
                    target.kind
                );
                return Ok(());
            }
            (Err(error), Vanish::Tabs) => return Err(error),
        }
    }
    Ok(())
}

async fn vanishing(vanish: Vanish) -> Result<()> {
    install_tracing();
    let base = serve().await?;
    let browser = step(
        "launching chromium",
        Browser::launch(launch_options(Engine::Chromium, ProtocolPreference::Auto)),
    )
    .await?;
    let page = step("opening a page", async {
        browser
            .default_context(ContextOptions::default())
            .await?
            .page()
            .await
    })
    .await?;
    step("the first navigation", page.goto(&base, WaitUntil::Load)).await?;
    step(
        &format!("closing {vanish:?}"),
        close_targets(&browser, vanish),
    )
    .await?;
    let started = std::time::Instant::now();
    let navigated = tokio::time::timeout(
        Duration::from_secs(20),
        page.goto(&format!("{base}/second"), WaitUntil::Load),
    )
    .await;
    let took = started.elapsed();
    tracing::info!("the navigation ended after {took:?}: {navigated:?}");
    let health = browser.health().await;
    tracing::info!("health after vanishing: {health}");
    let closed = step("closing the vanished browser", browser.close()).await;
    let navigated = navigated.context("navigating a vanished browser hung")?;
    match navigated {
        Err(error) if error.downcast_ref::<BrowserGone>().is_some() => {}
        other => bail!("expected BrowserGone, got {other:?}"),
    }
    ensure!(
        took < Duration::from_secs(10),
        "noticing the vanished browser took {took:?}"
    );
    ensure!(
        matches!(health, Health::Gone(_)),
        "health after vanishing: {health}"
    );
    closed
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome; run with --ignored"]
async fn chromium_reports_closed_tabs() -> Result<()> {
    vanishing(Vanish::Tabs).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome; run with --ignored"]
async fn chromium_reports_a_vanished_browser() -> Result<()> {
    vanishing(Vanish::Everything).await
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

/// A drop zone taller than the box that scrolls it settles once scrolled into
/// view: aligning one edge must not push the other out and scroll it back, or
/// the target never holds still and the drag times out.
async fn drop_onto_a_target_taller_than_its_box(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/tall"), WaitUntil::Ready).await?;
    page.locator("#tall-source")
        .drag_to(
            &page
                .locator("#tall-drop")
                .with_timeout(Duration::from_secs(10)),
        )
        .await
        .context("drag onto a drop zone taller than its scroll box")?;
    expect(&page.locator("#tall-status"))
        .to_have_text("dropped")
        .await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// A drop zone far down the page, reached by scrolling it: the press lands on the
/// source, not on whatever the scroll brings under the point the source held.
async fn drag_onto_a_target_the_page_scrolls_to(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/far"), WaitUntil::Ready).await?;
    page.locator("#far-source")
        .drag_to(&page.locator("#far-drop"))
        .await
        .context("drag onto a drop zone the page scrolls to")?;
    expect(&page.locator("#far-status"))
        .to_have_text("dropped")
        .await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// A click whose press is taken by something the page put over the target is
/// blocked and retried once the target is clear, as Playwright's hit-target
/// guard does: the target is clicked once and the cover never.
async fn click_retried_when_its_press_lands_on_a_cover(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/cover"), WaitUntil::Ready)
        .await?;
    page.locator("#cover-target")
        .click()
        .await
        .context("click a target the page covers on the first press")?;
    expect(&page.locator("#cover-status"))
        .to_have_text("clicked 1")
        .await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// An item of a menu that closes when the page scrolls takes a click while it
/// is in view: a target already visible is not scrolled first, as Playwright
/// does on Chromium, which would close the menu out from under the click.
async fn click_an_item_of_a_menu_scrolling_closes(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/reveal"), WaitUntil::Ready)
        .await?;
    page.locator("#reveal-action")
        .click()
        .await
        .context("click an item of a menu any scroll closes")?;
    expect(&page.locator("#reveal-status"))
        .to_have_text("clicked 1")
        .await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// A click into a frame that replaces its document while the click resolves
/// into it, as a payment field's frame does while it starts, waits the
/// restarts out rather than failing on the document that went away.
async fn click_into_a_frame_that_restarts_its_document(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/restart"), WaitUntil::Ready)
        .await?;
    let input = page.locator("iframe#restart >> #restart-input");
    input
        .click()
        .await
        .context("click into a frame that keeps restarting its document")?;
    expect(&page.locator("iframe#restart >> #restart-status"))
        .to_have_text("settled")
        .await?;
    input
        .type_into("4242")
        .await
        .context("type into the settled frame")?;
    expect(&input).to_have_value("4242").await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// An input far down a fixed dialog's own scroll, under its floating label,
/// takes typing: the dialog is the box it scrolls in, not a bar covering it, so
/// scrolling it into view must not push it back out.
async fn type_into_an_input_low_in_a_fixed_dialog(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/dialog"), WaitUntil::Ready)
        .await?;
    page.locator("#dialog-input")
        .with_timeout(Duration::from_secs(10))
        .type_into("123-45-6789")
        .await
        .context("type into an input low in a fixed dialog")?;
    expect(&page.locator("#dialog-input"))
        .to_have_value("123-45-6789")
        .await?;
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// An element far down a fixed dialog's own scroll, with the document behind it
/// scrolled too, is captured from where it is drawn: its middle is its own color,
/// not the dialog's or the page's.
async fn element_shot_is_the_element(page: &Page, base: &str) -> Result<()> {
    page.goto(&format!("{base}/shot"), WaitUntil::Ready).await?;
    page.evaluate::<bool>("(() => { scrollTo(0, 500); return true; })()")
        .await?;
    let shot = page.locator("#shot-target").screenshot().await?;
    let mut reader = png::Decoder::new(std::io::Cursor::new(shot))
        .read_info()
        .context("read the element shot")?;
    let mut pixels = vec![
        0;
        reader
            .output_buffer_size()
            .context("size the element shot")?
    ];
    let info = reader
        .next_frame(&mut pixels)
        .context("decode the element shot")?;
    let row = usize::try_from(info.height / 2).context("the middle row")?;
    let column = usize::try_from(info.width / 2).context("the middle column")?;
    let start = row
        .checked_mul(info.line_size)
        .and_then(|offset| offset.checked_add(column.checked_mul(info.color_type.samples())?))
        .context("locate the middle pixel")?;
    let middle = pixels
        .get(start..start.saturating_add(3))
        .context("the middle pixel lies outside the shot")?;
    ensure!(
        middle == [0, 128, 255],
        "the element shot's middle is {middle:?}, not the element's rgb(0, 128, 255)"
    );
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}

/// A page that patches a builtin, as Svelte's dev build does to `indexOf`,
/// leaves `inspect` with the original while it still reads the same document.
async fn inspect_runs_under_pristine_globals(page: &Page, base: &str) -> Result<()> {
    page.goto(base, WaitUntil::Load).await?;
    page.evaluate::<bool>(
        "(() => { Array.prototype.indexOf = () => { throw new Error('page patched indexOf'); }; \
         window.__pageOnly = 1; return true; })()",
    )
    .await?;
    let patched: bool = page
        .evaluate("(() => { try { [1].indexOf(1); return false; } catch { return true; } })()")
        .await?;
    ensure!(patched, "the page's own world should see its patch");
    let index: i64 = page.inspect("[1, 2].indexOf(2)").await?;
    ensure!(index == 1, "inspect saw the page's indexOf: {index}");
    let title: String = page.inspect("document.title").await?;
    ensure!(
        title == "Onday fixture",
        "inspect read the title as {title:?}"
    );
    let leaked: bool = page.inspect("'__pageOnly' in window").await?;
    ensure!(!leaked, "inspect sees a global the page defined");
    page.goto(base, WaitUntil::Load).await?;
    Ok(())
}
