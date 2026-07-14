//! The single-instance lock (spec §11.6).
//!
//! The server takes an exclusive advisory `flock` on `$ORCR_HOME/orcr.lock` and holds it
//! for its entire lifetime; it refuses to open the store without it. This is what makes
//! "exactly one server" true even when many clients race to auto-start one: whoever wins
//! the `flock` becomes the server, and the losers fall back to waiting for readiness.
//!
//! The lock is advisory and process-associated: it is released automatically when the
//! holding process exits (even on `kill -9`), which is exactly the crash-recovery property
//! we want — a dead server's lock is free for the next starter.

use crate::error::{OrcrError, Result};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// An held exclusive instance lock. Dropping it (or the process exiting) releases the lock.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Try to acquire the exclusive lock without blocking. Returns `Ok(None)` when another
    /// process already holds it (i.e. a server is running or starting).
    pub fn try_acquire(path: impl AsRef<Path>) -> Result<Option<InstanceLock>> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| {
                OrcrError::environment(
                    "store_locked",
                    format!("cannot open lock file {}: {e}", path.display()),
                )
            })?;
        // SAFETY: flock on a valid fd; LOCK_EX|LOCK_NB is non-blocking.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Ok(Some(InstanceLock { _file: file, path }))
        } else {
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(None),
                _ => Err(OrcrError::environment(
                    "store_locked",
                    format!("failed to acquire instance lock {}: {err}", path.display()),
                )
                .with_details(json!({ "cause": "store_locked" }))),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_is_blocked_then_freed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("orcr.lock");
        let first = InstanceLock::try_acquire(&path).unwrap();
        assert!(first.is_some(), "first acquire should win");
        // A second acquire from the same process on a different fd is blocked
        // (flock is per open-file-description).
        let second = InstanceLock::try_acquire(&path).unwrap();
        assert!(second.is_none(), "second acquire should be blocked");
        drop(first);
        // Now it is free again. The `flock` release happens on `close(2)` in `drop`, but under
        // heavy parallel test load the kernel occasionally doesn't reflect that release to an
        // immediately-following `flock` on a fresh fd, so poll briefly rather than assert an
        // instantaneous re-acquire (the real auto-start reaper is likewise release-latency
        // tolerant via its stable-dead probe window).
        let mut third = None;
        for _ in 0..100 {
            third = InstanceLock::try_acquire(&path).unwrap();
            if third.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(third.is_some(), "lock should be free after drop");
    }
}
