use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use serde::Serialize;
use signal_hook::consts::SIGTERM;
use signal_hook::flag;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::config::Config;
use crate::herdr::{discover_herdr, HerdrClient};
use crate::jobs::{
    append_job_event, parse_rfc3339_utc, run_loop_tick, run_schedule_tick, tick_on_fires,
    CatchupPolicy, LoopSpec, ScheduleSpec, TICK_ON_POLL_SECS,
};
use crate::store::{EventRow, JobRow, Store};
use crate::{engine::Engine, profile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

pub fn pid_path(store_root: &Path) -> PathBuf {
    store_root.join("serve.pid")
}

pub fn sock_path(store_root: &Path) -> PathBuf {
    store_root.join("serve.sock")
}

pub fn read_pid(store_root: &Path) -> Option<u32> {
    fs::read_to_string(pid_path(store_root))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub fn ping(store_root: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock_path(store_root)) else {
        return false;
    };
    if stream.write_all(b"ping\n").is_err() {
        return false;
    }
    let mut buf = [0_u8; 8];
    matches!(stream.read(&mut buf), Ok(n) if &buf[..n] == b"pong\n")
}

pub fn status(store_root: &Path) -> DaemonStatus {
    let running = ping(store_root);
    DaemonStatus {
        running,
        pid: running.then(|| read_pid(store_root)).flatten(),
    }
}

pub fn start_background(config: &Config) -> Result<DaemonStatus> {
    if ping(&config.store_root) {
        return Ok(status(&config.store_root));
    }
    fs::create_dir_all(config.store_root.join("logs"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.store_root.join("logs/serve.log"))?;
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    Command::new(exe)
        .arg("serve")
        .arg("--foreground")
        .env("ORCR_STORE", &config.store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .context("failed to spawn orcr serve")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ping(&config.store_root) {
            return Ok(status(&config.store_root));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(anyhow!("daemon did not become ready"))
}

pub fn serve_foreground(config: Config) -> Result<()> {
    fs::create_dir_all(config.store_root.join("logs"))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.store_root.join("logs/serve.log"))?;
    let _subscriber = tracing_subscriber::fmt()
        .with_writer(log.with_max_level(tracing::Level::INFO))
        .with_ansi(false)
        .try_init();

    let _ = fs::remove_file(sock_path(&config.store_root));
    fs::write(pid_path(&config.store_root), std::process::id().to_string())?;
    let listener = UnixListener::bind(sock_path(&config.store_root))?;
    listener.set_nonblocking(true)?;

    let term = Arc::new(AtomicBool::new(false));
    flag::register(SIGTERM, Arc::clone(&term))?;
    tracing::info!("orcr daemon started");
    let _ = reconcile(&config, false);

    while !term.load(Ordering::Relaxed) {
        accept_pings(&listener)?;
        supervise_once(&config)?;
        thread::sleep(Duration::from_secs(1));
    }
    let _ = fs::remove_file(sock_path(&config.store_root));
    let _ = fs::remove_file(pid_path(&config.store_root));
    tracing::info!("orcr daemon stopped");
    Ok(())
}

fn accept_pings(listener: &UnixListener) -> Result<()> {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0_u8; 16];
                let n = stream.read(&mut buf).unwrap_or(0);
                if &buf[..n] == b"ping\n" {
                    let _ = stream.write_all(b"pong\n");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn supervise_once(config: &Config) -> Result<()> {
    let mut store = Store::open(&config.store_root)?;
    promote_queued(config, &mut store)?;
    let now = Utc::now().to_rfc3339();
    let jobs = store.list_due_jobs(&now)?;
    for mut job in jobs {
        if let Err(error) = match job.job_type.as_str() {
            "loop" => supervise_loop_job(config, &mut store, &mut job),
            "schedule" => supervise_schedule_job(config, &mut store, &mut job),
            _ => continue,
        } {
            append_job_event(
                &store,
                "job.tick.failed",
                &job.id,
                serde_json::json!({"error": error.to_string()}),
            )?;
            job.status = "failed".to_string();
            job.ended_reason = Some("tick_failed".to_string());
            job.next_run_at = None;
            store.update_job(&job)?;
        }
    }
    Ok(())
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct ReconcileReport {
    pub killed_unknown_panes: Vec<String>,
    pub marked_lost: Vec<String>,
    pub deleted_stale_sessions: Vec<String>,
    pub readmitted_queued: Vec<String>,
    pub failed_queued: Vec<String>,
    pub dry_run: bool,
}

pub fn reconcile(config: &Config, dry_run: bool) -> Result<ReconcileReport> {
    let mut store = Store::open(&config.store_root)?;
    let herdr_bin = discover_herdr(&config.herdr.bin)?;
    let herdr = HerdrClient::new(herdr_bin, config.herdr.session.clone());
    let agents = store.list_agents()?;
    let live_panes = herdr.pane_list().map(|list| list.panes).unwrap_or_default();
    let known_panes: std::collections::HashSet<String> =
        agents.iter().filter_map(|a| a.pane_id.clone()).collect();
    let live_pane_ids: std::collections::HashSet<String> =
        live_panes.iter().map(|p| p.pane_id.clone()).collect();
    let mut report = ReconcileReport {
        dry_run,
        ..ReconcileReport::default()
    };

    for pane in live_panes {
        if pane
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with('a'))
            && !known_panes.contains(&pane.pane_id)
        {
            if !dry_run {
                let _ = herdr.pane_close(&pane.pane_id);
                append_reconcile_event(
                    &store,
                    "reconcile.pane.killed",
                    Some(&pane.pane_id),
                    serde_json::json!({"pane_id": pane.pane_id}),
                )?;
            }
            report.killed_unknown_panes.push(pane.pane_id);
        }
    }

    for agent in &agents {
        if matches!(
            agent.status.as_str(),
            "working" | "idle" | "blocked" | "starting"
        ) && agent
            .pane_id
            .as_ref()
            .is_some_and(|pane| !live_pane_ids.contains(pane))
        {
            if !dry_run {
                store.update_agent_status(
                    &agent.id,
                    "lost",
                    Some("pane_gone"),
                    Some(&Utc::now().to_rfc3339()),
                )?;
                store.clear_agent_pane(&agent.id)?;
                append_reconcile_event(
                    &store,
                    "reconcile.agent.lost",
                    Some(&agent.id),
                    serde_json::json!({"pane_id": agent.pane_id}),
                )?;
            }
            report.marked_lost.push(agent.id.clone());
        }
    }

    for agent in agents.iter().filter(|agent| agent.status == "queued") {
        if store.get_queued_run(&agent.id)?.is_some() {
            report.readmitted_queued.push(agent.id.clone());
            if !dry_run {
                append_reconcile_event(
                    &store,
                    "reconcile.agent.queued",
                    Some(&agent.id),
                    serde_json::json!({}),
                )?;
            }
        } else {
            report.failed_queued.push(agent.id.clone());
            if !dry_run {
                store.update_agent_status(
                    &agent.id,
                    "failed",
                    Some("missing_queue_spec"),
                    Some(&Utc::now().to_rfc3339()),
                )?;
                append_reconcile_event(
                    &store,
                    "reconcile.agent.failed",
                    Some(&agent.id),
                    serde_json::json!({"reason": "missing_queue_spec"}),
                )?;
            }
        }
    }

    if let Ok(sessions) = herdr.session_list() {
        for session in sessions.sessions {
            if session.name == config.herdr.session && !session.running {
                if !dry_run {
                    let _ = herdr.session_delete(&session.name);
                    append_reconcile_event(
                        &store,
                        "reconcile.session.deleted",
                        Some(&session.name),
                        serde_json::json!({"session": session.name}),
                    )?;
                }
                report.deleted_stale_sessions.push(session.name);
            }
        }
    }
    if !dry_run {
        promote_queued(config, &mut store)?;
    }
    Ok(report)
}

fn supervise_loop_job(config: &Config, store: &mut Store, job: &mut JobRow) -> Result<()> {
    let mut spec: LoopSpec = serde_json::from_str(&job.spec_json)?;
    if spec.tick_on.is_some() && !tick_on_fires(&mut spec)? {
        job.spec_json = serde_json::to_string(&spec)?;
        let next = Utc::now() + ChronoDuration::seconds(i64::try_from(TICK_ON_POLL_SECS)?);
        let fallback = job
            .next_run_at
            .as_deref()
            .and_then(|s| parse_rfc3339_utc(s).ok());
        job.next_run_at = Some(
            match fallback {
                Some(fallback) if fallback < next => fallback,
                _ => next,
            }
            .to_rfc3339(),
        );
        store.update_job(job)?;
        return Ok(());
    }
    job.spec_json = serde_json::to_string(&spec)?;
    store.update_job(job)?;
    let herdr_bin = discover_herdr(&config.herdr.bin)?;
    let herdr = HerdrClient::new(herdr_bin, config.herdr.session.clone());
    run_loop_tick(config, store, herdr, job)
}

fn supervise_schedule_job(config: &Config, store: &mut Store, job: &mut JobRow) -> Result<()> {
    let spec: ScheduleSpec = serde_json::from_str(&job.spec_json)?;
    if let Some(expires_at) = job
        .expires_at
        .as_deref()
        .and_then(|s| parse_rfc3339_utc(s).ok())
    {
        if Utc::now() >= expires_at {
            job.status = "done".to_string();
            job.ended_reason = Some("expired".to_string());
            job.next_run_at = None;
            store.update_job(job)?;
            append_job_event(store, "job.expired", &job.id, serde_json::json!({}))?;
            return Ok(());
        }
    }
    if let Some(max_duration_s) = spec.max_duration_s {
        let created = parse_rfc3339_utc(&job.created_at)?;
        if Utc::now().signed_duration_since(created).num_seconds() >= i64::try_from(max_duration_s)?
        {
            job.status = "done".to_string();
            job.ended_reason = Some("max_duration".to_string());
            job.next_run_at = None;
            store.update_job(job)?;
            return Ok(());
        }
    }
    if let Some(next) = job
        .next_run_at
        .as_deref()
        .and_then(|s| parse_rfc3339_utc(s).ok())
    {
        let now = Utc::now();
        if next < now - ChronoDuration::seconds(1) && spec.catchup == CatchupPolicy::Skip {
            if let crate::jobs::ScheduleTrigger::Cron { utc, .. } = &spec.trigger {
                job.next_run_at = crate::jobs::next_cron_after(utc, now).map(|dt| dt.to_rfc3339());
                job.spec_json = serde_json::to_string(&spec)?;
                store.update_job(job)?;
                return Ok(());
            }
        }
    }
    job.spec_json = serde_json::to_string(&spec)?;
    store.update_job(job)?;
    let herdr_bin = discover_herdr(&config.herdr.bin)?;
    let herdr = HerdrClient::new(herdr_bin, config.herdr.session.clone());
    run_schedule_tick(config, store, herdr, job)
}

fn promote_queued(config: &Config, store: &mut Store) -> Result<()> {
    let herdr_bin = discover_herdr(&config.herdr.bin)?;
    let herdr = HerdrClient::new(herdr_bin, config.herdr.session.clone());
    loop {
        if store.count_active_agents()? >= config.limits.max_concurrent {
            return Ok(());
        }
        let Some(agent) = store.first_queued_agent()? else {
            return Ok(());
        };
        let Some(profile) = profile::lookup(&agent.harness) else {
            store.update_agent_status(
                &agent.id,
                "failed",
                Some("unknown_harness"),
                Some(&Utc::now().to_rfc3339()),
            )?;
            continue;
        };
        {
            let mut engine = Engine::new(config, store, herdr.clone());
            let _ = engine.promote_queued(&agent.id, profile.as_ref(), true)?;
        }
    }
}

fn append_reconcile_event(
    store: &Store,
    kind: &str,
    ref_id: Option<&str>,
    payload: serde_json::Value,
) -> Result<()> {
    store.append_event(&EventRow::new(
        Utc::now().to_rfc3339(),
        kind,
        ref_id.map(ToString::to_string),
        payload.to_string(),
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn daemon_paths_and_status_without_daemon() {
        let temp = tempdir().unwrap();
        assert_eq!(pid_path(temp.path()), temp.path().join("serve.pid"));
        assert_eq!(sock_path(temp.path()), temp.path().join("serve.sock"));
        assert_eq!(
            status(temp.path()),
            DaemonStatus {
                running: false,
                pid: None
            }
        );
    }
}
