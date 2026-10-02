"""Offline tests for consult.py: error kinds, latency percentiles, the rounds table and column widths."""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

CONSULT = Path(__file__).parent / "skills/consult-stats/consult.py"

_spec = importlib.util.spec_from_file_location("consult", CONSULT)
mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mod)


def call(cid, label_model, ts, seconds, status="ok", rnd="r1", skill="gpt", **extra):
    return cid, {"type": "call", "id": cid, "ts": ts, "skill": skill, "model": label_model, "status": status,
                 "seconds": seconds, "round": rnd, **extra}


class ClassifyTests(unittest.TestCase):
    def test_known_texts(self):
        cases = {
            "HTTP 429: Too Many Requests": "limit",
            "You've hit your usage limit. Try again at 5pm": "limit",
            "RESOURCE_EXHAUSTED: quota": "limit",
            "HTTP 401: invalid key": "auth",
            "No openai-codex token in pi; log in to pi (/login)": "auth",
            "finish_reason deadline": "timeout",
            "finish_reason error: URLError: <urlopen error [Errno 8] nodename nor servname provided>": "network",
            "HTTP 503: Service Unavailable": "server",
            "HTTP 504 Gateway Timeout": "server",
            "Empty answer (the model probably tried to use a blocked tool)": "empty",
            "finish_reason length": "other",
            "The 'no-such-model-xyz' model is not supported when using Codex with a ChatGPT account.": "model",
            'invalid model selection (--model "gemini-x"): model gemini-x is not recognized': "model",
            "[claude-code:unrecognized_model] There's an issue with the selected model": "model",
            "HTTP 400: no-such/model is not a valid model ID": "model",
        }
        for text, kind in cases.items():
            with self.subTest(text=text):
                self.assertEqual(mod.classify_error(text), kind)

    def test_bare_numbers_inside_words_do_not_match(self):
        self.assertEqual(mod.classify_error("plugin blog 4290 codes, catalogue 5030"), "other")


class PercentileTests(unittest.TestCase):
    def test_median_of_even_count_is_the_mean_of_the_middle_pair(self):
        self.assertEqual(mod.p50([10, 20, 30, 40]), 25)

    def test_p90_needs_ten_values(self):
        self.assertIsNone(mod.p90(list(range(9))))
        self.assertEqual(mod.p90(list(range(1, 11))), 9)
        self.assertEqual(mod.p90(list(range(1, 21))), 18)

    def test_empty(self):
        self.assertIsNone(mod.p50([]))


class RoundTableTests(unittest.TestCase):
    def test_last_by_end_time_not_duration(self):
        # A ran 40 s from t=0; B started 30 s later and ran 20 s, so B ended last and the round waited for it.
        calls = dict([call("a", "A", 1040, 40), call("b", "B", 1050, 20)])
        t = mod.round_table(calls)
        self.assertEqual(t["gpt/B"]["last"], 1)
        self.assertEqual(t["gpt/B"]["gaps"], [10])
        self.assertEqual(t["gpt/A"]["last"], 0)
        self.assertEqual(t["gpt/A"]["rounds"], 1)

    def test_tie_has_no_straggler(self):
        calls = dict([call("a", "A", 1000, 5), call("b", "B", 1000, 5)])
        t = mod.round_table(calls)
        self.assertEqual(t["gpt/A"]["last"] + t["gpt/B"]["last"], 0)

    def test_failed_last_call_is_counted(self):
        calls = dict([call("a", "A", 1000, 5), call("b", "B", 1300, 300, status="error", error_kind="timeout")])
        self.assertEqual(mod.round_table(calls)["gpt/B"]["last_err"], 1)

    def test_single_model_and_retry_rounds(self):
        calls = dict([call("a", "A", 1000, 5, rnd="solo"),
                      call("a1", "A", 1000, 5, rnd="r2", status="error"), call("a2", "A", 1060, 50, rnd="r2"),
                      call("b", "B", 1030, 30, rnd="r2")])
        t = mod.round_table(calls)
        self.assertEqual(t["gpt/A"]["rounds"], 1)  # the solo round is not a round of 2+, the retry counts once
        self.assertEqual(t["gpt/A"]["gaps"], [30])

    def test_since_drops_whole_rounds(self):
        calls = dict([call("a", "A", 1000, 100), call("b", "B", 2000, 10)])  # started at 900
        self.assertEqual(mod.round_table(calls, since=950), {})
        self.assertEqual(len(mod.round_table(calls, since=900)), 2)


class WidthTests(unittest.TestCase):
    def test_width_follows_the_longest_label(self):
        self.assertEqual(mod.name_width(["short"]), 34)
        long = "openrouter/stealth/space-bunny-alpha via Stealth"
        self.assertEqual(mod.name_width(["short", long]), len(long))
        self.assertEqual(mod.name_width(["x" * 100]), 100)  # never cut: the label is the row's key


class LogCliTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.log = Path(self.dir.name) / "log.jsonl"
        self.env = {**os.environ, "CONSULT_LOG": str(self.log)}
        self.env.pop("CONSULT_ROUND", None)

    def tearDown(self):
        self.dir.cleanup()

    def run_log(self, *args, stdin=None):
        return subprocess.run([sys.executable, str(CONSULT), "log", "--skill", "gpt", "--model", "m", *args],
                              input=stdin, text=True, capture_output=True, env=self.env)

    def records(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_error_text_is_classified_and_never_stored(self):
        canary = "PROMPT_CANARY_7f3a"
        r = self.run_log("--status", "error", "--error-text-file", "-",
                         stdin=f"HTTP 429: Too Many Requests; request echoed: {canary}")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertNotIn(canary, self.log.read_text())
        self.assertEqual(self.records()[-1]["error_kind"], "limit")

    def test_explicit_kind(self):
        self.run_log("--status", "error", "--error-kind", "timeout")
        self.assertEqual(self.records()[-1]["error_kind"], "timeout")

    def test_error_without_reason_has_no_kind(self):
        self.run_log("--status", "error")
        self.assertNotIn("error_kind", self.records()[-1])

    def test_error_fields_are_rejected_on_ok(self):
        r = self.run_log("--status", "ok", "--error-kind", "limit")
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
