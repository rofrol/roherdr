#!/usr/bin/env bash
# A trial of TODO coordinators: does a Claude Code coordinator, asked "co się
# dzieje?" ("what is happening?") mid-work, answer and go on with the next
# item in the same turn, or end its turn waiting for a go-ahead?
#
#   scripts/coordinator_trial.sh [--runs N] [--first K] [--minutes M] [--results FILE]
#
# Each run creates a throwaway git repository under $TMPDIR with three trivial
# TODO items, opens a herdr space for it whose tab has the `coordinator` role
# (so the Stop hook of herdr's Claude integration applies), starts Claude
# there, answers its folder-trust prompt for that directory, and sends "Rób
# TODO po kolei.". The coordinator starts real workers as its rule says. When
# the first worker agent appears, the harness sends "co się dzieje?". A run
# ends when all three items are committed on the repository's master, or
# after M minutes (default 15). Then scripts/coordinator_turn_audit.py labels
# the coordinator's turn ends, and the Stop hook's log tells how often it
# blocked a stop. One JSON line per run goes to FILE (default
# $TMPDIR/coordinator-trial-results.jsonl). Every space, worktree and the
# throwaway repository are removed after each run.
#
# Costs Claude usage: a coordinator and up to three workers per run.
set -euo pipefail

runs=5 first=1 minutes=15
results=${TMPDIR:-/tmp}/coordinator-trial-results.jsonl
while [ $# -gt 0 ]; do
	case $1 in
	--runs) runs=$2; shift 2 ;;
	--first) first=$2; shift 2 ;;
	--minutes) minutes=$2; shift 2 ;;
	--results) results=$2; shift 2 ;;
	*) echo "coordinator_trial: unknown argument $1" >&2; exit 2 ;;
	esac
done
[ "${HERDR_ENV:-}" = 1 ] || { echo "coordinator_trial: not inside herdr" >&2; exit 1; }

here=$(cd "$(dirname "$0")" && pwd)
audit=$here/coordinator_turn_audit.py
hook_log=${XDG_STATE_HOME:-$HOME/.local/state}/herdr/awaiting-reply-stop.jsonl
stamp=$(date +%Y%m%d-%H%M%S)

