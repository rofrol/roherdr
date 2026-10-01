#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: scripts/fork_demo/record.sh [OUTPUT.mp4]

Records the fork demo video (usage widget, middle-click close, notification
click, job tabs) from a debug herdr build in a throwaway session. Defaults to
assets/fork-demo.mp4. See scripts/fork_demo/README.md.
USAGE
}

case "${1:-}" in
  -h|--help)
    usage
    exit 0
    ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/../.." && pwd)"
output="${1:-$repo_dir/assets/fork-demo.mp4}"

for tool in cargo uv ffmpeg; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

cargo build --manifest-path "$repo_dir/Cargo.toml"
herdr="$repo_dir/target/debug/herdr"

# Unix socket paths are short, so the socket lives in a short /tmp dir.
work="$(mktemp -d /tmp/herdr-demo.XXXXXX)"
socket="$work/api.sock"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then
    HERDR_SOCKET_PATH="$socket" "$herdr" server stop >/dev/null 2>&1 || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$work/config/herdr-dev" "$work/state" "$work/zsh" "$work/frames"
# Notifications are off so the recording never pops real banners on screen.
cat >"$work/config/herdr-dev/config.toml" <<'EOF'
onboarding = false
[theme]
name = "one-light"
auto_switch = false
[ui]
prompt_new_tab_name = false
[ui.toast]
delivery = "off"
[usage]
enabled = true
EOF
echo "PROMPT='%F{blue}%~%f \$ '" >"$work/zsh/.zshrc"

env -i HOME="$HOME" PATH="$PATH" USER="$USER" LANG=en_US.UTF-8 TERM=xterm-256color \
  SHELL=/bin/zsh ZDOTDIR="$work/zsh" \
  HERDR_SOCKET_PATH="$socket" XDG_CONFIG_HOME="$work/config" XDG_STATE_HOME="$work/state" \
  "$herdr" server >"$work/server.log" 2>&1 &
server_pid=$!

h() {
  env -u HERDR_ENV -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
    -u HERDR_CLIENT_SOCKET_PATH HERDR_SOCKET_PATH="$socket" "$herdr" "$@" >/dev/null
}

for _ in $(seq 50); do
  [[ -S "$socket" ]] && break
  sleep 0.1
done
[[ -S "$socket" ]] || { cat "$work/server.log" >&2; exit 1; }

# A fresh server has no workspaces, so the first one created is w1.
h workspace create --cwd "$repo_dir" --label herdr --focus
h workspace create --cwd "$(dirname "$repo_dir")" --label website --no-focus
h workspace create --cwd /tmp --label notes --no-focus
h tab rename w1:t1 code
h tab create --workspace w1 --label agent --no-focus
h tab create --workspace w1 --label logs --no-focus
h pane run w1:p1 "clear; git log --oneline -8"
h pane run w1:p2 "clear; printf '\\e]0;Fix the login bug\\a'; printf '\\n  > fix the login bug in src/auth\\n\\n  * Reading src/auth/login.rs\\n  * Updating session check in login()\\n  * Running cargo test auth\\n'"
h pane run w1:p3 "clear; tail -n 5 /etc/hosts"
h pane report-agent --source demo --agent claude --state working w1:p2
sleep 1

uv run --quiet --with pyte --with pillow python "$script_dir/record.py" \
  --herdr "$herdr" --socket "$socket" --out-dir "$work/frames" --agent-pane w1:p2 \
  --agent-tab w1:t2 --workspace w1

# Nearest-neighbor 2x keeps the rendered cells crisp.
ffmpeg -y -loglevel error -f concat -safe 0 -i "$work/frames/frames.txt" \
  -vf "scale=iw*2:ih*2:flags=neighbor,fps=30,format=yuv420p" \
  -c:v libx264 -crf 18 -movflags +faststart "$output"
echo "wrote $output"
