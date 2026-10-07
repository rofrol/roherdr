"""Regression checks for the job footer's terminal control sequences."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import re
import sys
import threading
import time
import unicodedata
import unittest
from unittest.mock import patch, Mock
import tempfile
import json
import subprocess
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


@unittest.skipUnless(sys.platform == "darwin", "caffeinate is macOS only")
class KeepAwakeTests(unittest.TestCase):
    def test_the_executor_holds_an_idle_sleep_assertion_until_it_exits(self):
        script = (f"import runpy; job = runpy.run_path({str(Path(__file__).with_name('herdr-job'))!r}); "
                  "job['keep_awake'](); import time; time.sleep(30)")
        executor = subprocess.Popen([sys.executable, "-c", script])
        try:
            def caffeinate():
                out = subprocess.run(["pgrep", "-f", f"caffeinate -i -w {executor.pid}$"],
                                     capture_output=True, text=True)
                return out.stdout.split()
            deadline = time.monotonic() + 10
            while not caffeinate() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(caffeinate())
        finally:
            executor.kill()
            executor.wait()
        # It goes with the executor, however that ends.
        deadline = time.monotonic() + 10
        while caffeinate() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(caffeinate(), [])

    def test_it_can_be_turned_off(self):
        popen = Mock()
        with patch.dict(os.environ, {"HERDR_JOB_KEEP_AWAKE": "0"}), \
                patch.object(JOB["subprocess"], "Popen", popen):
            JOB["keep_awake"]()
        popen.assert_not_called()


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
class WatchPidTests(unittest.TestCase):
    def watch(self, pid, started):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            JOB["cmd_watch_pid"](SimpleNamespace(pid=pid, started=started))
        return out.getvalue()

    def test_it_returns_when_the_process_exits_and_does_not_claim_its_status(self):
        child = subprocess.Popen(["sleep", "0.3"])
        started, command = JOB["process_identity"](child.pid)
        # Reap the child as its parent would, so its pid really goes away.
        threading.Thread(target=child.wait, daemon=True).start()
        out = self.watch(child.pid, started)
        self.assertIn(f"waiting for pid {child.pid}: sleep 0.3", out)
        self.assertIn("exit status is unknown", out)

    def test_a_reused_pid_is_not_waited_for(self):
        child = subprocess.Popen(["sleep", "30"])
        try:
            out = self.watch(child.pid, "Mon Jan 1 00:00:00 2001")
        finally:
            child.kill()
            child.wait()
        self.assertIn("gone already", out)


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

    def test_a_job_started_inside_a_slot_shares_it_instead_of_waiting_for_its_parent(self):
        job_slot = JOB["job_slot"]
        args = lambda slot=False, exclusive=False: SimpleNamespace(slot=slot, exclusive=exclusive)
        self.assertEqual((job_slot(args(slot=True)), job_slot(args(exclusive=True)), job_slot(args())),
                         ("shared", "exclusive", None))
        os.environ["HERDR_JOB_SLOT"] = "shared"
        self.assertIsNone(job_slot(args(slot=True)))
        with self.assertRaises(SystemExit):
            job_slot(args(exclusive=True))
        os.environ["HERDR_JOB_SLOT"] = "exclusive"
        self.assertIsNone(job_slot(args(exclusive=True)))


# A stand-in for the herdr CLI: it records every call and keeps the tabs it
# created in a file, so separate herdr-job processes see the same tabs.
HERDR_STUB = r"""#!/usr/bin/env python3
import fcntl, json, os, sys
d = os.environ["STUB_DIR"]
args = sys.argv[1:]
with open(os.path.join(d, "lock"), "a") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    with open(os.path.join(d, "calls"), "a") as calls:
        calls.write(json.dumps(args) + "\n")
    tabs_path = os.path.join(d, "tabs.json")
    tabs = json.load(open(tabs_path)) if os.path.exists(tabs_path) else []
    def out(result):
        print(json.dumps({"result": result}))
    if args[:2] == ["pane", "get"]:
        out({"pane": {"pane_id": args[2], "workspace_id": "w", "tab_id": "w:t0", "agent": "claude"}})
    elif args[:2] == ["workspace", "get"]:
        out({"workspace": {"label": "ws"}})
    elif args[:2] == ["tab", "get"]:
        out({"tab": {"tab_id": args[2]}})
    elif args[:2] == ["tab", "list"]:
        out({"tabs": tabs})
    elif args[:2] == ["tab", "create"]:
        n = len(tabs) + 1
        tabs.append({"tab_id": f"w:t{n}", "label": args[args.index("--label") + 1]})
        json.dump(tabs, open(tabs_path, "w"))
        out({"tab": {"tab_id": f"w:t{n}"}, "root_pane": {"pane_id": f"w:p{n}"}})
    elif args[:2] == ["tab", "close"]:
        json.dump([t for t in tabs if t["tab_id"] != args[2]], open(tabs_path, "w"))
    elif args[:1] == ["agent"]:
        # Answers queued by the test, one per call: [exit code, stdout, stderr].
        queue_path = os.path.join(d, "agent.json")
        queue = json.load(open(queue_path))
        code, stdout, stderr = queue.pop(0) if len(queue) > 1 else queue[0]
        json.dump(queue, open(queue_path, "w"))
        sys.stdout.write(stdout)
        sys.stderr.write(stderr)
        sys.exit(code)
