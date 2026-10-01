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


if __name__ == "__main__":
    unittest.main()
