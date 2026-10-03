"""Offline tests for consult.py: error kinds, latency percentiles, the rounds table and column widths."""
import importlib.util
import calendar
import json
import os
import subprocess
import sys
import tempfile
import time
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


class DisplayTests(unittest.TestCase):
    def test_strip_via_only_the_author(self):
        self.assertEqual(mod.strip_via("xiaomi/mimo-v2.6-pro via Xiaomi"), "xiaomi/mimo-v2.6-pro")
        self.assertEqual(mod.strip_via("stealth/space-bunny-alpha via Stealth"), "stealth/space-bunny-alpha")
        for kept in ("xiaomi/mimo-v2.6-pro via DeepInfra", "xiaomi/mimo via unknown-provider", "mimo via Mimo",
                     "deepseek-flash"):
            self.assertEqual(mod.strip_via(kept), kept)

    def test_display_keeps_effort_and_repo_mode(self):
        c = {"skill": "openrouter", "model": "xiaomi/mimo-v2.6-pro via Xiaomi", "effort": "low", "mode": "repo"}
        self.assertEqual(mod.display_names([c]), {mod.label(c): "openrouter/xiaomi/mimo-v2.6-pro@low -r"})

    def test_names_that_would_coincide_stay_full(self):
        versioned = {"skill": "openrouter", "model": "xiaomi/mimo-v2.6-pro via Xiaomi"}
        alias = {"skill": "openrouter", "model": "xiaomi/mimo-v2.6-pro"}  # logged without a version
        names = mod.display_names([versioned, alias])
        self.assertEqual(names[mod.label(versioned)], mod.label(versioned))
        self.assertEqual(names[mod.label(alias)], mod.label(alias))


class TableTests(unittest.TestCase):
    def test_columns_grow_with_their_widest_cell(self):
        lines = mod.table(["name", "acc/find", "note"], [["a", "12/13", "x"], ["b", "10213/99999", "longer"]], "<><")
        self.assertEqual(len(lines[0].split("acc/find")[0]), 34 + 1 + len("10213/99999") - len("acc/find"))
        self.assertTrue(lines[2].endswith(" 12/13 x"))
        self.assertTrue(lines[3].endswith(" 10213/99999 longer"))
        self.assertEqual(len(lines[1]), 34 + 1 + 11 + 1 + 6)
        self.assertFalse(any(line != line.rstrip() for line in lines))

    def test_empty_table_has_header_and_rule(self):
        self.assertEqual(len(mod.table(["name", "n"], [], "<>")), 2)


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


LONG = "openrouter/stealth/space-bunny-alpha via Stealth"


class BandTests(unittest.TestCase):
    header = ["skill/model", "uniq/call", "wrong", "rated", "err", "score", "acc/find", "unique", "lat n", "p50 s",
              "p90 s", "out/call"]
    rows = [[LONG, "0.58", "25%", "24/29", "3%", "0.75", "113/150", "14", "28", "58", "378", "9.0k"],
            ["gpt/gpt-6.1-sol", "1.48", "13%", "140/167", "4%", "0.96", "757/875", "207", "161", "44", "152", "1.3k"]]

    def bands(self, lines):
        out, cur = [], []
        for line in lines:
            if line:
                cur.append(line)
            else:
                out.append(cur)
                cur = []
        return out + [cur]

    def test_without_width_or_when_it_fits_the_table_is_whole(self):
        whole = mod.table(self.header, self.rows, "<" + ">" * 11)
        self.assertEqual(mod.table(self.header, self.rows, "<" + ">" * 11, width=len(whole[1])), whole)
        self.assertNotIn("", whole)

    def test_bands_repeat_names_and_fit(self):
        for width in (60, 80, 100):
            with self.subTest(width=width):
                lines = mod.table(self.header, self.rows, "<" + ">" * 11, width)
                self.assertTrue(all(len(line) <= width for line in lines))
                bands = self.bands(lines)
                self.assertGreater(len(bands), 1)
                seen = []
                for band in bands:
                    self.assertTrue(band[0].startswith("skill/model"))
                    self.assertEqual([line.split()[0] for line in band[2:]], [LONG.split()[0], "gpt/gpt-6.1-sol"])
                    seen += band[0].replace("lat n", "lat_n").replace("p50 s", "p50_s").replace("p90 s", "p90_s").split()[1:]
                self.assertEqual(seen, [h.replace(" ", "_") for h in self.header[1:]])  # every column once, in order

    def test_a_group_stays_together_when_it_fits(self):
        lines = mod.table(self.header, self.rows, "<" + ">" * 11, 100, groups=[4, 3, 3, 1])
        heads = [band[0] for band in self.bands(lines)]
        self.assertTrue(any("lat n p50 s p90 s" in h for h in heads))
        self.assertFalse(any(h.rstrip().endswith("score") for h in heads))

    def test_a_name_too_wide_for_any_column_keeps_the_table_whole(self):
        rows = [["x" * 70, "1"], ["y", "22"]]
        self.assertEqual(mod.table(["name", "n"], rows, "<>", 60), mod.table(["name", "n"], rows, "<>"))


