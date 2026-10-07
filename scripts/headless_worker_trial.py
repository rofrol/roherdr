#!/usr/bin/env python3
"""Trial: drive one headless Claude Code worker over stream-json.

Starts `claude -p --input-format stream-json --output-format stream-json
--verbose` as a plain subprocess (pipes, no PTY) in a throwaway git repo,
writes user messages and control messages to its stdin, reads events from
its stdout and journals every event to a JSONL file. Each case runs in a
fresh session and ends on an event: a `result` message, process exit or EOF,
never on a timer.

The control message shapes come from the Zod schemas bundled in the installed
Claude Code binary (`strings ~/.local/bin/claude`): `control_request` with
`request.subtype` `can_use_tool` / `interrupt`, answered by
`{"type":"control_response","response":{"subtype":"success","request_id":..,
"response":{"behavior":"allow"|"deny",..}}}`. Permission prompts reach stdout
only with `--permission-prompt-tool stdio`, which the Agent SDK also passes.

Usage: scripts/headless_worker_trial.py [--model sonnet] [--out DIR] [CASE...]
Cases: normal, no_host, host_permission, ask_user, interrupt, crash.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path

ALL_CASES = ["normal", "no_host", "host_permission", "ask_user", "interrupt", "crash"]


def make_repo(root: Path, name: str) -> Path:
    repo = root / name
    repo.mkdir(parents=True)
    run = lambda *a: subprocess.run(a, cwd=repo, check=True, capture_output=True)
    run("git", "init", "-q", "-b", "main")
    run("git", "config", "user.email", "trial@example.invalid")
    run("git", "config", "user.name", "Headless Trial")
    (repo / "notes.txt").write_text("first line\n")
    run("git", "add", "notes.txt")
    run("git", "commit", "-q", "-m", "initial")
    return repo


def child_env() -> dict[str, str]:
    # Without this the worker's hooks would report to (and rename) the herdr
    # pane that runs this script.
    return {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}


class Worker:
    """One `claude -p` stream-json session with a JSONL journal."""

    def __init__(self, repo: Path, journal: Path, model: str, extra: list[str]):
        self.argv = [
            "claude", "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--model", model,
            "--max-turns", "8",
            *extra,
        ]
        self.journal = journal.open("w")
        self.lock = threading.Lock()  # the gate watcher thread logs and sends too
        self.t0 = time.monotonic()
        self.log("meta", {"argv": self.argv, "cwd": str(repo)})
        self.proc = subprocess.Popen(
            self.argv, cwd=repo, env=child_env(),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1, start_new_session=True,
        )
        self.log("meta", {"pid": self.proc.pid})

    def log(self, direction: str, obj: object) -> None:
        rec = {"t": round(time.monotonic() - self.t0, 3), "dir": direction, "event": obj}
        with self.lock:
            self.journal.write(json.dumps(rec) + "\n")
            self.journal.flush()

    def send(self, obj: dict) -> None:
        self.log("in", obj)
        assert self.proc.stdin is not None
        with self.lock:
            self.proc.stdin.write(json.dumps(obj) + "\n")
            self.proc.stdin.flush()

    def send_user(self, text: str) -> None:
        self.send({"type": "user", "message": {"role": "user", "content": text}})

    def interrupt(self) -> str:
        request_id = f"req_{uuid.uuid4().hex[:12]}"
        self.send({"type": "control_request", "request_id": request_id,
                   "request": {"subtype": "interrupt"}})
        return request_id

    def answer(self, request_id: str, response: dict) -> None:
        self.send({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": response}})

    def events(self):
        """Yield parsed stdout events until EOF."""
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                ev = {"type": "_unparsed", "line": line}
            self.log("out", ev)
            yield ev
        self.log("meta", {"stdout": "EOF"})

    def finish(self) -> int:
        if self.proc.stdin and not self.proc.stdin.closed:
            try:
                self.proc.stdin.close()
            except BrokenPipeError:
                pass
        # Drain whatever is left; EOF comes when the process closes stdout.
        for _ in self.events():
            pass
        code = self.proc.wait()
        err = self.proc.stderr.read() if self.proc.stderr else ""
        self.log("meta", {"exit_code": code, "stderr": err[-4000:]})
        self.journal.close()
        return code


def summarize(ev: dict) -> str:
    t, st = ev.get("type"), ev.get("subtype")
    if t == "assistant":
        parts = []
        for c in ev.get("message", {}).get("content", []):
            if c.get("type") == "tool_use":
                parts.append(f"tool_use {c.get('name')}")
            elif c.get("type") == "text":
                parts.append("text")
        return f"assistant [{', '.join(parts)}]"
    if t == "user":
        kinds = []
        content = ev.get("message", {}).get("content")
        if isinstance(content, list):
            for c in content:
                if c.get("type") == "tool_result":
                    kinds.append("tool_result" + (" error" if c.get("is_error") else ""))
        else:
            kinds.append("text")
        return f"user [{', '.join(kinds)}]"
    if t == "control_request":
        return f"control_request {ev.get('request', {}).get('subtype')} {ev.get('request', {}).get('tool_name', '')}"
    return f"{t}/{st}" if st else str(t)


RESULT_FIELDS = ["subtype", "is_error", "terminal_reason", "api_error_status", "stop_reason",
                 "num_turns", "duration_ms", "total_cost_usd", "permission_denials",
                 "session_id", "result"]


def report(name: str, seen: list[str], result: dict | None, code: int, extra: dict) -> dict:
    out = {"case": name, "events": seen, "exit_code": code,
           "result": {k: result.get(k) for k in RESULT_FIELDS if result and k in result},
           "result_keys": sorted(result.keys()) if result else None, **extra}
    print(json.dumps(out, indent=2))
    return out


def run_turns(w: Worker, prompts: list[str], on_event=None) -> tuple[list[str], list[dict]]:
    """Send prompts one at a time, each after the previous turn's `result`."""
    seen, results = [], []
    queue = list(prompts)
    w.send_user(queue.pop(0))
    for ev in w.events():
        seen.append(summarize(ev))
        if on_event:
            on_event(w, ev)
        if ev.get("type") == "result":
            results.append(ev)
            if queue:
                w.send_user(queue.pop(0))
            else:
                break
    return seen, results


