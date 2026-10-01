# local.job

Long work started by an agent (a build in a VM, an export, a test suite) is
invisible when the agent only launches it: the agent shows `idle` and you have
to ask how it is going. `herdr-job` runs the command in its own unfocused tab,
so the output is live one click away, and puts its state in the sidebar of the
pane that started it. The state lives in files, not in the agent, so it works
the same for Claude Code, pi, or any other agent, and for you.

```sh
id=$(herdr-job run --name "Build b17" --why "test the new app set" -- make image)
herdr-job wait "$id"      # follows the log, exits with the command's exit code
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
  success the tab closes itself 10 s later (`--keep` leaves it open); after
  a failure it stays open with the output. Closing the parent tab asks first
  and closes its jobs too. herdr builds without child tabs get a top-level
  tab labelled
  `⧖ Build b17`, then `✓ Build b17` or `✗ Build b17`.
- The tab's last row is a pinned footer: state, name, `--why`, which agent and
  workspace started it, and the job id. Output scrolls above it. It has no
  background (reverse video is a black bar on light themes); the state is
  bold in a palette colour.
- The command gets `HERDR_JOB_ID` and `HERDR_JOB_TTY`, the tab's terminal. It
  runs without a controlling terminal, so `/dev/tty` fails; writing progress
  to `$HERDR_JOB_TTY` shows it in the tab but keeps it out of the log that
  `wait` streams (see [Skills](#skills-a-script-in-its-own-job-tab)).
- The starting pane gets a `$jobs` token with counts of its jobs that still
  have a tab, e.g. `2⧖ 1✗ 1✓`: running, failed (or lost) and successful.
  Closing a job tab drops it from the counts (a `tab.closed` hook recounts),
  so a success shows until its tab closes itself and a failure until you
  close its tab. Counts, not names: the agents panel cannot scroll, so a
  list would push other agents out of view. Names and exit codes are in
  `herdr-job list` and the tab labels.
- The space's row in the sidebar counts its running and failed tabs, e.g.
  `○ repo ⧖ 1`: the space's dot is its agents' state, so it stays idle while
  the agent waits for a job (the `tab_jobs` token).
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

## Setup

```sh
herdr plugin link ~/personal_projects/herdr/plugins/job   # or: herdr plugin install rofrol/herdr/plugins/job
ln -s ~/personal_projects/herdr/plugins/job/herdr-job ~/.local/bin/
ln -s ~/personal_projects/herdr/plugins/job/herdr-bg-badge ~/.local/bin/
```

Show the tokens in the sidebar (`~/.config/herdr/config.toml`):

```toml
[ui.sidebar.agents]
rows = [
  ["state_icon", "machine", "workspace", "tab"],
  ["agent", "$jobs", "$bg"],
]
```

The tokens sit next to the agent name, so jobs never add a row.

then apply it to the running server with `herdr server reload-config`.

## Tell your agents to use it

Agents only use `herdr-job` if their instructions say so. Add this to the
global instruction file of each agent, so it applies in every project:

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
```

Running sessions read the file at start, so restart them or send them the
text. A project whose long work runs somewhere special (a VM console, a
remote builder) should keep a script that runs a command there and blocks
until it finishes, so agents can wrap it: `herdr-job run -- ./vm-run make`.

The `Jobs` popup lists all jobs: open it from the plugin pane list, or bind a
key to `herdr-job list` with `[[keys.command]] type = "popup"`.
