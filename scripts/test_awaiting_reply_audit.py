#!/usr/bin/env python3
"""Tests for awaiting_reply_audit.py on synthetic transcripts."""

import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import awaiting_reply_audit as audit  # noqa: E402
import coordinator_stall_replay as replay  # noqa: E402
import coordinator_turn_audit as coordinator  # noqa: E402
from test_coordinator_turn_audit import ORDER, Lines  # noqa: E402
from test_coordinator_turn_audit import ask as coordinator_ask  # noqa: E402
from test_coordinator_turn_audit import bash as coordinator_bash  # noqa: E402


def claude_line(kind, content, **extra):
    entry = {"type": kind, "message": {"role": kind, "content": content, "model": "m1"}}
    entry.update(extra)
    return json.dumps(entry)


def bash(command):
    return {"type": "tool_use", "name": "Bash", "input": {"command": command}}


def text(value):
    return {"type": "text", "text": value}


def tool_use(tool_id, command="ls"):
    return {"type": "tool_use", "id": tool_id, "name": "Bash", "input": {"command": command}}


def tool_result(tool_id, is_error):
    return {"type": "tool_result", "tool_use_id": tool_id, "content": "x", "is_error": is_error}


class QuestionHeuristic(unittest.TestCase):
    def test_questions_in_both_languages(self):
        for message in [
            "Done.\n\nShould I push the commits?",
            "Zrobione.\n\nWypchnąć commity?",
            "Opcje:\n1. a\n2. b\n\nCo wybierasz?",
            "Let me know which option you prefer, A or B?",
        ]:
            self.assertTrue(audit.looks_like_question(message), message)

    def test_statements_code_quotes_and_courtesy_are_not_questions(self):
        for message in [
            "Done. All tests pass.",
            "```\nwhy?\n```\n\nFixed.",
            "> quoted: ok?\n\nFinished.",
            "Finished.\n\nAnything else?",
            "Skończone. Coś jeszcze?",
            "",
        ]:
            self.assertFalse(audit.looks_like_question(message), message)


class Turns(unittest.TestCase):
    def write(self, lines):
        handle = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False)
        handle.write("\n".join(lines) + "\n")
        handle.close()
        self.addCleanup(os.unlink, handle.name)
        return handle.name

    def test_claude_turn_report_and_miss(self):
        path = self.write(
            [
                claude_line("user", "first", uuid="u1", timestamp="2026-10-01T10:00"),
                claude_line("assistant", [bash("herdr agent awaiting-reply"), text("Push?")],
                            uuid="a1"),
                # A tool result is not a new turn.
                claude_line("user", [{"type": "tool_result", "content": "x"}], uuid="u2"),
                claude_line("user", "second", uuid="u3", timestamp="2026-10-01T11:00"),
                claude_line("assistant", [text("Install it now?")], uuid="a2"),
                # Sidechains and duplicates are ignored.
                claude_line("assistant", [text("Sub?")], uuid="a3", isSidechain=True),
                claude_line("assistant", [text("Install it now?")], uuid="a2"),
            ]
        )
        stats, misses = audit.audit([path])
        s = stats["m1"]
        self.assertEqual((s["turns"], s["question_like"], s["reported"]), (2, 2, 1))
        self.assertEqual(s["missed"], 1)
        self.assertEqual(s["order_violation"], 0)
        self.assertIn("Install it now?", misses["m1"][0])

    def test_since_skips_older_turns(self):
        path = self.write(
            [
                claude_line("user", "old", uuid="u1", timestamp="2026-09-01T10:00"),
                claude_line("assistant", [text("Old?")], uuid="a1"),
            ]
        )
        stats, _ = audit.audit([path], since="2026-10-01")
        self.assertEqual(sum(s["turns"] for s in stats.values()), 0)

    def test_pi_turns_use_the_message_model_and_flag_order_violations(self):
        def message(role, content, **extra):
            body = {"role": role, "content": content}
            body.update(extra)
            return json.dumps({"type": "message", "message": body, "timestamp": "2026-10-02"})

        path = self.write(
            [
                json.dumps({"type": "session", "id": "s"}),
                message("user", [text("go")]),
                message(
                    "assistant",
                    [
                        {"type": "toolCall", "name": "bash",
                         "arguments": {"command": "herdr agent awaiting-reply"}},
                        {"type": "toolCall", "name": "bash", "arguments": {"command": "ls"}},
                        text("Which one do you want?"),
                    ],
                    provider="deepseek",
                    model="flash",
                ),
            ]
        )
        stats, _ = audit.audit([path])
        s = stats["deepseek/flash"]
        self.assertEqual((s["turns"], s["reported"], s["order_violation"]), (1, 1, 1))


class Wilson(unittest.TestCase):
    def test_interval_brackets_the_rate(self):
        lo, hi = audit.wilson(5, 10)
        self.assertLess(lo, 0.5)
        self.assertGreater(hi, 0.5)
        self.assertEqual(audit.wilson(0, 0), (0.0, 0.0))


HOOK = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "src",
    "integration",
    "assets",
    "claude",
    "herdr-agent-state.sh",
)


FIXTURES = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "fixtures", "coordinator_stall_2026-10-09.json"
)