def case_normal(root, model, outdir):
    repo = make_repo(root, "normal")
    w = Worker(repo, outdir / "normal.jsonl", model,
               ["--permission-mode", "acceptEdits",
                "--allowedTools", "Bash(git add:*)", "Bash(git commit:*)"])
    # Sent before system/init on purpose: does a prompt written at once get lost?
    seen, results = run_turns(w, [
        "Append the line 'hello from the headless worker' to notes.txt, then "
        "commit it with `git add notes.txt` and `git commit -m 'add hello'`. "
        "Reply with one word: done."])
    code = w.finish()
    log = subprocess.run(["git", "log", "--oneline"], cwd=repo, capture_output=True, text=True).stdout
    init = next((json.loads(l)["event"] for l in (outdir / "normal.jsonl").open()
                 if '"subtype": "init"' in l), {})
    return report("normal", seen, results[-1] if results else None, code, {
        "git_log": log.splitlines(), "cwd": str(repo),
        "init_keys": sorted(init.keys()),
        "init": {k: init.get(k) for k in ("session_id", "model", "permissionMode",
                                          "cwd", "claude_code_version", "apiKeySource")}})


def case_no_host(root, model, outdir):
    repo = make_repo(root, "no_host")
    w = Worker(repo, outdir / "no_host.jsonl", model,
               ["--permission-mode", "manual", "--permission-prompts", "none"])
    seen, results = run_turns(w, [
        "Run exactly this shell command with the Bash tool: `touch created-by-bash.txt`. "
        "If it is refused, do not retry or use another tool; reply: refused."])
    code = w.finish()
    return report("no_host", seen, results[-1] if results else None, code,
                  {"file_exists": (repo / "created-by-bash.txt").exists()})


def case_host_permission(root, model, outdir):
    repo = make_repo(root, "host_permission")
    w = Worker(repo, outdir / "host_permission.jsonl", model,
               ["--permission-mode", "manual", "--permission-prompt-tool", "stdio"])
    asks = []

    def on_event(w, ev):
        if ev.get("type") == "control_request" and ev["request"].get("subtype") == "can_use_tool":
            req = ev["request"]
            # First ask denied, every later one allowed.
            if not asks:
                w.answer(ev["request_id"], {"behavior": "deny",
                                            "message": "Denied by the trial host."})
            else:
                w.answer(ev["request_id"], {"behavior": "allow", "updatedInput": req["input"]})
            asks.append({k: req.get(k) for k in ("tool_name", "input", "tool_use_id",
                                                 "decision_reason", "decision_reason_type",
                                                 "requires_user_interaction")}
                        | {"request_keys": sorted(req.keys())})

    seen, results = run_turns(w, [
        "Run exactly this shell command with the Bash tool: `touch first.txt`. "
        "If it is refused, do not retry or use another tool; reply: refused.",
        "Now run exactly this shell command with the Bash tool: `touch second.txt`. "
        "Reply: done."], on_event)
    code = w.finish()
    return report("host_permission", seen, results[-1] if results else None, code, {
        "asks": asks, "first_result": {k: results[0].get(k) for k in RESULT_FIELDS} if results else None,
        "first_exists": (repo / "first.txt").exists(),
        "second_exists": (repo / "second.txt").exists()})


