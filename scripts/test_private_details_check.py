"""Synthetic-only privacy guard regressions (also in maintenance-test)."""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("private_details_check.py").resolve()


class PrivateDetailsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Synthetic Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.repo / "note.txt").write_text("public\n")
        self.base = self.commit()
        (self.repo / ".local").mkdir()
        self.patterns("SYNTHETIC-PRIVATE-[0-9]+\n")

    def git(self, *args):
        env = os.environ.copy()
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            env.pop(name, None)
        result = subprocess.run(["git", *args], cwd=self.repo, env=env,
                                capture_output=True, check=True)
        return result.stdout.decode().strip()

    def commit(self):
        self.git("add", "note.txt")
        self.git("commit", "-qm", "synthetic change")
        return self.git("rev-parse", "HEAD")

    def patterns(self, text):
        (self.repo / ".local/private-patterns.txt").write_text(text)

    def check(self, base=None, head="HEAD", hook=None):
        args = ["--pre-push"] if hook is not None else ["--range", self.base if base is None else base, head]
        return subprocess.run([sys.executable, str(SCRIPT), *args], cwd=self.repo,
                              input=hook or "", capture_output=True, text=True)

    def test_repository_ignores_local_private_configuration(self):
        result = subprocess.run(
            ["git", "check-ignore", "--no-index", ".local/private-patterns.txt"],
            cwd=SCRIPT.parent.parent, capture_output=True,
        )
        self.assertEqual(result.returncode, 0)
        policy = (SCRIPT.parent.parent / "AGENTS.md").read_text()
        self.assertIn("Never put private details in tracked files", policy)
        self.assertIn("belong only in the ignored `.local/`", policy)

    def test_reports_only_path_and_added_line_and_checks_intermediate_history(self):
        (self.repo / "note.txt").write_text("public\nSYNTHETIC-PRIVATE-42\n")
        leaked = self.commit()
        (self.repo / "note.txt").write_text("public\n")
        self.commit()
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('"note.txt":2', result.stderr)
        self.assertNotIn("SYNTHETIC-PRIVATE", result.stderr + result.stdout)
        self.assertEqual(self.check(base=leaked).returncode, 0)

    def test_missing_configuration_skips_but_bad_configuration_and_refs_fail_closed(self):
        (self.repo / ".local/private-patterns.txt").unlink()
        self.assertEqual(self.check().returncode, 0)
        for text in ("", "\n", "[SYNTHETIC-PRIVATE"):
            self.patterns(text)
            result = self.check()
            self.assertEqual(result.returncode, 2)
            self.assertNotIn("SYNTHETIC-PRIVATE", result.stderr)
        self.patterns("SYNTHETIC-PRIVATE\n")
        self.assertEqual(self.check(head="missing-ref").returncode, 2)

    def test_existing_private_context_and_removed_lines_are_not_additions(self):
        (self.repo / "note.txt").write_text("SYNTHETIC-PRIVATE-42\npublic\n")
        base = self.commit()
        (self.repo / "note.txt").write_text("SYNTHETIC-PRIVATE-42\nchanged\n")
        self.commit()
        self.assertEqual(self.check(base=base).returncode, 0)

    def test_hook_updates_new_refs_and_deletions(self):
        (self.repo / "note.txt").write_text("SYNTHETIC-PRIVATE-42\n")
        head = self.commit()
        zeros = "0" * 40
        for base in (self.base, zeros):
            result = self.check(hook=f"refs/heads/master {head} refs/heads/master {base}\n")
            self.assertEqual(result.returncode, 1)
        self.assertEqual(self.check(hook=f"(delete) {zeros} refs/heads/master {head}\n").returncode, 0)
        self.assertEqual(self.check(hook="malformed\n").returncode, 2)
        self.assertEqual(self.check(base="").returncode, 1)

    def test_renames_binary_and_quoted_paths_are_checked(self):
        target = self.repo / 'quoted "name".txt'
        (self.repo / "note.txt").rename(target)
        target.write_bytes(b"public\n\0SYNTHETIC-PRIVATE-42\n")
        self.git("add", "-u")
        self.git("add", target.name)
        self.git("commit", "-qm", "synthetic rename")
        result = self.check()
        self.assertEqual(result.returncode, 1)
        self.assertIn('quoted \\"name\\".txt', result.stderr)
        self.assertNotIn("SYNTHETIC-PRIVATE", result.stderr)


if __name__ == "__main__":
    unittest.main()