def hook_source(marker="stop_check() {"):
    with open(HOOK, encoding="utf-8") as handle:
        text = handle.read()
    start = text.index(marker)
    start = text.index("<<'PY'\n", start) + len("<<'PY'\n")
    return text[start : text.index("\nPY\n", start)]


def pre_tool_namespace():
    """The pre-tool hook's definitions (everything before it reads its input)."""
    source = hook_source('if [ "$action" = "pre-tool" ]; then')
    main = source.index("try:\n    with open(os.environ[")
    namespace = {}
    exec(source[:main], namespace)  # noqa: S102 - the repository's own script
    return namespace


@unittest.skipIf(os.name == "nt", "the hook script runs under a POSIX shell")
class StopHook(unittest.TestCase):
    """The Claude Stop hook asks once for a final question that was not reported."""

    def test_its_question_heuristic_equals_the_audits(self):
        source = hook_source()
        main = source.index("try:\n    with open(os.environ[")
        namespace = {}
        exec(source[:main], namespace)  # noqa: S102 - the repository's own script
        for message in [
            "Done.\n\nShould I push the commits?",
            "Wypchnąć commity?",
            "Finished.\n\nAnything else?",
            "```\nwhy?\n```\n\nFixed.",
            "> quoted?\n\nFinished.",
            "Let me know which option you prefer, A or B?",
            "Done. All tests pass.",
        ]:
            self.assertEqual(
                namespace["looks_like_question"](message),
                audit.looks_like_question(message),
                message,
            )
        for message in [
            "Czekam?\n\nherdr agent awaiting-reply",
            "Czekam?\n\n`herdr agent awaiting-reply`",
            "Czekam?\n\n```\n$ herdr agent awaiting-reply\n```",
            "Explaining: the command herdr agent awaiting-reply marks a pane.",
        ]:
            self.assertEqual(
                namespace["printed_command"](message),
                audit.printed_command(message),
                message,
            )

    def test_blocks_once_when_the_command_is_printed_not_run(self):
        entries = [
            {"type": "user", "message": {"role": "user", "content": "go"}},
            {"type": "assistant", "message": {"role": "assistant", "content": [
                text("Masz email?\n\nherdr agent awaiting-reply")]}},
        ]
        output = json.loads(self.run_hook(entries))
        self.assertEqual(output["decision"], "block")
        self.assertIn("Call the Bash tool", output["reason"])
        # Run properly, the same ending passes.
        ran = [entries[0], {"type": "assistant", "message": {"role": "assistant", "content": [
            bash("herdr agent awaiting-reply"), text("Masz email?\n\nherdr agent awaiting-reply")]}}]
        self.assertEqual(self.run_hook(ran), "")

    def run_hook(self, entries, reports=None, stop_mode=None, **fields):
        """Runs the hook on a transcript; with a `reports` list, also gives it a herdr socket
        and appends the requests it sent there."""
        import socket
        import subprocess

        with tempfile.TemporaryDirectory() as directory:
            transcript = os.path.join(directory, "t.jsonl")
            with open(transcript, "w", encoding="utf-8") as handle:
                handle.write("\n".join(json.dumps(e) for e in entries) + "\n")
            payload = {"hook_event_name": "Stop", "transcript_path": transcript}
            payload.update(fields)
            env = dict(
                os.environ,
                HERDR_ENV="1",
                HERDR_PANE_ID="p1",
                XDG_STATE_HOME=os.path.join(directory, "state"),
            )
            env.pop("CURSOR_VERSION", None)
            env.pop("HERDR_SOCKET_PATH", None)
            env.pop("HERDR_AWAITING_REPLY_STOP", None)
            if stop_mode:
                env["HERDR_AWAITING_REPLY_STOP"] = stop_mode
            server = None
            if reports is not None:
                path = os.path.join(directory, "s.sock")
                server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                server.bind(path)
                server.listen(4)
                env["HERDR_SOCKET_PATH"] = path
            try:
                result = subprocess.run(
                    ["sh", HOOK, "stop-check"],
                    input=json.dumps(payload),
                    text=True,
                    capture_output=True,
                    env=env,
                    check=True,
                )
                # The hook has exited, so whatever it sent waits in the backlog.
                server and server.setblocking(False)
                while server:
                    try:
                        connection, _ = server.accept()
                    except BlockingIOError:
                        break
                    connection.settimeout(1)
                    report = json.loads(connection.recv(65536).decode())
                    # Turn reports have their own tests (tests/cli/hooks.rs).
                    if report.get("method") != "pane.report_turn":
                        reports.append(report)
                    connection.close()
            finally:
                server and server.close()
            return result.stdout.strip()

    def blocked_turn(self, final, *results):
        """A turn whose last batch of tool calls ended with these `is_error` results."""
        entries = [
            {"type": "user", "message": {"role": "user", "content": "go"}},
            {"type": "assistant", "message": {"role": "assistant", "content": [tool_use("a0")]}},
            {"type": "user", "message": {"role": "user", "content": [tool_result("a0", False)]}},
        ]
        for index, _ in enumerate(results):
            entries.append({"type": "assistant", "message": {
                "role": "assistant", "content": [tool_use(f"b{index}")]}})
        for index, is_error in enumerate(results):
            entries.append({"type": "user", "message": {
                "role": "user", "content": [tool_result(f"b{index}", is_error)]}})
        entries.append({"type": "assistant", "message": {"role": "assistant",
                                                         "content": [text(final)]}})
        return entries

    def test_a_turn_ending_in_denied_calls_marks_the_pane_without_blocking(self):
        reports = []
        entries = self.blocked_turn("Write 'dalej' to continue?", True, True)
        self.assertEqual(self.run_hook(entries, reports), "")
        self.assertEqual(len(reports), 1)
        self.assertEqual(reports[0]["method"], "pane.report_awaiting_reply")
        self.assertEqual(reports[0]["params"], {"pane_id": "p1", "question": "blocked tool calls"})
        # A statement after denied calls is marked too: the agent may not have been able to ask.
        reports = []
        self.assertEqual(self.run_hook(self.blocked_turn("I stop here.", True), reports), "")
        self.assertEqual(len(reports), 1)

    def test_a_successful_last_call_changes_nothing(self):
        reports = []
        entries = self.blocked_turn("All done.", True, False)
        self.assertEqual(self.run_hook(entries, reports), "")
        self.assertEqual(reports, [])
        # An earlier failure followed by a successful call is not a blocked ending either.
        entries = self.blocked_turn("All done.", True)
        entries[-1:-1] = [
            {"type": "assistant", "message": {"role": "assistant", "content": [tool_use("c")]}},
            {"type": "user", "message": {"role": "user", "content": [tool_result("c", False)]}},
        ]
        self.assertEqual(self.run_hook(entries, reports), "")
        self.assertEqual(reports, [])
        # A question after a successful call is still blocked once, as before.
        output = self.run_hook(self.blocked_turn("Push the commits?", False), reports)
        self.assertEqual(json.loads(output)["decision"], "block")
        self.assertEqual(reports, [])

    def test_failed_calls_in_an_earlier_turn_do_not_count(self):
        reports = []
        entries = self.blocked_turn("Stuck.", True) + [
            {"type": "user", "message": {"role": "user", "content": "next"}},
            {"type": "assistant", "message": {"role": "assistant",
                                              "content": [text("All done.")]}},
        ]
        self.assertEqual(self.run_hook(entries, reports), "")
        self.assertEqual(reports, [])

    def test_the_second_stop_is_never_blocked_but_still_marked(self):
        reports = []
        question = self.blocked_turn("Push the commits?", False)
        self.assertEqual(self.run_hook(question, reports, stop_hook_active=True), "")
        self.assertEqual(reports, [])
        # Asked to run the command, the agent was denied again: the pane is marked.
        denied = self.blocked_turn("Push the commits?", True)
        self.assertEqual(self.run_hook(denied, reports, stop_hook_active=True), "")
        self.assertEqual(len(reports), 1)

    def test_shadow_mode_and_a_subagent_do_not_mark(self):
        entries = self.blocked_turn("Stuck.", True)
        reports = []
        self.assertEqual(self.run_hook(entries, reports, agent_id="sub"), "")
        self.assertEqual(self.run_hook(entries, reports, stop_mode="shadow"), "")
        self.assertEqual(reports, [])

    def test_blocks_once_for_an_unreported_question(self):
        entries = [
            {"type": "user", "message": {"role": "user", "content": "go"}},
            {"type": "assistant", "message": {"role": "assistant",
                                              "content": [text("Done.\n\nPush the commits?")]}},
        ]
        output = self.run_hook(entries)
        self.assertEqual(json.loads(output)["decision"], "block")
        # The second stop of the same turn is left alone.
        self.assertEqual(self.run_hook(entries, stop_hook_active=True), "")

    def test_a_reported_question_and_a_statement_pass(self):
        reported = [
            {"type": "user", "message": {"role": "user", "content": "go"}},
            {"type": "assistant", "message": {"role": "assistant", "content": [
                bash("herdr agent awaiting-reply"), text("Push the commits?")]}},
        ]
        self.assertEqual(self.run_hook(reported), "")
        statement = [
            {"type": "user", "message": {"role": "user", "content": "go"}},
            {"type": "assistant", "message": {"role": "assistant",
                                              "content": [text("All done, tests pass.")]}},
        ]
        self.assertEqual(self.run_hook(statement), "")

    def test_a_report_from_an_earlier_turn_does_not_count(self):
        entries = [
            {"type": "user", "message": {"role": "user", "content": "first"}},
            {"type": "assistant", "message": {"role": "assistant", "content": [
                bash("herdr agent awaiting-reply"), text("Which one?")]}},
            {"type": "user", "message": {"role": "user", "content": "second"}},
            {"type": "assistant", "message": {"role": "assistant",
                                              "content": [text("Want me to push?")]}},
        ]
        self.assertEqual(json.loads(self.run_hook(entries))["decision"], "block")


