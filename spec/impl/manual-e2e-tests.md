# orchestratr — manual end-to-end test plan

The final manual e2e phase (master-prompt §8). These are **real** end-to-end scenarios
run against **live herdr 0.7.2** and, where marked, **real `claude`/`codex` providers** —
prioritizing coverage the mock-based automated e2e can't fully exercise (real transcript
adapters, real completion timing, real spawn/first-turn behavior). The mock provider is
used only where determinism matters (queue/concurrency, GC clocks, glob/path model,
top rendering, error codes).

Each test below is a whole scenario (many commands), not a single command. Execute them
one at a time; record outcomes in `manual-e2e-results.md`. This phase **reports** issues;
it does not silently fix them (the one exception is known-issue #2, which the plan
requires be root-caused + fixed + regression-tested — see **E01/E02**).

## Binaries

```
ORCR=/Users/hkandala/code/orchestratr/target/debug/orcr
ORCR_MOCK=/Users/hkandala/code/orchestratr/target/debug/orcr-mock-agent
```

Build once before starting: `cargo build` (both `orcr` and `orcr-mock-agent` land in
`target/debug/`).

## Safety / harness (STRICT — read before running anything)

Every test runs against a **throwaway `ORCR_HOME`** (a tempdir) and a **disposable herdr
session** named `orcr_e2e_<rand>`. NEVER touch the user's real herdr `default` session or
its panes; NEVER use `~/.orcr`. Tear down every disposable session and stop every server
you start, even on failure.

**Common setup** (`[SETUP]` — run at the start of each test; add the mock lines only for
`provider: mock` tests):

```bash
export ORCR=/Users/hkandala/code/orchestratr/target/debug/orcr
export ORCR_HOME="$(mktemp -d /tmp/orcr_e2e.XXXXXX)"
# rand from uuid (NOT a timestamp prefix — avoids near-simultaneous collisions)
export SESS="orcr_e2e_$(uuidgen | tr 'A-Z' 'a-z' | tr -d '-' | cut -c1-8)"
printf '{"herdr":{"session":"%s"}}\n' "$SESS" > "$ORCR_HOME/config.json"
export ORCR_HERDR_SESSION="$SESS"     # belt-and-suspenders (any orphan child pins it)
export ORCR_DISABLE_DISCOVERY=1       # omit ONLY in the unmanaged-discovery test
# --- mock-provider tests only: ---
export ORCR_ALLOW_MOCK_PROVIDER=1
export ORCR_MOCK_AGENT_BIN=/Users/hkandala/code/orchestratr/target/debug/orcr-mock-agent
```

**Common teardown** (`[TEARDOWN]` — run at the end of EVERY test, even on failure):

```bash
"$ORCR" agent kill "**" -y 2>/dev/null || true    # close any panes orcr owns
"$ORCR" server stop  2>/dev/null || true
herdr session stop "$SESS"   2>/dev/null || true  # kill panes BEFORE delete
herdr session delete "$SESS" 2>/dev/null || true
rm -rf "$ORCR_HOME"
# LEAK CHECK — MUST show neither "$SESS" nor a bare "orcr" session:
herdr session list | grep -E "orcr(_e2e)?" && echo "LEAK!" || echo "no leak"
```

**Real-provider cost discipline:** real `claude`/`codex` agents cost money and take real
wall-clock time. Keep each test to one or two short agents, prefer `--gc immediate` (or an
explicit `--timeout`), use trivial prompts (`say READY`, `reply with the word PONG`), and
always clean up. Do NOT spawn fleets of real agents.

## Results

Record every run in [`manual-e2e-results.md`](manual-e2e-results.md): test id, provider,
pass/fail, expected vs actual, exit code, any `--json` error `{code,details}`, and notes.
After each test, confirm the leak check printed `no leak`.

---

## Tests

### E01 — CRITICAL: `agent ask` against a REAL claude (known-issue #2 repro → fix)

- **area:** agent · ask · transcript adapter · gc-immediate ordering
- **provider:** claude
- **priority:** critical
- **steps:**
  1. `[SETUP]` (no mock env — this is a real provider).
  2. `"$ORCR" agent ask --name quick_check -a claude -p "Reply with exactly the word PONG and nothing else." --timeout 3m` on a plain shell (no `ORCR_*` scope).
  3. Repeat with `--json`: `"$ORCR" agent ask --json --name quick_check2 -a claude -p "Reply with exactly the word PONG." --timeout 3m`.
  4. Capture stderr, exit code, and (for `--json`) the full envelope; check `"$ORCR" server logs --tail 100` for the GC-immediate teardown / transcript-locate lines; run `"$ORCR" agent ls --all --json` to see the ended row's `exit_reason`.
  5. `[TEARDOWN]`.
