use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal};
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::json;

use crate::cli::{children, descendant_ids, token_totals_for_agent, token_totals_for_subtree};
use crate::config::Config;
use crate::daemon;
use crate::engine::Engine;
use crate::herdr::HerdrClient;
use crate::profile;
use crate::store::{AgentRow, JobRow, Store};

/// The pane label used to guard against opening more than one live viewer per herdr
/// session. Also used by `daemon::reconcile`'s "kill unknown a*-labeled pane" pass to
/// leave the viewer pane alone (it does not start with `a`).
pub const AUTO_VIEWER_LABEL: &str = "orcr-top";

/// How many characters of the latest response file to show in the detail pane.
pub const SNIPPET_CHARS: usize = 300;

const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------------
// Pure tree-building logic (unit tested below; no herdr/TTY involved).
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Agent,
    Job,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub id: String,
    pub name: String,
    pub kind: EntryKind,
    pub depth: usize,
    pub status: String,
    pub harness: String,
    /// True if this node or any descendant is `blocked` — the needs-a-human queue.
    pub blocked_subtree: bool,
}

enum Root<'a> {
    Job(&'a JobRow),
    Agent(&'a AgentRow),
}

fn root_created_at<'a>(root: &Root<'a>) -> &'a str {
    match root {
        Root::Job(j) => &j.created_at,
        Root::Agent(a) => &a.created_at,
    }
}

fn root_id<'a>(root: &Root<'a>) -> &'a str {
    match root {
        Root::Job(j) => &j.id,
        Root::Agent(a) => &a.id,
    }
}

fn root_blocked(root: &Root<'_>, child_map: &HashMap<&str, Vec<&AgentRow>>) -> bool {
    match root {
        Root::Job(_) => false,
        Root::Agent(a) => subtree_has_blocked(a, child_map),
    }
}

/// Flattens agents + jobs into a single display tree (jobs and orphaned agents are
/// roots; an agent whose `parent_id` names a live job or agent nests under it).
/// Siblings are ordered with any subtree containing a blocked node first (stable
/// otherwise, in creation order) so the operator's attention is drawn upward.
pub fn flatten_tree(agents: &[AgentRow], jobs: &[JobRow]) -> Vec<TreeEntry> {
    let mut known_ids: HashSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
    known_ids.extend(agents.iter().map(|a| a.id.as_str()));

    let mut child_map: HashMap<&str, Vec<&AgentRow>> = HashMap::new();
    for agent in agents {
        let parent_key = match &agent.parent_id {
            Some(pid) if known_ids.contains(pid.as_str()) => pid.as_str(),
            _ => "",
        };
        child_map.entry(parent_key).or_default().push(agent);
    }
    for siblings in child_map.values_mut() {
        siblings.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    }

    let mut roots: Vec<Root> = jobs.iter().map(Root::Job).collect();
    for agent in agents {
        let is_root = match &agent.parent_id {
            None => true,
            Some(pid) => !known_ids.contains(pid.as_str()),
        };
        if is_root {
            roots.push(Root::Agent(agent));
        }
    }
    roots.sort_by(|a, b| {
        root_blocked(b, &child_map)
            .cmp(&root_blocked(a, &child_map))
            .then(root_created_at(a).cmp(root_created_at(b)))
            .then(root_id(a).cmp(root_id(b)))
    });

    let mut out = Vec::new();
    for root in &roots {
        match root {
            Root::Job(job) => push_job(job, &child_map, 0, &mut out),
            Root::Agent(agent) => push_agent(agent, &child_map, 0, &mut out),
        }
    }
    out
}

fn subtree_has_blocked(agent: &AgentRow, child_map: &HashMap<&str, Vec<&AgentRow>>) -> bool {
    if agent.status == "blocked" {
        return true;
    }
    child_map
        .get(agent.id.as_str())
        .into_iter()
        .flatten()
        .any(|child| subtree_has_blocked(child, child_map))
}

fn push_job<'a>(
    job: &'a JobRow,
    child_map: &HashMap<&'a str, Vec<&'a AgentRow>>,
    depth: usize,
    out: &mut Vec<TreeEntry>,
) {
    let harness = job_harness(job);
    out.push(TreeEntry {
        id: job.id.clone(),
        name: job_label(job),
        kind: EntryKind::Job,
        depth,
        status: job.status.clone(),
        harness,
        blocked_subtree: false,
    });
    push_children(job.id.as_str(), child_map, depth + 1, out);
}

