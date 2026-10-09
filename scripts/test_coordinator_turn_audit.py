#!/usr/bin/env python3
"""Tests for coordinator_turn_audit.py on synthetic transcripts."""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import coordinator_turn_audit as audit  # noqa: E402

ORDER = "Rób TODO po kolei."


class Lines:
    """Builds a Claude Code transcript one entry at a time."""

    def __init__(self):
        self.lines = []
        self.count = 0

    def add(self, kind, content, **extra):
        self.count += 1
        entry = {
            "type": kind,
            "uuid": "e%d" % self.count,
            "timestamp": "2026-10-07T00:%02d:00Z" % self.count,
            "message": {"role": kind, "content": content, "model": "m1"},
        }
        entry.update(extra)
        self.lines.append(json.dumps(entry))
        return self

    def user(self, text):
        return self.add("user", text, origin={"kind": "human"})

    def notify(self, tool_id):
        text = "<task-notification>\n<tool-use-id>%s</tool-use-id>\n</task-notification>" % tool_id
        return self.add("user", text, origin={"kind": "task-notification"})

    def tools(self, *blocks):
        self.add("assistant", list(blocks))
        results = [
            {"type": "tool_result", "tool_use_id": b["id"], "content": "ok"} for b in blocks
        ]
        return self.add("user", results)

    def say(self, text):
        return self.add("assistant", [{"type": "text", "text": text}])


def bash(tool_id, command, background=False):
    args = {"command": command}
    if background:
        args["run_in_background"] = True
    return {"type": "tool_use", "id": tool_id, "name": "Bash", "input": args}


def ask(tool_id):
    return {"type": "tool_use", "id": tool_id, "name": "AskUserQuestion", "input": {}}


class Labels(unittest.TestCase):
    def write(self, lines):
        handle = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        handle.write("\n".join(lines.lines) + "\n")
        handle.close()
        self.addCleanup(os.unlink, handle.name)
        return handle.name

    def labels(self, lines):
        session = audit.load_session(self.write(lines))
        self.assertIsNotNone(session)
        return [turn.label for turn in session.turns]

    def test_ask_user_question_as_last_tool_is_asked(self):
        lines = Lines().user(ORDER).tools(bash("t1", "cat TODO.md"), ask("t2"))
        lines.say("Two decisions are open.")
        self.assertEqual(self.labels(lines), ["asked"])

    def test_awaiting_reply_report_is_asked_but_a_mention_is_not(self):
        lines = Lines().user(ORDER).tools(bash("t1", 'herdr agent awaiting-reply "2 decisions"'))
        lines.say("Two decisions wait for you.")
        lines.user("ok").tools(bash("t2", "grep -n 'herdr agent awaiting-reply' AGENTS.md"))
        lines.say("Updated the notes.")
        self.assertEqual(self.labels(lines), ["asked", "other"])

    def test_deferring_to_the_users_go_ahead_is_abandoned(self):
        lines = Lines().user(ORDER).tools(bash("t1", "cat TODO.md"))
        lines.say(
            "Następna w kolejce jest „Hand a session over”. "
            "Zlecę ją pracownikowi, gdy powiesz „dalej”."
        )
        lines.user("why?").say("Because.\n\nShould I continue with the next item?")
        lines.user("and?").say("Done for now; let me know when I can go on.")
        self.assertEqual(self.labels(lines), ["abandoned", "abandoned", "abandoned"])

    def test_a_running_background_wait_is_waiting_until_it_reports(self):
        lines = Lines().user(ORDER)
        lines.tools(bash("t1", "herdr-job wait 20261007-010203-abcd", background=True))
        lines.say("The worker runs; its wait wakes me when it ends.")
        lines.notify("t1").tools(bash("t2", "git log -1"))
        lines.say("Brought the worker's commit in. The queue is empty.")
        self.assertEqual(self.labels(lines), ["waiting", "other"])

    def test_a_notification_mid_turn_finishes_the_wait(self):
        lines = Lines().user(ORDER)
        lines.tools(bash("t1", "herdr agent wait w1:p2", background=True))
        lines.add(
            "attachment",
            None,
            attachment={
                "type": "queued_command",
                "prompt": "<task-notification><tool-use-id>t1</tool-use-id>",
            },
        )
        lines.say("All items are done.")
        self.assertEqual(self.labels(lines), ["other"])

    def test_a_question_is_asked_and_a_report_is_other(self):
        lines = Lines().user(ORDER).say("Which variant do you prefer, A or B?")
        lines.user("A").say("Committed variant A; every item is done.")
        self.assertEqual(self.labels(lines), ["asked", "other"])

    def test_turns_before_the_order_are_not_counted(self):
        lines = Lines().user("hi").say("Hello. Should I continue later?")
        lines.user(ORDER).say("Finished the queue.")
        self.assertEqual(self.labels(lines), ["other"])

    def test_a_rename_to_todo_marks_a_coordinator(self):
        lines = Lines().user("go").tools(bash("t1", 'herdr agent rename "$HERDR_PANE_ID" todo-herdr'))
        lines.say("Zlecę ją, gdy powiesz dalej.")
        self.assertEqual(self.labels(lines), ["abandoned"])

    def test_sessions_without_a_todo_order_are_ignored(self):
        for lines in [
            Lines().user("fix the bug").say("Fixed. Should I continue?"),
            # Quoting the order while discussing the rule is not an order.
            Lines().user('"rób todo po kolei" albo "koordynator"? pytaj modeli').say("Both."),
            # A worker named todo-worker, and the documented placeholder.
            Lines().user("x").tools(bash("t1", "herdr agent rename p1 todo-worker")).say("ok"),
            Lines().user("x").tools(bash("t1", "echo 'herdr agent rename p todo-<repo>'")).say("ok"),
            # The skill that only launched a coordinator in another space.
            Lines()
            .user("<command-name>/todo</command-name>")
            .tools(bash("t1", '~/scripts/todo-worker "rormpc" 2>&1 | tail -30'))
            .say("Started the coordinator in rormpc."),
        ]:
            self.assertIsNone(audit.load_session(self.write(lines)))

    def test_the_todo_skill_coordinating_here_counts(self):
        lines = Lines().user("<command-name>/todo</command-name>").say("Queue empty; done.")
        self.assertEqual(self.labels(lines), ["other"])


