---
name: openrouter
description: Consult an OpenRouter model (default: the free cloaked `stealth/space-bunny-alpha`; `-m mimo` = Xiaomi MiMo-V2.6-Pro pinned to Xiaomi) for a second opinion, with hard guardrails against sending secrets. Use when the user asks to try/evaluate an OpenRouter, cloaked/stealth or MiMo model (e.g. "zapytaj Space Bunny", "zapytaj MiMo", "spróbuj modelu z OpenRoutera"), and for the MiMo and Space Bunny trial rounds described inside; never as a default consultant otherwise.
---

# Consulting an OpenRouter model (cloaked models such as Space Bunny Alpha)

Commands below use `$D` for this skill's directory (Claude Code shows it as "Base directory for this skill"; pi lists
the skill's location); set it first, e.g. `D=~/.claude/skills/openrouter` or `D=~/.pi/agent/skills/openrouter`.
`consult-stats` is always installed next to it, as `"$D/../consult-stats"`.

Goes through OpenRouter's OpenAI-compatible API, billed to the user's OpenRouter account. The bearer comes from pi
(`pi auth print-bearer-token --provider openrouter`), which refreshes the OAuth token; no key is stored in the skill.

```bash
"$D"/ask_openrouter.sh "question"                     # RECOMMENDED: sandboxed, no access to secrets/repo
"$D"/ask_openrouter_raw.py "question"                 # raw client (no sandbox) — see the warning below
"$D"/ask_openrouter_raw.py -m openai/gpt-4o-mini "q"  # any OpenRouter slug
"$D"/ask_openrouter.sh -m mimo "q"                    # Xiaomi MiMo-V2.6-Pro, served only by Xiaomi
```

## Safe entry point: `ask_openrouter.sh` (sandboxed)

Prefer **`ask_openrouter.sh`** on macOS. It fetches the OpenRouter token (outside the sandbox), stages a clean copy of
this skill into a per-run directory under `/tmp/consult-openrouter` (parallel runs must not share one), and runs `ask_openrouter_raw.py` under `sandbox-exec` with `sandbox.sb`, so
the process that talks to the cloaked, logging provider **cannot read any secret or project file** — the kernel denies
`~/.pi`, `~/.ssh`, `~/.config`, `~/personal_projects`, and any `.env`/`*.key`/`credentials`/`auth.json` path (verified:
all return `PermissionError`). Paste the code the model needs into the prompt. Network and `/tmp/consult-openrouter` are
allowed. consult-stats logging still works (only byte counts).

Residual hole (known, per the 2026-10-02 consults): a caller that puts a secret into the prompt text itself, e.g.
`ask_openrouter.sh "$(cat ~/.env)"` — the `cat` runs in the caller's shell before the sandbox. The sandbox cannot stop that;
closing it needs the caller to also lack read access (a separate macOS account holding the key). So: synthetic/public
material only.

Optional extra layer — a restricted subagent so the orchestrating agent itself cannot read secrets to paste. Create
`~/.claude/agents/openrouter.md` (Claude Code) with a minimal tool set that can only run `ask_openrouter.sh`:

```markdown
---
name: openrouter
description: Ask an OpenRouter model for a second opinion through the sandboxed wrapper. No repo or secret access.
tools: Bash
---
You can ONLY consult OpenRouter via `~/.claude/skills/openrouter/ask_openrouter.sh "<prompt>"`.
Never read files, never include real secrets, config or private code — synthetic or public material only.
```

(Not installed automatically: writing into `~/.claude` is a config change — ask the user first.)

Options: `-m SLUG|ALIAS` (default `stealth/space-bunny-alpha`, env `OPENROUTER_MODEL`; aliases `mimo` =
`xiaomi/mimo-v2.6-pro`, `mimo-flash` = `xiaomi/mimo-v2.6-flash`, both pinned to provider Xiaomi), `--provider NAME`
(serve only through that OpenRouter provider, no fallbacks; env `OPENROUTER_PROVIDER`; overrides an alias's pin), `-f FILE` (repeatable; `-` =
stdin; `--allow-files` is still accepted but no longer needed), `-s SYSTEM`, `-t SECONDS` (default 420), env `OPENROUTER_BASE_URL`,
`OPENROUTER_TIMEOUT`. Answers can take a few minutes — use a Bash timeout of 600000.

