#!/usr/bin/env python3
"""Replay coordinator turn ends through the Stop hook's stall check.

The Claude Stop hook (src/integration/assets/claude/herdr-agent-state.sh)
blocks, and logs as `stall_would_block`, a coordinator's stop that leaves its
TODO stalled. This runs that predicate, loaded from the hook itself, over turn
ends that scripts/coordinator_turn_audit.py finds in transcripts, with the
facts the hook would have had rebuilt from the record:

- `background_tasks`: one entry per background task the transcript shows
  running at the end (the audit's `pending`);
- the turn's report, AskUserQuestion and question-like final text;
- the user's last message (a plain "stop" pauses);
- herdr's `todo.runnable_state`: the open items with an id in "Next, in
  order" of the repository's `TODO.md` on its main branch as committed at
  the turn's end. Queue mode, runs and item coordinators are taken as
  absent: a run in progress showed as a background wait in these sessions.

Blocked tool calls and worker obligations are not in the record and count
as absent. `--write-fixtures` stores the facts (no transcript text beyond
an 80-character tail of each `other` or `abandoned` end) so that a test replays them without transcripts;
`--fixtures` replays such a file. `--silent SESSION:ENDED` marks a turn end
found by hand as a silent stop (the session's prefix, the end's UTC time
as the audit prints it, to the minute); the report counts how many of those
the check catches and how many other turn ends it would block.

    scripts/coordinator_stall_replay.py [paths ...] [--since T] [--until T]
        [--sessions PREFIX,...] [--silent SESSION:ENDED ...] [--json]
        [--write-fixtures PATH | --fixtures PATH]
"""

import argparse
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import coordinator_turn_audit as audit  # noqa: E402
from awaiting_reply_audit import looks_like_question  # noqa: E402

HOOK = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "src",
    "integration",
    "assets",
    "claude",
    "herdr-agent-state.sh",
)
NEXT_SECTION = "Next, in order"
ITEM_ID = re.compile(r"\[(t-[a-z0-9]{8})\]")


def hook_namespace():
    """The stop check's definitions (everything before it reads its input), from the hook."""
    with open(HOOK, encoding="utf-8") as handle:
        text = handle.read()
    start = text.index("stop_check() {")
    start = text.index("<<'PY'\n", start) + len("<<'PY'\n")
    source = text[start : text.index("\nPY\n", start)]
    main = source.index("try:\n    with open(os.environ[")
    namespace = {}
    exec(source[:main], namespace)  # noqa: S102 - the repository's own script
    return namespace


def next_items(todo):
    """The ids of the open items of "Next, in order", as the driver reads them."""
    items = []
    inside = False
    level = 0
    fence = False
    for line in todo.splitlines():
        if line.startswith("```"):
            fence = not fence
        if fence:
            continue
        heading = re.match(r"^(#+)\s+(.*)$", line)
        if heading:
            if inside and len(heading.group(1)) <= level:
                break
            if heading.group(2).strip() == NEXT_SECTION:
                inside = True
                level = len(heading.group(1))
            continue
        if inside and line.startswith("- [ ] "):
            found = ITEM_ID.search(line)
            if found:
                items.append(found.group(1))
    return items


class Repository:
    """A repository's `TODO.md` on its main branch at a time, read from git."""

    def __init__(self, cwd):
        self.root = None
        self.branch = None
        self.cache = {}
        if not cwd:
            return
        root = self.git(cwd, "rev-parse", "--show-toplevel")
        if root is None:
            return
        self.root = root.strip()
        for branch in ("master", "main"):
            if self.git(self.root, "rev-parse", "--verify", "-q", branch) is not None:
                self.branch = branch
                break

    @staticmethod
    def git(cwd, *args):
        try:
            done = subprocess.run(["git", "-C", cwd, *args], capture_output=True, text=True)
        except OSError:
            return None
        return done.stdout if done.returncode == 0 else None

    def items_at(self, moment):
        """The runnable items at `moment` (ISO 8601), or None when git cannot tell."""
        if self.root is None or self.branch is None:
            return None
        commit = self.git(self.root, "log", "-1", "--format=%H", "--before=" + moment,
                          self.branch, "--", "TODO.md")
        if not commit or not commit.strip():
            return None
        commit = commit.strip()
        if commit not in self.cache:
            todo = self.git(self.root, "show", commit + ":TODO.md")
            self.cache[commit] = None if todo is None else next_items(todo)
        return self.cache[commit]


def facts_of(turn, items, namespace):
    """The facts the hook would have had at this turn end, without transcript text."""
    return {
        "background_tasks": [None] * turn.pending,
        "reported": turn.ran_report,
        "asked_tool": turn.last_tool == audit.ASK_TOOL,
        "question": looks_like_question(turn.final_text),
        "blocked_calls": False,
        "obligations": False,
        "user_stop": namespace["is_plain_stop"](turn.prompt),
        "role": namespace["COORDINATOR_ROLE"],
        "state": None if items is None else {
            "stalled": bool(items),
            "reason": "%d runnable item(s), the first %s" % (len(items), items[0])
            if items else "no runnable item",
        },
    }


