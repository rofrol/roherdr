# local.job

Long work started by an agent (a build in a VM, an export, a test suite) is
invisible when the agent only launches it: the agent shows `idle` and you have
to ask how it is going. `herdr-job` runs the command in its own unfocused tab,
so the output is live one click away, and puts its state in the sidebar of the
pane that started it. The state lives in files, not in the agent, so it works
the same for Claude Code, pi, or any other agent, and for you.

```sh
id=$(herdr-job run --name "Build b17" --why "test the new app set" -- make image)
herdr-job wait "$id"      # a start line, then the final line; exits with the command's exit code
herdr-job wait --stream "$id"  # follows the whole log as it grows (the old behaviour)
herdr-job list            # all jobs: running / ok / failed (code) / lost
herdr-job log "$id"       # log path
herdr-job clean           # close this pane's finished job tabs (--all: everyone's)
```

- The job tab is a child of the tab that started it. While the workspace
  has jobs, a second row under the tab bar lists the active tab's own
  content first (named after its agent, e.g. `claude`) and then its jobs,
  e.g. `claude  ⧖ tests  ! build`; the parent tab shows a summary such as
  `1 ⧖ 1 !2 ✓3`. herdr draws the icons from the tab's status
  (`herdr tab status`), set to running, then succeeded or failed; failure
  is `!` because `✗` next to a tab label reads as a close button. After
  success the tab closes itself 10 s later, or once you leave it if it is
  the focused tab then (`--keep` leaves it open); after
  a failure it stays open with the output. Closing the parent tab asks first
  and closes its jobs too. herdr builds without child tabs get a top-level
  tab labelled
  `⧖ Build b17`, then `✓ Build b17` or `✗ Build b17`.
- The tab's last row is a pinned footer: ` ← `, state, name, `--why`, which
  agent and workspace started it, the job id and ` × `. Output scrolls above
  it into terminal scrollback. After the job ends, a shell or full-screen
  application may reset the margins or overwrite the footer; the final tab
  status and printed exit summary remain available.
  With herdr's vertical tabs (`ui.sidebar.spaces.tabs`) there is no tab bar,
  so this row is the job's title: clicking ` ← ` goes back to the tab that started the job and
  ` × ` closes the job tab (a running job asks first). It has no background
  (reverse video is a black bar on light themes); the state is bold in a
  palette colour.
