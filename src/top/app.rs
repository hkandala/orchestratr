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

use super::model::{build_tree, Row, Snapshot, TopFilter, Tree};
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
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
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
    list_state: ListState,
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
            list_state: ListState::default(),
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

    fn rows(&self) -> Vec<Row> {
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
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(f.area());

        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }

        let items: Vec<ListItem> = rows.iter().map(|r| self.row_item(r)).collect();
        let header = self.header();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(header))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        self.list_state
            .select(if n == 0 { None } else { Some(self.selected) });
        f.render_stateful_widget(list, chunks[0], &mut self.list_state);

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
        f.render_widget(Paragraph::new(footer), chunks[1]);
    }

    fn header(&self) -> String {
        let live = self
            .snapshot
            .agents
            .iter()
            .filter(|a| a.status != "ended" && a.status != "lost")
            .count();
        let blocked = self
            .snapshot
            .agents
            .iter()
            .filter(|a| a.status == "blocked")
            .count();
        let loops = self.snapshot.loops.len();
        let mut h = format!(" orcr · {live} agents ({blocked} blocked) · {loops} loops ");
        if let Some(f) = &self.filter_label {
            h.push_str(&format!("· /{f} "));
        }
        h
    }

    fn row_item(&self, r: &Row) -> ListItem<'static> {
        let indent = "  ".repeat(r.depth);
        let marker = if r.has_children {
            if r.collapsed {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };
        let glyph_style = Style::default().fg(glyph_color(r.glyph));
        let mut spans = vec![
            Span::raw(indent),
            Span::raw(marker.to_string()),
            Span::styled(format!("{} ", r.glyph), glyph_style),
            Span::styled(
                r.label.clone(),
                if r.blocked {
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                },
            ),
        ];
        if !r.detail.is_empty() {
            spans.push(Span::styled(
                format!("   {}", r.detail),
                Style::default().fg(Color::Gray),
            ));
        }
        ListItem::new(Line::from(spans))
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