fn push_agent<'a>(
    agent: &'a AgentRow,
    child_map: &HashMap<&'a str, Vec<&'a AgentRow>>,
    depth: usize,
    out: &mut Vec<TreeEntry>,
) {
    out.push(TreeEntry {
        id: agent.id.clone(),
        name: agent.name.clone().unwrap_or_default(),
        kind: EntryKind::Agent,
        depth,
        status: agent.status.clone(),
        harness: agent.harness.clone(),
        blocked_subtree: subtree_has_blocked(agent, child_map),
    });
    push_children(agent.id.as_str(), child_map, depth + 1, out);
}

fn push_children<'a>(
    parent_id: &str,
    child_map: &HashMap<&'a str, Vec<&'a AgentRow>>,
    depth: usize,
    out: &mut Vec<TreeEntry>,
) {
    let Some(kids) = child_map.get(parent_id) else {
        return;
    };
    let mut ordered: Vec<&&AgentRow> = kids.iter().collect();
    ordered.sort_by(|a, b| {
        subtree_has_blocked(b, child_map)
            .cmp(&subtree_has_blocked(a, child_map))
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
    for agent in ordered {
        push_agent(agent, child_map, depth, out);
    }
}

fn job_label(job: &JobRow) -> String {
    let spec: serde_json::Value =
        serde_json::from_str(&job.spec_json).unwrap_or(serde_json::Value::Null);
    spec.get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| job.job_type.clone())
}

fn job_harness(job: &JobRow) -> String {
    let spec: serde_json::Value =
        serde_json::from_str(&job.spec_json).unwrap_or(serde_json::Value::Null);
    spec.get("harness")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default()
}

/// Filters a flattened tree to the rows that should currently be drawn: children of a
/// collapsed node are hidden. Relies on `entries` being in DFS pre-order (guaranteed by
/// `flatten_tree`), so a single depth watermark is enough.
pub fn visible_entries<'a>(
    entries: &'a [TreeEntry],
    collapsed: &HashSet<String>,
) -> Vec<&'a TreeEntry> {
    let mut out = Vec::with_capacity(entries.len());
    let mut hide_below: Option<usize> = None;
    for entry in entries {
        if let Some(depth) = hide_below {
            if entry.depth > depth {
                continue;
            }
            hide_below = None;
        }
        out.push(entry);
        if collapsed.contains(&entry.id) {
            hide_below = Some(entry.depth);
        }
    }
    out
}

/// Case-insensitive substring filter over id/name/status/harness.
pub fn filter_entries<'a>(entries: &[&'a TreeEntry], needle: &str) -> Vec<&'a TreeEntry> {
    if needle.is_empty() {
        return entries.to_vec();
    }
    let needle = needle.to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            entry.id.to_lowercase().contains(&needle)
                || entry.name.to_lowercase().contains(&needle)
                || entry.status.to_lowercase().contains(&needle)
                || entry.harness.to_lowercase().contains(&needle)
        })
        .copied()
        .collect()
}

/// First ~`max_chars` characters of a response body, trimmed, with an ellipsis marker
/// when truncated. Operates on chars (not bytes) so it never splits a UTF-8 sequence.
pub fn response_snippet(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let total = trimmed.chars().count();
    if total <= max_chars {
        return trimmed.to_string();
    }
    let mut snippet: String = trimmed.chars().take(max_chars).collect();
    snippet.push('…');
    snippet
}

// ---------------------------------------------------------------------------------
// Auto-viewer guard logic (pure; herdr calls are a thin wrapper below).
// ---------------------------------------------------------------------------------

/// True when a spawn/job-creation should attempt to open the viewer pane: only inside
/// a herdr pane (`HERDR_ENV=1`) with `viewer.auto = true` in config.
pub fn auto_viewer_enabled(herdr_env: Option<&str>, viewer_auto: bool) -> bool {
    herdr_env == Some("1") && viewer_auto
}

/// True when no pane in the session is already labeled `orcr-top` — the once-per-session
/// guard for both the auto-viewer and `orcr top --pane`.
pub fn should_open_viewer_pane(pane_labels: &[Option<String>]) -> bool {
    !pane_labels
        .iter()
        .any(|label| label.as_deref() == Some(AUTO_VIEWER_LABEL))
}

