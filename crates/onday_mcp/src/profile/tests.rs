use std::fs::OpenOptions;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use onday::{Browser, ContextOptions, Engine, LaunchOptions, WaitUntil};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::ProfileSource;

/// A fresh scratch directory for one test, holding an earlier session `before`.
fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("onday-profile-{name}-{}", std::process::id()));
    if base.exists() {
        std::fs::remove_dir_all(&base).expect("clear the last run");
    }
    std::fs::create_dir_all(base.join("before")).expect("create the earlier session");
    std::fs::write(base.join("before/lock"), "").expect("create its lock file");
    base
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create the parent");
    std::fs::write(path, contents).expect("write");
}

/// A Chromium profile with an extension's storage, cookies, a nested link and the
/// entries a running browser claims the profile with.
fn chromium_profile(base: &Path) -> PathBuf {
    let profile = base.join("before/profile-chromium");
    write(&profile.join("Local State"), "local state");
    write(&profile.join("Default/Cookies"), "cookies");
    write(
        &profile.join("Default/Local Extension Settings/abcdef/000003.log"),
        "extension storage",
    );
    write(
        &profile.join("Default/Extensions/abcdef/1.0_0/manifest.json"),
        "{}",
    );
    write(&profile.join("RunningChromeVersion"), "156");
    write(&profile.join("DevToolsActivePort"), "54321");
    symlink("host-1234", profile.join("SingletonLock")).expect("link SingletonLock");
    symlink("9876", profile.join("SingletonCookie")).expect("link SingletonCookie");
    symlink("Cookies", profile.join("Default/Cookies-link")).expect("link inside Default");
    profile
}

fn source(base: &Path) -> ProfileSource {
    ProfileSource::new("before".to_string(), base.join("before"))
}

#[test]
fn copies_the_whole_profile_but_its_claims() {
    let base = scratch("whole");
    chromium_profile(&base);
    let target = base.join("after/profile-chromium");
    std::fs::create_dir_all(base.join("after")).expect("create the new session");

    source(&base)
        .copy_to(Engine::Chromium, &target)
        .expect("copy the profile");

    let read = |relative: &str| std::fs::read_to_string(target.join(relative)).expect(relative);
    assert_eq!(read("Local State"), "local state");
    assert_eq!(read("Default/Cookies"), "cookies");
    assert_eq!(
        read("Default/Local Extension Settings/abcdef/000003.log"),
        "extension storage"
    );
    assert_eq!(read("Default/Extensions/abcdef/1.0_0/manifest.json"), "{}");
    assert_eq!(
        std::fs::read_link(target.join("Default/Cookies-link")).expect("a link stays a link"),
        Path::new("Cookies")
    );
    for claim in [
        "SingletonLock",
        "SingletonCookie",
        "RunningChromeVersion",
        "DevToolsActivePort",
    ] {
        assert!(
            std::fs::symlink_metadata(target.join(claim)).is_err(),
            "{claim} was copied"
        );
    }
    assert!(!base.join("after/profile-chromium.partial").exists());
    assert!(
        std::fs::symlink_metadata(base.join("before/profile-chromium/SingletonLock")).is_ok(),
        "the earlier session's profile changed"
    );
    std::fs::remove_dir_all(&base).expect("clean up");
}

#[test]
fn refuses_a_session_another_process_holds() {
    let base = scratch("held");
    chromium_profile(&base);
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(base.join("before/lock"))
        .expect("open the lock");
    held.try_lock().expect("hold the lock");
    let target = base.join("after/profile-chromium");

    let refusal = source(&base)
        .copy_to(Engine::Chromium, &target)
        .expect_err("a held session was copied");

    assert!(
        refusal.to_string().contains("open in another onday_mcp"),
        "{refusal:#}"
    );
    assert!(!target.exists());
    drop(held);
    std::fs::remove_dir_all(&base).expect("clean up");
}

#[test]
fn refuses_an_engine_the_session_has_no_profile_for() {
    let base = scratch("engine");
    chromium_profile(&base);
    let target = base.join("after/profile-firefox");

    let refusal = source(&base)
        .copy_to(Engine::Firefox, &target)
        .expect_err("copied a profile that does not exist");

    assert!(
        refusal.to_string().contains("has no firefox profile"),
        "{refusal:#}"
    );
    assert!(!target.exists());
    std::fs::remove_dir_all(&base).expect("clean up");
}

#[test]
fn refuses_a_session_that_does_not_exist() {
    let base = scratch("missing");
    let missing = ProfileSource::new("never".to_string(), base.join("never"));

    let refusal = missing
        .copy_to(Engine::Chromium, &base.join("after/profile-chromium"))
        .expect_err("copied from no session");

    assert!(
        refusal.to_string().contains("no session never"),
        "{refusal:#}"
    );
    std::fs::remove_dir_all(&base).expect("clean up");
}