class Since(unittest.TestCase):
    def write(self, lines):
        handle = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        handle.write("\n".join(lines.lines) + "\n")
        handle.close()
        self.addCleanup(os.unlink, handle.name)
        return handle.name

    def report(self, *argv):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            audit.main(list(argv) + ["--json"])
        return json.loads(out.getvalue())

    def test_only_turn_ends_at_or_after_the_time_count(self):
        # Turn ends at 00:02 (order at 00:01), 00:04 and 00:06.
        lines = Lines().user(ORDER).say("Zlecę ją, gdy powiesz dalej.")
        lines.user("why?").say("Should I continue?")
        lines.user("go").say("Queue empty; done.")
        path = self.write(lines)

        everything = self.report(path)
        self.assertEqual(everything["turn_ends"], 3)

        later = self.report(path, "--since", "2026-10-07T00:04:00Z")
        self.assertEqual(later["turn_ends"], 2)
        self.assertEqual(later["totals"], {"asked": 0, "waiting": 0, "abandoned": 1, "other": 1})
        abandoned = later["sessions"][0]["abandoned"]
        self.assertEqual([a["ended"] for a in abandoned], ["2026-10-07T00:04:00Z"])
        self.assertEqual(abandoned[0]["head"], "Should I continue?")

        # An offset is honoured: 02:05+02:00 is 00:05Z.
        self.assertEqual(self.report(path, "--since", "2026-10-07T02:05:00+02:00")["turn_ends"], 1)
        # A session with no turn end left is not counted at all.
        none = self.report(path, "--since", "2026-10-08T00:00:00Z")
        self.assertEqual((none["coordinator_sessions"], none["turn_ends"]), (0, 0))

    def test_a_bad_time_is_refused(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            audit.main(["--since", "yesterday", "--json"])


class Output(unittest.TestCase):
    def test_table_json_and_tails(self):
        lines = Lines().user(ORDER).tools(bash("t1", "cat TODO.md"))
        lines.say("Status report.\n\n" + "x" * 300 + " Zlecę ją pracownikowi, gdy powiesz „dalej”.")
        handle = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        handle.write("\n".join(lines.lines) + "\n")
        handle.close()
        self.addCleanup(os.unlink, handle.name)

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            audit.main([handle.name, "--json"])
        report = json.loads(out.getvalue())
        self.assertEqual(report["coordinator_sessions"], 1)
        self.assertEqual(report["totals"], {"asked": 0, "waiting": 0, "abandoned": 1, "other": 0})
        tail = report["sessions"][0]["abandoned"][0]["tail"]
        self.assertEqual(len(tail), 200)
        self.assertTrue(tail.endswith("„dalej”."))

        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            audit.main([handle.name])
        self.assertIn("TOTAL (1 sessions)", out.getvalue())
        self.assertIn("abandoned per 100 turn ends: 100.0 (1 of 1)", out.getvalue())
        self.assertNotIn("Status report", out.getvalue())


if __name__ == "__main__":
    unittest.main()
