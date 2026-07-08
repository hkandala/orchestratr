use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::daemon;
use crate::engine::{Engine, RunMode, RunRequest};
use crate::herdr::{discover_herdr, HerdrClient, HerdrError, INSTALL_URL};
use crate::jobs::{
    self, CatchupPolicy, EverySpec, GoalSpec, LoopSpec, OrphanPolicy, ScheduleSpec,
    ScheduleTrigger, AUTO_FALLBACK_SECS,
};
use crate::profile;
use crate::store::{AgentRow, EventRow, IdKind, JobRow, Store, TurnRow};

#[derive(Debug, Parser)]
#[command(name = "orcr", version, about = "Agent orchestration over herdr")]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    Send(SendArgs),
    Wait(WaitArgs),
    Out(OutArgs),
    Show(ShowArgs),
    Ps(JsonArgs),
    Tree(TreeArgs),
    Kill(KillArgs),
    Attach(IdArg),
    Status(JsonArgs),
    History(HistoryArgs),
    Gc(GcArgs),
    Loop(LoopArgs),
    Goal(GoalArgs),
    Workflow(WorkflowArgs),
    Schedule(ScheduleArgs),
    Job(JobArgs),
    Top(TopArgs),
    Events(EventsArgs),
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct JsonArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct IdArg {
    id: String,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long = "harness", short = 'a')]
    harness: String,
    #[arg(
        short = 'p',
        conflicts_with = "prompt_file",
        required_unless_present = "prompt_file"
    )]
    prompt: Option<String>,
    #[arg(long = "prompt-file", value_name = "f|-", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, default_value = "")]
    model: String,
    #[arg(long, default_value = "")]
    effort: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, default_value = "600s")]
    timeout: String,
    #[arg(long)]
    keep: bool,
    #[arg(long, value_enum, default_value_t = CliRunMode::Tui)]
    mode: CliRunMode,
    #[arg(long)]
    worktree: bool,
    #[arg(long)]
    parent: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    wait: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliRunMode {
    Tui,
    Exec,
}

impl From<CliRunMode> for RunMode {
    fn from(value: CliRunMode) -> Self {
        match value {
            CliRunMode::Tui => Self::Tui,
            CliRunMode::Exec => Self::Exec,
        }
    }
}

#[derive(Debug, Args)]
struct SendArgs {
    id: String,
    text: Option<String>,
    #[arg(long = "prompt-file", value_name = "f|-", conflicts_with = "text")]
    prompt_file: Option<PathBuf>,
    #[arg(long, conflicts_with = "turn")]
    steer: bool,
    #[arg(long, conflicts_with = "steer")]
    turn: bool,
    #[arg(long)]
    wait: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WaitArgs {
    ids: Vec<String>,
    #[arg(long)]
    any: bool,
    #[arg(long)]
    tree: Option<String>,
    #[arg(long)]
    timeout: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct OutArgs {
    id: String,
    #[arg(long)]
    turn: Option<i64>,
    #[arg(long)]
    recursive: bool,
    #[arg(long, value_enum, default_value_t = OutFormat::Body)]
    format: OutFormat,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutFormat {
    Body,
    Path,
    Json,
}

#[derive(Debug, Args)]
struct ShowArgs {
    id: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TreeArgs {
    id: Option<String>,
    #[arg(long)]
    watch: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct KillArgs {
    ids: Vec<String>,
    #[arg(long)]
    tree: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct HistoryArgs {
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    status: Option<String>,
    #[arg(long)]
    parent: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    harness: Option<String>,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct GcArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long)]
    foreground: bool,
}

#[derive(Debug, Args)]
struct TopArgs {
    #[arg(long)]
    pane: bool,
}

#[derive(Debug, Args)]
struct EventsArgs {
    #[arg(long)]
    follow: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct JobArgs {
    #[command(subcommand)]
    command: JobCommand,
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum JobCommand {
    Ls,
    Show(IdArg),
    Pause(IdArg),
    Resume(IdArg),
    Rm(IdArg),
}

#[derive(Debug, Args)]
struct LoopArgs {
    #[arg(long = "harness", short = 'a')]
    harness: String,
    #[arg(
        short = 'p',
        conflicts_with = "prompt_file",
        required_unless_present = "prompt_file"
    )]
    prompt: Option<String>,
    #[arg(long = "prompt-file", value_name = "f|-", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    #[arg(long, default_value = "10m")]
    every: String,
    #[arg(long = "tick-on")]
    tick_on: Option<String>,
    #[arg(long)]
    max: Option<u64>,
    #[arg(long = "max-runs")]
    max_runs: Option<u64>,
    #[arg(long = "max-duration")]
    max_duration: Option<String>,
    #[arg(long)]
    until: Option<String>,
    #[arg(long)]
    foreground: bool,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, default_value = "")]
    model: String,
    #[arg(long, default_value = "")]
    effort: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, default_value = "600s")]
    timeout: String,
    #[arg(long)]
    keep: bool,
    #[arg(long, value_enum, default_value_t = CliRunMode::Tui)]
    mode: CliRunMode,
    #[arg(long)]
    worktree: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct GoalArgs {
    #[arg(long = "harness", short = 'a')]
    harness: String,
    #[arg(
        short = 'p',
        conflicts_with = "prompt_file",
        required_unless_present = "prompt_file"
    )]
    prompt: Option<String>,
    #[arg(long = "prompt-file", value_name = "f|-", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    #[arg(long = "judge-harness")]
    judge_harness: Option<String>,
    #[arg(long = "judge-model")]
    judge_model: Option<String>,
    #[arg(long = "max-iters", default_value_t = 5)]
    max_iters: u64,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, default_value = "")]
    model: String,
    #[arg(long, default_value = "")]
    effort: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, default_value = "600s")]
    timeout: String,
    #[arg(long)]
    keep: bool,
    #[arg(long, value_enum, default_value_t = CliRunMode::Tui)]
    mode: CliRunMode,
    #[arg(long)]
    worktree: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct WorkflowArgs {
    #[command(subcommand)]
    command: WorkflowCommand,
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    Run(WorkflowRunArgs),
}

#[derive(Debug, Args)]
struct WorkflowRunArgs {
    script: PathBuf,
    #[arg(long = "on-orphan", value_enum, default_value_t = CliOrphanPolicy::Kill)]
    on_orphan: CliOrphanPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliOrphanPolicy {
    Kill,
    Keep,
}

impl From<CliOrphanPolicy> for OrphanPolicy {
    fn from(value: CliOrphanPolicy) -> Self {
        match value {
            CliOrphanPolicy::Kill => Self::Kill,
            CliOrphanPolicy::Keep => Self::Keep,
        }
    }
}

#[derive(Debug, Args)]
struct ScheduleArgs {
    #[command(subcommand)]
    command: ScheduleCommand,
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ScheduleCommand {
    Add(Box<ScheduleAddArgs>),
    Ls,
    Show(IdArg),
    Pause(IdArg),
    Resume(ScheduleResumeArgs),
    Rm(IdArg),
    FromLoop(ScheduleFromLoopArgs),
}

#[derive(Debug, Args)]
struct ScheduleAddArgs {
    cron: Option<String>,
    #[arg(
        long = "at",
        conflicts_with = "cron",
        help = "One-shot time: RFC3339, 'today HH:MM', 'tomorrow HH:MM', or 'in <dur>' such as 'in 2h'"
    )]
    at: Option<String>,
    #[arg(long = "harness", short = 'a')]
    harness: String,
    #[arg(
        short = 'p',
        conflicts_with = "prompt_file",
        required_unless_present = "prompt_file"
    )]
    prompt: Option<String>,
    #[arg(long = "prompt-file", value_name = "f|-", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = CliCatchup::Skip)]
    catchup: CliCatchup,
    #[arg(long)]
    expires: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, default_value = "")]
    model: String,
    #[arg(long, default_value = "")]
    effort: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, default_value = "600s")]
    timeout: String,
    #[arg(long)]
    keep: bool,
    #[arg(long, value_enum, default_value_t = CliRunMode::Tui)]
    mode: CliRunMode,
    #[arg(long)]
    worktree: bool,
    #[arg(long = "max-runs")]
    max_runs: Option<u64>,
    #[arg(long = "max-duration")]
    max_duration: Option<String>,
}

#[derive(Debug, Args)]
struct ScheduleResumeArgs {
    id: String,
    #[arg(
        long = "at",
        help = "Re-arm time: RFC3339, 'today HH:MM', 'tomorrow HH:MM', or 'in <dur>' such as 'in 2h'"
    )]
    at: Option<String>,
}