class FakeServer:
    """A herdr socket that answers `pane.get` and `tab.get` with the given tab role,
    `todo.runnable_state` with `runnable` and `pane.report_awaiting_reply` with success."""

    def __init__(self, directory, role, runnable=None):
        import socket
        import threading

        self.path = os.path.join(directory, "s.sock")
        self.role = role
        self.runnable = runnable
        self.requests = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(self.path)
        self.server.listen(8)
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while True:
            try:
                connection, _ = self.server.accept()
            except OSError:
                return
            with connection:
                connection.settimeout(2)
                data = b""
                while not data.endswith(b"\n"):
                    chunk = connection.recv(65536)
                    if not chunk:
                        break
                    data += chunk
                request = json.loads(data.decode())
                self.requests.append(request)
                if request["method"] == "pane.get":
                    result = {"type": "pane_info", "pane": {"pane_id": "p1", "tab_id": "w1:t1"}}
                elif request["method"] == "tab.get":
                    tab = {"tab_id": "w1:t1"}
                    if self.role:
                        tab["role"] = self.role
                    result = {"type": "tab_info", "tab": tab}
                elif request["method"] == "todo.runnable_state" and self.runnable is not None:
                    result = {"type": "todo_runnable_state", "state": self.runnable}
                elif request["method"] == "pane.report_awaiting_reply":
                    result = {"type": "ok"}
                else:
                    result = {}
                reply = {"id": request["id"], "result": result}
                connection.sendall((json.dumps(reply) + "\n").encode())

    def close(self):
        import socket

        try:
            self.server.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.server.close()


