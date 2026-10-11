"""Offline tests for `consult.py round`: the model set from the config file, caller exclusion, parallel runs."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

PLUGIN = Path(__file__).parent
CONSULT = PLUGIN / "skills/consult-stats/consult.py"

# A stand-in ask_* script: answers with its name, the round id and the prompt, and prints a call id like the real
# wrappers. `fail` exits 1 with a limit error. `reader` and `writer` meet at a FIFO, whose open blocks until the other
# end opens too: run one after the other they would deadlock, so they answer only when the round runs them at once.
FAKE = r"""#!/bin/sh
name=$1; shift
case $name in
  fail) echo "HTTP 429: usage limit" >&2; echo "[consult id: id-$name]" >&2; exit 1 ;;
  empty) echo "[consult id: id-$name]" >&2; exit 0 ;;
  reader) read -r line < "$MARK_DIR/fifo" ;;
  writer) echo go > "$MARK_DIR/fifo" ;;
esac
printf 'answer from %s in round %s: %s\n' "$name" "$CONSULT_ROUND" "$1"
echo "[consult id: id-$name]" >&2
"""


class RoundTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = Path(self.tmp.name)
        fake = d / "ask_fake.sh"
        fake.write_text(FAKE)
        fake.chmod(0o755)
        os.mkfifo(d / "fifo")
        self.config = d / "models.json"
        self.write_config(["sol", "mimo", "deepseek"])
        self.brief = d / "brief.txt"
        self.brief.write_text("Is this design sound?\n")
        self.env = {k: v for k, v in os.environ.items() if k not in ("PI_MODEL", "CONSULT_ROUND", "HERDR_SOCKET_PATH")}
        self.env.update(CONSULT_MODELS=str(self.config), CONSULT_LOG=str(d / "log.jsonl"), MARK_DIR=str(d))

    def tearDown(self):
        self.tmp.cleanup()

    def write_config(self, default):
        fake = str(Path(self.tmp.name) / "ask_fake.sh")
        names = {"sol": "gpt", "mimo": "mimo", "deepseek": "deepseek", "sonnet": "claude",
                 "fail": "gemini", "empty": "gemini", "reader": "gemini", "writer": "gemini"}
        self.config.write_text(json.dumps({
            "default": default,
            "models": {n: {"family": f, "command": [fake, n]} for n, f in names.items()},
            "families": {"deepseek": "deepseek", "mimo": "mimo|xiaomi", "claude": "claude|opus",
                         "gemini": "gemini", "gpt": "gpt|codex"},
        }))

    def run_round(self, *args, env=None):
        # delay: test harness deadline, so a round that ran the FIFO pair one after the other fails instead of hanging.
        return subprocess.run([sys.executable, str(CONSULT), "round", "--brief", str(self.brief), *args],
                              capture_output=True, text=True, env={**self.env, **(env or {})}, timeout=60)

    def test_claude_caller_asks_the_whole_default_set_under_one_round(self):
        r = self.run_round("--caller-model", "claude")
        self.assertEqual(r.returncode, 0, r.stderr)
        rid = r.stdout.splitlines()[0].removeprefix("round: ")
        for n in ("sol", "mimo", "deepseek"):
            self.assertIn(f"=== {n}: ok, call id id-{n} ===", r.stdout)
            self.assertIn(f"answer from {n} in round {rid}: Is this design sound?", r.stdout)
        self.assertIn("skipped: none", r.stdout)
        self.assertIn("failed: none", r.stdout)

    def test_the_config_is_read_at_each_invocation(self):
        self.write_config(["sol", "deepseek"])
        r = self.run_round("--caller-model", "claude")
        self.assertIn("asked: sol, deepseek", r.stdout)
        self.assertNotIn("=== mimo", r.stdout)

    def test_the_callers_family_is_left_out(self):
        for caller, gone in (("openai-codex/gpt-6.1-sol", "sol"), ("deepseek/deepseek-flash", "deepseek"),
                             ("openrouter/xiaomi/mimo-v2.6-pro", "mimo"), ("sol", "sol")):
            with self.subTest(caller=caller):
                r = self.run_round("--caller-model", caller)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertNotIn(f"=== {gone}:", r.stdout)
                self.assertIn(f"skipped: {gone} (the caller's own family", r.stdout)

    def test_pi_model_names_the_caller(self):
        r = self.run_round(env={"PI_MODEL": "deepseek-flash"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("asked: sol, mimo", r.stdout)

    def test_an_unknown_caller_fails_closed(self):
        for args in ((), ("--caller-model", "mystery-7b")):
            with self.subTest(args=args):
                r = self.run_round(*args)
                self.assertNotEqual(r.returncode, 0)
                self.assertIn("--caller-model", r.stderr)
                self.assertNotIn("round:", r.stdout)

    def test_explicit_models_win(self):
        r = self.run_round("--models", "sol,sonnet")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("asked: sol, sonnet", r.stdout)
        r = self.run_round("--models", "sol", "--caller-model", "gpt-6.1-sol")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("=== sol: ok", r.stdout)
        self.assertIn("caller's own family", r.stderr)

    def test_an_unknown_model_is_refused(self):
        r = self.run_round("--models", "sol,nope")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("nope", r.stderr)

    def test_failed_models_are_named_and_the_others_answer(self):
        r = self.run_round("--models", "sol,fail,empty")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("=== fail: failed (exit 1), call id id-fail ===", r.stdout)
        self.assertIn("HTTP 429: usage limit", r.stdout)
        self.assertIn("=== empty: failed (empty answer)", r.stdout)
        self.assertIn("failed: fail, empty", r.stdout)

    def test_a_round_where_every_model_failed_exits_nonzero(self):
        r = self.run_round("--models", "fail")
        self.assertEqual(r.returncode, 1)
        self.assertIn("failed: fail", r.stdout)

    def test_models_run_in_parallel(self):
        r = self.run_round("--models", "reader,writer")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("=== reader: ok", r.stdout)
        self.assertIn("=== writer: ok", r.stdout)

    def test_the_shipped_config_is_valid(self):
        cfg = json.loads((PLUGIN / "consult-models.json").read_text())
        for n in cfg["default"]:
            self.assertIn(n, cfg["models"])
        for n, m in cfg["models"].items():
            self.assertIn(m["family"], cfg["families"], n)
            self.assertTrue((PLUGIN / "skills" / m["command"][0]).is_file(), n)


if __name__ == "__main__":
    unittest.main()
