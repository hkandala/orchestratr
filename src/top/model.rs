//! The pure, testable core of `orcr top` (spec §7): parse a runtime snapshot into a typed
//! model, apply the same filters as `agent ls` (§5.1 pattern grammar, no implicit prefixing),
//! and build the **path tree** — level-1 segments as top nodes, loops and their active runs
//! as subtrees, parked agents collapsed into an `Idle` node, unmanaged agents grouped under
//! their session — with cross-scope lineage shown as a `↖ <parent path>` annotation (never a
//! second placement or a re-root).
//!
//! Rendering to a real terminal lives in `app.rs`; everything here is deterministic and
//! unit-tested so the correctness/filter/lineage acceptance criteria are provable without a
//! PTY.

use crate::path::Pattern;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The synthetic top node parked agents collapse into (spec §7).
pub const IDLE_NODE: &str = "idle";

/// One agent as seen in a runtime snapshot (`api.snapshot` / `watch.open`, §11.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapAgent {
    pub uuid: String,
    pub path: String,
    pub status: String,
    pub managed: bool,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub blocked_kind: Option<String>,
    pub parent_path: Option<String>,
    pub herdr_session: Option<String>,
    pub queue_position: Option<i64>,
    pub created_at: i64,
    pub last_status_change_at: Option<i64>,
    pub starting_at: Option<i64>,
    pub idle_since: Option<i64>,
    pub parked_at: Option<i64>,
}

/// One active run of a loop (spec §6.2/§7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapRun {
    pub run_id: String,
    pub uuid: String,
    pub kind: String,
    pub status: String,
    pub due_at: Option<i64>,
    pub started_at: Option<i64>,
}

/// One loop definition (spec §6.2/§7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapLoop {
    pub uuid: String,
    pub name: String,
    pub status: String,
    pub cadence: Option<String>,
    pub next_fire_at: Option<i64>,
    pub runs: Vec<SnapRun>,
}

/// A whole runtime snapshot at `seq` (spec §11.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub seq: i64,
    pub agents: Vec<SnapAgent>,
    pub loops: Vec<SnapLoop>,
}

impl Snapshot {
    /// Parse the `api.snapshot` / `watch.open` result document.
    pub fn from_json(v: &Value) -> Snapshot {
        let seq = v.get("snapshot_seq").and_then(|s| s.as_i64()).unwrap_or(0);
        let agents = v
            .get("agents")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().map(SnapAgent::from_json).collect())
            .unwrap_or_default();
        let loops = v
            .get("loops")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().map(SnapLoop::from_json).collect())
            .unwrap_or_default();
        Snapshot { seq, agents, loops }
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(String::from)
}
fn i(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}

impl SnapAgent {
    fn from_json(v: &Value) -> SnapAgent {
        SnapAgent {
            uuid: s(v, "uuid").unwrap_or_default(),
            path: s(v, "path").unwrap_or_default(),
            status: s(v, "status").unwrap_or_default(),
            managed: v.get("managed").and_then(|x| x.as_bool()).unwrap_or(true),
            agent: s(v, "agent"),
            model: s(v, "model"),
            blocked_kind: s(v, "blocked_kind"),
            parent_path: s(v, "parent_path"),
            herdr_session: s(v, "herdr_session"),
            queue_position: i(v, "queue_position"),
            created_at: i(v, "created_at").unwrap_or(0),
            last_status_change_at: i(v, "last_status_change_at"),
            starting_at: i(v, "starting_at"),
            idle_since: i(v, "idle_since"),
            parked_at: i(v, "parked_at"),
        }
    }

    /// The moment the current status began — the basis for the age column (spec §7).
    pub fn since_ms(&self) -> i64 {
        match self.status.as_str() {
            "starting" => self.starting_at,
            "idle" => self.idle_since,
            "parked" => self.parked_at,
            _ => self.last_status_change_at,
        }
        .unwrap_or(self.created_at)
    }
}

