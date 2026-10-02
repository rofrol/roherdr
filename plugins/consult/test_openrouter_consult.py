"""Offline tests for the OpenRouter consultation helper's safety guardrails.

These cover the parts that keep secrets and unvetted files from reaching the anonymous cloaked provider. They never
touch the network: every case here is refused before the request is built, or exercises a pure helper.
"""
import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

HELPER = Path(__file__).parent / "skills/openrouter/ask_openrouter.py"

_spec = importlib.util.spec_from_file_location("ask_openrouter", HELPER)
mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mod)


@contextmanager
def git_repo():
    with tempfile.TemporaryDirectory() as d:
        root = Path(d).resolve()
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        (root / ".gitignore").write_text(".env\nsecrets/\n")
        prev = Path.cwd()
        os.chdir(root)
        try:
            yield root
        finally:
            os.chdir(prev)


class ScanSecretsTests(unittest.TestCase):
    def test_clean_prompt_passes(self):
        mod.scan_secrets("Please review this pure function that adds two numbers and returns the sum.")

    def test_rejects_private_key_block(self):
        with self.assertRaises(SystemExit):
            mod.scan_secrets("key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIB...\n")

    def test_rejects_env_assignment(self):
        with self.assertRaises(SystemExit):
            mod.scan_secrets("here is my config: DB_PASSWORD=hunter2supersecret")

    def test_rejects_url_with_credentials(self):
        with self.assertRaises(SystemExit):
            mod.scan_secrets("postgres://admin:s3cr3tpass@db.internal:5432/app")

    def test_rejects_aws_key(self):
        with self.assertRaises(SystemExit):
            mod.scan_secrets("aws key AKIAIOSFODNN7EXAMPLE in the handler")


class CheckFileTests(unittest.TestCase):
    def test_accepts_tracked_text_file(self):
        with git_repo() as root:
            (root / "ok.txt").write_text("fn add(a, b) { a + b }\n")
            self.assertIn("add", mod.check_file("ok.txt"))

    def test_rejects_gitignored_file(self):
        with git_repo() as root:
            (root / ".env").write_text("TOKEN=abc\n")
            with self.assertRaises(SystemExit):
                mod.check_file(".env")

    def test_rejects_denylisted_name(self):
        with git_repo() as root:
            (root / "service.pem").write_text("cert material\n")
            with self.assertRaises(SystemExit):
                mod.check_file("service.pem")

    def test_rejects_path_outside_cwd(self):
        with git_repo():
            with self.assertRaises(SystemExit):
                mod.check_file("/etc/hosts")

    def test_rejects_symlink_component(self):
        with git_repo() as root:
            (root / "real.txt").write_text("data\n")
            (root / "link.txt").symlink_to(root / "real.txt")
            with self.assertRaises(SystemExit):
                mod.check_file("link.txt")

    def test_rejects_binary(self):
        with git_repo() as root:
            (root / "blob.bin").write_bytes(b"\x00\x01\x02binary")
            subprocess.run(["git", "add", "blob.bin"], cwd=root, check=True)
            with self.assertRaises(SystemExit):
                mod.check_file("blob.bin")

    def test_rejects_oversize(self):
        with git_repo() as root:
            (root / "big.txt").write_text("x" * (mod.MAX_FILE_BYTES + 1))
            with self.assertRaises(SystemExit):
                mod.check_file("big.txt")

    def test_non_repo_is_fail_closed(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve()
            (root / "ok.txt").write_text("data\n")
            prev = Path.cwd()
            os.chdir(root)
            try:
                with self.assertRaises(SystemExit):
                    mod.check_file("ok.txt")
            finally:
                os.chdir(prev)


class CliGateTests(unittest.TestCase):
    def run_helper(self, args, cwd):
        env = dict(os.environ, CONSULT_IN_JOB="1")  # skip the herdr-job re-exec
        return subprocess.run([sys.executable, str(HELPER), *args], cwd=cwd,
                              capture_output=True, text=True, env=env, timeout=20)

    def test_file_without_allow_files_is_refused(self):
        with git_repo() as root:
            (root / "ok.txt").write_text("data\n")
            r = self.run_helper(["-f", "ok.txt", "review"], root)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("--allow-files", r.stderr + r.stdout)

    def test_secret_in_prompt_is_refused_before_send(self):
        with git_repo() as root:
            r = self.run_helper(["my token is ghp_" + "a" * 30], root)
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("secret-shaped", r.stderr + r.stdout)


if __name__ == "__main__":
    unittest.main()
