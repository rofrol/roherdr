#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=12

set -eu

action="${1:-}"

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

# Reports that the main agent's turn started (`UserPromptSubmit`, with the submitted prompt) or
# ended (`Stop` that the stop check let through, `StopFailure` with its error), so herdr can tell a caller of
# `herdr agent prompt --wait` when the turn its prompt started ends. Not instructions: it runs
# whatever HERDR_AWAITING_REPLY_* say.
report_turn() {
  [ "${HERDR_ENV:-}" = "1" ] || return 0
  [ -n "${HERDR_SOCKET_PATH:-}" ] || return 0
  [ -n "${HERDR_PANE_ID:-}" ] || return 0
  [ -z "${CURSOR_VERSION:-}" ] || return 0
  command -v python3 >/dev/null 2>&1 || return 0
  HERDR_TURN_PHASE="$1" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY' || true
import json
import os
import socket
import time

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if not isinstance(hook_input, dict) or hook_input.get("agent_id"):
    raise SystemExit(0)
phase = os.environ.get("HERDR_TURN_PHASE")
params = {"pane_id": os.environ["HERDR_PANE_ID"], "source": "herdr:claude", "agent": "claude", "phase": phase}
if phase == "started":
    prompt = hook_input.get("prompt")
    if not isinstance(prompt, str):
        raise SystemExit(0)
    params["prompt"] = prompt
    if prompt.lstrip().startswith("<task-notification>"):
        # A background task's notification goes on with the running prompt's work: that
        # prompt's turn ends at the `Stop` that follows, not here.
        params["continuation"] = True
elif hook_input.get("hook_event_name") == "StopFailure":
    # The turn ended on an error: its prompts end as failed, with the error.
    error = hook_input.get("error")
    details = hook_input.get("error_details")
    parts = [part for part in (error, details) if isinstance(part, str) and part.strip()]
    params["error"] = (": ".join(parts) or "unknown error")[:1000]
request = {
    "id": f"herdr:claude:turn:{int(time.time() * 1000)}",
    "method": "pane.report_turn",
    "params": params,
}
try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(os.environ["HERDR_SOCKET_PATH"])
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
}

# Repeats the awaiting-reply and task instructions on every prompt, since the SessionStart
# context is far back in a long session. Printed as is: it needs no hook input and no socket.
if [ "$action" = "reminder" ]; then
  report_turn started
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  reminder=""
  if [ "${HERDR_AWAITING_REPLY_INSTRUCTIONS:-1}" != "0" ]; then
    reminder='Herdr reminder: if you end this turn needing the user'"'"'s answer or decision before you can continue (a question, a choice, a confirmation, or a request to check something first, even without a question mark), call the Bash tool with `herdr agent awaiting-reply \"<question>\"` (the question in at most 6 words; never write the command in your reply) on its own as the last command of the turn, right before your final message. Not for AskUserQuestion or courtesy offers.'
  fi
  if [ "${HERDR_AGENT_TASK:-1}" != "0" ]; then
    reminder="${reminder:+$reminder }"'If this prompt starts a new task (not a follow-up, refinement or side question about the current one), first run `herdr agent set-task \"<title>\"`.'
  fi
  [ -n "$reminder" ] || exit 0
  printf '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"%s"}}\n' "$reminder"
  exit 0
fi

