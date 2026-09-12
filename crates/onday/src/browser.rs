//! A running browser: its driver, its WebDriver session and the protocol it speaks.

use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use thirtyfour::bidi::BiDi;
use thirtyfour::manager::{BrowserKind, WebDriverManager};
use thirtyfour::{Capabilities, WebDriver, WindowHandle};
use tokio::process::Child;

use crate::context::{BrowserContext, ContextOptions};
use crate::engine::Engine;
use crate::events::EventHub;
use crate::launch::{DriverSource, LaunchOptions, Protocol, ProtocolPreference};
use crate::poll::{Backoff, poll_until};

/// One WebDriver session and, when negotiated, its BiDi connection.
pub(crate) struct Session {
    pub(crate) driver: WebDriver,
    pub(crate) bidi: Option<BiDi>,
    pub(crate) hub: Option<Arc<EventHub>>,
    /// The Classic window commands currently target; held for a page operation.
    pub(crate) classic_window: tokio::sync::Mutex<Option<WindowHandle>>,
    endpoint: Endpoint,
}

impl Session {
    async fn open(
        endpoint: Endpoint,
        caps: &Capabilities,
        preference: ProtocolPreference,
    ) -> Result<Session> {
        let driver = match &endpoint {
            Endpoint::Managed(manager) => manager
                .launch(caps.clone())
                .await
                .context("start a session through the driver manager")?,
            Endpoint::Url(url) => WebDriver::new(url.as_str(), caps.clone())
                .await
                .with_context(|| format!("start a session at {url}"))?,
            Endpoint::Spawned(process) => WebDriver::new(process.url.as_str(), caps.clone())
                .await
                .with_context(|| format!("start a session at {}", process.url))?,
        };
        let offered = driver
            .handle()
            .capabilities()
            .get("webSocketUrl")
            .is_some_and(|url| url.is_string());
        let bidi = if preference == ProtocolPreference::Auto && offered {
            match driver.bidi().await {
                Ok(bidi) => Some(bidi),
                Err(error) => {
                    tracing::warn!(
                        "the driver offered BiDi but the connection failed; using Classic: {error}"
                    );
                    None
                }
            }
        } else {
            None
        };
        let hub = match &bidi {
            Some(bidi) => Some(
                EventHub::start(bidi.clone())
                    .await
                    .context("start BiDi event routing")?,
            ),
            None => None,
        };
        let window = driver
            .window()
            .await
            .context("read the initial window handle")?;
        Ok(Session {
            driver,
            bidi,
            hub,
            classic_window: tokio::sync::Mutex::new(Some(window)),
            endpoint,
        })
    }

    pub(crate) fn protocol(&self) -> Protocol {
        if self.bidi.is_some() {
            Protocol::Bidi
        } else {
            Protocol::Classic
        }
    }

    /// End the session, then stop a driver process onday spawned for it.
    pub(crate) async fn quit(&self) -> Result<()> {
        let quit = self
            .driver
            .clone()
            .quit()
            .await
            .context("end the WebDriver session");
        if let Endpoint::Spawned(process) = &self.endpoint {
            let child = process
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            if let Some(mut child) = child {
                child.start_kill().context("stop the driver process")?;
                child.wait().await.context("reap the driver process")?;
            }
        }
        quit
    }
}

/// A driver process onday spawned itself.
pub(crate) struct SpawnedDriver {
    url: String,
    child: Mutex<Option<Child>>,
}

pub(crate) enum Endpoint {
    Managed(Arc<WebDriverManager>),
    Url(String),
    Spawned(SpawnedDriver),
}

pub(crate) struct BrowserInner {
    pub(crate) engine: Engine,
    pub(crate) options: LaunchOptions,
    pub(crate) caps: Capabilities,
    pub(crate) primary: Arc<Session>,
}

impl BrowserInner {
    /// Another session with its own driver: some drivers (geckodriver) hold one session each.
    pub(crate) async fn open_session(&self) -> Result<Session> {
        let endpoint = endpoint_for(&self.options).await?;
        Session::open(endpoint, &self.caps, self.options.protocol).await
    }
}

/// A launched browser.
#[derive(Clone)]
pub struct Browser {
    pub(crate) inner: Arc<BrowserInner>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("engine", &self.inner.engine)
            .field("protocol", &self.protocol())
            .finish()
    }
}

impl Browser {
    /// Start a browser as described by `options`.
    pub async fn launch(options: LaunchOptions) -> Result<Browser> {
        let engine = options.engine;
        let caps = engine.capabilities(&options)?;
        let endpoint = endpoint_for(&options).await?;
        let primary = Session::open(endpoint, &caps, options.protocol)
            .await
            .with_context(|| format!("launch {engine}"))?;
        if let (Some((width, height)), true) = (options.window_size, engine != Engine::Chromium) {
            primary
                .driver
                .set_window_rect(0, 0, width, height)
                .await
                .context("size the browser window")?;
        }
        Ok(Browser {
            inner: Arc::new(BrowserInner {
                engine,
                options,
                caps,
                primary: Arc::new(primary),
            }),
        })
    }