def fixtures_from(sessions, silent):
    """One fixture per turn end: the session, its end, the audit's label and the facts."""
    namespace = hook_namespace()
    fixtures = []
    for session in sessions:
        repository = Repository(session.cwd)
        for turn in session.turns:
            ended = audit.parse_time(turn.ended)
            minute = ended.astimezone(audit.datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M")
            key = "%s:%s" % (session.session_id[:8], minute)
            fixtures.append({
                "session": session.session_id[:8],
                "ended": minute,
                "label": turn.label,
                "silent_stop": key in silent,
                # Only the ends a reader reviews by hand keep a tail of their text.
                "tail": audit.tail_of(turn.final_text, 80) if turn.label in ("other", "abandoned")
                else "",
                "facts": facts_of(turn, repository.items_at(turn.ended), namespace),
            })
    return fixtures


def replay(fixtures, namespace=None):
    """Each fixture with the hook's decision, the streak counted per session as the hook does."""
    namespace = namespace or hook_namespace()
    streaks = {}
    decided = []
    for fixture in fixtures:
        facts = dict(fixture["facts"])
        session = fixture["session"]
        facts["streak"] = streaks.get(session, 0)
        would_block, reason = namespace["stall_decision"](facts)
        if would_block:
            streaks[session] = facts["streak"] + 1
        elif reason != namespace["STALL_CAPPED"]:
            streaks.pop(session, None)
        decided.append(dict(fixture, would_block=would_block, reason=reason))
    return decided


def summary(decided):
    silent = [d for d in decided if d["silent_stop"]]
    caught = [d for d in silent if d["would_block"]]
    false = [d for d in decided if d["would_block"] and not d["silent_stop"]]
    ends = len(decided)
    return {
        "turn_ends": ends,
        "silent_stops": len(silent),
        "caught": len(caught),
        "missed": [d["session"] + " " + d["ended"] for d in silent if not d["would_block"]],
        "false_positives": len(false),
        "false_per_100": round(100 * len(false) / ends, 2) if ends else 0.0,
        "false": [
            {"session": d["session"], "ended": d["ended"], "label": d["label"], "tail": d["tail"]}
            for d in false
        ],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="*", help="transcripts (default: ~/.claude/projects/*/*.jsonl)")
    parser.add_argument("--since", help="only turn ends at or after this ISO 8601 time")
    parser.add_argument("--until", help="only turn ends at or before this ISO 8601 time")
    parser.add_argument("--sessions", help="comma-separated session id prefixes to keep")
    parser.add_argument("--silent", action="append", default=[], help="SESSION:ENDED of a silent stop")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--write-fixtures", help="store the rebuilt facts in this JSON file")
    parser.add_argument("--fixtures", help="replay this fixture file instead of transcripts")
    args = parser.parse_args(argv)
    if args.fixtures:
        with open(args.fixtures, encoding="utf-8") as handle:
            fixtures = json.load(handle)["turn_ends"]
    else:
        bounds = []
        for name in ("since", "until"):
            value = getattr(args, name)
            moment = audit.parse_time(value) if value else None
            if value and moment is None:
                parser.error("--%s: not an ISO 8601 time: %s" % (name, value))
            bounds.append(moment)
        since, until = bounds
        sessions = audit.audit(args.paths or audit.default_paths(), since)
        if args.sessions:
            prefixes = tuple(p for p in args.sessions.split(",") if p)
            sessions = [s for s in sessions if s.session_id.startswith(prefixes)]
        if until is not None:
            for session in sessions:
                session.turns = [t for t in session.turns
                                 if (audit.parse_time(t.ended) or until) <= until]
        fixtures = fixtures_from(sessions, set(args.silent))
        if args.write_fixtures:
            with open(args.write_fixtures + ".tmp", "w", encoding="utf-8") as handle:
                # One turn end per line, so a diff of the file reads by turn end.
                handle.write('{"turn_ends": [\n')
                handle.write(",\n".join(json.dumps(f, ensure_ascii=False) for f in fixtures))
                handle.write("\n]}\n")
            os.replace(args.write_fixtures + ".tmp", args.write_fixtures)
    decided = replay(fixtures)
    report = summary(decided)
    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
        return 0
    print("turn ends: %d" % report["turn_ends"])
    print("silent stops caught: %d of %d" % (report["caught"], report["silent_stops"]))
    for missed in report["missed"]:
        print("    missed: %s" % missed)
    print("false positives: %d (%.2f per 100 turn ends)"
          % (report["false_positives"], report["false_per_100"]))
    for false in report["false"]:
        print("    %s %s [%s] ...%s" % (false["session"], false["ended"], false["label"], false["tail"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
