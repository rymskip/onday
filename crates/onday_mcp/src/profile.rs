//! Launching a browser from a copy of another session's profile.
//!
//! A profile holds everything a browser keeps between runs: installed extensions
//! with their own storage and service workers, cookies, saved logins, local
//! storage and IndexedDB. The copy is the whole profile less the entries a running
//! browser claims it with, so the new browser opens it as its own and the other
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

/// The session a browser's profile is copied from.
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

    /// Replace `target` with a copy of this session's `engine` profile. The
    /// session's lock is held for the copy, so no browser of its own writes the
    /// profile meanwhile, and `target` changes only once the copy is complete.
    pub fn replace(&self, engine: Engine, target: &Path) -> Result<()> {
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
                    "session {} is open in another onday_mcp (pid {holder}); close it before copying its profile",
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
        let replaced = target.with_file_name(format!("{}.replaced", name.to_string_lossy()));
        // Left by a replacement interrupted part way.
        for leftover in [&partial, &replaced] {
            if leftover.exists() {
                std::fs::remove_dir_all(leftover)
                    .with_context(|| format!("clear {}", leftover.display()))?;
            }
        }
        copy_tree(&source, &partial, true).with_context(|| {
            format!(
                "copy session {}'s {engine} profile from {}",
                self.id,
                source.display()
            )
        })?;
        let had_profile = target.exists();
        if had_profile {
            std::fs::rename(target, &replaced)
                .with_context(|| format!("move {} aside", target.display()))?;
        }
        std::fs::rename(&partial, target)
            .with_context(|| format!("move the copied profile to {}", target.display()))?;
        // The copy is in place; a profile left behind here is cleared by the next replacement.
        if had_profile && let Err(error) = std::fs::remove_dir_all(&replaced) {
            tracing::warn!(
                "removing the replaced profile {} failed: {error}",
                replaced.display()
            );
        }
        Ok(())
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
