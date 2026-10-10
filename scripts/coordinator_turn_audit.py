#!/usr/bin/env python3
"""Audit where TODO coordinators end their turns.

A TODO coordinator is a Claude Code session ordered to work through a
repository's TODO ("rób TODO po kolei", "work through the TODO", the `/todo`
skill) or one that renamed its pane `todo-<repo>`. While approved items
remain, it should keep handing them to workers; it should end a turn only to
ask the user something or to wait for a worker. This reads Claude Code
transcripts (JSONL) and labels every turn end after the order:

- asked: the turn's last tool call was AskUserQuestion, the turn ran
  `herdr agent awaiting-reply`, or its final text asks a real question
  (the heuristic of awaiting_reply_audit.py);
- waiting: a background task (a `run_in_background` command such as
  `herdr-job wait`, or anything a task notification later reports on) was
  still running when the turn ended;
- abandoned: the final text promises to go on later or asks for permission
  to continue ("gdy powiesz dalej", "should I continue?"), and neither of the
  above holds;
- other: everything else (reports, finished queues).

Precedence: a tool-based ask, then waiting, then abandoned, then a
question-like final text, then other. A question asking permission to
continue therefore counts as abandoned unless it went through a tool.

A turn ends whenever the session goes idle: before the next user prompt or
task notification, or at the transcript's end. The labels are a screen for
review, not ground truth; each abandoned turn is listed with the tail of its
final text (and, in JSON, its head). Read only; nothing is sent anywhere, and
nothing beyond those excerpts is printed.

`--since` counts only turn ends at or after a time (ISO 8601; without an
offset it is local time), so a rule change can be measured on the turns that
followed it even in a session that started before it.

    scripts/coordinator_turn_audit.py [paths ...] [--json] [--tail 200] [--since TIME]
"""

import argparse
import collections
import datetime
import glob
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from awaiting_reply_audit import (  # noqa: E402
    INTERRUPT,
    is_report,
    last_paragraph,
    looks_like_question,
    strip_code,
    text_of,
)

LABELS = ("asked", "waiting", "abandoned", "other")

# A user prompt that orders working through the TODO: the order opens the
# message (a message that quotes it while discussing the rule is no order),
# or the coordinator skill runs (`/todo`, called `/todo-worker` before
# 2026-10-07).
TODO_ORDER = re.compile(
    r"(\A\s*(?:r[óo]b\s+todo\s+po\s+kolei|work\s+through\s+the\s+todo)"
    r"|<command-name>/todo(?:-worker)?</command-name>|\A\s*/todo(?:-worker)?(?:\s|\Z))",
    re.IGNORECASE,
)
# A shell command that names the pane as a coordinator (`todo-<repo>`).
TODO_RENAME = re.compile(r"herdr\s+agent\s+rename\s+\S+\s+[\"']?todo-(?!worker\b)[A-Za-z0-9]")
# The launcher the skill runs to start a coordinator in another space: the
# session that runs it hands the TODO over and does not coordinate.
LAUNCHER = re.compile(r"(?:^|[\s;&|])(?:~|\S*)/scripts/todo-worker\s+[\"']", re.MULTILINE)
# Cheap byte screen before anything is decoded or parsed (a regex over every
# transcript costs seconds; these finds cost milliseconds).
PREFILTER = (
    b"po kolei",
    b"through the TODO",
    b"through the todo",
    b"<command-name>/todo",
    b"agent rename",
)

# Final text that defers the next step to the user's go-ahead.
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
# The report run as a command (not merely mentioned in a file or a grep).
REPORT_RUN = re.compile(r"(?:^|[;&|]|\n)\s*herdr agent awaiting-reply(?:\s+[\"']|\s*$)", re.MULTILINE)
NOTIFICATION_ID = re.compile(r"<tool-use-id>([^<\s]+)</tool-use-id>")


