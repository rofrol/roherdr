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

    def test_a_slot_is_swept_to_its_own_limit_or_refused_when_the_disk_stays_short(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            for name in ("debug", "release"):
                (target / name).mkdir()
            fake = {"debug": 12 * GIB, "release": 2 * GIB}
            size = lambda directory: fake[directory.name]
            slot_max = target_sweep.DEFAULT_SLOT_MAX_TARGET_GIB * GIB
            min_free = target_sweep.DEFAULT_MIN_FREE_GIB * GIB

            def bound(free_values):
                frees = iter(free_values)
                return target_sweep.bound_slot(
                    target, slot_max, min_free, dry_run=True, size=size, free=lambda _: next(frees)
                )

            # Plenty of room: swept to the slot limit only, and used.
            self.assertIsNone(bound([100 * GIB]))
            # 14 GiB is under the shared limit but over the slot's: debug goes.
            self.assertEqual(
                [d.name for d in target_sweep.plan(target, {target / n: s for n, s in fake.items()}, slot_max)],
                ["debug"],
            )
            # Short of room after that, but fine once the whole target/ goes.
            self.assertIsNone(bound([1 * GIB, 100 * GIB]))
            # Short even then: refused, with why.
            refusal = bound([1 * GIB, 1 * GIB])
            self.assertIn("GiB free", refusal)
            self.assertIn("not starting a worker", refusal)
            # A build holding cargo's lock refuses the start.
            with open(target / "debug" / ".cargo-lock", "a") as building:
                fcntl.flock(building, fcntl.LOCK_EX)
                self.assertIn("cargo build", bound([100 * GIB]))
            self.assertTrue((target / "debug").exists())

    def test_dry_run_removes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            (target / "debug").mkdir()
            (target / "debug" / "big.bin").write_bytes(b"x" * 2048)
            target_sweep.sweep(target, 0, dry_run=True)
            self.assertTrue((target / "debug" / "big.bin").exists())


if __name__ == "__main__":
    unittest.main()