impl SnapLoop {
    fn from_json(v: &Value) -> SnapLoop {
        let runs = v
            .get("runs")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().map(SnapRun::from_json).collect())
            .unwrap_or_default();
        SnapLoop {
            uuid: s(v, "uuid").unwrap_or_default(),
            name: s(v, "name").unwrap_or_default(),
            status: s(v, "status").unwrap_or_default(),
            cadence: s(v, "cadence"),
            next_fire_at: i(v, "next_fire_at"),
            runs,
        }
    }
}

impl SnapRun {
    fn from_json(v: &Value) -> SnapRun {
        SnapRun {
            run_id: s(v, "run_id").unwrap_or_default(),
            uuid: s(v, "uuid").unwrap_or_default(),
            kind: s(v, "kind").unwrap_or_default(),
            status: s(v, "status").unwrap_or_default(),
            due_at: i(v, "due_at"),
            started_at: i(v, "started_at"),
        }
    }
}

/// The pre-scoping filter (spec §6.3): the CLI flags and the in-TUI `/` pattern share this.
/// The predicate over agents is **byte-for-byte the same** as the store's `agent ls` filter
/// so the tree's agent node set equals the equivalent `ls` query (an acceptance criterion).
#[derive(Debug, Clone, Default)]
pub struct TopFilter {
    /// Compiled, scope-resolved §5.1 pattern (no implicit prefix matching).
    pub pattern: Option<Pattern>,
    pub provider: Option<String>,
    pub status: Option<String>,
    /// `Some(true)` = managed-only, `Some(false)` = unmanaged-only, `None` = both.
    pub managed: Option<bool>,
    /// `--loops`: show only loops and their run subtrees.
    pub loops_only: bool,
}

impl TopFilter {
    /// The `agent ls` predicate (spec §6.1): pattern (anchored §5.1), provider, status, and
    /// managed/unmanaged. `loops_only` is applied separately (it is about placement, not the
    /// per-agent predicate).
    pub fn agent_matches(&self, a: &SnapAgent) -> bool {
        if let Some(p) = &self.provider {
            if a.agent.as_deref() != Some(p.as_str()) {
                return false;
            }
        }
        if let Some(st) = &self.status {
            if &a.status != st {
                return false;
            }
        }
        if let Some(m) = self.managed {
            if a.managed != m {
                return false;
            }
        }
        if let Some(pat) = &self.pattern {
            if !pat.matches(&a.path) {
                return false;
            }
        }
        true
    }
}

/// What a tree node is (drives its glyph + row format).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A structural ancestor segment with no agent/loop of its own.
    Scope,
    /// An agent sits exactly at this path.
    Agent(usize),
    /// A loop definition.
    Loop(usize),
    /// An active loop run (`run <run_id>`).
    Run { loop_idx: usize, run_idx: usize },
    /// The synthetic `Idle` node collecting parked agents.
    Idle,
}

/// One node in the path tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// The last path segment (the display label). Synthetic nodes use a fixed label.
    pub segment: String,
    /// The full accumulated path (unique node key).
    pub path: String,
    pub kind: NodeKind,
    /// Cross-scope lineage annotation (`↖ <parent path>`) when the parent lives elsewhere.
    pub lineage: Option<String>,
    /// Children keyed by segment → deterministic ordering.
    pub children: BTreeMap<String, Node>,
}

impl Node {
    fn new(segment: &str, path: &str, kind: NodeKind) -> Node {
        Node {
            segment: segment.to_string(),
            path: path.to_string(),
            kind,
            lineage: None,
            children: BTreeMap::new(),
        }
    }
}

/// The built path tree plus the source snapshot (rows resolve agent/loop metadata by index).
#[derive(Debug, Clone)]
pub struct Tree {
    pub roots: BTreeMap<String, Node>,
    pub snapshot: Snapshot,
}

