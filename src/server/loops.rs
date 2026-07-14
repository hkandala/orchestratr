//! The loop scheduler (spec §6.2, §11.3): durable cron over any argv command, surviving the
//! caller's shell and machine reboots.
//!
//! Runtime model: one tick thread (`timings.loop_tick`) fires due loops, reaps finished runs,
//! and enforces per-run timeouts. Each started run executes in its **own process group**
//! (`setsid`) with the §5.3 env contract; pid/pgid **plus the OS process start time** are
//! recorded so a kill or recovery only ever signals a pgid whose start time still matches (pids
//! get reused). stdout/stderr are captured line-tagged to a rotated JSONL `run.log`. Every
//! scheduler action is an event row — that's `loop logs --source orcr`. Restart recovery is a
//! per-loop pass: dead runs are closed out (their agents glob-killed), pending fires decided
//! once, and missed cron fires skipped-and-logged (never replayed).

use super::params::{str_array_param, str_param};
use super::Server;
use crate::cron::{self, Cron};
use crate::error::{OrcrError, Result};
use crate::store::{now_millis, LoopRow, LoopRunRow, RunAllocation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The persisted loop definition payload (`<loop data dir>/loop.json`, spec §12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopPayload {
    pub version: u32,
    pub uuid: String,
    pub name: String,
    pub argv: Vec<String>,
    pub cadence_kind: String,
    pub cadence_value: String,
    pub tz: String,
    pub cwd: String,
    pub max_concurrency: i64,
    pub overlap: String,
    pub timeout_s: Option<i64>,
    pub created_at: i64,
}

impl Server {
    /// Start the loop scheduler tick thread (spec §11.3).
    pub(super) fn start_loop_scheduler(&self) {
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                server.loop_tick();
                std::thread::sleep(server.inner.config.timings.loop_tick);
            }
        });
    }

    fn loop_tick(&self) {
        self.enforce_run_timeouts();
        self.fire_due_loops();
    }

    // --- firing (spec §11.3) ---

    fn fire_due_loops(&self) {
        let now = now_millis();
        let due = {
            let store = self.inner.store.lock().unwrap();
            store.loops_due(now).unwrap_or_default()
        };
        for l in due {
            let due_at = l.next_fire_at.unwrap_or(now);
            if let Err(e) = self.fire_loop(&l, due_at, "scheduled") {
                self.log()
                    .warn(format!("loop {}: fire failed: {e}", l.name));
            }
        }
    }

    /// Fire a loop once (scheduled or manual). Allocates the run row transactionally, advances
    /// the schedule (for scheduled fires), and starts the process if a slot was free. Returns
    /// the allocation for the manual (`loop run start`) caller.
    pub(super) fn fire_loop(&self, l: &LoopRow, due_at: i64, kind: &str) -> Result<RunAllocation> {
        let (alloc, ev) = {
            let mut store = self.inner.store.lock().unwrap();
            store.allocate_run(&l.uuid, kind, due_at, l.max_concurrency, &l.overlap)?
        };
        self.publish(ev);

        // Advance the schedule for scheduled fires: a `once` loop ends after firing; a cron
        // loop recomputes its next occurrence.
        if kind == "scheduled" {
            self.advance_schedule(l);
        }

        if let RunAllocation::Allocated {
            run,
            start_now: true,
        } = &alloc
        {
            self.spawn_run(l, run);
        }
        Ok(alloc)
    }

    /// Recompute `next_fire_at` after a scheduled fire (or end a `once` loop).
    fn advance_schedule(&self, l: &LoopRow) {
        let now = now_millis();
        let mut store = self.inner.store.lock().unwrap();
        let _ = store.set_last_fire(&l.uuid, now);
        if l.cadence_kind == "once" {
            let ev = store
                .set_loop_status(&l.uuid, "ended", Some("fired"), "loop.ended")
                .unwrap_or(0);
            drop(store);
            self.publish(ev);
            return;
        }
        let next = self.compute_next_fire(&l.cadence_value, &l.tz, now);
        let _ = store.set_next_fire(&l.uuid, next);
    }

    /// The next UTC-ms fire strictly after `after`, or `None` if the cron never fires again.
    pub(super) fn compute_next_fire(&self, cadence: &str, tz: &str, after: i64) -> Option<i64> {
        let cron = Cron::parse(cadence).ok()?;
        let tz = cron::tz_from_name(tz);
        let after = chrono::DateTime::from_timestamp_millis(after)?;
        cron.next_after(after, tz).map(|d| d.timestamp_millis())
    }

    // --- run process (spec §6.2, §11.3, §5.3) ---

    /// Spawn a run's command in its own process group with the §5.3 env contract, capturing
    /// output to a rotated JSONL `run.log`, and record its process identity. A monitor thread
    /// reaps the process and finalizes the run.
    fn spawn_run(&self, l: &LoopRow, run: &LoopRunRow) {
        let run_path = format!("{}/{}", l.name, run.run_id);
        let loop_data_dir = self.loop_data_dir(&l.name);
        let run_dir = loop_data_dir.join(&run.run_id);
        if let Err(e) = std::fs::create_dir_all(&run_dir) {
            self.log()
                .warn(format!("loop {}: cannot create run dir: {e}", l.name));
            self.finalize_failed(run, "failed");
            return;
        }

        // The argv comes from the persisted loop payload (never re-parsed from the store).
        let payload = match self.read_loop_payload(&l.name) {
            Ok(p) => p,
            Err(e) => {
                self.log()
                    .warn(format!("loop {}: cannot read loop.json: {e}", l.name));
                self.finalize_failed(run, "failed");
                return;
            }
        };
        if payload.argv.is_empty() {
            self.log().warn(format!("loop {}: empty argv", l.name));
            self.finalize_failed(run, "failed");
            return;
        }

        let run_log = Arc::new(Mutex::new(RunLog::new(
            run_dir.join("run.log"),
            self.inner.config.logs.max_bytes,
            self.inner.config.logs.max_files,
        )));

        let mut cmd = Command::new(&payload.argv[0]);
        cmd.args(&payload.argv[1..]);
        cmd.current_dir(&l.cwd);
        // §5.3 env contract for a loop-run command (parentless; not an agent).
        cmd.env("ORCR_ID", &run.uuid);
        cmd.env("ORCR_PATH", &run_path);
        cmd.env("ORCR_LOOP_DATA_DIR", &loop_data_dir);
        cmd.env("ORCR_HOME", self.inner.home.root());
        cmd.env_remove("ORCR_PARENT_ID");
        cmd.env_remove("ORCR_PARENT_PATH");
        cmd.env_remove("ORCR_AGENT_DATA_DIR");
        cmd.env_remove("ORCR_LAUNCH_TOKEN");
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // Own process group: setsid makes the child a session + group leader (pgid == pid).
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                run_log
                    .lock()
                    .unwrap()
                    .append("stderr", &format!("failed to spawn command: {e}"));
                self.log()
                    .warn(format!("loop {}: spawn failed: {e}", l.name));
                self.finalize_failed(run, "failed");
                return;
            }
        };
        let pid = child.id() as i64;
        let start_time = os_process_start_time(pid).unwrap_or_else(now_millis);
        let timeout_at = l.timeout_s.map(|s| now_millis() + s * 1000);

        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store
                .record_run_start(&run.uuid, pid, pid, start_time, now_millis(), timeout_at)
                .unwrap_or(0)
        };
        // record_run_start only fills pid/pgid `WHERE status IN ('running','stopping')`; a 0
        // return means the row was already terminal (a stop/rm — e.g. `loop rm --kill-active`
        // — raced the spawn while pgid was still NULL, so the stop couldn't signal us). The
        // child would otherwise run unmanaged forever, so kill its process group and bail.
        if ev == 0 {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            self.log().warn(format!(
                "loop {}: run {} raced a stop during spawn — killed orphan pgid {pid}",
                l.name, run.run_id
            ));
            return;
        }
        self.publish(ev);
        self.log().info(format!(
            "loop {}: run {} started pid={pid}",
            l.name, run.run_id
        ));

        // Capture stdout/stderr line-tagged into run.log.
        if let Some(out) = child.stdout.take() {
            spawn_capture(out, run_log.clone(), "stdout");
        }
        if let Some(err) = child.stderr.take() {
            spawn_capture(err, run_log.clone(), "stderr");
        }

        // Monitor: wait for exit, then finalize (unless a stop/timeout path already owns it).
        let server = self.clone();
        let run_uuid = run.uuid.clone();
        let loop_uuid = l.uuid.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            server.finalize_on_exit(&run_uuid, &loop_uuid, status);
        });
    }

    /// Finalize a run when its process exits on its own (not via stop/timeout). If the run is
    /// already `stopping` (a stop/timeout path owns it) or terminal, this is a no-op.
    fn finalize_on_exit(
        &self,
        run_uuid: &str,
        loop_uuid: &str,
        status: std::io::Result<std::process::ExitStatus>,
    ) {
        let cur = {
            let store = self.inner.store.lock().unwrap();
            store.run_by_uuid(run_uuid).ok().flatten()
        };
        let Some(cur) = cur else { return };
        if cur.status != "running" {
            // A stop/timeout path is finalizing (stopping), or the run is already terminal.
            if cur.status != "stopping" {
                self.promote_pending(loop_uuid);
            }
            return;
        }
        let (final_status, code, signal) = match status {
            Ok(es) if es.success() => ("ok", Some(0i64), None),
            Ok(es) => {
                use std::os::unix::process::ExitStatusExt;
                match es.signal() {
                    Some(sig) => ("failed", None, Some(sig as i64)),
                    None => ("failed", es.code().map(|c| c as i64), None),
                }
            }
            Err(_) => ("failed", None, None),
        };
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store
                .finish_run(run_uuid, final_status, code, signal)
                .unwrap_or(0)
        };
        self.publish(ev);
        self.promote_pending(loop_uuid);
    }

    fn finalize_failed(&self, run: &LoopRunRow, status: &str) {
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.finish_run(&run.uuid, status, None, None).unwrap_or(0)
        };
        self.publish(ev);
        self.promote_pending(&run.loop_uuid);
    }

    /// Start the oldest pending run of a loop if a slot is now free (spec §11.3). The slot is
    /// **reserved atomically** in one transaction (`claim_pending_run`) before we release the
    /// store lock and spawn, so two concurrent promoters (exit-monitor threads, the resume/stop
    /// paths, recovery) can never claim the same slot and double-spawn a run.
    fn promote_pending(&self, loop_uuid: &str) {
        let (loop_row, claimed) = {
            let mut store = self.inner.store.lock().unwrap();
            let loop_row = store.loop_by_uuid(loop_uuid).ok().flatten();
            let claimed = match &loop_row {
                // Only an `active` loop promotes queued runs: a `paused` loop holds its queue
                // (fires + pending runs resume only on `loop resume`, §6.2), and an `ended`
                // loop never starts new runs.
                Some(l) if l.status == "active" => store
                    .claim_pending_run(loop_uuid, l.max_concurrency)
                    .ok()
                    .flatten(),
                _ => None,
            };
            (loop_row, claimed)
        };
        if let (Some(l), Some(run)) = (loop_row, claimed) {
            self.spawn_run(&l, &run);
        }
    }

    // --- timeout + stop (spec §6.2, §11.3) ---

    fn enforce_run_timeouts(&self) {
        let due = {
            let store = self.inner.store.lock().unwrap();
            store.timed_out_runs(now_millis()).unwrap_or_default()
        };
        for run in due {
            let loop_row = {
                let store = self.inner.store.lock().unwrap();
                store.loop_by_uuid(&run.loop_uuid).ok().flatten()
            };
            if let Some(l) = loop_row {
                self.log()
                    .warn(format!("loop {}: run {} timed out", l.name, run.run_id));
                // Enter the `stopping` barrier synchronously (fast, under the store lock) so the
                // next tick's `timed_out_runs` no longer selects this run, then dispatch the
                // blocking TERM→grace→KILL onto its own thread. This keeps the shared scheduler
                // tick free to fire other loops and enforce other timeouts during the grace
                // period instead of stalling for `run_term_grace` (spec §11.3).
                self.enter_stop_barrier(&run);
                let server = self.clone();
                std::thread::spawn(move || {
                    server.finish_stop(&l, &run, "timeout");
                });
            }
        }
    }

    /// Stop a run's process group: `stopping` barrier → TERM → grace → KILL → glob-kill the
    /// run's agents until a clean snapshot → finalize with `terminal_status` (spec §6.2,
    /// §11.3). `terminal_status` is `stopped` (manual/`loop run stop`) or `timeout`. This is
    /// synchronous (it blocks its caller for the grace period); the manual `loop run stop`
    /// handler runs off the scheduler tick, so that is by design. The timeout path instead
    /// enters the barrier then dispatches [`Server::finish_stop`] to a thread.
    pub(super) fn stop_run_process(&self, l: &LoopRow, run: &LoopRunRow, terminal_status: &str) {
        self.enter_stop_barrier(run);
        self.finish_stop(l, run, terminal_status);
    }

    /// Enter the `stopping` admission barrier so descendant `agent run`s are rejected from here
    /// on (no-op if the run is not `running`). Publishes the `loop_run.stopping` event.
    fn enter_stop_barrier(&self, run: &LoopRunRow) {
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.set_run_stopping(&run.uuid).unwrap_or(0)
        };
        self.publish(ev);
    }

    /// Finish a stop after the barrier is entered: signal the process group, wait the grace
    /// period, KILL if still alive, glob-kill the run's agents until clean, finalize, and
    /// promote the next pending run (spec §6.2, §11.3).
    fn finish_stop(&self, l: &LoopRow, run: &LoopRunRow, terminal_status: &str) {
        // Re-read the run's pid/pgid immediately before signaling: a stop can race the spawn
        // window (allocate_run commits `running` with pgid=NULL, then `record_run_start` fills
        // pid/pgid — even from the `stopping` barrier), so the struct captured by the caller may
        // predate the recorded pgid. Signaling the fresh row (and re-reading again after the
        // grace sleep) ensures a pgid recorded after the barrier is still TERM/KILLed (§11.3).
        let fresh = self.reread_run(run);
        self.signal_run(&fresh, libc::SIGTERM);
        std::thread::sleep(self.inner.config.timings.run_term_grace);
        let fresh = self.reread_run(run);
        if self.run_leader_alive(&fresh) {
            self.signal_run(&fresh, libc::SIGKILL);
        }

        // Barrier glob-kill of the run's agents until a final snapshot shows none.
        let run_path = format!("{}/{}", l.name, run.run_id);
        self.glob_kill_run_agents(&run_path);

        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store
                .finish_run(&run.uuid, terminal_status, None, None)
                .unwrap_or(0)
        };
        self.publish(ev);
        self.log().info(format!(
            "loop {}: run {} {terminal_status}",
            l.name, run.run_id
        ));
        self.promote_pending(&l.uuid);
    }

    /// Glob-kill every active agent under `<loop>/<run_id>/**`, looping until clean (§6.2).
    pub(super) fn glob_kill_run_agents(&self, run_path: &str) {
        let pattern = format!("{run_path}/**");
        for _ in 0..100 {
            let params = json!({ "targets": [pattern], "force": true });
            match self.handle_agent_kill(&params) {
                Ok(r) => {
                    let killed = r["killed"].as_array().map(|a| a.len()).unwrap_or(0);
                    if killed == 0 {
                        break;
                    }
                }
                // not_found = nothing matched; the subtree is clean.
                Err(_) => break,
            }
        }
    }

    /// Re-read the current run row from the store, falling back to the caller's copy if the
    /// row can't be fetched — used so a stop honors a pgid recorded after the barrier.
    fn reread_run(&self, run: &LoopRunRow) -> LoopRunRow {
        let store = self.inner.store.lock().unwrap();
        store
            .run_by_uuid(&run.uuid)
            .ok()
            .flatten()
            .unwrap_or_else(|| run.clone())
    }

    /// Signal a run's process group, guarded by the recorded start time (spec §11.3: signal
    /// only a pgid whose leader start time matches — pids get reused).
    fn signal_run(&self, run: &LoopRunRow, sig: libc::c_int) -> bool {
        let (Some(pgid), true) = (run.pgid, self.run_leader_alive(run)) else {
            return false;
        };
        // Negative pid targets the whole process group.
        let _ = unsafe { libc::kill(-(pgid as i32), sig) };
        true
    }

    /// Whether the run's process-group leader is alive with a matching start time.
    fn run_leader_alive(&self, run: &LoopRunRow) -> bool {
        let Some(pid) = run.pid else { return false };
        if unsafe { libc::kill(pid as i32, 0) } != 0 {
            return false;
        }
        match (os_process_start_time(pid), run.pgid_start_time) {
            (Some(cur), Some(rec)) => cur == rec,
            // Can't compare start times on this platform — best-effort (alive is enough).
            _ => true,
        }
    }

    // --- restart recovery (spec §11.3) ---

    /// Per-loop restart recovery: verify running pgids (dead → closed out + agents glob-killed),
    /// recompute the active count, honor paused/ended, decide pending fires once, and recompute
    /// `next_fire_at` — skipping (and logging) fires missed while the server was down.
    pub(super) fn recover_loops_on_start(&self) {
        let now = now_millis();
        let loops = {
            let store = self.inner.store.lock().unwrap();
            store.all_loops().unwrap_or_default()
        };
        for l in loops {
            // Close out dead running/stopping runs; keep alive orphans under a poll monitor.
            // active_runs already filters to running/stopping — exactly the ones to verify.
            let active_runs = {
                let store = self.inner.store.lock().unwrap();
                store.active_runs(&l.uuid).unwrap_or_default()
            };
            for run in active_runs {
                if self.run_leader_alive(&run) {
                    if run.status == "stopping" {
                        // Mid-stop when the server crashed (barrier already `stopping`, but
                        // `finish_stop` was interrupted before `finish_run`). Re-drive the stop
                        // on a thread so the run reaches a terminal state instead of being left
                        // `stopping` forever (spec §11.3).
                        let server = self.clone();
                        let l2 = l.clone();
                        let run2 = run.clone();
                        std::thread::spawn(move || {
                            server.finish_stop(&l2, &run2, "stopped");
                        });
                        self.log().info(format!(
                            "recover: loop {} run {} was mid-stop — re-driving stop",
                            l.name, run.run_id
                        ));
                    } else {
                        // Survived the restart (orphaned): keep it and poll for its exit.
                        self.spawn_poll_monitor(&run, &l.uuid);
                        self.log().info(format!(
                            "recover: loop {} run {} still alive — monitoring",
                            l.name, run.run_id
                        ));
                    }
                } else {
                    // Dead → close out and glob-kill its agents (spec §11.3).
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store
                            .finish_run(&run.uuid, "failed", None, None)
                            .unwrap_or(0)
                    };
                    self.publish(ev);
                    let run_path = format!("{}/{}", l.name, run.run_id);
                    self.glob_kill_run_agents(&run_path);
                    self.log().info(format!(
                        "recover: loop {} run {} was dead — closed out",
                        l.name, run.run_id
                    ));
                }
            }

            if l.status == "ended" {
                continue;
            }

            // Recompute the schedule: any fire due while we were down is skipped-and-logged;
            // never replayed (spec §6.2, §11.3) — for cron AND once loops alike.
            if let Some(nf) = l.next_fire_at {
                if nf <= now {
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store
                            .append_event(
                                "loop.skipped",
                                Some(&l.uuid),
                                &json!({
                                    "loop_uuid": l.uuid, "name": l.name,
                                    "reason": "missed_while_down", "due_at": nf,
                                }),
                            )
                            .unwrap_or(0)
                    };
                    self.publish(ev);
                    if l.cadence_kind == "once" {
                        // A once loop fires exactly once; a missed fire is skipped, so the
                        // definition ends without ever running (spec §6.2 "fires once then ends").
                        let ev = {
                            let mut store = self.inner.store.lock().unwrap();
                            store
                                .set_loop_status(&l.uuid, "ended", Some("fired"), "loop.ended")
                                .unwrap_or(0)
                        };
                        self.publish(ev);
                    } else {
                        let next = self.compute_next_fire(&l.cadence_value, &l.tz, now);
                        let mut store = self.inner.store.lock().unwrap();
                        let _ = store.set_next_fire(&l.uuid, next);
                    }
                    self.log().info(format!(
                        "recover: loop {} missed a fire while down — skipped",
                        l.name
                    ));
                }
            }

            // Decide any pending run exactly once now that the active count is recomputed.
            if l.status == "active" {
                self.promote_pending(&l.uuid);
            }
        }
    }

    /// Poll a recovered (orphaned) run's leader until it dies, then finalize it (exit code
    /// unknown → `failed`). Used only for runs that survived a server restart.
    fn spawn_poll_monitor(&self, run: &LoopRunRow, loop_uuid: &str) {
        let server = self.clone();
        let run = run.clone();
        let loop_uuid = loop_uuid.to_string();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                if !server.run_leader_alive(&run) {
                    let cur = {
                        let store = server.inner.store.lock().unwrap();
                        store.run_by_uuid(&run.uuid).ok().flatten()
                    };
                    if let Some(cur) = cur {
                        // Finalize whether the orphan was `running` (unknown exit → failed) or
                        // `stopping` (a stop was in flight → stopped) so it can never be left
                        // un-finalized (spec §11.3).
                        let terminal = match cur.status.as_str() {
                            "running" => Some("failed"),
                            "stopping" => Some("stopped"),
                            _ => None,
                        };
                        if let Some(ts) = terminal {
                            let ev = {
                                let mut store = server.inner.store.lock().unwrap();
                                store.finish_run(&run.uuid, ts, None, None).unwrap_or(0)
                            };
                            server.publish(ev);
                        }
                    }
                    server.promote_pending(&loop_uuid);
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    // --- helpers ---

    /// The loop's data dir: `$ORCR_HOME/data/<loop_name>` (shared across runs, §5.3).
    pub(super) fn loop_data_dir(&self, loop_name: &str) -> PathBuf {
        self.inner.home.data_dir().join(loop_name)
    }

    /// Read the persisted loop payload (argv etc.) from `<loop data dir>/loop.json`.
    pub(super) fn read_loop_payload(&self, loop_name: &str) -> Result<LoopPayload> {
        let file = self.loop_data_dir(loop_name).join("loop.json");
        let text = std::fs::read_to_string(&file).map_err(|e| {
            OrcrError::server_error(
                "loop_payload",
                format!("cannot read {}: {e}", file.display()),
            )
        })?;
        serde_json::from_str(&text)
            .map_err(|e| OrcrError::server_error("loop_payload", format!("bad loop.json: {e}")))
    }
}

impl Server {
    // --- loop verb handlers (spec §6.2) ---

    /// `loop.create` (spec §6.2): validate name/cadence/payload, persist `loop.json`, insert
    /// the durable definition, and echo the parsed argv + cadence + cancel command.
    pub(super) fn handle_loop_create(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name").ok_or_else(|| {
            OrcrError::invalid_request("loop create requires a name", "name_required")
        })?;
        // A loop name is one segment, root-level, never a reserved level-1 name (§5.1, §6.2).
        let name = crate::path::expand_rand(&name);
        if !crate::path::valid_segment(&name) {
            return Err(OrcrError::invalid_request(
                format!("loop name `{name}` must be one segment ([a-z0-9_], 1-64 chars)"),
                "invalid_name",
            ));
        }
        if crate::path::RESERVED_LEVEL1.contains(&name.as_str()) {
            return Err(OrcrError::invalid_request(
                format!("`{name}` is a reserved name owned by orcr"),
                "reserved_name",
            )
            .with_details(json!({ "reason": "reserved_name", "name": name })));
        }

        let cron = str_param(params, "cron").filter(|s| !s.is_empty());
        let once_at = str_param(params, "once_at").filter(|s| !s.is_empty());
        let now = now_millis();
        let tz = cron::local_tz_name();
        let (cadence_kind, cadence_value, next_fire_at) = match (cron, once_at) {
            (Some(_), Some(_)) => {
                return Err(OrcrError::invalid_request(
                    "pass exactly one of a cron expression or --once-at",
                    "cadence_conflict",
                ))
            }
            (Some(expr), None) => {
                // Validate the cron up front (units + fields, §6.2).
                Cron::parse(&expr)?;
                let next = self.compute_next_fire(&expr, &tz, now);
                ("cron".to_string(), expr, next)
            }
            (None, Some(when)) => {
                let at = parse_once_at(&when, now)?;
                ("once".to_string(), when, Some(at))
            }
            (None, None) => {
                return Err(OrcrError::invalid_request(
                    "loop create requires a cron expression or --once-at",
                    "cadence_required",
                ))
            }
        };

        let command = str_array_param(params, "command");
        if command.is_empty() {
            return Err(OrcrError::invalid_request(
                "loop create requires a command after `--`",
                "command_required",
            ));
        }

        let max_concurrency = params
            .get("max_concurrency")
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        if max_concurrency < 1 {
            return Err(OrcrError::invalid_request(
                "--max-concurrency must be >= 1",
                "invalid_max_concurrency",
            ));
        }
        let overlap = str_param(params, "overlap").unwrap_or_else(|| "queue".to_string());
        if !matches!(overlap.as_str(), "queue" | "skip") {
            return Err(OrcrError::invalid_request(
                format!("invalid --overlap `{overlap}` (queue|skip)"),
                "invalid_overlap",
            ));
        }
        let timeout_s = match str_param(params, "timeout").filter(|s| !s.is_empty()) {
            Some(t) => Some((crate::duration::parse_duration(&t)?.as_millis() as i64) / 1000),
            None => None,
        };
        let cwd = str_param(params, "cwd")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string());

        let uuid = uuid::Uuid::now_v7().to_string();

        // Persist the loop payload before the durable row (audit/recovery, §12).
        let data_dir = self.loop_data_dir(&name);
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            OrcrError::server_error("loop_data_dir", format!("cannot create loop data dir: {e}"))
        })?;
        let payload = LoopPayload {
            version: 1,
            uuid: uuid.clone(),
            name: name.clone(),
            argv: command.clone(),
            cadence_kind: cadence_kind.clone(),
            cadence_value: cadence_value.clone(),
            tz: tz.clone(),
            cwd: cwd.clone(),
            max_concurrency,
            overlap: overlap.clone(),
            timeout_s,
            created_at: now,
        };
        std::fs::write(
            data_dir.join("loop.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .map_err(|e| {
            OrcrError::server_error("loop_data_dir", format!("cannot write loop.json: {e}"))
        })?;

        let new = crate::store::NewLoop {
            uuid: uuid.clone(),
            name: name.clone(),
            cadence_kind: cadence_kind.clone(),
            cadence_value: cadence_value.clone(),
            tz: tz.clone(),
            cwd,
            max_concurrency,
            overlap: overlap.clone(),
            timeout_s,
            next_fire_at,
            created_at: now,
        };
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.create_loop(&new)?
        };
        self.publish(ev);
        self.log().info(format!(
            "loop {name} created ({cadence_kind} {cadence_value})"
        ));

        Ok(json!({
            "loop": {
                "uuid": uuid,
                "name": name,
                "cadence": cadence_value,
                "cadence_kind": cadence_kind,
                "tz": tz,
                "next_fire_at": next_fire_at,
                "argv": command,
                "max_concurrency": max_concurrency,
                "overlap": overlap,
                "cancel": format!("orcr loop rm {name}"),
            }
        }))
    }

    /// `loop.pause` / `loop.resume` (spec §6.2): hold or release fires. On resume, a held
    /// pending run starts if due; `next_fire_at` is recomputed forward so a fire missed while
    /// paused is not replayed.
    pub(super) fn handle_loop_set_paused(&self, params: &Value, paused: bool) -> Result<Value> {
        let names = names_param(params)?;
        let mut updated = Vec::new();
        let mut skipped = Vec::new();
        for name in &names {
            let l = {
                let store = self.inner.store.lock().unwrap();
                store.find_loop_by_name(name)?
            };
            let Some(l) = l.filter(|l| l.status != "ended") else {
                skipped.push(json!({ "name": name, "reason": "not_found" }));
                continue;
            };
            let (target, kind) = if paused {
                ("paused", "loop.paused")
            } else {
                ("active", "loop.resumed")
            };
            let now = now_millis();
            let ev = {
                let mut store = self.inner.store.lock().unwrap();
                // On resume, recompute next fire forward (skip any missed-while-paused fire).
                if !paused && l.cadence_kind == "cron" {
                    let next = self.compute_next_fire(&l.cadence_value, &l.tz, now);
                    let _ = store.set_next_fire(&l.uuid, next);
                }
                store.set_loop_status(&l.uuid, target, None, kind)?
            };
            self.publish(ev);
            if !paused {
                self.promote_pending(&l.uuid);
            }
            updated.push(json!({ "name": name, "status": target }));
        }
        Ok(json!({ "updated": updated, "skipped": skipped }))
    }

    /// `loop.rm` (spec §6.2): end the definition (`removed` / `removed_by_run`). The active run
    /// and its agents continue unless `--kill-active`. History stays queryable.
    pub(super) fn handle_loop_rm(&self, params: &Value) -> Result<Value> {
        let names = names_param(params)?;
        let kill_active = params
            .get("kill_active")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Self-removal from inside a run → `removed_by_run` (spec §6.2).
        let caller_path = str_param(params, "caller_path");
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        for name in &names {
            let l = {
                let store = self.inner.store.lock().unwrap();
                store.find_loop_by_name(name)?
            };
            let Some(l) = l.filter(|l| l.status != "ended") else {
                skipped.push(json!({ "name": name, "reason": "not_found" }));
                continue;
            };
            let by_run = caller_path
                .as_deref()
                .map(|p| p.split('/').next() == Some(name.as_str()))
                .unwrap_or(false);
            let reason = if by_run { "removed_by_run" } else { "removed" };

            if kill_active {
                // Stop every active + pending run, killing their agents (spec §6.2).
                let runs = {
                    let store = self.inner.store.lock().unwrap();
                    store
                        .runs_for_loop(&l.uuid, None, false)
                        .unwrap_or_default()
                };
                for run in runs {
                    self.stop_or_cancel_run(&l, &run, "stopped");
                }
            }

            let ev = {
                let mut store = self.inner.store.lock().unwrap();
                store.set_loop_status(&l.uuid, "ended", Some(reason), "loop.removed")?
            };
            self.publish(ev);
            self.log().info(format!("loop {name} removed ({reason})"));
            removed.push(json!({ "name": name, "reason": reason }));
        }
        Ok(json!({ "removed": removed, "skipped": skipped }))
    }

    /// `loop.ls` (spec §6.2).
    pub(super) fn handle_loop_ls(&self, params: &Value) -> Result<Value> {
        let names = str_array_param(params, "names");
        let status = str_param(params, "status").filter(|s| !s.is_empty());
        let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
        let store = self.inner.store.lock().unwrap();
        let loops = store.list_loops(&names, status.as_deref(), all)?;
        let rows: Vec<Value> = loops.iter().map(loop_row_json).collect();
        Ok(json!({ "loops": rows }))
    }

    /// `loop.run.start` (spec §6.2): manual trigger (works on paused loops too). Prints
    /// `<loop_name>/<run_id> <run_uuid>`; at capacity the run sits pending.
    pub(super) fn handle_loop_run_start(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name").ok_or_else(|| {
            OrcrError::invalid_request("loop run start requires a name", "name_required")
        })?;
        let l = {
            let store = self.inner.store.lock().unwrap();
            store.find_loop_by_name(&name)?
        };
        let l = l
            .filter(|l| l.status != "ended")
            .ok_or_else(|| OrcrError::not_found(format!("no active loop named `{name}`")))?;
        let alloc = self.fire_loop(&l, now_millis(), "manual")?;
        let run = match alloc {
            RunAllocation::Allocated { run, start_now } => {
                let status = if start_now { "running" } else { "pending" };
                json!({
                    "uuid": run.uuid, "run_id": run.run_id,
                    "path": format!("{}/{}", l.name, run.run_id),
                    "loop": l.name, "kind": "manual", "status": status,
                })
            }
            _ => {
                return Err(OrcrError::server_error(
                    "loop",
                    "manual run did not allocate",
                ))
            }
        };
        Ok(json!({ "run": run }))
    }

    /// `loop.run.stop` (spec §6.2): stop run(s) without touching the definition. An optional
    /// `run` targets one run; otherwise all active + pending runs of the loop.
    pub(super) fn handle_loop_run_stop(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name").ok_or_else(|| {
            OrcrError::invalid_request("loop run stop requires a name", "name_required")
        })?;
        let l = {
            let store = self.inner.store.lock().unwrap();
            store.find_loop_by_name(&name)?
        };
        let l = l.ok_or_else(|| OrcrError::not_found(format!("no loop named `{name}`")))?;
        let run_sel = str_param(params, "run").filter(|s| !s.is_empty());

        let targets: Vec<LoopRunRow> = {
            let store = self.inner.store.lock().unwrap();
            match &run_sel {
                Some(sel) => match store.run_by_id_or_uuid(&l.uuid, sel)? {
                    Some(r) => vec![r],
                    None => {
                        return Err(OrcrError::not_found(format!(
                            "no run `{sel}` in loop `{name}`"
                        )))
                    }
                },
                None => store
                    .runs_for_loop(&l.uuid, None, false)
                    .unwrap_or_default(),
            }
        };

        let mut stopped = Vec::new();
        let mut skipped = Vec::new();
        for run in targets {
            match run.status.as_str() {
                "running" | "stopping" | "pending" => {
                    // Same dispatch as `loop rm --kill-active` (stop active / cancel pending);
                    // the handler adds the terminal row (`canceled` for a pending run).
                    let row_status = if run.status == "pending" {
                        "canceled"
                    } else {
                        "stopped"
                    };
                    self.stop_or_cancel_run(&l, &run, "stopped");
                    stopped.push(json!({
                        "run_id": run.run_id, "path": format!("{}/{}", l.name, run.run_id),
                        "status": row_status,
                    }));
                }
                other => {
                    skipped.push(
                        json!({ "run_id": run.run_id, "reason": "not_running", "status": other }),
                    );
                }
            }
        }
        Ok(json!({ "stopped": stopped, "skipped": skipped }))
    }

    /// `loop.run.ls` (spec §6.2).
    pub(super) fn handle_loop_run_ls(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name").ok_or_else(|| {
            OrcrError::invalid_request("loop run ls requires a name", "name_required")
        })?;
        let status = str_param(params, "status").filter(|s| !s.is_empty());
        let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
        let l = {
            let store = self.inner.store.lock().unwrap();
            store.find_loop_by_name(&name)?
        };
        let l = l.ok_or_else(|| OrcrError::not_found(format!("no loop named `{name}`")))?;
        let runs = {
            let store = self.inner.store.lock().unwrap();
            store.runs_for_loop(&l.uuid, status.as_deref(), all)?
        };
        let rows: Vec<Value> = runs.iter().map(|r| self.run_row_json(&l, r)).collect();
        Ok(json!({ "runs": rows }))
    }

    /// `loop.logs` (spec §6.2): interleave the runs' captured command output (`run.log`) with
    /// orcr's own scheduler actions (the event log), each line tagged with its run.
    pub(super) fn handle_loop_logs(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name").ok_or_else(|| {
            OrcrError::invalid_request("loop logs requires a name", "name_required")
        })?;
        let run_sel = str_param(params, "run").filter(|s| !s.is_empty());
        let source = str_param(params, "source").filter(|s| !s.is_empty());
        let tail = params
            .get("tail")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let l = {
            let store = self.inner.store.lock().unwrap();
            store.find_loop_by_name(&name)?
        };
        let l = l.ok_or_else(|| OrcrError::not_found(format!("no loop named `{name}`")))?;

        // Resolve the runs in scope (all history, so old runs' logs are reachable).
        let runs = {
            let store = self.inner.store.lock().unwrap();
            store.runs_for_loop(&l.uuid, None, true)?
        };
        let runs: Vec<LoopRunRow> = match &run_sel {
            Some(sel) => runs
                .into_iter()
                .filter(|r| &r.run_id == sel || &r.uuid == sel)
                .collect(),
            None => runs,
        };

        let mut lines: Vec<Value> = Vec::new();

        // Command output from each run's run.log (source=command).
        if source.as_deref() != Some("orcr") {
            for run in &runs {
                let file = self
                    .loop_data_dir(&l.name)
                    .join(&run.run_id)
                    .join("run.log");
                if let Ok(text) = std::fs::read_to_string(&file) {
                    for line in text.lines() {
                        if let Ok(rec) = serde_json::from_str::<Value>(line) {
                            lines.push(json!({
                                "run": format!("{}/{}", l.name, run.run_id),
                                "source": "command",
                                "ts": rec.get("ts").cloned().unwrap_or(Value::Null),
                                "stream": rec.get("stream").cloned().unwrap_or(Value::Null),
                                "text": rec.get("text").cloned().unwrap_or(Value::Null),
                            }));
                        }
                    }
                }
            }
        }

        // Scheduler actions from the event log (source=orcr).
        if source.as_deref() != Some("command") {
            let run_by_uuid: std::collections::HashMap<String, String> = runs
                .iter()
                .map(|r| (r.uuid.clone(), r.run_id.clone()))
                .collect();
            // Fetch only this loop's + its runs' events (indexed by ref_uuid) rather than
            // scanning the whole events table (all loop.* events ref the loop uuid; loop_run.*
            // ref the run uuid — spec §11.6). Retention-trimmed old events are not returned.
            let events = {
                let mut refs: Vec<&str> = vec![l.uuid.as_str()];
                refs.extend(runs.iter().map(|r| r.uuid.as_str()));
                let store = self.inner.store.lock().unwrap();
                store.events_for_refs(&refs)?
            };
            for ev in events {
                if !ev.kind.starts_with("loop") {
                    continue;
                }
                // Attribute the event to a run: loop_run.* reference the run uuid; loop.* the
                // loop uuid (with a run_id in the payload for fired/coalesced/skipped).
                let ref_uuid = ev.ref_uuid.clone().unwrap_or_default();
                let run_id = run_by_uuid.get(&ref_uuid).cloned().or_else(|| {
                    ev.payload
                        .get("run_id")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });
                // A --run filter drops scheduler lines not attributable to that run.
                if let Some(sel) = &run_sel {
                    match &run_id {
                        Some(rid) if rid == sel => {}
                        _ => continue,
                    }
                }
                let run_tag = run_id
                    .map(|r| format!("{}/{}", l.name, r))
                    .unwrap_or_else(|| l.name.clone());
                lines.push(json!({
                    "run": run_tag,
                    "source": "orcr",
                    "ts": ev.ts,
                    "text": format!("{}: {}", ev.kind, ev.payload),
                }));
            }
        }

        // Interleave chronologically, then apply --tail.
        lines.sort_by_key(|l| l.get("ts").and_then(|v| v.as_i64()).unwrap_or(0));
        if let Some(n) = tail {
            if lines.len() > n {
                lines = lines.split_off(lines.len() - n);
            }
        }
        Ok(json!({ "lines": lines }))
    }

    /// A `loop_runs` JSON row (spec §13). `agents` is derived: active agents under
    /// `<loop>/<run_id>/**`.
    fn run_row_json(&self, l: &LoopRow, r: &LoopRunRow) -> Value {
        let run_path = format!("{}/{}", l.name, r.run_id);
        let agents = self.run_agent_count(&run_path);
        json!({
            "uuid": r.uuid,
            "loop_uuid": r.loop_uuid,
            "run_id": r.run_id,
            "path": run_path,
            "kind": r.kind,
            "status": r.status,
            "due_at": r.due_at,
            "created_at": r.created_at,
            "started_at": r.started_at,
            "ended_at": r.ended_at,
            "exit_code": r.exit_code,
            "signal": r.signal,
            "pid": r.pid,
            "pgid": r.pgid,
            "agents": agents,
        })
    }

    /// Count active agents under a run's subtree (`<loop>/<run_id>/**`, spec §12 derived).
    fn run_agent_count(&self, run_path: &str) -> usize {
        let filter = crate::store::AgentFilter {
            pattern: Some(format!("{run_path}/**")),
            include_ended: false,
            ..Default::default()
        };
        let store = self.inner.store.lock().unwrap();
        store.list_agents(&filter).map(|v| v.len()).unwrap_or(0)
    }

    /// Stop a run if active, or cancel it if still pending (used by `loop rm --kill-active`).
    fn stop_or_cancel_run(&self, l: &LoopRow, run: &LoopRunRow, terminal_status: &str) {
        match run.status.as_str() {
            "running" | "stopping" => self.stop_run_process(l, run, terminal_status),
            "pending" => {
                let ev = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.cancel_pending_run(&run.uuid).unwrap_or(0)
                };
                self.publish(ev);
            }
            _ => {}
        }
    }
}

