try:
    import fcntl
except ImportError:
    fcntl = None
import tempfile
import unittest
from pathlib import Path

from scripts import target_sweep

GIB = target_sweep.GIB


class PlanTests(unittest.TestCase):
    def sizes(self, **named):
        base = Path("/t")
        return {base / name: size * GIB for name, size in named.items()}

    def names(self, doomed):
        return [d.name for d in doomed]

    def test_nothing_is_removed_under_the_limit(self):
        sizes = self.sizes(debug=10, release=5)
        self.assertEqual(target_sweep.plan(Path("/t"), sizes, 25 * GIB), [])

    def test_debug_goes_first_and_alone_when_it_is_enough(self):
        sizes = self.sizes(debug=60, release=5, **{"aarch64-unknown-linux-gnu": 4})
        self.assertEqual(self.names(target_sweep.plan(Path("/t"), sizes, 25 * GIB)), ["debug"])

    def test_cross_targets_follow_largest_first_and_release_goes_last(self):
        sizes = self.sizes(debug=20, release=10, a=8, b=3)
        self.assertEqual(self.names(target_sweep.plan(Path("/t"), sizes, 5 * GIB)), ["debug", "a", "b", "release"])
        self.assertEqual(self.names(target_sweep.plan(Path("/t"), sizes, 13 * GIB)), ["debug", "a"])


@unittest.skipIf(fcntl is None, "advisory file locks are Unix-only")
class LockTests(unittest.TestCase):
    def test_a_running_build_stops_the_sweep(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            (target / "debug").mkdir()
            lock = target / "debug" / ".cargo-lock"
            with open(lock, "a") as building:
                fcntl.flock(building, fcntl.LOCK_EX)
                with self.assertRaises(target_sweep.BuildRunning):
                    target_sweep.sweep(target, 0)
                self.assertTrue((target / "debug").exists())
            # The build is done: the sweep removes the profile.
            (target / "debug" / "big.bin").write_bytes(b"x" * 2048)
            freed = target_sweep.sweep(target, 0)
            self.assertGreater(freed, 0)
            self.assertFalse((target / "debug").exists())

    def test_dry_run_removes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            (target / "debug").mkdir()
            (target / "debug" / "big.bin").write_bytes(b"x" * 2048)
            target_sweep.sweep(target, 0, dry_run=True)
            self.assertTrue((target / "debug" / "big.bin").exists())


if __name__ == "__main__":
    unittest.main()
