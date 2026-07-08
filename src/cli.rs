use std::collections::{HashMap, HashSet};
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
use crate::engine::{Engine, RunMode, RunRequest};
use crate::herdr::{discover_herdr, HerdrClient, HerdrError, INSTALL_URL};
use crate::profile;
use crate::store::{AgentRow, Store, TurnRow};

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

#[derive(Debug, Serialize)]
struct StatusReport {
    herdr: HerdrStatus,
    session: SessionStatus,
    store: StoreStatus,
    db: DbStatus,
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
                .filter_map(|id| tree_node(&agents, id))
                .collect::<Vec<_>>()
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
    let agents = ctx.store.list_agents()?;
    let mut ids: Vec<String> = args
        .ids
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
    let value = json!({ "killed": killed, "skipped": skipped });
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
    let items: Vec<Value> = agents.iter().map(agent_json).collect();
    if args.json {
        print_ok(json!({ "items": items }));
    } else {
        for item in items {
            println!(
                "{}\t{}\t{}\t{}",
                item["id"].as_str().unwrap_or_default(),
                item["status"].as_str().unwrap_or_default(),
                item["harness"].as_str().unwrap_or_default(),
                item["created_at"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(Exit::Ok)
}

fn cmd_gc(args: GcArgs) -> Result<Exit> {
    let ctx = ContextBundle::load(None)?;
    let agents = ctx.store.list_agents()?;
    let live_panes = ctx
        .herdr
        .pane_list()
        .map(|list| list.panes)
        .unwrap_or_default();
    let known_panes: HashSet<String> = agents.iter().filter_map(|a| a.pane_id.clone()).collect();
    let live_pane_ids: HashSet<String> = live_panes.iter().map(|p| p.pane_id.clone()).collect();
    let mut killed_unknown = Vec::new();
    for pane in live_panes {
        if pane
            .label
            .as_deref()
            .is_some_and(|label| label.starts_with("a"))
            && !known_panes.contains(&pane.pane_id)
        {
            if !args.dry_run {
                let _ = ctx.herdr.pane_close(&pane.pane_id);
            }
            killed_unknown.push(pane.pane_id);
        }
    }
    let mut marked_lost = Vec::new();
    for agent in agents {
        if matches!(
            agent.status.as_str(),
            "working" | "idle" | "blocked" | "starting"
        ) && agent
            .pane_id
            .as_ref()
            .is_some_and(|pane| !live_pane_ids.contains(pane))
        {
            if !args.dry_run {
                ctx.store.update_agent_status(
                    &agent.id,
                    "lost",
                    Some("pane_gone"),
                    Some(&Utc::now().to_rfc3339()),
                )?;
            }
            marked_lost.push(agent.id);
        }
    }
    let value = json!({
        "killed_unknown_panes": killed_unknown,
        "marked_lost": marked_lost,
        "deleted_stale_sessions": [],
        "dry_run": args.dry_run,
    });
    if args.json {
        print_ok(value);
    } else {
        println!("{}", value);
    }
    Ok(Exit::Ok)
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
    })
}

fn show_json(store: &Store, agent: &AgentRow) -> Result<Value> {
    let turns: Vec<Value> = store
        .list_turns_by_agent(&agent.id)?
        .iter()
        .map(turn_summary_json)
        .collect();
    let children = children(&store.list_agents()?, &agent.id);
    Ok(json!({
        "agent": agent_json(agent),
        "turns": turns,
        "children": children,
    }))
}

fn children(agents: &[AgentRow], id: &str) -> Vec<String> {
    agents
        .iter()
        .filter(|agent| agent.parent_id.as_deref() == Some(id))
        .map(|agent| agent.id.clone())
        .collect()
}

fn descendant_ids(agents: &[AgentRow], root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = children(agents, root);
    while let Some(id) = stack.pop() {
        out.push(id.clone());
        stack.extend(children(agents, &id));
    }
    out
}

fn tree_depth(agents: &[AgentRow], id: &str) -> usize {
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

fn tree_node(agents: &[AgentRow], id: &str) -> Option<Value> {
    let agent = agents.iter().find(|agent| agent.id == id)?;
    Some(json!({
        "id": agent.id,
        "name": agent.name,
        "status": agent.status,
        "children": children(agents, id)
            .iter()
            .filter_map(|child| tree_node(agents, child))
            .collect::<Vec<_>>()
    }))
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
}
