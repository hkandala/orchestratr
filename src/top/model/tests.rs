//! Unit tests for the pure `top` tree/filter/lineage model (spec §7 acceptance).

use super::*;
use serde_json::json;

fn agent(path: &str, status: &str, provider: &str) -> Value {
    json!({
        "uuid": format!("uuid-{path}"),
        "path": path,
        "status": status,
        "managed": true,
        "agent": provider,
        "model": "opus",
        "created_at": 1000,
        "last_status_change_at": 1000,
    })
}

fn unmanaged(session: &str, pane: &str) -> Value {
    json!({
        "uuid": format!("u-{session}-{pane}"),
        "path": format!("unmanaged/{session}/{pane}"),
        "status": "working",
        "managed": false,
        "agent": "claude",
        "herdr_session": session,
        "created_at": 1000,
    })
}

fn snap(agents: Vec<Value>, loops: Vec<Value>) -> Snapshot {
    Snapshot::from_json(&json!({
        "snapshot_seq": 42,
        "agents": agents,
        "loops": loops,
    }))
}

/// The `ls` filter the tree must mirror: a plain pattern match over the live agent paths
/// (the store applies exactly this — anchored §5.1, no implicit prefix).
fn ls_uuids(s: &Snapshot, pattern: &str) -> BTreeSet<String> {
    let pat = Pattern::compile(pattern).unwrap();
    s.agents
        .iter()
        .filter(|a| pat.matches(&a.path))
        .map(|a| a.uuid.clone())
        .collect()
}

fn tree_with_pattern(s: &Snapshot, pattern: Option<&str>) -> Tree {
    let filter = TopFilter {
        pattern: pattern.map(|p| Pattern::compile(p).unwrap()),
        ..Default::default()
    };
    build_tree(s, &filter)
}

// --- Filters: tree node set == the equivalent `ls` node set (acceptance) --------------------

#[test]
fn filter_node_sets_match_ls() {
    let s = snap(
        vec![
            agent("review", "working", "claude"),
            agent("review/lint", "idle", "codex"),
            agent("review/fanout/file_1", "working", "claude"),
            agent("review/fanout/file_2", "blocked", "claude"),
            agent("reviewer/a", "working", "claude"),
            agent("reviewer/deep/b", "idle", "codex"),
            agent("other/x", "working", "claude"),
        ],
        vec![],
    );

    for pattern in [
        "review",
        "review/*",
        "review/**",
        "reviewer/**",
        "review/fanout/*",
    ] {
        let tree = tree_with_pattern(&s, Some(pattern));
        assert_eq!(
            tree.agent_uuids(),
            ls_uuids(&s, pattern),
            "pattern `{pattern}` node set diverged from ls"
        );
    }
}

#[test]
fn no_filter_includes_every_agent() {
    let s = snap(
        vec![
            agent("a", "working", "claude"),
            agent("b/c", "idle", "codex"),
        ],
        vec![],
    );
    let tree = tree_with_pattern(&s, None);
    assert_eq!(tree.agent_uuids().len(), 2);
}

#[test]
fn absolute_pattern_from_scope_is_anchored() {
    // A `/`-prefixed input from a scoped context resolves absolute (mirrors `resolve_selector`).
    let s = snap(
        vec![
            agent("review/x", "working", "claude"),
            agent("verify/x", "idle", "codex"),
        ],
        vec![],
    );
    let resolved = crate::path::resolve_selector(Some("review"), "/verify/*").unwrap();
    assert_eq!(resolved, "verify/*");
    let tree = tree_with_pattern(&s, Some(&resolved));
    assert_eq!(tree.agent_uuids(), ls_uuids(&s, "verify/*"));
}

#[test]
fn provider_and_status_filters_mirror_ls() {
    let s = snap(
        vec![
            agent("a", "working", "claude"),
            agent("b", "idle", "claude"),
            agent("c", "working", "codex"),
        ],
        vec![],
    );
    let f = TopFilter {
        provider: Some("claude".into()),
        status: Some("working".into()),
        ..Default::default()
    };
    let tree = build_tree(&s, &f);
    assert_eq!(tree.agent_uuids(), BTreeSet::from(["uuid-a".to_string()]));
}

// --- Lineage: cross-scope child placed once with `↖` annotation (acceptance) ----------------