- **expected:** stdout prints the model's final response (contains `PONG`); exit 0; `--json` envelope `{"ok":true,"result":{uuid,path,response:{text,final}}}`. The ended agent shows `exit_reason: completed`. **If it fails** (the manual-testing symptom): capture the exact failure (`transcript_unavailable` / premature pane teardown / wait never settling / CLI error surfacing), root-cause per known-issues.md #2, FIX it, and add a regression test (real-provider smoke where feasible, else a mock test over the same code path). Record the root cause + fix in results.

### E02 — CRITICAL: `agent ask` against a REAL codex

- **area:** agent · ask · codex transcript adapter
- **provider:** codex
- **priority:** critical
- **steps:**
  1. `[SETUP]` (no mock env).
  2. `"$ORCR" agent ask --name quick_check -a codex -p "Reply with exactly the word PONG and nothing else." --timeout 3m`.
  3. `"$ORCR" agent ask --json --name quick_check2 -a codex -p "Reply with exactly the word PONG." --timeout 3m`; inspect `server logs --tail 100` and `agent ls --all --json`.
  4. `[TEARDOWN]`.
- **expected:** same as E01 but for codex — response on stdout, exit 0, `--json` response object, ended `exit_reason: completed`. The codex transcript path is `~/.codex/sessions/**/rollout-*-<session_id>.jsonl`; confirm the adapter locates it via the identity gate (not cwd-mtime). Any failure → root-cause + fix + regression test, recorded in results.

### E03 — Full managed lifecycle on REAL claude: run → wait → logs → send → wait

