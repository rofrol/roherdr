#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=11

set -eu

action="${1:-}"

# Repeats the awaiting-reply instruction on every prompt, since the SessionStart context is far
# back in a long session. Printed as is: it needs no hook input and no socket.
if [ "$action" = "reminder" ]; then
  cat >/dev/null 2>&1 || true
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  [ "${HERDR_AWAITING_REPLY_INSTRUCTIONS:-1}" != "0" ] || exit 0
  printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"Herdr reminder: if you end this turn needing the user'"'"'s answer or decision before you can continue (a question, a choice, a confirmation, or a request to check something first, even without a question mark), run `herdr agent awaiting-reply` on its own as the last command of the turn, right before your final message. Not for AskUserQuestion or courtesy offers."}}'
  exit 0
fi

hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

# Stop hook: when the turn ends with a question for the user and the agent did not report it with
# `herdr agent awaiting-reply`, ask it once to do so. The agent decides (a rhetorical question is
# not reported); herdr never marks the pane itself. HERDR_AWAITING_REPLY_STOP=0 turns it off,
# =shadow only logs what it would have done to ~/.local/state/herdr/awaiting-reply-stop.jsonl.
if [ "$action" = "stop-check" ]; then
  [ "${HERDR_ENV:-}" = "1" ] || exit 0
  [ -n "${HERDR_PANE_ID:-}" ] || exit 0
  [ -z "${CURSOR_VERSION:-}" ] || exit 0
  [ "${HERDR_AWAITING_REPLY_INSTRUCTIONS:-1}" != "0" ] || exit 0
  [ "${HERDR_AWAITING_REPLY_STOP:-block}" != "0" ] || exit 0
  command -v python3 >/dev/null 2>&1 || exit 0
  HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import re
import time

mode = os.environ.get("HERDR_AWAITING_REPLY_STOP", "block")
COMMAND = "herdr agent awaiting-reply"
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


def text_of(content):
    if isinstance(content, str):
        return content
    return "\n".join(
        block.get("text", "")
        for block in content or []
        if isinstance(block, dict) and block.get("type") == "text"
    )


try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        hook_input = json.loads(handle.read() or "{}")
except Exception:
    raise SystemExit(0)
if hook_input.get("hook_event_name") != "Stop" or hook_input.get("stop_hook_active"):
    raise SystemExit(0)
if hook_input.get("agent_id"):
    raise SystemExit(0)

final_text = hook_input.get("last_assistant_message")
final_text = final_text if isinstance(final_text, str) else ""
reported = False
last_text = ""
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
        elif entry.get("type") == "assistant":
            for block in message.get("content") or []:
                if (
                    isinstance(block, dict)
                    and block.get("type") == "tool_use"
                    and block.get("name") == "Bash"
                    and (block.get("input") or {}).get("command", "").strip() == COMMAND
                ):
                    reported = True
            text = text_of(message.get("content")).strip()
            if text:
                last_text = text
if not final_text.strip():
    final_text = last_text

question = looks_like_question(final_text)
block = question and not reported
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
                    "blocked": block and mode != "shadow",
                    "tail": last_paragraph(final_text)[-200:],
                },
                ensure_ascii=False,
            )
            + "\n"
        )
except OSError:
    pass
if block and mode != "shadow":
    print(
        json.dumps(
            {
                "decision": "block",
                "reason": (
                    "Herdr: your last message looks like a question for the user, but you did "
                    "not run `herdr agent awaiting-reply`. If you are waiting for the user's "
                    "answer or decision, run `herdr agent awaiting-reply` now as the only "
                    "command, then stop without repeating your message. If you are not "
                    "waiting for the user, just stop."
                ),
            }
        )
    )
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
                "`herdr agent awaiting-reply` on its own, as the last command of the turn, "
                "right before your final message, so Herdr keeps your pane marked until the "
                "user replies. This covers a plain-text question, a choice between options, a "
                "confirmation before you proceed, and a request to check something before you "
                "go on (\"let me know how it looks, then I will commit\"), even without a "
                "question mark. Never append it to another command, never run it earlier in the "
                "turn, and never run it for AskUserQuestion or any other question tool or "
                "prompt answered inside the turn: Herdr already shows those as blocked. Run "
                "it at most once per turn and ignore its failure. Do not run it when you "
                "simply finished and ask nothing, or for courtesy offers such as asking "
                "whether anything else is needed."
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
