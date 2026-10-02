---
name: gpt
description: Consult OpenAI GPT (GPT-6.1 Sol, GPT-6 Astra/Luna, GPT-5.6 Terra) via Codex CLI + ChatGPT subscription for a second opinion — use when the user asks to "ask/consult GPT", "zapytaj GPT/Astrę/Sol/Terrę/Lunę", or wants an independent review of a plan, bug hypothesis, design or code snippet from OpenAI. Sends a prompt you write straight to GPT and logs the call for consult-stats; not the pi-fabric `oracle` reviewer.
---

# Consulting GPT

Commands below use `$D` for this skill's directory, the one holding this `SKILL.md` (Claude Code shows it as "Base directory for this skill", pi lists the skill's location); set it first, e.g. `D=~/.claude/skills/gpt` or `D=~/.pi/agent/skills/gpt`. `consult-stats` is always installed next to it, as `"$D/../consult-stats"`.

Goes through Codex CLI (`codex exec`), billed to the user's ChatGPT Plus subscription — not the API.
Credentials come from pi: `openai-codex` in `~/.pi/agent/auth.json` (token via `pi auth print-bearer-token`, which refreshes it).
~/.codex (config.toml, auth.json) is not used — the script runs with `--ignore-user-config` and a temporary `CODEX_HOME`.

Models available on ChatGPT (as of 2026-09-30): `sol` = **gpt-6.1-sol**, `astra` = **gpt-6-astra**,
`luna` = **gpt-6-luna**; `terra` = **gpt-5.6-terra** (no GPT-6 Terra yet); the older `gpt-6-sol` needs its full id.
Check with `jq -r '.models[].slug' ~/.codex/models_cache.json`; when a new slug appears there, update the `case` in ask_gpt.sh.

```bash
"$D"/ask_gpt.sh "question"                  # sol = gpt-6.1-sol (default)
"$D"/ask_gpt.sh -m astra "q"                # gpt-6-astra; also terra (gpt-5.6-terra), luna (gpt-6-luna, fast);
                                            # the older sol as -m gpt-6-sol
"$D"/ask_gpt.sh -f src/foo.py "Find bugs in this file"
git diff | "$D"/ask_gpt.sh -f - "Review this diff"   # stdin only via -f -
"$D"/ask_gpt.sh -r "Review ... (see Code review below)"  # run in the current git repo, read-only
```