@unittest.skipIf(os.name == "nt", "the hook script runs under a POSIX shell")
class CoordinatorStop(unittest.TestCase):
    """The Stop hook keeps a coordinator from waiting for the user's go-ahead."""

    GO_AHEAD = "Następna w kolejce jest „Hand a session over”. Zlecę ją pracownikowi, gdy powiesz „dalej”."

    def run_hook(self, lines, role="coordinator", socket=True, stop_mode=None, runnable=None,
                 state_home=None, **fields):
        """The hook's output (parsed, or None when it lets the stop pass) on a transcript.
        `state_home` keeps the hook's state (its streaks) across runs."""
        import subprocess

        with tempfile.TemporaryDirectory() as directory:
            state_home = state_home or os.path.join(directory, "state")
            transcript = os.path.join(directory, "t.jsonl")
            with open(transcript, "w", encoding="utf-8") as handle:
                handle.write("\n".join(lines) + "\n")
            payload = {"hook_event_name": "Stop", "transcript_path": transcript}
            payload.update(fields)
            env = dict(
                os.environ,
                HERDR_ENV="1",
                HERDR_PANE_ID="p1",
                XDG_STATE_HOME=state_home,
            )
            for name in ("CURSOR_VERSION", "HERDR_SOCKET_PATH", "HERDR_AWAITING_REPLY_STOP",
                         "HERDR_AWAITING_REPLY_INSTRUCTIONS"):
                env.pop(name, None)
            if stop_mode:
                env["HERDR_AWAITING_REPLY_STOP"] = stop_mode
            server = None
            if socket:
                server = FakeServer(directory, role, runnable)
                self.server = server
                env["HERDR_SOCKET_PATH"] = server.path
            elif socket is None:
                # A socket path nobody listens on.
                env["HERDR_SOCKET_PATH"] = os.path.join(directory, "gone.sock")
            try:
                result = subprocess.run(
                    ["sh", HOOK, "stop-check"],
                    input=json.dumps(payload),
                    text=True,
                    capture_output=True,
                    env=env,
                    check=True,
                )
            finally:
                server and server.close()
            self.last_log = None
            log_path = os.path.join(state_home, "herdr", "awaiting-reply-stop.jsonl")
            if os.path.exists(log_path):
                with open(log_path, encoding="utf-8") as log:
                    self.last_log = json.loads(log.read().splitlines()[-1])
            output = result.stdout.strip()
            return json.loads(output) if output else None

    def assertCoordinatorBlock(self, output):
        self.assertIsNotNone(output)
        self.assertEqual(output["decision"], "block")
        self.assertIn("TODO coordinator", output["reason"])

    def assertNoCoordinatorBlock(self, output):
        self.assertTrue(output is None or "TODO coordinator" not in output["reason"], output)

    def test_its_go_ahead_expression_equals_the_audits(self):
        source = hook_source()
        main = source.index("try:\n    with open(os.environ[")
        namespace = {}
        exec(source[:main], namespace)  # noqa: S102 - the repository's own script
        self.assertEqual(namespace["ABANDON"].pattern, coordinator.ABANDON.pattern)
        self.assertEqual(namespace["ABANDON"].flags, coordinator.ABANDON.flags)
        self.assertEqual(namespace["NOTIFICATION_ID"].pattern, coordinator.NOTIFICATION_ID.pattern)
        self.assertEqual(namespace["ASK_TOOL"], coordinator.ASK_TOOL)
        for message in [self.GO_AHEAD, "Should I continue?", "`when you say`\n\nDone.", "Done."]:
            self.assertEqual(
                namespace["is_abandon_text"](message), coordinator.is_abandon_text(message), message
            )

    def test_blocks_a_coordinator_waiting_for_a_go_ahead_once(self):
        for final in [self.GO_AHEAD, "Item 3 is committed.\n\nShould I continue?"]:
            lines = Lines().user(ORDER).tools(coordinator_bash("t1", "cat TODO.md")).say(final)
            self.assertCoordinatorBlock(self.run_hook(lines.lines))
            self.assertTrue(self.last_log["coordinator_blocked"])
            self.assertEqual(self.last_log["role"], "coordinator")
            self.assertEqual(self.run_hook(lines.lines, stop_hook_active=True), None)

    def test_does_not_block_other_tabs_or_endings(self):
        go_ahead = Lines().user(ORDER).say(self.GO_AHEAD).lines
        for role in ["worker", None]:
            self.assertEqual(self.run_hook(go_ahead, role=role), None)
            self.assertFalse(self.last_log["coordinator_blocked"])
        # A report, a real question, a question asked through a tool or the report command.
        report = Lines().user(ORDER).say("Committed item 3; the queue is empty.").lines
        self.assertEqual(self.run_hook(report), None)
        question = Lines().user(ORDER).say("Which variant do you prefer, A or B?").lines
        self.assertNoCoordinatorBlock(self.run_hook(question))
        asked = Lines().user(ORDER).tools(coordinator_ask("t1")).say("Should I continue?").lines
        self.assertNoCoordinatorBlock(self.run_hook(asked))
        reported = Lines().user(ORDER).tools(
            coordinator_bash("t1", 'herdr agent awaiting-reply "Continue?"')
        ).say("Should I continue?").lines
        self.assertEqual(self.run_hook(reported), None)

    def test_does_not_block_while_a_background_wait_runs(self):
        lines = Lines().user(ORDER)
        lines.tools(coordinator_bash("t1", "herdr-job wait 1", background=True))
        lines.say(self.GO_AHEAD)
        self.assertEqual(self.run_hook(lines.lines), None)
        self.assertEqual(self.last_log["pending"], 1)
        # Once the wait reports, the same ending is blocked.
        lines.notify("t1").say(self.GO_AHEAD)
        self.assertCoordinatorBlock(self.run_hook(lines.lines))

    def test_a_wait_started_before_the_read_tail_still_counts(self):
        lines = Lines().user(ORDER)
        lines.tools(coordinator_bash("t1", "herdr-job wait 1", background=True))
        for index in range(60):
            lines.user("x" * 10_000).say("noted %d" % index)
        lines.say(self.GO_AHEAD)
        self.assertEqual(self.run_hook(lines.lines), None)
        self.assertEqual(self.last_log["pending"], 1)

    def test_fails_open(self):
        lines = Lines().user(ORDER).say(self.GO_AHEAD).lines
        self.assertEqual(self.run_hook(lines, socket=None), None)  # unreachable
        self.assertEqual(self.run_hook(lines, socket=False), None)  # no socket path
        self.assertEqual(self.run_hook(lines, stop_mode="shadow"), None)
        self.assertTrue(self.last_log["abandon"])
        self.assertFalse(self.last_log["coordinator_blocked"])

    def test_blocks_only_the_audits_abandoned_turn_ends(self):
        """Every turn end of the audit's labelled cases, cut where the stop happens."""
        cases = []
        lines = Lines().user(ORDER).tools(coordinator_bash("t1", "cat TODO.md"), coordinator_ask("t2"))
        cases.append(lines.say("Two decisions are open."))
        lines = Lines().user(ORDER).tools(coordinator_bash("t1", 'herdr agent awaiting-reply "2 decisions"'))
        lines.say("Two decisions wait for you.")
        lines.user("ok").tools(coordinator_bash("t2", "grep -n 'herdr agent awaiting-reply' AGENTS.md"))
        cases.append(lines.say("Updated the notes."))
        lines = Lines().user(ORDER).tools(coordinator_bash("t1", "cat TODO.md")).say(self.GO_AHEAD)
        lines.user("why?").say("Because.\n\nShould I continue with the next item?")
        cases.append(lines.user("and?").say("Done for now; let me know when I can go on."))
        lines = Lines().user(ORDER)
        lines.tools(coordinator_bash("t1", "herdr-job wait 20261007-010203-abcd", background=True))
        lines.say("The worker runs; its wait wakes me when it ends.")
        lines.notify("t1").tools(coordinator_bash("t2", "git log -1"))
        cases.append(lines.say("Brought the worker's commit in. The queue is empty."))
        lines = Lines().user(ORDER).tools(coordinator_bash("t1", "herdr agent wait w1:p2", background=True))
        lines.add("attachment", None, attachment={
            "type": "queued_command", "prompt": "<task-notification><tool-use-id>t1</tool-use-id>"})
        cases.append(lines.say("All items are done. Zlecę następne, gdy powiesz."))
        lines = Lines().user(ORDER).say("Which variant do you prefer, A or B?")
        cases.append(lines.user("A").say("Committed variant A; every item is done."))
        lines = Lines().user(ORDER).tools(coordinator_bash("t1", "herdr-job wait 2", background=True))
        cases.append(lines.say("Waiting for the worker. Should I continue after it?"))

        checked = {"abandoned": 0, "other": 0}
        for case in cases:
            with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as handle:
                handle.write("\n".join(case.lines) + "\n")
            self.addCleanup(os.unlink, handle.name)
            labels = [turn.label for turn in coordinator.load_session(handle.name).turns]
            ends = [index for index, line in enumerate(case.lines)
                    if index and starts_turn(json.loads(line))] + [len(case.lines)]
            self.assertEqual(len(ends), len(labels))
            for end, label in zip(ends, labels):
                output = self.run_hook(case.lines[:end])
                if label == "abandoned":
                    self.assertCoordinatorBlock(output)
                    checked["abandoned"] += 1
                else:
                    self.assertNoCoordinatorBlock(output)
                    checked["other"] += 1
        self.assertGreaterEqual(checked["abandoned"], 4)
        self.assertGreaterEqual(checked["other"], 6)

    STALLED = {"repo": "/repo", "stalled": True, "runnable_items": ["t-aaaaaaaa"],
               "reason": "nothing runs while 1 item(s) of \"Next, in order\" can run"}

    def runnable_requests(self):
        return [r for r in self.server.requests if r["method"] == "todo.runnable_state"]

    REPORT = "Landed item 3 and installed it. Next in the queue is item 4."

    def requests_of(self, method):
        return [r for r in self.server.requests if r["method"] == method]

    def test_a_stalled_coordinator_is_blocked_with_the_next_command(self):
        report = Lines().user(ORDER).tools(coordinator_bash("t1", "git log")).say(self.REPORT).lines
        output = self.run_hook(report, runnable=self.STALLED, background_tasks=[],
                               session_id="s1", cwd="/repo/sub")
        self.assertEqual(output["decision"], "block")
        self.assertIn("herdr todo run t-aaaaaaaa --task", output["reason"])
        self.assertIn("herdr decision add", output["reason"])
        self.assertIn("Block 1 of 3", output["reason"])
        self.assertTrue(self.last_log["stall_would_block"])
        self.assertTrue(self.last_log["stall_blocked"])
        self.assertIn("1 item(s)", self.last_log["stall_reason"])
        self.assertEqual(self.last_log["background_tasks"], 0)
        self.assertEqual(self.runnable_requests()[0]["params"], {"cwd": "/repo/sub"})
        # Shadow mode only logs.
        self.assertIsNone(self.run_hook(report, runnable=self.STALLED, background_tasks=[],
                                        stop_mode="shadow"))
        self.assertTrue(self.last_log["stall_would_block"])
        self.assertFalse(self.last_log["stall_blocked"])

    def test_an_open_decision_is_the_users_turn(self):
        report = Lines().user(ORDER).say(self.REPORT).lines
        waiting = dict(self.STALLED, open_decisions=["d-aaaaaaaa"])
        self.assertIsNone(self.run_hook(report, runnable=waiting, background_tasks=[]))
        self.assertFalse(self.last_log["stall_would_block"])
        self.assertEqual(self.last_log["stall_reason"], "decision d-aaaaaaaa waits on the user")

    def test_three_blocks_in_a_row_then_the_stop_goes_through_and_the_user_is_asked(self):
        with tempfile.TemporaryDirectory() as state:
            lines = Lines().user(ORDER).say(self.REPORT)

            def stop(**fields):
                fields.setdefault("stop_hook_active", True)
                return self.run_hook(lines.lines, runnable=self.STALLED, background_tasks=[],
                                     session_id="s1", state_home=state, **fields)

            for n in (1, 2, 3):
                output = stop(stop_hook_active=n > 1)
                self.assertEqual(output["decision"], "block", n)
                self.assertIn("Block %d of 3" % n, output["reason"])
                self.assertEqual(self.requests_of("pane.report_awaiting_reply"), [])
                # Claude Code feeds the block back as a user message: not the user's.
                lines.user("Stop hook feedback:\n" + output["reason"]).say(self.REPORT)
            # The fourth stop goes through, and the pane enters the user's `?` list, once.
            self.assertIsNone(stop())
            self.assertEqual(self.last_log["stall_reason"], "three would-blocks in a row")
            self.assertTrue(self.last_log["stall_escalated"])
            [escalation] = self.requests_of("pane.report_awaiting_reply")
            self.assertEqual(escalation["params"]["pane_id"], "p1")
            self.assertIn("t-aaaaaaaa", escalation["params"]["question"])
            self.assertIsNone(stop())
            self.assertEqual(self.requests_of("pane.report_awaiting_reply"), [])
            # A new message of the user starts the count again.
            lines.user("go on").say(self.REPORT)
            self.assertEqual(stop(stop_hook_active=False)["decision"], "block")

    def test_the_stall_check_passes_without_evidence_of_a_stall(self):
        report = Lines().user(ORDER).say("Landed item 3.").lines
        # `background_tasks` absent is unknown: herdr is not even asked.
        self.run_hook(report, runnable=self.STALLED)
        self.assertFalse(self.last_log["stall_would_block"])
        self.assertEqual(self.last_log["stall_reason"], "background tasks unknown")
        self.assertIsNone(self.last_log["background_tasks"])
        self.assertEqual(self.runnable_requests(), [])
        self.run_hook(report, runnable=self.STALLED, background_tasks=[{"id": "b1"}])
        self.assertEqual(self.last_log["stall_reason"], "background tasks running")
        # Queue mode, a pause or a run in progress: herdr's state says no stall.
        queued = {"stalled": False, "reason": "queue mode is on: the server starts the next run"}
        self.run_hook(report, runnable=queued, background_tasks=[])
        self.assertFalse(self.last_log["stall_would_block"])
        self.assertEqual(self.last_log["stall_reason"], queued["reason"])
        # herdr cannot tell, or the tab is no coordinator's.
        self.run_hook(report, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "repository state unknown")
        self.run_hook(report, role="worker", runnable=self.STALLED, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "not a coordinator tab")
        self.assertEqual(self.runnable_requests(), [])

    def test_a_question_or_the_users_stop_is_no_stall(self):
        question = Lines().user(ORDER).say("Which variant do you prefer, A or B?").lines
        self.run_hook(question, runnable=self.STALLED, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "asked the user")
        asked = Lines().user(ORDER).tools(coordinator_ask("t1")).say("Two decisions.").lines
        self.run_hook(asked, runnable=self.STALLED, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "asked the user")
        stopped = Lines().user(ORDER).say("Working.").user("stop").say("Stopped.").lines
        self.run_hook(stopped, runnable=self.STALLED, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "the user said stop")
        # A task notification after the stop is no new message of the user.
        stopped = Lines().user(ORDER).tools(
            coordinator_bash("t1", "herdr-job wait 1", background=True)
        ).say("Waiting.").user("Zatrzymaj się.").say("Ok.").notify("t1").say("Landed.").lines
        self.run_hook(stopped, runnable=self.STALLED, background_tasks=[])
        self.assertEqual(self.last_log["stall_reason"], "the user said stop")

    def test_plain_stop_messages(self):
        namespace = replay.hook_namespace()
        for message in ["stop", "Stop.", "pause for now", "zatrzymaj się", "wstrzymaj!", "dość"]:
            self.assertTrue(namespace["is_plain_stop"](message), message)
        for message in ["stop the server and go on", "rób TODO po kolei", "dalej", "",
                        "please stop " + "x" * 80]:
            self.assertFalse(namespace["is_plain_stop"](message), message)

    def test_the_streak_allows_three_would_blocks_in_a_row(self):
        stalled = {"background_tasks": [], "role": "coordinator",
                   "state": {"stalled": True, "reason": "stalled"}}
        waiting = dict(stalled, background_tasks=[None])
        fixtures = [{"session": "s1", "ended": str(n), "label": "other", "silent_stop": False,
                     "tail": "", "facts": facts}
                    for n, facts in enumerate([stalled] * 5 + [waiting, stalled])]
        decided = replay.replay(fixtures)
        self.assertEqual([d["would_block"] for d in decided],
                         [True, True, True, False, False, False, True])
        self.assertEqual(decided[3]["reason"], "three would-blocks in a row")

    def test_the_audited_turn_ends_meet_the_shadow_targets(self):
        """The 244 turn ends of docs/coordinator-audit-2026-10-09.md, as fixtures: 3 of 3 silent
        stops caught, at most 1 false per 100 (kill criterion: more than 1 per 20)."""
        with open(FIXTURES, encoding="utf-8") as handle:
            fixtures = json.load(handle)["turn_ends"]
        report = replay.summary(replay.replay(fixtures))
        self.assertEqual(report["turn_ends"], 244)
        self.assertEqual((report["caught"], report["silent_stops"]), (3, 3), report["missed"])
        self.assertLessEqual(report["false_per_100"], 1.0, report["false"])

    def test_a_stop_after_a_block_is_checked_again(self):
        namespace = replay.hook_namespace()
        facts = {"background_tasks": [], "stop_hook_active": True, "role": "coordinator",
                 "state": {"stalled": True, "reason": "stalled"}}
        self.assertEqual(namespace["stall_decision"](facts), (True, "stalled"))
        waiting = dict(facts, state={"stalled": True, "open_decisions": ["d-a", "d-b"]})
        self.assertEqual(namespace["stall_decision"](waiting),
                         (False, "decision d-a, d-b waits on the user"))
        reason = namespace["stall_block_reason"](
            {"reason": "nothing runs", "runnable_items": ["t-aaaaaaaa"]}, 2)
        self.assertIn("`herdr todo run t-aaaaaaaa --task <file>", reason)
        self.assertIn("herdr todo queue pause", reason)
        self.assertIn("Block 2 of 3", reason)

    def test_next_items_are_the_open_items_with_an_id(self):
        todo = ("# TODO\n\n## Next, in order\n\n- [ ] A [t-aaaaaaaa]\n  text\n"
                "- [ ] No id\n```\n- [ ] B [t-cccccccc]\n```\n### Sub\n- [ ] C [t-dddddddd]\n"
                "## Proposed\n- [ ] D [t-bbbbbbbb]\n")
        self.assertEqual(replay.next_items(todo), ["t-aaaaaaaa", "t-dddddddd"])


