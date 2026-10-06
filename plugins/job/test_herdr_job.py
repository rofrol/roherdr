"""Regression checks for the job footer's terminal control sequences."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import re
import sys
import time
import unicodedata
import unittest
from unittest.mock import patch, Mock
import tempfile
import json
from types import SimpleNamespace

JOB = runpy.run_path(str(Path(__file__).with_name("herdr-job"))) if os.name == "posix" else {}
Footer = JOB.get("Footer")


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class ExecutorFooterTests(unittest.TestCase):
    def test_registration_uses_actual_cli_contract_and_preserves_rejection(self):
        register = JOB["register_job_metadata"]
        meta = {"id": "probe", "name": "build 雪", "why": "check", "origin": "agent",
                "owner_pane": "p1", "tab_id": "t1"}
        for accepted in (True, False):
            call = Mock(return_value=accepted)
            with patch.dict(register.__globals__, {"herdr_ok": call}):
                self.assertEqual(register(meta), accepted)
            self.assertEqual(call.call_args.args[:3], ("tab", "job-metadata", "t1"))
            payload = json.loads(call.call_args.args[3])
            self.assertEqual(payload, {k: v for k, v in meta.items() if k != "tab_id"})

    def test_job_started_from_a_job_tab_nests_under_its_top_level_parent(self):
        top = JOB["top_level_tab"]
        reply = lambda tab: json.dumps({"result": {"tab": tab}})
        for tab, expected in ({"tab_id": "w:t1"}, "w:t1"), ({"tab_id": "w:t2", "parent_tab_id": "w:t1"}, "w:t1"):
            with patch.dict(top.__globals__, {"herdr": Mock(return_value=reply(tab))}):
                self.assertEqual(top("w:t2" if "parent_tab_id" in tab else "w:t1"), expected)
        with patch.dict(top.__globals__, {"herdr": Mock(side_effect=SystemExit("gone"))}):
            self.assertEqual(top("w:t9"), "w:t9")

    def test_job_tab_is_created_nested_and_falls_back_on_older_herdr(self):
        create_tab = JOB["create_job_tab"]
        created = json.dumps({"result": {"tab": {"tab_id": "w:t5"}}})
        call = Mock(return_value=created)
        nest = Mock(return_value=True)
        with patch.dict(create_tab.__globals__, {"herdr": call, "herdr_ok": nest}):
            self.assertEqual(create_tab(["tab", "create"], "w", "w:t1")["tab"]["tab_id"], "w:t5")
        self.assertEqual(call.call_args.args, ("tab", "create", "--parent", "w:t1"))
        nest.assert_not_called()
        # An older herdr rejects --parent: create in the workspace, then nest.
        call = Mock(side_effect=['{"error": {"code": "invalid_request"}}', created])
        with patch.dict(create_tab.__globals__, {"herdr": call, "herdr_ok": nest}):
            self.assertEqual(create_tab(["tab", "create"], "w", "w:t1")["tab"]["tab_id"], "w:t5")
        self.assertEqual(call.call_args.args, ("tab", "create", "--workspace", "w"))
        self.assertEqual(nest.call_args.args, ("tab", "parent", "w:t5", "w:t1"))

    def test_registration_sanitizes_display_text_without_changing_job_identity(self):
        register = JOB["register_job_metadata"]
        meta = {"id": "probe", "name": "Build\n雪\x1b", "why": "check\noutput\tready",
                "origin": "agent\x00", "owner_pane": "p1", "tab_id": "t1"}
        call = Mock(return_value=True)
        with patch.dict(register.__globals__, {"herdr_ok": call}):
            self.assertTrue(register(meta))
        payload = json.loads(call.call_args.args[3])
        self.assertEqual(payload["name"], "Build 雪")
        self.assertEqual(payload["why"], "check output ready")
        self.assertEqual(payload["origin"], "agent")
        self.assertEqual(payload["id"], "probe")
        self.assertEqual(payload["owner_pane"], "p1")
        self.assertEqual(meta["why"], "check\noutput\tready")

    def test_rejected_launch_clears_only_registered_client_metadata(self):
        launch = JOB["launch_job_executor"]
        for registered in (True, False):
            with self.subTest(registered=registered):
                meta = {"pane_id": "p1", "tab_id": "t1", "client_footer": registered}
                run = Mock(side_effect=SystemExit("launch rejected"))
                clear = Mock(return_value=True)
                with patch.dict(launch.__globals__, {"herdr": run, "herdr_ok": clear}):
                    with self.assertRaisesRegex(SystemExit, "launch rejected"):
                        launch(meta, "probe")
                if registered:
                    clear.assert_called_once_with("tab", "job-metadata", "t1", "null")
                else:
                    clear.assert_not_called()

    def test_registration_bounds_display_text_and_keeps_optional_reason(self):
        register = JOB["register_job_metadata"]
        meta = {"id": "probe", "name": "雪" * 5000, "why": None,
                "origin": "agent", "owner_pane": None, "tab_id": "t1"}
        call = Mock(return_value=True)
        with patch.dict(register.__globals__, {"herdr_ok": call}):
            self.assertTrue(register(meta))
        payload = json.loads(call.call_args.args[3])
        self.assertLessEqual(len(payload["name"].encode()), 4096)
        self.assertIsNone(payload["why"])

    def test_client_footer_streams_clean_output_and_legacy_calls_footer(self):
        for client_footer in (True, False):
            with self.subTest(client_footer=client_footer), tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp)
                meta = {"id": "probe", "name": "test", "command": "printf 'marker-1\\nmarker-2\\n'",
                        "cwd": tmp, "keep": True, "tab_status": True, "client_footer": client_footer}
                executor = JOB["cmd_exec"]
                footer = Mock()
                output = io.BytesIO()
                stream = io.TextIOWrapper(output, encoding="utf-8", write_through=True)
                overrides = {"job_dir": lambda _: path, "read_meta": lambda _: dict(meta),
                             "write_meta": Mock(), "Footer": Mock(return_value=footer),
                             "set_final_tab_status": Mock(), "update_owner_token": Mock(), "notify": Mock()}
                with patch.dict(executor.__globals__, overrides), \
                        patch.object(executor.__globals__["signal"], "signal") as signal_mock, \
                        contextlib.redirect_stdout(stream):
                    with self.assertRaises(SystemExit) as result:
                        executor(SimpleNamespace(id="probe"))
                self.assertEqual(result.exception.code, 0)
                self.assertEqual((path / "log").read_bytes(), b"marker-1\nmarker-2\n")
                self.assertNotIn(b"\x1b", output.getvalue())
                if client_footer:
                    footer.start.assert_not_called()
                    footer.finish.assert_not_called()
                    footer.release.assert_not_called()
                    self.assertNotIn(executor.__globals__["signal"].SIGWINCH,
                                     [call.args[0] for call in signal_mock.call_args_list])
                else:
                    footer.start.assert_called_once()
                    footer.finish.assert_called_once_with(0)
                    footer.release.assert_called_once()


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class FooterTests(unittest.TestCase):
    def setUp(self):
        self.footer = Footer({"id": "probe", "name": "test", "why": "check", "origin": "agent"})
        self.output = io.StringIO()

    def capture(self, method, rows=24, *args):
        with patch.object(self.footer, "size", return_value=os.terminal_size((100, rows))):
            with contextlib.redirect_stdout(self.output):
                method(*args)
        return self.output.getvalue()

    def test_start_reserves_last_row_and_keeps_region_at_row_one(self):
        output = self.capture(self.footer.start)
        self.assertIn("\x1b[1;23r\x1b[1;1H", output)
        self.assertIn("\x1b7\x1b[24;1H\x1b[2K", output)
        for text in (" ← ", "⧖", "test", "check", "agent", "herdr-job probe", " × "):
            self.assertIn(text, output)
        self.assertTrue(output.endswith("\x1b8"))

    def test_resize_moves_footer_and_preserves_cursor(self):
        output = self.capture(self.footer.resize, 12)
        self.assertIn("\x1b7\x1b[1;11r\x1b8", output)
        self.assertIn("\x1b7\x1b[12;1H", output)

    def test_finish_uses_current_last_row_and_exit_status(self):
        for code, expected in ((0, "✓"), (7, "! exit 7")):
            with self.subTest(code=code):
                self.output = io.StringIO()
                output = self.capture(self.footer.finish, 30, code)
                self.assertIn("\x1b7\x1b[30;1H", output)
                self.assertIn(expected, output)

    def test_growth_clears_old_footer_before_expanding_region(self):
        self.capture(self.footer.start, 12)
        self.output = io.StringIO()
        output = self.capture(self.footer.resize, 24)
        self.assertIn("\x1b7\x1b[12;1H\x1b[2K\x1b[1;23r\x1b8", output)
        self.assertIn("\x1b7\x1b[24;1H", output)

    def test_shrink_does_not_clear_an_output_row(self):
        self.capture(self.footer.start, 24)
        self.output = io.StringIO()
        output = self.capture(self.footer.resize, 12)
        self.assertNotIn("\x1b[24;1H", output)

    def test_width_preserves_buttons_without_wrapping(self):
        for name in ("a long job name", "界" * 40, "e\u0301" * 40):
            for cols in range(6, 101):
                with self.subTest(name=name, cols=cols):
                    self.footer.meta["name"] = name
                    self.footer.rows = 24
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output):
                        self.footer.draw(cols)
                    text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]|\x1b[78]", "", output.getvalue())
                    cells = sum(0 if unicodedata.category(c).startswith("M") else
                                2 if unicodedata.east_asian_width(c) in ("W", "F") else 1
                                for c in text)
                    self.assertEqual(cells, cols)
                    self.assertTrue(text.startswith(" ← "))
                    self.assertTrue(text.endswith(" × "))

    def test_clip_rejects_terminal_controls_and_counts_wide_cells(self):
        self.assertEqual(Footer.clip("界界", 3), ("界", 2))
        self.assertEqual(Footer.clip("e\u0301", 1), ("e\u0301", 1))
        self.assertNotIn("\x1b", Footer.clip("\x1b[2J", 20)[0])

    def test_tiny_pane_does_not_emit_an_invalid_region(self):
        self.assertEqual(self.capture(self.footer.start, 3), "")


if __name__ == "__main__":
    unittest.main()


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class OpenJobTests(unittest.TestCase):
    JOB = "20261001-124548-1827"

    def run_open(self, args, env_url=None, tab=None, meta=None, exists=True):
        cmd_open = JOB["cmd_open"]
        focused = []
        base = Path(tempfile.mkdtemp())
        path = base / self.JOB
        if exists:
            path.mkdir()
        def herdr(*a, **k):
            if a[:2] == ("tab", "get"):
                if tab is None:
                    return json.dumps({"result": {"tab": {"job": {"id": self.JOB}}}})
                return tab
            focused.append(a)
            return "{}"

        patches = {
            "job_dir": lambda job_id: base / job_id,
            "read_meta": lambda _path: meta or {"tab_id": "w:t5"},
            "herdr": herdr,
        }
        env = {"HERDR_PLUGIN_CLICKED_URL": env_url} if env_url is not None else {}
        with patch.dict(cmd_open.__globals__, patches), patch.dict(os.environ, env):
            try:
                cmd_open(args)
                return focused, None
            except SystemExit as error:
                return focused, str(error)

    def test_a_job_link_focuses_the_jobs_tab(self):
        args = SimpleNamespace(from_click=True, id=None)
        focused, error = self.run_open(args, f"herdr-job://{self.JOB}")
        self.assertEqual((focused, error), ([("tab", "focus", "w:t5")], None))
        focused, error = self.run_open(SimpleNamespace(from_click=False, id=self.JOB))
        self.assertEqual(focused, [("tab", "focus", "w:t5")])

    def test_anything_but_a_well_formed_job_link_is_refused(self):
        args = SimpleNamespace(from_click=True, id=None)
        for url in ("", "https://example.com", f"herdr-job://{self.JOB}/x", f"herdr-job://{self.JOB}?a=1",
                    "herdr-job://../../etc", f"herdr-job:{self.JOB}"):
            focused, error = self.run_open(args, url)
            self.assertEqual(focused, [], url)
            self.assertIn("not a job link", error or "", url)

    def test_a_missing_job_or_a_gone_or_reused_tab_focuses_nothing(self):
        args = SimpleNamespace(from_click=False, id=self.JOB)
        focused, error = self.run_open(args, exists=False)
        self.assertEqual(focused, [])
        self.assertIn("no such job", error)
        # The tab is gone (no answer), or another job holds its id now.
        for answer in ("", json.dumps({"result": {"tab": {"job": {"id": "20260101-000000-abcd"}}}}),
                       json.dumps({"result": {"tab": {}}})):
            focused, error = self.run_open(args, tab=answer)
            self.assertEqual(focused, [], answer)
            self.assertIn("is gone", error, answer)


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class WaitTests(unittest.TestCase):
    JOB = "20261001-124548-1827"

    def wait(self, log, outcome=("ok", 0), polls=1, **flags):
        cmd_wait = JOB["cmd_wait"]
        base = Path(tempfile.mkdtemp())
        path = base / self.JOB
        path.mkdir()
        (path / "log").write_bytes(log)
        states = iter([("running", None)] * polls + [outcome])
        patches = {
            "job_dir": lambda _id: path,
            "read_meta": lambda _path: {"name": "build", "tab_id": "w:t7", "owner_pane": None},
            "status": lambda _path, meta=None: next(states),
            "reconcile_tabs": lambda: None,
            "update_owner_token": lambda _pane: None,
        }
        args = SimpleNamespace(id=self.JOB, quiet=flags.get("quiet", False), stream=flags.get("stream", False))
        out = io.TextIOWrapper(io.BytesIO(), encoding="utf-8")
        with patch.dict(cmd_wait.__globals__, patches), patch.object(sys, "stdout", out), \
                patch.object(time, "sleep", lambda _s: None):
            try:
                cmd_wait(args)
                code = 0
            except SystemExit as error:
                code = error.code
        out.flush()
        return code, out.buffer.getvalue().decode()

    def test_by_default_a_job_is_a_start_line_and_the_final_line(self):
        code, text = self.wait(b"".join(b"line %d\n" % n for n in range(5000)), polls=3)
        self.assertEqual(code, 0)
        lines = [line for line in text.split("\n") if line]
        self.assertEqual(len(lines), 2, text)
        self.assertIn("waiting; to look at it: herdr tab focus w:t7", lines[0])
        self.assertEqual(lines[1], f"herdr-job {self.JOB} (build): ok, exit 0")

    def test_a_failure_adds_a_bounded_tail_without_escapes_and_keeps_the_code(self):
        log = b"".join(b"\x1b[31mline %d\x1b[0m\n" % n for n in range(5000))
        code, text = self.wait(log, outcome=("failed", 42))
        self.assertEqual(code, 42)
        self.assertIn("line 4999", text)
        self.assertNotIn("line 4900", text)
        self.assertNotIn("\x1b", text)
        self.assertLessEqual(text.count("line "), 41)
        self.assertTrue(text.rstrip().endswith(f"herdr-job {self.JOB} (build): failed, exit 42"))

    def test_stream_follows_the_whole_log_and_quiet_prints_only_the_final_line(self):
        log = b"a\nb\nc\n"
        code, text = self.wait(log, stream=True)
        self.assertEqual(text, f"a\nb\nc\n\nherdr-job {self.JOB} (build): ok, exit 0\n")
        code, text = self.wait(log, quiet=True, outcome=("failed", 3))
        self.assertEqual((code, text), (3, f"\nherdr-job {self.JOB} (build): failed, exit 3\n"))

    def test_a_missing_log_is_not_an_error(self):
        code, text = self.wait(b"", outcome=("failed", 1))
        self.assertEqual(code, 1)
        self.assertNotIn("last lines", text)


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class IdleJobTests(unittest.TestCase):
    def test_cputime_formats(self):
        parse = JOB["parse_cputime"]
        self.assertEqual(parse("0:01.50"), 1.5)
        self.assertEqual(parse("12:34.00"), 12 * 60 + 34)
        self.assertEqual(parse("1:02:03"), 3723)
        self.assertEqual(parse("2-00:00:10"), 2 * 86400 + 10)

    def test_the_tree_sums_the_job_and_its_descendants_only(self):
        tree = JOB["tree_cpu_seconds"]
        ps = "\n".join(
            ["100 1 0:01.00", "101 100 0:02.00", "102 101 0:04.00", "200 1 5:00.00", "bad line"]
        )
        self.assertEqual(tree(ps, 100), 7.0)
        self.assertIsNone(tree(ps, 999))

    def test_a_job_is_idle_only_after_a_quiet_window_with_almost_no_cpu(self):
        idle = JOB["job_is_idle"]
        window = JOB["IDLE_WINDOW_S"]
        step = JOB["SAMPLE_INTERVAL_S"]
        times = list(range(0, window + 2 * step, step))
        flat = [(t, 100.0) for t in times]
        now = times[-1]
        self.assertTrue(idle(flat, last_output=0, now=now))
        # Output inside the window means it works.
        self.assertFalse(idle(flat, last_output=now - 10, now=now))
        # CPU use above 2% of a core means it works.
        busy = [(t, 100.0 + t * 0.5) for t in times]
        self.assertFalse(idle(busy, last_output=0, now=now))
        # Too few samples to say.
        self.assertFalse(idle(flat[-2:], last_output=0, now=now))
        self.assertFalse(idle([], last_output=0, now=now))


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class ReconcileTests(unittest.TestCase):
    def reconcile(self, tab_status):
        reconcile = JOB["reconcile_tabs"]
        tab = {"tab_id": "w:t1", "label": "rescue", "focused": False}
        if tab_status is not None:
            tab["status"] = tab_status
        meta = {"tab_id": "w:t1", "tab_status": True, "keep": True}
        calls = Mock(return_value=True)
        with patch.dict(reconcile.__globals__, {
            "list_tabs": lambda: [tab],
            "job_tabs": lambda tabs: {"w:t1": (Path("/nonexistent"), meta)},
            "status": lambda path, meta: ("lost", JOB["EXIT_LOST"]),
            "herdr_ok": calls,
        }):
            reconcile()
        return [call.args for call in calls.call_args_list]

    def test_a_lost_job_whose_tab_lost_its_status_on_a_cold_restart_is_marked_failed(self):
        for tab_status in ("running", None):
            self.assertEqual(self.reconcile(tab_status), [("tab", "status", "w:t1", "failed")])

    def test_a_tab_that_already_shows_an_outcome_is_left_alone(self):
        self.assertEqual(self.reconcile("failed"), [])


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class SlotTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        take = JOB["take_slot"]
        patcher = patch.dict(take.__globals__, {"SLOTS": Path(self.tmp.name) / "slots", "SLOT_POLL_S": 0.01})
        patcher.start()
        self.addCleanup(patcher.stop)
        env = patch.dict(os.environ, {"HERDR_JOB_SLOTS": "1"})
        env.start()
        self.addCleanup(env.stop)
        os.environ.pop("HERDR_JOB_SLOT", None)

    def take(self, name, exclusive=False, on_wait=None):
        """take_slot as a separate requester: its env mark must not make the next one nest."""
        held = JOB["take_slot"](name, exclusive, on_wait=on_wait)
        os.environ.pop("HERDR_JOB_SLOT", None)
        return held

    def take_in_thread(self, name, exclusive=False):
        import threading
        heard, result = [], []
        waiting = threading.Event()

        def on_wait(text):
            heard.append(text)
            waiting.set()

        thread = threading.Thread(target=lambda: result.append(self.take(name, exclusive, on_wait)))
        thread.start()
        self.assertTrue(waiting.wait(5), "the second request did not wait")
        return thread, heard, result

    def release(self, held):
        for handle in held:
            handle.close()

    def test_a_second_request_waits_and_names_the_holder_until_the_slot_is_free(self):
        first = self.take("cargo build")
        thread, heard, result = self.take_in_thread("just test")
        self.assertIn("cargo build", heard[0])
        self.assertTrue(thread.is_alive())
        self.release(first)
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(len(result[0]), 1)
        self.assertIn("just test", JOB["describe_holder"](JOB["slot_holder"](JOB["slot_files"]()[0])))
        self.release(result[0])
        self.assertIsNone(JOB["slot_holder"](JOB["slot_files"]()[0]))

    def test_exclusive_takes_every_slot_and_waits_for_a_shared_holder(self):
        os.environ["HERDR_JOB_SLOTS"] = "2"
        shared = self.take("tests")
        thread, heard, result = self.take_in_thread("bench", exclusive=True)
        self.assertIn("tests", heard[0])
        self.release(shared)
        thread.join(5)
        self.assertEqual(len(result[0]), 2)
        late, heard, late_result = self.take_in_thread("late")
        self.assertEqual(heard[0].count("bench"), 2)
        self.release(result[0])
        late.join(5)
        self.release(late_result[0])

    def test_inside_a_slot_shared_needs_nothing_and_exclusive_fails_at_once(self):
        os.environ["HERDR_JOB_SLOT"] = "shared"
        self.assertEqual(JOB["take_slot"]("nested"), [])
        with self.assertRaises(SystemExit):
            JOB["take_slot"]("bench", exclusive=True)
        os.environ["HERDR_JOB_SLOT"] = "exclusive"
        self.assertEqual(JOB["take_slot"]("bench", exclusive=True), [])

    def test_zero_slots_turns_them_off(self):
        os.environ["HERDR_JOB_SLOTS"] = "0"
        self.assertEqual(JOB["take_slot"]("anything", exclusive=True), [])

    def test_the_slot_command_marks_the_child_and_keeps_its_exit_code(self):
        import subprocess
        env = dict(os.environ, XDG_STATE_HOME=self.tmp.name)
        env.pop("HERDR_JOB_SLOT", None)
        out = subprocess.run([str(Path(__file__).with_name("herdr-job")), "slot", "--", "sh", "-c",
                              'echo "$HERDR_JOB_SLOT"; exit 3'], env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual((out.returncode, out.stdout.strip()), (3, "shared"))
        out = subprocess.run([str(Path(__file__).with_name("herdr-job")), "slots"], env=env,
                             capture_output=True, text=True, timeout=30)
        self.assertEqual(out.stdout.strip(), "slot 0: free")

    def test_a_slot_job_holds_its_slot_while_the_command_runs(self):
        import subprocess
        job = Path(__file__).with_name("herdr-job")
        state = Path(self.tmp.name) / "herdr-job"
        (state / "j1").mkdir(parents=True)
        meta = {"id": "j1", "name": "slot job", "command": f"{job} slots", "cwd": self.tmp.name, "keep": True,
                "notify": "never", "owner_pane": None, "tab_id": "t1", "tab_status": False, "pane_id": "p1",
                "client_footer": True, "slot": "shared", "created": time.time()}
        (state / "j1" / "meta.json").write_text(json.dumps(meta))
        env = dict(os.environ, XDG_STATE_HOME=self.tmp.name, HERDR_BIN_PATH="/usr/bin/true")
        env.pop("HERDR_JOB_SLOT", None)
        out = subprocess.run([str(job), "_exec", "j1"], env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("slot 0: job j1 slot job", out.stdout)
