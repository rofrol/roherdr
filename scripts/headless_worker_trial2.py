#!/usr/bin/env python3
"""Trial 2: failure cases for a headless Claude Code worker over stream-json.

Builds on `headless_worker_trial.py` (its `Worker` journals every stdin and
stdout line). Each case ends on an event: a `result` message, EOF, process
exit (waited for with kqueue `NOTE_EXIT`), or a FIFO read; never a timer.

Cases:
  config     which flags keep the global CLAUDE.md and the login but turn the
             global hooks off (`hook_started` events, `apiKeySource`)
  policy     the user's choices: tools allowed inside the worktree (realpath
             check), a write outside it denied, AskUserQuestion answered by
             the host
  interrupt_child  interrupt a Bash tool that has a child process, then end
             the worker's process group: any orphans?
  crash_group      SIGKILL the CLI mid-tool, then kill its process group,
             its session, and the tool sessions recorded before the crash
  sigterm    SIGTERM to the CLI mid-tool without an interrupt
  storm      20 `can_use_tool` requests in one turn, all answered, in order
  two_writers      `claude -p --resume <id>` while the first writer is alive
  parallel   three workers at once in three repos: no cross-talk
  takeover   interrupt, `aborted_tools`, end the group, exit; prints the
             session id for an interactive `claude --resume` in a herdr tab

Usage: scripts/headless_worker_trial2.py [--model sonnet] [--out DIR] [CASE...]
"""

from __future__ import annotations

import argparse
import json
import os
import select
import signal
import subprocess
import sys
import tempfile
import threading
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from headless_worker_trial import (  # noqa: E402
    RESULT_FIELDS, Worker, make_repo, release_fifo, report, run_turns, summarize)

# Chosen by the `config` case: keeps CLAUDE.md, user settings and the OAuth
# login, and turns every hook off (see the Trial 2 report).
HOOKS_OFF = ["--settings", json.dumps({"disableAllHooks": True})]

ALL_CASES = ["config", "policy", "interrupt_child", "crash_group", "sigterm", "storm",
             "two_writers", "parallel", "takeover"]


# --- process helpers -------------------------------------------------------

def ps_matching(token: str) -> list[dict]:
    """Processes whose command line contains `token`, and their children."""
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,pgid=,command="],
                         capture_output=True, text=True).stdout
    procs = []
    for line in out.splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4:
            procs.append({"pid": int(parts[0]), "ppid": int(parts[1]),
                          "pgid": int(parts[2]), "command": parts[3][:160]})
    hits = {p["pid"] for p in procs if token in p["command"]}
    rows = [p for p in procs if p["pid"] in hits or p["ppid"] in hits]
    for p in rows:
        p["sid"] = getsid(p["pid"])
    return rows


def getsid(pid: int) -> int | None:
    try:
        return os.getsid(pid)
    except OSError:
        return None


def descendant_sessions(root_pid: int) -> list[int]:
    """Session ids of all descendants of `root_pid`, read while it is alive.
    Claude Code starts each Bash tool in a new session (setsid), so after a
    crash these sessions are no longer linked to the dead CLI."""
    out = subprocess.run(["ps", "-axo", "pid=,ppid="], capture_output=True, text=True).stdout
    kids: dict[int, list[int]] = {}
    for line in out.splitlines():
        pid, ppid = map(int, line.split())
        kids.setdefault(ppid, []).append(pid)
    found, stack = [], list(kids.get(root_pid, []))
    while stack:
        pid = stack.pop()
        found.append(pid)
        stack.extend(kids.get(pid, []))
    return sorted({sid for sid in map(getsid, found) if sid is not None and sid != root_pid})