class SayTests(unittest.TestCase):
    def say(self, *args, **kw):
        import contextlib, io
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            mod.say(*args, **kw)
        return out.getvalue()

    def test_without_width_prints_as_is(self):
        self.assertEqual(self.say("\na b\nc", None), "\na b\nc\n")

    def test_paragraph_is_reflowed_and_keeps_its_leading_blank_line(self):
        text = self.say("\nalpha beta-gamma\ndelta epsilon zeta eta", 12)
        self.assertTrue(text.startswith("\nalpha\n"))
        self.assertIn("beta-gamma", text)  # not broken at the hyphen
        self.assertTrue(all(len(line) <= 12 for line in text.split("\n")))

    def test_items_wrap_with_an_indent(self):
        self.assertEqual(self.say("one two three\nfour", 9, items=True), "one two\n  three\nfour\n")

    def test_a_long_word_overflows_instead_of_breaking(self):
        self.assertEqual(self.say("ab " + "x" * 20, 10, items=True), "ab\n  " + "x" * 20 + "\n")


class WidthCliTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        log = Path(self.dir.name) / "log.jsonl"
        recs = []
        for i, (skill, model) in enumerate([("openrouter", "stealth/space-bunny-alpha via Stealth"),
                                            ("deepseek", "DeepSeek-V4.1-Flash"), ("gpt", "gpt-6.1-sol")] * 6):
            cid = f"c{i:02d}"
            recs.append({"type": "call", "id": cid, "ts": 1000 + i, "skill": skill, "model": model, "status": "ok",
                         "seconds": 30 + i, "round": f"20261003-0000{i // 3:02d}-aaaa", "cwd": "/tmp/run.x",
                         "usage": {"output": 1000 + 100 * i}})
            recs.append({"type": "rating", "id": cid, "verdict": "useful", "findings": 4, "accepted": 3, "unique": 1})
        log.write_text("".join(json.dumps(r) + "\n" for r in recs))
        self.env = {**os.environ, "CONSULT_LOG": str(log)}

    def tearDown(self):
        self.dir.cleanup()

    def run_cli(self, *args):
        return subprocess.run([sys.executable, str(CONSULT), *args], text=True, capture_output=True, env=self.env)

    def test_every_mode_fits_the_width(self):
        for args in (["stats"], ["stats", "--pairs"], ["stats", "--all"], ["stats", "--vs", "bunny", "sol"],
                     ["recent", "-n", "40"]):
            for width in (60, 80, 100, 120):
                with self.subTest(args=args, width=width):
                    r = self.run_cli(*args, "--width", str(width))
                    self.assertEqual(r.returncode, 0, r.stderr)
                    long = [line for line in r.stdout.splitlines() if len(line) > width]
                    self.assertEqual(long, [])

    def test_a_narrow_width_is_rejected(self):
        r = self.run_cli("stats", "--width", "39")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("at least 40", r.stderr)

    def test_page_consult_without_a_terminal(self):
        # No /dev/tty: the width falls back to 100; less writes straight through when stdout is not a terminal.
        r = subprocess.run([str(Path(__file__).parent / "page-consult"), "stats", "--pairs"], text=True,
                           capture_output=True, env=self.env, stdin=subprocess.DEVNULL, start_new_session=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("gpt/gpt-6.1-sol", r.stdout)
        self.assertTrue(all(len(line) <= 100 for line in r.stdout.splitlines()))


if __name__ == "__main__":
    unittest.main()


class VsTests(unittest.TestCase):
    def run_vs(self, calls, ratings, **kw):
        import contextlib, io
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            mod.print_vs(dict(calls), ratings, "mimo", "deepseek", **kw)
        return out.getvalue()

    def rounds(self):
        calls, ratings = [], {}
        for i, (um, ud) in enumerate([(2, 0), (1, 1), (0, 1)]):
            rnd = f"2026100{i + 1}-000000-aaaa"
            for cid, model, skill, u in ((f"m{i}", "xiaomi/mimo-v2.6-pro", "openrouter", um),
                                         (f"d{i}", "DeepSeek-V4.1-Flash", "deepseek", ud)):
                calls.append(call(cid, model, 0, 10, rnd=rnd, skill=skill))
                ratings[cid] = {"verdict": "useful", "findings": 4, "accepted": 3, "unique": u}
        return calls, ratings

    def test_pairs_only_shared_rated_rounds(self):
        calls, ratings = self.rounds()
        calls.append(call("x", "DeepSeek-V4.1-Flash", 0, 10, rnd="20261009-000000-bbbb", skill="deepseek"))
        calls.append(call("e", "xiaomi/mimo-v2.6-pro", 0, 10, status="error", rnd="20261009-000000-bbbb",
                          skill="openrouter"))
        text = self.run_vs(calls, ratings)
        self.assertIn("3 shared rated rounds", text)
        self.assertIn("0 rounds with only mimo, 1 with only deepseek", text)
        self.assertIn("paired uniq/call mimo - deepseek: +0.33", text)
        self.assertIn("W/T/L 1/1/1", text)
        self.assertIn("rejected share mimo - deepseek: +0.0 points", text)

    def test_since_and_rounds_cut_chronologically(self):
        calls, ratings = self.rounds()
        self.assertIn("1 shared rated rounds (20261002-000000-aaaa", self.run_vs(calls, ratings, since="20261001-000000-aaaa", limit=1))
        self.assertIn("1 later rounds left out", self.run_vs(calls, ratings, since="20261001-000000-aaaa", limit=1))

    def test_bootstrap_is_seeded(self):
        data = list(range(10))
        self.assertEqual(mod.bootstrap_ci(data, lambda xs: sum(xs) / len(xs)),
                         mod.bootstrap_ci(data, lambda xs: sum(xs) / len(xs)))


class DateRangeTests(unittest.TestCase):
    def setUp(self):
        self.tz = os.environ.get("TZ")
        os.environ["TZ"] = "UTC"
        time.tzset()

    def tearDown(self):
        if self.tz is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = self.tz
        time.tzset()

    def ts(self, day):
        return calendar.timegm(time.strptime(day, "%Y-%m-%d")) + 3600

    def test_one_day_is_shown_once(self):
        now = self.ts("2026-10-03")
        self.assertEqual(mod.date_range(self.ts("2026-09-25"), self.ts("2026-09-25") + 600, now), "09-25")

    def test_a_range_in_the_current_year_has_no_year(self):
        now = self.ts("2026-10-03")
        self.assertEqual(mod.date_range(self.ts("2026-09-25"), self.ts("2026-09-28"), now), "09-25..09-28")

    def test_both_ends_get_the_year_when_one_is_outside_it(self):
        now = self.ts("2027-01-05")
        self.assertEqual(mod.date_range(self.ts("2026-12-28"), self.ts("2027-01-03"), now),
                         "2026-12-28..2027-01-03")
        self.assertEqual(mod.date_range(self.ts("2026-12-28"), self.ts("2026-12-30"), now), "2026-12-28..2026-12-30")