def case_ask_user(root, model, outdir):
    repo = make_repo(root, "ask_user")
    w = Worker(repo, outdir / "ask_user.jsonl", model,
               ["--permission-mode", "acceptEdits", "--permission-prompt-tool", "stdio"])
    asks = []

    def on_event(w, ev):
        if ev.get("type") != "control_request":
            return
        req = ev["request"]
        asks.append({"subtype": req.get("subtype"), "tool_name": req.get("tool_name"),
                     "input": req.get("input"), "request_keys": sorted(req.keys())})
        if req.get("subtype") == "can_use_tool" and req.get("tool_name") == "AskUserQuestion":
            qs = req["input"].get("questions", [])
            answers = {q["question"]: q["options"][-1]["label"] for q in qs if q.get("options")}
            w.answer(ev["request_id"], {"behavior": "allow",
                                        "updatedInput": {**req["input"], "answers": answers}})
        elif req.get("subtype") == "can_use_tool":
            w.answer(ev["request_id"], {"behavior": "allow", "updatedInput": req["input"]})
        else:
            w.answer(ev["request_id"], {})

    seen, results = run_turns(w, [
        "Use the AskUserQuestion tool once to ask me which file name to create, with "
        "the options alpha.txt and beta.txt. Then create the file I chose (empty) "
        "with the Write tool and reply with its name."], on_event)
    code = w.finish()
    return report("ask_user", seen, results[-1] if results else None, code, {
        "control_requests": asks,
        "alpha_exists": (repo / "alpha.txt").exists(),
        "beta_exists": (repo / "beta.txt").exists()})


GATE_SCRIPT = """\
echo started > started.fifo
cat gate.fifo
"""


def _gated_case(name, root, model, outdir, act):
    """A turn whose Bash tool blocks on a FIFO until the host releases it.

    A plain `sleep N` is refused by Claude Code ("Blocked: standalone
    sleep"), so the tool runs `sh wait-gate.sh`: it writes to started.fifo
    (the host's blocking read returns: the tool is running) and then reads
    gate.fifo until the host opens it for writing. No timer decides anything.
    """
    repo = make_repo(root, name)
    (repo / "wait-gate.sh").write_text(GATE_SCRIPT)
    os.mkfifo(repo / "started.fifo")
    os.mkfifo(repo / "gate.fifo")
    w = Worker(repo, outdir / f"{name}.jsonl", model,
               ["--permission-mode", "manual", "--allowedTools", "Bash(sh wait-gate.sh)"])
    acted: dict = {}

    def watcher():
        with open(repo / "started.fifo") as f:
            started = f.read().strip()
        if started == "started" and w.proc.poll() is None:
            w.log("meta", {"tool": "running (started.fifo read)"})
            acted.update(act(w, repo))

    th = threading.Thread(target=watcher, daemon=True)
    th.start()
    seen, results = [], []
    w.send_user("Run exactly this shell command with the Bash tool, in the foreground "
                "(not in the background): `sh wait-gate.sh`. Then reply: finished.")
    for ev in w.events():
        seen.append(summarize(ev))
        if ev.get("type") == "result":
            results.append(ev)
            break
    # Release the watcher if the tool never ran.
    release_fifo(repo / "started.fifo")
    th.join()
    return w, repo, seen, results, acted


def release_fifo(path: Path) -> bool:
    """Open a FIFO for writing without blocking and close it at once, so a
    reader blocked on it gets EOF. False when nobody has it open (ENXIO)."""
    try:
        os.close(os.open(path, os.O_WRONLY | os.O_NONBLOCK))
        return True
    except OSError:
        return False


def gate_readers() -> list[str]:
    ps = subprocess.run(["pgrep", "-fl", "cat gate.fifo"], capture_output=True, text=True)
    return ps.stdout.splitlines()


def case_interrupt(root, model, outdir):
    def act(w, repo):
        return {"interrupt_request_id": w.interrupt()}
    w, repo, seen, results, acted = _gated_case("interrupt", root, model, outdir, act)
    acted["gate_reader_after_result"] = gate_readers()
    code = w.finish()
    acted["gate_reader_after_exit"] = gate_readers()
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    return report("interrupt", seen, results[-1] if results else None, code, acted)


def case_crash(root, model, outdir):
    def act(w, repo):
        before = gate_readers()
        # Kill only the claude process, not its process group, like a crash.
        w.proc.kill()
        return {"killed_pid": w.proc.pid, "gate_reader_before_kill": before}
    w, repo, seen, results, acted = _gated_case("crash", root, model, outdir, act)
    code = w.finish()
    acted["gate_reader_after_exit"] = gate_readers()
    # The orphaned tool (if any) is ours: let it finish by opening its gate.
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    return report("crash", seen, results[-1] if results else None, code, acted)


CASES = {"normal": case_normal, "no_host": case_no_host,
         "host_permission": case_host_permission, "ask_user": case_ask_user,
         "interrupt": case_interrupt, "crash": case_crash}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cases", nargs="*", default=ALL_CASES, help=", ".join(ALL_CASES))
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--out", type=Path, help="directory for journals and summary")
    args = ap.parse_args()
    unknown = set(args.cases) - set(ALL_CASES)
    if unknown:
        ap.error(f"unknown cases: {sorted(unknown)}")
    root = Path(tempfile.mkdtemp(prefix="headless-trial-", dir=os.environ.get("TMPDIR")))
    outdir = args.out or root / "journal"
    outdir.mkdir(parents=True, exist_ok=True)
    print(f"repos under {root}, journals in {outdir}", file=sys.stderr)
    summary = [CASES[name](root, args.model, outdir) for name in args.cases]
    (outdir / "summary.json").write_text(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
