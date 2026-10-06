"""Tests for scripts/clean_tree.py with a throwaway repository."""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("clean_tree.py")


@unittest.skipIf(os.name == "nt", "clean_tree.py is Unix-only")
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
        return subprocess.run([sys.executable, str(SCRIPT), "run", *args], cwd=self.repo, env=self.env,
                              capture_output=True, text=True, timeout=60)

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


if __name__ == "__main__":
    unittest.main()