- **area:** agent · run/wait/logs/send · env contract · completion
- **provider:** claude
- **priority:** high
- **steps:**
  1. `[SETUP]` (no mock env).
  2. `"$ORCR" agent run --name worker -a claude --gc never -p "You are a helper. When you get a prompt, answer briefly. Say READY now." --timeout 15m` — capture the printed `<path> <uuid>` line.
  3. `"$ORCR" agent wait worker` → expect `worker turn_complete`, exit 0.
  4. `"$ORCR" agent logs worker --last-response` → expect the first response text (contains `READY`).
  5. `"$ORCR" agent send worker "What is 2+2? Answer with just the number."` → expect `delivered_while: idle` (or `working`), and a fresh `input_seq`.
  6. `"$ORCR" agent wait worker` then `"$ORCR" agent logs worker --last-response` → expect `4` (a *new* turn's response — the stale first idle must NOT satisfy the second wait).
  7. `"$ORCR" agent ls --json` and confirm the row shows provider `claude`, status, and correct absolute `path`. Check `$ORCR_HOME/data/worker/<uuid>/launch.json` exists.
  8. `"$ORCR" agent kill worker -y` → ended `exit_reason: killed`, pane closed (verify workspace removed via `herdr --session "$SESS"` state / `agent ls`).
  9. `[TEARDOWN]`.
- **expected:** each step exits 0; the second `wait`/`logs` reflects the *second* turn (not the first); `send` re-arms the agent to `working`; env contract present in the pane (data dir + launch.json written); kill closes the pane and empties the workspace.

### E04 — Full managed lifecycle on REAL codex: run → send → logs → kill

- **area:** agent · codex driver/integration · send while idle
- **provider:** codex
- **priority:** high
- **steps:**
  1. `[SETUP]` (no mock env).
  2. `"$ORCR" agent run --path proj/coder -a codex --gc never -p "Say READY." --timeout 15m`.
  3. `"$ORCR" agent wait proj/coder`; `"$ORCR" agent logs proj/coder --last-response`.
  4. `"$ORCR" agent send proj/coder "Reply with the single word DONE."`; `"$ORCR" agent wait proj/coder`; `logs proj/coder --last-response` → `DONE`.
  5. `"$ORCR" agent ls proj/**` shows the agent under workspace `proj`, tab `coder`.
  6. `"$ORCR" agent kill "proj/**" -y`.
  7. `[TEARDOWN]`.
- **expected:** codex spawns, completes turns, `send` delivers and starts a new tracked turn, logs resolve from the real codex transcript, glob kill closes the pane. Exit 0 throughout.

### E05 — `logs` variants on REAL claude: `--tail`, `--follow`, `--last-response` freshness

- **area:** agent · logs · transcript adapter · streaming
- **provider:** claude
- **priority:** high
- **steps:**
  1. `[SETUP]` (no mock env).
  2. `"$ORCR" agent run --name talker -a claude --gc never -p "List three fruits, one per line." --timeout 10m`; `"$ORCR" agent wait talker`.
  3. `"$ORCR" agent logs talker --tail 5` → last 5 transcript entries (structured turns, roles).
  4. In one shell: `"$ORCR" agent logs talker --tail 2 --follow` (leave running). In another shell (same `ORCR_HOME`/`SESS` exported): `"$ORCR" agent send talker "Now list two vegetables."` → the follow stream shows the new turn's entries appear live; Ctrl-C the follow.
  5. `"$ORCR" agent logs talker --last-response` immediately after the send+wait → the *vegetables* answer, not the fruits (freshness gate: response reported only after transcript advances past the completion).
  6. `[TEARDOWN]`.
- **expected:** `--tail` bounds history; `--tail N --follow` prints the tail then streams new entries (docker/kubectl semantics); `--last-response` never returns a stale prior response after a new turn.

### E06 — Identity, paths, globs, scope resolution (deterministic)

- **area:** core · §5.1 identity/path/glob · reserved names · {rand} · resolution
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env).
  2. Spawn a small tree: `agent run --path review/fanout/file_1 -a mock --gc never -p "@say=ok"`, `--path review/fanout/file_2`, `--path review/synth`, `--name lonely` (scope-less → workspace `default`).
  3. **Glob node sets** — verify each returns exactly the intended set (compare to §5.1): `agent ls "review/*"` (direct children of review → `synth` only, not the nested fanout ones), `agent ls "review/**"` (everything under review, never `review` itself), `agent ls "review/fanout/*"`, `agent ls "*"` (level-1 nodes).
  4. **{rand}:** `agent run --path "batch_{rand}/w1" -a mock -p "@say=ok"` twice → two distinct `batch_xxxxx` roots; confirm `{rand}` is rejected in a *selector* (`agent ls "batch_{rand}/*"` → `invalid_request`).
  5. **Reserved level-1:** `agent run --name idle -a mock` and `--path unmanaged/x` and `--path /idle/y` → `invalid_request` `reason:"reserved_name"`; but `--path review/idle` (level-2) succeeds.
  6. **Depth limit:** a 9-segment `--path a/b/c/d/e/f/g/h/i` → `invalid_request` `reason:"path_too_deep"`.
  7. **Resolution / path-in-use:** two concurrent `agent run --path review/synth` (same path) → exactly one wins, the other `state_conflict` `reason:"path_in_use"` with occupying `{uuid,path,status}`. Then `agent ls --all` disambiguates reused paths by uuid+created_at.
  8. **Wildcards rejected by exact verbs:** `agent send "review/*" "hi"` and `agent logs "review/**"` → `invalid_request`.
  9. `[TEARDOWN]`.
- **expected:** glob node sets match §5.1 exactly (`*`=one whole segment, `**`=any depth, anchored, never partial); `{rand}` expands only in creation and is rejected in selectors; reserved names blocked at level-1 only; depth guarded; concurrent same-path yields one winner + `state_conflict`; exact-target verbs reject patterns. Correct exit codes (1 for invalid_request, 7 for state_conflict).

### E07 — Queue + concurrency caps (global + per-provider FIFO, never over cap)

- **area:** core · §5.5 queue/concurrency
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env) but first set caps: add `"concurrency":{"max":3,"mock":2}` to `$ORCR_HOME/config.json` (keep the herdr.session key).
  2. Spawn 10 slow mock agents: `for i in $(seq 1 10); do "$ORCR" agent run --path burst/w$i -a mock --gc never -p "@turn_ms=60000"; done`.
  3. Immediately `"$ORCR" agent ls --json` → at most `max` (3) in `starting`/`working`, at most `mock`-cap (2) of provider mock working, the rest `queued` with ascending `queue_position`.
  4. Poll `agent ls --json` over ~30s → promotion is strictly FIFO by queue order; the count never exceeds the caps.
  5. `"$ORCR" agent kill "burst/**" -y` → queued ones dequeue as `canceled`, running ones `killed`.
  6. `[TEARDOWN]`.
- **expected:** never more than `concurrency.max` active total nor the per-provider cap active per provider; promotion FIFO by `queue_seq`; `wait` on a queued agent waits through promotion; kill dequeues queued (`canceled`) and kills running (`killed`).

### E08 — GC auto: park → send → un-park → reap (shortened timings)