Options: `-m sol|astra|luna|terra|<full id>` (default `sol` = gpt-6.1-sol), `-e low|medium` (reasoning effort; high/xhigh are disabled by the user's decision (2026-09-26): ~3× the time and 4–7× the output tokens without more unique findings — the script refuses them), `-f FILE` (repeatable; `-f -` = stdin, never read implicitly), `-r` (repo mode), env `GPT_MODEL`.
Codex runs ephemeral, in a read-only sandbox. By default it runs in an empty temp dir and sees only what you put in the prompt.
With `-r` it runs at the top of the current git repo (refuses `$HOME`), so it can read files, callers, tests and git history itself;
everything in that checkout it reads (including untracked files like `.env`) goes to OpenAI.
Answers can take a few minutes — use a Bash timeout of 600000.

Inside herdr the script runs in its own herdr-job tab (no notification) so the user can watch it; output and exit code are unchanged.

Guidelines:
- GPT has no context of this conversation: include the goal, relevant code and constraints in the prompt.
- Sending code sends it to OpenAI's servers. Don't send secrets, credentials, or code the user marked as confidential; ask first if unsure.
- Treat the answer as a second opinion, not ground truth — verify claims, and tell the user where you agree/disagree.
- Default consultation set (revised 2026-09-30 by the user: **sol 6.1 (default effort) plus DeepSeek** in parallel,
  unless the user named models; **astra is off by default for now, available on request**). The 2026-09-27 astra/luna
  trial is still the only paired evidence: luna@medium was clearly behind astra (0.58 vs 1.43 accepted unique findings
  per rated call and 22% vs 35% rejected findings; astra 69, luna@medium 33 rated calls), and gpt-6-sol never beat
  astra over ~12 paired rounds. **gpt-6.1-sol is new and unevaluated**, so rate its calls and revisit this default.
  Don't ask luna routinely: it adds an answer to read (Claude tokens) for little new. Use luna with `-e medium` only
  as a fallback when a sol call fails or hits the Plus limit. Terra only on request. Rate each call separately;
  `--unique` counts what the others (and Claude) missed.
- MiMo trial (from 2026-10-02, up to 20 rounds): add Xiaomi MiMo to each default round with the openrouter skill's
  `ask_openrouter.sh -m mimo`, launched in the same Bash call; the trial rules are in that skill's `SKILL.md`.
  Space Bunny trial (same date, same skill, `ask_openrouter.sh` without `-m`): add it too, but only to rounds whose
  prompt holds no private code or data, because its anonymous provider logs everything.
- Never consult the model you are running on: that is a self-consultation, not a second opinion. Check your own model
  first (`$PI_MODEL`, or the model id you were given) and drop it from the pair. When the acting model is DeepSeek,
  the pair is **sol + Claude Sonnet** (Gemini is the alternative); when it is Claude, ask sol + DeepSeek.
- If the user asks for "GPT and DeepSeek", run both in parallel and compare.
- On a usage-limit error, tell the user (Plus limits) and don't retry in that round (no other GPT model either); in a multi-model round go on with the others. The reported reset time is not reliable (on 2026-09-26 a limit said "try again tomorrow" and cleared within two hours), so try GPT once again at the next consultation in the session; after a second limit error in a row, skip it for the rest of the session.
- After triaging the answer, rate it (id is printed on stderr as `[consult id: ...]`):
  `"$D/../consult-stats/consult.py" rate <id> useful|partial|useless --findings N --accepted N --unique N --note "..."`
  — see the consult-stats skill for what the fields mean. Then score yourself for the round with `consult.py self`
  (write your own findings down before reading the answers).
- Every consultation is a round: start the command with `export CONSULT_ROUND=$("$D/../consult-stats/consult.py" new-round)`
  and launch all models for that question in the same Bash call, so their calls share the round id
  (paired token comparisons in `consult.py stats --pairs`; `consult.py self --round <id>`).

## Code review

Use `-r` for reviewing changes in a repo, so the reviewer gathers evidence itself instead of seeing only what you picked.
Review consequential changes (auth, migrations, concurrency, data integrity, public APIs/protocols, unfamiliar code,
uncertain diagnoses), not routine edits. For expensive or hard-to-reverse designs, review the plan before implementing.

Give intent, not a summary or selection of the code. Prompt template:

```text
Independently review this change for actionable correctness, security and regression bugs.
Intent / acceptance criteria: ...
Constraints: ...
Scope: base <SHA>, head <SHA> (plus staged/unstaged changes, if intended)
Tests actually run: ...
Inspect the diff and the relevant repository context (callers, tests, history).
Report only problems introduced by this change. For each: file:line, trigger, impact, evidence.
Separate demonstrated bugs from unverified concerns. "No actionable findings" is a valid answer.
Do not edit files.
```

Use explicit SHAs: `master...HEAD` excludes uncommitted changes.
Each review is a fresh session; don't carry a reviewer across tasks.

After the review:
- Triage every finding as accepted / rejected / needs user decision. Reject only with concrete evidence
  (counterexample, invariant, code path), not "I disagree". Before fixing an alleged bug, trace it and preferably
  reproduce it or add a regression test.
- Show the user the triage. Questions of intent, scope and tradeoffs are the user's call; present both positions
  and a recommendation instead of arguing with the reviewer.
- At most one fix round plus one focused re-check of the disputed findings and the fixes. If serious issues remain,
  stop and ask the user; the plan is probably wrong.