#[test]
fn replaces_an_unfinished_copy() {
    let base = scratch("partial");
    chromium_profile(&base);
    write(
        &base.join("after/profile-chromium.partial/stale"),
        "left by an interrupted copy",
    );
    let target = base.join("after/profile-chromium");

    source(&base)
        .copy_to(Engine::Chromium, &target)
        .expect("copy over the unfinished one");

    assert!(!target.join("stale").exists());
    assert!(target.join("Default/Cookies").exists());
    assert!(!base.join("after/profile-chromium.partial").exists());
    std::fs::remove_dir_all(&base).expect("clean up");
}

/// Stores state the way a signed-in page or extension does: a persistent cookie,
/// local storage and an IndexedDB record.
const STORE: &str = r#"(async () => {
    document.cookie = 'session=kept; max-age=86400; path=/';
    localStorage.setItem('session', 'kept');
    await new Promise((resolve, reject) => {
        const open = indexedDB.open('profile', 1);
        open.onupgradeneeded = () => open.result.createObjectStore('state');
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
            const write = open.result.transaction('state', 'readwrite');
            write.objectStore('state').put('kept', 'session');
            write.oncomplete = () => { open.result.close(); resolve(); };
            write.onerror = () => reject(write.error);
        };
    });
    return true;
})()"#;

const READ: &str = r#"(async () => {
    const indexed = await new Promise((resolve, reject) => {
        const open = indexedDB.open('profile', 1);
        open.onerror = () => reject(open.error);
        open.onupgradeneeded = () => open.result.createObjectStore('state');
        open.onsuccess = () => {
            const read = open.result.transaction('state').objectStore('state').get('session');
            read.onsuccess = () => { open.result.close(); resolve(read.result ?? null); };
            read.onerror = () => reject(read.error);
        };
    });
    return { cookie: document.cookie, local: localStorage.getItem('session'), indexed };
})()"#;

#[derive(Deserialize)]
struct Stored {
    cookie: String,
    local: Option<String>,
    indexed: Option<String>,
}

/// One blank page for every path, on a fixed origin both browsers visit.
async fn serve() -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the page server")?;
    let address = listener.local_addr().context("page server address")?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut request: Vec<u8> = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => request.extend(buffer.iter().take(read)),
                    }
                }
                let body = "<!doctype html><title>profile</title>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                if let Err(error) = stream.write_all(response.as_bytes()).await {
                    eprintln!("page server write failed: {error}");
                }
            });
        }
    });
    Ok(format!("http://{address}/"))
}

fn launch_options(profile: &Path) -> LaunchOptions {
    let mut options = LaunchOptions::new(Engine::Chromium)
        .headless(true)
        .no_sandbox()
        .user_data_dir(profile);
    if let Ok(path) = std::env::var("ONDAY_CHROMIUM_EXECUTABLE") {
        options = options.executable(path);
    }
    options
}

async fn visit(profile: &Path, origin: &str, script: &str) -> Result<serde_json::Value> {
    let browser = Browser::launch(launch_options(profile)).await?;
    let outcome = async {
        let page = browser
            .default_context(ContextOptions::default())
            .await?
            .page()
            .await?;
        page.goto(origin, WaitUntil::Load).await?;
        page.evaluate::<serde_json::Value>(script).await
    }
    .await;
    browser.close().await?;
    outcome
}

#[tokio::test]
#[ignore = "launches a real Chromium"]
async fn a_copied_chromium_profile_keeps_cookies_and_storage() -> Result<()> {
    let base = scratch("live");
    let origin = serve().await?;
    visit(&base.join("before/profile-chromium"), &origin, STORE).await?;

    let copied = base.join("after/profile-chromium");
    std::fs::create_dir_all(base.join("after")).context("create the new session")?;
    source(&base).copy_to(Engine::Chromium, &copied)?;
    let stored: Stored = serde_json::from_value(visit(&copied, &origin, READ).await?)
        .context("read what the copied profile holds")?;

    ensure!(
        stored.cookie.contains("session=kept"),
        "cookie lost: {:?}",
        stored.cookie
    );
    ensure!(
        stored.local.as_deref() == Some("kept"),
        "local storage lost: {:?}",
        stored.local
    );
    ensure!(
        stored.indexed.as_deref() == Some("kept"),
        "IndexedDB lost: {:?}",
        stored.indexed
    );
    std::fs::remove_dir_all(&base).context("clean up")?;
    Ok(())
}
