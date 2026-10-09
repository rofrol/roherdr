# Coordinator turn audit, 2026-10-09

Step 5 of the TODO item "A coordinator stops between items without being
asked" ([t-xe6hpo4z]): do real TODO coordinators still end a turn between
items now that the rule line (step 2) and the Stop hook (step 3) are in
place? Baseline (step 1, 2026-10-07): 3 coordinator sessions, 36 turn ends,
2 abandoned, 5.6 abandoned per 100 turn ends.

## Method

```bash
scripts/coordinator_turn_audit.py --since 2026-10-07T13:48:00+02:00
```

`--since` is new in this step (tested in `scripts/test_coordinator_turn_audit.py`):
it counts only turn ends at or after the time, by the turn's last assistant
entry, so the long sessions that started before the cutoff are measured on
the turns that followed it. JSON output now also carries each abandoned
turn's `ended` time and the `head` of its final text.

- Window: 2026-10-07 13:48 (local, UTC+2) to about 2026-10-09 14:15. The
  rule line and the hook went in earlier (`e238621c`, 03:31 local); 13:48 is
  the end of the five-run trial (`docs/coordinator-trial-2026-10-07.md`), so
  the window holds only real work and none of the trial's throwaway runs.
- Input: every `~/.claude/projects/*/*.jsonl` (main transcripts, not
  subagents), read-only.
- Coverage check: every transcript written since the cutoff that mentions
  "po kolei", "through the TODO", `/todo`, `herdr coordinator start`,
  `herdr todo run` or a coordinator tab role (190 files) was looked at by its
  first prompt. Apart from the five sessions below they are workers (their
  task quotes the TODO), headless-worker trials, `/todo` launchers that only
  started a coordinator elsewhere, or questions. No new TODO coordinator
  session started in the window; all five began on 2026-10-06/07 and kept
  running. One session that coordinated tabs without a TODO order
  (`music-mpd` `f983ed84`, "skoordynuj prace z tych zakładek") is out of
  scope.
- Cross-check: the Stop hook's log, `~/.local/state/herdr/awaiting-reply-stop.jsonl`.

## Counts

| Session | Repository | asked | waiting | abandoned | other | Turn ends |
|---------|-----------|------:|--------:|----------:|------:|----------:|
| `cdbf366a` | herdr | 6 | 178 | 0 | 3 | 187 |
| `5264f907` | guix/try-roguix | 6 | 25 | 0 | 0 | 31 |
| `e11f51c5` | rormpc | 7 | 5 | 0 | 4 | 16 |
| `a255c48e` | email-assistant | 3 | 3 | 0 | 0 | 6 |
| `ab7e9632` | job-seeker | 1 | 2 | 0 | 1 | 4 |
| **Total (5 sessions)** | | **23** | **213** | **0** | **8** | **244** |

**Abandoned per 100 turn ends: 0.0 (0 of 244), against 5.6 (2 of 36) in the
baseline.** With nothing labelled abandoned there is nothing to quote under
that label; every `other` and `asked` turn end was reviewed by hand instead
(below), and that review found three real stops the audit misses.

87% of turn ends are now `waiting` (a background wait on a worker, a
`herdr todo` run or a check was running), against 89% in the baseline: the
background-wait habit holds. Of the 213, 155 were woken by the task
notification and 56 by the user first; the longest such gap (try-roguix,
2026-10-08 16:11Z, 7.9 h) was a worker running that long, whose wait did
notify later (00:10Z).

## Review of every non-waiting turn end

- `asked` (23): each ran `herdr agent awaiting-reply` or AskUserQuestion,
  and the final text waits on something only the user can do: a choice
  between variants, freeing disk space, answering Claude's folder-trust
  prompt in a worker's tab, pressing a key for a live test, data for a
  courier. All are correct waits on the user.
