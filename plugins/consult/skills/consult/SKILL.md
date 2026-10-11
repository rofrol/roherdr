---
name: consult
description: Ask several other models for a second opinion in one round — use when the user says "pytaj modeli", "spytaj modeli", "skonsultuj z modelami", "ask the models" or "consult the models" without naming them, or before a non-trivial design decision. Runs the current default set with one command (`consult.py round`), excludes your own model family, and says how to rate and score a round.
---

# Consulting the default set of models

Commands below use `$D` for this skill's directory (Claude Code shows it as "Base directory for this skill"; pi lists
the skill's location); set it first, e.g. `D=~/.claude/skills/consult` or `D=~/.pi/agent/skills/consult`. The vendor
skills are installed next to it (`"$D/../consult-stats"` and one per vendor); each vendor skill's `SKILL.md` has its
options, privacy rules and limits.

A request to ask the models means consult now, in this turn: gather the facts, brief the models, verify and
report. If the same message also asks for a TODO entry, do both and record the outcome in the entry.

## Who joins a round

This skill names no models on purpose. The default set lives in one file, `consult-models.json` in the consult
plugin, and `consult.py round` reads it each time it runs, so a session that loaded this skill long ago still asks
the current set. Never pick the models yourself from memory or from an older skill text, and never call the vendor
scripts one by one for a default round: call only the command below.

- `--caller-model` names the model you run on (your exact model id, or its family); `round` leaves that family out,
  because consulting yourself is not a second opinion. Claude passes `--caller-model claude`; pi may omit it, since
  `round` reads `$PI_MODEL`. When the caller is unknown, `round` refuses to run rather than risk a
  self-consultation.
- When the user names the models, pass them with `--models a,b` (names from `consult-models.json`); the user's choice
  wins over the default set and over the family exclusion (`round` then only warns).
- A model that fails (a usage limit, an error) is named in the output and sits out that round; go on with the others
  and tell the user. The vendor skills say when to try it again.

## Running a round

1. Gather the facts from the code first. Write your own findings or hypotheses down **before** reading any answer
   (you score yourself against them).
2. Write one brief to a file: an explicit role (usually devil's advocate), the facts, the constraints, the questions,
   and a length cap. The models have no context of this conversation and no access to it. Never put secrets in it.
3. Run the round, in the foreground:
   `"$D/../consult-stats/consult.py" round --brief <file> --caller-model <your model id or family> [--models a,b]`.
   It mints one round id, asks the models in parallel, and prints the round id, then each model's answer with its
   status and call id, then which models were asked, skipped (and why) and failed. It exits non-zero only when no
   model answered.
4. Treat the answers as second opinions, not ground truth: verify each claim against the code, then report to the user
   in four buckets, each finding with the models that raised it and a one-line reason:
   - **Act on**: real issues for correctness, safety or maintainability given the actual goal; you do them.
   - **Consider**: valid, but you are not sure it is worth its cost now; the user decides.
   - **Noted**: valid but not actionable now (premature, low impact).
   - **Dismissed**: wrong, nitpicky or missing context, with why.
   Say where the models agreed and where they diverged; a finding raised independently by both weighs more. Name the
   models that were skipped or failed.
5. Rate each call by the call id `round` printed, with the same buckets:
   `"$D/../consult-stats/consult.py" rate <id> useful|partial|useless --findings N --accepted N --unique N
   --act N --consider N --noted N --dismissed N --note "..."`
   — `--unique` counts accepted findings the others (and you) missed; see the consult-stats skill for the fields.
   Then score yourself for the round with `consult.py self --round <round id>`.
6. Compare two models over shared rounds with `consult.py stats --vs A B` (a trial's verdict), never from the
   pooled table rows.