# Stop hook: when the turn ends with a question for the user and the agent did not report it with
# `herdr agent awaiting-reply`, ask it once to do so. The agent decides (a rhetorical question is
# not reported). A turn whose last tool calls all failed or were denied (a permission prompt, the
# auto-mode classifier) may have left the agent unable to run that command, so the hook marks the
# pane as awaiting a reply itself, without blocking the stop. HERDR_AWAITING_REPLY_STOP=0 turns it
# off, =shadow only logs what it would have done to ~/.local/state/herdr/awaiting-reply-stop.jsonl.
# In a tab with the role `coordinator`, a turn that ends by waiting for the user's go-ahead ("when
# you say continue", "should I continue?") while no background task of the session runs is blocked
# once too, so the coordinator goes on with the next approved item or asks its open questions.
# A coordinator's stop is also blocked, every time, while `herdr worker obligations` lists a worker
# event it has not acknowledged (a question, a turn end, the worker's end); the server derives that
# list, so it ends once the coordinator answers or acknowledges (`herdr worker ack`). When herdr
# cannot be asked, the stop goes through and the reason goes to stderr.
# A stop the check blocks does not end the turn; any other stop reports the turn finished.
stop_check() {
  [ "${HERDR_ENV:-}" = "1" ] || return 0
  [ -n "${HERDR_PANE_ID:-}" ] || return 0
  [ -z "${CURSOR_VERSION:-}" ] || return 0
  [ "${HERDR_AWAITING_REPLY_INSTRUCTIONS:-1}" != "0" ] || return 0
  [ "${HERDR_AWAITING_REPLY_STOP:-block}" != "0" ] || return 0
  command -v python3 >/dev/null 2>&1 || return 0
  HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import re
import socket
import subprocess
import sys
import time

mode = os.environ.get("HERDR_AWAITING_REPLY_STOP", "block")
COMMAND = "herdr agent awaiting-reply"
BLOCKED_QUESTION = "blocked tool calls"
# The same question heuristic as scripts/awaiting_reply_audit.py (a test keeps them equal).
ASK_PHRASES = re.compile(
    r"\b("
    r"let me know|tell me|which (one|option|variant)|should i|shall i|"
    r"do you want|would you like|want me to|how would you like|"
    r"czy mam|czy chcesz|daj (mi )?zna[ćc]|powiedz|który wariant|którą opcję|"
    r"co wybierasz|jak wolisz|zainstalować|wypchnąć|zrobić"
    r")\b",
    re.IGNORECASE,
)
COURTESY = re.compile(
    r"(anything else|something else|coś jeszcze|czy mogę jeszcze w czymś pomóc"
    r"|let me know if you (need|have) anything)",
    re.IGNORECASE,
)
# The same expression as ABANDON in scripts/coordinator_turn_audit.py (a test keeps them equal):
# final text that defers the next step to the user's go-ahead.
ABANDON = re.compile(
    r"("
    # English
    r"when you say|once you say|say (?:\"|“)?(?:continue|go|next)\b|"
    r"(?:should|shall) i (?:continue|proceed|go on|start|delegate|carry on)|"
    r"do you want me to (?:continue|proceed|go on|start the next)|"
    r"let me know when|on your (?:go|signal|word)|"
    r"(?:i will|i'll) (?:continue|delegate|start|proceed|resume|go on)[^.\n]{0,60}"
    r"\b(?:when|once|after) you|"
    # Polish
    r"gdy powiesz|jak powiesz|kiedy powiesz|gdy napiszesz|jak napiszesz|"
    r"zlec[ęe] (?:j[aą]|je|go)?[^.\n]{0,60}\b(?:gdy|jak|kiedy)\b|"
    r"daj zna[ćc],? (?:gdy|kiedy|jak|czy)|"
    r"czy (?:mam )?(?:kontynuowa[ćc]|i[śs][ćc] dalej|zleca[ćc])|"
    r"kontynuowa[ćc]\s*\?|mam (?:kontynuowa[ćc]|zleci[ćc])"
    r")",
    re.IGNORECASE,
)
ASK_TOOL = "AskUserQuestion"
NOTIFICATION_ID = re.compile(r"<tool-use-id>([^<\s]+)</tool-use-id>")
COORDINATOR_ROLE = "coordinator"


def strip_code(text):
    text = re.sub(r"```.*?```", " ", text, flags=re.DOTALL)
    text = re.sub(r"`[^`\n]*`", " ", text)
    lines = [line for line in text.splitlines() if not line.lstrip().startswith(">")]
    return "\n".join(lines)


def last_paragraph(text):
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", strip_code(text)) if p.strip()]
    return paragraphs[-1] if paragraphs else ""


def looks_like_question(text):
    paragraph = last_paragraph(text)
    if not paragraph or COURTESY.search(paragraph):
        return False
    tail = paragraph[-400:]
    if re.search(r"\?[\s\)\"'»”*_]*$", tail):
        return True
    sentences = re.split(r"(?<=[.!?])\s+", tail)
    last = " ".join(sentences[-2:])
    return "?" in last and bool(ASK_PHRASES.search(last))


def is_command(text):
    """The awaiting-reply command, with or without its question."""
    return text == COMMAND or text.startswith(COMMAND + " ")


def printed_command(text):
    """The final paragraph is the command written out, not run (small models do this)."""
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]
    if not paragraphs:
        return False
    last = paragraphs[-1].replace("`", "").strip()
    last = re.sub(r"^\$\s*", "", last).strip()
    return is_command(last)


