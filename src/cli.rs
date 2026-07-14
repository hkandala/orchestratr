//! The `orcr` CLI (spec §6): a thin client of the socket API. Every server-touching verb
//! maps 1:1 to a socket method; the CLI's job is arg parsing, the `--json` envelope, the
//! §13 error→exit-code mapping, TTY detection, and human-readable rendering.
//!
//! M1 wires the `server` and `api` nouns plus all shared plumbing; agent/loop nouns land
//! in later milestones (their methods are already registered in [`crate::api`]).

use crate::api;
use crate::config::{Config, LoadedConfig};
use crate::error::{OrcrError, Result};
use crate::home::Home;
use crate::server::{self, Client};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

/// `orcr` — a cross-provider orchestrator for AI coding agents, built on herdr.
#[derive(Parser, Debug)]
#[command(name = "orcr", version, about, disable_help_subcommand = true)]
pub struct Cli {
    /// Emit exactly one JSON envelope on stdout (`{"ok":true,...}` / `{"ok":false,...}`).
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Spawn, message, and manage agents (§6.1).
    Agent {
        #[command(subcommand)]
        cmd: AgentCmd,
    },
    /// Durable cron over any command: loops and their runs (§6.2).
    Loop {
        #[command(subcommand)]
        cmd: LoopCmd,
    },
    /// The orcr server: single writer, socket API (§6.4).
    Server {
        #[command(subcommand)]
        cmd: ServerCmd,
    },
    /// The self-describing socket API (§6.5).
    Api {
        #[command(subcommand)]
        cmd: ApiCmd,
    },
    /// The live, view-only monitoring TUI (§6.3, §7).
    Top {
        /// Optional path pattern (or uuid) to pre-scope the tree (§5.1 grammar).
        pattern: Option<String>,
        /// Only show agents of this provider.
        #[arg(short = 'a', long = "agent")]
        agent: Option<String>,
        /// Only show agents in this status.
        #[arg(long)]
        status: Option<String>,
        /// Only show managed agents.
        #[arg(long)]
        managed: bool,
        /// Only show unmanaged agents.
        #[arg(long)]
        unmanaged: bool,
        /// Show only loops and their run subtrees.
        #[arg(long)]
        loops: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// Spawn a managed agent (async): validate, enqueue, print `<path> <uuid>`.
    Run {
        /// The agent's name (lands directly in your scope). Exactly one of --name/--path.
        #[arg(long)]
        name: Option<String>,
        /// The agent's path (last segment = name; relative to scope, `/` = absolute).
        #[arg(long)]
        path: Option<String>,
        /// Provider (falls back to config `defaults.agent`).
        #[arg(short = 'a', long = "agent")]
        agent: Option<String>,
        /// Prompt text; `-p -` reads the prompt from stdin.
        #[arg(short = 'p', long = "prompt")]
        prompt: Option<String>,
        /// GC policy for the pane's lifetime.
        #[arg(long, value_parser = ["auto", "immediate", "never"])]
        gc: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        /// Working directory for the agent (default: the caller's cwd).
        #[arg(long)]
        cwd: Option<String>,
        /// Kill the agent after this duration (no default timeout).
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Spawn, wait for the first completion, and print the response (`run --gc immediate`
    /// → `wait` → `logs --last-response`), all in one call.
    Ask {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(short = 'a', long = "agent")]
        agent: Option<String>,
        #[arg(short = 'p', long = "prompt")]
        prompt: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Block until every target agent settles (patterns + uuids).
    Wait {
        /// Targets (`<pattern|uuid>...`).
        #[arg(required = true)]
        targets: Vec<String>,
        /// Give up waiting after this duration (partial result, exit 3).
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Read an agent's native transcript (exact target).
    Logs {
        /// The target agent (`<path|uuid>`; no wildcards).
        target: String,
        /// Print only the final assistant response (fails loudly if none).
        #[arg(long = "last-response")]
        last_response: bool,
        /// Show only the last N entries.
        #[arg(long)]
        tail: Option<usize>,
        /// Keep streaming new entries after the tail.
        #[arg(long)]
        follow: bool,
    },
    /// Deliver a prompt to an existing agent's TUI (exact target).
    Send {
        /// The target agent (`<path|uuid>`; no wildcards).
        target: String,
        /// The prompt (positional); `-` reads from stdin. Or use -p.
        prompt: Option<String>,
        /// Prompt text; `-p -` reads from stdin.
        #[arg(short = 'p', long = "prompt")]
        prompt_flag: Option<String>,
    },
    /// Attach your terminal to an agent's pane (observe by default; --takeover claims input).
    Attach {
        /// The target agent (`<path|uuid>`; no wildcards).
        target: String,
        /// Claim input (drive the agent directly), rather than observe-only.
        #[arg(long)]
        takeover: bool,
    },
    /// Kill matched agents (patterns + uuids); graceful shutdown → pane closed.
    Kill {
        /// Targets (`<pattern|uuid>...`).
        #[arg(required = true)]
        targets: Vec<String>,
        /// Kill unmanaged agents too (closes a pane orcr does not own).
        #[arg(long)]
        force: bool,
        /// Skip the TTY confirmation.
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// List active (and, with --all, ended) agents.
    Ls {
        /// Optional path pattern to filter by.
        pattern: Option<String>,
        #[arg(short = 'a', long = "agent")]
        agent: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        managed: bool,
        #[arg(long)]
        unmanaged: bool,
        /// Include ended agents (history).
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoopCmd {
    /// Create a durable loop over a command (`-- <command…>`), on a cron or `--once-at`.
    Create {
        /// The loop's name (one segment, root-level, mandatory).
        name: String,
        /// Five-field cron expression (quote it). Mutually exclusive with --once-at.
        cron: Option<String>,
        /// Fire once at a time (a duration like `30m` from now, or an RFC3339/local timestamp).
        #[arg(long = "once-at")]
        once_at: Option<String>,
        /// Max concurrent runs (default 1).
        #[arg(long = "max-concurrency")]
        max_concurrency: Option<i64>,
        /// At capacity: `queue` (coalesce, default) or `skip` (drop the fire).
        #[arg(long, value_parser = ["queue", "skip"])]
        overlap: Option<String>,
        /// Kill a run after this duration (no default).
        #[arg(long)]
        timeout: Option<String>,
        /// The command to run, after `--` (an argv array, executed directly — no shell).
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Pause loop(s): no new fires (a pending scheduled run is held).
    Pause {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Resume paused loop(s).
    Resume {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// End loop definition(s); history stays queryable.
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
        /// Also stop active/pending runs and kill their agents.
        #[arg(long = "kill-active")]
        kill_active: bool,
        /// Skip the TTY confirmation.
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// List loops.
    Ls {
        names: Vec<String>,
        #[arg(long)]
        status: Option<String>,
        /// Include ended loops (history).
        #[arg(long)]
        all: bool,
    },
    /// Interleaved command output + scheduler actions, tagged by run.
    Logs {
        name: String,
        #[arg(long)]
        run: Option<String>,
        #[arg(long, value_parser = ["orcr", "command"])]
        source: Option<String>,
        #[arg(long)]
        tail: Option<usize>,
        #[arg(long)]
        follow: bool,
    },
    /// Verbs on a loop's runs (executions).
    Run {
        #[command(subcommand)]
        cmd: LoopRunCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum LoopRunCmd {
    /// Manually trigger a run (works on paused loops); prints `<loop>/<run_id> <run_uuid>`.
    Start { name: String },
    /// Stop run(s): optional `<run_id|run_uuid>` targets one, else all active + pending.
    Stop {
        name: String,
        run: Option<String>,
        /// Skip the TTY confirmation.
        #[arg(short = 'y', long = "yes")]
        yes: bool,
    },
    /// List a loop's runs.
    Ls {
        name: String,
        #[arg(long)]
        status: Option<String>,
        /// Include ended runs (history).
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServerCmd {
    /// Start the server (idempotent); blocks until ready.
    Start {
        /// Run in the foreground (what a service unit uses).
        #[arg(long)]
        foreground: bool,
    },
    /// Graceful control-plane stop (never touches agent panes).
    Stop,
    /// Health: version, protocol, paths, herdr reachability, counts, integrations.
    Status,
    /// Read the server log.
    Logs {
        /// Show only the last N lines.
        #[arg(long)]
        tail: Option<usize>,
        /// Keep streaming new lines.
        #[arg(long)]
        follow: bool,
    },
    /// Register start-at-login so loops fire after a reboot (launchd/systemd).
    Enable,
    /// Remove the start-at-login registration (server + store untouched).
    Disable,
}

#[derive(Subcommand, Debug)]
pub enum ApiCmd {
    /// Print the versioned JSON Schema of the whole socket protocol.
    Schema {
        /// Write the schema to a file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Dump live runtime state stamped with snapshot_seq.
    Snapshot,
}

/// Parse args and run; returns the process exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    let json = cli.json;
    match dispatch(&cli) {
        Ok(()) => 0,
        Err(e) => {
            emit_error(json, &e);
            e.exit_code()
        }
    }
}

fn dispatch(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Agent { cmd } => dispatch_agent(cli.json, cmd),
        Command::Loop { cmd } => dispatch_loop(cli.json, cmd),
        Command::Server { cmd } => match cmd {
            ServerCmd::Start { foreground } => cmd_server_start(cli.json, *foreground),
            ServerCmd::Stop => cmd_server_stop(cli.json),
            ServerCmd::Status => cmd_server_status(cli.json),
            ServerCmd::Logs { tail, follow } => cmd_server_logs(cli.json, *tail, *follow),
            ServerCmd::Enable => cmd_server_enable(cli.json),
            ServerCmd::Disable => cmd_server_disable(cli.json),
        },
        Command::Api { cmd } => match cmd {
            ApiCmd::Schema { output } => cmd_api_schema(cli.json, output.as_deref()),
            ApiCmd::Snapshot => cmd_api_snapshot(cli.json),
        },
        Command::Top {
            pattern,
            agent,
            status,
            managed,
            unmanaged,
            loops,
        } => cmd_top(pattern, agent, status, *managed, *unmanaged, *loops),
    }
}

/// `orcr top` (spec §6.3): build the pre-scoping filter from the flags (resolving any pattern
/// against the caller's `ORCR_PATH` scope, §5.1) and launch the view-only TUI. Live-only by
/// design — there is no `--all` (that is `ls --all`).
#[allow(clippy::too_many_arguments)]
fn cmd_top(
    pattern: &Option<String>,
    agent: &Option<String>,
    status: &Option<String>,
    managed: bool,
    unmanaged: bool,
    loops: bool,
) -> Result<()> {
    use crate::top::{model::TopFilter, run_top};
    let (_caller_id, caller_path) = caller_identity();
    let scope = caller_path.as_deref().and_then(crate::path::scope_of_agent);

    let compiled = match pattern.as_deref().filter(|s| !s.is_empty()) {
        Some(p) => {
            let resolved = crate::path::resolve_selector(scope.as_deref(), p)?;
            Some(crate::path::Pattern::compile(&resolved)?)
        }
        None => None,
    };
    if managed && unmanaged {
        return Err(OrcrError::invalid_request(
            "pass at most one of --managed / --unmanaged",
            "conflicting_flags",
        ));
    }
    let filter = TopFilter {
        pattern: compiled,
        provider: agent.clone(),
        status: status.clone(),
        managed: match (managed, unmanaged) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        },
        loops_only: loops,
    };
    run_top(scope, filter)
}

// --- agent ---

fn dispatch_agent(json: bool, cmd: &AgentCmd) -> Result<()> {
    match cmd {
        AgentCmd::Run {
            name,
            path,
            agent,
            prompt,
            gc,
            model,
            effort,
            cwd,
            timeout,
        } => cmd_agent_run(
            json, name, path, agent, prompt, gc, model, effort, cwd, timeout,
        ),
        AgentCmd::Ask {
            name,
            path,
            agent,
            prompt,
            model,
            effort,
            cwd,
            timeout,
        } => cmd_agent_ask(json, name, path, agent, prompt, model, effort, cwd, timeout),
        AgentCmd::Wait { targets, timeout } => cmd_agent_wait(json, targets, timeout.as_deref()),
        AgentCmd::Logs {
            target,
            last_response,
            tail,
            follow,
        } => cmd_agent_logs(json, target, *last_response, *tail, *follow),
        AgentCmd::Send {
            target,
            prompt,
            prompt_flag,
        } => cmd_agent_send(json, target, prompt.as_deref(), prompt_flag.as_deref()),
        AgentCmd::Attach { target, takeover } => cmd_agent_attach(json, target, *takeover),
        AgentCmd::Kill {
            targets,
            force,
            yes,
        } => cmd_agent_kill(json, targets, *force, *yes),
        AgentCmd::Ls {
            pattern,
            agent,
            status,
            managed,
            unmanaged,
            all,
        } => cmd_agent_ls(json, pattern, agent, status, *managed, *unmanaged, *all),
    }
}

/// The caller identity (`ORCR_ID`/`ORCR_PATH`) from the CLI's own env — how lineage + scope
/// assemble for nested agents (§5.3). Absent at a plain shell.
fn caller_identity() -> (Option<String>, Option<String>) {
    let id = std::env::var("ORCR_ID").ok().filter(|s| !s.is_empty());
    let path = std::env::var("ORCR_PATH").ok().filter(|s| !s.is_empty());
    (id, path)
}

/// Resolve a `-p <text>` / `-p -` prompt: `-` reads all of stdin.
fn resolve_prompt(p: Option<&str>) -> Result<Option<String>> {
    match p {
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .lock()
                .read_to_string(&mut buf)
                .map_err(|e| {
                    OrcrError::invalid_request(format!("cannot read stdin: {e}"), "stdin")
                })?;
            Ok(Some(buf))
        }
        Some(text) => Ok(Some(text.to_string())),
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_agent_run(
    json: bool,
    name: &Option<String>,
    path: &Option<String>,
    agent: &Option<String>,
    prompt: &Option<String>,
    gc: &Option<String>,
    model: &Option<String>,
    effort: &Option<String>,
    cwd: &Option<String>,
    timeout: &Option<String>,
) -> Result<()> {
    if name.is_some() == path.is_some() {
        return Err(OrcrError::invalid_request(
            "naming is mandatory: pass exactly one of --name or --path",
            "name_required",
        ));
    }
    let prompt = resolve_prompt(prompt.as_deref())?;
    // Default cwd = the caller's current directory (§6.1).
    let cwd = match cwd {
        Some(c) => Some(c.clone()),
        None => std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string()),
    };
    let (caller_id, caller_path) = caller_identity();

    let mut params = json!({});
    let obj = params.as_object_mut().unwrap();
    if let Some(n) = name {
        obj.insert("name".into(), json!(n));
    }
    if let Some(p) = path {
        obj.insert("path".into(), json!(p));
    }
    if let Some(a) = agent {
        obj.insert("agent".into(), json!(a));
    }
    if let Some(p) = &prompt {
        obj.insert("prompt".into(), json!(p));
    }
    if let Some(g) = gc {
        obj.insert("gc".into(), json!(g));
    }
    if let Some(m) = model {
        obj.insert("model".into(), json!(m));
    }
    if let Some(e) = effort {
        obj.insert("effort".into(), json!(e));
    }
    if let Some(c) = &cwd {
        obj.insert("cwd".into(), json!(c));
    }
    if let Some(t) = timeout {
        obj.insert("timeout".into(), json!(t));
    }
    if let Some(id) = &caller_id {
        obj.insert("caller_id".into(), json!(id));
    }
    if let Some(cp) = &caller_path {
        obj.insert("caller_path".into(), json!(cp));
    }

    let result = connect_and_request("agent.run", params)?;
    let a = &result["agent"];
    let agent_path = a["path"].as_str().unwrap_or_default().to_string();
    let uuid = a["uuid"].as_str().unwrap_or_default().to_string();
    emit_success(json, result.clone(), || {
        // `<path> <uuid>` on one stdout line (cut-friendly, §5.1).
        println!("{agent_path} {uuid}");
        if stdout_is_tty() {
            let name = agent_path.rsplit('/').next().unwrap_or(&agent_path);
            eprintln!(
                "wait: orcr agent wait {name} · response: orcr agent logs {name} \
                 --last-response · attach: orcr agent attach {name}"
            );
        }
    });
    Ok(())
}

fn cmd_agent_send(
    json: bool,
    target: &str,
    positional_prompt: Option<&str>,
    prompt_flag: Option<&str>,
) -> Result<()> {
    let raw = prompt_flag.or(positional_prompt);
    let prompt = resolve_prompt(raw)?.ok_or_else(|| {
        OrcrError::invalid_request(
            "send requires a prompt (positional or -p)",
            "prompt_required",
        )
    })?;
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({ "target": target, "prompt": prompt });
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("agent.send", params)?;
    emit_success(json, result.clone(), || {
        println!(
            "{} delivered (while {}) input_seq={}",
            result["path"].as_str().unwrap_or_default(),
            result["delivered_while"].as_str().unwrap_or_default(),
            result["input_seq"],
        );
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_agent_ask(
    json: bool,
    name: &Option<String>,
    path: &Option<String>,
    agent: &Option<String>,
    prompt: &Option<String>,
    model: &Option<String>,
    effort: &Option<String>,
    cwd: &Option<String>,
    timeout: &Option<String>,
) -> Result<()> {
    if name.is_some() == path.is_some() {
        return Err(OrcrError::invalid_request(
            "naming is mandatory: pass exactly one of --name or --path",
            "name_required",
        ));
    }
    let prompt = resolve_prompt(prompt.as_deref())?;
    let cwd = match cwd {
        Some(c) => Some(c.clone()),
        None => std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string()),
    };
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({});
    let obj = params.as_object_mut().unwrap();
    if let Some(n) = name {
        obj.insert("name".into(), json!(n));
    }
    if let Some(p) = path {
        obj.insert("path".into(), json!(p));
    }
    if let Some(a) = agent {
        obj.insert("agent".into(), json!(a));
    }
    if let Some(p) = &prompt {
        obj.insert("prompt".into(), json!(p));
    }
    if let Some(m) = model {
        obj.insert("model".into(), json!(m));
    }
    if let Some(e) = effort {
        obj.insert("effort".into(), json!(e));
    }
    if let Some(c) = &cwd {
        obj.insert("cwd".into(), json!(c));
    }
    if let Some(t) = timeout {
        obj.insert("timeout".into(), json!(t));
    }
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("agent.ask", params)?;
    emit_success(json, result.clone(), || {
        // The final response on stdout (§6.1).
        println!(
            "{}",
            result["response"]["text"].as_str().unwrap_or_default()
        );
    });
    Ok(())
}

fn cmd_agent_wait(json: bool, targets: &[String], timeout: Option<&str>) -> Result<()> {
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({ "targets": targets });
    if let Some(t) = timeout {
        params["timeout"] = json!(t);
    }
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("agent.wait", params)?;
    emit_success(json, result.clone(), || {
        for t in result["targets"].as_array().into_iter().flatten() {
            println!(
                "{}  {}",
                t["path"].as_str().unwrap_or_default(),
                t["reason"].as_str().unwrap_or_default(),
            );
        }
    });
    // Exit code from the settle outcome (spec §6.1): the outcomes rank
    // `4 any target blocked · 5 any target dead · 3 --timeout expired`, so a settled
    // blocked/dead target wins over the wait's own timeout when a mixed wait both times
    // out (a target still working) and has an already-settled blocked/dead target.
    let all_ok = result["all_ok"].as_bool().unwrap_or(false);
    let timed_out = result["timed_out"].as_bool().unwrap_or(false);
    let targets = result["targets"].as_array();
    let any_blocked = targets
        .map(|a| {
            a.iter()
                .any(|t| t["reason"].as_str().unwrap_or("").starts_with("blocked"))
        })
        .unwrap_or(false);
    // A "dead" target settled non-ok for a reason other than blocked or the wait's own
    // timeout (killed / canceled / failed / timeout / lost → exit 5).
    let any_dead = targets
        .map(|a| {
            a.iter().any(|t| {
                let reason = t["reason"].as_str().unwrap_or("");
                t["ok"].as_bool() == Some(false)
                    && !reason.starts_with("blocked")
                    && reason != "wait_timeout"
            })
        })
        .unwrap_or(false);
    let code = if all_ok {
        0
    } else if any_blocked {
        4
    } else if any_dead {
        5
    } else if timed_out {
        3
    } else {
        5
    };
    std::process::exit(code);
}

fn cmd_agent_logs(
    json: bool,
    target: &str,
    last_response: bool,
    tail: Option<usize>,
    follow: bool,
) -> Result<()> {
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({ "target": target, "last_response": last_response });
    if let Some(n) = tail {
        params["tail"] = json!(n);
    }
    add_caller(&mut params, &caller_id, &caller_path);

    if last_response {
        let result = connect_and_request("agent.logs", params)?;
        emit_success(json, result.clone(), || {
            println!(
                "{}",
                result["response"]["text"].as_str().unwrap_or_default()
            );
        });
        return Ok(());
    }

    let result = connect_and_request("agent.logs", params.clone())?;
    let mut seen = print_entries(json, &result, 0);
    if follow {
        // Follow is a poll under the hood (§6.1): re-read the transcript and print new
        // entries. Ignore --json for the live stream (each entry printed as it arrives).
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let mut p = json!({ "target": target, "last_response": false });
            add_caller(&mut p, &caller_id, &caller_path);
            match connect_and_request("agent.logs", p) {
                Ok(r) => seen = print_entries(false, &r, seen),
                Err(_) => continue,
            }
        }
    }
    Ok(())
}

/// Print transcript entries beyond `skip`; returns the new total count. In `--json` mode the
/// whole envelope is printed once (skip is ignored).
fn print_entries(json: bool, result: &Value, skip: usize) -> usize {
    let entries = result["entries"].as_array().cloned().unwrap_or_default();
    if json {
        emit_success(true, result.clone(), || {});
        return entries.len();
    }
    for e in entries.iter().skip(skip) {
        let role = e["role"].as_str().unwrap_or("");
        let kind = e["kind"].as_str().unwrap_or("");
        if let Some(tool) = e["tool"].as_str() {
            println!("{role} [{kind}] {tool}");
        } else {
            println!("{role} [{kind}] {}", e["text"].as_str().unwrap_or_default());
        }
    }
    entries.len()
}

/// `agent attach` (spec §6.1): the one terminal-mediated verb. The CLI calls
/// `agent.attach.prepare` (which inserts the lease first, so GC defers), execs `herdr agent
/// attach` locally while heart-beating the lease, and releases it on exit. Abrupt CLI death →
/// the lease expires by heartbeat.
fn cmd_agent_attach(json: bool, target: &str, takeover: bool) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({
        "target": target, "takeover": takeover, "client_pid": std::process::id(),
    });
    add_caller(&mut params, &caller_id, &caller_path);
    let prep = connect_and_request("agent.attach.prepare", params)?;

    let uuid = prep["uuid"].as_str().unwrap_or_default().to_string();
    let path = prep["path"].as_str().unwrap_or_default().to_string();
    let lease_id = prep["lease_id"].as_str().unwrap_or_default().to_string();
    let ttl_ms = prep["ttl_ms"].as_u64().unwrap_or(30_000);
    let command: Vec<String> = prep["command"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if command.is_empty() {
        return Err(OrcrError::server_error(
            "attach_command",
            "attach.prepare returned no exec command",
        ));
    }

    // Heartbeat the lease in the background (every ~ttl/2) until the attach session ends, so GC
    // keeps deferring park/reap while attached (§5.4).
    let stop = Arc::new(AtomicBool::new(false));
    let hb_lease = lease_id.clone();
    let hb_stop = stop.clone();
    let heartbeat = std::thread::spawn(move || {
        let interval = Duration::from_millis((ttl_ms / 2).max(1000));
        while !hb_stop.load(Ordering::SeqCst) {
            std::thread::sleep(interval);
            if hb_stop.load(Ordering::SeqCst) {
                break;
            }
            let _ = connect_and_request("agent.attach.heartbeat", json!({ "lease_id": hb_lease }));
        }
    });

    // Exec the interactive herdr attach, inheriting the terminal.
    let status = std::process::Command::new(&command[0])
        .args(&command[1..])
        .status();

    // Detach: stop heart-beating and release the lease (GC resumes).
    stop.store(true, Ordering::SeqCst);
    let _ = heartbeat.join();
    let _ = connect_and_request("agent.attach.release", json!({ "lease_id": lease_id }));

    match status {
        Ok(_) => {
            emit_success(
                json,
                json!({ "uuid": uuid, "path": path, "attached": true, "takeover": takeover }),
                || {
                    println!("detached {path}");
                },
            );
            Ok(())
        }
        Err(e) => Err(OrcrError::environment(
            "herdr_unreachable",
            format!("failed to exec `{}`: {e}", command.join(" ")),
        )),
    }
}

fn cmd_agent_kill(json: bool, targets: &[String], force: bool, yes: bool) -> Result<()> {
    let (caller_id, caller_path) = caller_identity();

    // TTY confirmation by default (spec §6): preview the matched set, then ask. Non-TTY and
    // --json callers proceed without prompting.
    if !yes && !json && stdout_is_tty() {
        let mut preview = json!({ "targets": targets, "force": force, "preview": true });
        add_caller(&mut preview, &caller_id, &caller_path);
        let matched = connect_and_request("agent.kill", preview)?;
        let rows = matched["targets"].as_array().cloned().unwrap_or_default();
        eprintln!("Matched {} agent(s):", rows.len());
        for r in &rows {
            eprintln!(
                "  {} [{}]",
                r["path"].as_str().unwrap_or_default(),
                r["status"].as_str().unwrap_or_default()
            );
        }
        eprint!("Kill these {} agent(s)? [y/N] ", rows.len());
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok();
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            eprintln!("aborted");
            return Ok(());
        }
    }

    let mut params = json!({ "targets": targets, "force": force });
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("agent.kill", params)?;
    emit_success(json, result.clone(), || {
        let killed = result["killed"].as_array().map(|a| a.len()).unwrap_or(0);
        for k in result["killed"].as_array().into_iter().flatten() {
            println!("killed {}", k["path"].as_str().unwrap_or_default());
        }
        for s in result["skipped"].as_array().into_iter().flatten() {
            println!(
                "skipped {} ({})",
                s["path"].as_str().unwrap_or_default(),
                s["reason"].as_str().unwrap_or_default()
            );
        }
        if killed == 0
            && result["skipped"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(true)
        {
            println!("no agents killed");
        }
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_agent_ls(
    json: bool,
    pattern: &Option<String>,
    agent: &Option<String>,
    status: &Option<String>,
    managed: bool,
    unmanaged: bool,
    all: bool,
) -> Result<()> {
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({ "managed": managed, "unmanaged": unmanaged, "all": all });
    let obj = params.as_object_mut().unwrap();
    if let Some(p) = pattern {
        obj.insert("pattern".into(), json!(p));
    }
    if let Some(a) = agent {
        obj.insert("agent".into(), json!(a));
    }
    if let Some(s) = status {
        obj.insert("status".into(), json!(s));
    }
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("agent.ls", params)?;
    emit_success(json, result.clone(), || print_ls_human(&result));
    Ok(())
}

/// Attach the caller identity params to a request object.
fn add_caller(params: &mut Value, caller_id: &Option<String>, caller_path: &Option<String>) {
    let obj = params.as_object_mut().unwrap();
    if let Some(id) = caller_id {
        obj.insert("caller_id".into(), json!(id));
    }
    if let Some(cp) = caller_path {
        obj.insert("caller_path".into(), json!(cp));
    }
}

/// Auto-start the server if needed, then send one request.
fn connect_and_request(method: &str, params: Value) -> Result<Value> {
    let home = Home::ensure()?;
    let config = load_config(&home)?;
    let client = Client::new(home.socket_path());
    client.ensure_running(&home, &config)?;
    client.request(method, params)
}

/// Render `agent ls` as a path tree (spec §6.1): `PATH UUID STATUS AGENT AGE`.
fn print_ls_human(result: &Value) {
    let agents = result["agents"].as_array().cloned().unwrap_or_default();
    if agents.is_empty() {
        println!("no agents");
        return;
    }
    for a in &agents {
        let uuid = a["uuid"].as_str().unwrap_or_default();
        let short = uuid.get(..8).unwrap_or(uuid);
        println!(
            "{:<40} {:<8} {:<9} {:<8}",
            a["path"].as_str().unwrap_or_default(),
            short,
            a["status"].as_str().unwrap_or_default(),
            a["agent"].as_str().unwrap_or("-"),
        );
    }
}

// --- loop ---

fn dispatch_loop(json: bool, cmd: &LoopCmd) -> Result<()> {
    match cmd {
        LoopCmd::Create {
            name,
            cron,
            once_at,
            max_concurrency,
            overlap,
            timeout,
            command,
        } => cmd_loop_create(
            json,
            name,
            cron,
            once_at,
            max_concurrency,
            overlap,
            timeout,
            command,
        ),
        LoopCmd::Pause { names } => cmd_loop_set(json, "loop.pause", names),
        LoopCmd::Resume { names } => cmd_loop_set(json, "loop.resume", names),
        LoopCmd::Rm {
            names,
            kill_active,
            yes,
        } => cmd_loop_rm(json, names, *kill_active, *yes),
        LoopCmd::Ls { names, status, all } => cmd_loop_ls(json, names, status.as_deref(), *all),
        LoopCmd::Logs {
            name,
            run,
            source,
            tail,
            follow,
        } => cmd_loop_logs(
            json,
            name,
            run.as_deref(),
            source.as_deref(),
            *tail,
            *follow,
        ),
        LoopCmd::Run { cmd } => match cmd {
            LoopRunCmd::Start { name } => cmd_loop_run_start(json, name),
            LoopRunCmd::Stop { name, run, yes } => {
                cmd_loop_run_stop(json, name, run.as_deref(), *yes)
            }
            LoopRunCmd::Ls { name, status, all } => {
                cmd_loop_run_ls(json, name, status.as_deref(), *all)
            }
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_loop_create(
    json: bool,
    name: &str,
    cron: &Option<String>,
    once_at: &Option<String>,
    max_concurrency: &Option<i64>,
    overlap: &Option<String>,
    timeout: &Option<String>,
    command: &[String],
) -> Result<()> {
    if cron.is_some() == once_at.is_some() {
        return Err(OrcrError::invalid_request(
            "pass exactly one of a cron expression or --once-at",
            "cadence_required",
        ));
    }
    // The loop's creation cwd is the workspace its agents inherit (§6.2): the caller's cwd.
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());
    let mut params = json!({ "name": name, "command": command });
    let obj = params.as_object_mut().unwrap();
    if let Some(c) = cron {
        obj.insert("cron".into(), json!(c));
    }
    if let Some(o) = once_at {
        obj.insert("once_at".into(), json!(o));
    }
    if let Some(m) = max_concurrency {
        obj.insert("max_concurrency".into(), json!(m));
    }
    if let Some(o) = overlap {
        obj.insert("overlap".into(), json!(o));
    }
    if let Some(t) = timeout {
        obj.insert("timeout".into(), json!(t));
    }
    if let Some(c) = &cwd {
        obj.insert("cwd".into(), json!(c));
    }
    let result = connect_and_request("loop.create", params)?;
    let l = &result["loop"];
    emit_success(json, result.clone(), || {
        let argv: Vec<String> = l["argv"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let tz = l["tz"].as_str().unwrap_or_default();
        println!("loop {} created", l["name"].as_str().unwrap_or_default());
        println!("  command:  {}", argv.join(" "));
        println!(
            "  cadence:  {}",
            crate::cron::describe(
                l["cadence_kind"].as_str().unwrap_or_default(),
                l["cadence"].as_str().unwrap_or_default(),
                tz,
            ),
        );
        if let Some(nf) = l["next_fire_at"].as_i64() {
            println!("  next:     {}", crate::cron::describe_next_fire(nf, tz));
        }
        println!("  cancel:   {}", l["cancel"].as_str().unwrap_or_default());
    });
    Ok(())
}

fn cmd_loop_set(json: bool, method: &str, names: &[String]) -> Result<()> {
    let result = connect_and_request(method, json!({ "names": names }))?;
    emit_success(json, result.clone(), || {
        for u in result["updated"].as_array().into_iter().flatten() {
            println!(
                "{} {}",
                u["name"].as_str().unwrap_or_default(),
                u["status"].as_str().unwrap_or_default()
            );
        }
        for s in result["skipped"].as_array().into_iter().flatten() {
            println!(
                "skipped {} ({})",
                s["name"].as_str().unwrap_or_default(),
                s["reason"].as_str().unwrap_or_default()
            );
        }
    });
    Ok(())
}

fn cmd_loop_rm(json: bool, names: &[String], kill_active: bool, yes: bool) -> Result<()> {
    if !yes && !json && stdout_is_tty() {
        eprint!("Remove loop(s) {}? [y/N] ", names.join(", "));
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok();
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            eprintln!("aborted");
            return Ok(());
        }
    }
    let (caller_id, caller_path) = caller_identity();
    let mut params = json!({ "names": names, "kill_active": kill_active });
    add_caller(&mut params, &caller_id, &caller_path);
    let result = connect_and_request("loop.rm", params)?;
    emit_success(json, result.clone(), || {
        for r in result["removed"].as_array().into_iter().flatten() {
            println!(
                "removed {} ({})",
                r["name"].as_str().unwrap_or_default(),
                r["reason"].as_str().unwrap_or_default()
            );
        }
        for s in result["skipped"].as_array().into_iter().flatten() {
            println!(
                "skipped {} ({})",
                s["name"].as_str().unwrap_or_default(),
                s["reason"].as_str().unwrap_or_default()
            );
        }
    });
    Ok(())
}

fn cmd_loop_ls(json: bool, names: &[String], status: Option<&str>, all: bool) -> Result<()> {
    let mut params = json!({ "names": names, "all": all });
    if let Some(s) = status {
        params["status"] = json!(s);
    }
    let result = connect_and_request("loop.ls", params)?;
    emit_success(json, result.clone(), || {
        let loops = result["loops"].as_array().cloned().unwrap_or_default();
        if loops.is_empty() {
            println!("no loops");
            return;
        }
        for l in &loops {
            println!(
                "{:<20} {:<8} {:<16} next={}",
                l["name"].as_str().unwrap_or_default(),
                l["status"].as_str().unwrap_or_default(),
                l["cadence"].as_str().unwrap_or_default(),
                l["next_fire_at"]
                    .as_i64()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into()),
            );
        }
    });
    Ok(())
}

fn cmd_loop_logs(
    json: bool,
    name: &str,
    run: Option<&str>,
    source: Option<&str>,
    tail: Option<usize>,
    follow: bool,
) -> Result<()> {
    let build = || {
        let mut params = json!({ "name": name });
        if let Some(r) = run {
            params["run"] = json!(r);
        }
        if let Some(s) = source {
            params["source"] = json!(s);
        }
        if let Some(t) = tail {
            params["tail"] = json!(t);
        }
        params
    };
    let result = connect_and_request("loop.logs", build())?;
    let mut seen = print_loop_lines(json, &result, 0);
    if follow {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            match connect_and_request("loop.logs", build()) {
                Ok(r) => seen = print_loop_lines(false, &r, seen),
                Err(_) => continue,
            }
        }
    }
    Ok(())
}

fn print_loop_lines(json: bool, result: &Value, skip: usize) -> usize {
    let lines = result["lines"].as_array().cloned().unwrap_or_default();
    if json {
        emit_success(true, result.clone(), || {});
        return lines.len();
    }
    for l in lines.iter().skip(skip) {
        println!(
            "[{}] {}",
            l["run"].as_str().unwrap_or_default(),
            l["text"].as_str().unwrap_or_default(),
        );
    }
    lines.len()
}

fn cmd_loop_run_start(json: bool, name: &str) -> Result<()> {
    let result = connect_and_request("loop.run.start", json!({ "name": name }))?;
    let r = &result["run"];
    let path = r["path"].as_str().unwrap_or_default().to_string();
    let uuid = r["uuid"].as_str().unwrap_or_default().to_string();
    emit_success(json, result.clone(), || {
        println!("{path} {uuid}");
    });
    Ok(())
}

fn cmd_loop_run_stop(json: bool, name: &str, run: Option<&str>, yes: bool) -> Result<()> {
    if !yes && !json && stdout_is_tty() {
        let what = run
            .map(|r| format!("run {name}/{r}"))
            .unwrap_or_else(|| format!("all runs of loop {name}"));
        eprint!("Stop {what}? [y/N] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok();
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            eprintln!("aborted");
            return Ok(());
        }
    }
    let mut params = json!({ "name": name });
    if let Some(r) = run {
        params["run"] = json!(r);
    }
    let result = connect_and_request("loop.run.stop", params)?;
    emit_success(json, result.clone(), || {
        for s in result["stopped"].as_array().into_iter().flatten() {
            println!(
                "{} {}",
                s["path"].as_str().unwrap_or_default(),
                s["status"].as_str().unwrap_or_default()
            );
        }
        for s in result["skipped"].as_array().into_iter().flatten() {
            println!(
                "skipped {} ({})",
                s["run_id"].as_str().unwrap_or_default(),
                s["reason"].as_str().unwrap_or_default()
            );
        }
    });
    Ok(())
}

fn cmd_loop_run_ls(json: bool, name: &str, status: Option<&str>, all: bool) -> Result<()> {
    let mut params = json!({ "name": name, "all": all });
    if let Some(s) = status {
        params["status"] = json!(s);
    }
    let result = connect_and_request("loop.run.ls", params)?;
    emit_success(json, result.clone(), || {
        let runs = result["runs"].as_array().cloned().unwrap_or_default();
        if runs.is_empty() {
            println!("no runs");
            return;
        }
        for r in &runs {
            println!(
                "{:<10} {:<10} {:<10} agents={}",
                r["run_id"].as_str().unwrap_or_default(),
                r["status"].as_str().unwrap_or_default(),
                r["kind"].as_str().unwrap_or_default(),
                r["agents"].as_i64().unwrap_or(0),
            );
        }
    });
    Ok(())
}

// --- server ---

fn cmd_server_start(json: bool, foreground: bool) -> Result<()> {
    let home = Home::ensure()?;
    let config = load_config(&home)?;
    if foreground {
        // This process becomes (or defers to) the server; blocks until graceful stop.
        let outcome = server::run_foreground(&home, config)?;
        emit_success(json, json!({ "status": outcome.as_str() }), || {
            println!("server {}", outcome.as_str());
        });
        Ok(())
    } else {
        let client = Client::new(home.socket_path());
        let outcome = client.ensure_running(&home, &config)?;
        emit_success(json, json!({ "status": outcome.as_str() }), || {
            println!("server {}", outcome.as_str());
        });
        Ok(())
    }
}

fn cmd_server_stop(json: bool) -> Result<()> {
    let home = Home::resolve()?;
    let client = Client::new(home.socket_path());
    // Do not auto-start just to stop; if nothing is running, that's an idempotent no-op.
    if client.handshake().is_err() {
        emit_success(json, json!({ "status": "not_running" }), || {
            println!("server not_running");
        });
        return Ok(());
    }
    client.request("server.stop", json!({}))?;
    emit_success(json, json!({ "status": "stopped" }), || {
        println!("server stopped");
    });
    Ok(())
}

fn cmd_server_status(json: bool) -> Result<()> {
    let home = Home::ensure()?;
    let config = load_config(&home)?;
    let client = Client::new(home.socket_path());
    client.ensure_running(&home, &config)?;
    let result = client.request("server.status", json!({}))?;
    emit_success(json, result.clone(), || print_status_human(&result));
    Ok(())
}

fn cmd_server_logs(json: bool, tail: Option<usize>, follow: bool) -> Result<()> {
    let home = Home::resolve()?;
    let path = home.logs_dir().join("server.log");

    if follow {
        // Stream: print the tail, then keep printing new lines. Ignore --json for the live
        // stream (each line is already a JSON object).
        stream_follow(&path, tail)?;
        return Ok(());
    }

    let lines = read_tail(&path, tail)?;
    if json {
        let parsed: Vec<Value> = lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|_| json!({ "raw": l })))
            .collect();
        emit_success(true, json!({ "lines": parsed }), || {});
    } else {
        for l in &lines {
            println!("{l}");
        }
    }
    Ok(())
}

fn cmd_server_enable(json: bool) -> Result<()> {
    let home = Home::ensure()?;
    let result = crate::service::enable(&home)?;
    emit_success(json, result.clone(), || {
        println!(
            "enabled: wrote {}\n  verify: {}",
            result["unit"].as_str().unwrap_or_default(),
            result["verify"].as_str().unwrap_or_default(),
        );
    });
    Ok(())
}

fn cmd_server_disable(json: bool) -> Result<()> {
    let home = Home::ensure()?;
    let result = crate::service::disable(&home)?;
    emit_success(json, result.clone(), || {
        println!(
            "disabled: removed {}",
            result["unit"].as_str().unwrap_or_default(),
        );
    });
    Ok(())
}

// --- api ---

fn cmd_api_schema(json: bool, output: Option<&std::path::Path>) -> Result<()> {
    // The schema is derived from the compiled method registry — no server needed (mirrors
    // `herdr api schema` working offline).
    let doc = api::schema_document();
    let text = serde_json::to_string_pretty(&doc).unwrap();
    if let Some(path) = output {
        std::fs::write(path, format!("{text}\n")).map_err(|e| {
            OrcrError::environment(
                "config_invalid",
                format!("cannot write schema to {}: {e}", path.display()),
            )
        })?;
        emit_success(
            json,
            json!({ "written": path.display().to_string() }),
            || {
                eprintln!("wrote schema to {}", path.display());
            },
        );
    } else if json {
        emit_success(true, doc, || {});
    } else {
        println!("{text}");
    }
    Ok(())
}

fn cmd_api_snapshot(json: bool) -> Result<()> {
    let home = Home::ensure()?;
    let config = load_config(&home)?;
    let client = Client::new(home.socket_path());
    client.ensure_running(&home, &config)?;
    let result = client.request("api.snapshot", json!({}))?;
    emit_success(json, result.clone(), || {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    });
    Ok(())
}

// --- helpers ---

/// Load config, printing any warnings to stderr (never stdout — stdout is the envelope).
fn load_config(home: &Home) -> Result<Config> {
    let LoadedConfig { config, warnings } = Config::load(home)?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    Ok(config)
}

/// Print a `{"ok":true,"result":…}` envelope in JSON mode, else run the human renderer.
fn emit_success(json: bool, result: Value, human: impl FnOnce()) {
    if json {
        println!("{}", json!({ "ok": true, "result": result }));
    } else {
        human();
    }
}

/// Emit an error: the JSON envelope to stdout in `--json` mode, else a message to stderr.
fn emit_error(json: bool, e: &OrcrError) {
    if json {
        println!("{}", e.to_envelope());
    } else {
        eprintln!("error: {e}");
    }
}

fn print_status_human(s: &Value) {
    let g = |k: &str| s.get(k).cloned().unwrap_or(Value::Null);
    println!("orcr server");
    println!("  version   {}", g("version"));
    println!("  protocol  {}", g("protocol"));
    println!("  socket    {}", g("socket"));
    println!("  store     {}", g("store"));
    if let Some(h) = s.get("herdr") {
        println!(
            "  herdr     reachable={} session={} running={}",
            h.get("reachable").unwrap_or(&Value::Null),
            h.get("session").unwrap_or(&Value::Null),
            h.get("session_running").unwrap_or(&Value::Null),
        );
    }
    if let Some(c) = s.get("counts") {
        println!(
            "  counts    live={} queued={} blocked={} unmanaged={} unmarked_panes={} \
             unknown_marked_panes={}",
            c.get("live").unwrap_or(&Value::Null),
            c.get("queued").unwrap_or(&Value::Null),
            c.get("blocked").unwrap_or(&Value::Null),
            c.get("unmanaged").unwrap_or(&Value::Null),
            c.get("unmarked_panes").unwrap_or(&Value::Null),
            c.get("unknown_marked_panes").unwrap_or(&Value::Null),
        );
    }
    if let Some(d) = s.get("drift") {
        println!(
            "  drift     lost={} repaired={}",
            d.get("lost").unwrap_or(&Value::Null),
            d.get("repaired").unwrap_or(&Value::Null),
        );
    }
}

/// Whether stdout is a TTY (spec §6: TTY vs non-TTY behavior — hints, confirmations).
pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty on a valid fd is always safe.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Read the last `tail` lines of a file (all lines if `tail` is None). Missing file = empty.
fn read_tail(path: &std::path::Path, tail: Option<usize>) -> Result<Vec<String>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(OrcrError::environment(
                "config_invalid",
                format!("cannot read {}: {e}", path.display()),
            ))
        }
    };
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    match tail {
        Some(n) if n < lines.len() => Ok(lines[lines.len() - n..].to_vec()),
        _ => Ok(lines),
    }
}

/// Print the tail of a log file, then keep printing newly-appended lines (`--follow`).
fn stream_follow(path: &std::path::Path, tail: Option<usize>) -> Result<()> {
    use std::io::{Seek, SeekFrom};

    // Print the initial tail.
    for l in read_tail(path, tail)? {
        println!("{l}");
    }
    let mut pos: u64 = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let stdout = std::io::stdout();
    loop {
        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < pos {
            // The file was rotated/truncated — restart from the top.
            pos = 0;
        }
        if len > pos {
            file.seek(SeekFrom::Start(pos)).ok();
            let reader = std::io::BufReader::new(&mut file);
            let mut handle = stdout.lock();
            for line in reader.lines() {
                let line = line.map_err(|e| {
                    OrcrError::server_error("socket_io", format!("log read error: {e}"))
                })?;
                writeln!(handle, "{line}").ok();
            }
            handle.flush().ok();
            pos = len;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
