"""Offline end-to-end tests for the Claude consultation helper."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HELPER = Path(__file__).parent / "skills/claude/ask_claude.py"


def load_helper():
    spec = importlib.util.spec_from_file_location("ask_claude", HELPER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ClaudeConsultTests(unittest.TestCase):
    def call(self, mode="ok", extra=(), stdin=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cli = root / "claude"
            cli.write_text("#!/usr/bin/env python3\n" + '''import json, os, sys
from pathlib import Path
Path(os.environ["CAPTURE"]).write_text(json.dumps({"args":sys.argv[1:], "prompt":sys.stdin.read(), "cwd":os.getcwd()}))
mode = os.environ["FAKE_MODE"]
if mode == "malformed": print("not JSON"); sys.exit(0)
model = sys.argv[sys.argv.index("--model") + 1]
print(json.dumps({"subtype":"success", "is_error":mode == "error",
                  "result":"usage limit reached" if mode == "error" else "answer", "usage":{"input_tokens":2,"cache_creation_input_tokens":10,"cache_read_input_tokens":5,"output_tokens":7,"output_tokens_details":{"thinking_tokens":3}}, "modelUsage":{model:{"canonicalModel":model}}}))
''')
            cli.chmod(0o755)
            log = root / "log.jsonl"
            env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}", CONSULT_IN_JOB="1",
                       CLAUDE_CONSULT_MODEL="claude-sonnet-5-5",
                       CONSULT_LOG=str(log), CONSULT_ROUND="test-round", CAPTURE=str(root / "capture"), FAKE_MODE=mode)
            result = subprocess.run([sys.executable, str(HELPER), *extra, "question"],
                                    input=stdin, capture_output=True, text=True, env=env)
            records = [json.loads(line) for line in log.read_text().splitlines()]
            capture = json.loads((root / "capture").read_text())
            return result, records[-1], capture

    def test_success_model_usage_and_no_tools(self):
        result, record, capture = self.call()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "answer")
        self.assertEqual(record["skill"], "claude")
        self.assertEqual(record["model"], "claude-sonnet-5-5")
        self.assertEqual(record["status"], "ok")
        self.assertEqual(record["usage"]["input"], 17)
        self.assertEqual(record["usage"]["cached"], 5)
        self.assertEqual(record["usage"]["output"], 7)
        self.assertEqual(record["usage"]["reasoning"], 3)
        self.assertIn("--strict-mcp-config", capture["args"])
        self.assertEqual(capture["args"][capture["args"].index("--tools") + 1], "")
        self.assertNotEqual(capture["cwd"], os.getcwd())

    def test_explicit_opus_model_is_forwarded_and_logged(self):
        result, record, capture = self.call(extra=("-m", "claude-opus-5-5"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(capture["args"][capture["args"].index("--model") + 1],
                         "claude-opus-5-5")
        self.assertEqual(record["model"], "claude-opus-5-5")
        self.assertEqual(record["model_version"], "claude-opus-5-5")

    def test_error_and_malformed_logged_with_their_kind(self):
        # consult.py classifies the error text: the CLI's own error message, or the JSON parse failure.
        for mode, kind in (("error", "limit"), ("malformed", "other")):
            with self.subTest(mode=mode):
                result, record, _ = self.call(mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(record["status"], "error")
                self.assertEqual(record["error_kind"], kind)

    def test_timeout_logged_and_deadline_forwarded(self):
        # In process, the CLI runner replaced by one that raises TimeoutExpired at once: no child, no sleep, no clock.
        helper = load_helper()
        calls = []

        def runner(argv, **kwargs):
            calls.append(kwargs)
            raise subprocess.TimeoutExpired(argv, kwargs["timeout"])

        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "log.jsonl"
            env = {"CONSULT_IN_JOB": "1", "CLAUDE_CONSULT_MODEL": "claude-sonnet-5-5",
                   "CONSULT_LOG": str(log), "CONSULT_ROUND": "test-round"}
            with mock.patch.dict(os.environ, env), mock.patch.object(helper, "run_claude", runner), \
                    mock.patch.object(sys, "argv", [str(HELPER), "-t", "7", "question"]), \
                    contextlib.redirect_stderr(io.StringIO()):
                rc = helper.main()
            record = json.loads(log.read_text().splitlines()[-1])
        self.assertEqual(rc, 1)
        self.assertEqual([call["timeout"] for call in calls], [7])
        self.assertEqual(record["status"], "error")
        self.assertEqual(record["error_kind"], "timeout")

    def test_stdin_attachment(self):
        result, _, capture = self.call(extra=("-f", "-"), stdin="attached text")
        self.assertEqual(result.returncode, 0)
        self.assertIn("--- stdin ---\nattached text", capture["prompt"])


if __name__ == "__main__":
    unittest.main()