def text_of(content):
    if isinstance(content, str):
        return content
    return "\n".join(
        block.get("text", "")
        for block in content or []
        if isinstance(block, dict) and block.get("type") == "text"
    )


def is_abandon_text(text):
    """True when the end of the final text waits for the user's go-ahead."""
    tail = strip_code(text).strip()[-600:]
    return bool(ABANDON.search(tail))


def pending_background(lines):
    """Background tool calls of the session that no task notification has reported yet."""
    background = set()
    finished = set()
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if entry.get("isSidechain"):
            continue
        kind = entry.get("type")
        if kind == "attachment":
            attachment = entry.get("attachment") or {}
            if attachment.get("type") == "queued_command":
                finished.update(NOTIFICATION_ID.findall(str(attachment.get("prompt", ""))))
            continue
        message = entry.get("message") or {}
        if kind == "user" and not entry.get("isMeta"):
            content = message.get("content")
            if isinstance(content, str) or not any(
                isinstance(b, dict) and b.get("type") == "tool_result" for b in content or []
            ):
                finished.update(NOTIFICATION_ID.findall(text_of(content)))
        elif kind == "assistant":
            for block in message.get("content") or []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    args = block.get("input") or {}
                    if isinstance(args, dict) and args.get("run_in_background"):
                        background.add(block.get("id"))
    return len(background - finished)


def ask_server(method, params):
    """The result of one request to the herdr server, or None on any failure."""
    request = {"id": f"herdr:claude:{method}:{int(time.time() * 1000)}", "method": method,
               "params": params}
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((json.dumps(request) + "\n").encode())
        reply = b""
        while not reply.endswith(b"\n") and len(reply) < 1_000_000:
            chunk = client.recv(65536)
            if not chunk:
                break
            reply += chunk
        client.close()
        result = json.loads(reply.decode("utf-8", "replace")).get("result")
        return result if isinstance(result, dict) else None
    except Exception:
        return None


def tab_role():
    """The role of the pane's tab (`tab.set_role`), or None when it is unknown."""
    if not os.environ.get("HERDR_SOCKET_PATH"):
        return None
    pane = (ask_server("pane.get", {"pane_id": os.environ["HERDR_PANE_ID"]}) or {}).get("pane")
    tab_id = pane.get("tab_id") if isinstance(pane, dict) else None
    if not isinstance(tab_id, str) or not tab_id:
        return None
    tab = (ask_server("tab.get", {"tab_id": tab_id}) or {}).get("tab")
    role = tab.get("role") if isinstance(tab, dict) else None
    return role if isinstance(role, str) else None


def runs_workers(path):
    """Whether the session's transcript shows a `herdr worker` command. Only such a session can own
    workers, so no other stop asks herdr anything."""
    if not isinstance(path, str) or not path:
        return False
    try:
        with open(path, "rb") as handle:
            return b"herdr worker " in handle.read()
    except OSError:
        return False


def worker_obligations():
    """The caller's workers' unacknowledged events (`herdr worker obligations`), or None when
    herdr cannot tell; why goes to stderr."""
    herdr = os.environ.get("HERDR_BIN_PATH") or "herdr"
    try:
        done = subprocess.run(
            [herdr, "worker", "obligations", "--pane", os.environ["HERDR_PANE_ID"]],
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )
    except OSError as error:
        print(f"herdr stop check: cannot run {herdr} worker obligations: {error}", file=sys.stderr)
        return None
    if done.returncode != 0:
        print(
            f"herdr stop check: {herdr} worker obligations failed ({done.returncode}): "
            + done.stderr.strip()[-300:],
            file=sys.stderr,
        )
        return None
    try:
        obligations = json.loads(done.stdout)["result"]["obligations"]
    except (ValueError, KeyError, TypeError):
        print("herdr stop check: unreadable worker obligations: " + done.stdout.strip()[-300:],
              file=sys.stderr)
        return None
    return [entry for entry in obligations if isinstance(entry, dict)] if isinstance(
        obligations, list) else None


