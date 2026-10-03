#!/bin/sh
# Runs ci.yml of the fork on the current master. Push and pull_request events do
# not start workflows on this fork, so this pushes a throwaway branch whose ci.yml
# also has `workflow_dispatch:` and dispatches it. Usage: fork_ci_dispatch.sh [name]
# Prints the run id; follow it with `gh run watch <id> -R rofrol/roherdr`. When the run
# is done, delete the throwaway branch: `git push origin --delete ci-dispatch-<name>`.
set -e
cd "$(dirname "$0")/.."
N="${1:-$(date +%m%d-%H%M)}"
B="ci-dispatch-$N"
W="$(cd .. && pwd)/herdr-worktrees/$B"
git worktree prune
rm -rf "$W"
git worktree add -q -B "$B" "$W" master
(
  cd "$W"
  python3 - <<'PY'
p = ".github/workflows/ci.yml"
s = open(p).read()
s = s.replace("on:\n  pull_request:", "on:\n  workflow_dispatch:\n  pull_request:", 1)
open(p, "w").write(s)
PY
  git commit -qam "ci: dispatch" && git push -q origin "$B:refs/heads/$B"
)
gh workflow run ci.yml -R rofrol/roherdr --ref "$B"
sleep 15
gh run list -R rofrol/roherdr --workflow ci.yml --limit 1 --json databaseId --jq '.[0].databaseId'
git worktree remove --force "$W"