- The command gets `HERDR_JOB_ID` and `HERDR_JOB_TTY`, the tab's terminal. It
  runs without a controlling terminal, so `/dev/tty` fails; writing progress
  to `$HERDR_JOB_TTY` shows it in the tab but keeps it out of the log that
  `wait --stream` shows (see [Skills](#skills-a-script-in-its-own-job-tab)).
- On macOS the job keeps the Mac from idle sleep while it waits for its slot
  and runs (`caffeinate -i -w` on the job's executor), so an unattended build
  goes on with the display off; the agent's own sleep inhibitor is not held
  while it blocks on `herdr-job wait`, or has gaps. Closing the lid still
  sleeps. `HERDR_JOB_KEEP_AWAKE=0` turns it off.
- The starting pane gets a `$jobs` token with counts of its jobs that still
  have a tab, e.g. `⧖ 2 !1 ✓1`: running, failed (or lost) and successful,
  with the same icons as the tab bar.
  Closing a job tab drops it from the counts (a `tab.closed` hook recounts),
  so a success shows until its tab closes itself and a failure until you
  close its tab. Counts, not names: the agents panel cannot scroll, so a
  list would push other agents out of view. Names and exit codes are in
  `herdr-job list` and the tab labels.
- The space's row in the sidebar counts its running and failed tabs, e.g.
  `○ repo ⧖ 1`: the space's dot is its agents' state, so it stays idle while
  the agent waits for a job (the `tab_jobs` token). With
  `ui.sidebar.spaces.tabs` each tab listed under the space shows the
  running and failed counts of the job tabs nested under it, read from the
  tab statuses, not from `$jobs`; the space row keeps only jobs nested under
  a tab that is gone.
- `herdr-bg-badge` (a Claude Code Stop hook) puts the number of Claude's own
  background tasks in `$bg` (`2 bg`), skipping `herdr-job wait` tasks, which
  `$jobs` already counts.
- The job tab's shell gets the `_exec` line typed into it, so it is kept out of
  your history: a leading space for atuin, and `HISTORY_IGNORE` in the tab's
  environment for zsh's history file.
- An agent runs `herdr-job wait <id>` as its background task, so it wakes up
  with the real result instead of "started".
- Closing a job tab stops the job; `wait` then reports `lost` (exit 125) or
  the hangup's exit code (129). A job never runs twice, even if relaunch
  replays its tab after a server restart.
- The command must represent the work: if it only starts something elsewhere
  (a VM, `nohup`, a remote host) and returns, wait for that work inside the
  command (e.g. poll its status file).

State: `~/.local/state/herdr-job/<id>/` (`meta.json`, `log`, `exit`, `lock`).
macOS and Linux only. The state directory must be on a local filesystem:
a job's liveness is an `flock` held by its executor, which network
filesystems may not honour.

## One wait, one job

Wait for something with one wait command, never with a retry loop around
`herdr-job run`: each `run` opens a tab (a PTY) and a failed job's tab stays
open. A coordinator that retried its wait for a worker this way after a herdr
server restart opened about 400 tabs, until macOS ran out of PTYs and herdr
could open no tab anywhere. When a wait fails because herdr did not answer,
check the state once and report it.

To wait for a worker (an agent in another pane), start its prompt with
`herdr agent prompt`, whose result carries `prompt_request.request_id`, and
run one job that waits for that prompt's turn and reconnects inside:

```sh
request=$(herdr agent prompt "$pane" "$task" | jq -r '.result.prompt_request.request_id')
id=$(herdr-job run --name "wait w-docs" --why "review its commit" -- \
  herdr-job wait-agent "$pane" --request "$request")
herdr-job wait "$id"
```

- `wait-agent <pane> --request <id>` blocks on `herdr agent wait-turn`: the
  turn's end is an event (the agent's turn report, its process exiting, the
  pane closing), never a timer, a screen read or a debounce. Then it reads
  the worker's transcript once and prints its verdict from the final
  message: `WORKER-DONE <sha>` when that commit exists in the worker's
  checkout (exit 0), `WORKER-BLOCKED <reason>` (exit 4), `awaiting input`
  when the message asks something (exit 6), or what needs attention: a turn
  that failed, was interrupted or finished without a WORKER line, or a sha
  that is not a commit (exit 7). An agent that exited ends it with exit 3,
  and a request herdr no longer knows (its server restarted) with exit 8.
- `wait-agent <pane> [--until STATE]...` ends when the agent reaches one of
  the states (default: idle, done or blocked), as `herdr agent wait` does.
  A worker's state flickers to done or idle while it works, so use
  `--request` for workers.
- When herdr does not answer (a server restart, `EmptyResponse`, no socket)
  it retries with backoff from 1 s to 30 s and gives up after 15 minutes of
  continuous failures (exit 5). An agent that is gone ends the wait (exit 3),
  not a retry. Another error exits 2.

`run` also refuses, before it opens the job's tab:

- more than 16 job tabs of one owner pane (running jobs and kept failed
  tabs) or 64 in all; a job started from inside a job, or from a job tab,
  counts against the pane that started the first one. At most 8 failed job
  tabs per owner stay open: older ones are closed (never the focused tab);
  their logs stay in the state directory. `herdr-job clean` closes finished
  ones. `HERDR_JOB_MAX_PER_OWNER`, `HERDR_JOB_MAX_GLOBAL` and
  `HERDR_JOB_MAX_FAILED_KEPT` change the limits.
- a `--name` that failed 3 times in the last 10 minutes for the same owner,
  unless `--force`. The counters are on disk
  (`~/.local/state/herdr-job/failures.json`), so a server restart does not
  reset them.

The checks run under a lock file held until the new job is recorded, so
concurrent launches cannot pass a limit together.

## Design and limits

- State lives in files, not in the agent, so Claude, pi and a person share it,
  and it survives the agent's context. A rule in the agent's instructions
  alone was not enough: agents forget it, and a launcher that returns at once
  still looks finished.
- Notifications: a job started by hand notifies when it ends; a job started
  by an agent does not, because the agent reports it when `herdr-job wait`
  wakes it, and two notifications for one event are noise. `--notify`
  overrides both.
- Clicking the notification focuses the job tab, or its workspace once the
  tab is closed (herdr's `notification.show_for_pane`; older herdr builds get
  a notification without a click target). A click only moves focus; it never
  runs a command, so a remote server cannot choose what your machine runs.
  Click actions work on macOS with `terminal-notifier` and on Linux with
  `notify-send` from libnotify >= 0.7.10 (see below).
- A job is alive while its executor holds an `flock`, not while its PID
  exists: after a crash or reboot the PID can belong to another process and
  `wait` would hang forever.
- The final tab status and the closing of a succeeded job's tab are retried
  with backoff (about a minute each): during a live handoff the server may
  not answer. Whatever still slips through is repaired by `run`, `wait`,
  `list`, `clean` and the `tab.closed` hook: a tab still marked running whose
  job has ended gets succeeded or failed (a lost job is failed, never
  succeeded), and a succeeded job's tab closes 10 s after the job ended
  unless `--keep`. Tab ids can be reused, so only the newest job of a tab
  decides, and only while the tab still has that job's label; `clean` does
  not close a tab the user renamed either.
- macOS and Linux only (`flock`, `/bin/sh`, POSIX signals). State files are
  private (0600): commands and logs may contain secrets.

## Clickable notifications on Linux

The client shows notifications, so it is on the desktop's session bus even
when the server runs elsewhere. For a notification with a click target it runs
`notify-send --action=default=Open --wait` on a background thread; when that
prints `default`, it runs the same focus commands as macOS (agent → tab →
workspace), never a command chosen by the server. An older `notify-send` that
rejects `--action` gets a plain notification instead. At most 16 notifications
wait for a click at once (GNOME keeps unread ones in its tray); later ones are
shown without a click target. Only the foreground client gets a notification,
so several attached clients never act on one click.

System notifications need `[ui.toast] delivery = "system"` in herdr's config
(a debug build reads `~/.config/herdr-dev/`, not `~/.config/herdr/`).

Checked:

| Desktop | Daemon | `notify-send` | Click → focus | Date |
|---|---|---|---|---|
| Fedora 44 Workstation, aarch64 VM (QEMU) | GNOME Shell 50.0, spec 1.2 | 0.8.8 | yes, switched to the pane's workspace | 2026-09-26 |

To check another desktop: in a herdr pane run
`herdr notification show T --pane "$HERDR_PANE_ID"` after a short `sleep`,
switch to another herdr workspace, click the notification; then with a closed
tab (`herdr-job run --notify always --name T -- true`, workspace fallback).
On a Mac, `scripts/linux_vm/gnome_vm.sh` builds a small GNOME VM for this.

**Later, research: bring the terminal window forward.** Wayland blocks focus
stealing, so switching herdr's tab may leave the terminal behind another
window or on another desktop workspace (macOS gets this from
`terminal-notifier -activate`). GNOME sends an `ActivationToken` just before
`ActionInvoked`, but `notify-send` does not print it, so raising the window
needs the D-Bus route (the existing `zbus` dependency); every desktop differs
(`ActivationToken` handling, compositor commands such as `hyprctl`).

## Skills: a script in its own job tab

A skill's script can re-run itself as a job, so you watch it in a tab while
the agent that called it sees nothing different: the same stdout, stderr and
exit code, and no desktop notification (the agent reports the result).

```sh
# at the top of the script; the variable stops the recursion
if [ -z "${IN_JOB:-}" ] && [ -n "${HERDR_SOCKET_PATH:-}" ] && command -v herdr-job >/dev/null; then
  id=$(herdr-job run --name "ask gpt" --why "$1" --notify never --cwd "$PWD" -- \
    env IN_JOB=1 "$0" "$@" ...)   # save stdout/stderr to files in the job
  herdr-job wait --quiet "$id"; ...  # then print them and exit with its code
fi
```

The job runs in another pane, so it cannot read the caller's stdin: save it
to a file first. Progress meant only for you goes to `$HERDR_JOB_TTY`.
The consult skills (`ask_gpt.sh`, `ask_gemini.sh`, `ask_deepseek.py`) do this
through `~/.claude/skills/consult-stats/in_herdr_job.sh` (in rofrol/dotfiles).

## Claude background tasks: `herdr-bg-badge`

A Claude Code `Stop`/`SubagentStop` hook that shows Claude's own background
tasks (from the hook payload's `background_tasks`) as a `$bg` token:
`⏳ <description>` for one task, `⏳ N bg` for several. Payloads are logged to
`~/.local/state/herdr-bg/payloads.jsonl`.

```json
"Stop": [{"hooks": [{"type": "command", "command": "~/.local/bin/herdr-bg-badge", "timeout": 10}]}],
"SubagentStop": [{"hooks": [{"type": "command", "command": "~/.local/bin/herdr-bg-badge", "timeout": 10}]}]
```

## Claude session peer name: `herdr-peer-token`

A Claude Code `SessionStart`/`Stop` hook that reports the name other Claude
sessions use to message this one (`herdr-ef`, from
`~/.claude/sessions/<pid>.json`) as the `$peer` token, so you can tell which
pane a cross-session message came from. `Stop` refreshes it and picks up a
later `/rename`.

```json
"SessionStart": [{"hooks": [{"type": "command", "command": "~/.local/bin/herdr-peer-token", "timeout": 10}]}],
"Stop": [{"hooks": [{"type": "command", "command": "~/.local/bin/herdr-peer-token", "timeout": 10}]}]
```

## Setup

```sh
herdr plugin install rofrol/herdr/plugins/job   # or from a checkout: herdr plugin link plugins/job
```

Then open the plugin's "Jobs: set up for agents" popup from herdr's plugin
pane list, or run `./install` in the plugin's directory (`herdr plugin list`
shows it). `install` links `herdr-job`, `herdr-bg-badge` and `herdr-peer-token` into
`~/.local/bin` (`--bin DIR` elsewhere), and asks per agent whether to add the
instructions below to its global instruction file (see
[Tell your agents to use it](#tell-your-agents-to-use-it)). Re-run it after an
update: it changes only what is missing or older.

Show the tokens in the sidebar (`~/.config/herdr/config.toml`):

```toml
[ui.sidebar.agents]
rows = [
  ["state_icon", "machine", "workspace", "tab"],
  ["agent", "$peer", "$jobs", "$bg"],
]
```

The tokens sit next to the agent name, so jobs never add a row.

then apply it to the running server with `herdr server reload-config`.

## Tell your agents to use it

Agents only use `herdr-job` if their instructions say so. `install` adds
this block (from `agent-instructions.md`, between markers so a re-run can
update it) to the global instruction file of each agent you have, after
asking; or paste it yourself:

| Agent | Global instructions |
|---|---|
| Claude Code | `~/.claude/CLAUDE.md` |
| pi | `~/.pi/agent/AGENTS.md` (or `CLAUDE.md` there) |
| Codex | `~/.codex/AGENTS.md` |

```markdown
# Long-running work

Run work that takes more than a minute (builds, exports, VM or remote jobs,
long test suites) with
`herdr-job run --name "<short description>" --why "<what it is for>" -- <command>`,
then wait for it in the background with `herdr-job wait <id>`. The command must
block until the work is really done: if it only starts work elsewhere (a VM,
a remote host, a detached process), make it wait for that work, e.g. by polling
its status file. Do not detach it with `nohup` or `&`.

The same goes for any wait your next step depends on, however short, and for
processes you did not start (a scheduled run, another session's build): wait
for one with `herdr-job watch --pid <pid> --why "<what you do after it>"`, then
`herdr-job wait <id>`, not with a loop in a background shell of your own. A job
shows in the user's sidebar; your own background shell does not. `watch` only
sees the process end, not its exit status: check its result yourself.

When `git status` shows changes that are not yours (another session works in
the same checkout), build and test your change in a clean tree:
`herdr-job clean-tree <your paths> -- <build or test command>` runs it on
`HEAD` plus only those paths.
```

Running sessions read the file at start, so restart them or send them the
text. A project whose long work runs somewhere special (a VM console, a
remote builder) should keep a script that runs a command there and blocks
until it finishes, so agents can wrap it: `herdr-job run -- ./vm-run make`.

The `Jobs` popup lists all jobs: open it from the plugin pane list, or bind a
key to `herdr-job list` with `[[keys.command]] type = "popup"`.

## Slots

Several agent sessions on one machine each start builds and test suites, and
cargo and nextest each use every core, so two at once are slower than one
after the other and skew benchmarks. A slot is admission for such work:

```sh
herdr-job run --slot --name "release build" -- cargo build --release --locked
herdr-job slot -- cargo nextest run       # in the foreground, for scripts and just recipes
herdr-job slot --exclusive -- ./bench.sh  # every slot: nothing else heavy runs meanwhile
herdr-job slots                           # who holds them: job, name, pid, since when
```

- One slot by default; `HERDR_JOB_SLOTS=N` sets the number, `0` turns slots
  off (a CI runner). A slot is an `flock` on a file in
  `~/.local/state/herdr-job/slots/`, so a holder that dies frees it.
- Opt-in, never the default for a job: network-bound jobs such as the
  consult scripts must not wait behind a build. In this repository the
  `just` recipes that build or test take a slot themselves (the benchmarks an
  exclusive one), so agents need no flag for them.
- A job with `--slot` waits in its own tab: the tab says who it waits for and
  shows as idle until it gets the slot.
- Inside a slot (`HERDR_JOB_SLOT` is set), a shared request takes nothing, so
  `just check` under `herdr-job run --slot` does not wait for itself, and a
  `herdr-job run --slot` started from inside a slot shares its parent's
  instead of waiting for it in a new tab. An
  exclusive request inside a shared slot fails at once instead of waiting
  forever for its own ancestor. Background children inherit the mark too,
  so do not start detached heavy work from inside a slot.
- Cargo's lock on `target/` is not enough: nextest runs the tests after
  cargo has released it, and worktrees have their own `target/`.
- Slots only order work that asks for them, and a hung holder keeps its slot
  until it is stopped: `herdr-job slots` names it. An exclusive request does
  not stop newcomers from taking slots it has not reached yet; with one slot
  that cannot happen.

## Clean tree

Several agent sessions often edit one shared checkout. A build or test run
there compiles whatever another session left half done, so it can fail on
their code or pass on code that will not be committed. `clean-tree` runs a
command in a clean copy instead: the checkout's `HEAD` plus only the paths you
name.

```sh
herdr-job clean-tree src/foo.rs tests/foo.rs -- cargo test   # HEAD + these paths' changes
herdr-job clean-tree -- make check                           # HEAD as committed
herdr-job run --slot --name "check foo" -- herdr-job clean-tree src/foo.rs -- just check
herdr-job clean-tree --then 'make install' -- make check    # then this, under the same lock
herdr-job clean-tree --path                                  # where the tree is
```

- One persistent worktree per repository, `<repo>-worktrees/clean-check`
  next to the checkout (or `$HERDR_CLEAN_TREE`). Each run resets it to
  `HEAD` and removes untracked files, but keeps ignored build output such as
  `target/` or `node_modules/` warm: only the first run builds from cold.
- Name your own paths: the shared checkout cannot tell whose edits are
  whose. Edits and deletions come as a patch against `HEAD`, new files as
  copies. A file another session also edits brings their hunks too: check
  `git diff -- <path>` first.
- `HEAD` and the named paths are read once, into a snapshot (the commit and
  a tree object written from a throwaway index), and the tree is built from
  that: a commit another session makes meanwhile does not mix in.
- One run at a time per repository (an `flock` under `.git/`); another run
  waits and says for whom. The command gets `HERDR_CLEAN_TREE_BASE`, the
  commit it was built on, and `HERDR_CLEAN_TREE_BUILD`, the identity a herdr
  build of that tree prints first in `--build-commit` (`<hash>` or
  `<hash>~<tree>`, as build.rs computes it).
- `--then SHELL-COMMAND` runs after the command succeeds (not after a
  failure), with `sh -c` in the clean tree and under the same lock. Use it
  for a step that needs the command's output, such as installing the build:
  a separate command after the run could find the tree reset and rebuilt by
  another run (`just clean-install` in herdr).
- Worth it where several sessions edit and build one checkout; a checkout
  only you edit needs none.

## Idle jobs

While a job runs, `herdr-job` samples the CPU time of its process tree every 15 seconds and
watches its output. After 5 minutes with no output and under 2% of one core, it reports the job as
idle (`herdr tab status <tab> running --activity idle`); the sidebar then shows the agent that
started it with a still mauve `z` ("asleep") instead of the purple turning half circle, and the job's footer
says `(idle)`. Output or CPU use clears it. A job can be idle and healthy (a VM waiting for a
build); the mark says only that nothing is happening.