def obligation_text(entry):
    """One worker's obligation, as the block reason names it."""
    worker = entry.get("worker_id", "?")
    reason = entry.get("reason")
    if reason == "question":
        ids = [q.get("request_id") for q in entry.get("questions") or [] if isinstance(q, dict)]
        return f"{worker}: question {', '.join(str(i) for i in ids) or '?'}"
    if reason == "turn_end":
        return f"{worker}: turn ended, review it"
    if reason == "gone":
        return f"{worker}: exited, review it"
    return f"{worker}: {reason}"


try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if hook_input.get("hook_event_name") != "Stop" or hook_input.get("agent_id"):
    raise SystemExit(0)

# A coordinator's workers that need it: checked before anything else and on every stop (the
# server's list, not this hook, ends the block). Shadow mode only logs.
role = None
role_known = False
obligations = None
if runs_workers(hook_input.get("transcript_path")):
    role = tab_role()
    role_known = True
    if role == COORDINATOR_ROLE:
        obligations = worker_obligations()
if obligations:
    seqs = ", ".join(f"`herdr worker ack {e.get('worker_id')} {e.get('seq')}`" for e in obligations)
    try:
        state_dir = os.path.join(
            os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"), "herdr"
        )
        os.makedirs(state_dir, exist_ok=True)
        with open(os.path.join(state_dir, "awaiting-reply-stop.jsonl"), "a", encoding="utf-8") as log:
            log.write(json.dumps({
                "time": time.time(),
                "pane": os.environ.get("HERDR_PANE_ID"),
                "session": hook_input.get("session_id"),
                "role": role,
                "obligations": [obligation_text(e) for e in obligations],
                "blocked": mode != "shadow",
            }, ensure_ascii=False) + "\n")
    except OSError:
        pass
    if mode != "shadow":
        print(json.dumps({
            "decision": "block",
            "reason": (
                "Herdr: workers you started need you before you stop: "
                + "; ".join(obligation_text(e) for e in obligations)
                + ". Handle each (answer the question or escalate it to the user, review the "
                "ended turn or the ended worker), then acknowledge it: " + seqs + "."
            ),
        }))
        raise SystemExit(0)
# The second stop of a turn the hook already blocked is never blocked again, but it may still
# be marked: the command the agent was asked to run may have been denied too.
stop_hook_active = bool(hook_input.get("stop_hook_active"))

final_text = hook_input.get("last_assistant_message")
final_text = final_text if isinstance(final_text, str) else ""
reported = False
last_text = ""
last_tool = ""
# Outcomes (True: failed or denied) of the turn's last batch of tool calls: the calls the agent
# made together before their results came back. A call after a result starts a new batch.
last_batch = []
batch_has_results = False
transcript = hook_input.get("transcript_path")
if isinstance(transcript, str) and transcript:
    try:
        with open(transcript, "rb") as handle:
            handle.seek(0, os.SEEK_END)
            size = handle.tell()
            handle.seek(max(0, size - 400_000))
            lines = handle.read().decode("utf-8", "replace").splitlines()[1:]
    except OSError:
        lines = []
        size = 0
    truncated = size > 400_000
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if entry.get("isSidechain") or entry.get("isMeta"):
            continue
        message = entry.get("message") or {}
        if entry.get("type") == "user":
            content = message.get("content")
            tool_result = isinstance(content, list) and any(
                isinstance(b, dict) and b.get("type") == "tool_result" for b in content
            )
            if not tool_result and text_of(content).strip():
                # A new prompt starts a new turn.
                reported = False
                last_text = ""
                last_tool = ""
                last_batch = []
                batch_has_results = False
            elif tool_result:
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        last_batch.append(bool(block.get("is_error")))
                        batch_has_results = True
        elif entry.get("type") == "assistant":
            for block in message.get("content") or []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    last_tool = block.get("name", "")
                if isinstance(block, dict) and block.get("type") == "tool_use" and batch_has_results:
                    last_batch = []
                    batch_has_results = False
                if (
                    isinstance(block, dict)
                    and block.get("type") == "tool_use"
                    and block.get("name") == "Bash"
                    and is_command((block.get("input") or {}).get("command", "").strip())
                ):
                    reported = True
            text = text_of(message.get("content")).strip()
            if text:
                last_text = text
