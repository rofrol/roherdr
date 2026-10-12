#!/usr/bin/env python3
"""Refuse outgoing additions matching locally configured private patterns.

No patterns or matched content are printed. With no local configuration this
optional guard is disabled. Scan each outgoing commit, not just the final tree:
a secret added and then removed still becomes public when its history is pushed.
"""
import argparse
import json
import re
import subprocess
import sys
from pathlib import Path


class CheckError(Exception):
    pass


def git(*args):
    result = subprocess.run(
        ["git", *args], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        check=False, input=b"",
    )
    if result.returncode:
        raise CheckError("cannot inspect outgoing history")
    return result.stdout


def patterns(root):
    path = root / ".local/private-patterns.txt"
    if not path.exists():
        return None
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        raise CheckError("cannot read local private patterns") from None
    compiled = []
    for number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        try:
            compiled.append(re.compile(line))
        except re.error:
            raise CheckError(f"invalid private pattern at configuration line {number}") from None
    if not compiled:
        raise CheckError("local private patterns file has no patterns")
    return compiled


def scan(base, head, rules):
    # Resolve revisions before passing them to rev-list; never interpret them
    # as options. An empty base means a new remote ref, so inspect all history.
    head = git("rev-parse", "--verify", "--end-of-options", head + "^{commit}").decode().strip()
    args = ["rev-list", head]
    if base:
        base = git("rev-parse", "--verify", "--end-of-options", base + "^{commit}").decode().strip()
        args += ["^" + base]
    refused = False
    for commit in git(*args).decode().splitlines():
        parents = git("rev-list", "--parents", "-n", "1", commit).decode().split()[1:]
        # Compare merges to their first parent too, including merge resolutions.
        before = parents[0] if parents else git("hash-object", "-t", "tree", "--stdin").decode().strip()
        paths = git("diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--name-only", "-z", before, commit).split(b"\0")
        for raw_path in filter(None, paths):
            path = raw_path.decode("utf-8", "surrogateescape")
            diff = git("diff", "--no-ext-diff", "--no-textconv", "--no-renames", "--no-color", "--text", "--unified=0", before, commit, "--", ":(literal)" + path)
            number = None
            for line in diff.decode("utf-8", "surrogateescape").split("\n"):
                hunk = re.match(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
                if hunk:
                    number = int(hunk[1])
                elif number is not None and line.startswith("+"):
                    if any(rule.search(line[1:]) for rule in rules):
                        # JSON escaping keeps filenames from injecting terminal controls.
                        print(f"private details refused: {json.dumps(path, ensure_ascii=True)}:{number}", file=sys.stderr)
                        refused = True
                    number += 1
                elif number is not None and line.startswith(" "):
                    number += 1
    return refused


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--range", nargs=2, metavar=("BASE", "HEAD"))
    group.add_argument("--pre-push", action="store_true")
    options = parser.parse_args()
    try:
        root = Path(git("rev-parse", "--show-toplevel").decode().strip())
        rules = patterns(root)
        if rules is None:
            return 0
        ranges = []
        if options.range:
            ranges.append(options.range)
        else:
            for line in sys.stdin:
                fields = line.split()
                if len(fields) != 4:
                    raise CheckError("invalid pre-push input")
                _, head, _, base = fields
                if set(head) == {"0"}:  # Deleting a ref adds nothing.
                    continue
                ranges.append(("" if set(base) == {"0"} else base, head))
        refused = False
        for base, head in ranges:
            refused = scan(base, head, rules) or refused
        return 1 if refused else 0
    except CheckError as error:
        # These messages are ours, never Git output or regex exception text.
        print(f"private details check failed: {error}", file=sys.stderr)
        return 2
    except OSError:
        print("private details check failed: cannot inspect outgoing history", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
