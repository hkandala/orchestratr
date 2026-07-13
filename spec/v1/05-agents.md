# 05 · Agent integrations

## Design rule: one module per harness, one shared contract

Every supported harness lives in its **own file** under `src/profile/`, implementing the
same `Profile` trait. The engine never contains harness-specific branches; adding a
harness = adding one file + registering it. This is the extensibility seam — an external
plugin mechanism (out-of-tree profiles) builds on the same contract later (10).

```
src/profile/
  mod.rs        # Profile trait, registry, shared helpers (flag pushing, marker matching)
  claude.rs     # Claude Code
  codex.rs      # Codex
  pi.rs         # Pi
  opencode.rs   # OpenCode
  mock.rs       # orcr-mock-agent (test harness)
```

## The `Profile` contract

```rust
pub trait Profile {
    fn harness(&self) -> &'static str;                  // "claude", "codex", …
    /// argv to launch the interactive TUI (bypass = v1 default true)
    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String>;
    /// optional headless argv for --mode exec (None = exec unsupported)
    fn exec_argv(&self, model: &str, effort: &str, prompt: &str) -> Option<Vec<String>>;
    /// screen-scrape driven prep before the first prompt (update modals etc.)
    fn startup_recipe(&self) -> &[StartupStep];
    /// how turn completion is detected
    fn completion(&self) -> Completion;   // StatusTransition | StatusWithGrace(ms)
                                          // | OutputMarker { done, blocked }
    /// graceful shutdown inputs tried before pane close
    fn shutdown_recipe(&self) -> &[ShutdownStep];
    /// recover the final response from the harness's native transcript
    fn transcript(&self) -> Option<&dyn TranscriptAdapter>;
    /// recognize rate-limit / usage-cap screens (best effort)
    fn limit_screen_markers(&self) -> &[&'static str];
}
```

`TranscriptAdapter::extract_last_response(session_ref) -> Option<String>` plus
best-effort `tokens(session_ref) -> Option<(u64, u64)>`.

## Per-harness specifics (v1)

| harness | launch argv (bypass mode) | completion | transcript |
| --- | --- | --- | --- |
| claude | `claude --dangerously-skip-permissions [--model M] [--effort E]` | StatusTransition | `~/.claude/projects/**/<session_id>.jsonl`, last assistant text; usage fields for tokens |
| codex | `codex --dangerously-bypass-approvals-and-sandbox [--model M] [-c model_reasoning_effort="E"]` | StatusTransition | `~/.codex/**/*<session_id>*.jsonl`: task_complete.last_agent_message → agent_message → response_item output_text |
| pi | `pi [--model M] [--thinking E]` | StatusTransition | `~/.pi/agent/sessions/**/*.jsonl`, last assistant message |
| opencode | `opencode [--model M]` | StatusWithGrace(5000) — can finish faster than herdr's poll sees `working` | `opencode export <session_id>` JSON |
| mock | `orcr-mock-agent` | OutputMarker { done: `MOCK_DONE`, blocked: `MOCK_BLOCKED` } | none (writes response file directly) |

Startup recipes (screen-scrape substring matches, run once per session before the first
prompt): codex update menu → send `2` + enter; opencode update modal → Escape ×2.

herdr integration classes to know about: some harnesses report lifecycle to herdr
directly (Pi, OpenCode, …), others only report session identity and herdr falls back to
screen detection (Claude, Codex, …). The driver treats both identically — poll
`pane get` — but transcript adapters consume `agent_session {kind, value}` when present.

## Adding a new harness (the checklist)

1. New file `src/profile/<name>.rs` implementing `Profile`.
2. Register in `mod.rs` registry (name → constructor).
3. Add transcript fixtures + parser tests if it has native transcripts.
4. Verify against the real harness manually: startup modal? fast turns? limit screens?
5. Document the row in the table above.

No engine changes. If a harness needs a genuinely new capability (e.g. a new completion
strategy), extend the enum in `mod.rs` — never special-case in the engine.

## Transcript formats are unstable private APIs

They are the *fallback*, not the contract (04). Version-pin expectations in fixtures;
smoke-test on harness upgrades; always keep the pane-scrape last resort.
