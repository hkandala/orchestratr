//! `server enable` / `server disable` (spec §6.4): start-at-login registration so loops fire
//! after a reboot before any orcr command runs. macOS uses a launchd agent; Linux a systemd
//! user unit; anything else → `unsupported_platform`. Units use the **absolute binary path** and
//! explicitly propagate `ORCR_HOME` / `ORCR_HERDR_BIN` + log paths (no PATH assumptions under
//! launchd/systemd). Windows lands with general Windows support (§17).

use crate::error::{OrcrError, Result};
use crate::home::Home;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The launchd label / systemd unit base name (spec §6.4).
pub const LABEL: &str = "dev.orchestratr.orcr";

/// The rendered unit + where it lives, so `enable` can echo them and tests can assert content.
pub struct Unit {
    pub path: PathBuf,
    pub content: String,
    pub verify_command: String,
}

/// The absolute path to the running `orcr` binary (spec §6.4: units use the absolute path).
fn binary_path() -> Result<PathBuf> {
    std::env::current_exe().map_err(|e| {
        OrcrError::environment(
            "server_start_failed",
            format!("cannot determine orcr binary path: {e}"),
        )
    })
}

/// The `ORCR_HERDR_BIN` value to propagate: the configured/overridden value if present, else the
/// discovered herdr binary path (so launchd/systemd need no PATH).
fn herdr_bin_value(home: &Home) -> String {
    if let Ok(v) = std::env::var("ORCR_HERDR_BIN") {
        if !v.is_empty() {
            return v;
        }
    }
    let configured = crate::config::Config::load(home)
        .ok()
        .map(|c| c.config.herdr.bin)
        .filter(|s| !s.is_empty());
    let hint = configured.as_deref();
    crate::driver::HerdrBinary::discover(hint)
        .map(|b| b.path().display().to_string())
        .unwrap_or_else(|_| "herdr".to_string())
}

/// Render the macOS launchd plist (spec §6.4): `RunAtLoad`, `KeepAlive` on crash, absolute argv,
/// propagated `ORCR_HOME` / `ORCR_HERDR_BIN`, redirected logs.
pub fn launchd_plist(bin: &str, home: &Home, herdr_bin: &str) -> String {
    let out = home.logs_dir().join("service.out.log");
    let err = home.logs_dir().join("service.err.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>server</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>ORCR_HOME</key>
        <string>{home}</string>
        <key>ORCR_HERDR_BIN</key>
        <string>{herdr_bin}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>Crashed</key>
        <true/>
    </dict>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#,
        label = LABEL,
        bin = bin,
        home = home.root().display(),
        herdr_bin = herdr_bin,
        out = out.display(),
        err = err.display(),
    )
}

/// Render the Linux systemd user unit (spec §6.4): `Restart=on-failure`, absolute `ExecStart`,
/// propagated environment, wanted by `default.target`.
pub fn systemd_unit(bin: &str, home: &Home, herdr_bin: &str) -> String {
    format!(
        r#"[Unit]
Description=orchestratr server (orcr)
After=network.target

[Service]
Type=simple
ExecStart={bin} server start --foreground
Environment=ORCR_HOME={home}
Environment=ORCR_HERDR_BIN={herdr_bin}
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
"#,
        bin = bin,
        home = home.root().display(),
        herdr_bin = herdr_bin,
    )
}

/// The launchd plist path for the current user.
fn launchd_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        OrcrError::environment("server_start_failed", "cannot determine home directory")
    })?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

/// The systemd user-unit path for the current user.
fn systemd_path() -> Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| {
        OrcrError::environment("server_start_failed", "cannot determine config directory")
    })?;
    Ok(config.join("systemd").join("user").join("orcr.service"))
}

