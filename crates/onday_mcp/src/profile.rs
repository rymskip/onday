//! Starting a new session's browser from a copy of an earlier session's profile.
//!
//! A profile holds everything a browser keeps between runs: installed extensions
//! with their own storage and service workers, cookies, saved logins, local
//! storage and IndexedDB. The copy is the whole profile less the entries a running
//! browser claims it with, so the new browser opens it as its own and the earlier
//! session keeps its original untouched.

use std::fs::{OpenOptions, TryLockError};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use onday::Engine;

use crate::session::profile_path;

/// Entries at the top of a profile through which a running browser claims it:
/// Chromium's `Singleton*` links, version marker and debugging port, Firefox's
/// lock files. A copy carrying them reads as a profile another browser already
/// has open, or points the driver at a port no browser listens on.
const CLAIMS: [&str; 8] = [
    "SingletonLock",
    "SingletonSocket",
    "SingletonCookie",
    "RunningChromeVersion",
    "DevToolsActivePort",
    "parent.lock",
    ".parentlock",
    "lock",
];

/// The earlier session a new session's profile starts from.
pub struct ProfileSource {
    id: String,
    root: PathBuf,
}

impl ProfileSource {
    pub fn new(id: String, root: PathBuf) -> ProfileSource {
        ProfileSource { id, root }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Copy this session's `engine` profile to `target`, which does not exist yet.
    /// The session's lock is held for the copy, so no browser of its own writes the
    /// profile meanwhile, and the copy lands under its name only once complete.
    pub fn copy_to(&self, engine: Engine, target: &Path) -> Result<()> {
        if !self.root.is_dir() {
            bail!(
                "there is no session {} at {} to start the profile from",
                self.id,
                self.root.display()
            );
        }
        let lock_path = self.root.join("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let holder = std::fs::read_to_string(&lock_path)
                    .map(|pid| pid.trim().to_string())
                    .unwrap_or_else(|error| format!("unreadable: {error}"));
                bail!(
                    "session {} is open in another onday_mcp (pid {holder}); close it before starting from its profile",
                    self.id
                );
            }
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("lock {}", lock_path.display()));
            }
        }

        let source = profile_path(&self.root, engine);
        if !source.is_dir() {
            bail!("session {} has no {engine} profile to start from", self.id);
        }
        let name = target
            .file_name()
            .with_context(|| format!("{} names no profile", target.display()))?;
        let partial = target.with_file_name(format!("{}.partial", name.to_string_lossy()));
        // Left by a copy interrupted before it was renamed into place.
        if partial.exists() {
            std::fs::remove_dir_all(&partial)
                .with_context(|| format!("clear the unfinished copy {}", partial.display()))?;
        }
        copy_tree(&source, &partial, true).with_context(|| {
            format!(
                "copy session {}'s {engine} profile from {}",
                self.id,
                source.display()
            )
        })?;
        std::fs::rename(&partial, target)
            .with_context(|| format!("move the copied profile to {}", target.display()))
    }
}

/// Copy `from` into `to`, keeping links as links. `top` marks the profile's own
/// directory, the only place a browser keeps its claims.
fn copy_tree(from: &Path, to: &Path, top: bool) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("create {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry.with_context(|| format!("read an entry of {}", from.display()))?;
        let name = entry.file_name();
        if top && CLAIMS.iter().any(|claim| name == *claim) {
            continue;
        }
        let path = entry.path();
        let destination = to.join(&name);
        let kind = entry
            .file_type()
            .with_context(|| format!("read the type of {}", path.display()))?;
        if kind.is_symlink() {
            let points_to = std::fs::read_link(&path)
                .with_context(|| format!("read the link {}", path.display()))?;
            symlink(&points_to, &destination)
                .with_context(|| format!("link {}", destination.display()))?;
        } else if kind.is_dir() {
            copy_tree(&path, &destination, false)?;
        } else {
            std::fs::copy(&path, &destination)
                .with_context(|| format!("copy {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
