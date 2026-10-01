"""Offline end-to-end tests for Gemini availability and error propagation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

HELPER = Path(__file__).parent / "skills/gemini/ask_gemini.sh"


class GeminiConsultTests(unittest.TestCase):
    def call(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cli = root / "agy"
            cli.write_text("#!/usr/bin/env python3\n" + '''import json, os, sys
from pathlib import Path
with Path(os.environ["CAPTURE"]).open("a") as capture:
    capture.write(json.dumps({"args": sys.argv[1:], "prompt": sys.stdin.read()}) + "\\n")
if "/quota" in sys.argv:
    print("Unexpected quota preflight", file=sys.stderr)
    sys.exit(99)
if os.environ["FAKE_MODE"] == "rejected":
    print("Weekly quota exhausted; resets 2026-10-07T00:00:00Z", file=sys.stderr)
    sys.exit(1)
result = {"status": "SUCCESS", "response": "answer", "denied_actions": []}
if os.environ["FAKE_MODE"] == "error-result":
    result = {"status": "ERROR", "error": "Weekly quota exhausted"}
print(json.dumps({"event": "result", "result": result}))
''')
            cli.chmod(0o755)
            log = root / "log.jsonl"
            env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                       CONSULT_LOG=str(log), CONSULT_ROUND="test-round",
                       CAPTURE=str(root / "capture"), FAKE_MODE=mode)
            # Exercise the ordinary entry point, not the in-job bypass.
            env.pop("CONSULT_IN_JOB", None)
            env.pop("HERDR_SOCKET_PATH", None)
            result = subprocess.run(["bash", str(HELPER), "question"],
                                    capture_output=True, text=True, env=env, timeout=10)
            captures = [json.loads(line) for line in (root / "capture").read_text().splitlines()]
            records = [json.loads(line) for line in log.read_text().splitlines()]
            return result, captures, records

    def test_success_attempts_once_without_quota_preflight(self):
        result, captures, records = self.call("ok")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "answer")
        self.assertEqual(len(captures), 1)
        self.assertNotIn("/quota", captures[0]["args"])
        self.assertIn("--model", captures[0]["args"])
        self.assertIn("question", captures[0]["prompt"])
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["skill"], "gemini")
        self.assertEqual(records[0]["status"], "ok")

    def test_rejections_are_reported_logged_and_not_retried(self):
        for mode in ("rejected", "error-result"):
            with self.subTest(mode=mode):
                result, captures, records = self.call(mode)
                self.assertEqual(result.returncode, 1)
                self.assertIn("Weekly quota exhausted", result.stderr)
                self.assertEqual(len(captures), 1)
                self.assertEqual(len(records), 1)
                self.assertEqual(records[0]["status"], "error")


if __name__ == "__main__":
    unittest.main()
