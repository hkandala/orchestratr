# todos

open follow-ups, roadmap, and deferred infra for orchestratr (`orcr`). the full future-work
catalogue is spec §17 and the deferred table in spec/_impl/spec-completeness.md; this file is
the actionable digest. open bugs/limitations live in issues.md.

## distribution & release

- **host the install script at `orchestratr.dev/install.sh`** — `install.sh` is in the repo
  and the repo is public (raw GitHub URL works auth-free); it still needs serving at that URL.
  recommended: a cloudflare worker on the route (snippet in docs/RELEASE.md), cloudflare
  pages, or a redirect rule to the raw GitHub URL. see spec/_impl/open-issues.md.
- **automate releases further** — today: `scripts/release.sh` (one-command bump+tag+push).
  consider release-please (auto version + `CHANGELOG.md` from the existing `feat:`/`fix:`/
  `chore:` commits) or cargo-dist (generates the release workflow *and* the installer); either
  would replace the hand-rolled `release.yml` + `install.sh`. see spec/_impl/open-issues.md.
- **publish to registries** — claim the `orchestratr` crate (crates.io) + `@orchestratr` scope
  (npm), then add `CARGO_REGISTRY_TOKEN` + `NPM_TOKEN` repo secrets to enable the gated publish
  jobs. `@orchestratr/sdk` is currently unpublished (`0.0.0`); `ORCR_SDK_SPEC` tarball is used
  for offline/scaffold install until then. see spec/_impl/open-issues.md,
  spec/_impl/spec-completeness.md (m7 notes), docs/RELEASE.md.

## features (roadmap, spec §17)

full list + sources in spec §17 and spec/_impl/spec-completeness.md. highlights:

- pi/opencode built-in `AgentIntegration` modules; degraded single-layer
  (no-integration) modes.
- `top` actions (detail panel: attach/send/kill/logs from the TUI) + live activity feed.
- `send` steer/stop options (interrupt / graceful-stop per provider).
- background-subagent detection for claude (don't park/reap in-flight subagents).
- structured per-provider blocked-reason classification + rate-limit policies.
- permission policies (`--read-only`, profiles) — everything runs bypass today.
- cross-host orchestration from the local CLI (socket tunnel, remote transcripts/pgroups).
- notifications beyond terminal (herdr notify, webhook/ntfy).
- python SDK + `orcr scaffold <lang>` (TS-only this release).
- coordination primitives (inboxes, decision gates, task boards); git worktree provisioning.
- windows support (named-pipe transport, path conventions, task scheduler `enable`).
- TCP/HTTP listener for the socket API.
- data-dir lifecycle / retention GC for `~/.orcr/data`.
- presets (`orcr agent run @review …`).
- herdr plugin packaging (orcr `top` plugin pane, context actions, `herdr plugin install`).
- declarative YAML workflows + run replay.

## tech-debt & robustness

- **submit-confirm hardening for slow boots** — adaptive-window re-delivery landed
  post-manual-e2e; widen further / verify prompt acceptance by reading the pane if the boot
  flake recurs on loaded real-provider boxes. see spec/_impl/open-issues.md (E02).
- **decide whether to document the submit-confirm integration keys** — `submit_ready_ms`,
  `submit_confirm_ms`, `submit_attempts` are validated `integrations.<provider>.*` config today
  but undocumented in spec §14 (which lists only the completion-tuning keys); expose as public
  config or keep internal.
- **validate real-claude on a non-enterprise box** — stock claude persists transcripts
  normally, but the Avocado/MetaCode-wrapped claude's session id maps to no locatable
  `~/.claude/projects/**/<session>.jsonl` and herdr doesn't report `agent_status: working`, so
  orcr can't detect turn completion or resolve the wrapped session's transcript — blocking
  real-claude E01/E03/E05 + the E18 claude leg (see issues.md: real-claude `ask`/`wait`/`logs`
  on the enterprise box). validate on a stock claude box, or add an orcr-side
  session-id→transcript discovery fallback for wrapped launchers. see
  spec/_impl/manual-e2e-results.md.
- **live launchd/systemd `enable` round-trip** — golden unit-file tests cover content; the
  real login-session registration is manual-only. see spec/_impl/spec-completeness.md.

## docs

- update the README install section (SDK on npm; `curl … | sh` one-liner live) once the
  registry publish + install-script hosting land above.
