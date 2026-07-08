# orchestratr (Python SDK)

Thin typed wrapper around the [`orcr`](https://github.com/hkandala/orchestratr) CLI —
agent orchestration over herdr. Every call shells `orcr … --json` via subprocess and
parses the JSON envelope; the CLI is the contract and the SDK never gains private
capabilities. Standard library only. Python >= 3.9.

```sh
pip install orchestratr
```

Import as `orcr`. The `orcr` binary must be on `PATH` (or set `ORCR_BIN`).

## Supervised fix loop with steering

```python
from orcr import run, send, wait, out

worker = run(harness="claude", prompt="make the test suite pass", name="fixer", keep=True)
for _ in range(5):
    wait(worker)
    verdict = run(harness="claude", wait=True,
        prompt=f"Run the tests. First line PASS or FAIL.\n{worker.out()}")
    if verdict.text.startswith("PASS"):
        break
    send(worker, f"Reviewer feedback, keep going:\n{verdict.text}", turn=True)
```

## Fan-out review & merge

```python
from orcr import run, wait

task = "refactor auth middleware to support API keys"
runs = [
    run(harness="claude", prompt=task, worktree=True, name="claude-try"),
    run(harness="codex", prompt=task, worktree=True, name="codex-try"),
    run(harness="opencode", prompt=task, worktree=True, name="oc-try"),
]
wait(runs, timeout_s=1800)
summaries = [r.out() for r in runs]
judge = run(harness="codex", wait=True,
    prompt="Pick the best of these three attempts and merge improvements:\n"
    + "\n---\n".join(summaries))
print(judge.text)
```

## Surface

```python
run(harness, prompt=…, prompt_file=…, name=…, model=…, effort=…, cwd=…,
    timeout_s=…, keep=…, mode=…, worktree=…, parent=…, session=…, wait=…) -> Handle
send(id, prompt, steer=…, turn=…, prompt_file=…, wait=…)
wait(ids, any_=…, tree=…, timeout_s=…)          # fan-in; ids may be Handles
out(id, turn=…, recursive=…, paths=…)           # -> list of items with response-file paths
show(id) / kill(id, tree=…) / ps() / tree() / history(since=…, harness=…, …)
events(on_event=None)                           # generator (or callback loop) over NDJSON events
Handle: id, text, wait(), out(), send(), kill()  # convenience object from run()
```

Exceptions are typed from the CLI exit-code table: `EnvConfigErr` (2), `TimeoutErr` (3),
`BlockedErr` (4), `KilledErr` (5), `NotFoundErr` (6), `StateConflictErr` (7), with the
raw envelope attached as `.details`.

## Development

```sh
python -m unittest discover -s tests   # skips cleanly when orcr is absent
```