def kill_session(sid: int) -> dict:
    """Kill every process group in session `sid` (the CLI leads its own
    session: start_new_session=True), then wait for all of them to exit.
    `ps` on macOS shows no session column, so ask getsid() per pid."""
    out = subprocess.run(["ps", "-axo", "pid=,pgid="], capture_output=True, text=True).stdout
    members = []
    for line in out.splitlines():
        pid, pgid = map(int, line.split())
        if getsid(pid) == sid:
            members.append((pid, pgid))
    for pgid in {g for _, g in members}:
        try:
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    wait_exit([pid for pid, _ in members])
    return {"session": sid, "members_killed": members}


def wait_exit(pids: list[int]) -> None:
    """Block until every pid has exited (kqueue NOTE_EXIT, macOS/BSD)."""
    kq = select.kqueue()
    pending = set()
    for pid in pids:
        ev = select.kevent(pid, filter=select.KQ_FILTER_PROC,
                           flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                           fflags=select.KQ_NOTE_EXIT)
        try:
            kq.control([ev], 0)
            pending.add(pid)
        except ProcessLookupError:
            pass  # already gone
    while pending:
        for ev in kq.control(None, len(pending)):
            pending.discard(ev.ident)
    kq.close()


def end_group(w: Worker, sig: int) -> dict:
    """Signal the worker's whole process group and wait for the CLI to exit."""
    pgid = w.proc.pid  # start_new_session=True: the CLI leads its own group
    try:
        os.killpg(pgid, sig)
        sent = True
    except ProcessLookupError:
        sent = False
    if not w.journal.closed:
        w.log("meta", {"killpg": pgid, "signal": sig, "sent": sent})
    return {"killpg": pgid, "signal": signal.Signals(sig).name, "sent": sent}


def orphan_check(token: str, group: int, tool_procs: list[dict]) -> dict:
    """After the group is gone: members of the group must exit (waited for);
    the tool processes outside the group are orphans if still alive."""
    in_group = [p["pid"] for p in tool_procs if p["pgid"] == group]
    wait_exit(in_group)
    survivors = ps_matching(token)
    return {"tool_procs_in_worker_group": in_group,
            "tool_procs_outside_group": [p for p in tool_procs if p["pgid"] != group],
            "survivors_after_group_end": survivors}


# --- gated tool with a child process ---------------------------------------

def gate_script(token: str) -> str:
    # The child carries `token` in its argv, so `ps` finds exactly this case's
    # processes, and it announces itself, so "started" means the child runs.
    # `wait` keeps the tool's shell alive with a child below it.
    return (
        f"sh -c 'echo started > started.fifo; cat gate.fifo; :' {token} &\n"
        "wait\n"
    )


def gated_worker(name, root, model, outdir, extra, act):
    """A turn whose Bash tool runs `sh wait-gate.sh`; `act(w, repo)` runs on
    the event "the tool is running" (the host's read of started.fifo returns).
    Returns after the first `result` or EOF."""
    token = f"trialtok-{uuid.uuid4().hex[:10]}"
    repo = make_repo(root, name)
    (repo / "wait-gate.sh").write_text(gate_script(token))
    os.mkfifo(repo / "started.fifo")
    os.mkfifo(repo / "gate.fifo")
    w = Worker(repo, outdir / f"{name}.jsonl", model,
               ["--permission-mode", "manual",
                "--allowedTools", "Bash(sh wait-gate.sh)", *extra])
    acted: dict = {"token": token}

    def watcher():
        with open(repo / "started.fifo") as f:
            started = f.read().strip()
        if started == "started" and w.proc.poll() is None:
            procs = ps_matching(token)
            w.log("meta", {"tool": "running", "procs": procs})
            acted["tool_procs"] = procs
            acted["worker_pgid"] = os.getpgid(w.proc.pid)
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
    release_fifo(repo / "started.fifo")
    th.join()
    return w, repo, seen, results, acted


# --- cases -----------------------------------------------------------------

CONFIG_VARIANTS = {
    "baseline": [],
    "settings_hooks_empty": ["--settings", json.dumps({"hooks": {}})],
    "settings_disableAllHooks": ["--settings", json.dumps({"disableAllHooks": True})],
    "setting_sources_project_local": ["--setting-sources", "project,local"],
    "bare": ["--bare"],
}