/// Build the platform [`Unit`] (rendered content + install path + verify command) without
/// touching the filesystem — the pure core `enable` writes and tests assert.
pub fn build_unit(home: &Home) -> Result<Unit> {
    let bin = binary_path()?.display().to_string();
    let herdr_bin = herdr_bin_value(home);
    if cfg!(target_os = "macos") {
        Ok(Unit {
            path: launchd_path()?,
            content: launchd_plist(&bin, home, &herdr_bin),
            verify_command: format!("launchctl list | grep {LABEL}"),
        })
    } else if cfg!(target_os = "linux") {
        Ok(Unit {
            path: systemd_path()?,
            content: systemd_unit(&bin, home, &herdr_bin),
            verify_command: "systemctl --user status orcr.service".to_string(),
        })
    } else {
        Err(OrcrError::environment(
            "unsupported_platform",
            "server enable/disable is supported on macOS and Linux only \
             (Windows lands with general Windows support)",
        ))
    }
}

/// `server enable` (spec §6.4): write the unit, then register + start it. Returns the created
/// unit path + the verify command; the loader step is best-effort (a headless CI session may
/// lack a launchd/systemd bus — the unit file is the durable registration).
pub fn enable(home: &Home) -> Result<Value> {
    let unit = build_unit(home)?;
    if let Some(dir) = unit.path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            OrcrError::environment(
                "server_start_failed",
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
    }
    std::fs::write(&unit.path, &unit.content).map_err(|e| {
        OrcrError::environment(
            "server_start_failed",
            format!("cannot write {}: {e}", unit.path.display()),
        )
    })?;

    let loader_ok = run_loader(&unit, true);
    Ok(json!({
        "status": "enabled",
        "unit": unit.path.display().to_string(),
        "verify": unit.verify_command,
        "loaded": loader_ok,
    }))
}

/// `server disable` (spec §6.4): unregister and remove the unit file. The running server + store
/// are untouched.
pub fn disable(home: &Home) -> Result<Value> {
    let unit = build_unit(home)?;
    let loader_ok = run_loader(&unit, false);
    let existed = unit.path.exists();
    if existed {
        std::fs::remove_file(&unit.path).map_err(|e| {
            OrcrError::environment(
                "server_start_failed",
                format!("cannot remove {}: {e}", unit.path.display()),
            )
        })?;
    }
    Ok(json!({
        "status": "disabled",
        "unit": unit.path.display().to_string(),
        "removed": existed,
        "unloaded": loader_ok,
    }))
}

/// Run the platform loader (`launchctl` / `systemctl`) to (un)register the unit. Best-effort:
/// returns whether it succeeded (a missing bus in CI is not fatal — the unit file persists).
fn run_loader(unit: &Unit, enable: bool) -> bool {
    use std::process::Command;
    let run = |program: &str, args: &[&str]| -> bool {
        Command::new(program)
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if cfg!(target_os = "macos") {
        let path = unit.path.display().to_string();
        if enable {
            run("launchctl", &["load", "-w", &path])
        } else {
            run("launchctl", &["unload", "-w", &path])
        }
    } else if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        if enable {
            run("systemctl", &["--user", "enable", "--now", "orcr.service"])
        } else {
            run("systemctl", &["--user", "disable", "--now", "orcr.service"])
        }
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_home() -> (TempDir, Home) {
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ORCR_HOME", tmp.path());
        let home = Home::ensure().unwrap();
        (tmp, home)
    }

    #[test]
    fn launchd_plist_golden() {
        let (_t, home) = test_home();
        let plist = launchd_plist("/usr/local/bin/orcr", &home, "/opt/herdr/bin/herdr");
        assert!(plist.contains("<string>dev.orchestratr.orcr</string>"));
        assert!(plist.contains("<string>/usr/local/bin/orcr</string>"));
        assert!(plist.contains("<string>--foreground</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ORCR_HOME</key>"));
        assert!(plist.contains(&home.root().display().to_string()));
        assert!(plist.contains("/opt/herdr/bin/herdr"));
        assert!(plist.contains("service.out.log"));
    }

    #[test]
    fn systemd_unit_golden() {
        let (_t, home) = test_home();
        let unit = systemd_unit("/usr/local/bin/orcr", &home, "/opt/herdr/bin/herdr");
        assert!(unit.contains("ExecStart=/usr/local/bin/orcr server start --foreground"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains(&format!("Environment=ORCR_HOME={}", home.root().display())));
        assert!(unit.contains("Environment=ORCR_HERDR_BIN=/opt/herdr/bin/herdr"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}
