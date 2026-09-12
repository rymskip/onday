//! The session directory: `<output-dir>/<id>/` with its lock, metadata, profile,
//! logs, screenshots and downloads.

use std::fs::{File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, RandomState};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use onday::{ConsoleMessage, Engine, NetworkEntry};
use serde::Serialize;
use time::OffsetDateTime;
use time::macros::format_description;

pub struct SessionDir {
    pub id: String,
    pub root: PathBuf,
    // Held for the process lifetime; the OS releases it if the process dies.
    lock: File,
    console_log: Mutex<File>,
    network_log: Mutex<File>,
}

#[derive(Serialize)]
struct Metadata<'a> {
    id: &'a str,
    pid: u32,
    cwd: String,
    started_at: String,
    engine: Option<Engine>,
    protocol: Option<String>,
}

impl SessionDir {
    /// Create (or reclaim) the session directory and take its lock.
    pub fn open(output_dir: &Path, id: Option<String>) -> Result<SessionDir> {
        let id = match id {
            Some(id) => validate_id(id)?,
            None => generated_id()?,
        };
        let root = absolute(&output_dir.join(&id))?;
        for dir in ["logs", "screenshots", "downloads", "snapshots"] {
            std::fs::create_dir_all(root.join(dir))
                .with_context(|| format!("create {}", root.join(dir).display()))?;
        }
        let lock_path = root.join("lock");
        let mut lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&lock_path).unwrap_or_default();
                bail!(
                    "session {id} is in use by another onday_mcp (pid {}); pick another --session",
                    holder.trim()
                );
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("lock {}", lock_path.display()));
            }
        }
        lock.set_len(0).context("reset the lock file")?;
        write!(lock, "{}", std::process::id()).context("record the pid in the lock file")?;
        let console_log = append(&root.join("logs/console.log"))?;
        let network_log = append(&root.join("logs/network.log"))?;
        let dir = SessionDir {
            id,
            root,
            lock,
            console_log: Mutex::new(console_log),
            network_log: Mutex::new(network_log),
        };
        dir.write_metadata(None, None)?;
        Ok(dir)
    }

    pub fn profile(&self, engine: Engine) -> PathBuf {
        self.root.join(format!("profile-{engine}"))
    }

    pub fn driver_log(&self) -> PathBuf {
        self.root.join("logs/driver.log")
    }

    pub fn mcp_log(&self) -> PathBuf {
        self.root.join("logs/mcp.log")
    }

    pub fn screenshots(&self) -> PathBuf {
        self.root.join("screenshots")
    }

    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }

    pub fn snapshots(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn write_metadata(&self, engine: Option<Engine>, protocol: Option<String>) -> Result<()> {
        let metadata = Metadata {
            id: &self.id,
            pid: std::process::id(),
            cwd: std::env::current_dir()
                .map(|dir| dir.display().to_string())
                .unwrap_or_default(),
            started_at: now()?,
            engine,
            protocol,
        };
        let text = serde_json::to_string_pretty(&metadata).context("serialize session.json")?;
        std::fs::write(self.root.join("session.json"), text).context("write session.json")
    }

    pub fn log_console(&self, message: &ConsoleMessage) {
        let line = format!(
            "{} [{}] {}{}",
            message.timestamp,
            message.level,
            message.text,
            message
                .location
                .as_deref()
                .map(|at| format!(" ({at})"))
                .unwrap_or_default()
        );
        write_line(&self.console_log, &line);
    }

    pub fn log_network(&self, entry: &NetworkEntry) {
        let outcome = match (&entry.status, &entry.error) {
            (Some(status), _) => status.to_string(),
            (None, Some(error)) => format!("FAILED {error}"),
            (None, None) => "pending".to_string(),
        };
        let line = format!(
            "{} {} {} -> {outcome} ({}ms)",
            entry.started,
            entry.method,
            entry.url,
            entry.duration_ms.unwrap_or(0)
        );
        write_line(&self.network_log, &line);
    }

    /// The pid holding this session, for status output.
    pub fn holder(&self) -> Result<String> {
        let mut text = String::new();
        use std::io::Read as _;
        (&self.lock)
            .read_to_string(&mut text)
            .context("read the lock file")?;
        Ok(text)
    }
}

fn write_line(file: &Mutex<File>, line: &str) {
    let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(error) = writeln!(file, "{line}") {
        tracing::warn!("writing a session log failed: {error}");
    }
}

fn append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .context("read the working directory")?
        .join(path))
}

fn now() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .context("format the time")
}

fn validate_id(id: String) -> Result<String> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.');
    if !valid {
        bail!("session id {id:?} must be 1-64 letters, digits, '-', '_' or '.'");
    }
    Ok(id)
}

fn generated_id() -> Result<String> {
    let stamp = OffsetDateTime::now_utc()
        .format(format_description!(
            "[year][month][day]-[hour][minute][second]"
        ))
        .context("format the session id")?;
    let salt = RandomState::new().hash_one(std::process::id()) & 0xffff;
    Ok(format!("{stamp}-{salt:04x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_path_tricks() {
        assert!(validate_id("../escape".to_string()).is_err());
        assert!(validate_id(".hidden".to_string()).is_err());
        assert!(validate_id("session1".to_string()).is_ok());
    }

    #[test]
    fn a_second_process_cannot_take_a_live_session() {
        let base = std::env::temp_dir().join(format!("onday-lock-test-{}", std::process::id()));
        let first = SessionDir::open(&base, Some("shared".to_string())).expect("first open");
        let second = SessionDir::open(&base, Some("shared".to_string()));
        assert!(second.is_err());
        drop(first);
        let third = SessionDir::open(&base, Some("shared".to_string()));
        assert!(third.is_ok());
        std::fs::remove_dir_all(&base).expect("clean up");
    }
}