#[test]
fn cross_scope_child_annotated_not_rerooted() {
    // fix_build/fixer created /verify/checker (absolute, outside its parent's scope).
    let mut checker = agent("verify/checker", "working", "codex");
    checker["parent_path"] = json!("fix_build/fixer");
    let s = snap(
        vec![agent("fix_build/fixer", "working", "claude"), checker],
        vec![],
    );
    let tree = build_tree(&s, &TopFilter::default());

    // Placed under `verify`, once — never re-rooted under `fix_build`.
    let verify = tree.roots.get("verify").expect("verify top node");
    let checker_node = verify
        .children
        .get("checker")
        .expect("checker under verify");
    assert_eq!(checker_node.lineage.as_deref(), Some("fix_build/fixer"));
    assert!(
        !tree
            .roots
            .get("fix_build")
            .and_then(|n| n.children.get("fixer"))
            .map(|f| f.children.contains_key("checker"))
            .unwrap_or(false),
        "checker must not be duplicated under its parent"
    );
    // Exactly one agent node for checker.
    let lines = tree.structure_lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("verify/checker"))
            .count(),
        1
    );
    assert!(lines.iter().any(|l| l.contains("<-fix_build/fixer")));
}

#[test]
fn in_scope_child_has_no_annotation() {
    let mut child = agent("refactor/phase_1/file_1", "working", "claude");
    child["parent_path"] = json!("refactor/phase_1");
    let s = snap(
        vec![agent("refactor/phase_1", "working", "claude"), child],
        vec![],
    );
    let tree = build_tree(&s, &TopFilter::default());
    let node = &tree.roots["refactor"].children["phase_1"].children["file_1"];
    assert_eq!(node.lineage, None);
}

// --- Placement: parked → Idle, unmanaged grouping, loop/run subtrees -----------------------

#[test]
fn parked_agents_collapse_into_idle_node() {
    let s = snap(
        vec![
            agent("work/a", "working", "claude"),
            agent("work/b", "parked", "claude"),
            agent("work/c", "parked", "codex"),
        ],
        vec![],
    );
    let tree = build_tree(&s, &TopFilter::default());
    let idle = tree.roots.get(IDLE_NODE).expect("idle node");
    assert_eq!(idle.children.len(), 2, "both parked agents under Idle");
    // Parked agents are NOT also placed at their real path.
    assert!(!tree.roots["work"].children.contains_key("b"));
    // Still counted in the node set (matches `ls`, which lists parked agents).
    assert!(tree.agent_uuids().contains("uuid-work/b"));
}

#[test]
fn unmanaged_agents_grouped_under_session() {
    let s = snap(
        vec![
            agent("build/x", "working", "claude"),
            unmanaged("main", "w6_p1"),
            unmanaged("main", "w7_p2"),
            unmanaged("side", "w1_p1"),
        ],
        vec![],
    );
    let tree = build_tree(&s, &TopFilter::default());
    let un = tree.roots.get("unmanaged").expect("unmanaged top node");
    assert!(un.children.contains_key("main"));
    assert!(un.children.contains_key("side"));
    assert_eq!(un.children["main"].children.len(), 2);
}

#[test]
fn loops_and_runs_and_run_agents_form_subtrees() {
    let loops = vec![json!({
        "uuid": "l1",
        "name": "nightly",
        "status": "active",
        "next_fire_at": 9_000_000,
        "runs": [
            { "uuid": "run-uuid", "run_id": "r82c9s", "kind": "scheduled", "status": "running", "due_at": 8_000_000, "started_at": 8_100_000 }
        ]
    })];
    // A run agent's path lives under `<loop>/<run_id>` — the path tree nests it naturally.
    let s = snap(
        vec![
            agent("nightly/r82c9s/triage", "idle", "claude"),
            agent("nightly/r82c9s/fix_1", "working", "codex"),
            agent("standalone", "working", "claude"),
        ],
        loops,
    );
    let tree = build_tree(&s, &TopFilter::default());
    let nightly = tree.roots.get("nightly").expect("loop node");
    assert!(matches!(nightly.kind, NodeKind::Loop(_)));
    let run = nightly.children.get("r82c9s").expect("run node");
    assert!(matches!(run.kind, NodeKind::Run { .. }));
    assert_eq!(run.children.len(), 2, "both run agents under the run");
}

