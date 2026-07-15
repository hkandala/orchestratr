# M6 · top — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts. Capture *decisions and deviations*.

## Deviations from spec

- **Refresh model = re-snapshot per coalesced frame, not in-place event application.** §7/§11.6
  describe snapshot-then-subscribe with "renders are event-driven … a 100-event burst is one
  redraw" and "the tree can never miss or double-apply an update." Rather than hand-write
  incremental appliers for ~20 event kinds (which the milestone acceptance — a golden *final
  state* diff — does not require), the TUI treats the event stream as a **coalesced change
  signal**: `watch.open` seeds a pinned snapshot, a background reader collapses any burst into
  one `Dirty` per frame, and the render loop then re-reads a fresh **consistent** `api.snapshot`
  and rebuilds the tree. This is event-driven (never a fixed timer poll — no events, no
  refresh), collapses a burst into a single redraw, and by construction can neither miss nor
  double-apply a state change (each frame reflects an authoritative store snapshot). The event
  `seq` is still used to detect delivery/gaps; `server_stopping`/EOF/`cursor_expired` trigger a
  reconnect + re-snapshot. Proven by `top_e2e` (snapshot==ls, stream-delivers-change,
  restart-still-matches).

## Decisions on under-specified points

- **Filter node-set == `agent ls` by construction.** `TopFilter::agent_matches` applies exactly
  the store's `list_agents` predicate (anchored §5.1 pattern, provider, status, managed/
  unmanaged) over the same snapshot agent set (which excludes only `ended`, same as `ls` without
  `--all`). So the tree's agent node set is identical to the equivalent `ls` query — parked
  agents are collapsed under `Idle` but still counted as nodes (they appear in `ls`).
- **Loop visibility under filters.** Loops are managed constructs: `--unmanaged` hides them;
  every other view shows the loop node + its active runs (a live schedule row is useful even
  with an agent filter active). `--loops` keeps only agents whose top segment is a loop name
  (i.e. loop-run agents), hiding standalone agents. `--loops` has no `ls` equivalent, so it is
  not part of the filter==ls cross-check.
- **Node placement is pure path-tree.** Loop-run agents (`<loop>/<run_id>/<name>`) and unmanaged
  agents (`unmanaged/<session>/<pane>`) need no special casing — the path tree nests them under
  the loop→run and `unmanaged`→session nodes naturally. Only parked agents are relocated (to the
  synthetic `Idle` node).
- **Lineage annotation rule.** A row gets `↖ <parent path>` iff its `parent_path` is set and is
  NOT a proper ancestor of its own path (`!path.starts_with(parent_path + "/")`). A parent that
  is an ancestor already shows the edge by placement, so no annotation. Selection highlight of
  lineage is a UI nicety in `app.rs`.
- **Age column basis.** Age = `now − since_ms()`, where `since_ms` picks the status-appropriate
  clock (`starting_at`/`idle_since`/`parked_at`, else `last_status_change_at`, else
  `created_at`). Deterministic golden output (`structure_lines`) omits age.
- **Scope for the `/` and CLI patterns.** Resolved against `scope_of_agent(ORCR_PATH)` — the
  same scope `agent ls` uses for a non-run caller — so a `/`-prefixed input is absolute and a
  bare pattern is relative to the caller's scope, matching `ls`.

## Discovered facts / gotchas

- A **relative** `agent.run --path` resolves under the caller's scope; to place a cross-scope
  child at an absolute path the caller must pass a leading `/` (the e2e lineage case spawns
  `/verify/checker` with `caller_path=fix_build/fixer`). Relevant for reproducing §7's lineage
  example.
- Added deps: `ratatui` 0.28 + `crossterm` 0.28.

## Verifier & reviewer history

- **Verify round 1 — PASS.** Ran `cargo build` (green), `cargo clippy --all-targets -D warnings`
  (clean), `cargo fmt --check` (clean), `cargo test` (155 unit + all integration/e2e binaries
  green), and `ORCR_E2E=1 cargo test --test top_e2e -- --test-threads=1` (5/5 against live herdr
  0.7.2 + mock provider). No `orcr_test_*` sessions leaked (a pre-existing `orcr` session from an
  earlier milestone smoke-check remains — not introduced by M6). All four acceptance criteria
  proven: correctness (snapshot node-set == `agent ls` + lineage placed once), scale (100-agent
  unit build <50ms; 24-agent live snapshot), filters (tree node set == authoritative live
  `agent.ls` for review/review/*/review/**/reviewer/**/review/fanout/*), lineage golden
  (`/verify/checker` under `verify` with `↖ fix_build/fixer`, not re-rooted). Filter==ls holds by
  construction: `build_snapshot` uses `include_ended:false` (matches `ls` default) and
  `TopFilter::agent_matches` mirrors `store::list_agents`.
  - Non-blocking finding (for reviewer/reviser): `app.rs::run` computed `dirty` as a per-iteration
    local and drained `rx` each pass; if a coalesced event was drained in a sub-`FRAME` window right
    after a refresh (e.g. concurrent keypress), the `last_refresh.elapsed() >= FRAME` gate skipped the
    refresh and the dirty signal was lost until the *next* event, so the tree could briefly display a
    stale final state after keyboard activity.
  - **Resolved (scribe/finalize).** `dirty` is now hoisted to a loop-persistent `let mut dirty`
    outside the render loop and only cleared (`dirty = false`) once a refresh actually runs, so a
    coalesced signal held back by the frame gate survives to the next iteration and is never dropped —
    honoring §11.6's "never miss an update" strictly rather than relying on self-heal. No behavior
    change on the acceptance-tested path; unit + `top_e2e` remain green.

## Reviewer history

- **Review round 1 — PASS.** Code-review pass over the M6 surface (`src/top/model.rs`,
  `src/top/app.rs`, `src/cli.rs` top wiring, the `agent_row_json`/`build_snapshot` snapshot
  enrichment, and `tests/top_e2e.rs`). Findings: only the single non-blocking coalescing nit
  carried over from verify round 1 (above), now resolved at finalize. No correctness, security,
  spec-adherence, or test-quality blockers: the pure model is heavily unit-tested with golden
  structure diffs, `TopFilter::agent_matches` mirrors `store::list_agents` byte-for-byte (filter==ls
  by construction), snapshot enrichment is strictly additive (backward-compatible shapes), and the
  e2e suite exercises all four acceptance criteria against live herdr + mock. Verdict: PASS.

## Finalize (scribe)

- Fixed a stray tool-call artifact (`</content></invoke>`) accidentally appended to `todos.md`.
- Resolved the verifier's non-blocking coalescing finding in `app.rs` (see above).
- Final green check re-run at finalize — see the milestone completion summary; all gates green
  (`cargo build`, unit `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`, and
  `ORCR_E2E=1 top_e2e` against live herdr 0.7.2 + mock).
- Test-hygiene: cleaned up the pre-existing leaked owned `orcr` herdr session (bootstrapped by an
  earlier milestone smoke-check that didn't override `herdr.session`; flagged in commit 9a2afef).
  Verified `herdr session list --json` shows no `orcr`/`orcr_test_*` sessions after finalize; the
  user's real `default` session was never touched.