- **area:** core · §5.4/§11.2 GC engine · two-phase moves
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env); set fast timings in config: `"timings":{"idle_after":"3s","kill_after":"4s","gc_tick":"1s"}`.
  2. `agent run --path gc/a -a mock --gc auto -p "@say=ok"` (completes turn fast → idle).
  3. Wait ~5s; `agent ls --json` → status `parked`, pane moved to the `idle` workspace (verify via herdr session state that the pane is in workspace `idle`); a `parked` row records its home workspace.
  4. `agent send gc/a "@say=back"` → the agent un-parks (moves back to home workspace `gc`), status returns to `working`→`idle`, both GC clocks reset; delivery addresses the live terminal_id.
  5. Let it park again, then wait past `kill_after` (~5s more) → agent `ended` `exit_reason: reaped`, pane closed, workspace `idle` emptied/removed.
  6. Spawn `agent run --path gc/pin -a mock --gc never -p "@say=ok"`, wait past the windows → stays `idle`, never parked/reaped.
  7. `[TEARDOWN]`.
- **expected:** idle-past-`idle_after` → `parked` in `idle` workspace; `send` un-parks to home + resets clocks; parked-past-`kill_after` → `ended(reaped)` with pane closed; `gc never` exempt. No leaked panes/workspaces.

### E09 — GC immediate vs never (teardown ordering)

- **area:** core · §5.4 gc immediate/never · response-before-kill
- **provider:** mock
- **priority:** normal
- **steps:**
  1. `[SETUP]` (with mock env).
  2. `agent run --path once/a -a mock --gc immediate -p "@say=result_a"`; `agent wait once/a` → settles on `ended(completed)` (NOT a transient public `idle`).
  3. `agent logs once/a --last-response` → `result_a` (the response was captured/readable BEFORE the pane closed).
  4. `agent ls --all --json` → `once/a` ended `exit_reason: completed`, pane gone.
  5. `agent run --path once/pin -a mock --gc never -p "@say=x"`; wait; confirm it stays alive (idle) until explicit kill.
  6. `[TEARDOWN]`.
- **expected:** `gc immediate` closes the pane only after the final response is verified readable; `wait` on it settles `completed`; `logs --last-response` still resolves post-kill (transcript locator recorded); `gc never` persists.

### E10 — Loops: create (cron + `--once-at`), run start/stop/ls/logs, overlap coalesce

- **area:** loop · §6.2/§11.3 scheduler · runs · overlap
- **provider:** mock (loop command spawns mock agents / trivial argv)
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env).
  2. **create echo:** `"$ORCR" loop create nightly "0 9 * * *" -- /bin/echo hello` → prints parsed argv, cadence-in-words + local/UTC next fire, and the cancel command; `loop ls --json` shows `next_fire_at`, `max_concurrency:1`, `overlap:queue`.
  3. **--once-at:** `"$ORCR" loop create oneshot --once-at 5s -- /bin/echo once` → after ~6s `loop run ls oneshot --all --json` shows one run `ok`; the loop ends (name reusable).
  4. **manual run + logs:** `"$ORCR" loop run start nightly` → prints `nightly/<run_id> <run_uuid>`; `"$ORCR" loop run ls nightly --all` shows it; `"$ORCR" loop logs nightly` interleaves the command's stdout (`[nightly/rXXXXX] hello`) and orcr scheduler actions (`fired`), each tagged by run; `--source command` / `--source orcr` / `--run <run_id>` filter correctly.
  5. **overlap coalesce (cap 1):** `"$ORCR" loop create slow --max-concurrency 1 --overlap queue -- /bin/sh -c 'sleep 30'`; fire it 3× rapidly via `loop run start` → at most one pending *scheduled* run beyond the running one for scheduled fires (manual runs each allocate their own pending row); `loop run ls slow` shows the pending/running set.
  6. **loop run stop:** with two concurrent runs of a cap-2 loop, `"$ORCR" loop run stop <name> <run_id>` stops one (status `stopped`, its agents glob-killed `<loop>/<run_id>/**`) while the other survives.
  7. `[TEARDOWN]` (also `loop rm` any live loops).
- **expected:** create echoes argv+cadence+next-fire; `--once-at` fires once then ends; run ids are `r`+5 chars; `loop logs` interleaves+tags both sources and filters; overlap `queue` coalesces scheduled fires to ≤1 pending; `loop run stop <run_id>` targets exactly one run.

### E11 — Loop restart recovery + pause/resume/rm

