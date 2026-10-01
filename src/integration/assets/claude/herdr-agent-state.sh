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
