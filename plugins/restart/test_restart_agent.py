import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("restart_agent", Path(__file__).with_name("restart_agent.py"))
restart_agent = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restart_agent)
argv = restart_agent.relaunch_argv
CLAUDE = {"kind": "id", "value": "abc-123"}
PI = {"kind": "path", "value": "/s/session.jsonl"}


class RelaunchArgv(unittest.TestCase):
    def test_keeps_flags_and_adds_the_resume(self):
        self.assertEqual(
            argv("claude", ["claude", "--dangerously-skip-permissions", "--model", "opus"], CLAUDE),
            ["claude", "--dangerously-skip-permissions", "--model", "opus", "--resume", "abc-123"],
        )

    def test_drops_old_resume_continue_and_session_arguments(self):
        self.assertEqual(
            argv("claude", ["claude", "--resume", "old", "-c", "--session-id", "x", "--effort=low"], CLAUDE),
            ["claude", "--effort=low", "--resume", "abc-123"],
        )
        self.assertEqual(argv("claude", ["claude", "--resume"], CLAUDE), ["claude", "--resume", "abc-123"])

    def test_drops_prompts_so_they_are_not_sent_again(self):
        self.assertEqual(
            argv("claude", ["claude", "--verbose", "fix the build", "--model", "opus"], CLAUDE),
            ["claude", "--verbose", "--model", "opus", "--resume", "abc-123"],
        )
        self.assertEqual(argv("claude", ["claude", "fix it"], CLAUDE), ["claude", "--resume", "abc-123"])
        self.assertEqual(argv("claude", ["claude", "--", "-weird prompt"], CLAUDE),
                         ["claude", "--resume", "abc-123"])

    def test_unknown_flags_keep_their_value(self):
        self.assertEqual(
            argv("claude", ["claude", "--new-flag", "value"], CLAUDE),
            ["claude", "--new-flag", "value", "--resume", "abc-123"],
        )

    def test_pi_resumes_by_session_path(self):
        self.assertEqual(
            argv("pi", ["pi", "--model", "sonnet", "--continue"], PI),
            ["pi", "--model", "sonnet", "--session", "/s/session.jsonl"],
        )
        self.assertEqual(argv("pi", ["pi", "--session", "/old"], PI), ["pi", "--session", "/s/session.jsonl"])


if __name__ == "__main__":
    unittest.main()