- **area:** loop · §11.3 restart recovery · pause/resume/rm · process groups
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env).
  2. `"$ORCR" loop create job --max-concurrency 1 -- /bin/sh -c 'sleep 60'`; `"$ORCR" loop run start job` → a running run (record pgid via `loop run ls job --all --json`).
  3. **kill -9 the server** (simulate crash): find the server pid from `server status --json` (or `pgrep -f "orcr server"` scoped to this `ORCR_HOME`), `kill -9` it. Any CLI call auto-starts a fresh server.
  4. `"$ORCR" loop run ls job --all --json` after restart → the recovery transaction verified the run's pgid by start-time: a dead run is closed out (its agents glob-killed), a still-live one is kept; pending fires decided once; `next_fire_at` recomputed; missed fires skipped-and-logged (`loop logs job --source orcr`).
  5. **pause/resume:** `loop pause job` → no new fires; a pending scheduled run is held. `loop resume job` → fires resume, a held pending run starts if due.
  6. **rm:** `loop rm job` (non-destructive) → definition ends, running run continues; then `loop create job2 -- /bin/echo x; loop run start job2`; `loop rm job2 --kill-active -y` → active run + agents killed. `loop ls --all` keeps history.
  7. `[TEARDOWN]`.
- **expected:** restart recovery is pid-reuse-safe (start-time match), never signals a non-matching pgid, decides pending once, skips missed fires with event rows; `pause` holds fires, `resume` restarts held work; plain `rm` is non-destructive (no prompt), `rm --kill-active` confirms on TTY and kills the run+agents. No leaked run process groups after teardown.

### E12 — `server enable` / `disable` (launchd on macOS)

- **area:** server · §6.4 service unit
- **provider:** none
- **priority:** normal
- **steps:**
  1. `[SETUP]` (env only; do NOT rely on the real user LaunchAgents — use a throwaway `ORCR_HOME`; the plist is written to `~/Library/LaunchAgents/dev.orchestratr.orcr.plist`). **Note:** enable touches a real user-level path — inspect but then `disable` to clean up; verify the plist points at the throwaway `ORCR_HOME`.
  2. `"$ORCR" server enable` → prints the created unit path + the verify command; the plist contains the **absolute** orcr binary path, `argv orcr server start --foreground`, `RunAtLoad`, `KeepAlive`, and propagates `ORCR_HOME`/`ORCR_HERDR_BIN`.
  3. `launchctl list | grep dev.orchestratr.orcr` → loaded (best-effort).
  4. `"$ORCR" server disable` → removes the plist + unloads; registration gone (a running server + store untouched).
  5. `[TEARDOWN]` (ensure the plist is removed even on failure: `rm -f ~/Library/LaunchAgents/dev.orchestratr.orcr.plist; launchctl remove dev.orchestratr.orcr 2>/dev/null`).
- **expected:** enable writes a correct launchd plist (absolute binary, propagated env, RunAtLoad/KeepAlive) and best-effort loads it; disable removes+unloads; both echo the platform verify command; exit 0.

### E13 — `top`: launch, filters, live updates

- **area:** top · §7 TUI
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env).
  2. Spawn a tree: `agent run --path refactor/phase_1/file_1 -a mock --gc never -p "@turn_ms=60000"`, `refactor/phase_1/review -p "@block"`, `verify/checker`, plus a loop `loop create nightly --once-at 60s -- /bin/sh -c 'sleep 45'` + `loop run start nightly`.
  3. Launch `"$ORCR" top` in a real terminal → renders the path tree (level-1 nodes as workspaces, loop + active run as a subtree, blocked agent floats up with `◐`), header shows agent/loop counts.
  4. **Filters:** `"$ORCR" top "refactor/**"`, `"$ORCR" top -a mock`, `"$ORCR" top --status blocked`, `"$ORCR" top --loops` — each pre-scopes the tree; the agent node set equals the equivalent `agent ls` query.
  5. **Live update:** with `top` open, from another shell `agent send refactor/phase_1/review "@say=cleared"` → the blocked row transitions live; `agent kill verify/checker -y` → the node disappears without a full-screen glitch.
  6. **`/` filter + nav:** inside `top`, press `/`, type `refactor/**`, Enter; use arrows to collapse/expand; `q` to quit.
  7. `[TEARDOWN]`.
- **expected:** tree matches the path model + lineage annotations (`↖ parent` for cross-scope), CLI filters == `ls` node sets, `/` filter uses §5.1 grammar, live events update the tree with no missed/dup rows, `q` exits cleanly. (This is a visual test — capture a screenshot/description in results.)

