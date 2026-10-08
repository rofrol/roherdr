#!/usr/bin/env bash
# Install this checkout's release build into the running Herdr session right
# away, or roll back to the build installed before it.
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: herdr_live.sh install [--force] [--expect-build ID] | rollback [--force] | list

  install   back up the installed binary, install this checkout's
            target/release/herdr over it and hand the running server off to it
  rollback  restore the most recent backup and hand the server off to it;
            repeat to go further back
  list      show the installed build and the backups, newest first

The handoff keeps every pane running but disconnects attached clients: run
`herdr` again to reattach. From a plain terminal the script reattaches itself.

Headless workers do not survive a handoff, and the server refuses one
while a worker's process is alive. The script first stops each worker idle
between turns (`herdr worker stop`, then `herdr worker wait --exit`); a
worker in a turn makes it stop without installing. `--force` skips that and
hands off anyway: the server sends the workers SIGTERM (it needs a running
build that knows the flag).

`--expect-build ID` makes install refuse, installing nothing, unless the
first word of the build's `--build-commit` is that identity (`<hash>`
or `<hash>~<tree>`, see build.rs). `herdr-job clean-tree` exports the identity
of the tree it built as HERDR_CLEAN_TREE_BUILD; `just clean-install` passes
it, so the install ships the build that was checked and nothing else.

The installed binary is $HERDR_INSTALLED (default ~/.cargo/bin/herdr).
Backups live in ~/.cache/herdr/installed/ (the last 5 are kept), named
<install time>_<commit>_<commit subject> after the build they hold.
USAGE
}

force=()
expect_build=""
args=("${@:2}")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[i]}" in
    --force) force=(--force) ;;
    --expect-build)
      expect_build="${args[i + 1]:-}"
      [[ -n "$expect_build" && "${1:-}" == install ]] || { usage >&2; exit 2; }
      i=$((i + 1))
      ;;
    *) usage >&2; exit 2 ;;
  esac
done

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
candidate="$repo/target/release/herdr"
installed="${HERDR_INSTALLED:-$HOME/.cargo/bin/herdr}"
backups="$HOME/.cache/herdr/installed"
keep_backups=5
lock="$HOME/.cache/herdr/install.lock"

# Several agent sessions may install at once; one install or rollback at a
# time keeps the backup stack in order.
take_lock() {
  mkdir -p "$(dirname "$lock")"
  local holder
  until mkdir "$lock" 2>/dev/null; do
    holder="$(cat "$lock/pid" 2>/dev/null || true)"
    if [[ -n "$holder" ]] && ! kill -0 "$holder" 2>/dev/null; then
      rm -rf "$lock"
      continue
    fi
    echo "waiting for another install or rollback (pid ${holder:-?})" >&2
    sleep 1
  done
  echo $$ >"$lock/pid"
}

# Copy $1 next to the installed binary, then rename it over: a running server
# keeps its binary, and nothing ever sees a half-copied file.
replace_installed() {
  local tmp
  tmp="$(mktemp "$installed.XXXXXX")"
  cp "$1" "$tmp"
  chmod 755 "$tmp"
  mv -f "$tmp" "$installed"
}

# $1 sends the request, so a broken new binary can still be rolled back by
# the previous one. The server keeps running its old binary if the new one
# fails to take over.
handoff() {
  "$1" server live-handoff ${force[@]+"${force[@]}"} --import-exe "$installed"
}

