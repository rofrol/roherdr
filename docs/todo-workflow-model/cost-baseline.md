# TODO pipeline cost: baseline and estimates (2026-10-23)

The cost of working through the TODO with a long-lived coordinator and
headless workers, measured before the per-item design (see the README's
"Target design" and `PerItem` in `TodoWorkflow.lean`), and what each
variant was estimated to cost. After the cheaper design lands, run the same
script over a window that starts at the cutover and fill in the last
section. Consult rounds: `20261023-211512-5f06`, `20261023-211937-9803`,
`20261023-212316-447d`, `20261023-212927-b9d8`.

All dollar figures are API list-price equivalents (the 2026-10-06 price
table: Opus 5.5 $4 input, ~$5 cache write, $0.20 cache read, $20 output per
million tokens; Sonnet 5.5 $2, ~$2.50, $0.20, $10; Haiku 5.5 $0.10 input,
$0.50 output). Most of the work runs on a subscription, so read them as
shares of its limit.

## Baseline, everything up to 2026-10-23 (all Opus 5.5)

`python3 docs/todo-workflow-model/measure.py --until 2026-10-24`:

| Measure | Value |
|---|---|
| Worker runs | 468 |
| Worker cost per run (Claude Code's own figure) | median $1.59, mean $2.73, p90 ~$6.90 |
| Worker runs per item (`herdr worker runs`) | 2.16 (median 2) |
| Coordinator cost (Claude Code coordinator sessions) | $773 |
| Coordinator cost per item | $3.90-6.50, depending on the denominator: $4.89 per done `todo run` of this repository (158) |
| Coordinator calls per item | 32-53 |
| Coordinator mean context per call | ~490k tokens (one session: 547k over 115 items) |
| Share of coordinator cost that is cache reads | ~80% |
| Consult rounds started from this repository | 538 in total (~3 per item); external providers, cheap on the Claude side |

So an item cost about $10: ~$4-5 coordinator, ~$6 worker (2.2 runs). The
coordinator's cost is bound to its turns and its context, not its model:
Sonnet's cache-read price equals Opus's.

Known gaps: coordinator sessions under pi are not counted; the worker
journals cover every repository, so the worker cost per done run ($8.09)
is an upper bound; `herdr todo run` lines without a literal item id are
missed by the session filter. Item t-j2ayjqdl (turn kinds and every
worker's usage) replaces these estimates with recorded numbers.

## Estimates per variant

| Variant | Per item | Basis |
|---|---|---|
| A, today: Opus coordinator, Opus workers | ~$10 | measured |
| Opus coordinator at effort medium | -$0.1-0.2 | it trims only output (~$0.36 per item) |
| Sonnet coordinator | -$0.4 | same cache-read price; rejected |
| C: Opus coordinator, Sonnet workers | ~$8-8.5 | Sonnet halves only the non-cache share of a worker run (~30% per run); gone if runs per item rise ~20% |
| Haiku workers | worker ~$0.1-0.6 per run | only for mechanical items, pending its failure rate |
| Coordinator reads one item instead of TODO.md | -$0.5 or more | ~75k fewer tokens per call |
| Fresh coordinator session per item | coordinator ~$1.6-2.2 | only with the one-item read; otherwise 0-20% saving |
| B: no coordinator, one agent per item | ~$3-5 | needs an independent reviewer and an incremental TODO analysis |
| Target: per-item session, one-item read, class-routed workers, short-context review, consults only when needed | ~$3-4 | the models' range, to be measured |

Adoption criterion (the trial, 20-40 items stratified by item class): cost
per item at least 35% lower, with no rise in rework (amended or reverted
commits, follow-up items), lost questions or the user's interventions.

## After the cheaper model

To fill in after the cutover: `python3 docs/todo-workflow-model/measure.py --since <cutover date>`,
plus t-j2ayjqdl's per-turn numbers.

| Measure | Baseline | After |
|---|---|---|
| Coordinator cost per done run | $4.89 | |
| Coordinator mean context per call | ~490k | |
| Worker cost per run (mean) | $2.73 | |
| Worker runs per item | 2.16 | |
| Rework, lost questions, interventions | (from t-j2ayjqdl) | |

## The script

`measure.py` next to this file. Run it from the repository root; it reads
herdr's worker journals (`~/.local/state/herdr/workers/`), this
repository's Claude Code transcripts (`~/.claude/projects/<repository
path>/`), `herdr worker runs` and `herdr todo runs`, and prints JSON.
