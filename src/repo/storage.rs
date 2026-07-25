//! Durable-store plumbing: atomic file writes and the repo mutation lock.
//!
//! Navi exists for concurrent agents, so its own bookkeeping must survive
//! concurrent navi processes. Writes go through a temp-file-plus-rename so a
//! crash can never leave truncated TOML behind, and mutating verbs serialize
//! on one advisory file lock so read-modify-write cycles cannot interleave.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::config::navi_dir_path;

const LOCK_FILE: &str = "mutation.lock";
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Write `contents` to `path` atomically (temp file + rename).
pub(crate) fn save_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("storage path has no parent"))?;
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("storage path has no file name"))?;
    let mut temp_name = file_name.to_owned();
    temp_name.push(format!(".tmp.{}", std::process::id()));
    let temp_path = parent.join(temp_name);

    fs::write(&temp_path, contents)?;
    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

/// Advisory repo-wide lock held for the duration of a mutating navi verb.
///
/// The lock serializes navi's own read-modify-write cycles (registry,
/// metadata, landing sequences). It cannot serialize raw `jj` commands run
/// outside navi; that is what `jj`'s own operation log is for.
pub(crate) struct MutationLock {
    file: File,
}

impl MutationLock {
    /// Acquire the repo mutation lock, waiting up to the configured timeout.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created or if another
    /// process holds the lock past the timeout
    /// (`NAVI_LOCK_TIMEOUT_MS`, default 60s).
    pub(crate) fn acquire(repo_storage_path: &Path) -> Result<Self> {
        let path = lock_path(repo_storage_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;

        let timeout = lock_timeout();
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(Error::MutationLockTimeout {
                            path: path.display().to_string(),
                            waited_ms: timeout.as_millis(),
                        });
                    }
                    std::thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(Error::Io(error)),
            }
        }
    }
}

impl Drop for MutationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn lock_path(repo_storage_path: &Path) -> PathBuf {
    navi_dir_path(repo_storage_path).join(LOCK_FILE)
}

fn lock_timeout() -> Duration {
    std::env::var("NAVI_LOCK_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_LOCK_TIMEOUT, Duration::from_millis)
}
