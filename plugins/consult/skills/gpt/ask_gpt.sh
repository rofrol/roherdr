#!/usr/bin/env bash
# Ask GPT via Codex CLI, billed to the ChatGPT subscription; credentials come from pi (~/.pi/agent/auth.json).
# Usage: ask_gpt.sh [-m astra|sol|terra|luna|<id>] [-e low|medium] [-r] [-f FILE]... "prompt"   (-f - reads stdin)
# -r: run Codex in the current git repo (read-only), so it can read files and git history itself.
set -euo pipefail
consult_dir="$(dirname "$(realpath "$0")")/../consult-stats"  # the skills live side by side, wherever they are linked from
orig=("$@")  # for the herdr-job re-run, once the model is known
model="${GPT_MODEL:-astra}"; effort=""; files=(); repo=""
while getopts "m:e:f:r" o; do
  case $o in m) model=$OPTARG;; e) effort=$OPTARG;; f) files+=("$OPTARG");; r) repo=1;; *) exit 2;; esac
done
shift $((OPTIND-1))
# No high/xhigh: in consult-stats it took ~3x the time and 4-7x the output tokens without more unique findings.
case $effort in high|xhigh) echo "Effort $effort is disabled; use the default, low or medium" >&2; exit 2;; esac
if [ -z "${CONSULT_IN_JOB:-}" ] && [ -n "${HERDR_SOCKET_PATH:-}" ] && command -v herdr-job >/dev/null; then
  exec "$consult_dir"/in_herdr_job.sh "gpt $model/${effort:-default}" "$0" ${orig[@]+"${orig[@]}"}  # watch it in its own herdr tab
fi
case $model in astra|sol|luna) model="gpt-6-$model";; terra) model=gpt-5.6-terra;; esac  # no GPT-6 Terra yet

prompt="$*"
# stdin only via -f -: a background job can inherit an open stdin that never sends EOF.
for f in ${files[@]+"${files[@]}"}; do
  [ "$f" = - ] && label=stdin || label=$f
  if [ "$f" = - ] && [ -n "${CONSULT_STDIN:-}" ]; then f=$CONSULT_STDIN; fi  # saved by in_herdr_job.sh
  prompt="$prompt"$'\n\n'"--- $label ---"$'\n'"$(cat "$f")"
done
# Regex match instead of ${prompt//[[:space:]]/}: the substitution is quadratic in bash and hangs on long prompts.
[[ $prompt =~ [^[:space:]] ]] || { echo "Empty prompt" >&2; exit 1; }

start=$SECONDS; answer_chars=""
# Log every call for consult-stats; logging must not change the exit code or fail the call.
consult_log() {
  local rc=$? usage=""
  # Token usage from the last Codex event that has one; read before $tmp goes away.
  usage=$(jq -c 'select(.usage? | type == "object") | .usage' "$tmp/events" 2>/dev/null | tail -1) || true
  rm -rf "$tmp" 2>/dev/null || true  # a straggling Codex child can still be writing there
  "$consult_dir"/consult.py log --skill gpt --model "$model" --effort "${effort:-default}" --mode "${repo:+repo}" \
    --status "$([ $rc = 0 ] && echo ok || echo error)" --seconds $((SECONDS-start)) \
    --prompt-chars ${#prompt} ${answer_chars:+--answer-chars $answer_chars} \
    ${usage:+--usage-raw "$usage"} ${usage:+--usage "$(jq -c '{input: .input_tokens, cached: .cached_input_tokens,
      output: .output_tokens, reasoning: (.reasoning_output_tokens // .reasoning_tokens)}' <<<"$usage" 2>/dev/null)"} || true
  exit $rc
}
tmp=$(mktemp -d); trap consult_log EXIT

# After the trap, so a failed login is logged as an error too.
# pi refreshes the OAuth token if needed and writes it back to its auth.json; Codex only gets a bearer token,
# so it never refreshes (rotating) tokens itself. Separate CODEX_HOME: ~/.codex (and its auth.json) is not used.
export PI_CODEX_TOKEN PI_CODEX_ACCOUNT
PI_CODEX_TOKEN=$(pi auth print-bearer-token --provider openai-codex --min-expiry 15m) || { echo "No openai-codex token in pi; log in to pi (/login)" >&2; exit 1; }
PI_CODEX_ACCOUNT=$(jq -er '."openai-codex".accountId' ~/.pi/agent/auth.json) || { echo "No openai-codex accountId in ~/.pi/agent/auth.json" >&2; exit 1; }

out="$tmp/answer"; mkdir "$tmp/cwd" "$tmp/home"
cwd="$tmp/cwd"
if [ -n "$repo" ]; then
  # The dotfiles env (GIT_DIR/GIT_WORK_TREE) would point git, and Codex's git commands, at the home repo.
  unset GIT_DIR GIT_WORK_TREE
  cwd=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "-r: $PWD is not in a git repository" >&2; exit 1; }
  [ "$cwd" != "$HOME" ] || { echo "-r: refusing to run in \$HOME (Codex would see the whole home directory)" >&2; exit 1; }
fi
provider='model_providers.pi={name="pi",base_url="https://chatgpt.com/backend-api/codex",wire_api="responses",env_key="PI_CODEX_TOKEN",env_http_headers={"chatgpt-account-id"="PI_CODEX_ACCOUNT"}}'
export CODEX_HOME="$tmp/home"
args=(exec --json --ignore-user-config --ephemeral --skip-git-repo-check -s read-only -C "$cwd" -m "$model" -o "$out"
      -c model_provider=pi -c "$provider")
[ -n "$effort" ] && args+=(-c "model_reasoning_effort=\"$effort\"")
# Codex's progress (stderr) goes to the herdr-job tab when there is one, its JSON events (with usage) to
# $tmp/events, the answer to $out.
progress=${HERDR_JOB_TTY:-/dev/null}
if ! printf '%s' "$prompt" | codex "${args[@]}" - 2>&1 >"$tmp/events" | tee "$tmp/err" >"$progress"; then
  cat "$tmp/err" >&2
  # With --json Codex reports API errors (e.g. 401) only as events, not on stderr.
  jq -r 'select(.type == "turn.failed") | .error.message' "$tmp/events" 2>/dev/null | tail -1 | cut -c1-500 >&2 || true
  exit 1
fi
answer_chars=$(wc -m <"$out" | tr -d " ")
cat "$out"
