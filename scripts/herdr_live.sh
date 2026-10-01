#!/usr/bin/env bash
# Try this checkout's release build in the running Herdr session, then install
# it or go back. The installed binary is the last known-good build.
set -euo pipefail

usage() {
  cat <<'USAGE'
usage: herdr_live.sh test|keep|back

  test  hand the session off to this checkout's target/release/herdr and
        record its hash; this terminal then attaches with it
  keep  install the tested build over the installed binary (refuses if the
        build changed since `test`) and hand the session off to it
  back  hand the session back to the installed binary

Run it in a plain terminal, not in a Herdr pane: the handoff closes every
attached client, including the one showing that pane.

The installed binary is $HERDR_INSTALLED (default ~/.cargo/bin/herdr).
USAGE
}

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
candidate="$repo/target/release/herdr"
installed="${HERDR_INSTALLED:-$HOME/.cargo/bin/herdr}"
tested="$HOME/.cache/herdr/tested"

# Clients (not servers) whose executable is not $1. The client draws the tab
# bar and the rest of the UI, so a client left on another binary shows the old
# UI even when the server runs the new one.
stale_clients() {
  local pid args exe
  while read -r pid args; do
    [[ "$args" == *" server"* ]] && continue
    exe="$(lsof -a -p "$pid" -d txt -Fn 2>/dev/null | sed -n 's/^n//p' | head -n 1)"
    if [[ -n "$exe" && "$exe" != "$1" ]]; then
      echo "  pid $pid $exe"
    fi
  done < <(ps -axo pid=,comm=,args= | awk '$2 ~ /(^|\/)herdr$/ { $2 = ""; print }')
}

handoff() {
  "$installed" server live-handoff --import-exe "$1"
  local stale
  stale="$(stale_clients "$1")"
  if [[ -z "${HERDR_PANE_ID:-}" && -t 0 && -t 1 ]]; then
    # Outside Herdr: this terminal becomes a client of the new binary.
    echo "attaching with $2"
    exec "$2"
  fi
  echo
  echo "The server now runs $1."
  if [[ -n "$stale" ]]; then
    echo "These clients still run another binary and show the old UI:"
    echo "$stale"
  fi
  echo "Detach (ctrl+b q) and attach with:"
  echo "  $2"
}

fingerprint() {
  printf '%s %s\n' "$(shasum -a 256 "$candidate" | cut -d' ' -f1)" "$candidate"
}

# The handoff closes every attached client, including the one showing this
# pane, so a message printed here is never seen. From a plain terminal the
# script attaches with the new binary itself.
if [[ -n "${HERDR_PANE_ID:-}" && "${1:-}" =~ ^(test|keep|back)$ ]]; then
  echo "run this outside Herdr, in a plain terminal: the handoff closes the client showing this pane" >&2
  exit 1
fi

case "${1:-}" in
  test)
    [[ -x "$candidate" ]] || { echo "no build at $candidate; run cargo build --release --locked" >&2; exit 1; }
    mkdir -p "$(dirname "$tested")"
    fingerprint >"$tested"
    handoff "$candidate" "$candidate"
    ;;
  keep)
    if [[ "$(fingerprint)" != "$(cat "$tested" 2>/dev/null)" ]]; then
      echo "$candidate is not the build last tried with \`test\`" >&2
      exit 1
    fi
    # A new file renamed over the old one: a running server keeps its binary,
    # and nothing ever sees a half-copied file.
    tmp="$(mktemp "$installed.XXXXXX")"
    trap 'rm -f "$tmp"' EXIT
    cp "$candidate" "$tmp"
    chmod 755 "$tmp"
    mv -f "$tmp" "$installed"
    handoff "$installed" herdr
    ;;
  back)
    handoff "$installed" herdr
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
