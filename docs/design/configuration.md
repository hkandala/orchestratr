# Configuration

## File

```
~/.config/orcr/config.toml
```

The file is optional. orcr works with no configuration at all.

## Contents

```toml
[orcr]
socket = "~/.local/state/orcr/orcr.sock"

[kind.claude]
binary = "claude"
home   = "~/.claude"
model  = "sonnet"
effort = "high"
args   = []

[kind.codex]
binary = "codex"
home   = "~/.codex"
args   = ["--search"]

[kind.pi]
binary = "pi"
home   = "~/.pi"

[kind.opencode]
binary = "opencode"
home   = "~/.config/opencode"
```

| Key | Default | Purpose |
|---|---|---|
| `orcr.socket` | see below | Where the server listens. |
| `kind.<k>.binary` | the kind's own name | Which program to run. Use this to test a build. |
| `kind.<k>.home` | the kind's own default | The configuration directory orcr reads and writes for that kind. |
| `kind.<k>.model` | the kind's own default | Used when `--model` is not given. |
| `kind.<k>.effort` | the kind's own default | Used when `--effort` is not given. |
| `kind.<k>.args` | empty | Extra arguments, appended after the arguments orcr builds. |

The default socket path is `$XDG_RUNTIME_DIR/orcr/orcr.sock`, and
`~/.local/state/orcr/orcr.sock` when `XDG_RUNTIME_DIR` is not set.

## Precedence

A command line flag beats the configuration file. The configuration file beats the kind's own
default.

`args` is the exception. It is always appended. It does not replace what orcr builds, because
orcr must control the permission bypass argument and the startup arguments.

## `binary`

This key exists because the name of a program does not always identify the program.

`orcr doctor` reports the resolved path and the reported version of each kind's binary.

## `home`

orcr reads each kind's configuration directory. It writes to only one of them: three keys in
Claude's, for the folder trust record, the theme picker and the security notes screen. Codex,
Pi and OpenCode carry their startup settings as launch arguments or inline configuration, so
orcr writes nothing for them. See `docs/design/agents/`.

Point `home` at a directory under a temporary path to run agents with a configuration
separate from the person's own. If the directory is fresh, some kinds show every startup
screen. orcr must then suppress each screen key by key.

## What is deliberately not configurable

| Not configurable | Why |
|---|---|
| Permission bypass | orcr bypasses permissions on every agent it starts. A setting would create a mode that nothing else in orcr accounts for. |
| The backend | herdr is required. |
| The backend session name | It is always `orcr`. A name that varies makes reconciliation ambiguous for no gain. |
| Delivery timeouts | They are derived from measurements per kind and recorded in `docs/design/agents/`. A value someone guesses is worse than a value that was measured. |
| Polling intervals | Same reason. |
| The readiness signal per kind | It is a property of the agent, not a preference. |
| State names | A shared vocabulary that can be renamed is not shared. |

## Environment

orcr reads no environment variable for its own settings.

orcr removes known child session markers from the environment it passes to the herdr server.
One of them silently disables transcript writing for a kind, and orcr confirms message
delivery by reading transcripts.

orcr preserves every variable the herdr integrations require. Removing them stops the
integrations reporting, which stops session binding.
