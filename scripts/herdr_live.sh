#!/usr/bin/env bash
# Install this checkout's release build into the running Herdr session right
# away, or roll back to the build installed before it.
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: herdr_live.sh install|rollback|list

  install   back up the installed binary, install this checkout's
            target/release/herdr over it and hand the running server off to it
  rollback  restore the most recent backup and hand the server off to it;
            repeat to go further back
  list      show the installed build and the backups, newest first

The handoff keeps every pane running but disconnects attached clients: run
`herdr` again to reattach. From a plain terminal the script reattaches itself.

The installed binary is $HERDR_INSTALLED (default ~/.cargo/bin/herdr).
Backups live in ~/.cache/herdr/installed/ (the last 5 are kept), named
<install time>_<commit>_<commit subject> after the build they hold.
USAGE
}

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
  "$1" server live-handoff --import-exe "$installed"
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
    if cmp -s "$staged" "$installed"; then
      echo "$installed is already this build ($(describe "$installed"))"
      exit 0
    fi
    mkdir -p "$backups"
    backup="$backups/$(date +%Y%m%d-%H%M%S)_$(describe "$installed")"
    cp -p "$installed" "$backup"
    replace_installed "$staged"
    if ! handoff "$backup"; then
      replace_installed "$backup"
      rm -f "$backup"
      echo "handoff failed; restored the previous binary, the server still runs it" >&2
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
    # Keep the replaced build until the handoff succeeds, to restore it
    # otherwise: the server still runs it then.
    current="$(mktemp "$HOME/.cache/herdr/replaced.XXXXXX")"
    trap 'rm -rf "$lock"; rm -f "$current"' EXIT
    cp -p "$installed" "$current"
    replace_installed "$latest"
    if ! handoff "$latest"; then
      replace_installed "$current"
      echo "handoff failed; kept $(describe "$current")" >&2
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
