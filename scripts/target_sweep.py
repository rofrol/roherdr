#!/usr/bin/env python3
"""Keeps `target/` from filling the disk (see `just sweep` and `just guard`).

`cargo test` leaves a hashed binary per build in `target/debug/deps`, so the
directory grows for good (66 GB after a few days of several sessions building
in the shared checkout). Without cargo-sweep, the simple rule is: when `target/`
is over a size limit, remove the debug profile (it is rebuilt on demand), then
the cross-compilation targets if that is not enough.

Safety: cargo holds an exclusive lock on `target/<profile>/.cargo-lock` while it
builds. The sweep takes that lock without waiting and gives up if a build is
running, instead of guessing from the process list. Run it through the justfile,
not by hand, and do not delete `target/` artifacts yourself.

`slot` is what herdr runs before it gives a persistent worker folder ("folder
slot", `herdr worker start --folder-slot`) to a new worker: the same sweep with
the smaller DEFAULT_SLOT_MAX_TARGET_GIB, then the `guard` check, refusing the
start when the disk stays short.
"""
import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

try:
    import fcntl
except ImportError:  # Windows: nothing here applies
    fcntl = None

GIB = 1024**3
DEFAULT_MAX_TARGET_GIB = 25
DEFAULT_MIN_FREE_GIB = 15
# A folder slot builds one branch at a time, so the test binaries of the
# branches before it are dead weight: its target/ grew to 14 GB in a day
# (2026-10-08) while the shared limit above freed nothing. 10 GiB holds one
# warm debug build of herdr with its tests.
DEFAULT_SLOT_MAX_TARGET_GIB = 10


def dir_size(path):
    """Bytes under `path`, from `du` (fast on the huge deps directory)."""
    if not path.exists():
        return 0
    out = subprocess.run(["du", "-sk", str(path)], capture_output=True, text=True, check=False)
    try:
        return int(out.stdout.split()[0]) * 1024
    except (IndexError, ValueError):
        return 0


def plan(target, sizes, max_bytes):
    """The directories to remove, in order, to bring the total under `max_bytes`.

    `sizes` maps each directory under `target` to its size. Debug goes first
    (the common culprit), then the other profiles and cross targets, largest
    first; `release` is kept for last since it is what gets installed.
    """
    total = sum(sizes.values())
    if total <= max_bytes:
        return []
    order = [d for d in sizes if d.name == "debug"]
    others = sorted((d for d in sizes if d.name not in ("debug", "release")), key=lambda d: -sizes[d])
    order += others + [d for d in sizes if d.name == "release"]
    removed = []
    for directory in order:
        if total <= max_bytes:
            break
        removed.append(directory)
        total -= sizes[directory]
    return removed


class BuildRunning(Exception):
    pass


class Locks:
    """Cargo's own build locks for every profile directory, held without waiting."""

    def __init__(self, target):
        self.target = target
        self.files = []

    def __enter__(self):
        for lock in sorted(self.target.glob("*/.cargo-lock")):
            handle = open(lock, "a")
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                handle.close()
                self.__exit__()
                raise BuildRunning(f"a cargo build holds {lock}") from None
            self.files.append(handle)
        return self

    def __exit__(self, *exc):
        for handle in self.files:
            fcntl.flock(handle, fcntl.LOCK_UN)
            handle.close()
        self.files = []


def sweep(target, max_bytes, dry_run=False, size=dir_size):
    """Removes what `plan` picks; returns the bytes freed. Raises BuildRunning.

    `size` measures a directory (tests pass fake sizes).
    """
    sizes = {child: size(child) for child in target.iterdir() if child.is_dir()}
    doomed = plan(target, sizes, max_bytes)
    freed = 0
    with Locks(target):
        for directory in doomed:
            print(f"{'would remove' if dry_run else 'removing'} {directory} ({sizes[directory] / GIB:.1f} GiB)")
            if not dry_run:
                shutil.rmtree(directory, ignore_errors=True)
            freed += sizes[directory]
    return freed


def free_bytes(path):
    return shutil.disk_usage(path).free


def bound_slot(target, max_bytes, min_free_bytes, dry_run=False, size=dir_size, free=free_bytes):
    """Keeps a folder slot's `target/` within `max_bytes` before a worker uses it.

    Returns None when the slot may be used, else why not. With the disk still
    short of `min_free_bytes` after that sweep, the whole `target/` goes (a
    slot's caches only save build time); short even then, the start is refused.
    `size` and `free` measure (tests pass fake values).
    """
    try:
        sweep(target, max_bytes, dry_run, size)
        if free(target) >= min_free_bytes:
            return None
        sweep(target, 0, dry_run, size)
    except BuildRunning as busy:
        return f"{busy} in the folder slot; stop that build, then start the worker again"
    if free(target) >= min_free_bytes:
        return None
    return (
        f"less than {min_free_bytes / GIB:g} GiB free even after removing the folder slot's target/: "
        "not starting a worker that would build there. Free disk space elsewhere, then start it again."
    )


def main():
    if fcntl is None:
        print("target sweep: not needed on Windows")
        return 0
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=["sweep", "guard", "slot"])
    parser.add_argument("--target", type=Path, default=Path(__file__).resolve().parent.parent / "target")
    parser.add_argument("--max-gib", type=float, help=f"default {DEFAULT_MAX_TARGET_GIB}, {DEFAULT_SLOT_MAX_TARGET_GIB} for slot")
    parser.add_argument("--min-free-gib", type=float, default=DEFAULT_MIN_FREE_GIB)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.max_gib is None:
        args.max_gib = DEFAULT_SLOT_MAX_TARGET_GIB if args.command == "slot" else DEFAULT_MAX_TARGET_GIB
    target = args.target
    if not target.is_dir():
        return 0
    if args.command == "slot":
        refusal = bound_slot(target, int(args.max_gib * GIB), int(args.min_free_gib * GIB), args.dry_run)
        if refusal:
            print(refusal, file=sys.stderr)
            return 1
        return 0
    try:
        if args.command == "sweep":
            freed = sweep(target, int(args.max_gib * GIB), args.dry_run)
            print(f"freed {freed / GIB:.1f} GiB" if freed else f"target is within {args.max_gib:g} GiB")
            return 0
        # guard: with plenty of room do nothing; short of it, sweep hard, then check again.
        if free_bytes(target) >= args.min_free_gib * GIB:
            return 0
        print(f"less than {args.min_free_gib:g} GiB free: sweeping target/", file=sys.stderr)
        sweep(target, 0 if args.max_gib <= 0 else int(args.max_gib * GIB) // 2, args.dry_run)
        if free_bytes(target) >= args.min_free_gib * GIB:
            return 0
        print(
            f"still less than {args.min_free_gib:g} GiB free after sweeping target/: not building. "
            "Free disk space elsewhere, then try again.",
            file=sys.stderr,
        )
        return 1
    except BuildRunning as busy:
        print(f"target_sweep: {busy}; try again when it is done", file=sys.stderr)
        return 1 if args.command == "guard" and free_bytes(target) < args.min_free_gib * GIB else 0


if __name__ == "__main__":
    sys.exit(main())
