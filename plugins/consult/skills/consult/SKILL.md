---
name: consult
description: Ask several other models for a second opinion in one round — use when the user says "pytaj modeli", "spytaj modeli", "skonsultuj z modelami", "ask the models" or "consult the models" without naming them, or before a non-trivial design decision. Holds the default set (Sol + MiMo), the no-self-consultation rule, and how to run, rate and score a round.
---

# Consulting the default set of models

Commands below use `$D` for this skill's directory (Claude Code shows it as "Base directory for this skill"; pi lists
the skill's location); set it first, e.g. `D=~/.claude/skills/consult` or `D=~/.pi/agent/skills/consult`. The vendor
skills are installed next to it: `"$D/../gpt"`, `"$D/../deepseek"`, `"$D/../openrouter"`, `"$D/../claude"`,
`"$D/../gemini"`, `"$D/../consult-stats"`. Each vendor skill's `SKILL.md` has its options, privacy rules and limits.

A request to ask the models means consult now, in this turn: gather the facts, brief the models, verify and
report. If the same message also asks for a TODO entry, do both and record the outcome in the entry.

## Who joins a round

- Default set (revised 2026-10-03 by the user: **sol 6.1 (default effort) plus Xiaomi MiMo** in parallel, unless
  the user named models). Ask MiMo with `"$D/../openrouter/ask_openrouter.sh"` (MiMo is its default model).
  DeepSeek left the default set on 2026-10-03: in shared rounds it was the weakest of the three (sol +0.67 accepted
  unique findings per call over 161 rounds, MiMo +0.48 over 58 rounds and +0.45 in its 20-round trial), and its
  speed bought nothing because a round waits for sol anyway. It was not dropped for cost (about a cent a call); it
  stays available on request. **Astra is off by default for now, available on request**. The 2026-09-27 astra/luna
  trial is still the only paired evidence: luna@medium was clearly behind astra (0.58 vs 1.43 accepted unique findings
  per rated call and 22% vs 35% rejected findings; astra 69, luna@medium 33 rated calls), and gpt-6-sol never beat
  astra over ~12 paired rounds.
  Don't ask luna routinely: it adds an answer to read (Claude tokens) for little new. Use luna with `-e medium` only
  as a fallback when a sol call fails or hits the Plus limit. Terra only on request.
- Space Bunny was removed on 2026-10-03 (failed its trial); the MiMo trials are recorded in the openrouter skill.
- Never consult the model you are running on: that is a self-consultation, not a second opinion. Check your own model
  first (`$PI_MODEL`, or the model id you were given) and drop it from the set. When the acting model is MiMo, the
  pair is **sol + DeepSeek**; when it is GPT, **MiMo + Claude Sonnet**; Claude and DeepSeek ask the default sol + MiMo.
- A vendor that answers with a usage-limit error sits out that round; go on with the others and tell the user. The
  vendor skills say when to try it again.

## Running a round

1. Gather the facts from the code first. Write your own findings or hypotheses down **before** reading any answer
   (you score yourself against them).
2. Brief every model with the same prompt: an explicit role (usually devil's advocate), the facts, the constraints,
   the questions, and a length cap. The models have no context of this conversation and no access to it.
3. Every consultation is a round: start the command with
   `export CONSULT_ROUND=$("$D/../consult-stats/consult.py" new-round)` and launch all models for that question in the
   same Bash call, so their calls share the round id. A `herdr-job run` tab does not inherit that shell's exports:
   pass the id on the command, `herdr-job run ... -- env CONSULT_ROUND="$R" <ask command>`, or the calls get no
   round and `consult.py self --round` finds none (2026-10-06).
4. Treat the answers as second opinions, not ground truth: verify each claim against the code, then report to the user
   in four buckets, each finding with the models that raised it and a one-line reason:
   - **Act on**: real issues for correctness, safety or maintainability given the actual goal; you do them.
   - **Consider**: valid, but you are not sure it is worth its cost now; the user decides.
   - **Noted**: valid but not actionable now (premature, low impact).
   - **Dismissed**: wrong, nitpicky or missing context, with why.
   Say where the models agreed and where they diverged; a finding raised independently by both weighs more.
5. Rate each call (id printed on stderr as `[consult id: ...]`), with the same buckets:
   `"$D/../consult-stats/consult.py" rate <id> useful|partial|useless --findings N --accepted N --unique N
   --act N --consider N --noted N --dismissed N --note "..."`
   — `--unique` counts accepted findings the others (and you) missed; see the consult-stats skill for the fields.
   Then score yourself for the round with `consult.py self --round <id>`.
6. Compare two models over shared rounds with `consult.py stats --vs A B` (a trial's verdict), never from the
   pooled table rows.