class QuestionGate(unittest.TestCase):
    """The pre-tool hook's question gate, on its own definitions (no socket)."""

    RECORDS = {
        "d-aaaaaaaa": {"id": "d-aaaaaaaa", "status": "open"},
        "d-bbbbbbbb": {"id": "d-bbbbbbbb", "status": "decided", "answer": "yes, in shadow mode",
                       "source": "relayed", "relayed_by": "todo-herdr"},
        "d-cccccccc": {"id": "d-cccccccc", "status": "superseded", "superseded_by": "d-aaaaaaaa"},
    }

    def setUp(self):
        self.namespace = pre_tool_namespace()
        self.looked_up = []

    def lookup(self, decision_id):
        self.looked_up.append(decision_id)
        if decision_id == "d-ffffffff":
            return "failed", "timed out"
        record = self.RECORDS.get(decision_id)
        return ("found", record) if record else ("missing", "not found")

    def refusal(self, *questions):
        return self.namespace["ask_refusal"]({"questions": list(questions)}, self.lookup)

    def test_an_open_record_or_a_user_only_capability_is_asked(self):
        for question in [
            {"header": "d-aaaaaaaa", "question": "Enforce the stop check?"},
            {"header": "Stop check", "question": "Enforce it (d-aaaaaaaa)?"},
            {"header": "Grant", "question": "Grant prepare?"},
            {"header": "login", "question": "Log in to the provider?"},
            {"header": "Trust", "question": "Trust the folder?"},
            {"header": "Live test", "question": "Run it with your account?"},
        ]:
            self.assertIsNone(self.refusal(question), question)
        self.assertIsNone(self.refusal({"header": "Grant", "question": "Grant?"},
                                       {"header": "d-aaaaaaaa", "question": "Enforce?"}))

    def test_anything_else_is_refused_with_why(self):
        for question, named in [
            ({"header": "Install", "question": "Install the integration?"},
             "herdr decision add"),
            ({"header": "d-bbbbbbbb", "question": "Install?"},
             "decided already (relayed by todo-herdr): yes, in shadow mode"),
            ({"header": "Old", "question": "As in d-cccccccc?"}, "superseded by d-aaaaaaaa"),
            ({"header": "d-dddddddd", "question": "?"}, "not in the decision ledger"),
            ({"header": "d-ffffffff", "question": "?"}, "could not look up d-ffffffff"),
            ({"header": "x-d-aaaaaaaa", "question": "d-aaaaaaaab?"}, "cites no open decision"),
        ]:
            refusal = self.refusal(question)
            self.assertIsNotNone(refusal, question)
            self.assertIn(named, refusal)
        # A decided record among open ones refuses the question.
        self.assertIn("decided already", self.refusal(
            {"header": "d-aaaaaaaa", "question": "And d-bbbbbbbb?"}))
        # One question without a citation refuses the call.
        self.assertIn("cites no open decision", self.refusal(
            {"header": "Grant", "question": "Grant?"}, {"header": "Next", "question": "Go on?"}))
        for empty in [{}, {"questions": []}, {"questions": "x"}]:
            self.assertIsNotNone(self.namespace["ask_refusal"](empty, self.lookup))

    def test_the_decision_command_is_allowed_to_a_coordinator(self):
        check = self.namespace["check_bash"]
        self.assertIsNone(check('herdr decision add "Enforce it?" --item t-aaaaaaaa', "/r", "s"))
        self.assertIsNone(check("herdr decision list --open", "/r", "s"))


