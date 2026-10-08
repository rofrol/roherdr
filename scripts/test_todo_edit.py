import contextlib
import fcntl
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import todo_edit

TODO = """\
# TODO

Intro text.

## Next, in order

Agents may do these from the top.

- [ ] First item (user, 2026-10-07: "one").
  More about the first.
  Decided: keep it.

- [ ] Second item
  continues here.

## Proposed

Items agents add.

- [ ] Proposed thing
  with detail.

## Needs a decision

Moved here in the triage.

### Decide

- [ ] Last decide item
  Options: a | b

### Needs you to act or watch

- [ ] Watch this
"""

DECISIONS = """\
# Decisions

Durable decisions.

## Old section (2026-10-07)

Something decided.
"""


class TodoEditTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.root = Path(self.dir.name)
        self.todo = self.root / "TODO.md"
        self.todo.write_text(TODO)

    def text_file(self, content):
        path = self.root / f"text{len(list(self.root.iterdir()))}.txt"
        path.write_text(content)
        return str(path)

    def files(self):
        return sorted(p.name for p in self.root.iterdir())

    def run_tool(self, *argv, file=None):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = todo_edit.run(["--file", str(file or self.todo), *argv])
        return code, out.getvalue(), err.getvalue()

    def assert_refused(self, *argv, message, file=None):
        path = file or self.todo
        before = path.read_text()
        code, _, err = self.run_tool(*argv, file=path)
        self.assertEqual(code, 1, err)
        self.assertIn(message, err)
        self.assertEqual(path.read_text(), before)

    def test_find_prints_section_and_lines(self):
        code, out, _ = self.run_tool("find", "Last decide")
        self.assertEqual(code, 0)
        self.assertEqual(out.strip(), "TODO > Needs a decision > Decide: lines 29-30")

    def test_find_matches_a_prefix_across_wrapped_lines(self):
        code, out, _ = self.run_tool("find", "Second item continues")
        self.assertEqual(code, 0)
        self.assertIn("Next, in order: lines 13-14", out)

    def test_find_refuses_missing_and_ambiguous(self):
        self.assert_refused("find", "Nope", message="no item starts with 'Nope'")
        self.assert_refused("find", "", message="empty title prefix")

    def test_ambiguous_prefix_lists_the_matches(self):
        self.todo.write_text(TODO.replace("Watch this", "First other"))
        before = self.todo.read_text()
        code, _, err = self.run_tool("remove", "First")
        self.assertEqual(code, 1)
        self.assertIn("2 items start with 'First'", err)
        self.assertIn("line 9: First item", err)
        self.assertEqual(self.todo.read_text(), before)

    def test_add_at_end_of_section(self):
        code, _, err = self.run_tool(
            "add", "Next, in order", "--text-file", self.text_file("- [ ] Third\n  body\n")
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(),
            TODO.replace("  continues here.\n", "  continues here.\n\n- [ ] Third\n  body\n"),
        )

    def test_add_after_an_item_and_at_top(self):
        code, _, err = self.run_tool(
            "add", "Next, in order", "--after", "First", "--text-file", self.text_file("- [ ] Between\n")
        )
        self.assertEqual(code, 0, err)
        self.assertIn("  Decided: keep it.\n\n- [ ] Between\n\n- [ ] Second", self.todo.read_text())
        code, _, err = self.run_tool("add", "Proposed", "--top", "--text-file", self.text_file("- [ ] Top\n"))
        self.assertEqual(code, 0, err)
        self.assertIn("Items agents add.\n\n- [ ] Top\n\n- [ ] Proposed thing", self.todo.read_text())

    def test_add_to_a_section_with_subsections_stays_before_them(self):
        code, _, err = self.run_tool("add", "Needs a decision", "--text-file", self.text_file("- [ ] Here\n"))
        self.assertEqual(code, 0, err)
        self.assertIn("Moved here in the triage.\n\n- [ ] Here\n\n### Decide\n", self.todo.read_text())

    def test_add_refuses_bad_input(self):
        self.assert_refused("add", "Nowhere", "--text-file", self.text_file("- [ ] X\n"), message="no heading 'Nowhere'")
        self.assert_refused("add", "Proposed", "--text-file", self.text_file("X\n"), message="must start with '- [ ] '")
        self.assert_refused(
            "add", "Proposed", "--text-file", self.text_file("- [ ] X\nnot indented\n"), message="must be indented"
        )
        self.assert_refused(
            "add", "Proposed", "--after", "First", "--text-file", self.text_file("- [ ] X\n"),
            message="is in 'TODO > Next, in order'",
        )

    def test_append_to_item(self):
        code, _, err = self.run_tool("append-to", "First item", "--text-file", self.text_file("  Result: done.\n\n"))
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(), TODO.replace("  Decided: keep it.\n", "  Decided: keep it.\n  Result: done.\n")
        )
        self.assert_refused("append-to", "First item", "--text-file", self.text_file("## Oops\n"), message="must be indented")

    def test_insert_after_anchor(self):
        code, _, err = self.run_tool(
            "insert-after", "First item", "--anchor", "More about the first.",
            "--text-file", self.text_file("\n  Inserted line.\n"),
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(),
            TODO.replace("  More about the first.\n", "  More about the first.\n  Inserted line.\n"),
        )

    def test_insert_after_an_anchor_ending_in_a_newline_keeps_separate_lines(self):
        # Historical bug 3: the text file's last line was glued to the next line.
        code, _, err = self.run_tool(
            "insert-after", "First item", "--anchor", "More about the first.\n",
            "--text-file", self.text_file("  Inserted line.\n  Second inserted line."),
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(),
            TODO.replace(
                "  More about the first.\n",
                "  More about the first.\n  Inserted line.\n  Second inserted line.\n",
            ),
        )

    def test_insert_after_an_anchor_at_a_line_end_inserts_whole_lines(self):
        code, _, err = self.run_tool(
            "insert-after", "First item", "--anchor", "More about the first.",
            "--text-file", self.text_file("  Inserted line.\n\n\n"),
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(),
            TODO.replace("  More about the first.\n", "  More about the first.\n  Inserted line.\n"),
        )

    def test_insert_after_the_unterminated_last_line_of_the_file(self):
        self.todo.write_text(TODO.rstrip("\n"))
        code, _, err = self.run_tool(
            "insert-after", "Watch this", "--anchor", "Watch this", "--text-file", self.text_file("  Note."),
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(self.todo.read_text(), TODO + "  Note.\n")

    def test_insert_after_a_mid_line_anchor_is_literal(self):
        code, _, err = self.run_tool(
            "insert-after", "First item", "--anchor", "More about", "--text-file", self.text_file(" (much)"),
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(self.todo.read_text(), TODO.replace("More about the first.", "More about (much) the first."))
        code, _, err = self.run_tool(
            "insert-after", "First item", "--anchor", "Decided:", "--text-file", self.text_file(" yes\n  and"),
        )
        self.assertEqual(code, 0, err)
        self.assertIn("  Decided: yes\n  and keep it.\n", self.todo.read_text())

    def test_append_to_and_add_after_the_unterminated_last_line(self):
        self.todo.write_text(TODO.rstrip("\n"))
        code, _, err = self.run_tool("append-to", "Watch this", "--text-file", self.text_file("  More."))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.todo.read_text(), TODO + "  More.\n")
        self.todo.write_text(TODO.rstrip("\n"))
        code, _, err = self.run_tool(
            "add", "Needs you to act or watch", "--text-file", self.text_file("- [ ] Next")
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(self.todo.read_text(), TODO + "\n- [ ] Next\n")

    def test_verify_insertion_refuses_a_joined_line(self):
        old = TODO.splitlines(keepends=True)
        at = old.index("  More about the first.\n") + 1
        insertion = todo_edit.Insertion(at, at, 1, ("  Inserted line.\n",))
        glued = TODO.replace("  More about the first.\n", "  More about the first.\n  Inserted line.")
        with self.assertRaisesRegex(todo_edit.Refusal, "join or split lines"):
            todo_edit.verify_insertion(old, glued, insertion)
        # Same line count, but a neighbouring line changed.
        shifted = TODO.replace(
            "  More about the first.\n  Decided: keep it.\n",
            "  More about the first.\n  Inserted line.\n  Decided: keep it!\n",
        )
        with self.assertRaisesRegex(todo_edit.Refusal, "lines after the insertion"):
            todo_edit.verify_insertion(old, shifted, insertion)
        good = TODO.replace("  More about the first.\n", "  More about the first.\n  Inserted line.\n")
        todo_edit.verify_insertion(old, good, insertion)

    def test_missing_anchor_refuses(self):
        # Historical bug 1: a replace whose anchor did not match changed nothing silently.
        self.assert_refused(
            "insert-after", "First item", "--anchor", "not there", "--text-file", self.text_file("x\n"),
            message="occurs 0 times",
        )

    def test_anchor_must_be_unique_and_inside_the_item(self):
        self.assert_refused(
            "insert-after", "First item", "--anchor", "st", "--text-file", self.text_file("x\n"),
            message="occurs 2 times",
        )
        # "continues here." is in another item, not in the first one.
        self.assert_refused(
            "insert-after", "First item", "--anchor", "continues here.", "--text-file", self.text_file("x\n"),
            message="occurs 0 times",
        )

    def test_insert_that_would_split_the_item_refuses(self):
        self.assert_refused(
            "insert-after", "First item", "--anchor", "More about the first.",
            "--text-file", self.text_file("\n- [ ] Smuggled item\n"),
            message="alter items other than the intended one",
        )
        self.assert_refused(
            "insert-after", "First item", "--anchor", "More about the first.",
            "--text-file", self.text_file("\n## Smuggled heading\n"),
            message="alter",
        )

    def test_remove_item(self):
        code, _, err = self.run_tool("remove", "Second item")
        self.assertEqual(code, 0, err)
        self.assertEqual(
            self.todo.read_text(),
            TODO.replace("- [ ] Second item\n  continues here.\n\n", ""),
        )

    def test_removing_the_last_item_of_a_section_keeps_the_next_heading(self):
        # Historical bug 2: cutting "up to the next item" deleted a heading.
        code, _, err = self.run_tool("remove", "Proposed thing")
        self.assertEqual(code, 0, err)
        text = self.todo.read_text()
        self.assertIn("Items agents add.\n\n## Needs a decision\n\nMoved here in the triage.\n\n### Decide\n", text)
        code, _, err = self.run_tool("remove", "Last decide")
        self.assertEqual(code, 0, err)
        self.assertIn("### Decide\n\n### Needs you to act or watch\n\n- [ ] Watch this\n", self.todo.read_text())

    def test_remove_the_last_item_of_the_file(self):
        code, _, err = self.run_tool("remove", "Watch this")
        self.assertEqual(code, 0, err)
        self.assertTrue(self.todo.read_text().endswith("### Needs you to act or watch\n"))

    def test_move_item(self):
        code, _, err = self.run_tool("move", "Proposed thing", "Next, in order", "--top")
        self.assertEqual(code, 0, err)
        text = self.todo.read_text()
        self.assertIn("from the top.\n\n- [ ] Proposed thing\n  with detail.\n\n- [ ] First item", text)
        self.assertIn("Items agents add.\n\n## Needs a decision", text)
        code, _, err = self.run_tool("move", "First item", "Decide")
        self.assertEqual(code, 0, err)
        self.assertIn("  Options: a | b\n\n- [ ] First item", self.todo.read_text())

    def test_move_refuses_unknown_section_and_self(self):
        self.assert_refused("move", "First item", "Nowhere", message="no heading 'Nowhere'")
        self.assert_refused("move", "First item", "Next, in order", "--after", "First", message="after itself")

    def test_verify_rejects_a_change_to_another_item_or_heading(self):
        doc = todo_edit.Doc(TODO)
        with self.assertRaisesRegex(todo_edit.Refusal, "delete headings"):
            todo_edit.verify(TODO.replace("### Decide\n", ""), doc.entries(), doc.frame, doc)
        with self.assertRaisesRegex(todo_edit.Refusal, "other than the intended"):
            todo_edit.verify(TODO.replace("with detail.", "with detail!"), doc.entries(), doc.frame, doc)
        with self.assertRaisesRegex(todo_edit.Refusal, "outside the items"):
            todo_edit.verify(TODO.replace("Items agents add.", "Items."), doc.entries(), doc.frame, doc)

    def test_fenced_lines_are_not_headings(self):
        self.todo.write_text(TODO.replace("Intro text.\n", "Intro text.\n\n```\n# not a heading\n- [ ] not an item\n```\n"))
        code, out, _ = self.run_tool("find", "First item")
        self.assertEqual(code, 0)
        code, _, err = self.run_tool("find", "not an item")
        self.assertEqual(code, 1)

    def test_add_section_to_decisions(self):
        decisions = self.root / "DECISIONS.md"
        decisions.write_text(DECISIONS + "\n")
        code, _, err = self.run_tool(
            "add-section", "New thing (2026-10-08)", "--text-file", self.text_file("Why it was chosen.\n"),
            file=decisions,
        )
        self.assertEqual(code, 0, err)
        self.assertEqual(decisions.read_text(), DECISIONS + "\n## New thing (2026-10-08)\n\nWhy it was chosen.\n")
        self.assert_refused(
            "add-section", "Old section (2026-10-07)", "--text-file", self.text_file("x\n"),
            file=decisions, message="already exists",
        )
        self.assert_refused(
            "add-section", "Another", "--text-file", self.text_file("## Nested\n"),
            file=decisions, message="must not contain headings",
        )

    def test_failed_rename_leaves_the_file_and_no_temp_file(self):
        with mock.patch.object(todo_edit.os, "replace", side_effect=OSError("disk full")):
            with self.assertRaises(OSError):
                self.run_tool("remove", "Second item")
        self.assertEqual(self.todo.read_text(), TODO)
        self.assertEqual(self.files(), ["TODO.md", "TODO.md.lock"])

    def test_write_goes_through_a_rename_and_keeps_the_mode(self):
        os.chmod(self.todo, 0o640)
        calls = []
        real_replace = os.replace

        def spy(src, dst):
            calls.append((Path(src).parent, Path(dst)))
            self.assertEqual(self.todo.read_text(), TODO)  # untouched until the rename
            real_replace(src, dst)

        with mock.patch.object(todo_edit.os, "replace", side_effect=spy):
            code, _, err = self.run_tool("remove", "Second item")
        self.assertEqual(code, 0, err)
        self.assertEqual(calls, [(self.root, self.todo)])
        self.assertEqual(self.todo.stat().st_mode & 0o777, 0o640)

    def test_concurrent_change_refuses(self):
        # Reads: the edit's own, the check before the temp file, the check
        # right before the rename. A change seen by either check refuses.
        real_read = todo_edit.Path.read_bytes
        for changed_read in (2, 3):
            state = {"n": 0}

            def read(path, *a, **k):
                state["n"] += 1
                data = real_read(path, *a, **k)
                return data + b"changed\n" if state["n"] == changed_read else data

            with mock.patch.object(todo_edit.Path, "read_bytes", read):
                code, _, err = self.run_tool("remove", "Second item")
            self.assertEqual(code, 1)
            self.assertIn("changed while editing", err)
            self.assertIn(str(self.todo), err)
            self.assertEqual(self.todo.read_text(), TODO)
            self.assertEqual(self.files(), ["TODO.md", "TODO.md.lock"])

    OTHER = TODO + "- [ ] Added by another session\n"

    def test_a_real_write_while_editing_refuses_and_keeps_the_other_change(self):
        # Another session writes the file on disk after this edit read it.
        real_verify = todo_edit.verify
        written = []

        def verify_then_another_write(*a, **k):
            real_verify(*a, **k)
            if not written:
                self.todo.write_text(self.OTHER)
                written.append(True)

        with mock.patch.object(todo_edit, "verify", verify_then_another_write):
            code, _, err = self.run_tool("remove", "Second item")
        self.assertEqual(code, 1)
        self.assertIn("changed while editing", err)
        self.assertEqual(self.todo.read_text(), self.OTHER)
        self.assertEqual(self.files(), ["TODO.md", "TODO.md.lock"])

    def test_a_write_while_the_temp_file_is_written_refuses_and_is_not_lost(self):
        # Another writer (one that does not take the lock) changes the file
        # after the first check, while the temp file is being written: the
        # check right before the rename refuses, and its change stays.
        real_fsync = os.fsync

        def another_write_then_fsync(fd):
            self.todo.write_text(self.OTHER)
            real_fsync(fd)

        with mock.patch.object(todo_edit.os, "fsync", side_effect=another_write_then_fsync):
            code, _, err = self.run_tool("remove", "Second item")
        self.assertEqual(code, 1)
        self.assertIn(f"{self.todo} changed while editing; nothing written", err)
        self.assertEqual(self.todo.read_text(), self.OTHER)
        self.assertEqual(self.files(), ["TODO.md", "TODO.md.lock"])

    def test_two_concurrent_runs_serialize(self):
        # While the lock is held, two runs start and another holder of the
        # lock changes the file; both runs then edit the current file, one
        # after the other, and every change is kept.
        script = Path(__file__).with_name("todo_edit.py")
        runs = []
        with open(self.todo.with_name("TODO.md.lock"), "a+") as held:
            fcntl.flock(held.fileno(), fcntl.LOCK_EX)
            for title in ("Third", "Fourth"):
                text = self.text_file(f"- [ ] {title}\n")
                runs.append(
                    subprocess.Popen(
                        [sys.executable, str(script), "--file", str(self.todo),
                         "add", "Proposed", "--text-file", text],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                    )
                )
            self.todo.write_text(TODO + "\n- [ ] Under the lock\n")
            # Released when the file closes.
        results = [run.communicate() + (run.returncode,) for run in runs]
        for out, err, code in results:
            self.assertEqual(code, 0, err)
        text = self.todo.read_text()
        for title in ("Third", "Fourth", "Under the lock"):
            self.assertIn(f"- [ ] {title}\n", text)


if __name__ == "__main__":
    unittest.main()