log() { printf '%s %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# Wait until a command succeeds, checking every 3 s, for at most $1 seconds:
# a wait on an observable condition (a commit, an agent appearing), not on a
# duration.
wait_until() {
	local limit=$1; shift
	local deadline=$((SECONDS + limit))
	until "$@"; do
		[ "$SECONDS" -lt "$deadline" ] || return 1
		sleep 3
	done
}

make_repo() {
	local repo=$1
	mkdir -p "$repo"
	git -C "$repo" init -q -b master
	cat >"$repo/AGENTS.md" <<'EOF'
# Trial repository

A throwaway repository for a trial of TODO coordinators. It has no remote:
never push. Agents may commit here without aligning messages first.

- Workers: one worktree per item, `herdr worktree create --cwd "$PWD"
  --branch w/<slug> --base master --label "w: <what>" --no-focus`, then
  `herdr agent start w-<slug> --kind claude --pane <pane>`.
- Checks: `git diff --check HEAD~1` and `cat notes.txt`.
- Commit subjects: `notes: <what>`.
EOF
	cat >"$repo/TODO.md" <<'EOF'
# TODO

## Next, in order

- [ ] Add a line `alpha` at the end of `notes.txt` (create the file).
- [ ] Add a line `beta` at the end of `notes.txt`.
- [ ] Add a line `gamma` at the end of `notes.txt`.

## Needs a decision

## Proposed
EOF
	git -C "$repo" add AGENTS.md TODO.md
	git -C "$repo" -c user.name=trial -c user.email=trial@example.invalid \
		commit -q -m "trial: three items"
}

items_done() {
	local repo=$1 n=0 word
	for word in alpha beta gamma; do
		git -C "$repo" show master:notes.txt 2>/dev/null | grep -qx "$word" && n=$((n + 1))
	done
	echo "$n"
}

all_done() { [ "$(items_done "$1")" = 3 ]; }

# Spaces of this run: the coordinator's and every one created from its tab or
# for a worktree of its repository.
run_workspaces() {
	local ws=$1 tab=$2 name=$3
	herdr workspace list | jq -r --arg w "$ws" --arg t "$tab" --arg n "$name" '
		.result.workspaces[] | select(.workspace_id == $w
			or (.worktree.creator_tab_id // "") == $t
			or ((.worktree.repo_root // "") | endswith("/" + $n))
			or ((.worktree.checkout_path // "") | contains("/" + $n + "/")))
		| .workspace_id'
}

worker_count() {
	local ws=$1 tab=$2 name=$3 pane=$4 spaces
	spaces=$(run_workspaces "$ws" "$tab" "$name" | jq -R . | jq -s .)
	herdr agent list | jq --argjson s "$spaces" --arg p "$pane" \
		'[.result.agents[] | select(.pane_id != $p and (.workspace_id as $w | $s | index($w)))] | length'
}

has_worker() { [ "$(worker_count "$@")" -gt 0 ]; }

transcript_of() {
	local pane=$1 id
	id=$(herdr agent list | jq -r --arg p "$pane" \
		'.result.agents[] | select(.pane_id == $p) | .agent_session.value // empty' | head -1)
	[ -n "$id" ] || return 1
	ls "$HOME"/.claude/projects/*/"$id".jsonl 2>/dev/null | head -1
}

question_seen() {
	local pane=$1 file
	file=$(transcript_of "$pane") && grep -q 'co się dzieje' "$file" && return 0
	herdr agent read "$pane" --source detection --format text 2>/dev/null | grep -q 'co się dzieje'
}

cleanup() {
	local ws=$1 tab=$2 name=$3 repo=$4 dir=$5 id path
	for id in $(run_workspaces "$ws" "$tab" "$name"); do
		herdr workspace close "$id" >/dev/null 2>&1 || log "could not close space $id"
	done
	if [ -d "$repo/.git" ]; then
		git -C "$repo" worktree list --porcelain | sed -n 's/^worktree //p' | while read -r path; do
			[ "$path" = "$(cd "$repo" && pwd -P)" ] || [ "$path" = "$repo" ] && continue
			git -C "$repo" worktree remove --force "$path" >/dev/null 2>&1 || log "could not remove worktree $path"
		done
	fi
	rm -rf "$dir"
	# herdr's default worktree root for this repository, named after it.
	[ -n "$name" ] && rm -rf "$HOME/.herdr/worktrees/$name"
}

one_run() {
	local n=$1 name dir repo created ws tab pane started transcript sent landed reason ndone limit session audit_json blocks workers
	name=cotrial-$stamp-r$n
	dir=$(mktemp -d "${TMPDIR:-/tmp}/$name.XXXX")
	repo=$dir/$name
	make_repo "$repo"
	repo=$(cd "$repo" && pwd -P)

	created=$(herdr workspace create --cwd "$repo" --label "trial: coordinator $n" --no-focus)
	ws=$(jq -r '.result.workspace.workspace_id' <<<"$created")
	tab=$(jq -r '.result.tab.tab_id' <<<"$created")
	pane=$(jq -r '.result.root_pane.pane_id' <<<"$created")
	trap 'cleanup "$ws" "$tab" "$name" "$repo" "$dir"' EXIT
	herdr tab role "$tab" coordinator >/dev/null
	log "run $n: space $ws, pane $pane, repo $repo"

	# Claude asks to trust the new directory: the harness created it, so it
	# answers "Yes, I trust this folder" (the second option; "No, exit" is
	# preselected).
	if ! herdr agent start "trial-coord-$n" --kind claude --pane "$pane" >/dev/null 2>&1; then
		herdr agent send-keys "$pane" down enter >/dev/null
		herdr-job wait-agent "$pane" --until idle >/dev/null
	fi

	started=$SECONDS
	landed=
	for _ in 1 2; do
		herdr agent prompt "$pane" "Rób TODO po kolei." >/dev/null 2>&1 || continue
		if herdr agent wait "$pane" --until working --timeout 15000 >/dev/null 2>&1; then
			landed=1
			break
		fi
	done
	if [ -z "$landed" ]; then
		reason="order not taken"
	else
		limit=$((minutes * 60))
		sent=no
		if wait_until "$limit" has_worker "$ws" "$tab" "$name" "$pane"; then
			log "run $n: first worker started after $((SECONDS - started)) s; asking"
			for _ in 1 2; do
				herdr agent prompt "$pane" "co się dzieje?" >/dev/null 2>&1 || continue
				if wait_until 15 question_seen "$pane"; then
					sent=yes
					break
				fi
			done
		fi
		if wait_until $((limit - (SECONDS - started))) all_done "$repo"; then
			reason="all committed"
		else
			reason="timeout"
		fi
	fi
	ndone=$(items_done "$repo")
	workers=$(git -C "$repo" worktree list | wc -l | tr -d ' ')
	transcript=$(transcript_of "$pane" || true)
	session=$(basename "${transcript:-none}" .jsonl)
	log "run $n: $reason, $ndone/3 items, $((SECONDS - started)) s"

	cleanup "$ws" "$tab" "$name" "$repo" "$dir"
	trap - EXIT

	if [ -n "$transcript" ] && [ -f "$transcript" ]; then
		audit_json=$(python3 "$audit" "$transcript" --json)
	else
		audit_json='{}'
	fi
	blocks=$(jq -s --arg s "$session" '[.[] | select(.session == $s and .coordinator_blocked)] | length' \
		"$hook_log" 2>/dev/null || echo 0)
	jq -cn --argjson run "$n" --arg reason "$reason" --argjson done "$ndone" \
		--argjson seconds $((SECONDS - started)) --arg asked "${sent:-no}" --arg session "$session" \
		--arg transcript "${transcript:-}" --argjson blocks "$blocks" --argjson worktrees "$workers" \
		--argjson audit "$audit_json" '{
			run: $run, end: $reason, items_done: $done, seconds: $seconds, question_landed: $asked,
			worktrees_left_at_end: ($worktrees - 1), stop_hook_blocks: $blocks,
			turn_ends: ($audit.sessions[0].counts // {}),
			abandoned: ($audit.sessions[0].abandoned // []),
			session: $session, transcript: $transcript}' >>"$results"
	tail -1 "$results"
}

for n in $(seq "$first" $((first + runs - 1))); do
	one_run "$n"
done
