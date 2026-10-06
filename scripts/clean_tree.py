#!/usr/bin/env python3
"""Runs a command in a clean tree: this checkout's HEAD plus only the paths you name.

Several agent sessions edit one shared checkout, so a build or test run there
also compiles whatever another session has half done. This keeps one
persistent worktree (its own target/ stays warm between runs), resets it to
this checkout's HEAD, applies the named paths' changes against HEAD (edits,
deletions, new files), and runs the command there.

  clean_tree.py run [PATH...] -- COMMAND...   with no PATH: HEAD as committed
  clean_tree.py path                          print the clean tree's directory

Name your own paths: the shared checkout cannot tell whose edits are whose, so
`git diff` of all of it would bring the other sessions' work along. A file that
another session also edits comes with their hunks too; check `git diff -- PATH`.

One run at a time holds the tree (an flock under .git/); others wait for it.
The tree is `../herdr-worktrees/clean-check` next to the checkout, or
$HERDR_CLEAN_TREE. Unix only.
"""
import fcntl
import os
import shutil
import subprocess
import sys
from pathlib import Path


def git(cwd, *args, check=True, binary=False):
    out = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=not binary)
    if check and out.returncode != 0:
        err = out.stderr if not binary else out.stderr.decode(errors="replace")
        raise SystemExit(f"git {' '.join(args)} failed: {err.strip()}")
    return out


def checkout_root():
    root = git(Path.cwd(), "rev-parse", "--show-toplevel").stdout.strip()
    return Path(root).resolve()


def tree_dir(root):
    configured = os.environ.get("HERDR_CLEAN_TREE")
    if configured:
        return Path(configured).expanduser().resolve()
    return (root.parent / "herdr-worktrees" / "clean-check").resolve()


def take_lock(root):
    common = Path(git(root, "rev-parse", "--git-common-dir").stdout.strip())
    if not common.is_absolute():
        common = root / common
    handle = open(common / "clean-tree.lock", "a+")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        handle.seek(0)
        holder = handle.read().strip() or "another run"
        print(f"clean tree: waiting for {holder}", file=sys.stderr, flush=True)
        fcntl.flock(handle, fcntl.LOCK_EX)
    handle.seek(0)
    handle.truncate()
    handle.write(f"pid {os.getpid()}: {' '.join(sys.argv[2:])[:200]}\n")
    handle.flush()
    return handle


def prepare(root, tree, paths):
    """Resets the tree to the checkout's HEAD and copies the named paths' changes into it."""
    base = git(root, "rev-parse", "HEAD").stdout.strip()
    for path in paths:
        resolved = (root / path).resolve()
        if resolved != root and root not in resolved.parents:
            raise SystemExit(f"clean tree: {path} is outside {root}")
    if not (tree / ".git").exists():
        tree.parent.mkdir(parents=True, exist_ok=True)
        git(root, "worktree", "add", "--detach", str(tree), base)
    # Untracked files from an earlier run go; ignored ones (target/) stay warm.
    git(tree, "checkout", "--detach", "--force", base)
    git(tree, "clean", "-fd")
    if not paths:
        return base, 0, 0
    patch = git(root, "diff", "--binary", "HEAD", "--", *paths, binary=True).stdout
    if patch:
        applied = subprocess.run(["git", "-C", str(tree), "apply", "--binary", "-"], input=patch,
                                 capture_output=True)
        if applied.returncode != 0:
            raise SystemExit(f"clean tree: the patch does not apply: {applied.stderr.decode(errors='replace').strip()}")
    new = [line for line in git(root, "ls-files", "--others", "--exclude-standard", "-z", "--", *paths)
           .stdout.split("\0") if line]
    for name in new:
        target = tree / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / name, target)
    changed = git(tree, "status", "--porcelain").stdout.count("\n")
    return base, changed, len(new)


def cmd_run(argv):
    if "--" not in argv:
        raise SystemExit("usage: clean_tree.py run [PATH...] -- COMMAND...")
    split = argv.index("--")
    paths, command = argv[:split], argv[split + 1:]
    if not command:
        raise SystemExit("usage: clean_tree.py run [PATH...] -- COMMAND...")
    root = checkout_root()
    tree = tree_dir(root)
    if root == tree:
        raise SystemExit("clean tree: run this from the shared checkout, not from the clean tree")
    lock = take_lock(root)
    try:
        base, changed, new = prepare(root, tree, paths)
        print(f"clean tree {tree}: {base[:12]} + {changed} changed paths ({new} new)", file=sys.stderr, flush=True)
        env = dict(os.environ, HERDR_CLEAN_TREE_BASE=base)
        return subprocess.run(command, cwd=tree, env=env).returncode
    finally:
        lock.close()


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "run":
        sys.exit(cmd_run(sys.argv[2:]))
    if len(sys.argv) == 2 and sys.argv[1] == "path":
        print(tree_dir(checkout_root()))
        return
    raise SystemExit(__doc__.strip())


if __name__ == "__main__":
    main()