/// Splits a `--no-focus` pane beside `source_pane`, labels it `orcr-top`, and runs
/// `orcr top` in it. No-ops (returns `Ok(false)`) if a labeled pane already exists.
///
/// `HERDR_ENV`/`HERDR_PANE_ID`/etc. are injected by herdr itself for every pane it
/// manages, but `ORCR_STORE` (and a configured non-default herdr binary) are orcr's own
/// env vars — they must be threaded through explicitly so the split pane reads the same
/// store as the caller instead of falling back to `~/.orcr`.
pub fn open_viewer_pane(
    herdr: &HerdrClient,
    config: &Config,
    source_pane: &str,
    orcr_bin: &str,
) -> Result<bool> {
    let panes = herdr.pane_list()?;
    let labels: Vec<Option<String>> = panes.panes.iter().map(|p| p.label.clone()).collect();
    if !should_open_viewer_pane(&labels) {
        return Ok(false);
    }
    let split = herdr.pane_split(source_pane, "right", 0.35)?;
    herdr.pane_rename(&split.pane_id, AUTO_VIEWER_LABEL)?;
    herdr.pane_run(&split.pane_id, &viewer_command(config, orcr_bin))?;
    Ok(true)
}

fn viewer_command(config: &Config, orcr_bin: &str) -> String {
    let mut prefix = format!(
        "ORCR_STORE={}",
        shell_quote(&config.store_root.display().to_string())
    );
    if !config.herdr.bin.trim().is_empty() {
        prefix.push_str(&format!(
            " ORCR_HERDR_BIN={}",
            shell_quote(&config.herdr.bin)
        ));
    }
    format!("{prefix} {} top", shell_quote(orcr_bin))
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub fn is_headless() -> bool {
    !io::stdout().is_terminal()
}

// ---------------------------------------------------------------------------------
// The interactive TUI.
// ---------------------------------------------------------------------------------

enum Mode {
    Normal,
    Filter(String),
    Send { id: String, buf: String },
    ConfirmKill { id: String, tree: bool },
}

struct App {
    agents: Vec<AgentRow>,
    jobs: Vec<JobRow>,
    entries: Vec<TreeEntry>,
    collapsed: HashSet<String>,
    selected: usize,
    mode: Mode,
    filter: String,
    status: String,
    last_seq: i64,
    quit: bool,
}

pub fn run_tui(config: Config, mut store: Store, herdr: HerdrClient) -> Result<()> {
    enable_raw_mode().context("failed to enable raw terminal mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to init terminal")?;

    let outcome = event_loop(&mut terminal, &config, &mut store, &herdr);

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    outcome
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
) -> Result<()> {
    let agents = store.list_agents()?;
    let jobs = store.list_jobs()?;
    let entries = flatten_tree(&agents, &jobs);
    let last_seq = store.max_event_seq()?;
    let mut app = App {
        agents,
        jobs,
        entries,
        collapsed: HashSet::new(),
        selected: 0,
        mode: Mode::Normal,
        filter: String::new(),
        status: String::new(),
        last_seq,
        quit: false,
    };

    while !app.quit {
        terminal.draw(|frame| draw(frame, &app, store))?;

        if event::poll(REFRESH_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key.code, config, store, herdr, terminal)?;
                }
            }
        }

        let seq = store.max_event_seq()?;
        if seq != app.last_seq {
            app.last_seq = seq;
            app.agents = store.list_agents()?;
            app.jobs = store.list_jobs()?;
            app.entries = flatten_tree(&app.agents, &app.jobs);
        }
    }
    Ok(())
}

