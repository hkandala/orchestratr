//! The server's own structured log (`$ORCR_HOME/logs/server.log`), spec §6.4.
//!
//! One JSON object per line (`{ts, level, msg}`) — startup, herdr connection events,
//! shutdown, errors. Size-capped with simple numbered rotation
//! (`server.log` → `server.log.1` → … up to `logs.max_files`). `orcr server logs`
//! reads these back with `--tail`/`--follow`.

use crate::error::{OrcrError, Result};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A thread-safe, size-capped, rotating log writer.
pub struct ServerLog {
    path: PathBuf,
    max_bytes: u64,
    max_files: u32,
    file: Mutex<File>,
}

impl ServerLog {
    /// Open (creating/appending) the log at `logs_dir/server.log`.
    pub fn open(logs_dir: &Path, max_bytes: u64, max_files: u32) -> Result<ServerLog> {
        std::fs::create_dir_all(logs_dir).map_err(|e| {
            OrcrError::environment(
                "home_create_failed",
                format!("cannot create logs dir {}: {e}", logs_dir.display()),
            )
        })?;
        let path = logs_dir.join("server.log");
        let file = open_append(&path)?;
        Ok(ServerLog {
            path,
            max_bytes: max_bytes.max(1),
            max_files: max_files.max(1),
            file: Mutex::new(file),
        })
    }

    /// Append a structured line at the given level. Failures are swallowed (logging must
    /// never take the server down), but rotation is attempted first.
    pub fn log(&self, level: &str, msg: impl AsRef<str>) {
        let line = json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "level": level,
            "msg": msg.as_ref(),
        })
        .to_string();
        let mut file = match self.file.lock() {
            Ok(f) => f,
            Err(_) => return,
        };
        // Rotate if the current file is at/over the cap.
        if let Ok(meta) = file.metadata() {
            if meta.len() + line.len() as u64 + 1 > self.max_bytes {
                if let Ok(fresh) = self.rotate() {
                    *file = fresh;
                }
            }
        }
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.log("info", msg);
    }
    pub fn warn(&self, msg: impl AsRef<str>) {
        self.log("warn", msg);
    }
    pub fn error(&self, msg: impl AsRef<str>) {
        self.log("error", msg);
    }

    /// Shift `server.log.(n-1)` → `server.log.n` … and `server.log` → `server.log.1`,
    /// dropping anything past `max_files`, then reopen a fresh empty `server.log`.
    fn rotate(&self) -> Result<File> {
        rotate_numbered(&self.path, self.max_files);
        open_append(&self.path)
    }
}

/// Shift `path.(n-1)` → `path.n` … up to `max_files`, then `path` → `path.1`, dropping
/// anything past the cap. Shared by [`ServerLog`] and the per-run `RunLog`; does **not**
/// reopen `path` (each caller re-opens/reset as its threading model requires).
pub(super) fn rotate_numbered(path: &Path, max_files: u32) {
    if max_files == 0 {
        return;
    }
    let numbered = |n: u32| -> PathBuf {
        let name = format!(
            "{}.{n}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("log")
        );
        path.with_file_name(name)
    };
    for i in (1..max_files).rev() {
        let from = numbered(i);
        if from.exists() {
            let _ = std::fs::rename(&from, numbered(i + 1));
        }
    }
    let _ = std::fs::rename(path, numbered(1));
}

fn open_append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            OrcrError::environment(
                "home_create_failed",
                format!("cannot open log file {}: {e}", path.display()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_rotates() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        // Tiny cap so a couple of lines force rotation.
        let log = ServerLog::open(&logs, 120, 3).unwrap();
        for i in 0..20 {
            log.info(format!("line number {i} with some padding text"));
        }
        // The primary file exists and at least one rotated file was created.
        assert!(logs.join("server.log").exists());
        assert!(logs.join("server.log.1").exists());
        // Retention cap respected: no file beyond .3.
        assert!(!logs.join("server.log.4").exists());
    }

    #[test]
    fn lines_are_json() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        let log = ServerLog::open(&logs, 1_000_000, 3).unwrap();
        log.info("hello");
        let content = std::fs::read_to_string(logs.join("server.log")).unwrap();
        let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(v["level"], "info");
        assert_eq!(v["msg"], "hello");
    }
}
