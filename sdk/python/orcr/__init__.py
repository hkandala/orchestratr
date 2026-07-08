"""orchestratr — thin typed wrapper around the `orcr` CLI.

The CLI is the contract: every call here shells ``orcr … --json`` via subprocess and
parses the single ``{"ok": …}`` envelope on stdout. The SDK never gains private
capabilities. Zero dependencies beyond the standard library.
"""

from __future__ import annotations

import json
import os
import subprocess
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Iterator, List, Optional, Sequence, Union

__all__ = [
    "OrcrError",
    "EnvConfigErr",
    "TimeoutErr",
    "BlockedErr",
    "KilledErr",
    "NotFoundErr",
    "StateConflictErr",
    "Handle",
    "run",
    "send",
    "wait",
    "out",
    "show",
    "kill",
    "ps",
    "tree",
    "history",
    "events",
]


# ------------------------------------------------------------------------------------
# Errors — mapped from the CLI exit-code table (spec/03).
# ------------------------------------------------------------------------------------


class OrcrError(Exception):
    """Base error; carries the CLI exit code, error code, and envelope details."""

    def __init__(
        self,
        message: str,
        exit_code: int = 1,
        code: str = "error",
        details: Any = None,
    ) -> None:
        super().__init__(message)
        self.exit_code = exit_code
        self.code = code
        self.details = details


class EnvConfigErr(OrcrError):
    """exit 2 — environment/config problem (herdr missing, bad config.toml)."""


class TimeoutErr(OrcrError):
    """exit 3 — a wait or ``run(wait=True)`` timed out; details has the partial result."""


class BlockedErr(OrcrError):
    """exit 4 — an agent is blocked and needs a human; details has the result."""


class KilledErr(OrcrError):
    """exit 5 — the agent was killed."""


class NotFoundErr(OrcrError):
    """exit 6 — id or name not found."""


class StateConflictErr(OrcrError):
    """exit 7 — lifecycle-invalid operation; details has current_status/wanted/id."""


_ERROR_BY_EXIT = {
    2: EnvConfigErr,
    3: TimeoutErr,
    4: BlockedErr,
    5: KilledErr,
    6: NotFoundErr,
    7: StateConflictErr,
}


def _error_for(exit_code: int, message: str, code: str, details: Any = None) -> OrcrError:
    cls = _ERROR_BY_EXIT.get(exit_code, OrcrError)
    return cls(message, exit_code=exit_code, code=code, details=details)


# ------------------------------------------------------------------------------------
# CLI plumbing.
# ------------------------------------------------------------------------------------


def _orcr_bin() -> str:
    return os.environ.get("ORCR_BIN", "orcr")


def _run_cli(args: Sequence[str]) -> Any:
    """Runs ``orcr <args> --json`` and returns the envelope's ``result``."""
    argv = [_orcr_bin(), *args, "--json"]
    try:
        proc = subprocess.run(argv, capture_output=True, text=True, check=False)
    except FileNotFoundError:
        raise EnvConfigErr(
            f"orcr binary not found: {argv[0]}", exit_code=2, code="env_config"
        ) from None
    try:
        envelope = json.loads(proc.stdout)
    except (json.JSONDecodeError, ValueError):
        raise OrcrError(
            f"orcr did not print a JSON envelope (exit {proc.returncode}): "
            f"{proc.stdout[:200]!r}",
            exit_code=proc.returncode or 1,
            code="bad_output",
        ) from None
    if envelope.get("ok") is True:
        if proc.returncode == 0:
            return envelope.get("result")
        # e.g. `wait` exits 3/4 while still printing a full ok envelope.
        raise _error_for(
            proc.returncode,
            f"orcr exited {proc.returncode}",
            "status",
            envelope.get("result"),
        )
    error = envelope.get("error") or {}
    raise _error_for(
        proc.returncode or 1,
        error.get("message", f"orcr exited {proc.returncode}"),
        error.get("code", "error"),
        error.get("details"),
    )


IdRef = Union[str, "Handle"]


def _id_of(ref: IdRef) -> str:
    return ref.id if isinstance(ref, Handle) else ref


# ------------------------------------------------------------------------------------
# Handle — convenience object returned by run().
# ------------------------------------------------------------------------------------


@dataclass
class Handle:
    """A spawned agent. ``text`` is the first-turn response when run(wait=True)."""

    id: str
    text: Optional[str] = None
    result: Dict[str, Any] = field(default_factory=dict)

    def wait(self, **kwargs: Any) -> Dict[str, Any]:
        return wait(self, **kwargs)

    def out(self) -> str:
        """Latest response body (empty string when no response file exists yet)."""
        items = out(self)
        if not items:
            return ""
        return items[-1].get("text") or ""

    def send(self, prompt: str, **kwargs: Any) -> Dict[str, Any]:
        return send(self, prompt, **kwargs)

    def kill(self, **kwargs: Any) -> Dict[str, Any]:
        return kill(self, **kwargs)


# ------------------------------------------------------------------------------------
# The surface — mirrors the CLI verbs.
# ------------------------------------------------------------------------------------


