---
name: gemini
description: Consult Google Gemini (Gemini 3.8 Flash) via Antigravity CLI (agy) + Google AI subscription for a second opinion — use when the user asks to "ask/consult Gemini", "zapytaj Gemini/Gemini'ego", or wants an independent review of a plan, bug hypothesis, design or code snippet from Google.
---

# Consulting Gemini

Commands below use `$D` for this skill's directory, the one holding this `SKILL.md` (Claude Code shows it as "Base directory for this skill", pi lists the skill's location); set it first, e.g. `D=~/.claude/skills/gemini` or `D=~/.pi/agent/skills/gemini`. `consult-stats` is always installed next to it, as `"$D/../consult-stats"`.

Goes through Antigravity CLI (`agy -p`, headless), billed to the user's Google AI subscription — not the API.
Uses agy's own login (`~/.gemini/antigravity-cli/`). Gemini CLI (`gemini`) no longer serves AI Pro/Ultra accounts; don't use it.

Only **gemini-3.8-flash-{high,medium,low}** is used (check with `agy models`; when a newer Flash appears, update the
`case` in ask_gemini.sh). Gemini Pro is disabled by the user's decision (2026-09-25): it drained the shared weekly
quota, had only `view_file` in agy, and scored lowest in consult-stats — the script refuses `-m pro`.

```bash
"$D"/ask_gemini.sh "question"                   # gemini-3.8-flash-high (default)
"$D"/ask_gemini.sh -e low "q"                   # effort: low|medium|high
"$D"/ask_gemini.sh -f src/foo.py "Find bugs in this file"
git diff | "$D"/ask_gemini.sh -f - "Review this diff"   # stdin only via -f -
"$D"/ask_gemini.sh -r "Review ... (see Code review below)"  # run in the current git repo
```

Options: `-m flash|<full id>`, `-e low|medium|high`, `-f FILE` (repeatable; `-f -` = stdin, never read implicitly), `-r` (repo mode), env `GEMINI_MODEL`.
The prompt goes to agy via stdin (stream-json), so large diffs are fine.

By default agy runs in an empty temp dir and sees only what you put in the prompt; it's told not to use tools.
With `-r` it runs at the top of the current git repo (refuses `$HOME`) and can view, list and search files itself;
everything it reads (including untracked files like `.env`) goes to Google, and agy loads the repo's AGENTS.md as rules.
The weekly quota is consumed by token cost (check with `agy -p /quota`).

Headless limits (agy 1.2.x):
- Writes are auto-denied, and so are shell commands except those in `permissions.allow` of
  `~/.gemini/antigravity-cli/settings.json` (e.g. `git status`, `find`, `odin`) — those *can* run in `-r` mode.
- A denied tool call ends the run with an empty answer; the script then exits 1 with "Empty answer".
  No shell means no `git log/diff` in `-r`: pass the diff with `-f -` alongside `-r`.
- Conversations are saved in agy's history (no ephemeral mode).
Answers usually take seconds to a couple of minutes — use a Bash timeout of 600000.

Inside herdr the script runs in its own herdr-job tab (no notification) so the user can watch it; output and exit code are unchanged.

Guidelines:
- Gemini has no context of this conversation: include the goal, relevant code and constraints in the prompt.
- Sending code sends it to Google's servers. Don't send secrets, credentials, or code the user marked as confidential; ask first if unsure.
- Treat the answer as a second opinion, not ground truth — verify claims, and tell the user where you agree/disagree.
- If the user asks for several models ("Gemini i GPT", "wszystkie"), run them in parallel and compare. Who joins
  a round by default (and the running trials) is set under "Default consultation set" in the gpt skill's `SKILL.md`.
- Data collection for consult-stats (since 2026-09-26; only high had been used before): whenever you consult Gemini,
  ask it **three times in parallel with `-e low`, `-e medium` and `-e high`**, same prompt, same round (unless the user
  named one effort), and rate each call separately — so the efforts are compared on the same task. This uses about
  three times the weekly quota per consultation.
- The helper does not preflight quota or cache availability. It attempts the requested call once;
  agy decides whether the model is available. A rejected call exits 1 and is logged as an error.
  After an actual quota/usage-limit rejection, do not retry or silently swap models. In a multi-model
  round continue with the others and report Gemini's error, including the reset time if supplied.
  A quota display alone is not a rejection.
- After triaging the answer, rate it (id is printed on stderr as `[consult id: ...]`):
  `"$D/../consult-stats/consult.py" rate <id> useful|partial|useless --findings N --accepted N --unique N --note "..."`
  — see the consult-stats skill for what the fields mean. Then score yourself for the round with `consult.py self`
  (write your own findings down before reading the answers).
- Every consultation is a round: start the command with `export CONSULT_ROUND=$("$D/../consult-stats/consult.py" new-round)`
  and launch all models for that question in the same Bash call, so their calls share the round id
  (paired token comparisons in `consult.py stats --pairs`; `consult.py self --round <id>`).

## Code review

For reviewing changes in a repo use `-r` plus the diff on stdin, so the reviewer can check callers and tests itself:

```bash
git diff <base>...HEAD | "$D"/ask_gemini.sh -r -f - "$(cat <<'EOF'
Independently review this change (diff below) for actionable correctness, security and regression bugs.
Intent / acceptance criteria: ...
Constraints: ...
Tests actually run: ...
Inspect the relevant repository context (callers, tests) with your file tools.
Report only problems introduced by this change. For each: file:line, trigger, impact, evidence.
Separate demonstrated bugs from unverified concerns. "No actionable findings" is a valid answer.
EOF
)"
```

Give intent, not a summary of the code. Review consequential changes (auth, migrations, concurrency, data integrity,
public APIs, uncertain diagnoses), not routine edits. Each review is a fresh session.

After the review:
- Triage every finding as accepted / rejected / needs user decision. Reject only with concrete evidence
  (counterexample, invariant, code path), not "I disagree". Before fixing an alleged bug, trace it and preferably
  reproduce it or add a regression test.
- Show the user the triage. Questions of intent, scope and tradeoffs are the user's call.
- At most one fix round plus one focused re-check. If serious issues remain, stop and ask the user.
