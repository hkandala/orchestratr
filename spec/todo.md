# todo — live implementation tracker

Legend: `[x]` done · `[~]` in progress · `[ ]` pending. Updated continuously during
implementation; milestone intent lives in [10-roadmap.md](10-roadmap.md).

## M0 — Foundation
- [x] Repo bootstrap, private GitHub remote, spec contract
- [x] config module (config.toml, ORCR_STORE override, defaults)
- [x] store module (sqlite WAL, schema v1, CRUD)
- [x] rundir module (NNN-prompt/response naming, steer files, preamble, meta.json)
- [x] herdr driver (discovery, JSON envelopes, pane ops, seen-working completion, waits)
- [x] profiles: claude / codex / pi / opencode / mock (single file — split in M1)
- [x] orcr-mock-agent bin (READY/WORKING/DONE markers, directives)
- [x] unit suite: 41 tests (config/store/rundir/profiles/herdr/mock)
- [x] `orcr status [--json]` (herdr found/version, session, db health)
- [x] e2e: real-herdr round-trip + [[ignore-out]] scrape fallback + session hygiene

## Design docs & review
- [x] spec/ folder (01-overview … 10-roadmap, todo.md)
- [x] codex design review of CLI/API UX — 24 findings adjudicated in decisions.md
- [x] fold review outcomes into 02/03/04/06 (exit 7 state_conflict, JSON schemas,
      --harness, send intent flags, out --format, wait --tree, show verb, job surface,
      durable jobs, schedule --forever default, judge-harness, no --reuse)
- [x] remove root SPEC.md; README points at spec/

## M1 — Core verbs
- [x] id scheme: herdr-style short ids (a7 / l2 / s1 / g3 / w4, a7:tN turn sugar);
      migrate store + rundir off uuids; monotonic per-type counters
- [x] profile module split: src/profile/{mod,claude,codex,pi,opencode,mock}.rs
- [x] engine: spawn pipeline (admission → pane launch → env inject → startup recipe →
      deliver → track) with turn state machine incl. steer merging
- [x] run (async default, --wait, --keep, --mode, --worktree, --session)
- [x] send (steer vs new-turn resolution, --steer/--turn intent flags, exit 7)
- [x] wait (--all/--any/--tree/--timeout, rich timeout JSON, exit codes)
- [x] out (--turn, a7:tN sugar, --recursive, --format body|path|json)
- [x] ps / tree / show (--json; tree --watch; show = canonical object state)
- [x] kill (--tree, graceful recipe then pane close)
- [x] attach (terminal handoff)
- [x] history (basic: --since/--status/--parent/--name/--harness/--limit)
- [x] gc (sqlite ↔ herdr reconcile, orphaned panes, --dry-run)
- [x] response guarantee chain (file → transcript → scrape) wired into engine
- [x] env contract end-to-end (lineage recording, depth/tree caps, cycle refusal)
- [x] e2e checklist M1 (09): fan-out, steer-mid-turn, multi-turn --keep, recursive out,
      kill --tree, timeout exit 3, blocked exit 4, lineage
- [x] skill v0 draft (skill/SKILL.md)

## M2 — Jobs & daemon
- [x] serve: auto-start, pidfile, socket ping, --foreground; single-writer job ownership
- [x] job surface: job ls|show|pause|resume|rm across l/s/g/w
- [x] events feed (append + `orcr events --follow --json`)
- [x] loop: fixed --every, --every auto (NEXT_CHECK parse), --tick-on probe,
      --max/--until, --foreground mode, prompt-file re-read per tick
- [x] schedule: cron + --at, tz store + local/UTC echo, --catchup, --expires opt-in
      (forever default), resume --at re-arm, from-loop
- [x] reconciler on daemon start (lost agents, stale panes) + gc hardening
- [x] concurrency caps + queued admission; --max-runs/--max-duration budgets
- [x] e2e checklist M2 (09)

## M3 — Faces
- [ ] top TUI (tree+detail, attach/send/kill/out keys, filter, blocked sort)
- [ ] auto-viewer pane (HERDR_ENV detect, once-per-session guard, viewer.auto config)
- [ ] goal (worker+judge iterate, PASS/FAIL protocol, --judge-harness, self-check label)
- [ ] workflow run (env-contract child process, log capture, --on-orphan)
- [ ] history full (tokens, run-dir pointers) + token telemetry from transcripts
- [ ] SKILL.md final
- [ ] SDK: TS package + Python package (thin --json wrappers) + examples
- [ ] e2e checklist M3 (09)

## Final
- [ ] full-suite pass (units + fake-herdr + e2e), clippy/fmt clean
- [ ] README rewrite (install, quickstart, skill install)
- [ ] design-change report back to user
