# Live e2e tests — real harnesses via herdr

Manual/agent-driven end-to-end campaign against REAL harnesses (claude, codex, pi,
opencode) — no mocks. Run by parallel test agents, one per track. Status is updated as
results land; issues go to [issues.md](issues.md). Scripting these comes later, after
learnings.

## Ground rules (every track)

- **Sandbox only.** All agent work dirs live under `~/orcr-live-tests/track-<X>/work/…`.
  Prompts must only create/modify files in their cwd. No system settings, no installs,
  no network mutations, nothing outside the sandbox.
- **Isolated stores + sessions.** Each track: `ORCR_STORE=~/orcr-live-tests/track-<X>/store`
  with its own config (herdr session `orcr-live-<x>`, max_concurrent=3). NEVER touch the
  user's default herdr session or `~/.orcr`.
- **Setup template**:
  ```sh
  T=<x>; SB=~/orcr-live-tests/track-$T
  mkdir -p $SB/store $SB/work
  printf '[limits]\nmax_concurrent = 3\n\n[herdr]\nsession = "orcr-live-%s"\n' $T > $SB/store/config.toml
  export ORCR_STORE=$SB/store
  export PATH="$HOME/code/orchestratr/target/debug:$PATH"
  ```
- **Cleanup (always, even after failures)**: `orcr kill` all live ids → verify
  `orcr ps` empty → `herdr --session orcr-live-<x> server stop` →
  `herdr session delete orcr-live-<x>`.
- **Evidence per failure**: exact command, exit code, stderr, relevant `runs/<id>/`
  listing, `orcr show <id> --json`, herdr pane state if relevant.
- Real agents are nondeterministic — judge outcomes by *contract* (files exist, exit
  codes, tree shape), not exact wording. PARTIAL = contract met with caveats.

## Track A — basic runs across all four harnesses

| id | flow | steps / prompt | expect |
|---|---|---|---|
| A1 | claude run --wait | cwd `work/a1`: `orcr run -a claude -p "Create a file haiku.md in the current directory containing a haiku about terminal multiplexers, then reply DONE and quote the haiku." --wait` | exit 0 · `runs/a1/001-response.md` exists · `work/a1/haiku.md` exists · history shows done |
| A2 | codex run --wait | same shape, cwd `work/a2`, limerick.md about the borrow checker | same contract |
| A3 | pi run --wait | same shape, cwd `work/a3`, `three_facts.md` about SQLite | same contract |
| A4 | opencode run --wait | same shape, cwd `work/a4`, `tip.md` one shell tip | same contract (fast-turn grace path) |
| A5 | async lifecycle | `run -a claude` (async → prints id) → `orcr wait <id>` → `orcr out <id>` → `orcr show <id> --json` | id on stdout alone · wait exit 0 · out prints response · show: status done, 1 turn, children [], response path valid |
| A6 | prompt/response files | inspect `runs/<id>/` from A1: `001-prompt.md` = canonical prompt (no terminal noise), meta.json fields sane | file contract per spec/04 |
| A7 | fallback chain | `orcr run -a claude -p "Reply with exactly the word PONG and do not create or write any files." --wait` (conflicts with the write-your-answer preamble) | exit 0 · response file EXISTS anyway · `response_source` = transcript or scrape recorded in show --json |
| A8 | timeout | `run -a claude -p "Count from 1 to 500 slowly, one number per line of thought, then reply DONE" --timeout 20s --wait` | exit 3 · status timeout · pane closed · no zombie in `orcr ps` |
| A9 | error codes | `orcr out zzz --json` → 6 · `orcr run -a nosuch -p hi` → non-zero with clear message · `orcr send <done-id> --steer x` → 7 or 6 with state details |
| A10 | hygiene | `orcr ps --json`, `orcr tree --json`, `orcr history --json` parse; `orcr gc --dry-run` reports nothing unexpected | clean envelopes, exit 0 |

## Track B — skill-driven orchestration from inside Claude Code

| id | flow | steps / prompt | expect |
|---|---|---|---|
| B1 | boss spawns cross-harness child | `orcr run -a claude --keep --name boss -p "Read /Users/hkandala/code/orchestratr/skill/SKILL.md. Then use the orcr CLI to run a codex subagent with prompt: write a limerick about the rust borrow checker to limerick.md in your current directory then reply DONE. Wait for it, read its answer with orcr out, then reply with the subagent's id and the limerick text." --wait` (cwd `work/b1`) | boss completes · a codex child exists with `parent_id` = boss id (env contract worked) · boss response quotes limerick |
| B2 | lineage & tree | `orcr tree --json` after B1 | boss → child nesting correct; `orcr show <child> --json` parent field set |
| B3 | auto-viewer pane | after B1: `herdr --session orcr-live-b pane list` (or equivalent) | a pane labeled `orcr-top` exists · exactly ONE even after second spawn · boss pane still focused/unchanged (no focus steal) · subagent got its OWN pane |
| B4 | nested depth 3 | boss (kept) via `orcr send boss --turn "Now run a claude subagent whose prompt is: use the orcr CLI to run a pi subagent that writes a two-line poem to poem.md and replies DONE; wait for it and reply with its id. Wait for your subagent and reply with the full chain of ids."` | 3-level chain in `orcr tree` · ORCR_DEPTH increments (check `runs/<grandchild>` meta / spawn success) |
| B5 | recursive read | `orcr out boss --recursive --format path` | one line per descendant, id/name/path, paths exist |
| B6 | kill --tree | `orcr kill boss --tree` | boss + all descendants killed bottom-up · panes gone from herdr · statuses killed |