def starts_turn(entry):
    """A user prompt or task notification, not a tool result."""
    if entry.get("type") != "user":
        return False
    content = entry["message"]["content"]
    return isinstance(content, str)


@unittest.skipIf(os.name == "nt", "the hook script runs under a POSIX shell")
class PlanHook(unittest.TestCase):
    """The TodoWrite hook reports `done/total` as the plan token."""

    def report(self, payload):
        import socket
        import subprocess
        import threading

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "s.sock")
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            server.bind(path)
            server.listen(1)
            received = []

            def accept():
                server.settimeout(5)
                try:
                    connection, _ = server.accept()
                    received.append(connection.recv(65536).decode())
                    connection.sendall(b"{}\n")
                    connection.close()
                except OSError:
                    pass

            thread = threading.Thread(target=accept)
            thread.start()
            env = dict(os.environ, HERDR_ENV="1", HERDR_PANE_ID="p1", HERDR_SOCKET_PATH=path)
            env.pop("CURSOR_VERSION", None)
            subprocess.run(
                ["sh", HOOK, "plan"],
                input=json.dumps(payload),
                text=True,
                env=env,
                check=True,
                capture_output=True,
            )
            thread.join(6)
            server.close()
            return json.loads(received[0]) if received else None

    def test_reports_the_completed_count(self):
        todos = [
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "in_progress"},
            {"content": "c", "status": "pending"},
        ]
        request = self.report(
            {"hook_event_name": "PostToolUse", "tool_name": "TodoWrite",
             "tool_input": {"todos": todos}}
        )
        self.assertEqual(request["method"], "pane.report_metadata")
        self.assertEqual(request["params"]["tokens"], {"plan": "1/3"})
        self.assertEqual(request["params"]["pane_id"], "p1")

    def test_an_empty_list_clears_the_token(self):
        request = self.report(
            {"hook_event_name": "PostToolUse", "tool_name": "TodoWrite",
             "tool_input": {"todos": []}}
        )
        self.assertEqual(request["params"]["tokens"], {"plan": None})

    def test_other_tools_report_nothing(self):
        self.assertIsNone(
            self.report({"hook_event_name": "PostToolUse", "tool_name": "Bash",
                         "tool_input": {"command": "ls"}})
        )


if __name__ == "__main__":
    unittest.main()