/// Build the path tree from a snapshot under a filter (spec §7).
pub fn build_tree(snap: &Snapshot, filter: &TopFilter) -> Tree {
    let mut roots: BTreeMap<String, Node> = BTreeMap::new();

    let loop_names: BTreeSet<&str> = snap.loops.iter().map(|l| l.name.as_str()).collect();

    // 1) Loops (unless the view is restricted to unmanaged agents). A loop is a managed
    //    construct, so `--unmanaged` hides it; every other view shows the schedule + runs.
    let show_loops = filter.managed != Some(false);
    if show_loops {
        for (li, l) in snap.loops.iter().enumerate() {
            let node = roots
                .entry(l.name.clone())
                .or_insert_with(|| Node::new(&l.name, &l.name, NodeKind::Loop(li)));
            node.kind = NodeKind::Loop(li);
            for (ri, r) in l.runs.iter().enumerate() {
                let run_path = format!("{}/{}", l.name, r.run_id);
                node.children.entry(r.run_id.clone()).or_insert_with(|| {
                    Node::new(
                        &r.run_id,
                        &run_path,
                        NodeKind::Run {
                            loop_idx: li,
                            run_idx: ri,
                        },
                    )
                });
            }
        }
    }

    // 2) Agents. Parked agents collapse under the synthetic Idle node; every other agent is
    //    placed at its path, creating structural ancestors as needed.
    for (ai, a) in snap.agents.iter().enumerate() {
        if !filter.agent_matches(a) {
            continue;
        }
        // `--loops` keeps only agents that live under a loop run subtree.
        if filter.loops_only {
            let top = a.path.split('/').next().unwrap_or("");
            if !loop_names.contains(top) {
                continue;
            }
        }
        if a.status == "parked" {
            let idle = roots
                .entry(IDLE_NODE.to_string())
                .or_insert_with(|| Node::new(IDLE_NODE, IDLE_NODE, NodeKind::Idle));
            let leaf = format!("{IDLE_NODE}/{}", a.path);
            let node = idle
                .children
                .entry(a.path.clone())
                .or_insert_with(|| Node::new(&a.path, &leaf, NodeKind::Agent(ai)));
            node.kind = NodeKind::Agent(ai);
            annotate_lineage(node, a);
            continue;
        }
        insert_agent(&mut roots, ai, a);
    }

    Tree {
        roots,
        snapshot: snap.clone(),
    }
}

/// Insert an agent at its path, walking/creating structural ancestor nodes.
fn insert_agent(roots: &mut BTreeMap<String, Node>, ai: usize, a: &SnapAgent) {
    let segs: Vec<&str> = a.path.split('/').collect();
    let mut acc = String::new();
    let mut level = roots;
    for (depth, seg) in segs.iter().enumerate() {
        if depth == 0 {
            acc.push_str(seg);
        } else {
            acc.push('/');
            acc.push_str(seg);
        }
        let last = depth == segs.len() - 1;
        let node = level.entry(seg.to_string()).or_insert_with(|| {
            Node::new(
                seg,
                &acc,
                if last {
                    NodeKind::Agent(ai)
                } else {
                    NodeKind::Scope
                },
            )
        });
        if last {
            // A pre-existing structural node (an ancestor created by a deeper agent) becomes
            // the agent's node; never re-root or duplicate.
            node.kind = NodeKind::Agent(ai);
            annotate_lineage(node, a);
        }
        level = &mut node.children;
    }
}

/// Annotate a node with `↖ <parent path>` when its parent lives elsewhere in the tree
/// (a child created at an absolute path outside its parent's scope, spec §7). A parent that
/// is a proper ancestor needs no annotation — natural placement already shows the edge.
fn annotate_lineage(node: &mut Node, a: &SnapAgent) {
    if let Some(pp) = &a.parent_path {
        let is_ancestor = a.path.starts_with(&format!("{pp}/"));
        if !is_ancestor {
            node.lineage = Some(pp.clone());
        }
    }
}

impl Tree {
    /// The set of agent uuids present anywhere in the tree — the "node set" the filter
    /// acceptance compares against `agent ls` (spec §7 acceptance).
    pub fn agent_uuids(&self) -> BTreeSet<String> {
        let mut set = BTreeSet::new();
        for n in self.roots.values() {
            self.collect_uuids(n, &mut set);
        }
        set
    }