## Track C — interaction semantics (steer / turns / kill / wait)

| id | flow | steps / prompt | expect |
|---|---|---|---|
| C1 | steer mid-turn | cwd `work/c1`: `run -a claude --keep --name story -p "Write a 6-paragraph short story about a lighthouse keeper. Think carefully paragraph by paragraph, save the story to story.md, then reply DONE."` (async) → poll `show` until working ~10s → `orcr send story --steer "IMPORTANT CHANGE: set the story on a space station instead of a lighthouse."` → `orcr wait story` | steer accepted while working (JSON mode=steer) · `001-prompt.2.md` exists · exactly ONE `001-response.md` · story/response reflects space station |
| C2 | steer conflict | after C1 completes (idle, kept): `orcr send story --steer "x" --json` | exit 7 · error code state_conflict with current_status idle |
| C3 | multi-turn | `orcr send story --turn "Reply with exactly the word BANANA."` → `orcr out story --turn 2` and `orcr out story --turn 1` (or `story:t1`) | 002-prompt/response pair created · turn 1 response ≠ turn 2 · `:tN` sugar works |
| C4 | wait --any | start slow claude (write 5 paragraphs) + fast codex (reply OK) together → `orcr wait <slow> <fast> --any` | returns fast id promptly · slow still working after |
| C5 | kill mid-run | kill the slow C4 agent while working | status killed · exit_reason killed · pane closed · `ps` clean |
| C6 | names | `run --name a7 …` rejected (reserved) · reuse of live name `story` rejected · `orcr out story` resolves by name | clear errors, resolution works |

## Track D — jobs: loop, schedule, goal, workflow, daemon

| id | flow | steps / prompt | expect |
|---|---|---|---|
| D1 | loop --max | cwd `work/d1`: `orcr loop -a codex --every 15s --max 2 -p "Append exactly one line containing a timestamp to log.txt in the current directory, then reply DONE."` | daemon auto-starts (status --json daemon.running) · exactly 2 ticks → 2 lines in log.txt · job ends (max) · creation printed cadence + cancel hint |
| D2 | loop --until | cwd `work/d2`: loop `--every 15s --max 4 --until "ALL DONE"` with prompt "If log.txt in the current directory has 2 or more lines, reply ALL DONE. Otherwise append one line to it and reply ADDED." | stops on --until before max · history shows per-tick agents |
| D3 | schedule --at | `orcr schedule add --at "+2 minutes" -a codex -p "Write the word fired plus the current time into fired.txt in the current directory, reply DONE."` (cwd `work/d3`; if relative form unsupported use an absolute local time ~2min ahead) | confirmation echoes local + UTC · fires once within ~3min · job ended_reason fired · re-arm via `schedule resume <id> --at …` works or clean state_conflict without --at |
| D4 | goal + cross judge | cwd `work/d4`: `orcr goal -a claude -p "Create a file named exactly answer.txt in the current directory containing exactly the text 42 and nothing else." --judge-harness codex --max-iters 3` | worker+judge iterate · ends done with PASS · `job show` includes judge info (judge_independent true) · answer.txt correct |
| D5 | workflow | `work/d5/wf.sh`: bash script that runs two `orcr run -a codex … --wait` (facts about rust + sqlite → fact1.md fact2.md) then exits 0 → `orcr workflow run work/d5/wf.sh` | w-id created · both children parented to w-id in tree · script stdout captured to `runs/w1/log.txt` · workflow done |
| D6 | job surface | `orcr job ls --json` lists D1-D5 jobs with sane fields · `job show <goal>` · `job rm` on ended job works · `kill <live job>` stops it | consistent lifecycle, exit 7 where spec says |
| D7 | events | `orcr events --json` returns envelope; `orcr events --follow --json` streams NDJSON (sample 5s then ctrl-c equivalent) | event kinds cover spawn/status/turn/job ticks |
| D8 | reconcile | while an agent is working: `herdr … pane close <its pane>` manually → `orcr gc` (or daemon restart) | agent marked lost (not stuck working) · gc reports the action |

## Results

Filled in after the campaign: per-test PASS / PARTIAL / FAIL with notes, and issues
cross-referenced as issues.md ids.