if not final_text.strip():
    final_text = last_text

printed = printed_command(final_text)
question = looks_like_question(final_text) or printed
blocked_calls = bool(last_batch) and all(last_batch)
if stop_hook_active and not blocked_calls:
    raise SystemExit(0)
marked = False
if blocked_calls and mode != "shadow" and os.environ.get("HERDR_SOCKET_PATH"):
    request = {
        "id": f"herdr:claude:blocked:{int(time.time() * 1000)}",
        "method": "pane.report_awaiting_reply",
        "params": {"pane_id": os.environ["HERDR_PANE_ID"], "question": BLOCKED_QUESTION},
    }
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((json.dumps(request) + "\n").encode())
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
        marked = True
    except Exception:
        pass
# Asking the agent to run a command its tools may deny again only repeats the denial.
block = question and not reported and not blocked_calls and not stop_hook_active
# A coordinator that waits for the user's go-ahead while nothing it started runs: the precedence
# of scripts/coordinator_turn_audit.py (a tool-based ask, then a running background task, then
# the go-ahead text). Any missing input or failed request leaves the stop alone.
abandon = is_abandon_text(final_text)
pending = None
coordinator_block = False
if (
    abandon
    and not reported
    and last_tool != ASK_TOOL
    and not blocked_calls
    and not stop_hook_active
    and isinstance(transcript, str)
    and transcript
):
    if truncated:
        # A background task started before the tail that was read may still run.
        try:
            with open(transcript, "rb") as handle:
                lines = handle.read().decode("utf-8", "replace").splitlines()
        except OSError:
            lines = None
    if lines:
        pending = pending_background(lines)
    if pending == 0:
        if not role_known:
            role = tab_role()
        coordinator_block = role == COORDINATOR_ROLE
try:
    state_dir = os.path.join(
        os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"), "herdr"
    )
    os.makedirs(state_dir, exist_ok=True)
    with open(os.path.join(state_dir, "awaiting-reply-stop.jsonl"), "a", encoding="utf-8") as log:
        log.write(
            json.dumps(
                {
                    "time": time.time(),
                    "pane": os.environ.get("HERDR_PANE_ID"),
                    "session": hook_input.get("session_id"),
                    "question": question,
                    "reported": reported,
                    "blocked_calls": blocked_calls,
                    "marked": marked,
                    "blocked": (block or coordinator_block) and mode != "shadow",
                    "printed": printed,
                    "abandon": abandon,
                    "pending": pending,
                    "role": role,
                    "coordinator_blocked": coordinator_block and mode != "shadow",
                    "tail": last_paragraph(final_text)[-200:],
                },
                ensure_ascii=False,
            )
            + "\n"
        )
except OSError:
    pass
if coordinator_block and mode != "shadow":
    print(
        json.dumps(
            {
                "decision": "block",
                "reason": (
                    "Herdr: this tab is a TODO coordinator, and your message ends by waiting for "
                    "the user's go-ahead, but approved items need none and nothing you started "
                    "is still running. If \"Next, in order\" in TODO.md still has items, hand "
                    "the next one to a worker now. If every remaining item waits on the user, "
                    "ask the open questions (AskUserQuestion, or `herdr agent awaiting-reply`). "
                    "If the user told you to stop, or the queue is empty, just stop."
                ),
            }
        )
    )
elif block and mode != "shadow" and printed:
    print(
        json.dumps(
            {
                "decision": "block",
                "reason": (
                    "Herdr: you wrote `herdr agent awaiting-reply` in your message instead of "
                    "running it. Call the Bash tool with the command `herdr agent "
                    "awaiting-reply \"<question in at most 6 words>\"` now, as the only "
                    "command, then stop without repeating your message."
                ),
            }
        )
    )