### E14 — Socket API: `api schema` + `api snapshot`

- **area:** api · §6.5/§11.6 self-describing protocol
- **provider:** mock
- **priority:** normal
- **steps:**
  1. `[SETUP]` (with mock env).
  2. `"$ORCR" api schema --json` → valid JSON; every CLI verb maps to a method; params/results/event kinds/error codes present; pipe through a JSON-Schema validator (`jsonschema`) or `python3 -c "import json,sys;json.load(sys.stdin)"`.
  3. Spawn 2 mock agents + 1 loop; `"$ORCR" api snapshot --json` → one consistent document stamped with `snapshot_seq`, carrying `agents[]` (flat rows incl. `model/move_state/herdr_session/…`), `queue[]`, `loops[]` (with each loop's active `runs`).
  4. Cross-check: the snapshot's agent set equals `agent ls --json`; `server status --json` `counts` reconcile with the snapshot.
  5. `[TEARDOWN]`.
- **expected:** `api schema` is valid JSON Schema with 100% method coverage; `api snapshot` is one internally consistent, seq-stamped document whose agent/loop set matches `ls`/`status`.

### E15 — `server` start/stop/status/logs + auto-start race

- **area:** server · §6.4/§11.6 lifecycle, single-instance, auto-start
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env).
  2. `"$ORCR" server status` before anything → auto-starts, then reports version, protocol, socket/store paths, herdr bin/version/socket/session reachable, integration state per provider, counts, `loops_firing`, drift.
  3. **Auto-start race:** with no server running, launch several CLI calls concurrently: `for i in 1 2 3 4 5; do "$ORCR" agent ls & done; wait` → exactly ONE server ends up running (single-instance lock; losers wait for readiness), no `server_start_failed`.
  4. `"$ORCR" server logs --tail 50` → startup, herdr connection, GC/reconcile lines; `--follow` streams live (Ctrl-C to stop).
  5. **Graceful stop:** spawn a `--gc never` mock agent; `"$ORCR" server stop` → server exits, subscriptions closed with `server_stopping`, **the agent pane keeps running** (verify via herdr session state). A subsequent `agent ls` auto-starts the server again and still sees the agent.
  6. **kill -9 restart:** `kill -9` the server pid; next CLI call restarts cleanly with an intact store (reconciliation repairs running rows).
  7. `[TEARDOWN]`.
- **expected:** status is complete + accurate; the auto-start race yields one server; stop is a control-plane stop that never kills agents; `logs --follow` streams; kill -9 → clean restart + intact store.

### E16 — TS SDK: scope / ask / run-handle / watch / loop against the live server

- **area:** sdk · §8 client
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env); build the SDK: `(cd /Users/hkandala/code/orchestratr/sdk/ts && npm ci && npm run build)`.
  2. Write a small driver `.ts` using the built `dist/` (set `ORCR_BIN=$ORCR`): `orcr.scope("wf", …)` spawning `orcr.agent.run({path:"fanout/a", agent:"mock", prompt:"@say=ok"})`, awaiting `handle.wait()`, `handle.lastResponse()`, `handle.dataDir`; `orcr.agent.wait("fanout/*")`; `const ans = await orcr.ask({agent:"mock", name:"q", prompt:"@say=hi"})`.
  3. `orcr.watch({pattern:"wf/**"})` → iterate a few typed events (`agent.status_changed`, `queue.promoted`) while a second agent runs.
  4. `orcr.loop.create({name:"burn", cron:"*/30 * * * *", command:["/bin/echo","x"]})`; `orcr.loop.run.start("burn")`; `orcr.loop.run.ls("burn")`; `orcr.loop.rm("burn")`.
  5. **scope parity:** confirm an SDK-composed path (`wf` scope + `fanout/a`) equals the CLI's absolute path for the same nested scope; a nested `orcr.scope("phase_1", …)` stacks prefixes.
  6. **error typing:** trigger a `state_conflict` (duplicate path) → SDK throws the typed `StateConflict` with `{code,details}`.
  7. `[TEARDOWN]`.
- **expected:** SDK helpers map to the socket API; `scope()` composes/nests (AsyncLocalStorage) and matches CLI paths; `ask/run/wait/logs/watch/loop.*` all round-trip; failures surface as typed errors. `npm test` in `sdk/ts` (path parity, codegen 100% coverage) also passes.

### E17 — Scaffold a workflow project and run `workflow.ts`

