---
name: openrouter
description: Consult an OpenRouter model (default: the free cloaked `stealth/space-bunny-alpha`) for a second opinion, with hard guardrails against sending secrets. Use ONLY when the user asks to try/evaluate an OpenRouter or a cloaked/stealth model (e.g. "zapytaj Space Bunny", "spróbuj modelu z OpenRoutera"), never as a default consultant.
---

# Consulting an OpenRouter model (cloaked models such as Space Bunny Alpha)

Commands below use `$D` for this skill's directory (Claude Code shows it as "Base directory for this skill"; pi lists
the skill's location); set it first, e.g. `D=~/.claude/skills/openrouter` or `D=~/.pi/agent/skills/openrouter`.
`consult-stats` is always installed next to it, as `"$D/../consult-stats"`.

Goes through OpenRouter's OpenAI-compatible API, billed to the user's OpenRouter account. The bearer comes from pi
(`pi auth print-bearer-token --provider openrouter`), which refreshes the OAuth token; no key is stored in the skill.

```bash
"$D"/ask-bunny "question"                               # RECOMMENDED: sandboxed, no access to secrets/repo
"$D"/ask_openrouter.py "question"                       # raw client (no sandbox) — see the warning below
"$D"/ask_openrouter.py -m openai/gpt-4o-mini "q"        # any OpenRouter slug
```

## Safe entry point: `ask-bunny` (sandboxed)

Prefer **`ask-bunny`** on macOS. It fetches the OpenRouter token (outside the sandbox), stages a clean copy of this
skill into `/tmp/bunny`, and runs `ask_openrouter.py` under `sandbox-exec` with `bunny.sb`, so the process that talks
to the cloaked, logging provider **cannot read any secret or project file** — the kernel denies `~/.pi`, `~/.ssh`,
`~/.config`, `~/personal_projects`, and any `.env`/`*.key`/`credentials`/`auth.json` path (verified: all return
`PermissionError`). Network and `/tmp/bunny` are allowed. consult-stats logging still works (only byte counts).

Residual hole (known, per the 2026-10-02 consults): a caller that puts a secret into the prompt text itself, e.g.
`ask-bunny "$(cat ~/.env)"` — the `cat` runs in the caller's shell before the sandbox. The sandbox cannot stop that;
closing it needs the caller to also lack read access (a separate macOS account holding the key). So: synthetic/public
material only.

Optional extra layer — a restricted subagent so the orchestrating agent itself cannot read secrets to paste. Create
`~/.claude/agents/bunny.md` (Claude Code) with a minimal tool set that can only run `ask-bunny`:

```markdown
---
name: bunny
description: Ask the cloaked Space Bunny model for a synthetic/public second opinion. No repo or secret access.
tools: Bash
---
You can ONLY consult Space Bunny via `~/.claude/skills/openrouter/ask-bunny "<prompt>"`.
Never read files, never include real secrets, config or private code — synthetic or public material only.
```

(Not installed automatically: writing into `~/.claude` is a config change — ask the user first.)

Options: `-m SLUG` (default `stealth/space-bunny-alpha`, env `OPENROUTER_MODEL`), `-f FILE` (repeatable; needs
`--allow-files`; `- ` = stdin), `--allow-files`, `-s SYSTEM`, `-t SECONDS` (default 420), env `OPENROUTER_BASE_URL`,
`OPENROUTER_TIMEOUT`. Answers can take a few minutes — use a Bash timeout of 600000.

Inside herdr the script runs in its own herdr-job tab (no notification) so the user can watch it; the pre-send
payload summary is printed there.

## When to use it

- Only on an explicit request to try or evaluate an OpenRouter / cloaked model, or to run an A/B against the normal
  consultants. **Never** add it to the default consultation set (that stays sol + DeepSeek, Gemini/Claude on request).
- A cloaked model's provider is **anonymous** and **retains** the prompt and completion (OpenRouter Stealth Model
  Terms: not used for training, but logged by the unnamed lab). Treat everything sent as read by a third party.

## Privacy and secrets — read before sending any code

The real leak risk is not this script (it never reads the repository on its own; there is no `-r` mode) but **you,
the agent, pasting file contents into the prompt**. So:

- **Send synthetic or public material only.** Do not consult it about the user's private code, configuration or data
  unless the user explicitly says that exact content may go to an anonymous third party.
- Attachments are **off by default**. `-f` needs `--allow-files`, and each `-f` path is vetted fail-closed: it must be
  inside the current directory, have no symlink component, be a tracked, non-gitignored file in a git repo, not match
  the secrets denylist (`.env*`, `*.key`, `*.pem`, `auth.json`, `*secret*`, `*credential*`, …), not be binary, and be
  within a size cap. A path that fails any check is refused.
- The whole assembled prompt is scanned for secret-shaped strings (private-key blocks, AWS keys, JWTs, GitHub/Slack
  tokens, `NAME=secret` assignments, URLs with credentials). A match **hard-refuses** the send with no override — a
  human must sanitise and resend. Do not try to bypass it by reformatting a secret.
- The script prints the exact payload (size, sha256, attached files) before sending. Only byte counts go to
  consult-stats; the prompt and the answer are never logged.

Guidelines (as for the other consult skills):
- The model has no context of this conversation: put the goal, the (synthetic) code and the constraints in the prompt.
- Treat the answer as a second opinion, not ground truth — verify claims, and tell the user where you agree/disagree.
- **Identity is unverified by design.** A cloaked slug may resolve to a different, named model or an unknown provider.
  The script logs the served model and provider from `/api/v1/generation` and warns on a mismatch or when it cannot be
  read; report that to the user rather than assuming the slug is the model.
- Rate the call after triaging it (id printed on stderr as `[consult id: ...]`):
  `"$D/../consult-stats/consult.py" rate <id> useful|partial|useless --findings N --accepted N --unique N --note "..."`.
- In a multi-model round, start with `export CONSULT_ROUND=$("$D/../consult-stats/consult.py" new-round)` and launch
  all models in the same Bash call so their calls share the round id.
