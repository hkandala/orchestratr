# Headless: Claude

## The interface

Claude Code exposes a control protocol on standard input and output.

```
claude -p --input-format stream-json --output-format stream-json --verbose
```

The framing is newline delimited JSON. The controlling program writes `control_request`
objects and reads `control_response` objects, interleaved with the message stream.

```json
{"type":"control_request","request_id":"r1","request":{"subtype":"initialize"}}
```

The published Agent SDK starts this exact command. Using the protocol directly and using the
SDK are the same thing at the wire level.

## What initialize returns

The `initialize` response carries the model catalogue, and two fields that matter for state:

- `pending_permission_requests`
- `pending_user_dialog_requests`

Those two fields report whether the agent waits for a person. A headless Claude reports that
state as data.

Verified: an `initialize` request emits one `control_response` and exits, with no assistant
frames and no result frames. It starts no turn and costs no tokens.

## Sending a message

Write a user message object to standard input. There is no composer, no Enter key, and no
question about whether the text landed. The protocol answers that.

## State

Turn state comes from the message stream rather than from a screen. There is no fallback
case and no rule engine.

## Transcript

Claude writes the same transcript file in both modes, at
`$CLAUDE_CONFIG_DIR/projects/<resolved cwd, every / replaced by ->/<session-uuid>.jsonl`.

One warning carries over from interactive mode. Any supervisor must scrub `CLAUDE_CODE_*`
from the environment it passes down, for the reason `agents/claude.md` gives.

## Not verified

- Whether `--bare` skips session start hooks, which dominate the startup time of the
  initialize probe.
- Whether the interactive transcript and the headless transcript are byte identical for the
  same conversation.