    pub fn engine(&self) -> Engine {
        self.inner.engine
    }

    pub fn protocol(&self) -> Protocol {
        self.inner.primary.protocol()
    }

    pub fn options(&self) -> &LaunchOptions {
        &self.inner.options
    }

    /// The browser's first session as a raw thirtyfour driver.
    pub fn driver(&self) -> &WebDriver {
        &self.inner.primary.driver
    }

    /// The BiDi connection of the first session, when negotiated.
    pub fn bidi(&self) -> Option<&BiDi> {
        self.inner.primary.bidi.as_ref()
    }

    /// The context the browser started with (its default profile).
    pub async fn default_context(&self, options: ContextOptions) -> Result<BrowserContext> {
        BrowserContext::open(
            self.inner.clone(),
            self.inner.primary.clone(),
            None,
            false,
            options,
        )
        .await
    }

    /// A fresh, isolated context: a BiDi user context, or a new session on Classic.
    pub async fn new_context(&self, options: ContextOptions) -> Result<BrowserContext> {
        match &self.inner.primary.bidi {
            Some(bidi) => {
                let user_context = bidi
                    .browser()
                    .create_user_context()
                    .await
                    .context("browser.createUserContext")?
                    .user_context;
                BrowserContext::open(
                    self.inner.clone(),
                    self.inner.primary.clone(),
                    Some(user_context),
                    false,
                    options,
                )
                .await
            }
            None => {
                let session = Arc::new(self.inner.open_session().await?);
                BrowserContext::open(self.inner.clone(), session, None, true, options).await
            }
        }
    }

    /// End the session and stop any driver onday started.
    pub async fn close(&self) -> Result<()> {
        self.inner.primary.quit().await
    }
}

async fn endpoint_for(options: &LaunchOptions) -> Result<Endpoint> {
    if let DriverSource::Remote(url) = &options.driver {
        return Ok(Endpoint::Url(url.clone()));
    }
    let kind = match options.engine {
        Engine::Chromium => BrowserKind::Chrome,
        Engine::Firefox => BrowserKind::Firefox,
        Engine::Webkit if cfg!(target_os = "macos") => BrowserKind::Safari,
        Engine::Webkit => {
            let binary = match &options.driver {
                DriverSource::Binary(path) => path.clone(),
                _ => find_on_path("WebKitWebDriver").context(
                    "webkit needs WebKitWebDriver (install WebKitGTK's webdriver package or pass a driver path)",
                )?,
            };
            return Ok(Endpoint::Spawned(
                spawn_driver(&binary, options.driver_log.as_deref()).await?,
            ));
        }
    };
    let mut builder = WebDriverManager::builder();
    if let DriverSource::Binary(path) = &options.driver {
        builder = builder.driver_binary(kind, path.clone());
    }
    if let Some(path) = &options.driver_log {
        let file = open_log(path)?;
        builder = builder.on_driver_log(move |line| {
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Err(error) = writeln!(file, "[{}] {}", line.stream, line.line) {
                tracing::debug!("writing the driver log failed: {error}");
            }
        });
    }
    Ok(Endpoint::Managed(builder.build()))
}

fn open_log(path: &Path) -> Result<Arc<Mutex<std::fs::File>>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open driver log {}", path.display()))?;
    Ok(Arc::new(Mutex::new(file)))
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

async fn spawn_driver(binary: &Path, log: Option<&Path>) -> Result<SpawnedDriver> {
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .context("pick a free port for the driver")?
        .port();
    let (stdout, stderr) = match log {
        Some(path) => {
            let file = open_log(path)?;
            let file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let out = file.try_clone().context("share the driver log")?;
            let err = file.try_clone().context("share the driver log")?;
            (Stdio::from(out), Stdio::from(err))
        }
        None => (Stdio::null(), Stdio::null()),
    };
    let child = tokio::process::Command::new(binary)
        .arg(format!("--port={port}"))
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listening = poll_until(
        || async { tokio::net::TcpStream::connect(address).await.is_ok() },
        Duration::from_secs(15),
        Backoff::SERVICE,
        |_| {},
    )
    .await;
    if !listening {
        bail!(
            "{} did not start listening on port {port}",
            binary.display()
        );
    }
    Ok(SpawnedDriver {
        url: format!("http://127.0.0.1:{port}"),
        child: Mutex::new(Some(child)),
    })
}
