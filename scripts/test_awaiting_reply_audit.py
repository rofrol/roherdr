#!/usr/bin/env python3
"""Tests for awaiting_reply_audit.py on synthetic transcripts."""

import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import awaiting_reply_audit as audit  # noqa: E402


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


def hook_source():
    with open(HOOK, encoding="utf-8") as handle:
        text = handle.read()
    start = text.index('if [ "$action" = "stop-check" ]')
    start = text.index("<<'PY'\n", start) + len("<<'PY'\n")
    return text[start : text.index("\nPY\n", start)]


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
                    reports.append(json.loads(connection.recv(65536).decode()))
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
