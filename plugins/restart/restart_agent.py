#!/usr/bin/env python3
"""Restart agent CLIs in their panes and resume their sessions, e.g. after
Claude Code or pi updated. Only idle or finished agents are restarted; a
working or blocked one, or one with a draft in its input box, is skipped.

  restart_agent.py PANE...          restart the agents in these panes
  restart_agent.py --focused        the pane from $HERDR_PANE_ID (plugin action)
  restart_agent.py --workspace [W]  every agent pane of W ($HERDR_WORKSPACE_ID)
  --dry-run                         print what would run, change nothing

The new command is the agent's own command line (so flags such as
--model or --dangerously-skip-permissions stay) without its old resume or
prompt arguments, plus the resume arguments for the session herdr knows.
The agent is stopped with SIGTERM and started again from the pane's shell.
"""
import argparse
import json
import os
import re
import shlex
import signal
import subprocess
import sys
import time

HERDR = os.environ.get("HERDR_BIN_PATH") or "herdr"
SHELLS = {"zsh", "bash", "fish", "sh", "dash", "nu", "login"}
RESTARTABLE = {"idle", "done"}
EXIT_WAIT_S = 15

# Per agent: flags that take a value (so the next token is not a prompt),
# the resume flags to drop (with whether they take a value), and the resume
# arguments to add for a session reference.
AGENTS = {
    "claude": {
        "valued": {
            "--model", "--permission-mode", "--settings", "--add-dir", "--allowedTools",
            "--allowed-tools", "--disallowedTools", "--disallowed-tools", "--tools",
            "--mcp-config", "--append-system-prompt", "--system-prompt", "--agent", "--agents",
            "--effort", "--output-format", "--input-format", "--fallback-model", "--plugin-dir",
            "--betas", "--setting-sources", "--debug-file", "--max-budget-usd", "--name", "-n",
            "--remote-control-session-name-prefix", "--json-schema", "--file",
            "--permission-prompt-tool",
        },
        # --resume and -r take an optional session id.
        "drop": {"--resume": "optional", "-r": "optional", "--continue": None, "-c": None,
                 "--session-id": "value", "--fork-session": None, "--from-pr": "optional"},
        "resume": lambda ref: ["--resume", ref["value"]],
    },
    "pi": {
        "valued": {"--model", "--provider", "--thinking", "--tools", "--api-key",
                   "--system-prompt", "--append-system-prompt", "--mode", "--models",
                   "--extension", "-e", "--session-dir"},
        "drop": {"--session": "value", "--continue": None, "-c": None, "--resume": None, "-r": None},
        "resume": lambda ref: ["--session", ref["value"]],
    },
}


def herdr(*args):
    out = subprocess.run([HERDR, *args], capture_output=True, text=True, timeout=15)
    if out.returncode != 0:
        raise RuntimeError(f"herdr {' '.join(args)}: {out.stderr.strip() or out.stdout.strip()}")
    return json.loads(out.stdout)["result"] if out.stdout.strip() else None


def relaunch_argv(agent, argv, session_ref):
    """The agent's command line without resume and prompt arguments, plus the
    arguments that resume `session_ref`.

    A bare token is a flag's value after a flag known to take one, or after
    an unknown flag (keeping too much is safer than dropping a setting).
    After a known valueless flag, at the start or after `--` it is a prompt
    and is dropped, because the new process would send it again."""
    spec = AGENTS[agent]
    kept, i = [argv[0]], 1
    while i < len(argv):
        token = argv[i]
        name = token.split("=", 1)[0]
        following = argv[i + 1] if i + 1 < len(argv) else None
        bare_next = following is not None and not following.startswith("-")
        if token == "--":
            break
        if name in spec["drop"]:
            takes = spec["drop"][name]
            skip_value = "=" not in token and (takes == "value" or (takes == "optional" and bare_next))
            i += 2 if skip_value else 1
            continue
        if not token.startswith("-"):
            i += 1  # a prompt
            continue
        kept.append(token)
        takes_value = name in spec["valued"] or name not in KNOWN_FLAGS[agent]
        if "=" not in token and bare_next and takes_value:
            kept.append(following)
            i += 2
        else:
            i += 1
    return kept + spec["resume"](session_ref)


# Valueless flags known per agent: a bare token after them is a prompt.
KNOWN_FLAGS = {
    "claude": {"--dangerously-skip-permissions", "--allow-dangerously-skip-permissions", "--verbose",
               "--print", "-p", "--debug", "-d", "--ide", "--strict-mcp-config", "--chrome", "--no-chrome",
               "--bare", "--brief", "--include-partial-messages", "--replay-user-messages",
               "--no-session-persistence", "--disable-slash-commands", "--exclude-dynamic-system-prompt-sections",
               "--include-hook-events", "--safe-mode", "--tmux", "--worktree", "-w", "--mcp-debug"},
    "pi": {"--no-tools", "--no-extensions", "--print", "-p", "--verbose", "--no-session"},
}


