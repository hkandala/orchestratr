# orcr design

orcr runs and controls AI coding agents of four kinds through one interface.

Read in this order.

| File | Subject |
|---|---|
| `purpose.md` | What orcr is, why it works this way, and the rules that decide arguments |
| `cli-reference.md` | Every command, and the socket API with its schema |
| `implementation.md` | Layers, the kind adapter, and the shared procedures |
| `agents/claude.md` | Claude Code |
| `agents/codex.md` | Codex |
| `agents/pi.md` | Pi |
| `agents/opencode.md` | OpenCode |
| `configuration.md` | `config.toml`, and what is deliberately fixed |
| `limitations.md` | What does not work, with the evidence |

`cli-reference.md` is the contract. The agent files hold everything specific to one kind.
Nothing in them changes the contract.

Work that orcr does not do is in `future/`.

`html/` holds the same documents rendered for reading and commenting, and the script that
makes them. Open `html/index.html` for all of them in one page, or any other page in that
folder for a single document. Build them again after an edit, as `html/README.md` describes.

## Measurements

Every latency, path and key in these documents came from a live run on macOS against Claude
Code 2.1.276, Codex 0.154.0, Pi 0.85.1, and OpenCode 1.18.31 built from source. The run drove
all four through herdr 0.9.1 at protocol 22.

Where something was not tested, the document says so in a section named "Not verified". Those
sections are part of the specification. Do not implement around them silently.
