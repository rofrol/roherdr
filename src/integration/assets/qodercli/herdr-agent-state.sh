#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# HERDR_INTEGRATION_ID=qodercli
# HERDR_INTEGRATION_VERSION=4

[ "${1:-}" = "session" ] || exit 0
[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# Silent on every path; a report herdr does not accept is recorded in
# hook-report-failures.jsonl instead.
python3 -c '
import json
import os
import random
import socket
import sys
import time

HOOK = "qodercli"
METHOD = "pane.report_agent_session"
pane_id = os.environ["HERDR_PANE_ID"]


def record_failure(error):
    """Appends one line for a report herdr did not accept; a line it cannot write is skipped."""
    try:
        state_dir = os.path.join(
            os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"), "herdr"
        )
        os.makedirs(state_dir, exist_ok=True)
        path = os.path.join(state_dir, "hook-report-failures.jsonl")
        try:
            if os.path.getsize(path) > 1_000_000:
                os.replace(path, path + ".1")
        except OSError:
            pass
        entry = {
            "time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "hook": HOOK,
            "pane": pane_id,
            "method": METHOD,
            "error": error,
        }
        with open(path, "a", encoding="utf-8") as log:
            log.write(json.dumps(entry) + "\n")
    except Exception:
        pass


def report(params):
    """Sends one request to the herdr server and reads its reply."""
    request = {
        "id": "herdr:%s:%d:%06d" % (HOOK, time.time_ns() // 1_000_000, random.randrange(1_000_000)),
        "method": METHOD,
        "params": params,
    }
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            # delay: external deadline (round 20261023-142004-c534). The agent runs this
            # hook synchronously, so 0.5 s bounds each socket step against a hung server;
            # it is not a delivery policy.
            client.settimeout(0.5)
            client.connect(os.environ["HERDR_SOCKET_PATH"])
            client.sendall((json.dumps(request) + "\n").encode())
            reply = b""
            while b"\n" not in reply and len(reply) < 65536:
                chunk = client.recv(4096)
                if not chunk:
                    break
                reply += chunk
        finally:
            client.close()
    except Exception as error:
        record_failure(type(error).__name__)
        return
    if not reply:
        record_failure("no_reply")
        return
    try:
        answer = json.loads(reply.split(b"\n", 1)[0])
    except ValueError:
        record_failure("unreadable_reply")
        return
    if not isinstance(answer, dict):
        record_failure("unreadable_reply")
    elif "error" in answer:
        code = answer["error"].get("code") if isinstance(answer["error"], dict) else None
        record_failure("error_reply:%s" % code if isinstance(code, str) else "error_reply")


try:
    payload = json.load(sys.stdin)
    session_id = payload.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise ValueError
    params = {
        "pane_id": pane_id,
        "source": "herdr:qodercli",
        "agent": "qodercli",
        "seq": time.time_ns(),
        "agent_session_id": session_id,
    }
except Exception:
    params = None

if params is not None:
    report(params)
' 2>/dev/null || true