fn current_rows(app: &App) -> Vec<&TreeEntry> {
    let visible = visible_entries(&app.entries, &app.collapsed);
    filter_entries(&visible, &app.filter)
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    app: &mut App,
    code: KeyCode,
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    match &mut app.mode {
        Mode::Normal => handle_normal_key(app, code, config, store, herdr, terminal),
        Mode::Filter(_) => handle_filter_key(app, code),
        Mode::Send { .. } => handle_send_key(app, code, config, store, herdr),
        Mode::ConfirmKill { .. } => handle_confirm_kill_key(app, code, config, store, herdr),
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_normal_key(
    app: &mut App,
    code: KeyCode,
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let row_count = current_rows(app).len();
    match code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Up => app.selected = app.selected.saturating_sub(1),
        KeyCode::Down => {
            if row_count > 0 {
                app.selected = (app.selected + 1).min(row_count - 1);
            }
        }
        KeyCode::PageUp => app.selected = app.selected.saturating_sub(10),
        KeyCode::PageDown => {
            if row_count > 0 {
                app.selected = (app.selected + 10).min(row_count - 1);
            }
        }
        KeyCode::Char(' ') => {
            if let Some(entry) = current_rows(app).get(app.selected) {
                let id = entry.id.clone();
                if app.collapsed.contains(&id) {
                    app.collapsed.remove(&id);
                } else {
                    app.collapsed.insert(id);
                }
            }
        }
        KeyCode::Enter => attach_selected(app, store, herdr, terminal)?,
        KeyCode::Char('s') => {
            if let Some(entry) = current_rows(app).get(app.selected) {
                if entry.kind == EntryKind::Agent {
                    app.mode = Mode::Send {
                        id: entry.id.clone(),
                        buf: String::new(),
                    };
                } else {
                    app.status = "select an agent row to send".to_string();
                }
            }
        }
        KeyCode::Char('k') => {
            if let Some(entry) = current_rows(app).get(app.selected) {
                app.mode = Mode::ConfirmKill {
                    id: entry.id.clone(),
                    tree: false,
                };
            }
        }
        KeyCode::Char('K') => {
            if let Some(entry) = current_rows(app).get(app.selected) {
                app.mode = Mode::ConfirmKill {
                    id: entry.id.clone(),
                    tree: true,
                };
            }
        }
        KeyCode::Char('o') => open_selected_response(app, store, terminal)?,
        KeyCode::Char('/') => {
            app.mode = Mode::Filter(app.filter.clone());
        }
        KeyCode::Char('g') => {
            match daemon::reconcile(config, false) {
                Ok(report) => {
                    app.status = format!(
                        "gc: killed {} lost {} readmitted {}",
                        report.killed_unknown_panes.len(),
                        report.marked_lost.len(),
                        report.readmitted_queued.len()
                    );
                }
                Err(error) => app.status = format!("gc failed: {error}"),
            }
            app.agents = store.list_agents()?;
            app.jobs = store.list_jobs()?;
            app.entries = flatten_tree(&app.agents, &app.jobs);
        }
        _ => {}
    }
    Ok(())
}

fn handle_filter_key(app: &mut App, code: KeyCode) -> Result<()> {
    let Mode::Filter(buf) = &mut app.mode else {
        return Ok(());
    };
    match code {
        KeyCode::Enter => {
            app.filter = buf.clone();
            app.selected = 0;
            app.mode = Mode::Normal;
        }
        KeyCode::Esc => {
            app.mode = Mode::Normal;
        }
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char(ch) => buf.push(ch),
        _ => {}
    }
    Ok(())
}

fn handle_send_key(
    app: &mut App,
    code: KeyCode,
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
) -> Result<()> {
    let Mode::Send { id, buf } = &mut app.mode else {
        return Ok(());
    };
    match code {
        KeyCode::Enter => {
            let id = id.clone();
            let text = buf.clone();
            app.mode = Mode::Normal;
            if text.trim().is_empty() {
                return Ok(());
            }
            match send_prompt(config, store, herdr, &id, &text) {
                Ok(mode) => app.status = format!("{id}: sent ({mode})"),
                Err(error) => app.status = format!("{id}: send failed: {error}"),
            }
            app.agents = store.list_agents()?;
            app.entries = flatten_tree(&app.agents, &app.jobs);
        }
        KeyCode::Esc => app.mode = Mode::Normal,
        KeyCode::Backspace => {
            buf.pop();
        }
        KeyCode::Char(ch) => buf.push(ch),
        _ => {}
    }
    Ok(())
}

fn handle_confirm_kill_key(
    app: &mut App,
    code: KeyCode,
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
) -> Result<()> {
    let Mode::ConfirmKill { id, tree } = &app.mode else {
        return Ok(());
    };
    let id = id.clone();
    let tree = *tree;
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            app.mode = Mode::Normal;
            let result = if tree {
                kill_tree(config, store, herdr, &id)
            } else {
                kill_one(config, store, herdr, &id).map(usize::from)
            };
            match result {
                Ok(count) => app.status = format!("killed {count} for {id}"),
                Err(error) => app.status = format!("{id}: kill failed: {error}"),
            }
            app.agents = store.list_agents()?;
            app.jobs = store.list_jobs()?;
            app.entries = flatten_tree(&app.agents, &app.jobs);
        }
        _ => app.mode = Mode::Normal,
    }
    Ok(())
}

