import os
from pathlib import Path
import tempfile
import unittest
import unittest.mock
from unittest.mock import patch

from scripts import windows_cross


class WindowsCrossTests(unittest.TestCase):
    def test_libc_configuration_matches_xwin_layout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative in (
                "sdk/include/ucrt/stdlib.h", "crt/include/vcruntime.h",
                "sdk/lib/ucrt/x86_64/ucrt.lib", "crt/lib/x86_64/vcruntime.lib",
                "sdk/lib/um/x86_64/kernel32.lib",
            ):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            contents = windows_cross.libc_contents(root)
            self.assertIn(f"include_dir={root / 'sdk/include/ucrt'}\n", contents)
            self.assertIn(f"msvc_lib_dir={root / 'crt/lib/x86_64'}\n", contents)
            self.assertTrue(contents.endswith("gcc_dir=\n"))
            (root / "crt/include/vcruntime.h").unlink()
            with self.assertRaisesRegex(ValueError, "missing .*vcruntime.h"):
                windows_cross.libc_contents(root)

    def test_persistent_configuration_and_explicit_override(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            default = root / "libc.txt"
            override = root / "custom.txt"
            default.touch()
            override.touch()
            with patch.object(windows_cross, "SDK_ROOT", root), patch.dict(os.environ, {}, clear=True):
                self.assertEqual(windows_cross.libc_path(), default.resolve())
                with patch.dict(os.environ, {windows_cross.LIBC_ENV: str(override)}):
                    self.assertEqual(windows_cross.libc_path(), override.resolve())
                    override.unlink()
                    with self.assertRaisesRegex(ValueError, "just setup-windows-cross"):
                        windows_cross.libc_path()

    def test_missing_setup_does_not_run_build_or_download(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(windows_cross, "SDK_ROOT", Path(directory)), \
                    patch.dict(os.environ, {}, clear=True), \
                    patch.object(windows_cross.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "just setup-windows-cross"):
                    windows_cross.lint()
                run.assert_not_called()

    def write_libc(self, root):
        for relative in ("sdk/include/ucrt", "sdk/include/um", "sdk/include/shared", "crt/include"):
            (root / relative).mkdir(parents=True, exist_ok=True)
        libc = root / "libc.txt"
        libc.write_text(
            f"include_dir={root / 'sdk/include/ucrt'}\n"
            f"sys_include_dir={root / 'crt/include'}\n"
            "crt_dir=\ngcc_dir=\n"
        )
        return libc

    def test_lint_passes_sdk_to_cargo_without_changing_parent_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            libc = self.write_libc(Path(directory))
            with patch.object(windows_cross, "libc_path", return_value=libc), \
                    patch.object(windows_cross, "link_macos_system_libraries"), \
                    patch.object(windows_cross.shutil, "which", return_value="/zig/zig"), \
                    patch.dict(os.environ, {"KEEP_ME": "yes"}, clear=True), \
                    patch.object(windows_cross.subprocess, "run") as run:
                windows_cross.lint()
                self.assertEqual(run.call_count, 2)
                cargo = run.call_args
                self.assertEqual(cargo.args[0][:2], ["cargo", "clippy"])
                env = cargo.kwargs["env"]
                self.assertEqual(env[windows_cross.LIBC_ENV], str(libc))
                self.assertEqual(env["KEEP_ME"], "yes")
                self.assertEqual(env["AR_x86_64_pc_windows_msvc"], "/zig/zig lib")
                self.assertIn("-isystem", env["CFLAGS_x86_64_pc_windows_msvc"])
                # Host and native builds keep their own settings.
                for name in ("CFLAGS", "AR", "CC", "TARGET_CFLAGS"):
                    self.assertNotIn(name, env)
                for name in (windows_cross.LIBC_ENV, "CFLAGS_x86_64_pc_windows_msvc"):
                    self.assertNotIn(name, os.environ)

    def test_c_cross_env_adds_sdk_headers_and_zig_lib(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            libc = self.write_libc(root)
            with patch.object(windows_cross.shutil, "which", return_value="/zig/zig"):
                env = windows_cross.c_cross_env(libc, {"CFLAGS_x86_64_pc_windows_msvc": "-O1"})
                self.assertEqual(
                    env["CFLAGS_x86_64_pc_windows_msvc"],
                    " ".join(
                        f"-isystem {root / relative}"
                        for relative in ("sdk/include/ucrt", "crt/include", "sdk/include/um", "sdk/include/shared")
                    ) + " -O1",
                )
                self.assertEqual(env["AR_x86_64_pc_windows_msvc"], "/zig/zig lib")
                # An archiver the user set is kept.
                kept = windows_cross.c_cross_env(libc, {"AR_x86_64_pc_windows_msvc": "llvm-lib"})
                self.assertNotIn("AR_x86_64_pc_windows_msvc", kept)
            with patch.object(windows_cross.shutil, "which", return_value="/my zig/zig"):
                with self.assertRaisesRegex(ValueError, "must not contain spaces"):
                    windows_cross.c_cross_env(libc, {})
            with patch.object(windows_cross.shutil, "which", return_value=None):
                with self.assertRaisesRegex(ValueError, "Install Zig"):
                    windows_cross.c_cross_env(libc, {})

    @unittest.skipIf(os.name == "nt", "the macOS system-library link is a Unix symlink workaround")
    def test_macos_links_its_system_libraries_into_the_managed_sdk_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk_root = root / "windows-cross"
            sdk_root.mkdir()
            libc = sdk_root / "libc.txt"
            libc.touch()
            macos_libs = root / "MacOSX.sdk/usr/lib"
            macos_libs.mkdir(parents=True)
            (macos_libs / "libSystem.tbd").touch()
            link = sdk_root / "usr/lib"
            xcrun = unittest.mock.Mock(stdout=f"{root / 'MacOSX.sdk'}\n")
            with patch.object(windows_cross, "SDK_ROOT", sdk_root), \
                    patch.object(windows_cross.sys, "platform", "darwin"), \
                    patch.object(windows_cross.subprocess, "run", return_value=xcrun) as run:
                windows_cross.link_macos_system_libraries(libc)
                self.assertEqual(os.readlink(link), str(macos_libs))
                self.assertEqual(run.call_args.args[0], ["xcrun", "--sdk", "macosx", "--show-sdk-path"])
                windows_cross.link_macos_system_libraries(libc)
                self.assertEqual(os.readlink(link), str(macos_libs))

                # A link to an older SDK is ours to replace; anything else is not.
                link.unlink()
                link.symlink_to(root / "Old.sdk/usr/lib")
                windows_cross.link_macos_system_libraries(libc)
                self.assertEqual(os.readlink(link), str(macos_libs))
                link.unlink()
                link.symlink_to(root / "elsewhere")
                with self.assertRaisesRegex(ValueError, "not a macOS SDK"):
                    windows_cross.link_macos_system_libraries(libc)
                link.unlink()
                link.mkdir()
                with self.assertRaisesRegex(ValueError, "not a link to a macOS SDK"):
                    windows_cross.link_macos_system_libraries(libc)
                link.rmdir()

                # An SDK named by LIBGHOSTTY_VT_WINDOWS_LIBC is never touched.
                other = root / "custom.txt"
                other.touch()
                run.reset_mock()
                windows_cross.link_macos_system_libraries(other)
                run.assert_not_called()
                self.assertFalse(link.exists())

                (macos_libs / "libSystem.tbd").unlink()
                with self.assertRaisesRegex(ValueError, "no libSystem.tbd"):
                    windows_cross.link_macos_system_libraries(libc)

            with patch.object(windows_cross, "SDK_ROOT", sdk_root), \
                    patch.object(windows_cross.sys, "platform", "linux"), \
                    patch.object(windows_cross.subprocess, "run") as run:
                windows_cross.link_macos_system_libraries(libc)
                run.assert_not_called()
                self.assertFalse(link.exists())

    def test_license_acceptance_is_only_forwarded_when_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(windows_cross, "SDK_ROOT", Path(directory)), \
                    patch.object(windows_cross.shutil, "which", return_value="tool"), \
                    patch.object(windows_cross, "libc_contents", return_value="configuration"), \
                    patch.object(windows_cross.subprocess, "run") as run:
                for accepted in (False, True):
                    run.reset_mock()
                    windows_cross.setup(accepted)
                    command = run.call_args_list[0].args[0]
                    self.assertEqual("--accept-license" in command, accepted)
                    self.assertEqual(command[0], "xwin")
                    self.assertIn("--copy", command)


if __name__ == "__main__":
    unittest.main()
