//! The orcr home directory layout and its safety checks (spec §11.6, §14).
//!
//! Everything orcr owns lives under a single home directory — default `~/.orcr`,
//! relocatable via `ORCR_HOME` (which relocates the store, socket, lock, config, logs,
//! and data all at once; the guarantee tests and sandboxes rely on). The server refuses
//! to operate unless the home is owned by the current uid and is not group/world
//! writable (`unsafe_home`).

use crate::error::{OrcrError, Result};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A resolved orcr home layout. Construct with [`Home::resolve`] (path only) or
/// [`Home::ensure`] (create + safety-check).
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// Resolve the home *path* from the environment without creating or validating it:
    /// `$ORCR_HOME` if set and non-empty, else `~/.orcr`.
    pub fn resolve() -> Result<Home> {
        let root = match std::env::var_os("ORCR_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let home = dirs::home_dir().ok_or_else(|| {
                    OrcrError::environment(
                        "no_home_dir",
                        "cannot determine the user's home directory; set ORCR_HOME",
                    )
                })?;
                home.join(".orcr")
            }
        };
        Ok(Home { root })
    }

    /// Build a [`Home`] rooted at an explicit path (mainly for tests).
    pub fn at(root: impl Into<PathBuf>) -> Home {
        Home { root: root.into() }
    }

    /// Create the home layout (idempotent) and then run the safety check. The home root
    /// is created with mode `0700`; the `logs/` and `data/` subtrees are created too.
    pub fn ensure() -> Result<Home> {
        let home = Home::resolve()?;
        home.ensure_layout()?;
        Ok(home)
    }

    /// Create the directory tree and validate safety. Public so tests can drive it
    /// against a custom root via [`Home::at`].
    pub fn ensure_layout(&self) -> Result<()> {
        // Create the root with restrictive perms if it does not exist yet.
        if !self.root.exists() {
            std::fs::create_dir_all(&self.root).map_err(|e| {
                OrcrError::environment(
                    "home_create_failed",
                    format!("could not create {}: {e}", self.root.display()),
                )
            })?;
            // Tighten perms on the freshly created root (umask may have loosened them).
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&self.root, perms).map_err(|e| {
                OrcrError::environment(
                    "home_create_failed",
                    format!("could not set permissions on {}: {e}", self.root.display()),
                )
            })?;
        }
        self.check_safety()?;
        for sub in [self.logs_dir(), self.data_dir()] {
            std::fs::create_dir_all(&sub).map_err(|e| {
                OrcrError::environment(
                    "home_create_failed",
                    format!("could not create {}: {e}", sub.display()),
                )
            })?;
        }
        Ok(())
    }

    /// Refuse to proceed unless the home is owned by the current uid and is not
    /// group/world writable. Yields `environment_error {cause: unsafe_home}`.
    pub fn check_safety(&self) -> Result<()> {
        let md = std::fs::metadata(&self.root).map_err(|e| {
            OrcrError::environment(
                "unsafe_home",
                format!("cannot stat {}: {e}", self.root.display()),
            )
        })?;
        if !md.is_dir() {
            return Err(OrcrError::environment(
                "unsafe_home",
                format!("{} is not a directory", self.root.display()),
            ));
        }
        // SAFETY: getuid is always safe and never fails.
        let uid = unsafe { libc::getuid() };
        if md.uid() != uid {
            return Err(OrcrError::environment(
                "unsafe_home",
                format!(
                    "{} is owned by uid {}, not the current uid {uid}",
                    self.root.display(),
                    md.uid()
                ),
            ));
        }
        let mode = md.permissions().mode();
        if mode & 0o022 != 0 {
            return Err(OrcrError::environment(
                "unsafe_home",
                format!(
                    "{} is group- or world-writable (mode {:o}); tighten it to 0700",
                    self.root.display(),
                    mode & 0o777
                ),
            ));
        }
        Ok(())
    }

    /// The home root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The sqlite store path (`<home>/orcr.db`).
    pub fn store_path(&self) -> PathBuf {
        self.root.join("orcr.db")
    }

    /// The Unix socket path (`<home>/orcr.sock`).
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("orcr.sock")
    }

    /// The single-instance lock file (`<home>/orcr.lock`).
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("orcr.lock")
    }

    /// The config file (`<home>/config.json`).
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    /// The logs directory (`<home>/logs`).
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The data directory (`<home>/data`).
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subpaths_are_under_root() {
        let h = Home::at("/tmp/x");
        assert_eq!(h.store_path(), PathBuf::from("/tmp/x/orcr.db"));
        assert_eq!(h.socket_path(), PathBuf::from("/tmp/x/orcr.sock"));
        assert_eq!(h.lock_path(), PathBuf::from("/tmp/x/orcr.lock"));
        assert_eq!(h.config_path(), PathBuf::from("/tmp/x/config.json"));
        assert_eq!(h.logs_dir(), PathBuf::from("/tmp/x/logs"));
        assert_eq!(h.data_dir(), PathBuf::from("/tmp/x/data"));
    }

    #[test]
    fn ensure_creates_layout_and_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("orcr_home");
        let h = Home::at(&root);
        h.ensure_layout().unwrap();
        assert!(root.is_dir());
        assert!(h.logs_dir().is_dir());
        assert!(h.data_dir().is_dir());
        // idempotent
        h.ensure_layout().unwrap();
    }

    #[test]
    fn detects_world_writable_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("bad_home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        let h = Home::at(&root);
        let e = h.check_safety().unwrap_err();
        assert_eq!(e.details["cause"], "unsafe_home");
    }

    #[test]
    fn accepts_0700_home() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("good_home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        Home::at(&root).check_safety().unwrap();
    }
}
