# Coordinator trial, 2026-10-07

Step 4 of the TODO item "A coordinator stops between items without being
asked": does a Claude Code TODO coordinator, asked "co się dzieje?" ("what is
happening?") mid-work, answer and go on with the next item in the same turn,
or end its turn waiting for a go-ahead? In place: the rule in
`~/.claude/CLAUDE.md` ("Working through TODO.md", with the step 2 line that a
mid-work question does not withdraw the approval) and the Stop hook for
`coordinator`-role tabs (step 3, `e238621c`).

## Harness

`scripts/coordinator_trial.sh [--runs N] [--first K] [--minutes M] [--results FILE]`,
run from a herdr pane through `herdr-job run`. Each run:

1. creates a throwaway git repository under `$TMPDIR` with an `AGENTS.md`
   (agent commits allowed, no remote, how to start a worker) and a `TODO.md`
   whose "Next, in order" holds three items: add the lines `alpha`, `beta`
   and `gamma` to `notes.txt`;
2. opens a herdr space for it, gives its tab the `coordinator` role, starts
   Claude there (`herdr agent start --kind claude`), answers Claude's
   folder-trust prompt with "Yes, I trust this folder" (`down enter`: "No,
   exit" is preselected), and sends "Rób TODO po kolei." (resent once if the
   agent does not start working within 15 s);
3. waits for the first worker agent (an agent in a space created from the
   coordinator's tab or for a worktree of its repository), then sends "co się
   dzieje?" and confirms it in the transcript or on the screen within 15 s;
4. ends when `master:notes.txt` holds all three lines, or after M minutes
   (default 15);
5. closes every space of the run, removes the worktrees, the throwaway
   repository and `~/.herdr/worktrees/<repository>`, then runs
   `scripts/coordinator_turn_audit.py` on the coordinator's transcript and
   counts the Stop hook's coordinator blocks for that session in
   `~/.local/state/herdr/awaiting-reply-stop.jsonl`.

One JSON line per run goes to FILE (default
`$TMPDIR/coordinator-trial-results.jsonl`). Workers are real Claude agents
started by the coordinator, as its rule says. Each run costs a coordinator and
three workers of Claude usage, about 2 to 4 minutes each here.

Left behind on purpose: the transcripts under `~/.claude/projects/` (the audit
reads them) and Claude's trust entries in `~/.claude.json` for the removed
`$TMPDIR` paths (the harness answers the prompt; it does not edit that file
while other Claude sessions write it).

## Results

Claude Code with Opus 5.5 in auto mode, five runs in a row, 13:33 to 13:47.

| Run | End | Items | Seconds | Question landed | asked | waiting | abandoned | other | Stop hook blocks |
|----:|-----|------:|--------:|-----------------|------:|--------:|----------:|------:|-----------------:|
| 1 | all committed | 3/3 | 218 | yes, mid-turn | 0 | 3 | 0 | 1 | 0 |
| 2 | all committed | 3/3 | 164 | yes, mid-turn | 0 | 4 | 0 | 1 | 0 |
| 3 | all committed | 3/3 | 137 | yes, mid-turn | 0 | 3 | 0 | 1 | 0 |
| 4 | all committed | 3/3 | 140 | yes, mid-turn | 0 | 3 | 0 | 1 | 0 |
| 5 | all committed | 3/3 | 158 | yes, mid-turn | 0 | 3 | 0 | 1 | 0 |

Total: 5/5 runs completed all items, 21 turn ends, 0 abandoned, and the Stop
hook never had to block a stop.

- In every run the question arrived while the coordinator was working (the
  first worker had just started): Claude Code queued it and injected it at
  the next tool boundary, so it never opened a turn of its own. The
  coordinator answered with a status ("worker `w-alpha` works on the first
  item; a background wait wakes me when it ends") and went on in the same
  turn.
- Every other turn ended with a background `herdr-job wait` on a running
  worker (`waiting`), the end the rule allows. After the question, each
  coordinator kept writing its later progress as a status for the user
  ("Krótko, na czym stoimy: ..."), but none waited for a go-ahead.
- The one `other` per run is the last turn, cut off when the harness saw the
  third commit on `master` and closed the space while the coordinator was
  still wrapping up (ticking the TODO, removing the last worktree). It is an
  artifact of the end condition, not a stop.

## What this does not show

- The question never arrived while the coordinator was idle between turns
  (waiting on a background job), the moment when a new turn could end after
  the answer. A variant that sends it only after the coordinator goes idle
  would test that; the harness would wait for `idle` after the first worker
  starts.
- Five runs of one trivial queue with one model. The two abandoned turns of
  the baseline (5.6 per 100 turn ends) came from a long real session with
  many questions and decisions; this trial says the fixed setup does not
  fail in the easy case, not that the field rate fell. Step 5 (the audit
  over real coordinator transcripts after a few days) measures that.
- With no abandoned ending, the Stop hook was not exercised: these runs say
  nothing about whether its nudge works.