def run(
    harness: str,
    prompt: Optional[str] = None,
    prompt_file: Optional[str] = None,
    name: Optional[str] = None,
    model: Optional[str] = None,
    effort: Optional[str] = None,
    cwd: Optional[str] = None,
    timeout_s: Optional[int] = None,
    keep: bool = False,
    mode: Optional[str] = None,
    worktree: bool = False,
    parent: Optional[str] = None,
    session: Optional[str] = None,
    wait: bool = False,
) -> Handle:
    args: List[str] = ["run", "--harness", harness]
    if prompt is not None:
        args += ["-p", prompt]
    if prompt_file is not None:
        args += ["--prompt-file", prompt_file]
    if name is not None:
        args += ["--name", name]
    if model is not None:
        args += ["--model", model]
    if effort is not None:
        args += ["--effort", effort]
    if cwd is not None:
        args += ["--cwd", cwd]
    if timeout_s is not None:
        args += ["--timeout", f"{timeout_s}s"]
    if keep:
        args.append("--keep")
    if mode is not None:
        args += ["--mode", mode]
    if worktree:
        args.append("--worktree")
    if parent is not None:
        args += ["--parent", parent]
    if session is not None:
        args += ["--session", session]
    if wait:
        args.append("--wait")
    result = _run_cli(args)
    response = result.get("response") or {}
    return Handle(id=result["agent"]["id"], text=response.get("text"), result=result)


def send(
    id: IdRef,
    prompt: Optional[str] = None,
    prompt_file: Optional[str] = None,
    steer: bool = False,
    turn: bool = False,
    wait: bool = False,
) -> Dict[str, Any]:
    args: List[str] = ["send", _id_of(id)]
    if prompt_file is not None:
        args += ["--prompt-file", prompt_file]
    elif prompt is not None:
        args.append(prompt)
    if steer:
        args.append("--steer")
    if turn:
        args.append("--turn")
    if wait:
        args.append("--wait")
    return _run_cli(args)


def wait(
    ids: Union[IdRef, Sequence[IdRef]],
    any_: bool = False,
    tree: Optional[str] = None,
    timeout_s: Optional[int] = None,
) -> Dict[str, Any]:
    refs = [ids] if isinstance(ids, (str, Handle)) else list(ids)
    args: List[str] = ["wait", *[_id_of(ref) for ref in refs]]
    if any_:
        args.append("--any")
    if tree is not None:
        args += ["--tree", tree]
    if timeout_s is not None:
        args += ["--timeout", f"{timeout_s}s"]
    return _run_cli(args)


def out(
    id: IdRef,
    turn: Optional[int] = None,
    recursive: bool = False,
    paths: bool = False,
) -> List[Dict[str, Any]]:
    args: List[str] = ["out", _id_of(id)]
    if turn is not None:
        args += ["--turn", str(turn)]
    if recursive:
        args.append("--recursive")
    if paths:
        args += ["--format", "path"]
    return _run_cli(args)["items"]


def show(id: IdRef) -> Dict[str, Any]:
    return _run_cli(["show", _id_of(id)])


def kill(id: Union[IdRef, Sequence[IdRef]], tree: bool = False) -> Dict[str, Any]:
    refs = [id] if isinstance(id, (str, Handle)) else list(id)
    args: List[str] = ["kill", *[_id_of(ref) for ref in refs]]
    if tree:
        args.append("--tree")
    return _run_cli(args)


def ps() -> List[Dict[str, Any]]:
    return _run_cli(["ps"])["agents"]


def tree(id: Optional[IdRef] = None) -> List[Dict[str, Any]]:
    args: List[str] = ["tree"]
    if id is not None:
        args.append(_id_of(id))
    return _run_cli(args)["roots"]


def history(
    since: Optional[str] = None,
    status: Optional[str] = None,
    parent: Optional[str] = None,
    name: Optional[str] = None,
    harness: Optional[str] = None,
    limit: Optional[int] = None,
) -> List[Dict[str, Any]]:
    args: List[str] = ["history"]
    if since is not None:
        args += ["--since", since]
    if status is not None:
        args += ["--status", status]
    if parent is not None:
        args += ["--parent", parent]
    if name is not None:
        args += ["--name", name]
    if harness is not None:
        args += ["--harness", harness]
    if limit is not None:
        args += ["--limit", str(limit)]
    return _run_cli(args)["items"]


def events(on_event: Optional[Callable[[Dict[str, Any]], None]] = None) -> Iterator[Dict[str, Any]]:
    """Streams ``orcr events --follow --json`` NDJSON.

    With ``on_event``, blocks and invokes the callback per event. Without it, returns
    a generator yielding event dicts (close it to stop the child process).
    """
    if on_event is not None:
        for event in events():
            on_event(event)
        return iter(())
    return _event_stream()


def _event_stream() -> Iterator[Dict[str, Any]]:
    try:
        proc = subprocess.Popen(
            [_orcr_bin(), "events", "--follow", "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except FileNotFoundError:
        raise EnvConfigErr(
            f"orcr binary not found: {_orcr_bin()}", exit_code=2, code="env_config"
        ) from None
    try:
        assert proc.stdout is not None
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
    finally:
        proc.terminate()