Inside herdr the script runs in its own herdr-job tab (no notification) so the user can watch it; the pre-send
payload summary is printed there.

## When to use it

- On an explicit request to try or evaluate an OpenRouter / cloaked model, and in the two trials below: MiMo and
  Space Bunny each join the default round (sol + DeepSeek) with the same prompt and round id.
- A cloaked model's provider is **anonymous** and **retains** the prompt and completion (OpenRouter Stealth Model
  Terms: not used for training, but logged by the unnamed lab). Treat everything sent as read by a third party.

## Space Bunny trial (from 2026-10-02)

The user asked to add Space Bunny Alpha (the default model, no `-m`) to consultations. It is free while in preview,
so the cost is only the extra answer to read.

- Add `"$D"/ask_openrouter.sh "<same prompt>"` to every round, with the same prompt as the others, private code
  included: the user decided on 2026-10-02 that Space Bunny may get everything DeepSeek and MiMo get, accepting that
  its unnamed provider logs it. Secrets still never go (the secret scan hard-refuses them).
- Same scoring as the MiMo trial: rate every call, 20 rounds hard cap, stop after 8 rated rounds below 0.7 accepted
  unique findings per call or above 40% rejected. On a pass it can replace DeepSeek like MiMo could; a cloaked model can also vanish or turn into a paid named
  one, so re-check the slug when it errors.
- The served identity is unverified by design; consult-stats logs the provider the completion reports.

## MiMo trial (from 2026-10-02)

The user decided to trial Xiaomi MiMo-V2.6-Pro (`-m mimo`) through OpenRouter, pinned to the Xiaomi provider so the
prompt goes to one known party, not to whichever of the four hosts (GMICloud, DeepInfra, Novita, Xiaomi) OpenRouter
picks. Direct Xiaomi billing is not cheaper per token (same $0.435/$0.87 per M), and its Token Plan subscription
forbids calls from scripts. A consult costs about half a cent.

- The trial is an audition against DeepSeek, not an extra voice: in each consultation round add `-m mimo` next to the
  default pair (sol + DeepSeek), same prompt, same round id, and rate it like the others.
- 20 rounds, hard cap. Stop early after 8 rated rounds if MiMo's accepted unique findings per call are below 0.7 or
  more than 40% of its findings are rejected.
- Pass: MiMo beats DeepSeek by at least 0.5 accepted unique findings per call over the same rounds, with a rejected
  share no higher than DeepSeek's. On a pass MiMo replaces DeepSeek in the default pair; on a fail drop it. Compare
  with `consult.py stats --pairs`.
- Xiaomi publishes no retention or training terms for this API (China-based, like DeepSeek). Send it what you would
  send DeepSeek, never secrets; the secret scan still hard-refuses them. Attach files with `-f` as for DeepSeek: the
  `ask_openrouter.sh` sandbox cannot read the repo, so the wrapper vets each `-f` file outside it and passes in a copy.
- The served provider is taken from the completion (`provider` field), because `/generation` answers 404 for pi's
  OAuth token; consult-stats logs it as `xiaomi/mimo-v2.6-pro via Xiaomi`. A different provider prints a warning.

## Privacy and secrets — read before sending any code

The real leak risk is not this script (it never reads the repository on its own; there is no `-r` mode) but **you,
the agent, pasting file contents into the prompt**. So:

- **Private code is allowed, secrets never.** The user decided on 2026-10-02 that consults through this skill (Space
  Bunny and MiMo) may get the same code and context as DeepSeek. Never send credentials, keys, tokens, `.env` or
  auth files, or personal data of third parties.
- Attachments go only with an explicit `-f` (the `--allow-files` gate was dropped on 2026-10-03, when the user asked
  for `-f` to work for MiMo and Space Bunny). `ask_openrouter.sh` vets each path outside the sandbox (which cannot read
  the repo) and hands the sandboxed run only the vetted copy. Each `-f` path is vetted fail-closed: it must be
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
