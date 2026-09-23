# Purpose

## What orcr is

orcr runs and controls AI coding agents. It supports four kinds:

- Claude Code
- Codex
- Pi
- OpenCode

Each agent runs as an interactive terminal program inside a pane that
[herdr](https://herdr.dev) manages. orcr gives every agent an identity, tracks its state,
and offers one set of commands that works the same way for all four kinds.

The socket API is the product. The command line tool is one caller of that API. Any program
that can open a UNIX socket and write a line of JSON can do everything the command line tool
can do.

## The problem

A coding agent runs alone. One terminal, one agent, one person watching it.

Real work needs more than one agent.

- a reviewer per concern
- a worker under a verifier
- a nightly job that triages issues

That work often needs agents of different kinds, because each kind has different strengths
and its own subscription quota.

## Why interactive sessions

Each kind can also run without a terminal. orcr does not use that mode. Interactive
sessions keep three properties that matter at this scale.

1. Price. An interactive session bills against a flat subscription plan. A non-interactive
   call bills for each token. Across many agents this difference sets the total cost.
2. Control. A person can watch an interactive session while it works, correct it mid turn,
   and take the terminal over when it goes wrong.
3. One interface. Each kind has a different non-interactive mode, with different flags and
   different output. The interactive terminal is the one surface all four kinds share.

## What orcr provides

- Identity. Every agent gets a stable name and id that survive a restart.
- Placement. Agents that share a working directory share a workspace.
- Lifecycle. Start an agent, send it a message, interrupt it, attach to it, stop it.
- State. One state vocabulary for all four kinds.
- Transcripts. Read the conversation of any agent in one format.
- Catalogue. List the models and effort values each kind accepts.

## Scope

orcr controls the agents it starts. It does not adopt agents that another tool started.

orcr starts every agent with permission prompts turned off. An agent that orcr starts can
run any command in its working directory without asking. This is deliberate. orcr offers no
command to approve or reject a single action.

herdr is the terminal backend. orcr requires herdr, and requires the herdr integration for
each kind it drives.

## Design rules

These rules decide arguments. Read them before proposing a change.

1. One behavior for all kinds. Where the agents differ, orcr hides the difference or states
   it. orcr does not grow a separate code path for each kind.
2. Per-kind knowledge is data, not code. Launch arguments and startup steps live in a table.
3. Do not reimplement herdr. If herdr answers a question, orcr asks herdr.
4. Fail closed. When orcr cannot confirm that an action is safe, it refuses and says why.
5. The socket API comes first. A command line feature that the API cannot express is a bug
   in the API.
6. Say what is not known. An honest gap is more useful than a confident guess.