"""


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class GuardTests(unittest.TestCase):
    """Caps and failure backoff, checked before the job's tab is created."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.stub_dir = root / "stub"
        self.stub_dir.mkdir()
        stub = root / "herdr"
        stub.write_text(HERDR_STUB)
        stub.chmod(0o755)
        self.state = root / "state" / "herdr-job"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "STUB_"))}
        self.env.update(XDG_STATE_HOME=str(root / "state"), HERDR_BIN_PATH=str(stub), STUB_DIR=str(self.stub_dir),
                        HERDR_SOCKET_PATH="/nonexistent", HERDR_PANE_ID="w:p0", HERDR_JOB_MAX_PER_OWNER="3")

    def run_job(self, name, *flags, env=None):
        return subprocess.run([str(Path(__file__).with_name("herdr-job")), "run", "--name", name, *flags,
                               "--", "false"], env={**self.env, **(env or {})},
                              capture_output=True, text=True, timeout=30)

    def tab_creates(self):
        calls = [json.loads(line) for line in (self.stub_dir / "calls").read_text().splitlines()]
        return sum(1 for call in calls if call[:2] == ["tab", "create"])

    def fail_all_jobs(self):
        """The stub never runs the executor: end every job as `_exec` would after a failure."""
        for path in self.state.iterdir():
            if (path / "meta.json").exists() and not (path / "exit").exists():
                meta = json.loads((path / "meta.json").read_text())
                meta["started"] = meta["ended"] = time.time()
                (path / "meta.json").write_text(json.dumps(meta))
                (path / "exit").write_text("1\n")

    def test_three_failed_jobs_fill_the_owner_cap_and_the_fourth_opens_no_tab(self):
        for n in range(3):
            out = self.run_job(f"build {n}")
            self.assertEqual(out.returncode, 0, out.stderr)
        self.fail_all_jobs()
        out = self.run_job("build 3")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("0 running and 3 failed job tabs (limit 3)", out.stderr)
        self.assertIn("herdr-job clean", out.stderr)
        self.assertEqual(self.tab_creates(), 3)

    def test_a_nested_job_counts_against_the_pane_that_started_the_first_one(self):
        self.assertEqual(self.run_job("outer").returncode, 0)
        outer = next(self.state.glob("2*"))
        # From the outer job's tab: its pane is not an owner of its own.
        for n in range(2):
            self.assertEqual(self.run_job(f"inner {n}", env={"HERDR_PANE_ID": "w:p1"}).returncode, 0)
        out = self.run_job("inner 2", env={"HERDR_PANE_ID": "w:p1", "HERDR_JOB_ID": outer.name})
        self.assertIn("pane w:p0 has 3 running", out.stderr)
        self.assertEqual(self.tab_creates(), 3)

    def test_the_oldest_failed_tabs_beyond_the_kept_number_close_and_keep_their_logs(self):
        env = {"HERDR_JOB_MAX_PER_OWNER": "10", "HERDR_JOB_MAX_FAILED_KEPT": "2"}
        for n in range(4):
            self.assertEqual(self.run_job(f"job {n}", env=env).returncode, 0)
        self.fail_all_jobs()
        self.assertEqual(self.run_job("job 4", env=env).returncode, 0)
        tabs = [tab["label"] for tab in json.loads((self.stub_dir / "tabs.json").read_text())]
        self.assertEqual(tabs, ["job 2", "job 3", "job 4"])
        self.assertEqual(len(list(self.state.glob("2*"))), 5)

    def test_the_global_cap_counts_every_owner(self):
        env = {"HERDR_JOB_MAX_GLOBAL": "2"}
        self.assertEqual(self.run_job("a", env=env).returncode, 0)
        self.assertEqual(self.run_job("b", env={**env, "HERDR_PANE_ID": "w:p9"}).returncode, 0)
        out = self.run_job("c", env={**env, "HERDR_PANE_ID": "w:p8"})
        self.assertIn("2 running and 0 failed job tabs in all panes (limit 2)", out.stderr)

    def test_concurrent_launches_cannot_exceed_the_cap(self):
        job = str(Path(__file__).with_name("herdr-job"))
        procs = [subprocess.Popen([job, "run", "--name", f"race {n}", "--", "false"], env=self.env,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for n in range(8)]
        codes = [proc.wait(timeout=60) for proc in procs]
        for proc in procs:
            proc.stdout.close()
            proc.stderr.close()
        self.assertEqual(codes.count(0), 3, codes)
        self.assertEqual(self.tab_creates(), 3)

    def test_a_name_that_failed_three_times_is_refused_even_after_a_restart(self):
        job = Path(__file__).with_name("herdr-job")
        with patch.dict(os.environ, {"XDG_STATE_HOME": self.env["XDG_STATE_HOME"]}):
            first = runpy.run_path(str(job))
            for _ in range(3):
                first["record_failure"]("w:p0", "wait w-docs")
            # A new process, as after a herdr server restart: the counters come from disk.
            second = runpy.run_path(str(job))
            self.assertEqual(second["recent_failures"]("w:p0", "wait w-docs"), 3)
            self.assertEqual(second["recent_failures"]("w:p1", "wait w-docs"), 0)
            self.assertEqual(second["recent_failures"]("w:p0", "wait w-docs", now=time.time() + 601), 0)
        out = self.run_job("wait w-docs", env={"HERDR_JOB_MAX_PER_OWNER": "16"})
        self.assertIn("failed 3 times in the last 10 minutes", out.stderr)
        self.assertFalse((self.stub_dir / "calls").exists() and self.tab_creates())
        self.assertEqual(self.run_job("wait w-docs", "--force").returncode, 0)

    def test_a_failed_job_is_counted_by_its_executor(self):
        self.assertEqual(self.run_job("probe").returncode, 0)
        path = next(self.state.glob("2*"))
        env = {**self.env, "HERDR_JOB_KEEP_AWAKE": "0"}
        out = subprocess.run([str(Path(__file__).with_name("herdr-job")), "_exec", path.name], env=env,
                             capture_output=True, text=True, timeout=30)
        self.assertEqual(out.returncode, 1, out.stderr)
        failures = json.loads((self.state / "failures.json").read_text())
        self.assertEqual(len(failures["w:p0\tprobe"]), 1)


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class WaitAgentTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        stub = root / "herdr"
        stub.write_text(HERDR_STUB)
        stub.chmod(0o755)
        self.stub_dir = root
        self.wait_agent = JOB["cmd_wait_agent"]
        self.sleeps = []
        patcher = patch.dict(self.wait_agent.__globals__, {"HERDR": str(stub)})
        patcher.start()
        self.addCleanup(patcher.stop)
        env = patch.dict(os.environ, {"STUB_DIR": str(root)})
        env.start()
        self.addCleanup(env.stop)

    def run_wait(self, answers, **flags):
        (self.stub_dir / "agent.json").write_text(json.dumps(answers))
        args = SimpleNamespace(pane="w:p5", until=flags.get("until", []), worker_line=flags.get("worker_line", False))
        out = io.StringIO()
        with patch.object(time, "sleep", self.sleeps.append), contextlib.redirect_stdout(out):
            try:
                self.wait_agent(args)
                code = 0
            except SystemExit as error:
                code = error.code
        calls = [json.loads(line) for line in (self.stub_dir / "calls").read_text().splitlines()]
        return code, out.getvalue(), calls

    def test_transport_errors_are_retried_with_backoff_until_the_state(self):
        empty = [1, "", "Error: empty api response\n"]
        down = [1, "", json.dumps({"error": {"code": "server_not_running", "message": "x"}}) + "\n"]
        done = [0, json.dumps({"result": {"agent": {"status": "done"}}}) + "\n", ""]
        code, text, calls = self.run_wait([empty, down, empty, done], until=["done"])
        self.assertEqual(code, 0, text)
        self.assertEqual(self.sleeps, [1, 2, 4])
        self.assertEqual(calls[0], ["agent", "wait", "w:p5", "--until", "done"])
        self.assertEqual(len(calls), 4)
        self.assertIn("reached", text)

    def test_a_live_handoff_is_retried_not_an_error(self):
        shutting = [1, "", json.dumps({"id": "", "error": {"code": "server_unavailable",
                                                           "message": "server is shutting down"}}) + "\n"]
        done = [0, json.dumps({"result": {"agent": {"status": "idle"}}}) + "\n", ""]
        code, text, calls = self.run_wait([shutting, shutting, done])
        self.assertEqual(code, 0, text)
        self.assertEqual((len(calls), self.sleeps), (3, [1, 2]))
        self.assertIn("server is shutting down", text)

    def test_a_gone_agent_ends_the_wait_without_a_retry(self):
        for error in ("agent_not_found", "agent_not_running"):
            with self.subTest(error=error):
                self.sleeps.clear()
                (self.stub_dir / "calls").unlink(missing_ok=True)
                gone = [1, "", json.dumps({"error": {"code": error, "message": "no agent"}}) + "\n"]
                code, text, calls = self.run_wait([gone])
                self.assertEqual((code, len(calls), self.sleeps), (3, 1, []))
                self.assertIn("gone", text)

    def test_an_error_that_waiting_cannot_fix_ends_the_wait(self):
        mismatch = [1, "", json.dumps({"error": {"code": "protocol_mismatch", "message": "x"}}) + "\n"]
        code, _, calls = self.run_wait([mismatch])
        self.assertEqual((code, len(calls), self.sleeps), (2, 1, []))

    def test_it_gives_up_after_fifteen_minutes_of_transport_failures(self):
        clock = [0.0]
        empty = [1, "", "Error: empty api response\n"]

        def sleep(seconds):
            self.sleeps.append(seconds)
            clock[0] += seconds

        (self.stub_dir / "agent.json").write_text(json.dumps([empty]))
        args = SimpleNamespace(pane="w:p5", until=[], worker_line=False)
        with patch.object(time, "sleep", sleep), patch.object(time, "monotonic", lambda: clock[0]), \
                contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(SystemExit) as result:
                self.wait_agent(args)
        self.assertEqual(result.exception.code, 5)
        self.assertEqual(max(self.sleeps), 30)
        self.assertGreaterEqual(clock[0], 15 * 60)

    def test_worker_line_ignores_the_task_text_and_ends_on_the_workers_last_line(self):
        task = "a last line `WORKER-DONE <sha> | <summary>` or\nWORKER-BLOCKED <reason>\n"
        working = [0, task + "⏺ Running the tests\n", ""]
        finished = [0, task + "⏺ Done.\n  WORKER-DONE 1a2b3c4 | caps and backoff\n", ""]
        code, text, calls = self.run_wait([working, working, finished], worker_line=True)
        self.assertEqual(code, 0)
        self.assertIn("WORKER-DONE 1a2b3c4 | caps and backoff", text)
        self.assertEqual(calls[0][:3], ["agent", "read", "w:p5"])
        blocked = [0, task + "WORKER-BLOCKED needs a login\n", ""]
        self.sleeps.clear()
        (self.stub_dir / "calls").unlink()
        code, text, _ = self.run_wait([blocked], worker_line=True)
        self.assertEqual(code, 4)

    def test_worker_line_reads_the_visible_screen_of_a_working_agent(self):
        busy = [1, "", json.dumps({"error": {"code": "agent_not_idle", "message": "cannot read 200 lines"}}) + "\n"]
        working = [0, "⏺ Editing herdr-job\n\n esc to interrupt\n", ""]
        finished = [0, "⏺ Committed.\n  WORKER-DONE 1a2b3c4 | visible screen\n\n> \n", ""]
        code, text, calls = self.run_wait([working, busy, working, finished], worker_line=True)
        self.assertEqual(code, 0, text)
        self.assertIn("WORKER-DONE 1a2b3c4 | visible screen", text)
        self.assertEqual(calls[0], ["agent", "read", "w:p5", "--source", "detection", "--format", "text"])
        self.assertEqual((len(calls), self.sleeps), (4, [3, 3, 3]))


@unittest.skipUnless(os.name == "posix", "herdr-job supports Unix only")
class CleanTreeTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.repo = Path(self.tmp.name) / "repo"
        self.tree = Path(self.tmp.name) / "clean"
        self.repo.mkdir()
        for args in (["init", "-q"], ["config", "user.email", "t@example.com"], ["config", "user.name", "t"]):
            self.git(*args)
        for name in ("mine.txt", "theirs.txt", "gone.txt"):
            (self.repo / name).write_text(f"{name} committed\n")
        (self.repo / ".gitignore").write_text("target/\n")
        self.git("add", ".")
        self.git("commit", "-q", "-m", "base")
        self.env = dict(os.environ, HERDR_CLEAN_TREE=str(self.tree))

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.repo), *args], check=True, capture_output=True)

    def run_tree(self, *args):
        return subprocess.run([str(Path(__file__).with_name("herdr-job")), "clean-tree", *args], cwd=self.repo,
                              env=self.env, capture_output=True, text=True, timeout=60)

    def test_only_the_named_paths_reach_the_clean_tree(self):
        (self.repo / "mine.txt").write_text("mine edited\n")
        (self.repo / "theirs.txt").write_text("theirs half done\n")
        (self.repo / "gone.txt").unlink()
        (self.repo / "sub").mkdir()
        (self.repo / "sub" / "new.txt").write_text("new file\n")
        (self.repo / "other_new.txt").write_text("another session's new file\n")
        out = self.run_tree("mine.txt", "gone.txt", "sub", "--", "sh", "-c",
                            "cat mine.txt theirs.txt sub/new.txt; ls")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("mine edited\ntheirs.txt committed\nnew file\n", out.stdout)
        self.assertNotIn("gone.txt", out.stdout)
        self.assertNotIn("other_new.txt", out.stdout)
        # The shared checkout is untouched.
        self.assertEqual((self.repo / "theirs.txt").read_text(), "theirs half done\n")

    def test_a_second_run_starts_clean_but_keeps_ignored_build_output(self):
        (self.repo / "mine.txt").write_text("mine edited\n")
        self.assertEqual(self.run_tree("mine.txt", "--", "sh", "-c", "mkdir -p target; touch target/warm stray").returncode, 0)
        out = self.run_tree("--", "sh", "-c", "cat mine.txt; ls; ls target")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("mine.txt committed", out.stdout)
        self.assertNotIn("stray", out.stdout)
        self.assertIn("warm", out.stdout)

    def test_the_command_exit_code_comes_back_and_paths_must_stay_inside(self):
        self.assertEqual(self.run_tree("--", "sh", "-c", "exit 7").returncode, 7)
        out = self.run_tree("../elsewhere", "--", "true")
        self.assertNotEqual(out.returncode, 0)
        self.assertIn("outside", out.stderr)



