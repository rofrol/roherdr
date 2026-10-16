# Golden set of the automatic review

Past catches of the resident coordinator, replayed blind against the
automatic review (`todo run --auto-review`) with `herdr todo review-eval`.
The replay lives in `src/workers/runs/review_eval.rs`.

## How cases are chosen

A case is one reviewed attempt of a `herdr todo run` whose review by the
coordinator sent the run back for another attempt. Its catches are the
distinct problems that review names in the code of the attempt's diff;
process remarks (a model error, a check left unrun) are left out, and an
attempt with no such catch is not a case.

The review checklist (`.herdr/review-checklist.md`) is distilled from catches
made before its cut-off, the RFC 3339 timestamp on its `# Cut-off:` line (the
commit that added the file), and stays frozen. Only catches made after the
cut-off are cases, so the replay never scores the checklist on what it was
distilled from. A test refuses a case whose `caught_at` is not after the
cut-off.

Each case is taken from `herdr todo review <run> --attempt N+1` of its run:
that attempt's task holds the coordinator's "Review of attempt N", the catch.
Left out: run r-23475ciu, attempt 1, whose review is a process remark (the
attempt ended with a model error); the panic it cites is in code the attempt
did not change.

## Format

One directory per case, named `<run>-a<attempt>`:

- `attempt.patch`: `git diff <base> <commit>` of the reviewed attempt.
- `case.json`:
  - `run`, `item`, `attempt`, `base`, `commit`: the run, its TODO item, the
    reviewed attempt, the run's base and the attempt's commit.
  - `subject`, `allowed_paths`: the attempt's commit subject and the paths
    its task allowed.
  - `caught_at` (RFC 3339) and `caught_at_source`: when the catch was made.
    The dumps carry no time of the review, so it is the committer date of
    the next attempt's commit (`git log`), which the review came before.
  - `source`: the dump and its section the catch was read from.
  - `task`, `worker_final_text`: the task the worker got and its last
    reply, from the dump of the reviewed attempt. They are part of what the
    review read.
  - `expected`: one entry per distinct catch, with `file`, `line` (only where
    the review cites one, against the attempt's commit), `summary` (one
    sentence) and `severe` (true when the problem would lose, corrupt or
    duplicate state, deadlock, break a published contract or weaken a safety
    gate).
  - `patch_omitted` and `patch_omitted_reason`: set instead of storing a
    patch over 300 KB; such a case is not replayed.

## The replay and its metric

For each case and arm, the replay adds a temporary detached worktree at the
case's `base`, applies `attempt.patch` and commits it, and builds the review's
input as the automatic review does, from that commit: the diff, the context
pack, the checklist (the repository's current, frozen one) and, with
`--credits`, the bounded review tools. What needs a live run is replaced: the
checks say "not verified", the run history is empty, the task and the
worker's reply come from `case.json` and the item's text from `TODO.md` at
the base. The input never holds `expected`; the replay refuses a case whose
input contains an expected entry's summary. The call goes through the
review's own path (`claude -p`, or the credit proxy with `--credits`) with
`[review] model`, at most twice as the review calls it. Every call is
recorded in the report: the input's SHA-256, the model, the raw output or the
error, the tool calls, and the kept and dropped findings with their origin
tag (`checklist` or `free`). The worktrees are removed afterwards, also on
an error.

Scoring is mechanical, with no model judging: an expected entry is matched
when a kept finding names the same file and, when both have a line, lies
within 15 lines of it. Each finding matches one entry at most; the entries
with a line take their nearest finding first. An entry without a line is
matched by any remaining finding in its file, which is lenient where a file
holds several such entries. The report gives, per case and arm and in
total: matched / expected, matched severe / expected severe, and the
findings that match no entry.

Arms: the full input always; `--without-context-pack` and
`--without-checklist` each add an arm without that part, each in its own
worktree from the same base.

Predeclared primary metric: the recall of the severe expected entries
(matched severe / expected severe, per arm). A severe entry that one arm
matched and another missed is reported as a severe regression of the arm
that missed it. A difference that is not significant on a set this small is
not evidence that the arms are equivalent.