- **area:** scaffold · §6.6 · SDK integration
- **provider:** mock
- **priority:** high
- **steps:**
  1. `[SETUP]` (with mock env). Build an SDK tarball for offline install: `(cd /Users/hkandala/code/orchestratr/sdk/ts && npm run build && npm pack)` → note the `.tgz`; `export ORCR_SDK_SPEC=<abs path to tgz>`; `export ORCR_BIN=$ORCR`.
  2. `WF="$(mktemp -d)"; "$ORCR" scaffold "$WF"` → creates exactly `package.json` (pinning `@orchestratr/sdk` to the CLI version + tsx/typescript), `tsconfig.json`, `workflow.ts`; runs `npm install`. Confirm the pinned version == the CLI version.
  3. Edit `workflow.ts` to use `agent:"mock"` prompts (`@say=...`), then `(cd "$WF" && npx tsx workflow.ts)` → runs green: scope → run --name → wait → last-response prints.
  4. **Re-scaffold guard:** `"$ORCR" scaffold "$WF"` again → `state_conflict`, nothing overwritten.
  5. **Node preflight:** temporarily simulate missing node (`PATH` without node) `"$ORCR" scaffold "$(mktemp -d)"` → `environment_error` with an install pointer; nothing created.
  6. `[TEARDOWN]` (also `rm -rf "$WF"`).
- **expected:** scaffold creates exactly three files + installs, pins SDK==CLI version, `npx tsx workflow.ts` runs green against the mock; re-scaffold → `state_conflict`; missing Node → `environment_error` with nothing created.

### E18 — §9 recipes against a REAL provider (fan-out + tournament)