    fn collect_uuids(&self, n: &Node, set: &mut BTreeSet<String>) {
        if let NodeKind::Agent(ai) = n.kind {
            set.insert(self.snapshot.agents[ai].uuid.clone());
        }
        for c in n.children.values() {
            self.collect_uuids(c, set);
        }
    }

    /// A deterministic, time-independent canonical rendering of the whole tree — one line per
    /// node in DFS order — used for golden diffs and the storm/restart correctness gate.
    pub fn structure_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for n in self.roots.values() {
            self.structure_of(n, 0, &mut out);
        }
        out
    }

    fn structure_of(&self, n: &Node, depth: usize, out: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        let mut line = format!("{indent}{}", n.path);
        match &n.kind {
            NodeKind::Agent(ai) => {
                let a = &self.snapshot.agents[*ai];
                line.push_str(&format!(
                    " [agent {} {}]",
                    a.status,
                    a.agent.as_deref().unwrap_or("-")
                ));
                if let Some(m) = &a.model {
                    line.push_str(&format!("·{m}"));
                }
                if let Some(bk) = &a.blocked_kind {
                    line.push_str(&format!(" blocked:{bk}"));
                }
                if let Some(q) = a.queue_position {
                    line.push_str(&format!(" q{q}"));
                }
                if let Some(lin) = &n.lineage {
                    line.push_str(&format!(" <-{lin}"));
                }
            }
            NodeKind::Loop(li) => {
                let l = &self.snapshot.loops[*li];
                line.push_str(&format!(" [loop {}]", l.status));
            }
            NodeKind::Run { loop_idx, run_idx } => {
                let r = &self.snapshot.loops[*loop_idx].runs[*run_idx];
                line.push_str(&format!(" [run {} {}]", r.run_id, r.status));
            }
            NodeKind::Idle => line.push_str(" [idle]"),
            NodeKind::Scope => line.push_str(" [scope]"),
        }
        out.push(line);
        for c in n.children.values() {
            self.structure_of(c, depth + 1, out);
        }
    }
}

/// A flattened, display-ready row for the TUI (spec §7). `depth` drives indentation;
/// `has_children`/`collapsed` drive the expand/collapse affordance.
#[derive(Debug, Clone)]
pub struct Row {
    pub depth: usize,
    pub path: String,
    pub has_children: bool,
    pub collapsed: bool,
    /// The status glyph (or a space).
    pub glyph: char,
    /// The row label (name).
    pub label: String,
    /// The trailing detail (status · provider·model · blocked · age / loop schedule / run).
    pub detail: String,
    /// True for blocked agents (the "needs a human" queue floats upward, spec §7).
    pub blocked: bool,
}

impl Tree {
    /// Flatten the tree into visible rows honoring the `collapsed` set (by node path). Blocked
    /// agents float above their siblings (spec §7). `now` drives the age column.
    pub fn flatten(&self, collapsed: &BTreeSet<String>, now: i64) -> Vec<Row> {
        let mut out = Vec::new();
        for n in ordered_children(self.roots.values().collect(), &self.snapshot) {
            self.flatten_node(n, 0, collapsed, now, &mut out);
        }
        out
    }

    fn flatten_node(
        &self,
        n: &Node,
        depth: usize,
        collapsed: &BTreeSet<String>,
        now: i64,
        out: &mut Vec<Row>,
    ) {
        let has_children = !n.children.is_empty();
        let is_collapsed = collapsed.contains(&n.path);
        let (glyph, label, detail, blocked) = self.render_node(n, now);
        out.push(Row {
            depth,
            path: n.path.clone(),
            has_children,
            collapsed: is_collapsed,
            glyph,
            label,
            detail,
            blocked,
        });
        if has_children && !is_collapsed {
            for c in ordered_children(n.children.values().collect(), &self.snapshot) {
                self.flatten_node(c, depth + 1, collapsed, now, out);
            }
        }
    }