elif block and mode != "shadow":
    print(
        json.dumps(
            {
                "decision": "block",
                "reason": (
                    "Herdr: your last message looks like a question for the user, but you did "
                    "not run `herdr agent awaiting-reply`. If you are waiting for the user's "
                    "answer or decision, run `herdr agent awaiting-reply \"<question in at most "
                    "6 words>\"` now as the only command, then stop without repeating your "
                    "message. If you are not "
                    "waiting for the user, just stop."
                ),
            }
        )
    )
PY
}

if [ "$action" = "stop-check" ]; then
  stop_decision="$(stop_check)" || true
  if [ -n "$stop_decision" ]; then
    printf '%s\n' "$stop_decision"
  else
    report_turn finished
  fi
  exit 0
fi

# PostToolUse hook for TodoWrite: report how far the agent's own todo list is as the `plan` token
# (`3/7`), which the sidebar shows beside the tab. HERDR_AGENT_PLAN=0 turns it off.
if [ "$action" = "plan" ]; then
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  [ "${HERDR_AGENT_PLAN:-1}" != "0" ] || exit 0
  command -v python3 >/dev/null 2>&1 || exit 0
  HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import socket
import time

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if hook_input.get("hook_event_name") != "PostToolUse" or hook_input.get("tool_name") != "TodoWrite":
    raise SystemExit(0)
if hook_input.get("agent_id"):
    raise SystemExit(0)
todos = (hook_input.get("tool_input") or {}).get("todos")
if not isinstance(todos, list):
    raise SystemExit(0)
items = [t for t in todos if isinstance(t, dict)]
done = sum(1 for t in items if t.get("status") == "completed")
value = f"{done}/{len(items)}" if items else None
request = {
    "id": f"herdr:claude:plan:{int(time.time() * 1000)}",
    "method": "pane.report_metadata",
    "params": {
        "pane_id": os.environ["HERDR_PANE_ID"],
        "source": "herdr:claude",
        "agent": "claude",
        "tokens": {"plan": value},
        "seq": time.time_ns(),
        # Kept for six hours; every TodoWrite renews it, a new session in the pane lets it lapse.
        "ttl_ms": 21_600_000,
    },
}
try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(os.environ["HERDR_SOCKET_PATH"])
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
  exit 0
fi

# StopFailure hook (matched to rate_limit and billing_error): a usage limit or missing credits
# ended the turn, so herdr lists the agent as waiting on the user, with the reset time it knows.
if [ "$action" = "stop-failure" ]; then
  report_turn finished
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  command -v python3 >/dev/null 2>&1 || exit 0
  HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import socket
import time

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if hook_input.get("hook_event_name") != "StopFailure" or hook_input.get("agent_id"):
    raise SystemExit(0)
kind = {"rate_limit": "usage", "billing_error": "credits"}.get(hook_input.get("error"))
if kind is None:
    raise SystemExit(0)
params = {"pane_id": os.environ["HERDR_PANE_ID"], "kind": kind}
for field in ("error_details", "last_assistant_message"):
    message = hook_input.get(field)
    if isinstance(message, str) and message.strip():
        params["message"] = message[:1000]
        break
request = {
    "id": f"herdr:claude:limit:{int(time.time() * 1000)}",
    "method": "pane.report_limit",
    "params": params,
}
try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(os.environ["HERDR_SOCKET_PATH"])
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
  exit 0
fi

# SessionEnd hook: the user ended the session (`/exit`, Ctrl-D, `/logout`), so herdr forgets it
# and a restart does not resume it. A session that ends any other way, such as a signal when the
# OS logs out, stays resumable; reason `other` (a signal) also asks herdr to resume it in its pane
# once Claude has exited. Runs before Claude exits, so herdr sees it before the exit.
if [ "$action" = "session-end" ]; then
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  command -v python3 >/dev/null 2>&1 || exit 0
  HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if hook_input.get("hook_event_name") != "SessionEnd" or hook_input.get("agent_id"):
    raise SystemExit(0)
reason = hook_input.get("reason")
if reason in ("prompt_input_exit", "logout"):
    method = "pane.forget_agent_session"
elif reason == "other":
    method = "pane.report_agent_stopped"
else:
    raise SystemExit(0)
session_id = hook_input.get("session_id")
if not isinstance(session_id, str) or not session_id:
    raise SystemExit(0)
