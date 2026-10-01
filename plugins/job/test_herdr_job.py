"""Regression checks for the job footer's terminal control sequences."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import re
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
