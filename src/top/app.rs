//! The interactive `orcr top` TUI: a view-only, event-driven dashboard.
//!
//! Data path: `watch.open` yields one consistent snapshot pinned at `snapshot_seq`
//! plus the event stream from that sequence. A background reader turns the stream into a
//! coalesced "dirty" signal; the render loop then re-reads a fresh consistent `api.snapshot`
//! per frame (event-driven, never a fixed timer poll), so a burst of events collapses into a
//! single redraw and the tree can neither miss nor double-apply a change. `cursor_expired`,
//! `server_stopping`, or a dropped connection trigger a reconnect + re-snapshot.
//!
//! Interaction is navigation only: `/` filter (the path pattern grammar), arrows
//! collapse/expand and move the selection, `q` quits.

use super::model::{build_tree, Row as ModelRow, Snapshot, TopFilter, Tree};
use crate::error::{OrcrError, Result};
use crate::home::Home;
use crate::path::{self, Pattern};
use crate::server::Client;
use crate::store::now_millis;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, HighlightSpacing, Paragraph, Row as TableRow, Table, TableState};
use ratatui::Terminal;
use std::collections::BTreeSet;
use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the render loop wakes to poll keys / apply a coalesced refresh (frame budget).
const FRAME: Duration = Duration::from_millis(100);

/// A message from the background subscription reader to the render loop.
enum Msg {
    /// One or more events arrived (the render loop coalesces these into one refresh).
    Dirty,
    /// The stream ended (server stopping, restart, or connection dropped) — reconnect.
    Disconnected,
}

/// Run the TUI until the user quits. `scope` is the caller's `ORCR_PATH` (for resolving the
/// `/` filter's relative patterns); `initial` is the CLI pre-scoping filter.
pub fn run_top(scope: Option<String>, initial: TopFilter) -> Result<()> {
    let home = Home::ensure()?;
    let config = crate::config::Config::load(&home)?.config;
    let client = Client::new(home.socket_path());
    client.ensure_running(&home, &config)?;

    let mut terminal = setup_terminal()?;
    let mut app = App::new(client, scope, initial);
    let res = app.run(&mut terminal);
    restore_terminal(&mut terminal);
    res
}

/// The TUI application state.
struct App {
    client: Client,
    scope: Option<String>,
    filter: TopFilter,
    /// The raw text of each active CLI/`/` pattern, for the header display.
    filter_label: Option<String>,
    snapshot: Snapshot,
    tree: Tree,
    collapsed: BTreeSet<String>,
    selected: usize,
    table_state: TableState,
    /// The `/` filter text buffer while editing.
    input_mode: bool,
    input: String,
    message: Option<String>,
    /// The current subscription reader's stop flag + receiver.
    reader_stop: Arc<AtomicBool>,
    rx: Receiver<Msg>,
}

impl App {
    fn new(client: Client, scope: Option<String>, filter: TopFilter) -> App {
        let (_tx, rx) = mpsc::channel();
        App {
            client,
            scope,
            filter,
            filter_label: None,
            snapshot: Snapshot::default(),
            tree: build_tree(&Snapshot::default(), &TopFilter::default()),
            collapsed: BTreeSet::new(),
            selected: 0,
            table_state: TableState::default(),
            input_mode: false,
            input: String::new(),
            message: None,
            reader_stop: Arc::new(AtomicBool::new(false)),
            rx,
        }
    }