    /// (glyph, label, detail, is_blocked) for a node's UI row.
    fn render_node(&self, n: &Node, now: i64) -> (char, String, String, bool) {
        match &n.kind {
            NodeKind::Agent(ai) => {
                let a = &self.snapshot.agents[*ai];
                let mut detail = a.status.clone();
                if let Some(bk) = &a.blocked_kind {
                    detail.push_str(&format!(" {bk}"));
                }
                let prov = a.agent.as_deref().unwrap_or("-");
                match &a.model {
                    Some(m) => detail.push_str(&format!("   {prov} · {m}")),
                    None => detail.push_str(&format!("   {prov}")),
                }
                if let Some(q) = a.queue_position {
                    detail.push_str(&format!("   #{q}"));
                }
                if let Some(lin) = &n.lineage {
                    detail.push_str(&format!("   ↖ {lin}"));
                }
                detail.push_str(&format!("   {}", format_age(now - a.since_ms())));
                (
                    glyph_for_status(&a.status),
                    n.segment.clone(),
                    detail,
                    a.status == "blocked",
                )
            }
            NodeKind::Loop(li) => {
                let l = &self.snapshot.loops[*li];
                let mut detail = format!("loop · {}", l.status);
                if let Some(nf) = l.next_fire_at {
                    detail.push_str(&format!(" · next {}", format_clock(nf)));
                }
                ('·', n.segment.clone(), detail, false)
            }
            NodeKind::Run { loop_idx, run_idx } => {
                let r = &self.snapshot.loops[*loop_idx].runs[*run_idx];
                let mut detail = format!("running · {}", r.status);
                if let Some(due) = r.due_at {
                    detail.push_str(&format!(" · due {}", format_clock(due)));
                }
                if let Some(st) = r.started_at {
                    detail.push_str(&format!(" · {}", format_age(now - st)));
                }
                ('⟳', format!("run {}", r.run_id), detail, false)
            }
            NodeKind::Idle => {
                let count = n.children.len();
                ('▶', "idle".to_string(), format!("parked · {count}"), false)
            }
            NodeKind::Scope => (' ', n.segment.clone(), String::new(), false),
        }
    }
}

/// Order siblings for display: blocked agents float to the top, then alphabetical by segment
/// (spec §7). `structure_lines` stays purely alphabetical (deterministic golden output).
fn ordered_children<'a>(mut nodes: Vec<&'a Node>, snap: &Snapshot) -> Vec<&'a Node> {
    nodes.sort_by(|a, b| {
        let ab = node_is_blocked(a, snap);
        let bb = node_is_blocked(b, snap);
        bb.cmp(&ab).then_with(|| a.segment.cmp(&b.segment))
    });
    nodes
}

fn node_is_blocked(n: &Node, snap: &Snapshot) -> bool {
    matches!(n.kind, NodeKind::Agent(ai) if snap.agents[ai].status == "blocked")
}

/// The status glyph (spec §7).
pub fn glyph_for_status(status: &str) -> char {
    match status {
        "working" => '●',
        "idle" => '○',
        "blocked" => '◐',
        "queued" | "starting" => '◌',
        "parked" => '▪',
        "lost" => '✗',
        _ => '·',
    }
}

/// A compact age like `2m14s`, `8m`, `3h`, `4d` (spec §7 row age).
pub fn format_age(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let (m, s) = (secs / 60, secs % 60);
        if s == 0 {
            format!("{m}m")
        } else {
            format!("{m}m{s:02}s")
        }
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// A local wall-clock `HH:MM` for a UTC-ms instant (loop next-fire / run due, spec §7).
fn format_clock(ms: i64) -> String {
    use chrono::TimeZone;
    let tz = crate::cron::tz_from_name(&crate::cron::local_tz_name());
    match tz.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(dt) => dt.format("%H:%M").to_string(),
        _ => "--:--".to_string(),
    }
}

#[cfg(test)]
mod tests;
