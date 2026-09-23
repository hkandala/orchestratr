# Delivering a message without typing

orcr pastes text into the composer and presses Enter, then confirms delivery by reading the
delivery record. Two of the four kinds can instead be handed a message directly. That call
replaces the keystrokes and the inference, and it returns the outcome.

## What is possible today

| Kind | Mechanism | Needs orcr code inside the agent |
|---|---|---|
| OpenCode | `POST /session/{id}/message` on the port orcr already pins at launch | No |
| Pi | An extension calling the agent's own send function, loaded with `-e` | Yes |
| Codex | `codex queue`, only when a daemon holds the thread | No, but conditional |
| Claude | None usable | — |

### OpenCode

The cleanest case. orcr already pins an HTTP port at launch for readiness and for the blocked
check, so the send becomes one more request on a connection that exists.

The session identifier must be resolved from the agent's own session-selected event, never
guessed.

That request also accepts a `variant`, which is the only way to change effort part way through
a session without a person pressing keys. Verified: it took effect in an environment where the
configuration route had deliberately been left inert, so the effect came only from the
request.

Do not use `/tui/append-prompt` and `/tui/submit-prompt` for delivery. They return success
unconditionally and do nothing when the composer is not focused, so they offer the ergonomics
of an interface with all the uncertainty of a keystroke. They are usable for the readiness
round trip only because that check waits for the text to come back out again.

### Pi

Pi exposes a send function to extensions, including a choice of delivering as a steer or as a
follow-up. That send function can fix Pi's worst property: today a message sent mid turn is
not confirmed until the running tool call finishes, which is unbounded.

It requires shipping an extension, which orcr does not do. See `permissions.md` for the
general shape of that decision. If orcr ever ships one, this is the strongest reason.

### Codex

`codex queue` reaches a thread that a running app server daemon has loaded. orcr does not run
that daemon and does not speak that protocol, so this is only an opportunistic fast path, taken
when the daemon happens to exist and to hold the thread. It is not a foundation to build on.

### Claude

There is a cross-session inbox over a local socket that does reach the running interactive
session. It is key authenticated, undocumented, versioned, and has no command line client.
Revisit only if a supported command appears.

## Why the first version types instead

Typing works on all four kinds and is one code path. Every kind already confirms delivery from
its own delivery record, three of them in under half a second, so the uncertainty an interface
can remove is already small.

Adopting interface delivery for two kinds means two send implementations behind one interface,
for a gain that matters most on the kind where it is hardest to get.

## What it would buy

- No composer clearing, no bracketed paste, no Enter key, and no Enter retry.
- No risk of typed text being consumed by a dialog and answering it.
- `possibly_delivered` becomes rare or impossible for those kinds, because the call returns a
  result rather than leaving orcr to infer one.
- For Pi specifically, the unbounded mid turn confirmation wait disappears.

## What it would cost

- Two send paths to maintain and test instead of one.
- A second control plane per kind, versioned separately from the terminal.
- A new failure mode: a message that reaches a different session from the one shown in the
  pane. Any implementation must prove it targets the session the person can see, and must fail
  rather than create a parallel session.

## The evidence

A project solving the same problem across 13 agents delivers every interactive message as a
paste plus a separate Enter. It never attempted interface delivery.

That is not proof it is wrong. It is a reason to treat interface delivery as an improvement to
prove, rather than the obvious thing everyone does.