/// The `names` array param (bulk loop verbs), erroring if empty.
fn names_param(params: &Value) -> Result<Vec<String>> {
    let names = str_array_param(params, "names");
    if names.is_empty() {
        return Err(OrcrError::invalid_request(
            "at least one loop name is required",
            "name_required",
        ));
    }
    Ok(names)
}

/// A `loops` JSON row (spec §13).
pub(super) fn loop_row_json(l: &LoopRow) -> Value {
    json!({
        "uuid": l.uuid,
        "name": l.name,
        "status": l.status,
        "ended_reason": l.ended_reason,
        "cadence": l.cadence_value,
        "cadence_kind": l.cadence_kind,
        "tz": l.tz,
        "next_fire_at": l.next_fire_at,
        "last_fire_at": l.last_fire_at,
        "max_concurrency": l.max_concurrency,
        "overlap": l.overlap,
        "created_at": l.created_at,
    })
}

/// Parse `--once-at <time>`: a relative duration (`30s`, `5m` → now + dur) or an absolute
/// RFC3339 / `YYYY-MM-DDTHH:MM(:SS)` timestamp. Returns the UTC-ms fire time (spec §6.2).
fn parse_once_at(when: &str, now: i64) -> Result<i64> {
    // Relative duration first (the common scripting form, e.g. `--once-at 30s`).
    if let Ok(d) = crate::duration::parse_duration(when) {
        return Ok(now + d.as_millis() as i64);
    }
    // Absolute RFC3339 (with offset).
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(when) {
        return Ok(dt.timestamp_millis());
    }
    // Absolute local wall-clock `YYYY-MM-DDTHH:MM[:SS]`.
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(when, fmt) {
            let tz = cron::tz_from_name(&cron::local_tz_name());
            if let chrono::LocalResult::Single(dt) =
                chrono::TimeZone::from_local_datetime(&tz, &naive)
            {
                return Ok(dt.timestamp_millis());
            }
        }
    }
    Err(OrcrError::invalid_request(
        format!("cannot parse --once-at `{when}` (use a duration like `30m` or an RFC3339 time)"),
        "invalid_once_at",
    ))
}