@unittest.skipUnless(os.name == "posix", "the job plugin supports Unix only")
class InstallTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name)
        self.env = dict(os.environ, HOME=str(self.home), PATH=f"{self.home}/.local/bin:/usr/bin:/bin")

    def install(self, *args):
        return subprocess.run([str(Path(__file__).with_name("install")), *args], env=self.env,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)

    def test_links_commands_and_adds_one_block_per_present_agent(self):
        (self.home / ".claude").mkdir()
        (self.home / ".claude" / "CLAUDE.md").write_text("# Mine\n")
        (self.home / ".pi" / "agent").mkdir(parents=True)
        out = self.install("--yes")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertTrue((self.home / ".local/bin/herdr-job").resolve().samefile(Path(__file__).with_name("herdr-job")))
        claude = (self.home / ".claude/CLAUDE.md").read_text()
        self.assertTrue(claude.startswith("# Mine\n\n<!-- herdr-job agent instructions v2"))
        self.assertIn("herdr-job clean-tree", claude)
        self.assertIn("herdr-job watch --pid", claude)
        self.assertIn("herdr-job run", (self.home / ".pi/agent/AGENTS.md").read_text())
        self.assertFalse((self.home / ".codex").exists())
        # A re-run changes nothing.
        out = self.install("--yes")
        self.assertEqual((self.home / ".claude/CLAUDE.md").read_text(), claude)
        self.assertIn("ok       " + str(self.home / ".claude/CLAUDE.md"), out.stdout)

    def test_an_older_block_is_replaced_and_hand_written_instructions_are_kept(self):
        (self.home / ".claude").mkdir()
        (self.home / ".claude/CLAUDE.md").write_text(
            "a\n<!-- herdr-job agent instructions v0: old -->\nold text\n<!-- /herdr-job agent instructions -->\nb\n")
        (self.home / ".codex").mkdir()
        (self.home / ".codex/AGENTS.md").write_text("Use `herdr-job run` for builds.\n")
        self.assertEqual(self.install("--yes").returncode, 0)
        claude = (self.home / ".claude/CLAUDE.md").read_text()
        self.assertNotIn("old text", claude)
        self.assertTrue(claude.startswith("a\n<!-- herdr-job agent instructions v2") and claude.endswith("-->\nb\n"))
        self.assertEqual((self.home / ".codex/AGENTS.md").read_text(), "Use `herdr-job run` for builds.\n")

    def test_without_a_terminal_or_yes_nothing_is_written_and_foreign_files_are_kept(self):
        (self.home / ".claude").mkdir()
        (self.home / ".local/bin").mkdir(parents=True)
        (self.home / ".local/bin/herdr-job").write_text("someone else's\n")
        out = self.install()
        self.assertEqual(out.returncode, 1)
        self.assertIn("skipped", out.stderr)
        self.assertFalse((self.home / ".claude/CLAUDE.md").exists())
        self.assertEqual((self.home / ".local/bin/herdr-job").read_text(), "someone else's\n")

    def test_the_readme_shows_the_block_the_installer_writes(self):
        here = Path(__file__).parent
        self.assertIn((here / "agent-instructions.md").read_text().strip(), (here / "README.md").read_text())