fn attach_selected(
    app: &mut App,
    store: &mut Store,
    herdr: &HerdrClient,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Some(entry) = current_rows(app).get(app.selected).copied() else {
        return Ok(());
    };
    if entry.kind != EntryKind::Agent {
        app.status = "select an agent row to attach".to_string();
        return Ok(());
    }
    let Some(agent) = store.get_agent(&entry.id)? else {
        app.status = format!("{}: not found", entry.id);
        return Ok(());
    };
    if !matches!(
        agent.status.as_str(),
        "working" | "idle" | "blocked" | "starting"
    ) {
        app.status = format!("{}: not live ({})", agent.id, agent.status);
        return Ok(());
    }
    suspend_tui(terminal, || herdr.session_attach().map_err(Into::into))?;
    Ok(())
}

fn open_selected_response(
    app: &mut App,
    store: &mut Store,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Some(entry) = current_rows(app).get(app.selected).copied() else {
        return Ok(());
    };
    if entry.kind != EntryKind::Agent {
        app.status = "select an agent row to open".to_string();
        return Ok(());
    }
    let turns = store.list_turns_by_agent(&entry.id)?;
    let Some(turn) = turns.last() else {
        app.status = format!("{}: no turns yet", entry.id);
        return Ok(());
    };
    if !std::path::Path::new(&turn.response_path).exists() {
        app.status = format!("{}: no response file yet", entry.id);
        return Ok(());
    }
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    let path = turn.response_path.clone();
    suspend_tui(terminal, || {
        let status = Command::new(&pager).arg(&path).status()?;
        if !status.success() {
            bail!("{pager} exited with {status}");
        }
        Ok(())
    })
}

fn suspend_tui<F>(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, action: F) -> Result<()>
where
    F: FnOnce() -> Result<()>,
{
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    let result = action();
    execute!(terminal.backend_mut(), EnterAlternateScreen).ok();
    enable_raw_mode().ok();
    terminal.clear().ok();
    result
}

fn send_prompt(
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
    id: &str,
    text: &str,
) -> Result<&'static str> {
    let agent = store
        .get_agent(id)?
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    let mode = match agent.status.as_str() {
        "working" => "steer",
        "idle" => "turn",
        other => bail!("cannot send while status is {other}"),
    };
    let mut engine = Engine::new(config, store, herdr.clone());
    if mode == "steer" {
        engine.steer(id, text)?;
    } else {
        engine.turn(id, text)?;
    }
    Ok(mode)
}

fn kill_one(config: &Config, store: &mut Store, herdr: &HerdrClient, id: &str) -> Result<bool> {
    if kill_job(store, id)? {
        return Ok(true);
    }
    let agent = store
        .get_agent(id)?
        .ok_or_else(|| anyhow!("agent not found: {id}"))?;
    let Some(profile) = profile::lookup(&agent.harness) else {
        return Ok(false);
    };
    let mut engine = Engine::new(config, store, herdr.clone());
    engine.kill_agent(profile.as_ref(), id)
}

/// Mirrors `orcr kill <job-id>`: mark a running/paused job killed and disarm it.
fn kill_job(store: &Store, id: &str) -> Result<bool> {
    let Some(mut job) = store.get_job(id)? else {
        return Ok(false);
    };
    if job.status == "running" || job.status == "paused" {
        job.status = "killed".to_string();
        job.ended_reason = Some("killed".to_string());
        job.next_run_at = None;
        store.update_job(&job)?;
        crate::jobs::append_job_event(store, "job.state", &job.id, json!({"status": "killed"}))?;
    }
    Ok(true)
}

