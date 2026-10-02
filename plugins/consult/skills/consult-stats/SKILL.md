---
name: consult-stats
description: 'Statistics of consulted models (gpt: astra/sol/terra, gemini flash, deepseek, claude Sonnet 5.5 skills) — which were most useful. Use when the user asks for consult/model statistics ("statystyki consult", "statystyki oracle", "który model najlepszy"), or to rate a past consultation.'
---

# Consult statistics

Commands below use `$D` for this skill's directory, the one holding this `SKILL.md` (Claude Code shows it as "Base directory for this skill", pi lists the skill's location); set it first, e.g. `D=~/.claude/skills/consult-stats` or `D=~/.pi/agent/skills/consult-stats`.

The gpt, gemini, deepseek and claude scripts log every call to `~/.local/state/consult/log.jsonl` (skill, model, effort, mode, status,
seconds, prompt/answer size, cwd, round id from `$CONSULT_ROUND`, token usage) and print `[consult id: XXXXXXXX]` on stderr. Usefulness comes from ratings:

```bash
O="$D/consult.py"
$O rate <id> useful|partial|useless [--findings N] [--accepted N] [--unique N] [--note "..."]
$O new-round            # round id: export CONSULT_ROUND=$($O new-round) before launching a round's models
$O self --round <round> --model <your model id> --findings N --accepted N --refuted N --unique N --missed N [--note "..."]
$O stats [--days 30]     # per skill/model: unique per rated call, wrong (rejected findings), rated/calls, err
$O stats --all           # + score, acc/find, speed, tokens, @high history, per coordinator table
$O stats --pairs         # + token efficiency (acc/1M output tokens) and paired within-round token ratios (e.g. sol vs astra)
$O recent [-n 20]        # latest calls with their ids and ratings (find unrated ones)
$O stats --by-alias      # group by the requested model (alias) instead of the version it resolved to
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

## You as coordinator

Never consult the model you are running on (`$PI_MODEL`): answers from the same model are not an independent second
opinion, and their `--unique` count is not meaningful (rate such a call with `--unique 0` and say so in the note).
Check the acting model before choosing the round's models. The default set and the running trials (MiMo, and Space
Bunny for public material only) are listed under "Default consultation set" in the gpt skill's `SKILL.md`.

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