def case_config(root, model, outdir):
    out = {}
    for variant, flags in CONFIG_VARIANTS.items():
        repo = make_repo(root, f"config-{variant}")
        w = Worker(repo, outdir / f"config-{variant}.jsonl", model,
                   ["--permission-mode", "manual", *flags], max_turns=1)
        hooks, init = [], {}

        def on_event(w, ev):
            nonlocal init
            if ev.get("type") == "system" and ev.get("subtype") == "hook_started":
                hooks.append({k: ev.get(k) for k in ("hook_name", "hook_event")})
            if ev.get("type") == "system" and ev.get("subtype") == "init" and not init:
                init = ev

        seen, results = run_turns(w, [
            "What language do your instructions say to answer in, and which "
            "file under ~/.claude holds the user's global instructions, if "
            "any? Answer in one short line, without using tools."], on_event)
        code = w.finish()
        r = results[-1] if results else {}
        out[variant] = {
            "flags": flags, "exit_code": code, "hooks_started": hooks,
            "apiKeySource": init.get("apiKeySource"),
            "memory_paths": init.get("memory_paths"),
            "result": {k: r.get(k) for k in ("subtype", "is_error", "result",
                                             "api_error_status")},
        }
    print(json.dumps({"case": "config", **out}, indent=2))
    return {"case": "config", **out}


def inside(repo: Path, path: str) -> bool:
    p = Path(path)
    if not p.is_absolute():
        p = repo / p
    real_repo = os.path.realpath(repo)
    real = os.path.realpath(p)  # resolves symlinks in existing parents
    return real == real_repo or real.startswith(real_repo + os.sep)


def policy_answer(repo: Path, req: dict) -> dict:
    """The user's choice: allowed inside the worktree, everything else denied
    (a real supervisor escalates instead of denying)."""
    tool, inp = req.get("tool_name"), req.get("input", {})
    if tool == "AskUserQuestion":
        qs = inp.get("questions", [])
        answers = {q["question"]: q["options"][-1]["label"] for q in qs if q.get("options")}
        return {"behavior": "allow", "updatedInput": {**inp, "answers": answers}}
    path = inp.get("file_path") or inp.get("notebook_path")
    if path is not None:
        if inside(repo, path):
            return {"behavior": "allow", "updatedInput": inp}
        return {"behavior": "deny", "message": f"{path} is outside the worktree."}
    return {"behavior": "deny", "message": f"{tool} needs the user's approval."}


def case_policy(root, model, outdir):
    repo = make_repo(root, "policy")
    outside_dir = root / "policy-outside"
    outside_dir.mkdir()
    (repo / "link-out").symlink_to(outside_dir)
    w = Worker(repo, outdir / "policy.jsonl", model,
               ["--permission-mode", "manual", "--permission-prompt-tool", "stdio",
                *HOOKS_OFF], max_turns=12)
    asks = []

    def on_event(w, ev):
        if ev.get("type") != "control_request":
            return
        req = ev["request"]
        ans = policy_answer(repo, req) if req.get("subtype") == "can_use_tool" else {}
        asks.append({"tool": req.get("tool_name"),
                     "path": req.get("input", {}).get("file_path"),
                     "blocked_path": req.get("blocked_path"),
                     "decision_reason": req.get("decision_reason"),
                     "behavior": ans.get("behavior")})
        w.answer(ev["request_id"], ans)

    seen, results = run_turns(w, [
        f"Do these steps in order, each with the Write tool, one tool call per "
        f"message. If a step is refused, do not retry it; go on.\n"
        f"1. Create inside.txt (relative path) containing 'in'.\n"
        f"2. Create {outside_dir}/outside.txt containing 'out'.\n"
        f"3. Create link-out/escape.txt (relative path) containing 'escape'.\n"
        f"4. Use the AskUserQuestion tool to ask me which file to create, "
        f"with the options red.txt and blue.txt, then create the one I chose "
        f"(content 'chosen').\n"
        f"Finally reply with one line listing which steps succeeded."], on_event)
    code = w.finish()
    return report("policy", seen, results[-1] if results else None, code, {
        "asks": asks,
        "inside": (repo / "inside.txt").exists(),
        "outside": (outside_dir / "outside.txt").exists(),
        "escape_via_symlink": (outside_dir / "escape.txt").exists(),
        "blue": (repo / "blue.txt").exists(), "red": (repo / "red.txt").exists()})