/// Spawn a thread that reads lines from a child stream and appends them to the run log.
fn spawn_capture<R: std::io::Read + Send + 'static>(
    stream: R,
    log: Arc<Mutex<RunLog>>,
    tag: &'static str,
) {
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(text) => log.lock().unwrap().append(tag, &text),
                Err(_) => break,
            }
        }
    });
}

/// A run's captured-output log: JSONL `{ts, stream, text}`, size-capped + rotated (spec §12).
pub(super) struct RunLog {
    path: PathBuf,
    max_bytes: u64,
    max_files: u32,
    written: u64,
}

impl RunLog {
    fn new(path: PathBuf, max_bytes: u64, max_files: u32) -> RunLog {
        let written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        RunLog {
            path,
            max_bytes,
            max_files,
            written,
        }
    }

    fn append(&mut self, stream: &str, text: &str) {
        let record = serde_json::json!({ "ts": now_millis(), "stream": stream, "text": text });
        let line = format!("{record}\n");
        if self.max_bytes > 0 && self.written + line.len() as u64 > self.max_bytes {
            self.rotate();
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            if f.write_all(line.as_bytes()).is_ok() {
                self.written += line.len() as u64;
            }
        }
    }

    /// Rotate `run.log` → `run.log.1` → … up to `max_files`, then start fresh.
    fn rotate(&mut self) {
        super::log::rotate_numbered(&self.path, self.max_files);
        self.written = 0;
    }
}

/// The OS process start time for `pid`, comparable across the lifetime of one boot. Used to
/// guard signals against pid reuse (spec §11.3). `None` if it cannot be read.
fn os_process_start_time(pid: i64) -> Option<i64> {
    #[cfg(target_os = "linux")]
    {
        // /proc/<pid>/stat field 22 = starttime in clock ticks since boot. The comm field (2)
        // may contain spaces/parens, so split after the closing ')'.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After ')' the first field is state (index 0 here = field 3), so starttime (field 22)
        // is at index 22 - 3 = 19.
        fields.get(19)?.parse::<i64>().ok()
    }
    #[cfg(target_os = "macos")]
    {
        // proc_pidinfo(PROC_PIDTBSDINFO) → proc_bsdinfo.pbi_start_tvsec/tvusec.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if n == size {
            Some(info.pbi_start_tvsec as i64 * 1000 + info.pbi_start_tvusec as i64 / 1000)
        } else {
            None
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}
