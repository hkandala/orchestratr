# 10 · Roadmap and future work

Live task tracking lives in [todo.md](todo.md) — this file holds milestone intent and
acceptance; todo.md holds the granular checklist and current status.

## M0 — Foundation ✅

Config + store + run dirs + herdr discovery/driver + profiles + mock agent + fake-herdr
unit suite + real-herdr e2e round-trip. **Accepted when:** `orcr status --json` works;
e2e round-trips prompt→response incl. scrape fallback; zero leftover e2e sessions.

## M1 — Core verbs

`run send wait out ps tree kill attach history(basic) gc` + herdr-style ids (a7/l2/a7:t2
— replaces M0's uuids) + env contract + steer semantics + response guarantee + profile
module split (one file per harness) + `--json`/exit codes + skill v0 draft.
**Accepted when:** the M1 e2e checklist in 09 is green; a human can fan out two mocks,
steer one, and read merged results without touching herdr directly.

## M2 — Jobs & daemon

`serve` (auto-start, pidfile, socket ping), events feed, loop (fixed/auto/tick-on),
schedule (cron + --at, catchup, expiry, tz echo), reconciler + gc hardening, concurrency
caps + queued state, budget knobs. **Accepted when:** M2 e2e checklist green; daemon
survives restart with correct reconciliation; a schedule fires with the daemon running
and catches up per policy after downtime.

## M3 — Faces

`top` TUI + auto-viewer, goal, workflow run, full history, token telemetry, SKILL.md
final, TS + Python SDK packages. **Accepted when:** M3 e2e checklist green; the
fan-out/steer/merge demo runs end-to-end from inside a real harness using only the skill.

## M4 — Reach (post-v1)

Remote hosts (herdr --remote, host registry), presets, replay, reroute policies,
launchd/systemd install, drop-in workflow verbs from `~/.orcr/workflows/`.

## Future work (explicitly parked)

- **Plugin system for agent integrations.** v1 keeps integrations in-tree (one module per
  harness — 05). The plugin evolution: external profile definitions loaded at runtime —
  first as **declarative TOML manifests** (`~/.orcr/profiles/<name>.toml`: launch argv
  template, completion strategy, markers, transcript glob + jq-style extraction path),
  then, if needed, **exec-plugins** (an executable implementing the profile contract over
  stdin/stdout JSON-RPC) for harnesses needing real logic. The in-tree trait is already
  the contract; plugins are alternative constructors for it.
- **herdr socket API** — adopt `events.subscribe` push (replacing polling) once the
  installed herdr CLI exposes it; keep CLI polling as fallback.
- **Permission handling** — `--read-only` (per-harness write-tool disabling), permission
  profiles per job (allowed_tools shape), `--on-blocked fail|wait|notify` policies.
- **Presets** — `orcr preset save review`; `orcr run @review -p "…"`.
- **Notifications beyond the terminal** — webhook/ntfy on blocked, goal-met, failed.
- **Replay** — `orcr replay w4` from recorded spec_json.
- **Declarative workflows** — small YAML compiling onto the script engine.
- **Windows** — named-pipe herdr, path conventions.