source = "herdr:claude"
params = {
    "pane_id": os.environ["HERDR_PANE_ID"],
    "source": source,
    "agent": "claude",
    "seq": time.time_ns(),
    "agent_session_id": session_id,
}
request = {
    "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
    "method": method,
    "params": params,
}
try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(os.environ["HERDR_SOCKET_PATH"])
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
  exit 0
fi

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

if "CURSOR_VERSION" in os.environ or "cursor_version" in hook_input:
    raise SystemExit(0)
hook_event_name = str(hook_input.get("hook_event_name") or "")
if hook_event_name != "SessionStart":
    raise SystemExit(0)
is_subagent = bool(hook_input.get("agent_id"))
if is_subagent:
    raise SystemExit(0)

# Ask the agent to report a turn that ends with a question, so herdr keeps its pane marked
# until the user answers. HERDR_AWAITING_REPLY_INSTRUCTIONS=0 turns the instruction off.
contexts = []
if os.environ.get("HERDR_AWAITING_REPLY_INSTRUCTIONS", "1") != "0":
    contexts.append(
                "You run inside a Herdr pane. When you end a turn needing the user's answer "
                "or decision before you can continue the work, run the shell command "
                "`herdr agent awaiting-reply \"<question>\"` (call the Bash tool; never write the "
                "command in your reply) on its own, as the last command of the turn, "
                "right before your final message, so Herdr keeps your pane marked until the "
                "user replies and lists your question. The question is what you ask in at most "
                "6 words, in the language of the conversation (`Install now?`, `Which "
                "variant?`), without quotes, backticks or `$`. This covers a plain-text question, a choice between options, a "
                "confirmation before you proceed, and a request to check something before you "
                "go on (\"let me know how it looks, then I will commit\"), even without a "
                "question mark. Never append it to another command, never run it earlier in the "
                "turn, and never run it for AskUserQuestion or any other question tool or "
                "prompt answered inside the turn: Herdr already shows those as blocked. Run "
                "it at most once per turn and ignore its failure. Do not run it when you "
                "simply finished and ask nothing, or for courtesy offers such as asking "
                "whether anything else is needed."
    )
# Ask the agent to name its task, which names its tab: the terminal title Claude sets follows
# only the session's first prompt. HERDR_AGENT_TASK=0 turns it off.
if os.environ.get("HERDR_AGENT_TASK", "1") != "0":
    contexts.append(
        'Tab name: in the main agent only, when you start a new task (the session\'s first request, or a request you would treat as a new task rather than a follow-up, refinement or side question about the work in progress), run `herdr agent set-task "<title>"` with the Bash tool before other work. The title is 3-6 words in the language of that request, naming the action and its subject (`Fix OAuth callback redirect`, not `Fix bug`), without quotes, backticks or `$`. If unsure, keep the current title. Never write the command in your reply.'
    )
if os.environ.get("HERDR_AGENT_CONTEXT", "1") != "0":
    contexts.append("""[Herdr behavior context v1]
You are running in a Herdr pane. Herdr shows actual runtime activity, not promises.
- When authorized work has an executable next step, perform it instead of ending your turn with only a promise to continue.
- Only say work is running or queued when it has actually started and that claim is still accurate. This includes work started in an earlier turn.
- If you cannot start the next step, state what was not started and why. Ask explicitly when you need approval, a decision, credentials, or information.
- Report idle or finished truthfully. Do not fake activity, override an explicit stop, loop indefinitely, or start extra paid/model calls without authorization.
- User and repository instructions, approval requirements, and safety rules take precedence. This guidance is not permission to bypass them.""")
if contexts:
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "SessionStart", "additionalContext": "\n\n".join(contexts)
    }}))
request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
transcript_path = hook_input.get("transcript_path")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None
session_start_source = hook_input.get("source") if hook_event_name == "SessionStart" else None
if not isinstance(session_start_source, str) or not session_start_source:
    session_start_source = None
if agent_session_id:
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": report_seq,
        "agent_session_id": agent_session_id,
        # The reminder and stop-check hooks report this session's turns (`pane.report_turn`).
        "turn_reports": True,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": params,
    }
else:
    raise SystemExit(0)

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass
PY