def case_interrupt_child(root, model, outdir):
    def act(w, repo):
        return {"interrupt_request_id": w.interrupt()}
    w, repo, seen, results, acted = gated_worker(
        "interrupt_child", root, model, outdir, HOOKS_OFF, act)
    token = acted["token"]
    acted["procs_after_result"] = ps_matching(token)
    acted["end"] = end_group(w, signal.SIGTERM)
    code = w.finish()
    acted.update(orphan_check(token, acted["worker_pgid"], acted.get("tool_procs", [])))
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    return report("interrupt_child", seen, results[-1] if results else None, code, acted)


def case_crash_group(root, model, outdir):
    def act(w, repo):
        # What a supervisor can record on each tool_use event, before a crash.
        sessions = descendant_sessions(w.proc.pid)
        w.proc.kill()  # SIGKILL to the CLI only, like a crash
        return {"killed_pid": w.proc.pid, "tool_sessions_before_crash": sessions}
    w, repo, seen, results, acted = gated_worker(
        "crash_group", root, model, outdir, HOOKS_OFF, act)
    code = w.finish()  # EOF and exit of the CLI
    token = acted["token"]
    acted["procs_after_cli_death"] = ps_matching(token)
    acted["end"] = end_group(w, signal.SIGKILL)
    acted.update(orphan_check(token, acted["worker_pgid"], acted.get("tool_procs", [])))
    # The tools live in their own groups but (if they keep the CLI's
    # session) a supervisor can still find and kill them by session id.
    acted["cli_session_kill"] = kill_session(acted["worker_pgid"])
    acted["survivors_after_cli_session_kill"] = ps_matching(token)
    acted["tool_session_kills"] = [kill_session(sid)
                                   for sid in acted.get("tool_sessions_before_crash", [])]
    acted["survivors_after_tool_session_kills"] = ps_matching(token)
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    return report("crash_group", seen, results[-1] if results else None, code, acted)


def case_sigterm(root, model, outdir):
    """SIGTERM to the CLI mid-tool, no interrupt first: does it end its tools?"""
    def act(w, repo):
        w.proc.terminate()
        return {"terminated_pid": w.proc.pid}
    w, repo, seen, results, acted = gated_worker(
        "sigterm", root, model, outdir, HOOKS_OFF, act)
    code = w.finish()
    acted["survivors_after_cli_exit"] = ps_matching(acted["token"])
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    return report("sigterm", seen, results[-1] if results else None, code, acted)