def is_abandon_text(text):
    """True when the end of the final text waits for the user's go-ahead."""
    tail = strip_code(text).strip()[-600:]
    return bool(ABANDON.search(tail))


class TurnEnd:
    def __init__(self, started, prompt=""):
        self.started = started
        self.prompt = prompt  # the user's last message (task notifications are none)
        self.ended = started  # the last assistant entry's time
        self.final_text = ""
        self.last_tool = ""
        self.ran_report = False
        self.pending = 0  # background tasks running at the end
        self.label = ""


class Session:
    def __init__(self, path):
        self.path = path
        self.session_id = os.path.splitext(os.path.basename(path))[0]
        self.ordered_at = ""
        self.cwd = ""  # the session's first working directory
        self.turns = []

    def since(self, moment):
        """Keep only the turn ends at or after `moment` (an aware datetime)."""
        kept = []
        for turn in self.turns:
            ended = parse_time(turn.ended)
            if ended is not None and ended >= moment:
                kept.append(turn)
        self.turns = kept

    def counts(self):
        counter = collections.Counter({label: 0 for label in LABELS})
        counter.update(turn.label for turn in self.turns)
        return counter


def parse_time(text):
    """An aware datetime from an ISO 8601 string (local time without an offset), or None."""
    try:
        moment = datetime.datetime.fromisoformat(text.strip())
    except (AttributeError, ValueError):
        return None
    return moment if moment.tzinfo else moment.astimezone()


def label(turn):
    if turn.last_tool == ASK_TOOL or turn.ran_report:
        return "asked"
    if turn.pending:
        return "waiting"
    if is_abandon_text(turn.final_text):
        return "abandoned"
    if looks_like_question(turn.final_text):
        return "asked"
    return "other"


def user_prompt(entry, message):
    """(text, is_notification) for a prompt that starts a turn, else None."""
    content = message.get("content")
    if isinstance(content, list) and any(
        isinstance(b, dict) and b.get("type") == "tool_result" for b in content
    ):
        return None
    text = text_of(content).strip()
    if not text or text.startswith(INTERRUPT):
        return None
    origin = entry.get("origin") or {}
    notification = origin.get("kind") == "task-notification" or text.startswith(
        "<task-notification>"
    )
    return text, notification


def parse_session(path, lines):
    """Turn ends after the TODO order, or None when the session is no coordinator."""
    session = Session(path)
    background = set()  # tool_use ids that run in the background
    finished = set()  # their ids once a notification reported them
    ordered = False
    launcher = False
    current = None
    seen = set()
    last_prompt = ""

    def close():
        if current is not None and ordered and (current.final_text or current.last_tool):
            current.pending = len(background - finished)
            current.label = label(current)
            session.turns.append(current)

    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if entry.get("isSidechain"):
            continue
        if not session.cwd and isinstance(entry.get("cwd"), str):
            session.cwd = entry["cwd"]
        uuid = entry.get("uuid")
        if uuid:
            if uuid in seen:
                continue
            seen.add(uuid)
        kind = entry.get("type")
        if kind == "attachment":
            attachment = entry.get("attachment") or {}
            if attachment.get("type") == "queued_command":
                finished.update(NOTIFICATION_ID.findall(str(attachment.get("prompt", ""))))
            continue
        message = entry.get("message") or {}
        if kind == "user" and not entry.get("isMeta"):
            prompt = user_prompt(entry, message)
            if prompt is None:
                continue
            text, notification = prompt
            # The previous turn ended before this prompt arrived.
            close()
            if notification:
                finished.update(NOTIFICATION_ID.findall(text))
            if not notification and TODO_ORDER.search(text) and not ordered:
                ordered = True
                session.ordered_at = str(entry.get("timestamp") or "")
            if not notification:
                last_prompt = text
            current = TurnEnd(str(entry.get("timestamp") or ""), last_prompt)
        elif kind == "assistant" and current is not None:
            if message.get("model") == "<synthetic>":
                continue
            if entry.get("timestamp"):
                current.ended = str(entry["timestamp"])
            for block in message.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use":
                    name = block.get("name", "")
                    args = block.get("input") or {}
                    current.last_tool = name
                    if isinstance(args, dict) and args.get("run_in_background"):
                        background.add(block.get("id"))
                    if name == "Bash":
                        command = args.get("command", "") if isinstance(args, dict) else ""
                        if is_report(command) or REPORT_RUN.search(command):
                            current.ran_report = True
                        if LAUNCHER.search(command):
                            launcher = True
                        if TODO_RENAME.search(command) and not ordered:
                            ordered = True
                            session.ordered_at = str(entry.get("timestamp") or "")
                elif block.get("type") == "text" and block.get("text", "").strip():
                    current.final_text = block["text"]
    close()
    return session if ordered and not launcher else None


