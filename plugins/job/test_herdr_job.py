"""Regression checks for the job footer's terminal control sequences."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import re
import unicodedata
import unittest
from unittest.mock import patch

Footer = (runpy.run_path(str(Path(__file__).with_name("herdr-job")))["Footer"]
          if os.name == "posix" else None)


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
