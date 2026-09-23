# Headless: Codex

## The interface

```
codex app-server
```

JSON-RPC over standard input and output. The handshake is `initialize`, then `initialized`,
then ordinary method calls.

Some methods require the controlling program to declare `capabilities.experimentalApi: true`
during `initialize`. Without it they are rejected.

## Process shape

Verified: `codex app-server` is a plain subprocess, not a daemon. Closing its standard input
ends it, and it takes its whole process tree with it. There is no port and nothing to clean
up.

A separate daemon does exist, behind explicit subcommands. A supervisor that does not call
those subcommands never creates one.

## State

Codex reports turn state as typed events, including a variant for waiting on approval and a
variant for waiting on user input. That is the structured form of `blocked`, which the
interactive mode can only infer from the screen.

## Model catalogue

`model/list` returns the models and, for each, its supported effort values with a description
string. `agents/codex.md` records why orcr reads the same data from `codex debug models`
instead.

## Transcript

Codex writes a rollout file at
`~/.codex/sessions/<year>/<month>/<day>/rollout-<timestamp>-<uuid>.jsonl`.

## Not verified

- Whether `model/list` pagination matters. It returns a cursor, and today one page holds
  every model.
- Whether the rollout file layout is identical between headless and interactive runs.