def case_storm(root, model, outdir):
    repo = make_repo(root, "storm")
    w = Worker(repo, outdir / "storm.jsonl", model,
               ["--permission-mode", "manual", "--permission-prompt-tool", "stdio",
                *HOOKS_OFF], max_turns=30)
    order = []  # (kind, id) in stream order

    def on_event(w, ev):
        t = ev.get("type")
        if t == "control_request" and ev["request"].get("subtype") == "can_use_tool":
            req = ev["request"]
            order.append(("ask", req.get("tool_use_id"), ev["request_id"]))
            w.answer(ev["request_id"], {"behavior": "allow", "updatedInput": req["input"]})
        elif t == "assistant":
            for c in ev["message"].get("content", []):
                if c.get("type") == "tool_use":
                    order.append(("use", c["id"], None))
        elif t == "user" and isinstance(ev.get("message", {}).get("content"), list):
            for c in ev["message"]["content"]:
                if c.get("type") == "tool_result":
                    order.append(("result", c["tool_use_id"], c.get("is_error")))

    names = [f"f{i:02d}.txt" for i in range(1, 21)]
    seen, results = run_turns(w, [
        "Create these 20 files, each with its own Bash tool call running "
        "`touch <name>`. Issue all 20 Bash calls at once in a single message "
        "(parallel tool calls), not one by one: " + ", ".join(names) +
        ". Then reply: done."], on_event)
    code = w.finish()
    asks = [o for o in order if o[0] == "ask"]
    uses = [o[1] for o in order if o[0] == "use"]
    res = [o for o in order if o[0] == "result"]
    # In order: every result comes after its ask, every ask after its use.
    pos = {}
    for i, (kind, tid, _) in enumerate(order):
        pos.setdefault((kind, tid), i)
    misordered = [tid for _, tid, _ in res
                  if not (pos.get(("use", tid), -1) < pos.get(("ask", tid), -1)
                          < pos[("result", tid)])]
    return report("storm", seen, results[-1] if results else None, code, {
        "asks": len(asks), "unique_request_ids": len({a[2] for a in asks}),
        "tool_uses": len(uses), "results": len(res),
        "result_errors": sum(1 for r in res if r[2]),
        "misordered": misordered,
        "files_created": sum((repo / n).exists() for n in names)})


def session_file(session_id: str) -> Path | None:
    hits = list((Path.home() / ".claude" / "projects").glob(f"*/{session_id}.jsonl"))
    return hits[0] if hits else None


def transcript_check(path: Path | None) -> dict:
    if not path:
        return {"transcript": None}
    lines = path.read_text().splitlines()
    bad, uuids, parents_missing = 0, set(), 0
    entries = []
    for line in lines:
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            bad += 1
            continue
        entries.append(e)
        if e.get("uuid"):
            uuids.add(e["uuid"])
    for e in entries:
        p = e.get("parentUuid")
        if p and p not in uuids:
            parents_missing += 1
    # Two writers interleave if a parent has more than one child.
    children: dict = {}
    for e in entries:
        if e.get("uuid") and e.get("parentUuid"):
            children.setdefault(e["parentUuid"], []).append(e.get("type"))
    forks = {k: v for k, v in children.items() if len(v) > 1}
    return {"transcript": str(path), "lines": len(lines), "unparsable": bad,
            "dangling_parents": parents_missing, "forked_parents": len(forks),
            "types": [e.get("type") for e in entries][-40:]}


def case_two_writers(root, model, outdir):
    second: dict = {}

    def act(w1, repo):
        # The first writer is alive and mid-tool. Start the second.
        sid = w1.session_id
        w2 = Worker(repo, outdir / "two_writers-second.jsonl", model,
                    ["--permission-mode", "manual", "--resume", sid, *HOOKS_OFF])
        seen2, results2 = run_turns(w2, ["Reply with exactly: second writer."])
        code2 = w2.finish()
        r = results2[-1] if results2 else {}
        init2 = next((json.loads(l)["event"] for l in
                      (outdir / "two_writers-second.jsonl").open()
                      if '"subtype": "init"' in l), {})
        second.update({"events": seen2, "exit_code": code2,
                       "session_id": init2.get("session_id"),
                       "result": {k: r.get(k) for k in RESULT_FIELDS}})
        # Let the first writer's tool finish.
        with open(repo / "gate.fifo", "w"):
            pass
        return {"first_session_id": sid}

    # Capture the first writer's session id from its init via a wrapper.
    orig_events = Worker.events

    def events_with_sid(self):
        for ev in orig_events(self):
            if ev.get("type") == "system" and ev.get("subtype") == "init":
                self.session_id = ev["session_id"]
            yield ev

    Worker.events = events_with_sid
    try:
        w, repo, seen, results, acted = gated_worker(
            "two_writers", root, model, outdir, HOOKS_OFF, act)
        code = w.finish()
    finally:
        Worker.events = orig_events
    sid = acted.get("first_session_id")
    return report("two_writers", seen, results[-1] if results else None, code, {
        **acted, "second": second,
        "first_transcript": transcript_check(session_file(sid)) if sid else None,
        "second_transcript": transcript_check(session_file(second["session_id"]))
        if second.get("session_id") and second["session_id"] != sid else "same file"})


