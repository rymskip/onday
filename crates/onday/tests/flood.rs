//! A page that issues over a thousand requests at once, as a dev server's
//! unbundled module graph does. The session has to keep answering while their
//! events arrive, and has to end up with every one of them.
//!
//! Needs real browsers, so the tests are ignored by default:
//! `cargo test -p onday --test flood -- --ignored`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use onday::Protocol;
use onday::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Modules the page imports. With the page and its entry script, two more requests.
const MODULES: usize = 1200;

/// How long a command may take while the request events arrive. Left to itself
/// chromedriver's BiDi tab holds one up for about twenty seconds here.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);

const PAGE: &str =
    "<!doctype html><title>Flood</title><script type=\"module\" src=\"/entry.js\"></script>";

async fn serve() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the flood server")?;
    let address = listener.local_addr().context("flood address")?;
    let mut entry: String = (0..MODULES)
        .map(|index| format!("import \"/m/{index}.js\";\n"))
        .collect();
    entry.push_str("window.__loaded = true;\n");
    tokio::spawn(accept(listener, Arc::new(entry)));
    Ok(format!("http://{address}"))
}

async fn accept(listener: tokio::net::TcpListener, entry: Arc<String>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let entry = entry.clone();
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
            let (kind, body) = match head.split_whitespace().nth(1) {
                Some("/") => ("text/html; charset=utf-8", PAGE),
                Some("/entry.js") => ("text/javascript", entry.as_str()),
                _ => ("text/javascript", "export const loaded = true;\n"),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            if let Err(error) = stream.write_all(response.as_bytes()).await {
                eprintln!("flood write failed: {error}");
            }
        });
    }
}

fn launch_options(engine: Engine) -> LaunchOptions {
    let options = LaunchOptions::new(engine).no_sandbox();
    let variable = match engine {
        Engine::Chromium => "ONDAY_CHROMIUM_EXECUTABLE",
        Engine::Firefox => "ONDAY_FIREFOX_EXECUTABLE",
        Engine::Webkit => "ONDAY_WEBKIT_EXECUTABLE",
    };
    match std::env::var(variable) {
        Ok(path) => options.executable(path),
        Err(_) => options,
    }
}

async fn flood(page: &Page, base: &str) -> Result<()> {
    page.goto(base, WaitUntil::Commit).await?;
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    loop {
        let asked = Instant::now();
        let loaded = tokio::time::timeout(
            ANSWER_WITHIN,
            page.evaluate::<bool>("window.__loaded === true"),
        )
        .await
        .with_context(|| {
            format!(
                "a command asked {:?} into the flood had no answer in {ANSWER_WITHIN:?}",
                asked.duration_since(started)
            )
        })??;
        slowest = slowest.max(asked.elapsed());
        if loaded {
            break;
        }
        ensure!(
            started.elapsed() < Duration::from_secs(60),
            "the page never finished loading its modules"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eprintln!("the slowest command during the flood took {slowest:?}");

    let expected = MODULES + 2;
    let settling = Instant::now();
    loop {
        let entries = page.network_requests(0).await?;
        let settled = entries.iter().filter(|entry| entry.is_settled()).count();
        if entries.len() == expected && settled == expected {
            eprintln!(
                "all {expected} requests settled {:?} into the flood",
                started.elapsed()
            );
            return Ok(());
        }
        ensure!(
            settling.elapsed() < Duration::from_secs(15),
            "{} of {expected} requests recorded, {settled} of them settled",
            entries.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn scenario(engine: Engine) -> Result<()> {
    let base = serve().await?;
    let browser = Browser::launch(launch_options(engine)).await?;
    ensure!(
        browser.protocol() == Protocol::Bidi,
        "negotiated {} instead of BiDi",
        browser.protocol()
    );
    let context = browser.default_context(ContextOptions::default()).await?;
    let page = context.page().await?;
    let result = flood(&page, base.as_str()).await;
    let closed = browser.close().await;
    result?;
    closed
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Chrome; run with --ignored"]
async fn chromium_answers_through_a_request_flood() -> Result<()> {
    scenario(Engine::Chromium).await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Firefox; run with --ignored"]
async fn firefox_answers_through_a_request_flood() -> Result<()> {
    scenario(Engine::Firefox).await
}