# Stops the workers idle between turns and waits for each one's exit event,
# with the running build $1. The server never waits inside the handoff (its
# main loop would freeze every pane, and a deadline would let a timer decide
# the outcome), so the waiting happens here, on `worker wait --exit`, which
# ends on the worker's exit event. A worker in a turn refuses the install.
end_idle_workers() {
  local listing workers kind id state busy=()
  ((${#force[@]} == 0)) || return 0
  # A build without headless workers: there is nothing to stop.
  listing="$("$1" worker list 2>/dev/null)" || return 0
  workers="$(printf '%s' "$listing" | python3 -c '
import json, sys
for worker in json.load(sys.stdin).get("result", {}).get("workers", []):
    state = worker.get("state")
    ended = state in ("exited", "lost") or worker.get("end_note") \
        or worker.get("exit_code") is not None or worker.get("exit_signal") is not None
    if not ended:
        idle = state in ("finished", "failed", "interrupted")
        print("idle" if idle else "busy", worker["worker_id"], state)
')"
  while read -r kind id state; do
    if [[ "$kind" == busy ]]; then
      busy+=("$id ($state)")
    fi
  done <<<"$workers"
  if ((${#busy[@]} > 0)); then
    echo "headless workers are in a turn: ${busy[*]}; a handoff would end them." >&2
    echo "wait for their turns to end or stop them (herdr worker stop <id>), or install with --force; nothing installed" >&2
    exit 1
  fi
  while read -r kind id state; do
    if [[ "$kind" != idle ]]; then
      continue
    fi
    echo "stopping worker $id ($state, idle between turns) before the handoff"
    "$1" worker stop "$id" >/dev/null
    "$1" worker wait "$id" --exit >/dev/null
  done <<<"$workers"
}

# "<short hash>_<subject slug>" from the commit a binary was built from; a
# build with uncommitted changes is "<short hash>-dirty-<tree>_<label slug>"
# (build.rs). The binary itself says it, since other sessions may move the
# checkout's HEAD between build and install.
describe() {
  local line hash subject
  if ! line="$("$1" --build-commit 2>/dev/null)" || [[ -z "$line" ]]; then
    echo "unknown-$(shasum -a 256 "$1" | cut -c1-8)"
    return
  fi
  hash="${line%% *}"
  hash="${hash/\~/-dirty-}"
  hash="${hash/+/-dirty}"
  subject="${line#* }"
  subject="$(printf '%s' "${subject%% · base: *}" | sed -E 's/^[a-z]+(\([^)]*\))?!?: //' |
    LC_ALL=C tr -c 'a-zA-Z0-9' '-' | tr -s '-' | cut -c1-40 | sed 's/-$//')"
  echo "${hash}_${subject:-no-subject}"
}

newest_backup() {
  find "$backups" -mindepth 1 -maxdepth 1 -type f 2>/dev/null | LC_ALL=C sort | tail -n 1
}

reattach() {
  if [[ -z "${HERDR_PANE_ID:-}" && -t 0 && -t 1 ]]; then
    echo "attaching with $installed"
    exec "$installed"
  fi
  echo "Attached clients were disconnected; run \`herdr\` to reattach."
}

case "${1:-}" in
  install)
    [[ -x "$candidate" ]] || { echo "no build at $candidate; run cargo build --release --locked" >&2; exit 1; }
    [[ -x "$installed" ]] || { echo "no installed herdr at $installed" >&2; exit 1; }
    take_lock
    trap 'rm -rf "$lock"' EXIT
    # Stage a copy first: another session's cargo build may rewrite the
    # candidate while this runs.
    staged="$(mktemp "$HOME/.cache/herdr/staged.XXXXXX")"
    trap 'rm -rf "$lock"; rm -f "$staged"' EXIT
    cp "$candidate" "$staged"
    chmod 755 "$staged"
    "$staged" --version >/dev/null || { echo "the build does not run; nothing installed" >&2; exit 1; }
    if [[ -n "$expect_build" ]]; then
      built="$("$staged" --build-commit 2>/dev/null || true)"
      if [[ "${built%% *}" != "$expect_build" ]]; then
        echo "$candidate is build ${built:-unknown}, not the expected $expect_build" >&2
        echo "another run may have rebuilt it; nothing installed" >&2
        exit 1
      fi
    fi
    if cmp -s "$staged" "$installed"; then
      echo "$installed is already this build ($(describe "$installed"))"
      exit 0
    fi
    end_idle_workers "$installed"
    mkdir -p "$backups"
    backup="$backups/$(date +%Y%m%d-%H%M%S)_$(describe "$installed")"
    cp -p "$installed" "$backup"
    replace_installed "$staged"
    if ! handoff "$backup"; then
      replace_installed "$backup"
      rm -f "$backup"
      echo "handoff failed or refused (see above); restored the previous binary, the server still runs it" >&2
      exit 1
    fi
    find "$backups" -mindepth 1 -maxdepth 1 -type f | LC_ALL=C sort -r |
      tail -n "+$((keep_backups + 1))" | while read -r old; do rm -f "$old"; done
    echo "Installed $(describe "$installed")."
    echo "Backed up the previous build as $(basename "$backup")."
    rm -rf "$lock"
    trap - EXIT
    rm -f "$staged"
    reattach
    ;;
  rollback)
    latest="$(newest_backup)"
    [[ -n "$latest" ]] || { echo "no backups in $backups" >&2; exit 1; }
    take_lock
    trap 'rm -rf "$lock"' EXIT
    latest="$(newest_backup)"
    end_idle_workers "$installed"
    # Keep the replaced build until the handoff succeeds, to restore it
    # otherwise: the server still runs it then.
    current="$(mktemp "$HOME/.cache/herdr/replaced.XXXXXX")"
    trap 'rm -rf "$lock"; rm -f "$current"' EXIT
    cp -p "$installed" "$current"
    replace_installed "$latest"
    if ! handoff "$latest"; then
      replace_installed "$current"
      echo "handoff failed or refused (see above); kept $(describe "$current")" >&2
      exit 1
    fi
    # Popped: the next rollback goes one build further back.
    rm -f "$latest"
    echo "Rolled back from $(describe "$current") to $(describe "$installed")."
    rm -rf "$lock"
    trap - EXIT
    rm -f "$current"
    reattach
    ;;
  list)
    echo "installed: $(describe "$installed")"
    find "$backups" -mindepth 1 -maxdepth 1 -type f 2>/dev/null | LC_ALL=C sort -r |
      while read -r backup; do echo "backup:    $(basename "$backup")"; done || true
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