def case_parallel(root, model, outdir):
    tokens = {f"par{i}": f"TOKEN-{uuid.uuid4().hex[:8]}" for i in range(3)}
    found: dict = {}

    def one(name, token):
        repo = make_repo(root, name)
        w = Worker(repo, outdir / f"{name}.jsonl", model,
                   ["--permission-mode", "acceptEdits", *HOOKS_OFF])
        seen, results = run_turns(w, [
            f"Write the text {token} into token.txt with the Write tool, then "
            f"reply with exactly: {token}"])
        code = w.finish()
        found[name] = {"repo": str(repo), "exit_code": code,
                       "result": (results[-1].get("result") if results else None)}

    threads = [threading.Thread(target=one, args=kv) for kv in tokens.items()]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    cross = {}
    for name, token in tokens.items():
        text = (outdir / f"{name}.jsonl").read_text()
        sids = set()
        for line in text.splitlines():
            ev = json.loads(line)["event"]
            if isinstance(ev, dict) and ev.get("session_id"):
                sids.add(ev["session_id"])
        others = [t for n, t in tokens.items() if n != name and t in text]
        cross[name] = {"session_ids": sorted(sids), "foreign_tokens": others,
                       "file": (Path(found[name]["repo"]) / "token.txt").read_text()
                       if (Path(found[name]["repo"]) / "token.txt").exists() else None}
    out = {"case": "parallel", "tokens": tokens, "workers": found, "journals": cross}
    print(json.dumps(out, indent=2))
    return out


def case_takeover(root, model, outdir):
    def act(w, repo):
        return {"interrupt_request_id": w.interrupt()}

    orig_events = Worker.events

    def events_with_sid(self):
        for ev in orig_events(self):
            if ev.get("type") == "system" and ev.get("subtype") == "init":
                self.session_id = ev["session_id"]
            yield ev

    Worker.events = events_with_sid
    try:
        w, repo, seen, results, acted = gated_worker(
            "takeover", root, model, outdir, HOOKS_OFF, act)
        acted["end"] = end_group(w, signal.SIGTERM)
        code = w.finish()
    finally:
        Worker.events = orig_events
    acted.update(orphan_check(acted["token"], acted["worker_pgid"], acted.get("tool_procs", [])))
    acted["released_orphan"] = release_fifo(repo / "gate.fifo")
    acted["session_id"] = getattr(w, "session_id", None)
    acted["repo"] = str(repo)
    return report("takeover", seen, results[-1] if results else None, code, acted)


CASES = {"config": case_config, "policy": case_policy,
         "interrupt_child": case_interrupt_child, "crash_group": case_crash_group,
         "sigterm": case_sigterm,
         "storm": case_storm, "two_writers": case_two_writers,
         "parallel": case_parallel, "takeover": case_takeover}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("cases", nargs="*", default=ALL_CASES, help=", ".join(ALL_CASES))
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--out", type=Path, help="directory for journals and summary")
    args = ap.parse_args()
    unknown = set(args.cases) - set(ALL_CASES)
    if unknown:
        ap.error(f"unknown cases: {sorted(unknown)}")
    root = Path(tempfile.mkdtemp(prefix="headless-trial2-", dir=os.environ.get("TMPDIR")))
    outdir = args.out or root / "journal"
    outdir.mkdir(parents=True, exist_ok=True)
    print(f"repos under {root}, journals in {outdir}", file=sys.stderr)
    summary = [CASES[name](root, args.model, outdir) for name in args.cases]
    (outdir / "summary.json").write_text(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