- `other` in rormpc (4), job-seeker (1): reports with "Next, in order"
  empty (checked against the repositories' `TODO.md` at that time) or
  waiting on the user to restart an app. Correct ends.
- `other` in herdr (3): **real stops between items**. Each is a turn opened
  by a task notification (a worker or `herdr todo` run had ended), in which
  the coordinator reviewed and landed the result, wrote a report and ended
  the turn with no background wait, no question and "Next, in order" full
  (`TODO.md` at those times: 55, 56 and 59 open items, the next one named in the
  report itself).

| Session | Ended (UTC) | Final text, first 200 characters | Last line | What came next |
|---------|-------------|----------------------------------|-----------|----------------|
| `cdbf366a` | 2026-10-08 12:31 | Zrobione i zainstalowane. Uruchom `herdr`, żeby się podłączyć. W przestrzeni herdr powinieneś teraz zobaczyć: - **sidebar bez zakończonych pracowników:** zostają tylko żywi, a teraz żaden nie pracuje; | "Jeśli chcesz, mogę ją wziąć jako następną; w przeciwnym razie idę dalej kolejką." | The user 11 min later: "zdecyduj z modelami co dalej" |
| `cdbf366a` | 2026-10-08 18:14 | Sterownik `herdr todo run` przeprowadził pierwszą pozycję od początku do końca: start pracownika, przegląd, `verify`, cherry-pick, instalację, notatkę w TODO i push. Ja napisałem treść zadania, przejr | "Następne przebiegi przekażą środowisko przez handoff same." | The user 4 h 36 min later: "i co dalej?" |
| `cdbf366a` | 2026-10-09 09:54 | Żaden projekt open source nie robi tego wszystkiego, co DeltaDB, a samo DeltaDB nie jest wydane. Najbliżej są `git-ai` (pochodzenie poszczególnych linii) oraz biblioteki CRDT w Ruście (kotwice). Licen | "Następna w kolejce jest naprawa osieroconego brokera, który trzymał slot." | The user 2 min later with a new question |

Classification of all 244: 3 real stops between items, 23 correct waits on
the user, 213 background waits that woke the session (or were still running
when the user wrote first), 5 correct ends with nothing left; no audit false
positive (nothing was labelled abandoned). Counting the three missed stops,
the field rate is **1.2 per 100 turn ends (3 of 244)**. The baseline had one
unreviewed `other` as well, so its comparable rate lies between 5.6 and 8.3.

## The Stop hook in the window

- The hook acts only in a tab with the `coordinator` role. The herdr
  coordinator's tab had it from 2026-10-08 10:31 local on (109 logged stops);
  its 77 stops in the window before that, and every stop of the other four
  repositories' coordinators, ran without the role, so the hook could not
  have blocked them.
- Of the 109, two final texts matched the go-ahead expression ("Daj znać,
  gdy zwolnisz to miejsce…", "Daj znać, czy dopisać ten opis…"); both had
  run `herdr agent awaiting-reply`, so they were real questions and the hook
  correctly let them stop. It blocked nothing (`coordinator_blocked` never
  true).
- All three real stops happened while the tab had the role, and the hook
  let them through: their text asks for no go-ahead, so its `ABANDON`
  predicate does not match.

## What it means for the item

- The failure the item was opened for, ending with "I will delegate the next
  item when you say continue", did not recur in 244 real turn ends: the rule
  line removed that phrasing.
- A different mode remains: after a task notification, the coordinator lands
  the finished run, reports and ends the turn without starting the next item.
  Neither the audit's labels nor the hook's predicate see it, because both
  key on go-ahead wording. The cost was one wait of 4.6 h and two short
  ones.
- A predicate on state instead of wording would catch all three: a
  coordinator-role stop with no running background task, no question
  reported, and a non-empty "Next, in order". That is the hook variant the
  consult round rejected for its false-positive cost (it blocks a correct end
  when every remaining item waits on the user and the TODO was not updated to
  say so). Whether to try it, keep relying on `herdr todo run` plus the rule,
  or move the queue into the driver (the "durable item state machine" and
  "fresh coordinator per item" candidates) is the user's decision.
- The audit itself could flag the mode for the next measurement: an
  `other` turn end with no pending task and a non-empty "Next, in order" in
  the repository's `TODO.md` at that time. Not added here: it needs the
  repository history, not only the transcript.
- The hook covers only tabs that took the `coordinator` role; coordinators
  in other repositories ran without it for the whole window.
- Step 4 (headless runs with a question injected while the coordinator is
  idle between turns) remains open and still needs the user's go-ahead for
  its Claude usage.