DIM = re.compile(r"\x1b\[2m.*?(?=\x1b\[(?:0|22)?m|$)")
ESCAPE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def has_draft(pane_id, agent):
    """Whether the agent's input box holds unsent text. Claude draws it as a
    `❯` line right under a rule; its placeholder hint is dim, typed text is
    not. The bottom of the terminal is read, not the viewport, so a scrolled
    view cannot hide the box. Other agents count as having no draft."""
    if agent != "claude":
        return False
    out = subprocess.run(
        [HERDR, "pane", "read", pane_id, "--source", "recent", "--lines", "60", "--format", "ansi"],
        capture_output=True, text=True, timeout=15,
    )
    if out.returncode != 0:
        raise RuntimeError(f"herdr pane read {pane_id}: {out.stderr.strip() or out.stdout.strip()}")
    lines = out.stdout.splitlines()
    plain = [ESCAPE.sub("", line).strip() for line in lines]
    for index in range(len(lines) - 1, 0, -1):
        if plain[index].startswith("❯") and plain[index - 1].startswith("─"):
            typed = ESCAPE.sub("", DIM.sub("", lines[index])).strip()
            return bool(typed.lstrip("❯").strip())
    # No input box found: be safe and treat it as a draft.
    return True


def foreground(pane_id):
    info = herdr("pane", "process-info", "--pane", pane_id)["process_info"]
    procs = info.get("foreground_processes") or []
    leader = next((p for p in procs if p["pid"] == info.get("foreground_process_group_id")), None)
    return info.get("shell_pid"), leader, procs


def shell_in_foreground(pane_id):
    shell_pid, _, procs = foreground(pane_id)
    return bool(procs) and all(
        p["pid"] == shell_pid or str(p.get("name", "")).lstrip("-") in SHELLS for p in procs
    )


def restart(pane_id, dry_run):
    """Returns a one-line outcome for the pane."""
    pane = herdr("pane", "get", pane_id)["pane"]
    agent, ref, status = pane.get("agent"), pane.get("agent_session"), pane.get("agent_status")
    if agent not in AGENTS:
        return f"skip {pane_id}: {agent or 'no agent'} cannot be restarted here (only {', '.join(AGENTS)})"
    if not ref or not ref.get("value"):
        return f"skip {pane_id}: herdr knows no {agent} session to resume"
    if status not in RESTARTABLE:
        return f"skip {pane_id}: {agent} is {status}; restart it when it is idle"
    shell_pid, leader, _ = foreground(pane_id)
    if not leader or not shell_pid or leader["pid"] == shell_pid:
        return f"skip {pane_id}: {agent} was not started from a shell, so it cannot be started again in place"
    if has_draft(pane_id, agent):
        return f"skip {pane_id}: {agent} has unsent text in its input box"
    command = shlex.join(relaunch_argv(agent, leader["argv"], ref))
    if dry_run:
        return f"would restart {pane_id}: {command}"
    os.kill(leader["pid"], signal.SIGTERM)
    deadline = time.time() + EXIT_WAIT_S
    while time.time() < deadline:
        time.sleep(0.5)
        try:
            if shell_in_foreground(pane_id):
                break
        except RuntimeError:
            pass
    else:
        return f"fail {pane_id}: {agent} did not exit within {EXIT_WAIT_S} s after SIGTERM; nothing started"
    time.sleep(0.5)  # let the shell draw its prompt
    herdr("pane", "run", pane_id, command)
    return f"restarted {pane_id}: {command}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("panes", nargs="*")
    parser.add_argument("--focused", action="store_true")
    parser.add_argument("--workspace", nargs="?", const="")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    panes = list(args.panes)
    if args.focused:
        if not os.environ.get("HERDR_PANE_ID"):
            sys.exit("no focused pane ($HERDR_PANE_ID is not set)")
        panes.append(os.environ["HERDR_PANE_ID"])
    if args.workspace is not None:
        workspace = args.workspace or os.environ.get("HERDR_WORKSPACE_ID")
        if not workspace:
            sys.exit("no workspace given and $HERDR_WORKSPACE_ID is not set")
        panes += [p["pane_id"] for p in herdr("pane", "list")["panes"]
                  if p.get("workspace_id") == workspace and p.get("agent")]
    if not panes:
        sys.exit("nothing to restart: give pane ids, --focused or --workspace")
    failed = False
    # One at a time: several agents starting together compete for the same
    # login and rate limits.
    for pane_id in panes:
        try:
            line = restart(pane_id, args.dry_run)
        except (RuntimeError, OSError, KeyError, ValueError) as err:
            line = f"fail {pane_id}: {err}"
        failed |= line.startswith("fail")
        print(line, flush=True)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