#[test]
fn loops_only_hides_standalone_agents() {
    let loops = vec![json!({
        "uuid": "l1", "name": "nightly", "status": "active", "runs": []
    })];
    let s = snap(
        vec![
            agent("nightly/r1/triage", "working", "claude"),
            agent("standalone", "working", "claude"),
        ],
        loops,
    );
    let f = TopFilter {
        loops_only: true,
        ..Default::default()
    };
    let tree = build_tree(&s, &f);
    assert!(tree.roots.contains_key("nightly"));
    assert!(!tree.roots.contains_key("standalone"));
    assert!(tree.agent_uuids().contains("uuid-nightly/r1/triage"));
    assert!(!tree.agent_uuids().contains("uuid-standalone"));
}

#[test]
fn unmanaged_only_hides_loops_and_managed() {
    let loops = vec![json!({ "uuid": "l1", "name": "nightly", "status": "active", "runs": [] })];
    let s = snap(
        vec![
            agent("build/x", "working", "claude"),
            unmanaged("main", "p1"),
        ],
        loops,
    );
    let f = TopFilter {
        managed: Some(false),
        ..Default::default()
    };
    let tree = build_tree(&s, &f);
    assert!(
        !tree.roots.contains_key("nightly"),
        "loops hidden in --unmanaged"
    );
    assert!(
        !tree.roots.contains_key("build"),
        "managed hidden in --unmanaged"
    );
    assert!(tree.roots.contains_key("unmanaged"));
}

// --- Scale: a 100-agent tree builds + flattens well under the frame budget -----------------

#[test]
fn hundred_agent_tree_builds_under_frame_budget() {
    // A realistic deep/wide storm: 10 top scopes × 10 leaves = 100 agents.
    let mut agents = Vec::new();
    for i in 0..10 {
        for j in 0..10 {
            let status = ["working", "idle", "blocked", "parked"][(i + j) % 4];
            agents.push(agent(
                &format!("scope_{i}/phase/file_{j}"),
                status,
                "claude",
            ));
        }
    }
    let s = snap(agents, vec![]);
    let start = std::time::Instant::now();
    let tree = build_tree(&s, &TopFilter::default());
    let rows = tree.flatten(&BTreeSet::new(), 5000);
    let elapsed = start.elapsed();
    assert_eq!(tree.agent_uuids().len(), 100, "all 100 agents are nodes");
    assert!(!rows.is_empty());
    // The frame budget is 100ms; a build+flatten must be a small fraction of that.
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "build+flatten took {elapsed:?}, over budget"
    );
}

// --- Rendering helpers ---------------------------------------------------------------------

#[test]
fn age_formats_are_compact() {
    assert_eq!(format_age(5_000), "5s");
    assert_eq!(format_age(134_000), "2m14s");
    assert_eq!(format_age(120_000), "2m");
    assert_eq!(format_age(3 * 3_600_000), "3h");
    assert_eq!(format_age(2 * 86_400_000), "2d");
    assert_eq!(format_age(-10), "0s");
}

#[test]
fn glyphs_follow_status() {
    assert_eq!(glyph_for_status("working"), '●');
    assert_eq!(glyph_for_status("idle"), '○');
    assert_eq!(glyph_for_status("blocked"), '◐');
}

#[test]
fn flatten_floats_blocked_and_honors_collapse() {
    let s = snap(
        vec![
            agent("w/a_idle", "idle", "claude"),
            agent("w/z_blocked", "blocked", "codex"),
        ],
        vec![],
    );
    let tree = build_tree(&s, &TopFilter::default());
    let rows = tree.flatten(&BTreeSet::new(), 2000);
    // Under `w`, the blocked child sorts before the idle one despite alphabetical order.
    let names: Vec<&str> = rows
        .iter()
        .filter(|r| r.depth == 1)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(names, vec!["z_blocked", "a_idle"]);

    // Collapsing `w` hides its children.
    let mut collapsed = BTreeSet::new();
    collapsed.insert("w".to_string());
    let rows = tree.flatten(&collapsed, 2000);
    assert!(rows.iter().all(|r| r.depth == 0));
}

#[test]
fn snapshot_round_trips_from_json() {
    let s = snap(vec![agent("a", "working", "claude")], vec![]);
    assert_eq!(s.seq, 42);
    assert_eq!(s.agents.len(), 1);
    assert_eq!(s.agents[0].model.as_deref(), Some("opus"));
}