- **area:** recipes · §9 patterns · scope isolation
- **provider:** claude + codex
- **priority:** high
- **steps:**
  1. `[SETUP]` (no mock env; the recipes call real providers). Ensure the SDK is built (E16 step 1). `export ORCR_BIN=$ORCR`.
  2. **Fan-out & merge (§9.2)** on a tiny fixed input (2 short "files" as inline text so it's cheap): spawn 2 `gc:immediate` claude reviewers under `orcr.scope("orcr_e2e_review")`, each writing `$ORCR_AGENT_DATA_DIR/response.md` then `DONE`; `wait("fanout/*")`; a codex synthesizer `ask` merges. Keep prompts one line; set a `timeout`.
  3. **Tournament (§9.6)** with 3 short candidate strings, one claude judge per match → returns a winner.
  4. Confirm the file convention works (reviewers wrote to their `dataDir/response.md`, caller read+validated them) and scopes stay isolated (agents land under `orcr_e2e_review/**` / `tournament/**`).
  5. `[TEARDOWN]`.
- **expected:** the fan-out produces per-file findings + a merged summary; the tournament returns a single winner; real transcripts + the file convention both work end-to-end; scopes don't collide. (Cost-bounded: ≤ ~6 short real agents total, all `gc:immediate`/`ask`.)

### E19 — Skill hot path drill (real claude reads SKILL.md and orchestrates)

- **area:** skill · §10 · end-to-end "any agent gains orcr powers"
- **provider:** claude
- **priority:** normal
- **steps:**
  1. `[SETUP]` (no mock env).
  2. Verify the skill doc-tests: `cargo test --test skill_docs` (no stale CLI flags vs `--help`; every `agent run`/`ask` sample carries `--name`/`--path`).
  3. Manual drill: give a real claude agent access to `skill/SKILL.md` + `references/` and a task like *"use orcr to ask a codex agent for the capital of France and report its answer"*; confirm it follows the decision ladder and issues a correct `orcr agent ask --name … -a codex -p "…"` (or the run→wait→logs path), then relays the answer.
  4. `[TEARDOWN]`.
- **expected:** the skill doc-tests pass; a real agent, given only the skill, produces syntactically correct, naming-mandatory orcr commands and completes the delegation. Record the exact commands the agent emitted.

### E20 — Config validation + env contract + `ORCR_HOME` relocation

- **area:** config · §14 · §5.3 env contract
- **provider:** mock
- **priority:** normal
- **steps:**
  1. `[SETUP]` (with mock env).
  2. **Strict validation:** write a config with an unknown key (`"concurency":{"max":5}` typo) + a bad duration (`"timings":{"idle_after":"5"}` no unit) + `"concurrency":{"max":0}`. `"$ORCR" server status` → unknown key warns with a nearest-name suggestion (in `server logs`), the bad duration / `max:0` are rejected as `config_invalid` (`environment_error`, exit 2) or clamped with a warning per spec; fix and confirm a valid config loads.
  3. **Per-provider clamp:** `"concurrency":{"max":3,"mock":10}` → mock cap clamped to 3 with a warning.
  4. **Env contract:** spawn `agent run --path proj/child -a mock -p "@say=ok"` from a shell where `ORCR_ID`/`ORCR_PATH` are set to a fake parent agent's values → the child records `parent_id/parent_path` (lineage) and resolves relative paths against the caller's scope; the pane env (`$ORCR_AGENT_DATA_DIR/mock_env.json`) carries `ORCR_ID/ORCR_PATH/ORCR_PARENT_*/ORCR_AGENT_DATA_DIR` all absolute.
  5. **Relocation:** confirm store/socket/lock/config/logs/data all live under `$ORCR_HOME` (not `~/.orcr`).
  6. `[TEARDOWN]`.
- **expected:** unknown keys warn (nearest-name), invalid durations/`max<1` rejected or clamped per §14, precedence CLI>config>default; the §5.3 env contract reaches the pane with absolute values + lineage; `ORCR_HOME` relocates everything.

### E21 — Error codes & exit-code mapping sweep

- **area:** cli · §13 error enum + exit codes
- **provider:** mock
- **priority:** normal
- **steps:**
  1. `[SETUP]` (with mock env).
  2. Exercise each mapping and check exit code + `--json` `{code}`:
     - `not_found` (exit 6): `agent send nonexistent "hi"`; `agent wait "no/match/**"`.
     - `invalid_request` (exit 1): `agent run` with neither `--name` nor `--path`; both together; bad cron `loop create x "99 * * * *" -- echo hi`; bad duration `--timeout 5`.
     - `state_conflict` (exit 7): duplicate `--path`; `agent kill` on an unmanaged agent without `--force` (needs the discovery test setup — or use a starting agent race); `scaffold` into a populated dir.
     - `blocked` (exit 4): `agent run --path b/x -a mock -p "@block"` then `agent wait b/x`.
     - `timeout` (exit 3, agent's own): `agent run --path t/x -a mock --timeout 2s -p "@turn_ms=60000"` then `agent wait t/x` → reason `timeout`.
     - wait-timeout (exit 3, ok:true): `agent run --path w/x -a mock -p "@turn_ms=60000"`; `agent wait w/x --timeout 2s` → `ok:true, timed_out:true`, reason `wait_timeout`.
     - `integration_missing` (exit 2): `agent run --name p -a pi -p hi` (unsupported provider).
     - `transcript_unavailable` (exit 1): `agent logs <agent> --last-response` on an agent with no identifiable response (mock with `ORCR_MOCK_NO_TRANSCRIPT`).
     - `environment_error` (exit 2): point `ORCR_HERDR_BIN` at a bogus path and run a spawn → `herdr_unreachable`; `server enable` on an unsupported platform is N/A here.
  3. `[TEARDOWN]`.
- **expected:** each condition returns the documented error code AND the mapped exit code; wait-timeout is `ok:true`+exit 3 (distinct from an agent's own `timeout` error). `--json` carries `{code,message,details}` with the finer `reason`/`cause`.

### E22 — Attach: prepare, lease, and the GC interlock

- **area:** agent · attach · §5.4/§6.1 attach leases
- **provider:** mock
- **priority:** normal
- **steps:**
  1. `[SETUP]` (with mock env); set fast GC timings (as E08) so park/reap would fire quickly.
  2. `agent run --path at/a -a mock --gc auto -p "@say=ok"`; wait for idle.
  3. **prepare via API/SDK:** call `agent.attach.prepare` (or `orcr.agent.prepareAttach("at/a")`) → returns the `herdr agent attach` exec command + `leaseId/uuid/path/terminalId`; the lease is inserted *before* reading the pane locator (GC can't move/reap between resolution and lease).
  4. While a lease is fresh (heartbeat it, or hold within the SDK), wait past `idle_after`+`kill_after` → the agent is NOT parked/reaped (GC defers + logs it in `server logs`).
  5. Release the lease (or let it expire by heartbeat timeout `attach_lease_ttl`) → GC resumes; the agent parks/reaps.
  6. **exec the real attach** briefly: run the returned `herdr agent attach` command in a terminal, observe the mock pane, detach → the CLI releases the lease on exit.
  7. Interlock across restart: with a fresh lease, `kill -9` the server; after restart the persisted lease still defers GC until it expires.
  8. `[TEARDOWN]`.
- **expected:** `prepare` inserts the lease under the same txn as the locator read; a fresh lease defers park/reap (survives a restart); lease release/expiry re-enables GC; the exec command attaches to the real pane and the lease is released on detach.
