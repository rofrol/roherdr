#!/usr/bin/env python3
"""Record in herdr's decision ledger the approvals the user gave on 2026-10-10,
before the ledger existed, so a coordinator finds them instead of asking again:

- the Claude integration install with the coordinator stop check in shadow mode;
- the host-configuration rule of the user's global agent rules (it holds in every
  repository);
- the grant of the repository's `prepare` operation.

Each is added with `herdr decision add ... --answer ... --source user` (or
`--source relayed --by NAME` for the host-configuration rule with
`--relayed-by NAME`) unless a record with the same statement is in the ledger
already, so running it again adds nothing. `--dry-run` prints the commands.

    scripts/decision_backfill.py [--repo DIR] [--relayed-by NAME] [--dry-run]
"""

import argparse
import json
import os
import subprocess
import sys

APPROVALS = [
    {
        "statement": "Install the Claude integration (`herdr integration install claude`) with "
                     "the coordinator stop check in shadow mode",
        "answer": "Approved by the user on 2026-10-10.",
        "scope": "integration",
        "item": "t-pzba6fio",
        "entry": "A state-based coordinator stop check, in shadow mode first",
        "global": False,
    },
    {
        "statement": "Host configuration that targets a project's app or process (key-remapper "
                     "exemptions, launcher or window rules) is that project's TODO coordinator's "
                     "responsibility: it updates it when a change alters what it matches and "
                     "checks the behavior it protects",
        "answer": "The user's global agent rules, 2026-10-10.",
        "scope": "host configuration",
        "global": True,
        "relayable": True,
    },
    {
        "statement": "Grant the repository's `prepare` operation (`.herdr/operations.toml`) as "
                     "`master` defines it (`herdr todo grant --operation prepare`)",
        "answer": "Granted by the user on 2026-10-10.",
        "scope": "todo grant",
        "global": False,
    },
]


def herdr():
    return os.environ.get("HERDR_BIN_PATH") or "herdr"


def recorded_statements(repo):
    """The statements of the records the ledger holds for `repo` and globally."""
    done = subprocess.run(
        [herdr(), "decision", "list", "--repo", repo, "--json"],
        capture_output=True, text=True, stdin=subprocess.DEVNULL,
    )
    if done.returncode != 0:
        raise SystemExit("herdr decision list failed: " + (done.stderr or done.stdout).strip())
    try:
        records = json.loads(done.stdout)["result"]["decisions"]
    except (ValueError, KeyError, TypeError):
        raise SystemExit("unreadable herdr decision list reply: " + done.stdout.strip()[:300])
    return {record.get("statement") for record in records if isinstance(record, dict)}


def command_of(approval, repo, relayed_by):
    """The `herdr decision add` argv that records one approval."""
    argv = [herdr(), "decision", "add", approval["statement"], "--answer", approval["answer"],
            "--scope", approval["scope"]]
    if relayed_by and approval.get("relayable"):
        argv += ["--source", "relayed", "--by", relayed_by]
    else:
        argv += ["--source", "user"]
    for field in ("item", "entry"):
        if approval.get(field):
            argv += ["--" + field, approval[field]]
    argv += ["--global"] if approval["global"] else ["--repo", repo]
    return argv


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=os.getcwd(), help="the herdr repository (default: .)")
    parser.add_argument("--relayed-by", help="the coordinator that relayed the host-config rule")
    parser.add_argument("--dry-run", action="store_true", help="print the commands only")
    args = parser.parse_args(argv)
    repo = os.path.abspath(args.repo)
    present = recorded_statements(repo)
    for approval in APPROVALS:
        if approval["statement"] in present:
            print("already recorded: " + approval["statement"][:70])
            continue
        command = command_of(approval, repo, args.relayed_by)
        if args.dry_run:
            print(" ".join(json.dumps(word) if " " in word else word for word in command))
            continue
        done = subprocess.run(command, text=True, stdin=subprocess.DEVNULL)
        if done.returncode != 0:
            return done.returncode
    return 0


if __name__ == "__main__":
    sys.exit(main())