#[derive(Debug, Args)]
struct ScheduleFromLoopArgs {
    id: String,
    cron: Option<String>,
    #[arg(
        long = "at",
        conflicts_with = "cron",
        help = "One-shot time: RFC3339, 'today HH:MM', 'tomorrow HH:MM', or 'in <dur>' such as 'in 2h'"
    )]
    at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CliCatchup {
    Skip,
    Once,
}

impl From<CliCatchup> for CatchupPolicy {
    fn from(value: CliCatchup) -> Self {
        match value {
            CliCatchup::Skip => Self::Skip,
            CliCatchup::Once => Self::Once,
        }
    }
}

#[derive(Debug, Serialize)]
struct StatusReport {
    herdr: HerdrStatus,
    session: SessionStatus,
    store: StoreStatus,
    db: DbStatus,
    daemon: DaemonReport,
}

#[derive(Debug, Serialize)]
struct DaemonReport {
    running: bool,
    pid: Option<u32>,
}

#[derive(Debug, Serialize)]
struct HerdrStatus {
    path: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct SessionStatus {
    name: String,
    exists: bool,
    running: bool,
    session_dir: Option<String>,
    socket_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct StoreStatus {
    root: String,
    ok: bool,
}

#[derive(Debug, Serialize)]
struct DbStatus {
    path: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Ok = 0,
    Env = 2,
    Timeout = 3,
    Blocked = 4,
    Killed = 5,
    NotFound = 6,
    StateConflict = 7,
    Other = 1,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let command = cli
        .command
        .unwrap_or(Command::Status(JsonArgs { json: false }));
    let json_output = command_json(&command);
    match dispatch(command) {
        Ok(exit) => ExitCode::from(exit as u8),
        Err(error) => {
            let (exit, code, message, details) = classify_error(&error);
            print_error(json_output, code, &message, details);
            ExitCode::from(exit as u8)
        }
    }
}

fn dispatch(command: Command) -> Result<Exit> {
    match command {
        Command::Run(args) => cmd_run(args),
        Command::Send(args) => cmd_send(args),
        Command::Wait(args) => cmd_wait(args),
        Command::Out(args) => cmd_out(args),
        Command::Show(args) => cmd_show(args),
        Command::Ps(args) => cmd_ps(args),
        Command::Tree(args) => cmd_tree(args),
        Command::Kill(args) => cmd_kill(args),
        Command::Attach(args) => cmd_attach(args),
        Command::Status(args) => cmd_status(args.json),
        Command::History(args) => cmd_history(args),
        Command::Gc(args) => cmd_gc(args),
        Command::Loop(args) => cmd_loop(args),
        Command::Goal(args) => cmd_goal(args),
        Command::Workflow(args) => cmd_workflow(args),
        Command::Schedule(args) => cmd_schedule(args),
        Command::Job(args) => cmd_job(args),
        Command::Top(args) => cmd_top(args),
        Command::Events(args) => cmd_events(args),
        Command::Serve(args) => cmd_serve(args),
    }
}

fn cmd_run(args: RunArgs) -> Result<Exit> {
    let prompt = prompt_text(args.prompt.as_deref(), args.prompt_file.as_ref())?;
    let timeout_s = parse_duration_s(&args.timeout)?;
    let mut ctx = ContextBundle::load(args.session.as_deref())?;
    let profile = profile::lookup(&args.harness)
        .ok_or_else(|| anyhow!("unknown harness: {}", args.harness))?;
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?);
    if args.worktree {
        eprintln!(
            "warning: --worktree is accepted but worktree provisioning is not implemented yet"
        );
    }
    let mut engine = Engine::new(&ctx.config, &mut ctx.store, ctx.herdr.clone());
    let result = engine.run(
        profile.as_ref(),
        RunRequest {
            name: args.name,
            parent_id: args.parent,
            mode: args.mode.into(),
            model: args.model,
            effort: args.effort,
            cwd,
            timeout_s,
            keep: args.keep,
            prompt,
            wait: args.wait,
        },
    )?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    let mut value = json!({
        "agent": agent_json(&result.agent),
        "turn": turn_summary_json(&result.turn),
        "paths": {
            "run_dir": result.agent.run_dir,
            "response": result.turn.response_path,
        },
        "permissions": "bypass",
    });
    if let Some(response) = result.response {
        value["response"] = json!({
            "text": response.text,
            "path": result.turn.response_path,
            "source": response.source.as_str(),
        });
    }
    if args.json {
        print_ok(value);
    } else if args.wait {
        if let Some(text) = value.pointer("/response/text").and_then(Value::as_str) {
            print!("{text}");
        } else {
            println!("{}", result.agent.id);
        }
    } else {
        println!("{}", result.agent.id);
    }
    Ok(status_exit(result.agent.status.as_str()))
}

fn cmd_send(args: SendArgs) -> Result<Exit> {
    let prompt = prompt_text(args.text.as_deref(), args.prompt_file.as_ref())?;
    let mut ctx = ContextBundle::load(None)?;
    let id = ctx.store.resolve_agent_id(&args.id)?;
    let agent = ctx
        .store
        .get_agent(&id)?
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    let mode = match agent.status.as_str() {
        "working" => {
            if args.turn {
                return state_conflict(&id, &agent.status, "idle for turn");
            }
            "steer"
        }
        "idle" => {
            if args.steer {
                return state_conflict(&id, &agent.status, "working for steer");
            }
            "turn"
        }
        "done" | "killed" | "lost" | "timeout" => {
            return state_conflict(&id, &agent.status, "working or idle");
        }
        _ => return state_conflict(&id, &agent.status, "working or idle"),
    };
    let profile = profile::lookup(&agent.harness)
        .ok_or_else(|| anyhow!("unknown harness on agent {}: {}", id, agent.harness))?;
    let mut engine = Engine::new(&ctx.config, &mut ctx.store, ctx.herdr.clone());
    let turn = if mode == "steer" {
        engine.steer(&id, &prompt)?
    } else {
        engine.turn(&id, &prompt)?
    };
    let response = if args.wait {
        engine.wait_for_agent(
            profile.as_ref(),
            &id,
            u64::try_from(agent.timeout_s).unwrap_or(600),
        )?
    } else {
        None
    };
    let mut value = json!({
        "id": id,
        "mode": mode,
        "turn": turn_summary_json(&turn),
    });
    if let Some(response) = response {
        value["response"] = json!({
            "text": response.text,
            "path": turn.response_path,
            "source": response.source.as_str(),
        });
    }
    if args.json {
        print_ok(value);
    } else if args.wait {
        if let Some(text) = value.pointer("/response/text").and_then(Value::as_str) {
            print!("{text}");
        }
    } else {
        println!("{mode}");
    }
    let updated = ctx.store.get_agent(&id)?.unwrap_or(agent);
    Ok(status_exit(updated.status.as_str()))
}

fn cmd_wait(args: WaitArgs) -> Result<Exit> {
    let timeout_s = match args.timeout {
        Some(value) => Some(parse_duration_s(&value)?),
        None => None,
    };
    let deadline = timeout_s.map(|s| Instant::now() + Duration::from_secs(s));
    let mut ctx = ContextBundle::load(None)?;
    let mut ids: Vec<String> = args
        .ids
        .iter()
        .map(|id| ctx.store.resolve_agent_id(id))
        .collect::<Result<_>>()?;
    if let Some(root) = args.tree {
        let root = ctx.store.resolve_agent_id(&root)?;
        ids.extend(descendant_ids(&ctx.store.list_agents()?, &root));
        ids.push(root);
    }
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        bail!("wait needs at least one id or --tree <id>");
    }
    loop {
        drive_pending_waits(&mut ctx, &ids, deadline, args.any)?;
        ctx.store = Store::open(&ctx.config.store_root)?;
        let state = wait_state(&ctx.store, &ids)?;
        let done_enough = if args.any {
            !state.completed.is_empty() || !state.blocked.is_empty()
        } else {
            state.pending.is_empty()
        };
        if done_enough {
            let value = json!({
                "completed": state.completed,
                "pending": state.pending,
                "blocked": state.blocked,
                "timed_out": false,
            });
            let has_blocked = !state.blocked.is_empty();
            if args.json {
                print_ok(value);
            } else {
                for id in value["completed"].as_array().unwrap() {
                    println!("{}", id.as_str().unwrap());
                }
            }
            return Ok(if has_blocked { Exit::Blocked } else { Exit::Ok });
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            let value = json!({
                "completed": state.completed,
                "pending": state.pending,
                "blocked": state.blocked,
                "timed_out": true,
            });
            if args.json {
                print_ok(value);
            } else {
                eprintln!("timed out");
            }
            return Ok(Exit::Timeout);
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn drive_pending_waits(
    ctx: &mut ContextBundle,
    ids: &[String],
    deadline: Option<Instant>,
    stop_after_first: bool,
) -> Result<()> {
    for id in ids {
        let Some(agent) = ctx.store.get_agent(id)? else {
            continue;
        };
        if !matches!(agent.status.as_str(), "working" | "starting") {
            continue;
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(());
        }
        let wait_s = deadline
            .map(|d| d.saturating_duration_since(Instant::now()).as_secs().max(1))
            .unwrap_or_else(|| u64::try_from(agent.timeout_s).unwrap_or(600).max(1));
        let Some(profile) = profile::lookup(&agent.harness) else {
            continue;
        };
        let mut engine = Engine::new(&ctx.config, &mut ctx.store, ctx.herdr.clone());
        let _ = engine.wait_for_agent(profile.as_ref(), id, wait_s)?;
        if stop_after_first {
            let state = wait_state(&ctx.store, ids)?;
            if !state.completed.is_empty() || !state.blocked.is_empty() {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn cmd_out(args: OutArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let resolved = ctx.store.resolve_agent_ref(&args.id)?;
    let turn = args.turn.or(resolved.turn);
    let ids = if args.recursive {
        let mut ids = vec![resolved.agent_id.clone()];
        ids.extend(descendant_ids(
            &ctx.store.list_agents()?,
            &resolved.agent_id,
        ));
        ids
    } else {
        vec![resolved.agent_id]
    };
    let agents = agents_by_id(ctx.store.list_agents()?);
    let mut items = Vec::new();
    for id in ids {
        let Some(agent) = agents.get(&id) else {
            continue;
        };
        let turns = ctx.store.list_turns_by_agent(&id)?;
        let selected: Vec<TurnRow> = if let Some(n) = turn {
            turns.into_iter().filter(|row| row.n == n).collect()
        } else {
            turns.into_iter().rev().take(1).collect()
        };
        for turn in selected {
            let text = (args.format != OutFormat::Path)
                .then(|| fs::read_to_string(&turn.response_path).ok())
                .flatten();
            items.push(json!({
                "id": id,
                "name": agent.name,
                "turn": turn.n,
                "path": turn.response_path,
                "source": turn.response_source,
                "text": text,
            }));
        }
    }
    if args.json || args.format == OutFormat::Json {
        print_ok(json!({ "items": items }));
    } else if args.format == OutFormat::Path {
        for item in items {
            println!(
                "{}\t{}\t{}",
                item["id"].as_str().unwrap_or_default(),
                item["name"].as_str().unwrap_or_default(),
                item["path"].as_str().unwrap_or_default()
            );
        }
    } else {
        for item in items {
            if let Some(text) = item["text"].as_str() {
                print!("{text}");
            }
        }
    }
    Ok(Exit::Ok)
}

fn cmd_show(args: ShowArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let id = ctx.store.resolve_agent_id(&args.id)?;
    let agent = ctx
        .store
        .get_agent(&id)?
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    let value = show_json(&ctx.store, &agent)?;
    if args.json {
        print_ok(value);
    } else {
        println!(
            "{} {} {}",
            agent.id,
            agent.name.unwrap_or_default(),
            agent.status
        );
        println!("run_dir {}", agent.run_dir);
        for child in children(&ctx.store.list_agents()?, &agent.id) {
            println!("child {child}");
        }
    }
    Ok(Exit::Ok)
}

fn cmd_ps(args: JsonArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let agents: Vec<Value> = ctx.store.list_agents()?.iter().map(agent_json).collect();
    if args.json {
        print_ok(json!({ "agents": agents }));
    } else {
        for agent in agents {
            println!(
                "{}\t{}\t{}\t{}",
                agent["id"].as_str().unwrap_or_default(),
                agent["name"].as_str().unwrap_or_default(),
                agent["status"].as_str().unwrap_or_default(),
                agent["harness"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(Exit::Ok)
}

fn cmd_tree(args: TreeArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    loop {
        let agents = ctx.store.list_agents()?;
        let roots = if let Some(id) = &args.id {
            vec![ctx.store.resolve_agent_id(id)?]
        } else {
            agents
                .iter()
                .filter(|agent| agent.parent_id.is_none())
                .map(|agent| agent.id.clone())
                .collect()
        };
        let value = json!({
            "roots": roots
                .iter()
                .filter_map(|id| tree_node(&ctx.store, &agents, id).transpose())
                .collect::<Result<Vec<_>>>()?
        });
        if args.json {
            print_ok(value);
            return Ok(Exit::Ok);
        }
        print_tree_value(&value, 0);
        if !args.watch {
            return Ok(Exit::Ok);
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn cmd_kill(args: KillArgs) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let mut killed_jobs = Vec::new();
    let mut agent_inputs = Vec::new();
    for input in args.ids {
        if is_job_ref(&input) {
            if let Some(mut job) = ctx.store.get_job(&input)? {
                if job.status == "running" || job.status == "paused" {
                    job.status = "killed".to_string();
                    job.ended_reason = Some("killed".to_string());
                    job.next_run_at = None;
                    ctx.store.update_job(&job)?;
                    jobs::append_job_event(
                        &ctx.store,
                        "job.state",
                        &job.id,
                        json!({"status": "killed"}),
                    )?;
                    killed_jobs.push(job.id);
                }
                continue;
            }
        }
        agent_inputs.push(input);
    }
    let agents = ctx.store.list_agents()?;
    let mut ids: Vec<String> = agent_inputs
        .iter()
        .map(|id| ctx.store.resolve_agent_id(id))
        .collect::<Result<_>>()?;
    if args.tree {
        let mut expanded = Vec::new();
        for id in &ids {
            expanded.extend(descendant_ids(&agents, id));
        }
        ids.extend(expanded);
        ids.sort_by_key(|id| tree_depth(&agents, id));
        ids.reverse();
        ids.dedup();
    }
    let mut killed = Vec::new();
    let mut skipped = Vec::new();
    for id in ids {
        let Some(agent) = ctx.store.get_agent(&id)? else {
            skipped.push(json!({"id": id, "reason": "not_found"}));
            continue;
        };
        let Some(profile) = profile::lookup(&agent.harness) else {
            skipped.push(json!({"id": id, "reason": "unknown_harness"}));
            continue;
        };
        let mut engine = Engine::new(&ctx.config, &mut ctx.store, ctx.herdr.clone());
        if engine.kill_agent(profile.as_ref(), &id)? {
            killed.push(id);
        } else {
            skipped.push(json!({"id": id, "reason": "no_pane"}));
        }
    }
    let value = json!({ "killed": killed, "jobs": killed_jobs, "skipped": skipped });
    if args.json {
        print_ok(value);
    } else {
        for id in value["killed"].as_array().unwrap() {
            println!("{}", id.as_str().unwrap());
        }
    }
    Ok(Exit::Ok)
}

fn cmd_attach(args: IdArg) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let id = ctx.store.resolve_agent_id(&args.id)?;
    let agent = ctx
        .store
        .get_agent(&id)?
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    if !matches!(
        agent.status.as_str(),
        "working" | "idle" | "blocked" | "starting"
    ) {
        return state_conflict(&id, &agent.status, "live agent");
    }
    ctx.herdr.session_attach()?;
    Ok(Exit::Ok)
}

fn cmd_status(json_output: bool) -> Result<Exit> {
    let config = Config::load().context("config_error")?;
    let herdr_bin = discover(&config)?;
    let client = HerdrClient::new(herdr_bin.clone(), config.herdr.session.clone());
    let version = client.version()?;
    let session = client
        .session_list()?
        .sessions
        .into_iter()
        .find(|session| session.name == config.herdr.session)
        .map(|session| SessionStatus {
            name: session.name,
            exists: true,
            running: session.running,
            session_dir: session.session_dir,
            socket_path: session.socket_path,
        })
        .unwrap_or_else(|| SessionStatus {
            name: config.herdr.session.clone(),
            exists: false,
            running: false,
            session_dir: None,
            socket_path: None,
        });
    let db_path = config.store_root.join("orcr.db");
    let db_error = Store::open(&config.store_root)
        .err()
        .map(|error| error.to_string());
    let report = StatusReport {
        herdr: HerdrStatus {
            path: herdr_bin.display().to_string(),
            version,
        },
        session,
        store: StoreStatus {
            root: config.store_root.display().to_string(),
            ok: true,
        },
        db: DbStatus {
            path: db_path.display().to_string(),
            ok: db_error.is_none(),
            error: db_error,
        },
        daemon: {
            let daemon = daemon::status(&config.store_root);
            DaemonReport {
                running: daemon.running,
                pid: daemon.pid,
            }
        },
    };
    if json_output || !io::stdout().is_terminal() {
        print_ok(json!(report));
    } else {
        print_human_status(&report);
    }
    Ok(Exit::Ok)
}

fn cmd_history(args: HistoryArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let since = match args.since {
        Some(value) => {
            Some(Utc::now() - ChronoDuration::seconds(i64::try_from(parse_duration_s(&value)?)?))
        }
        None => None,
    };
    let parent = match args.parent {
        Some(value) if is_job_ref(&value) => Some(value),
        Some(value) => Some(ctx.store.resolve_agent_id(&value)?),
        None => None,
    };
    let mut agents = ctx.store.list_agents()?;
    agents.retain(|agent| {
        args.status
            .as_ref()
            .is_none_or(|status| &agent.status == status)
            && parent
                .as_ref()
                .is_none_or(|parent| agent.parent_id.as_ref() == Some(parent))
            && args
                .name
                .as_ref()
                .is_none_or(|name| agent.name.as_ref() == Some(name))
            && args.harness.as_ref().is_none_or(|h| &agent.harness == h)
            && since.is_none_or(|since| {
                DateTime::parse_from_rfc3339(&agent.created_at)
                    .map(|dt| dt.with_timezone(&Utc) >= since)
                    .unwrap_or(false)
            })
    });
    agents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if let Some(limit) = args.limit {
        agents.truncate(limit);
    }
    let items: Vec<Value> = agents
        .iter()
        .map(|agent| agent_history_json(&ctx.store, agent))
        .collect::<Result<_>>()?;
    if args.json {
        print_ok(json!({ "items": items }));
    } else {
        for item in items {
            println!(
                "{}\t{}\t{}\t{} in\t{} out\t{}",
                item["id"].as_str().unwrap_or_default(),
                item["status"].as_str().unwrap_or_default(),
                item["harness"].as_str().unwrap_or_default(),
                item["tokens"]["in"].as_i64().unwrap_or_default(),
                item["tokens"]["out"].as_i64().unwrap_or_default(),
                item["run_dir"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(Exit::Ok)
}

fn cmd_gc(args: GcArgs) -> Result<Exit> {
    let config = Config::load().context("config_error")?;
    let report = daemon::reconcile(&config, args.dry_run)?;
    let value = serde_json::to_value(report)?;
    if args.json {
        print_ok(value);
    } else {
        println!("{}", value);
    }
    Ok(Exit::Ok)
}

fn cmd_top(args: TopArgs) -> Result<Exit> {
    if !args.pane && crate::top::is_headless() {
        eprintln!("orcr top requires an interactive terminal (no TTY detected)");
        return Ok(Exit::Env);
    }
    let ctx = ContextBundle::load(None)?;
    if args.pane {
        let herdr_env = std::env::var("HERDR_ENV").ok();
        if herdr_env.as_deref() != Some("1") {
            bail!("orcr top --pane requires running inside a herdr pane (HERDR_ENV is not set)");
        }
        let pane_id = std::env::var("HERDR_PANE_ID").context(
            "HERDR_PANE_ID is not set; cannot determine which pane to split the viewer from",
        )?;
        let opened =
            crate::top::open_viewer_pane(&ctx.herdr, &ctx.config, &pane_id, &orcr_bin_path())?;
        if !opened {
            eprintln!("orcr-top pane already open");
        }
        return Ok(Exit::Ok);
    }
    crate::top::run_tui(ctx.config, ctx.store, ctx.herdr)?;
    Ok(Exit::Ok)
}

fn orcr_bin_path() -> String {
    std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "orcr".to_string())
}

/// Best-effort: open the live-tree viewer pane once per herdr session on run/job
/// creation, per spec/07 "Auto-viewer (v1)". Never fails the calling command.
fn maybe_auto_open_viewer(config: &Config, herdr: &HerdrClient) {
    let herdr_env = std::env::var("HERDR_ENV").ok();
    if !crate::top::auto_viewer_enabled(herdr_env.as_deref(), config.viewer.auto) {
        return;
    }
    let Ok(pane_id) = std::env::var("HERDR_PANE_ID") else {
        return;
    };
    let _ = crate::top::open_viewer_pane(herdr, config, &pane_id, &orcr_bin_path());
}

fn cmd_serve(args: ServeArgs) -> Result<Exit> {
    let config = Config::load().context("config_error")?;
    if args.foreground {
        daemon::serve_foreground(config)?;
    } else {
        let status = daemon::start_background(&config)?;
        if status.running {
            println!("daemon running pid={}", status.pid.unwrap_or_default());
        }
    }
    Ok(Exit::Ok)
}

fn cmd_events(args: EventsArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    if args.follow {
        let mut last_seq = 0;
        loop {
            let events = ctx.store.list_events_after(last_seq)?;
            for event in events {
                last_seq = event.seq;
                println!("{}", event_ndjson(&event));
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
    let events: Vec<Value> = ctx.store.list_events()?.iter().map(event_json).collect();
    if args.json {
        print_ok(json!({ "events": events }));
    } else {
        for event in events {
            println!(
                "{}\t{}\t{}",
                event["seq"].as_i64().unwrap_or_default(),
                event["kind"].as_str().unwrap_or_default(),
                event["ref_id"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(Exit::Ok)
}

fn cmd_job(args: JobArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    match args.command {
        JobCommand::Ls => {
            let jobs: Vec<Value> = ctx.store.list_jobs()?.iter().map(job_json).collect();
            if args.json {
                print_ok(json!({ "jobs": jobs }));
            } else {
                for job in jobs {
                    println!(
                        "{}\t{}\t{}\t{}",
                        job["id"].as_str().unwrap_or_default(),
                        job["type"].as_str().unwrap_or_default(),
                        job["status"].as_str().unwrap_or_default(),
                        job["next_run"].as_str().unwrap_or_default()
                    );
                }
            }
        }
        JobCommand::Show(id) => {
            let job = ctx
                .store
                .get_job(&id.id)?
                .ok_or_else(|| anyhow!("job not found: {}", id.id))?;
            let value = job_json(&job);
            if args.json {
                print_ok(value);
            } else {
                print_job_human(&job);
            }
        }
        JobCommand::Pause(id) => {
            let mut job = ctx
                .store
                .get_job(&id.id)?
                .ok_or_else(|| anyhow!("job not found: {}", id.id))?;
            if job.status != "running" {
                return state_conflict(&job.id, &job.status, "running");
            }
            job.status = "paused".to_string();
            ctx.store.update_job(&job)?;
            jobs::append_job_event(
                &ctx.store,
                "job.state",
                &job.id,
                json!({"status": "paused"}),
            )?;
            if args.json {
                print_ok(job_json(&job));
            } else {
                println!("{}", job.id);
            }
        }
        JobCommand::Resume(id) => {
            let mut job = ctx
                .store
                .get_job(&id.id)?
                .ok_or_else(|| anyhow!("job not found: {}", id.id))?;
            if job.status != "paused" {
                return state_conflict(&job.id, &job.status, "paused");
            }
            job.status = "running".to_string();
            if job.next_run_at.is_none() {
                job.next_run_at = Some(Utc::now().to_rfc3339());
            }
            ctx.store.update_job(&job)?;
            jobs::append_job_event(
                &ctx.store,
                "job.state",
                &job.id,
                json!({"status": "running"}),
            )?;
            ensure_daemon(&ctx.config)?;
            if args.json {
                print_ok(job_json(&job));
            } else {
                println!("{}", job.id);
            }
        }
        JobCommand::Rm(id) => {
            let job = ctx
                .store
                .get_job(&id.id)?
                .ok_or_else(|| anyhow!("job not found: {}", id.id))?;
            if job.status == "running" {
                return state_conflict(&job.id, &job.status, "paused or ended");
            }
            ctx.store.delete_job(&job.id)?;
            jobs::append_job_event(&ctx.store, "job.rm", &job.id, json!({}))?;
            if args.json {
                print_ok(json!({"id": job.id}));
            } else {
                println!("{}", job.id);
            }
        }
    }
    Ok(Exit::Ok)
}

fn cmd_loop(args: LoopArgs) -> Result<Exit> {
    let timeout_s = parse_duration_s(&args.timeout)?;
    let every = parse_every(&args.every)?;
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?);
    let prompt_file = args
        .prompt_file
        .as_ref()
        .map(|path| path.display().to_string());
    let prompt = if args.prompt_file.is_some() {
        None
    } else {
        Some(prompt_text(args.prompt.as_deref(), None)?)
    };
    let spec = LoopSpec {
        harness: args.harness,
        prompt,
        prompt_file,
        every,
        tick_on: args.tick_on,
        max: args.max,
        max_runs: args.max_runs,
        max_duration_s: args
            .max_duration
            .as_deref()
            .map(parse_duration_s)
            .transpose()?,
        until: args.until,
        name: args.name,
        model: args.model,
        effort: args.effort,
        cwd: cwd.display().to_string(),
        timeout_s,
        keep: args.keep,
        mode: match args.mode {
            CliRunMode::Tui => "tui".to_string(),
            CliRunMode::Exec => "exec".to_string(),
        },
        worktree: args.worktree,
        last_next_reason: None,
        last_tick_agent: None,
        last_tick_response: None,
        tick_probe: None,
    };

    if args.foreground {
        return run_foreground_loop(spec, args.json);
    }

    let mut ctx = ContextBundle::load(None)?;
    let id = ctx.store.allocate_id(IdKind::Loop)?;
    let mut job = JobRow::new(
        id.clone(),
        "loop",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    job.next_run_at = Some(Utc::now().to_rfc3339());
    ctx.store.create_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running"}),
    )?;
    ensure_daemon(&ctx.config)?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    let value = job_json(&job);
    if args.json {
        print_ok(value);
    } else {
        println!(
            "{} loop cadence={} cancel: orcr kill {}",
            job.id,
            cadence_label(&spec),
            job.id
        );
    }
    Ok(Exit::Ok)
}

fn cmd_goal(args: GoalArgs) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?);
    let prompt_file = args
        .prompt_file
        .as_ref()
        .map(|path| path.display().to_string());
    let prompt = if args.prompt_file.is_some() {
        None
    } else {
        Some(prompt_text(args.prompt.as_deref(), None)?)
    };
    let judge_independent =
        args.judge_harness.is_some() || args.judge_model.as_ref().is_some_and(|m| m != &args.model);
    let spec = GoalSpec {
        harness: args.harness,
        prompt,
        prompt_file,
        judge_harness: args.judge_harness,
        judge_model: args.judge_model,
        max_iters: args.max_iters,
        name: args.name,
        model: args.model,
        effort: args.effort,
        cwd: cwd.display().to_string(),
        timeout_s: parse_duration_s(&args.timeout)?,
        keep: args.keep,
        mode: match args.mode {
            CliRunMode::Tui => "tui".to_string(),
            CliRunMode::Exec => "exec".to_string(),
        },
        worktree: args.worktree,
        worker_agent: None,
        last_worker_response: None,
        last_judge_agent: None,
        last_judge_response: None,
        last_fail_reasons: None,
        judge_independent,
    };
    let id = ctx.store.allocate_id(IdKind::Goal)?;
    let mut job = JobRow::new(
        id.clone(),
        "goal",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    job.next_run_at = Some(Utc::now().to_rfc3339());
    ctx.store.create_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running"}),
    )?;
    ensure_daemon(&ctx.config)?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    if args.json {
        print_ok(job_json(&job));
    } else {
        let label = if spec.judge_independent {
            "judge"
        } else {
            "self-check"
        };
        println!(
            "{} goal evaluation={} max_iters={} cancel: orcr kill {}",
            job.id, label, spec.max_iters, job.id
        );
    }
    Ok(Exit::Ok)
}

fn cmd_workflow(args: WorkflowArgs) -> Result<Exit> {
    match args.command {
        WorkflowCommand::Run(run) => cmd_workflow_run(run, args.json),
    }
}

fn cmd_workflow_run(args: WorkflowRunArgs, json_output: bool) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let id = ctx.store.allocate_id(IdKind::Workflow)?;
    let spec = jobs::new_workflow_spec(
        &ctx.config,
        &id,
        &args.script.display().to_string(),
        args.on_orphan.into(),
        &cwd.display().to_string(),
    )?;
    let mut job = JobRow::new(
        id.clone(),
        "workflow",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    ctx.store.create_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running"}),
    )?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    jobs::run_workflow_job(&ctx.config, &mut ctx.store, &mut job)?;
    if json_output {
        print_ok(job_json(&job));
    } else {
        println!("{} workflow {} log {}", job.id, job.status, spec.log_path);
    }
    Ok(if job.status == "done" {
        Exit::Ok
    } else {
        Exit::Other
    })
}

fn cmd_schedule(args: ScheduleArgs) -> Result<Exit> {
    match args.command {
        ScheduleCommand::Add(add) => cmd_schedule_add(*add, args.json),
        ScheduleCommand::Ls => cmd_schedule_job_alias(JobCommand::Ls, args.json),
        ScheduleCommand::Show(id) => cmd_schedule_job_alias(JobCommand::Show(id), args.json),
        ScheduleCommand::Pause(id) => cmd_schedule_job_alias(JobCommand::Pause(id), args.json),
        ScheduleCommand::Resume(resume) => cmd_schedule_resume(resume, args.json),
        ScheduleCommand::Rm(id) => cmd_schedule_job_alias(JobCommand::Rm(id), args.json),
        ScheduleCommand::FromLoop(from) => cmd_schedule_from_loop(from, args.json),
    }
}

fn cmd_schedule_add(args: ScheduleAddArgs, json_output: bool) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let tz_name = jobs::current_iana_timezone();
    let tz = jobs::parse_timezone(&tz_name)?;
    let trigger = schedule_trigger(args.cron.as_deref(), args.at.as_deref(), tz)?;
    let next_run_at = match &trigger {
        ScheduleTrigger::At { at_utc, .. } => at_utc.clone(),
        ScheduleTrigger::Cron { utc, .. } => jobs::next_cron_after(utc, Utc::now())
            .ok_or_else(|| anyhow!("cron has no future ticks"))?
            .to_rfc3339(),
    };
    let cwd = args
        .cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?);
    let prompt_file = args
        .prompt_file
        .as_ref()
        .map(|path| path.display().to_string());
    let prompt = if args.prompt_file.is_some() {
        None
    } else {
        Some(prompt_text(args.prompt.as_deref(), None)?)
    };
    let spec = ScheduleSpec {
        harness: args.harness,
        prompt,
        prompt_file,
        trigger,
        catchup: args.catchup.into(),
        name: args.name,
        model: args.model,
        effort: args.effort,
        cwd: cwd.display().to_string(),
        timeout_s: parse_duration_s(&args.timeout)?,
        keep: args.keep,
        mode: match args.mode {
            CliRunMode::Tui => "tui".to_string(),
            CliRunMode::Exec => "exec".to_string(),
        },
        worktree: args.worktree,
        max_runs: args.max_runs,
        max_duration_s: args
            .max_duration
            .as_deref()
            .map(parse_duration_s)
            .transpose()?,
        last_tick_agent: None,
        last_tick_response: None,
    };
    let id = ctx.store.allocate_id(IdKind::Schedule)?;
    let mut job = JobRow::new(
        id.clone(),
        "schedule",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    job.tz = Some(tz_name);
    job.next_run_at = Some(next_run_at);
    if let Some(expires) = args.expires.as_deref() {
        job.expires_at = Some(
            (Utc::now() + ChronoDuration::seconds(i64::try_from(parse_duration_s(expires)?)?))
                .to_rfc3339(),
        );
    }
    ctx.store.create_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running"}),
    )?;
    ensure_daemon(&ctx.config)?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    print_schedule_created(&job, &spec, json_output);
    Ok(Exit::Ok)
}

fn cmd_schedule_resume(args: ScheduleResumeArgs, json_output: bool) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let mut job = ctx
        .store
        .get_job(&args.id)?
        .ok_or_else(|| anyhow!("job not found: {}", args.id))?;
    if job.job_type != "schedule" {
        return state_conflict(&job.id, &job.status, "schedule");
    }
    let mut spec: ScheduleSpec = serde_json::from_str(&job.spec_json)?;
    if job.ended_reason.as_deref() == Some("fired") && args.at.is_none() {
        return state_conflict(&job.id, &job.status, "re-arm with --at");
    }
    if let Some(at) = args.at {
        let tz_name = job.tz.clone().unwrap_or_else(jobs::current_iana_timezone);
        let tz = jobs::parse_timezone(&tz_name)?;
        let at_utc = jobs::parse_at_time(&at, tz, Utc::now())?;
        spec.trigger = ScheduleTrigger::At {
            at_utc: at_utc.to_rfc3339(),
            original: at,
        };
        job.next_run_at = Some(at_utc.to_rfc3339());
        job.ended_reason = None;
    } else if job.next_run_at.is_none() {
        job.next_run_at = Some(Utc::now().to_rfc3339());
    }
    job.status = "running".to_string();
    job.spec_json = serde_json::to_string(&spec)?;
    ctx.store.update_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running"}),
    )?;
    ensure_daemon(&ctx.config)?;
    if json_output {
        print_ok(job_json(&job));
    } else {
        println!("{}", job.id);
    }
    Ok(Exit::Ok)
}

fn cmd_schedule_from_loop(args: ScheduleFromLoopArgs, json_output: bool) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let loop_job = ctx
        .store
        .get_job(&args.id)?
        .ok_or_else(|| anyhow!("job not found: {}", args.id))?;
    if loop_job.job_type != "loop" {
        return state_conflict(&loop_job.id, &loop_job.status, "loop");
    }
    let loop_spec: LoopSpec = serde_json::from_str(&loop_job.spec_json)?;
    let tz_name = jobs::current_iana_timezone();
    let tz = jobs::parse_timezone(&tz_name)?;
    let trigger = schedule_trigger(args.cron.as_deref(), args.at.as_deref(), tz)?;
    let next_run_at = match &trigger {
        ScheduleTrigger::At { at_utc, .. } => at_utc.clone(),
        ScheduleTrigger::Cron { utc, .. } => jobs::next_cron_after(utc, Utc::now())
            .ok_or_else(|| anyhow!("cron has no future ticks"))?
            .to_rfc3339(),
    };
    let spec = ScheduleSpec {
        harness: loop_spec.harness,
        prompt: loop_spec.prompt,
        prompt_file: loop_spec.prompt_file,
        trigger,
        catchup: CatchupPolicy::Skip,
        name: loop_spec.name,
        model: loop_spec.model,
        effort: loop_spec.effort,
        cwd: loop_spec.cwd,
        timeout_s: loop_spec.timeout_s,
        keep: loop_spec.keep,
        mode: loop_spec.mode,
        worktree: loop_spec.worktree,
        max_runs: loop_spec.max_runs.or(loop_spec.max),
        max_duration_s: loop_spec.max_duration_s,
        last_tick_agent: None,
        last_tick_response: None,
    };
    let id = ctx.store.allocate_id(IdKind::Schedule)?;
    let mut job = JobRow::new(
        id,
        "schedule",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    job.tz = Some(tz_name);
    job.next_run_at = Some(next_run_at);
    ctx.store.create_job(&job)?;
    jobs::append_job_event(
        &ctx.store,
        "job.state",
        &job.id,
        json!({"status": "running", "from_loop": loop_job.id}),
    )?;
    ensure_daemon(&ctx.config)?;
    maybe_auto_open_viewer(&ctx.config, &ctx.herdr);
    print_schedule_created(&job, &spec, json_output);
    Ok(Exit::Ok)
}

fn cmd_schedule_job_alias(command: JobCommand, json_output: bool) -> Result<Exit> {
    cmd_job(JobArgs {
        command,
        json: json_output,
    })
}

struct ContextBundle {
    config: Config,
    store: Store,
    herdr: HerdrClient,
}

impl ContextBundle {
    fn load(session_override: Option<&str>) -> Result<Self> {
        let mut config = Config::load().context("config_error")?;
        if let Some(session) = session_override {
            config.herdr.session = session.to_string();
        }
        let herdr_bin = discover(&config)?;
        let store = Store::open(&config.store_root)?;
        let herdr = HerdrClient::new(herdr_bin, config.herdr.session.clone());
        Ok(Self {
            config,
            store,
            herdr,
        })
    }
}

fn discover(config: &Config) -> Result<PathBuf> {
    discover_herdr(&config.herdr.bin).map_err(|error| match error {
        HerdrError::NotFound => {
            anyhow!("herdr_missing: herdr was not found; install it from {INSTALL_URL}")
        }
        other => anyhow!(other),
    })
}

fn prompt_text(inline: Option<&str>, file: Option<&PathBuf>) -> Result<String> {
    match (inline, file) {
        (Some(text), None) => Ok(text.to_string()),
        (None, Some(path)) if path.as_os_str() == "-" => {
            let mut text = String::new();
            io::stdin().read_to_string(&mut text)?;
            Ok(text)
        }
        (None, Some(path)) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display())),
        (None, None) => bail!("prompt required"),
        (Some(_), Some(_)) => bail!("use exactly one of text or --prompt-file"),
    }
}

pub fn parse_duration_s(value: &str) -> Result<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("duration cannot be empty");
    }
    let (number, multiplier) = match trimmed.chars().last().unwrap() {
        's' => (&trimmed[..trimmed.len() - 1], 1),
        'm' => (&trimmed[..trimmed.len() - 1], 60),
        'h' => (&trimmed[..trimmed.len() - 1], 60 * 60),
        'd' => (&trimmed[..trimmed.len() - 1], 60 * 60 * 24),
        ch if ch.is_ascii_digit() => (trimmed, 1),
        _ => bail!("invalid duration `{value}`; use 45s, 20m, 3h, 30d, or bare seconds"),
    };
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        bail!("invalid duration `{value}`; duration must be a whole number");
    }
    number
        .parse::<u64>()?
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("duration overflow: {value}"))
}

fn wait_state(store: &Store, ids: &[String]) -> Result<WaitState> {
    let mut state = WaitState::default();
    for id in ids {
        let agent = store
            .get_agent(id)?
            .ok_or_else(|| anyhow!("agent not found: {id}"))?;
        match agent.status.as_str() {
            "done" | "idle" => state.completed.push(id.clone()),
            "blocked" => state.blocked.push(id.clone()),
            "killed" => state.blocked.push(id.clone()),
            _ => state.pending.push(id.clone()),
        }
    }
    Ok(state)
}

#[derive(Default)]
struct WaitState {
    completed: Vec<String>,
    pending: Vec<String>,
    blocked: Vec<String>,
}

fn agent_json(agent: &AgentRow) -> Value {
    json!({
        "id": agent.id,
        "name": agent.name,
        "parent_id": agent.parent_id,
        "kind": agent.kind,
        "harness": agent.harness,
        "model": agent.model,
        "effort": agent.effort,
        "host": agent.host,
        "herdr_session": agent.herdr_session,
        "pane_id": agent.pane_id,
        "terminal_id": agent.terminal_id,
        "cwd": agent.cwd,
        "worktree": agent.worktree,
        "status": agent.status,
        "exit_reason": agent.exit_reason,
        "keep": agent.keep,
        "timeout_s": agent.timeout_s,
        "created_at": agent.created_at,
        "ended_at": agent.ended_at,
        "run_dir": agent.run_dir,
    })
}

fn turn_summary_json(turn: &TurnRow) -> Value {
    let prompt_paths: Vec<String> = serde_json::from_str(&turn.prompt_paths).unwrap_or_default();
    json!({
        "n": turn.n,
        "prompt_path": prompt_paths.last(),
        "prompt_paths": prompt_paths,
        "response_path": turn.response_path,
        "response_source": turn.response_source,
        "started_at": turn.started_at,
        "ended_at": turn.ended_at,
        "tokens": tokens_json(turn.tokens_in, turn.tokens_out),
    })
}

fn agent_history_json(store: &Store, agent: &AgentRow) -> Result<Value> {
    let mut value = agent_json(agent);
    let totals = token_totals_for_agent(store, &agent.id)?;
    value["tokens"] = tokens_json(Some(totals.0), Some(totals.1));
    value["turns"] = json!(store.list_turns_by_agent(&agent.id)?.len());
    value["duration_s"] = json!(duration_s(&agent.created_at, agent.ended_at.as_deref()));
    Ok(value)
}

fn tokens_json(input: Option<i64>, output: Option<i64>) -> Value {
    json!({
        "in": input.unwrap_or(0),
        "out": output.unwrap_or(0),
    })
}

pub(crate) fn token_totals_for_agent(store: &Store, agent_id: &str) -> Result<(i64, i64)> {
    let mut input = 0_i64;
    let mut output = 0_i64;
    for turn in store.list_turns_by_agent(agent_id)? {
        input = input.saturating_add(turn.tokens_in.unwrap_or(0));
        output = output.saturating_add(turn.tokens_out.unwrap_or(0));
    }
    Ok((input, output))
}

pub(crate) fn token_totals_for_subtree(
    store: &Store,
    agents: &[AgentRow],
    id: &str,
) -> Result<(i64, i64)> {
    let mut totals = token_totals_for_agent(store, id)?;
    for child in children(agents, id) {
        let child_totals = token_totals_for_subtree(store, agents, &child)?;
        totals.0 = totals.0.saturating_add(child_totals.0);
        totals.1 = totals.1.saturating_add(child_totals.1);
    }
    Ok(totals)
}

pub(crate) fn duration_s(created_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let created = DateTime::parse_from_rfc3339(created_at)
        .ok()?
        .with_timezone(&Utc);
    let ended = ended_at
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    Some(ended.signed_duration_since(created).num_seconds().max(0))
}

fn job_json(job: &JobRow) -> Value {
    let spec: Value = serde_json::from_str(&job.spec_json).unwrap_or(Value::Null);
    json!({
        "id": job.id,
        "type": job.job_type,
        "status": job.status,
        "cadence": spec.get("every").cloned().unwrap_or(Value::Null),
        "next_run": job.next_run_at,
        "expires_at": job.expires_at,
        "runs_count": job.runs_count,
        "created_at": job.created_at,
        "ended_reason": job.ended_reason,
        "next_reason": spec.get("last_next_reason").cloned().unwrap_or(Value::Null),
        "last_tick_agent": spec.get("last_tick_agent").cloned().unwrap_or(Value::Null),
        "last_tick_response": spec.get("last_tick_response").cloned().unwrap_or(Value::Null),
        "judge_independent": spec.get("judge_independent").cloned().unwrap_or(Value::Null),
        "evaluation": if spec.get("judge_independent").and_then(Value::as_bool) == Some(false) {
            Value::String("self-check".to_string())
        } else {
            Value::String("judge".to_string())
        },
        "run_dir": spec.get("run_dir").cloned().unwrap_or(Value::Null),
        "log_path": spec.get("log_path").cloned().unwrap_or(Value::Null),
        "spec": spec,
    })
}

fn event_json(event: &EventRow) -> Value {
    json!({
        "seq": event.seq,
        "time": event.ts,
        "kind": event.kind,
        "type": event.kind,
        "ref_id": event.ref_id,
        "id": event.ref_id,
        "payload": serde_json::from_str::<Value>(&event.payload_json).unwrap_or(Value::Null),
    })
}

fn event_ndjson(event: &EventRow) -> Value {
    json!({
        "type": event.kind,
        "id": event.ref_id,
        "time": event.ts,
        "payload": serde_json::from_str::<Value>(&event.payload_json).unwrap_or(Value::Null),
    })
}

fn print_job_human(job: &JobRow) {
    println!("{} {} {}", job.id, job.job_type, job.status);
    if let Some(next) = &job.next_run_at {
        println!("next_run {next}");
    }
    if let Some(reason) = &job.ended_reason {
        println!("ended_reason {reason}");
    }
    if let Ok(spec) = serde_json::from_str::<Value>(&job.spec_json) {
        if let Some(reason) = spec.get("last_next_reason").and_then(Value::as_str) {
            println!("next_reason {reason}");
        }
    }
}

fn show_json(store: &Store, agent: &AgentRow) -> Result<Value> {
    let turns: Vec<Value> = store
        .list_turns_by_agent(&agent.id)?
        .iter()
        .map(turn_summary_json)
        .collect();
    let children = children(&store.list_agents()?, &agent.id);
    let totals = token_totals_for_agent(store, &agent.id)?;
    Ok(json!({
        "agent": agent_json(agent),
        "turns": turns,
        "tokens": tokens_json(Some(totals.0), Some(totals.1)),
        "children": children,
    }))
}

pub(crate) fn children(agents: &[AgentRow], id: &str) -> Vec<String> {
    agents
        .iter()
        .filter(|agent| agent.parent_id.as_deref() == Some(id))
        .map(|agent| agent.id.clone())
        .collect()
}

pub(crate) fn descendant_ids(agents: &[AgentRow], root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = children(agents, root);
    while let Some(id) = stack.pop() {
        out.push(id.clone());
        stack.extend(children(agents, &id));
    }
    out
}

pub(crate) fn tree_depth(agents: &[AgentRow], id: &str) -> usize {
    let mut depth = 0;
    let mut current = id;
    while let Some(agent) = agents.iter().find(|agent| agent.id == current) {
        let Some(parent) = agent.parent_id.as_deref() else {
            break;
        };
        depth += 1;
        current = parent;
    }
    depth
}

fn tree_node(store: &Store, agents: &[AgentRow], id: &str) -> Result<Option<Value>> {
    let Some(agent) = agents.iter().find(|agent| agent.id == id) else {
        return Ok(None);
    };
    let totals = token_totals_for_subtree(store, agents, id)?;
    Ok(Some(json!({
        "id": agent.id,
        "name": agent.name,
        "status": agent.status,
        "tokens": tokens_json(Some(totals.0), Some(totals.1)),
        "children": children(agents, id)
            .iter()
            .filter_map(|child| tree_node(store, agents, child).transpose())
            .collect::<Result<Vec<_>>>()?
    })))
}

fn print_tree_value(value: &Value, indent: usize) {
    if let Some(roots) = value["roots"].as_array() {
        for root in roots {
            print_node(root, indent);
        }
    }
}

fn print_node(node: &Value, indent: usize) {
    println!(
        "{}{} {} {}",
        " ".repeat(indent),
        node["id"].as_str().unwrap_or_default(),
        node["name"].as_str().unwrap_or_default(),
        node["status"].as_str().unwrap_or_default()
    );
    if let Some(children) = node["children"].as_array() {
        for child in children {
            print_node(child, indent + 2);
        }
    }
}

fn agents_by_id(agents: Vec<AgentRow>) -> HashMap<String, AgentRow> {
    agents
        .into_iter()
        .map(|agent| (agent.id.clone(), agent))
        .collect()
}

fn status_exit(status: &str) -> Exit {
    match status {
        "blocked" => Exit::Blocked,
        "timeout" => Exit::Timeout,
        "killed" => Exit::Killed,
        _ => Exit::Ok,
    }
}

fn parse_every(value: &str) -> Result<EverySpec> {
    if value == "auto" {
        Ok(EverySpec::Auto)
    } else {
        Ok(EverySpec::Fixed(parse_duration_s(value)?))
    }
}

fn cadence_label(spec: &LoopSpec) -> String {
    let base = match spec.every {
        EverySpec::Auto => format!("auto fallback {}s", AUTO_FALLBACK_SECS),
        EverySpec::Fixed(seconds) => format!("{seconds}s"),
    };
    match spec.tick_on.as_deref() {
        Some(cmd) => format!("tick-on `{cmd}` with fallback {base}"),
        None => base,
    }
}

fn schedule_trigger(
    cron: Option<&str>,
    at: Option<&str>,
    tz: chrono_tz::Tz,
) -> Result<ScheduleTrigger> {
    match (cron, at) {
        (Some(cron), None) => {
            let (utc, local, _) = jobs::normalize_cron_utc(cron, tz)?;
            Ok(ScheduleTrigger::Cron { utc, local })
        }
        (None, Some(at)) => {
            let at_utc = jobs::parse_at_time(at, tz, Utc::now())?;
            Ok(ScheduleTrigger::At {
                at_utc: at_utc.to_rfc3339(),
                original: at.to_string(),
            })
        }
        (None, None) => bail!("schedule add needs either a five-field cron or --at <time>"),
        (Some(_), Some(_)) => bail!("use exactly one of cron or --at"),
    }
}

fn print_schedule_created(job: &JobRow, spec: &ScheduleSpec, json_output: bool) {
    if json_output {
        print_ok(job_json(job));
        return;
    }
    let local = job
        .next_run_at
        .as_deref()
        .and_then(|next| jobs::parse_rfc3339_utc(next).ok())
        .and_then(|next| {
            job.tz
                .as_deref()
                .and_then(|tz| jobs::parse_timezone(tz).ok())
                .map(|tz| next.with_timezone(&tz).to_rfc3339())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let utc = job.next_run_at.as_deref().unwrap_or("unknown");
    let cadence = match &spec.trigger {
        ScheduleTrigger::Cron { utc, local } => format!("cron local `{local}` stored UTC `{utc}`"),
        ScheduleTrigger::At { original, .. } => format!("one-shot `{original}`"),
    };
    println!(
        "{} schedule {} next: {} = {} cancel: orcr kill {}",
        job.id, cadence, local, utc, job.id
    );
}

fn ensure_daemon(config: &Config) -> Result<()> {
    daemon::start_background(config).map(|_| ())
}

fn run_foreground_loop(spec: LoopSpec, json_output: bool) -> Result<Exit> {
    let mut ctx = ContextBundle::load(None)?;
    let id = ctx.store.allocate_id(IdKind::Loop)?;
    let mut job = JobRow::new(
        id.clone(),
        "loop",
        serde_json::to_string(&spec)?,
        "running",
        Utc::now().to_rfc3339(),
    );
    job.next_run_at = Some(Utc::now().to_rfc3339());
    ctx.store.create_job(&job)?;
    loop {
        jobs::run_loop_tick(&ctx.config, &mut ctx.store, ctx.herdr.clone(), &mut job)?;
        if job.status != "running" {
            break;
        }
        let next = job
            .next_run_at
            .as_deref()
            .and_then(|value| jobs::parse_rfc3339_utc(value).ok())
            .unwrap_or_else(Utc::now);
        let sleep = next.signed_duration_since(Utc::now()).num_seconds().max(0);
        thread::sleep(Duration::from_secs(u64::try_from(sleep).unwrap_or(0)));
        job = ctx
            .store
            .get_job(&id)?
            .ok_or_else(|| anyhow!("job not found: {id}"))?;
    }
    if json_output {
        print_ok(job_json(&job));
    } else {
        println!("{}", job.id);
    }
    Ok(Exit::Ok)
}

fn is_job_ref(value: &str) -> bool {
    let Some(prefix) = value.chars().next() else {
        return false;
    };
    matches!(prefix, 'l' | 's' | 'g' | 'w')
        && value.len() > 1
        && value[1..].chars().all(|ch| ch.is_ascii_digit())
}

fn state_conflict<T>(id: &str, current_status: &str, wanted: &str) -> Result<T> {
    bail!("state_conflict: id={id} current_status={current_status} wanted={wanted}")
}

fn classify_error(error: &anyhow::Error) -> (Exit, &'static str, String, Option<Value>) {
    let message = error.to_string();
    if message.contains("herdr_missing") || message.contains("config_error") {
        return (Exit::Env, "env_config", message, None);
    }
    if message.contains("agent not found") {
        return (Exit::NotFound, "not_found", message, None);
    }
    if let Some(details) = parse_state_conflict(&message) {
        return (
            Exit::StateConflict,
            "state_conflict",
            message,
            Some(details),
        );
    }
    (Exit::Other, "error", message, None)
}

fn parse_state_conflict(message: &str) -> Option<Value> {
    if !message.contains("state_conflict") {
        return None;
    }
    let mut id = None;
    let mut current_status = None;
    let mut wanted = None;
    for part in message.split_whitespace() {
        if let Some(value) = part.strip_prefix("id=") {
            id = Some(value.to_string());
        } else if let Some(value) = part.strip_prefix("current_status=") {
            current_status = Some(value.to_string());
        } else if let Some(value) = part.strip_prefix("wanted=") {
            wanted = Some(value.to_string());
        }
    }
    Some(json!({
        "id": id,
        "current_status": current_status,
        "wanted": wanted,
    }))
}

fn print_ok(result: Value) {
    println!("{}", json!({"ok": true, "result": result}));
}

fn print_error(json_output: bool, code: &str, message: &str, details: Option<Value>) {
    if json_output {
        let mut error = json!({"code": code, "message": message});
        if let Some(details) = details {
            error["details"] = details;
        }
        println!("{}", json!({"ok": false, "error": error}));
    } else {
        eprintln!("{message}");
    }
}

fn command_json(command: &Command) -> bool {
    match command {
        Command::Run(args) => args.json,
        Command::Send(args) => args.json,
        Command::Wait(args) => args.json,
        Command::Out(args) => args.json || args.format == OutFormat::Json,
        Command::Show(args) => args.json,
        Command::Ps(args) => args.json,
        Command::Tree(args) => args.json,
        Command::Kill(args) => args.json,
        Command::Attach(_) => false,
        Command::Status(args) => args.json,
        Command::History(args) => args.json,
        Command::Gc(args) => args.json,
        Command::Loop(args) => args.json,
        Command::Goal(args) => args.json,
        Command::Workflow(args) => args.json,
        Command::Schedule(args) => args.json,
        Command::Job(args) => args.json,
        Command::Top(_) => false,
        Command::Events(args) => args.json,
        Command::Serve(_) => false,
    }
}

fn print_human_status(report: &StatusReport) {
    println!("herdr: {} ({})", report.herdr.version, report.herdr.path);
    println!(
        "session: {} ({})",
        report.session.name,
        if report.session.running {
            "running"
        } else if report.session.exists {
            "stopped"
        } else {
            "missing"
        }
    );
    println!("store: {} (ok)", report.store.root);
    if report.db.ok {
        println!("db: {} (ok)", report.db.path);
    } else {
        println!(
            "db: {} (error: {})",
            report.db.path,
            report.db.error.as_deref().unwrap_or("unknown")
        );
    }
    println!(
        "daemon: {}{}",
        if report.daemon.running {
            "running"
        } else {
            "stopped"
        },
        report
            .daemon
            .pid
            .map(|pid| format!(" pid={pid}"))
            .unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_suffixes_and_bare_seconds() {
        assert_eq!(parse_duration_s("45s").unwrap(), 45);
        assert_eq!(parse_duration_s("20m").unwrap(), 1_200);
        assert_eq!(parse_duration_s("3h").unwrap(), 10_800);
        assert_eq!(parse_duration_s("30d").unwrap(), 2_592_000);
        assert_eq!(parse_duration_s("9").unwrap(), 9);
        assert!(parse_duration_s("1ms").is_err());
        assert!(parse_duration_s("1.5m").is_err());
    }

    #[test]
    fn parses_run_args_surface() {
        let cli = Cli::try_parse_from([
            "orcr", "run", "-a", "mock", "-p", "hi", "--name", "worker", "--mode", "exec",
            "--wait", "--json",
        ])
        .unwrap();
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(args.harness, "mock");
        assert_eq!(args.prompt.as_deref(), Some("hi"));
        assert_eq!(args.name.as_deref(), Some("worker"));
        assert_eq!(args.mode, CliRunMode::Exec);
        assert!(args.wait);
        assert!(args.json);
    }

    #[test]
    fn run_requires_exactly_one_prompt_source() {
        assert!(Cli::try_parse_from(["orcr", "run", "-a", "mock"]).is_err());
        assert!(Cli::try_parse_from([
            "orcr",
            "run",
            "-a",
            "mock",
            "-p",
            "hi",
            "--prompt-file",
            "p.md"
        ])
        .is_err());
    }

    #[test]
    fn parses_send_intent_flags() {
        let cli = Cli::try_parse_from(["orcr", "send", "a1", "hi", "--steer", "--json"]).unwrap();
        let Some(Command::Send(args)) = cli.command else {
            panic!("expected send");
        };
        assert_eq!(args.id, "a1");
        assert_eq!(args.text.as_deref(), Some("hi"));
        assert!(args.steer);
        assert!(args.json);
        assert!(Cli::try_parse_from(["orcr", "send", "a1", "hi", "--steer", "--turn"]).is_err());
    }

    #[test]
    fn parses_job_loop_events_and_serve_surface() {
        let cli = Cli::try_parse_from([
            "orcr",
            "loop",
            "-a",
            "mock",
            "--prompt-file",
            "p.md",
            "--every",
            "auto",
            "--tick-on",
            "test -f ready",
            "--max",
            "3",
            "--until",
            "ALL PASS",
            "--foreground",
            "--json",
        ])
        .unwrap();
        let Some(Command::Loop(args)) = cli.command else {
            panic!("expected loop");
        };
        assert_eq!(args.every, "auto");
        assert_eq!(args.max, Some(3));
        assert!(args.foreground);
        assert!(args.json);

        let cli = Cli::try_parse_from(["orcr", "job", "pause", "l1", "--json"]).unwrap();
        let Some(Command::Job(args)) = cli.command else {
            panic!("expected job");
        };
        assert!(args.json);
        assert!(matches!(args.command, JobCommand::Pause(_)));

        let cli = Cli::try_parse_from(["orcr", "events", "--follow", "--json"]).unwrap();
        let Some(Command::Events(args)) = cli.command else {
            panic!("expected events");
        };
        assert!(args.follow);
        assert!(args.json);

        let cli = Cli::try_parse_from(["orcr", "serve", "--foreground"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Serve(ServeArgs { foreground: true }))
        ));
    }
}