def load_session(path):
    try:
        with open(path, "rb") as handle:
            data = handle.read()
    except OSError:
        return None
    if not any(marker in data for marker in PREFILTER):
        return None
    return parse_session(path, data.decode("utf-8", errors="replace").splitlines())


def audit(paths, since=None):
    sessions = [s for s in (load_session(p) for p in paths) if s is not None]
    if since is None:
        return sessions
    for session in sessions:
        session.since(since)
    return [s for s in sessions if s.turns]


def default_paths():
    return sorted(glob.glob(os.path.expanduser("~/.claude/projects/*/*.jsonl")))


def tail_of(text, size):
    return last_paragraph(text)[-size:].replace("\n", " ")


def head_of(text, size):
    return text.strip()[:size].replace("\n", " ")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="*", help="transcripts (default: ~/.claude/projects/*/*.jsonl)")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--tail", type=int, default=200, help="characters of each abandoned tail")
    parser.add_argument("--since", help="count only turn ends at or after this ISO 8601 time")
    args = parser.parse_args(argv)
    since = None
    if args.since:
        since = parse_time(args.since)
        if since is None:
            parser.error("--since: not an ISO 8601 time: %s" % args.since)
    sessions = audit(args.paths or default_paths(), since)
    totals = collections.Counter({label: 0 for label in LABELS})
    for session in sessions:
        totals.update(session.counts())
    ends = sum(totals.values())
    if args.json:
        report = {
            "sessions": [
                {
                    "session": s.session_id,
                    "path": s.path,
                    "ordered_at": s.ordered_at,
                    "counts": dict(s.counts()),
                    "abandoned": [
                        {
                            "started": t.started,
                            "ended": t.ended,
                            "head": head_of(t.final_text, args.tail),
                            "tail": tail_of(t.final_text, args.tail),
                        }
                        for t in s.turns
                        if t.label == "abandoned"
                    ],
                }
                for s in sessions
            ],
            "totals": dict(totals),
            "coordinator_sessions": len(sessions),
            "turn_ends": ends,
        }
        print(json.dumps(report, indent=2, ensure_ascii=False))
        return 0
    print("%-38s %-17s %5s %7s %9s %5s" % ("session", "ordered", "asked", "waiting", "abandoned", "other"))
    for s in sessions:
        c = s.counts()
        print(
            "%-38s %-17s %5d %7d %9d %5d"
            % (s.session_id, s.ordered_at[:16], c["asked"], c["waiting"], c["abandoned"], c["other"])
        )
    print(
        "%-38s %-17s %5d %7d %9d %5d"
        % ("TOTAL (%d sessions)" % len(sessions), "", totals["asked"], totals["waiting"],
           totals["abandoned"], totals["other"])
    )
    if ends:
        print("abandoned per 100 turn ends: %.1f (%d of %d)"
              % (100 * totals["abandoned"] / ends, totals["abandoned"], ends))
    for s in sessions:
        for t in s.turns:
            if t.label == "abandoned":
                print("\n%s %s\n    ...%s" % (s.session_id, t.started[:16], tail_of(t.final_text, args.tail)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