fn kill_tree(
    config: &Config,
    store: &mut Store,
    herdr: &HerdrClient,
    root_id: &str,
) -> Result<usize> {
    let agents = store.list_agents()?;
    let mut ids = descendant_ids(&agents, root_id);
    ids.push(root_id.to_string());
    ids.sort_by_key(|id| crate::cli::tree_depth(&agents, id));
    ids.reverse();
    ids.dedup();
    let mut count = 0;
    for id in ids {
        if kill_one(config, store, herdr, &id).unwrap_or(false) {
            count += 1;
        }
    }
    Ok(count)
}

// ---------------------------------------------------------------------------------
// Rendering.
// ---------------------------------------------------------------------------------

fn draw(frame: &mut Frame, app: &App, store: &Store) {
    let area = frame.area();
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title(frame, root[0], app);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(root[1]);

    let rows = current_rows(app);
    draw_tree(frame, columns[0], app, &rows);
    draw_detail(frame, columns[1], app, &rows, store);

    draw_status(frame, root[2], app);
}

fn draw_title(frame: &mut Frame, area: Rect, app: &App) {
    let agent_count = app.agents.len();
    let job_count = app.jobs.len();
    let title = format!("orcr top · {agent_count} agents · {job_count} jobs");
    frame.render_widget(
        Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_tree(frame: &mut Frame, area: Rect, app: &App, rows: &[&TreeEntry]) {
    let items: Vec<ListItem> = rows
        .iter()
        .map(|entry| {
            let indent = "  ".repeat(entry.depth);
            let (glyph, color) = status_glyph(&entry.status, entry.kind);
            let warn = if entry.blocked_subtree && entry.status != "blocked" {
                " ⚠"
            } else {
                ""
            };
            let label = if entry.name.is_empty() {
                entry.id.clone()
            } else {
                format!("{} {}", entry.id, entry.name)
            };
            let line = Line::from(vec![
                Span::raw(indent),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::raw(label),
                Span::styled(warn, Style::default().fg(Color::Yellow)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    if !rows.is_empty() {
        state.select(Some(app.selected.min(rows.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("tree"))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, rows: &[&TreeEntry], store: &Store) {
    let block = Block::default().borders(Borders::ALL).title("detail");
    let Some(entry) = rows.get(app.selected).copied() else {
        frame.render_widget(Paragraph::new("no selection").block(block), area);
        return;
    };
    let mut lines = vec![Line::from(format!("id       {}", entry.id))];
    match entry.kind {
        EntryKind::Job => {
            if let Some(job) = app.jobs.iter().find(|j| j.id == entry.id) {
                lines.push(Line::from(format!("type     {}", job.job_type)));
                lines.push(Line::from(format!("status   {}", job.status)));
                lines.push(Line::from(format!(
                    "next_run {}",
                    job.next_run_at.as_deref().unwrap_or("-")
                )));
                lines.push(Line::from(format!("runs     {}", job.runs_count)));
            }
        }
        EntryKind::Agent => {
            if let Some(agent) = app.agents.iter().find(|a| a.id == entry.id) {
                lines.push(Line::from(format!("name     {}", entry.name)));
                lines.push(Line::from(format!("status   {}", agent.status)));
                lines.push(Line::from(format!(
                    "harness  {} · {} · {}",
                    agent.harness, agent.model, agent.effort
                )));
                lines.push(Line::from(format!(
                    "host     {} · herdr {}",
                    agent.host, agent.herdr_session
                )));
                if let Some(duration) =
                    crate::cli::duration_s(&agent.created_at, agent.ended_at.as_deref())
                {
                    lines.push(Line::from(format!("uptime   {}s", duration)));
                }
                let collapsed = app.collapsed.contains(&entry.id);
                let tokens = if collapsed {
                    token_totals_for_subtree(store, &app.agents, &entry.id).unwrap_or((0, 0))
                } else {
                    token_totals_for_agent(store, &entry.id).unwrap_or((0, 0))
                };
                lines.push(Line::from(format!(
                    "tokens   {} in · {} out{}",
                    tokens.0,
                    tokens.1,
                    if collapsed { " (subtree)" } else { "" }
                )));
                let turn_count = store
                    .list_turns_by_agent(&entry.id)
                    .map(|t| t.len())
                    .unwrap_or(0);
                lines.push(Line::from(format!("turns    {turn_count}")));
                let kids = children(&app.agents, &entry.id).len();
                lines.push(Line::from(format!("children {kids}")));
                lines.push(Line::from(""));
                lines.push(Line::from("last ►"));
                let snippet = latest_snippet(store, &entry.id).unwrap_or_default();
                lines.push(Line::from(snippet));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "[Enter]attach [s]end [k]ill [K]ill-tree [o]ut [space]collapse",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn latest_snippet(store: &Store, agent_id: &str) -> Option<String> {
    let turns = store.list_turns_by_agent(agent_id).ok()?;
    let turn = turns.last()?;
    let text = fs::read_to_string(&turn.response_path).ok()?;
    Some(response_snippet(&text, SNIPPET_CHARS))
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let text = match &app.mode {
        Mode::Filter(buf) => format!("/{buf}"),
        Mode::Send { id, buf } => format!("send to {id}> {buf}"),
        Mode::ConfirmKill { id, tree } => {
            format!("{} {id}? [y/N]", if *tree { "kill-tree" } else { "kill" })
        }
        Mode::Normal if !app.status.is_empty() => app.status.clone(),
        Mode::Normal => {
            "[/]filter [g]c [q]uit — Enter attach · s send · k kill · K kill-tree · o out"
                .to_string()
        }
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn status_glyph(status: &str, kind: EntryKind) -> (char, Color) {
    if kind == EntryKind::Job {
        return match status {
            "running" => ('⟳', Color::Cyan),
            "paused" => ('⏸', Color::DarkGray),
            "done" => ('✓', Color::Green),
            "failed" => ('✗', Color::Red),
            "killed" => ('✗', Color::Red),
            _ => ('·', Color::DarkGray),
        };
    }
    match status {
        "queued" => ('○', Color::DarkGray),
        "starting" => ('◐', Color::Yellow),
        "working" => ('●', Color::Yellow),
        "idle" => ('○', Color::Green),
        "blocked" => ('◐', Color::Red),
        "done" => ('✓', Color::Green),
        "killed" => ('✗', Color::Red),
        "timeout" => ('⏱', Color::Red),
        "lost" => ('?', Color::Red),
        _ => ('·', Color::DarkGray),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, parent: Option<&str>, status: &str) -> AgentRow {
        let mut a = AgentRow::new(
            id,
            Some(format!("{id}-name")),
            "tui",
            "mock",
            format!("2026-01-01T00:00:0{}Z", id.len().min(9)),
            format!("/runs/{id}"),
        );
        a.parent_id = parent.map(str::to_string);
        a.status = status.to_string();
        a
    }

    #[test]
    fn flattens_agent_only_tree_in_creation_order() {
        let agents = vec![
            agent("a1", None, "working"),
            agent("a2", Some("a1"), "idle"),
        ];
        let entries = flatten_tree(&agents, &[]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "a1");
        assert_eq!(entries[0].depth, 0);
        assert_eq!(entries[1].id, "a2");
        assert_eq!(entries[1].depth, 1);
    }

    #[test]
    fn agents_nest_under_their_owning_job() {
        let job = JobRow::new(
            "w1",
            "workflow",
            r#"{"harness":"codex"}"#,
            "running",
            "2026-01-01T00:00:00Z",
        );
        let agents = vec![agent("a1", Some("w1"), "working")];
        let entries = flatten_tree(&agents, std::slice::from_ref(&job));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "w1");
        assert_eq!(entries[0].kind, EntryKind::Job);
        assert_eq!(entries[1].id, "a1");
        assert_eq!(entries[1].depth, 1);
    }

    #[test]
    fn orphaned_parent_id_falls_back_to_root() {
        let agents = vec![agent("a1", Some("does-not-exist"), "idle")];
        let entries = flatten_tree(&agents, &[]);
        assert_eq!(entries[0].depth, 0);
    }

    #[test]
    fn blocked_subtree_sorts_before_healthy_siblings() {
        let agents = vec![agent("a1", None, "working"), agent("a2", None, "blocked")];
        let entries = flatten_tree(&agents, &[]);
        assert_eq!(entries[0].id, "a2");
        assert!(entries[0].blocked_subtree);
        assert_eq!(entries[1].id, "a1");
        assert!(!entries[1].blocked_subtree);
    }

    #[test]
    fn blocked_child_marks_ancestor_subtree_but_not_status() {
        let agents = vec![
            agent("a1", None, "working"),
            agent("a2", Some("a1"), "blocked"),
        ];
        let entries = flatten_tree(&agents, &[]);
        let root = entries.iter().find(|e| e.id == "a1").unwrap();
        assert!(root.blocked_subtree);
        assert_eq!(root.status, "working");
    }

    #[test]
    fn visible_entries_hides_collapsed_subtree() {
        let agents = vec![
            agent("a1", None, "working"),
            agent("a2", Some("a1"), "idle"),
        ];
        let entries = flatten_tree(&agents, &[]);
        let mut collapsed = HashSet::new();
        collapsed.insert("a1".to_string());
        let visible = visible_entries(&entries, &collapsed);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "a1");
    }

    #[test]
    fn visible_entries_keeps_uncollapsed_children() {
        let agents = vec![
            agent("a1", None, "working"),
            agent("a2", Some("a1"), "idle"),
        ];
        let entries = flatten_tree(&agents, &[]);
        let visible = visible_entries(&entries, &HashSet::new());
        assert_eq!(visible.len(), 2);
    }

    #[test]
    fn filter_entries_matches_id_name_status_harness() {
        let agents = vec![agent("a7", None, "working")];
        let entries = flatten_tree(&agents, &[]);
        let refs: Vec<&TreeEntry> = entries.iter().collect();
        assert_eq!(filter_entries(&refs, "a7").len(), 1);
        assert_eq!(filter_entries(&refs, "a7-name").len(), 1);
        assert_eq!(filter_entries(&refs, "working").len(), 1);
        assert_eq!(filter_entries(&refs, "mock").len(), 1);
        assert_eq!(filter_entries(&refs, "nope").len(), 0);
        assert_eq!(filter_entries(&refs, "").len(), 1);
    }

    #[test]
    fn kill_job_marks_running_job_killed_and_disarms_it() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let mut job = JobRow::new(
            "l1",
            "loop",
            r#"{"harness":"mock"}"#,
            "running",
            "2026-01-01T00:00:00Z",
        );
        job.next_run_at = Some("2026-01-01T00:10:00Z".to_string());
        store.create_job(&job).unwrap();

        assert!(kill_job(&store, "l1").unwrap());
        let killed = store.get_job("l1").unwrap().unwrap();
        assert_eq!(killed.status, "killed");
        assert_eq!(killed.ended_reason.as_deref(), Some("killed"));
        assert!(killed.next_run_at.is_none());

        // Already-ended jobs are claimed (true = handled) but left untouched.
        assert!(kill_job(&store, "l1").unwrap());
        assert_eq!(store.get_job("l1").unwrap().unwrap().status, "killed");
        // Unknown ids are not claimed, so agent kill logic can take over.
        assert!(!kill_job(&store, "a1").unwrap());
    }

    #[test]
    fn snippet_passes_short_text_through_untouched() {
        assert_eq!(response_snippet("  hello world  ", 300), "hello world");
    }

    #[test]
    fn snippet_truncates_long_text_on_char_boundaries() {
        let text = "a".repeat(400);
        let snippet = response_snippet(&text, 300);
        assert_eq!(snippet.chars().count(), 301);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn snippet_counts_multibyte_chars_not_bytes() {
        let text = "é".repeat(310);
        let snippet = response_snippet(&text, 300);
        assert_eq!(snippet.chars().count(), 301);
    }

    #[test]
    fn auto_viewer_requires_herdr_env_and_config_flag() {
        assert!(auto_viewer_enabled(Some("1"), true));
        assert!(!auto_viewer_enabled(Some("1"), false));
        assert!(!auto_viewer_enabled(None, true));
        assert!(!auto_viewer_enabled(Some("0"), true));
    }

    #[test]
    fn viewer_pane_guard_is_false_once_labeled_pane_exists() {
        let none: Vec<Option<String>> = vec![None, Some("a7".to_string())];
        assert!(should_open_viewer_pane(&none));
        let present: Vec<Option<String>> =
            vec![Some("a7".to_string()), Some(AUTO_VIEWER_LABEL.to_string())];
        assert!(!should_open_viewer_pane(&present));
    }

    #[test]
    fn shell_quote_leaves_plain_paths_alone_but_escapes_specials() {
        assert_eq!(shell_quote("/usr/local/bin/orcr"), "/usr/local/bin/orcr");
        assert_eq!(shell_quote("/has space/orcr"), "'/has space/orcr'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