    /// Open `watch.open`, seed the snapshot, and (re)start the background reader thread.
    fn connect(&mut self) -> Result<()> {
        self.reader_stop.store(true, Ordering::SeqCst);
        let (initial, mut sub) = self
            .client
            .open_stream("watch.open", serde_json::json!({}))?;
        let snap = initial
            .get("snapshot")
            .map(Snapshot::from_json)
            .unwrap_or_default();
        self.apply_snapshot(snap);

        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
        self.rx = rx;
        let stop = Arc::new(AtomicBool::new(false));
        self.reader_stop = stop.clone();
        let _ = sub.set_read_timeout(Some(Duration::from_millis(250)));
        std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match sub.next_event() {
                Ok(Some(frame)) => {
                    // A `server_stopping` (server going away) or `cursor_expired` (our cursor
                    // fell out of the retained window) control frame means we must re-snapshot
                    // and resubscribe.
                    let kind = frame
                        .get("event")
                        .and_then(|e| e.get("kind"))
                        .or_else(|| frame.get("kind"))
                        .and_then(|k| k.as_str());
                    if matches!(kind, Some("server_stopping") | Some("cursor_expired")) {
                        let _ = tx.send(Msg::Disconnected);
                        return;
                    }
                    if tx.send(Msg::Dirty).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(Msg::Disconnected);
                    return;
                }
                Err(e) => {
                    // A read timeout is expected (lets us re-check `stop`); anything else ends
                    // the stream and triggers a reconnect.
                    if is_timeout(&e) {
                        continue;
                    }
                    let _ = tx.send(Msg::Disconnected);
                    return;
                }
            }
        });
        Ok(())
    }

    /// Re-fetch a fresh consistent snapshot and rebuild the tree (the coalesced refresh).
    fn refresh(&mut self) -> Result<()> {
        let snap =
            Snapshot::from_json(&self.client.request("api.snapshot", serde_json::json!({}))?);
        self.apply_snapshot(snap);
        Ok(())
    }

    fn apply_snapshot(&mut self, snap: Snapshot) {
        self.snapshot = snap;
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.tree = build_tree(&self.snapshot, &self.filter);
    }

    fn rows(&self) -> Vec<ModelRow> {
        self.tree.flatten(&self.collapsed, now_millis())
    }

    fn run(&mut self, terminal: &mut Term) -> Result<()> {
        self.connect()?;
        let mut last_refresh = Instant::now();
        // `dirty` persists across iterations: a coalesced signal that arrives inside a
        // sub-FRAME window (e.g. right after a keypress-driven redraw) is held until the
        // frame gate opens, so a state change is never dropped ("never miss").
        let mut dirty = false;
        loop {
            terminal
                .draw(|f| self.draw(f))
                .map_err(|e| OrcrError::server_error("tui", e.to_string()))?;

            // Poll keys within the frame budget.
            if event::poll(FRAME).map_err(|e| OrcrError::server_error("tui", e.to_string()))? {
                if let Event::Key(k) =
                    event::read().map_err(|e| OrcrError::server_error("tui", e.to_string()))?
                {
                    if k.kind != KeyEventKind::Release && self.on_key(k.code, k.modifiers)? {
                        return Ok(()); // quit
                    }
                }
            }

            // Coalesce the event stream into at most one refresh per frame.
            let mut reconnect = false;
            while let Ok(msg) = self.rx.try_recv() {
                match msg {
                    Msg::Dirty => dirty = true,
                    Msg::Disconnected => reconnect = true,
                }
            }
            if reconnect {
                self.message = Some("reconnecting…".into());
                // Best-effort reconnect with a short backoff; keep the last tree meanwhile.
                for _ in 0..40 {
                    if self.connect().is_ok() {
                        self.message = None;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            } else if dirty && last_refresh.elapsed() >= FRAME {
                if self.refresh().is_err() {
                    self.message = Some("reconnecting…".into());
                    let _ = self.connect();
                }
                last_refresh = Instant::now();
                dirty = false;
            }
        }
    }

    /// Handle a keypress. Returns `true` to quit.
    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Result<bool> {
        if self.input_mode {
            match code {
                KeyCode::Esc => {
                    self.input_mode = false;
                    self.input.clear();
                }
                KeyCode::Enter => {
                    self.input_mode = false;
                    self.apply_filter_input();
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                _ => {}
            }
            return Ok(false);
        }
        match code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => return Ok(true),
            KeyCode::Char('/') => {
                self.input_mode = true;
                self.input.clear();
                self.message = None;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Right | KeyCode::Char('l') => self.set_collapsed(false),
            KeyCode::Left | KeyCode::Char('h') => self.set_collapsed(true),
            _ => {}
        }
        Ok(false)
    }

    fn apply_filter_input(&mut self) {
        let raw = self.input.trim().to_string();
        if raw.is_empty() {
            self.filter.pattern = None;
            self.filter_label = None;
            self.message = None;
            self.rebuild();
            return;
        }
        match path::resolve_selector(self.scope.as_deref(), &raw).and_then(|s| Pattern::compile(&s))
        {
            Ok(p) => {
                self.filter.pattern = Some(p);
                self.filter_label = Some(raw);
                self.message = None;
                self.rebuild();
                self.selected = 0;
            }
            Err(e) => self.message = Some(format!("bad filter: {}", e.message)),
        }
    }

    fn move_selection(&mut self, delta: i64) {
        let n = self.rows().len();
        if n == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected as i64;
        self.selected = (cur + delta).clamp(0, n as i64 - 1) as usize;
    }

    fn set_collapsed(&mut self, collapse: bool) {
        let rows = self.rows();
        if let Some(row) = rows.get(self.selected) {
            if row.has_children {
                if collapse {
                    self.collapsed.insert(row.path.clone());
                } else {
                    self.collapsed.remove(&row.path);
                }
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(2),
                Constraint::Length(1),
            ])
            .split(f.area());

        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }

        f.render_widget(Paragraph::new(self.header()), chunks[0]);

        let table_rows = rows.iter().map(|row| self.table_row(row));
        let header = TableRow::new(["TREE", "STATUS", "AGENT", "TIME"])
            .style(
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);
        let (widths, column_spacing) = table_layout(f.area().width);
        let table = Table::new(table_rows, widths)
            .header(header)
            .column_spacing(column_spacing)
            .highlight_symbol(Span::styled("▌ ", Style::default().fg(Color::Cyan)))
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().bg(Color::Rgb(30, 37, 43)));
        self.table_state
            .select(if n == 0 { None } else { Some(self.selected) });
        f.render_stateful_widget(table, chunks[1], &mut self.table_state);

        let footer = if self.input_mode {
            Line::from(vec![
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(self.input.clone()),
                Span::styled("▏", Style::default().fg(Color::Yellow)),
            ])
        } else if let Some(m) = &self.message {
            Line::from(Span::styled(m.clone(), Style::default().fg(Color::Yellow)))
        } else {
            Line::from(Span::styled(
                " [/] filter   [←→] collapse/expand   [↑↓] move   [q] quit",
                Style::default().fg(Color::DarkGray),
            ))
        };
        f.render_widget(Paragraph::new(footer), chunks[2]);
    }

    fn header(&self) -> Line<'static> {
        let visible = self.tree.agent_uuids();
        let live = visible.len();
        let blocked = self
            .snapshot
            .agents
            .iter()
            .filter(|a| a.status == "blocked" && visible.contains(&a.uuid))
            .count();
        let loops = if self.filter.managed == Some(false) {
            0
        } else {
            self.snapshot.loops.len()
        };
        let mut spans = vec![
            Span::styled(
                "orcr",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {live} agents  ·  {blocked} blocked  ·  {loops} loops"),
                Style::default().fg(Color::Gray),
            ),
        ];
        if let Some(f) = &self.filter_label {
            spans.push(Span::styled(
                format!("  ·  /{f}"),
                Style::default().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
    }

    fn table_row(&self, row: &ModelRow) -> TableRow<'static> {
        let disclosure = if row.has_children {
            if row.collapsed {
                "▸ "
            } else {
                "▾ "
            }
        } else {
            "  "
        };
        let mut tree = vec![
            Span::styled(
                row.tree_prefix.clone(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(disclosure, Style::default().fg(Color::DarkGray)),
            Span::styled(
                row.label.clone(),
                if row.blocked {
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            ),
        ];
        if let Some(parent) = &row.lineage {
            tree.push(Span::styled(
                format!("  ↖ {parent}"),
                Style::default().fg(Color::DarkGray),
            ));
        }

        let status = if row.status.is_empty() {
            Line::default()
        } else {
            Line::from(vec![
                Span::styled(
                    format!("{} ", row.glyph),
                    Style::default().fg(glyph_color(row.glyph)),
                ),
                Span::styled(row.status.clone(), Style::default().fg(Color::Gray)),
            ])
        };

        TableRow::new([
            Cell::from(Line::from(tree)),
            Cell::from(status),
            Cell::from(row.agent.clone()).style(Style::default().fg(Color::Gray)),
            Cell::from(row.age.clone()).style(Style::default().fg(Color::DarkGray)),
        ])
    }
}

fn glyph_color(g: char) -> Color {
    match g {
        '●' => Color::Green,
        '○' => Color::Blue,
        '◐' => Color::Magenta,
        '⟳' => Color::Cyan,
        '◌' => Color::DarkGray,
        _ => Color::Gray,
    }
}

fn table_layout(width: u16) -> ([Constraint; 4], u16) {
    if width >= 90 {
        (
            [
                Constraint::Min(24),
                Constraint::Length(20),
                Constraint::Length(22),
                Constraint::Length(10),
            ],
            2,
        )
    } else if width >= 64 {
        (
            [
                Constraint::Min(18),
                Constraint::Length(16),
                Constraint::Length(16),
                Constraint::Length(8),
            ],
            1,
        )
    } else {
        (
            [
                Constraint::Min(10),
                Constraint::Length(13),
                Constraint::Length(12),
                Constraint::Length(6),
            ],
            1,
        )
    }
}

fn is_timeout(e: &OrcrError) -> bool {
    let m = e.message.to_lowercase();
    m.contains("timed out") || m.contains("timeout") || m.contains("would block")
}

type Term = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> Result<Term> {
    enable_raw_mode().map_err(|e| OrcrError::server_error("tui", e.to_string()))?;
    let mut stdout = std::io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .map_err(|e| OrcrError::server_error("tui", e.to_string()))?;
    Terminal::new(CrosstermBackend::new(stdout))
        .map_err(|e| OrcrError::server_error("tui", e.to_string()))
}

fn restore_terminal(terminal: &mut Term) {
    let _ = disable_raw_mode();
    let _ = terminal.backend_mut().execute(LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn buffer_line(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn frame_is_borderless_aligned_and_uses_subtle_selection() {
        let client = Client::new(std::path::PathBuf::from("/tmp/orcr-top-render-test.sock"));
        let mut app = App::new(
            client,
            None,
            TopFilter {
                managed: Some(true),
                ..Default::default()
            },
        );
        app.apply_snapshot(Snapshot::from_json(&json!({
            "snapshot_seq": 1,
            "agents": [
                {
                    "uuid": "a",
                    "path": "review/worker_a",
                    "status": "working",
                    "managed": true,
                    "agent": "claude",
                    "model": "opus",
                    "created_at": 1_000,
                    "last_status_change_at": 1_000
                },
                {
                    "uuid": "b",
                    "path": "review/worker_b",
                    "status": "idle",
                    "managed": true,
                    "agent": "codex",
                    "model": "o3",
                    "created_at": 1_000,
                    "idle_since": 1_000
                },
                {
                    "uuid": "u",
                    "path": "unmanaged/default/w3_p3k",
                    "status": "working",
                    "managed": false,
                    "agent": "claude",
                    "created_at": 1_000
                }
            ],
            "loops": []
        })));

        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..buffer.area.height)
            .map(|y| buffer_line(buffer, y))
            .collect();

        assert!(lines[0].starts_with("orcr  2 agents"));
        assert!(!lines
            .iter()
            .any(|line| line.contains('┌') || line.contains('┐') || line.contains('┘')));
        assert!(!lines.iter().any(|line| line.contains("w3_p3k")));

        let worker_a = lines.iter().find(|line| line.contains("worker_a")).unwrap();
        let worker_b = lines.iter().find(|line| line.contains("worker_b")).unwrap();
        assert_eq!(worker_a.find("working"), worker_b.find("idle"));
        assert_eq!(worker_a.find("claude"), worker_b.find("codex"));

        let selected_y = lines
            .iter()
            .position(|line| line.contains("review"))
            .unwrap() as u16;
        assert!(lines[selected_y as usize].starts_with("▌ "));
        assert_eq!(buffer[(2, selected_y)].bg, Color::Rgb(30, 37, 43));
        assert!(!buffer[(2, selected_y)]
            .modifier
            .contains(Modifier::REVERSED));
    }
}
