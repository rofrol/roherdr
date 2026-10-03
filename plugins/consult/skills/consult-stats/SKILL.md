---
name: consult-stats
description: 'Statistics of consulted models (gpt: astra/sol/terra, gemini flash, deepseek, claude Sonnet 5.5 skills) — which were most useful. Use when the user asks for consult/model statistics ("statystyki consult", "statystyki oracle", "który model najlepszy"), or to rate a past consultation.'
---

# Consult statistics

Commands below use `$D` for this skill's directory, the one holding this `SKILL.md` (Claude Code shows it as "Base directory for this skill", pi lists the skill's location); set it first, e.g. `D=~/.claude/skills/consult-stats` or `D=~/.pi/agent/skills/consult-stats`.

The gpt, gemini, deepseek, claude and openrouter scripts log every call to `~/.local/state/consult/log.jsonl` (skill, model, effort, mode,
status, error kind, seconds, prompt/answer size, cwd, round id from `$CONSULT_ROUND`, token usage) and print `[consult id: XXXXXXXX]` on
stderr. Usefulness comes from ratings:

```bash
O="$D/consult.py"
$O rate <id> useful|partial|useless [--findings N] [--accepted N] [--unique N] [--note "..."]
$O new-round            # round id: export CONSULT_ROUND=$($O new-round) before launching a round's models
$O self --round <round> --model <your model id> --findings N --accepted N --refuted N --unique N --missed N [--note "..."]
$O stats [--days 30]     # per skill/model: call dates, unique per rated call, wrong (rejected findings), rated/calls, err
$O stats --all           # + score, acc/find, latency (n, p50, p90), tokens, errors by kind, rounds table, @high history,
                         #   per coordinator table
$O stats --pairs         # + token efficiency (acc/1M output tokens) and paired within-round token ratios (e.g. sol vs astra)
$O recent [-n 20]        # latest calls with their ids and ratings (find unrated ones)
$O stats --by-alias      # group by the requested model (alias) instead of the version it resolved to
                        # --width N (stats, recent): fit to N columns for a pager; agents read the default
$O stats --vs mimo deepseek [--since ROUND] [--rounds 20]
                        # head-to-head over rounds where both answered and were rated: findings, rejected n (%),
                        # uniq/call, acc/call, tokens, p50; paired uniq/call difference with a round-bootstrap CI and
                        # W/T/L, rejected-share difference. Numbers only: judge a trial's rule yourself. A and B match
                        # row labels by substring; a round where one matches two calls is left out and counted.
```

Stats name a model by the version the provider reported for the call (`model_version`, e.g. DeepSeek's
`deepseek-flash` alias is logged as served by `DeepSeek-V4.1-Flash`), so a newer model behind the same alias gets its own
row. DeepSeek calls logged before 2026-09-28 have no version and stay under the alias: their version is unknown, not
assumed. GPT and Gemini are called with explicit model ids, so they log none.

Rate after triaging the answer, not on first read:
- **useful**: changed what we did (a real bug, a better design, a disproved hypothesis); **partial**: something valid but
  minor or already known; **useless**: nothing actionable, wrong, or no answer.
- `--findings`: distinct claims/issues raised; `--accepted`: how many survived verification;
  `--unique`: accepted ones that neither you (the coordinator) nor another model in the same round had. `unique` is the key signal.
- Be honest and consistent across models; don't upgrade a verdict because the model agreed with you.
- `--note`: a few words on why (e.g. "caught race in cache invalidation", "hallucinated API").

When showing stats, point out small samples (<5 rated calls per model) instead of drawing conclusions from them.

## Errors and latency

A failed call records why in `error_kind`: `limit` (rate limit or exhausted plan), `auth`, `model` (unknown or
unsupported model id), `timeout`, `empty`, `server`, `network`, `other` (a reason was seen but not recognized). The
wrapper passes the kind when it knows it, otherwise its error text on `--error-text-file`, which `consult.py` classifies
and never stores (stderr and provider bodies can echo the prompt or credentials). Calls logged before 2026-10-02 have no
kind and show as `unknown`; do not backfill them by guessing.

Latency columns cover ok calls only: the median and a nearest-rank p90, shown from 10 calls on (below that it would just
be the maximum). The rounds table answers "which model does a round wait for": per model, the rounds it joined, how
often it finished last alone (by logged end time, so a model launched late is not blamed for the others), how often
that last call had failed, and the gap to the next model's end. A model that only fails fast never shows up there;
read its errors in the error table.

## You as coordinator

Never consult the model you are running on (`$PI_MODEL`): answers from the same model are not an independent second
opinion, and their `--unique` count is not meaningful (rate such a call with `--unique 0` and say so in the note).
Check the acting model before choosing the round's models. The default set and the running MiMo trial are
listed in the `consult` skill's `SKILL.md`.

The agent that asks (Claude Code, pi) is scored too, once per round (all consult calls on the same question), with `self`:
- **Before reading any model's answer**, write down your own findings/hypotheses (in the conversation or a scratchpad
  file). Counting them afterwards is biased — the models' answers leak into what you "already knew".
- After triage: `--findings` your own claims; `--accepted` how many survived verification; `--refuted` your claims
  disproved by a consulted model or by verification (your errors); `--unique` accepted ones no consulted model had; `--missed` accepted
  findings of consulted models you did not have. The entry records which agent you are (`claude-code`, `pi`),
  your model id and reasoning effort, with where each came from: pi gives both (`$PI_MODEL`, `$PI_REASONING_LEVEL`);
  Claude Code gives only the effort (`$CLAUDE_EFFORT`), so in Claude Code always pass `--model` with your exact model
  id (e.g. claude-opus-5-5). `--model`/`--effort` override; anything unknown is logged as `unknown`, never guessed.
  The coordinator table groups by `agent/model@effort`.
- `--note`: what you got wrong or missed (e.g. "assumed MBID stable across releases; missed video recordings").
- Log it even for a single-model round. Re-running `self` with the same round (or calls) replaces the entry.
  `recent` lists rated calls that have no coordinator entry yet.

## Tokens

Usage is normalized across vendors: `input` includes `cached`, `output` includes `reasoning` (GPT's
`reasoning_output_tokens`, Gemini's `thinking_tokens`, DeepSeek's `reasoning_tokens`); the provider's own object is kept
as `usage_raw`. Codex and agy add ~10k input tokens of their own system prompt, so compare output tokens.
When reading `stats --pairs`: compare tokens only within one vendor (tokenizers differ), trust paired rounds over
per-model sums, and remember `unique` depends on who else was asked (a model asked alone gets everything as unique).
Calls before 2026-09-25 have no usage.
