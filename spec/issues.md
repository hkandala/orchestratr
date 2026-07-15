# issues

open issues for orchestratr (`orcr`). fixed/closed items are history — see the archived
sources under `spec/_impl/` (`known-issues.md`, `open-issues.md`, `review-phase-notes.md`,
`manual-e2e-results.md`). this file tracks only what is still open.

## open bugs

- **codex intermittent completion timeout** — 2/6 real-codex runs go `working` (submit
  confirmed) but `turn 1 complete` is never observed within 3m. a downstream
  completion-detection / codex-slowness intermittency, unrelated to submit. see
  spec/_impl/open-issues.md.

## blocked on environment

- **real-claude `ask`/`wait`/`logs` on the enterprise box** — herdr's claude integration
  does not report `agent_status: working` for the Avocado/MetaCode-wrapped claude, so orcr
  never detects the turn completing and `logs` can't resolve the wrapped session's
  transcript. the prompt submits and claude answers in-pane (proven), but `ask`/`wait` time
  out. this is a herdr-integration / transcript-location limitation, not an orcr defect —
  codex runs fully end-to-end through the same pipeline. blocks E01/E03/E05 + the claude leg
  of E18. needs validation on a non-enterprise claude box (or a herdr-side integration fix).
  see spec/_impl/open-issues.md, spec/_impl/manual-e2e-results.md.

## known limitations

these are current-release behaviors a user should know about (the full deferred-feature
roadmap lives in todos.md and spec §17):

- **windows unsupported** — loops/process-groups/`server enable` are POSIX-only; `service.rs`
  returns `unsupported_platform` elsewhere. see spec/_impl/spec-completeness.md.
- **everything runs permission-bypass** — no `--read-only` / permission profiles yet. see
  spec §17.
- **`blocked_kind` is best-effort** — no structured per-provider blocked-reason classification
  or rate-limit policy. see spec §17.
- **no data-dir retention GC** — `~/.orcr/data` grows unbounded; no lifecycle/retention sweep.
  see spec §17.
- **`top` is view-only** — no detail-panel actions (attach/send/kill/logs) or live activity
  feed in this release. see spec §17, spec/_impl/spec-completeness.md.
- **real-provider validation is best-effort** — mock-against-live-herdr is the automated gate;
  real claude/codex smoke of recipes/logs and live launchd/systemd `enable` round-trips are
  manual-only. see spec/_impl/spec-completeness.md, spec/_impl/manual-e2e-results.md.
- **submit-confirm hardening not re-validated under heavy load** — the adaptive-window, up-to-6-
  re-deliveries submit-confirm (E02) is fixed and flake-free in validation, but has not been
  re-validated under heavy parallel real-provider boot load. the actionable follow-up lives in
  todos.md (tech-debt). see spec/_impl/open-issues.md (E02), spec/_impl/manual-e2e-results.md.

## spec nits (deferred — no code change expected)

- **`{rand}` in a selector** — rejected as `reason: invalid_segment` rather than a
  rand-specific reason; correctly `invalid_request`/exit 1, but a clearer reason would help.
  see spec/_impl/manual-e2e-results.md (E06 nit).
