#!/usr/bin/env bash
# Run the OpenRouter consult (ask_openrouter_raw.py) so the process that talks to the cloaked, logging provider has NO
# read access to the user's secrets or project tree — enforced by the macOS kernel via sandbox-exec.
#
# What this guarantees: the sandboxed Python cannot open ~/.pi, ~/.ssh, ~/.config, ~/personal_projects, or any
# .env/*.key/credentials file. The OpenRouter token is fetched OUTSIDE the sandbox (here) and handed in through the
# environment, so the sandboxed run never needs ~/.pi.
#
# What this does NOT stop (the known residual, per the 2026-10-02 consults): a caller that substitutes a secret into
# the prompt text itself, e.g. `ask_openrouter.sh "$(cat ~/.env)"` — the `cat` runs in the CALLER's shell, before the
# sandbox. Closing that needs the caller to also lack read access (a separate macOS account holding the key). Use
# this only for material without secrets.
#
# Usage: ask_openrouter.sh [ask_openrouter_raw.py args...]   e.g.  ask_openrouter.sh "Review this synthetic snippet: ..."
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "$(realpath "$0")")" && pwd)"          # .../skills/openrouter
SKILLS_ROOT="$(dirname "$SKILL_DIR")"                              # .../skills (has consult-stats sibling)
STAGE_ROOT=/tmp/consult-openrouter

# Token first, outside the sandbox (this is the one trusted step that may read pi's auth).
if ! TOK="$(pi auth print-bearer-token --provider openrouter --min-expiry 15m 2>/dev/null)" || [ -z "$TOK" ]; then
  echo "ask_openrouter.sh: no OpenRouter token from pi; log in to pi and authorise OpenRouter." >&2
  exit 1
fi

# Stage a clean copy of just the two skills the run needs, into a dir with no secrets. Preserve the sibling layout
# (openrouter/ + consult-stats/) so the script finds consult-stats for logging. Each run gets its own directory:
# rounds launch MiMo and Space Bunny in parallel, and a shared one let one run delete and rewrite sandbox.sb while
# the other was loading it ("sandbox-exec: no version specified", 2026-10-02).
mkdir -p "$STAGE_ROOT"
STAGE=$(mktemp -d "$STAGE_ROOT/run.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
cp -R "$SKILL_DIR" "$STAGE/openrouter"
cp -R "$SKILLS_ROOT/consult-stats" "$STAGE/consult-stats"
rm -rf "$STAGE/openrouter/__pycache__" "$STAGE/consult-stats/__pycache__"

# Expand @HOME@ in the profile (sandbox-exec does no variable expansion itself).
sed "s#@HOME@#$HOME#g" "$SKILL_DIR/sandbox.sb" > "$STAGE/sandbox.sb"

# Run from the clean staging dir: the repo cwd is read-denied, so os.getcwd() there would EPERM.
cd "$STAGE"

# CONSULT_IN_JOB=1 skips the herdr-job re-exec (the staged copy has no in_herdr_job sibling wiring to rely on).
# No exec: the EXIT trap removes this run's staging directory afterwards, keeping the script's exit code.
sandbox-exec -f "$STAGE/sandbox.sb" \
  /usr/bin/env OPENROUTER_BEARER="$TOK" CONSULT_IN_JOB=1 \
  /usr/bin/python3 "$STAGE/openrouter/ask_openrouter_raw.py" "$@"
