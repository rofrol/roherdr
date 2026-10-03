# roherdr

**roherdr is an unofficial fork of [herdr](https://github.com/herdrdev/herdr)**,
the terminal-based runtime for coding agents. It is not affiliated with the
herdr project. The binary, the commands, the configuration directory and the
`HERDR_*` variables keep the name `herdr`, so herdr's plugins, skills and agent
integrations work unchanged. herdr's licence (Apache-2.0) and notices apply.

## Fork changes

These commits sit on top of upstream `master` and are not meant for upstream
PRs.

- **Usage widget.** A footer at the bottom of the sidebar shows how much of
  your coding-agent allowance is used: Anthropic/Claude (`AN`) and
  OpenAI/Codex (`OA`) 5-hour and weekly limits with time to reset, the
  Google/Gemini (`GO`) weekly limit, and the DeepSeek (`DS`) and OpenRouter
  (`OR`) prepaid balances. Click it for details: reset clock times, plans,
  free Codex limit resets, and a refresh button (`r`). It is on by default; turn it off with:

  ```toml
  [usage]
  enabled = false
  ```

  Claude uses the Claude Code login, Codex goes through `codex app-server`,
  Gemini runs the Antigravity CLI's `agy -p /quota` (the details also show
  Antigravity's separate Claude/GPT weekly group), DeepSeek uses its balance
  API, OpenRouter its key and credits API (balance plus daily/weekly/monthly
  key spend). DeepSeek and OpenRouter keys come
  from `DEEPSEEK_API_KEY`/`OPENROUTER_API_KEY`, then `auth_file` (default
  `~/.pi/agent/auth.json`, the pi coding agent's logins); OpenRouter is
  hidden without a key. OpenAI API (`OP`, pay-as-you-go) month-to-date
  spend and completion tokens are opt-in (`openai_api = true`): they need an
  OpenAI Admin key with read access to usage and costs, one line in
  `~/.config/herdr/openai-admin-key` (`openai_admin_key_file`), mode 0600;
  no environment variable is read, so agent panes never inherit it. It is
  organization-wide spend, not prepaid credit left, refreshed at most every
  15 minutes. The server refreshes every 5 minutes
  (`refresh_interval_secs`); set `claude`, `codex`, `gemini`, `deepseek` or
  `openrouter` to `false` to hide a provider. All herdr instances share one
  cache (`~/.local/state/herdr/usage-cache.json`): a recent observation is
  reused instead of refetched, and a rate-limited provider backs off
  (5 min doubling to 1 h) while the footer keeps its last good values.
- **Job tabs from the [job plugin](plugins/job/README.md).** Each
  `herdr-job` job gets a child tab of the tab that started it. While a
  workspace has child tabs, a second row under the tab bar lists the active
  tab's own content and its jobs (`claude  ◐ tests  ! build`), and the parent
  tab shows a summary such as `◐ 1 !2 ✓3`. The space's sidebar row counts
  its running and failed tabs (`○ repo ◐ 1`, the `tab_jobs` token), so
  background work stays visible while the agent is idle, and the agent that
  started the jobs shows their counts (`2⧖ 1✗ 1✓`, the `$jobs` token).
  Closing a parent tab asks first and closes its jobs too. Child tabs and
  statuses are ordinary API (`herdr tab parent`, `herdr tab status`), usable
  by any script.
- **Vertical tabs in the sidebar** (experimental). With
  `spaces.tabs = true`, each space lists its tabs under it, one line per
  top-level tab with its agent state, label and the running and failed
  counts of its nested job tabs; click a line to open that tab. Both tab
  rows above the panes go. Click `►` before a tab's job counts to unfold
  its job tabs as small squares under it (`◐` running, `!` failed, `✓`
  done), and `▼` to fold them; click a square to open that job, click it
  again to go back, middle-click to close it; hovering a square names its
  job on the tab line. A job's first row is its header, with ` ← ` to
  go back and ` × ` to close. A disclosure triangle before the space's name
  collapses its tabs. The spaces list scrolls by rows; `keys.move_space_previous`
  and `keys.move_space_next` move the focused space with the keyboard.
  `show_agents_panel = false` hides the agents panel and gives its height
  to the spaces list:

  ```toml
  [ui.sidebar]
  show_agents_panel = false

  [ui.sidebar.spaces]
  tabs = true
  ```
  Tab lines can be dragged to reorder tabs within their space (also in a
  space that is not focused): the tab is drawn at the slot where it would
  land and the header says which tab moves and where (`2 → 4 · build · before
  review`), Esc or a drop outside the space cancels. Worktree spaces indent
  their tab lines under the worktree's name.
- **Spaces filter.** The `/ filter` button between `new` and `menu` opens a
  bar under the spaces header, like fzf: type to narrow the list to the
  spaces and tabs that match (a smart-case subsequence of a space's name,
  branch or agents and a tab's label, agents or nested jobs). The list keeps
  its order, a matching space shows all its tabs, folded groups open for the
  view, Enter opens the first match and Esc clears, then closes. It is
  client-only and nothing is saved.
- **Animated status glyphs.** A working agent turns `◐ ◓ ◑ ◒` clockwise, and
  a running job (and an agent waiting on one, in mauve) turns the other way,
  twice as slowly; the hourglass is gone. The timer runs only while
  something is drawn that turns. `[ui] animations = false` keeps them still
  (`◐` working, `◑` job).
- **Fork builds ignore upstream updates.** A build from this checkout
  (`HERDR_FORK_BUILD` in `.cargo/config.toml`) starts no check for upstream
  binary releases, ignores a restored "update available" and makes
  `herdr update` refuse; agent-manifest updates still run.
- **Awaiting-reply mark.** An agent whose turn ends with a question for you
  shows `?` in the sidebar and tab bar, so it stands out from agents that
  simply finished. The agent reports it with `herdr agent awaiting-reply`
  (`pane.report_awaiting_reply`); the Claude integration asks Claude to run
  it as its last command when it ends a turn needing your answer or
  decision, never for `AskUserQuestion`, which already shows as blocked,
  and repeats a short reminder with every prompt (`UserPromptSubmit`). The mark stays until
  you type into the pane (keys, text or a paste, or input sent through the
  API); viewing, clicking and scrolling do not clear it. Working hides it
  without clearing it, and a question form or permission prompt after the
  report drops it. The API exposes it as `awaiting_reply`; set
  `HERDR_AWAITING_REPLY_INSTRUCTIONS=0` to leave the instruction out.
- **Consult stats in the herdr menu.** The sidebar's `menu` has an
  **consult stats** item that opens the [consult plugin](plugins/consult/README.md)'s
  stats popup; a click outside closes it. Plugin popups also dim the
  background behind them, like herdr's own dialogs.
- **Middle click closes tabs and workspaces.** Middle-click a tab or a
  workspace in the sidebar to close it, with the same confirmation as the
  context menu's Close. Pane apps with mouse reporting still get middle clicks
  inside the pane.
- **Clickable, richer macOS notifications** (`[ui.toast] delivery = "system"`
  with `terminal-notifier`):
  - clicking an agent notification focuses that agent's pane
    (`herdr agent focus`, falling back to `herdr tab focus`) and activates the
    terminal;
  - the message is the agent's task (its terminal title), the subtitle is the
    workspace and tab;
  - a new notification from the same pane replaces the previous one.
- **terminal-notifier fixes.** Titles starting with `(`, `[`, `{`, `<` or a
  quote no longer crash terminal-notifier, and an empty message is no longer
  rejected; both made herdr fall back to the Script Editor notification.

### Plugins

`plugins/` holds herdr plugins that need no fork code and work with upstream
herdr too. Install one with `herdr plugin install rofrol/herdr/plugins/<name>`,
or link it from a checkout with `herdr plugin link plugins/<name>`.

- [**job**](plugins/job/README.md): `herdr-job run --name "Build" -- make`
  runs a long command in its own unfocused tab (live output one click away)
  and shows `⧖ Build`, then `✓`/`✗ <code>`, in the sidebar of the pane that
  started it; `herdr-job wait <id>` prints a start line, then the final line (a failure adds the log's
  last lines; `--stream` follows the whole log) and exits with the
  command's code. It keeps state in files, so it works for any agent (Claude
  Code, pi, ...) or by hand. Also has `herdr-bg-badge`, a Claude Code hook
  that shows Claude's own background tasks in the sidebar.
- [**relaunch**](plugins/relaunch/README.md): reruns the programs panes were
  running (lazygit, editors, ...) after a server restart or reboot; herdr
  itself brings them back as empty shells. zsh only; after installing, add its
  shell hook with the "Relaunch: install zsh hook" popup (`./install`).
- [**restart**](plugins/restart/README.md): restarts idle Claude and pi
  agents in place after they update and resumes their sessions, keeping the
  flags they were started with.
- [**pi-title**](plugins/pi-title/README.md): a Pi extension that names the
  session after its first prompt, so the sidebar shows the task like it does
  for Claude (`plugins/pi-title/install`, then `/reload` in Pi).
- [**consult**](plugins/consult/README.md): `gpt`, `gemini` and `deepseek`
  skills that let a coding agent ask another model for a second opinion, and
  `consult-stats`, which logs every call and rates which models helped.

The demo video below is recorded with `scripts/fork_demo/record.sh`; see
[scripts/fork_demo/README.md](scripts/fork_demo/README.md) to re-record it.

Install from source (needs Zig 0.16.0) with `cargo install --path . --locked`.
A fork build refuses `herdr update`, which would replace it with the upstream release; install with `scripts/herdr_live.sh` instead.
To switch the running server to the new build without losing panes, hand it
off live (experimental upstream), then start the client again:

```sh
herdr server live-handoff
herdr
```


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="https://herdr.dev/docs/quick-start/">quick start</a> · <a href="https://herdr.dev/docs/">docs</a>
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/herdrdev/herdr/releases"><img src="https://img.shields.io/github/downloads/herdrdev/herdr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/herdrdev/herdr/stargazers"><img src="https://img.shields.io/github/stars/herdrdev/herdr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/herdrdev/herdr/releases/latest"><img src="https://img.shields.io/github/v/release/herdrdev/herdr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
  <a href="https://formulae.brew.sh/formula/herdr"><img src="https://img.shields.io/homebrew/v/herdr?label=homebrew&labelColor=333333&color=666666" alt="Homebrew version" /></a>
  <a href="https://x.com/herdrdev"><img src="https://img.shields.io/badge/follow-%40herdrdev-000000?logo=x&logoColor=white" alt="follow @herdrdev on X" /></a>
</p>

---

https://github.com/user-attachments/assets/ac3b1146-2a78-43b2-9e53-b5b9434e4940

**the runtime your coding agents live on.**

- **detach without stopping work** — herdr keeps terminals running in a background server when you close the client or lose your SSH connection. after a server or machine restart, herdr restores the saved layout and can resume supported agent sessions; the original processes do not survive. [session state →](https://herdr.dev/docs/session-state/)
- **several machines, one window** — keep local work and saved ssh machines together, with a combined agent list and independent reconnects. [remote machines →](https://herdr.dev/docs/connecting-machines/)
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, herdr says so.
- **agent-native** — agents drive herdr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://herdr.dev/docs/agent-skill/)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. herdr doesn't wrap or replace them; it owns their terminals. building an agent? [add herdr support →](https://herdr.dev/docs/add-herdr-support/)
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://herdr.dev/plugins/)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or `brew install herdr` · `mise use -g herdr` · windows: `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"` · [endpoint-protected Windows](https://herdr.dev/docs/windows-beta/) · [binaries](https://github.com/herdrdev/herdr/releases)

then start it where the work lives:

```bash
herdr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `herdr` reattaches. [quick start →](https://herdr.dev/docs/quick-start/)

## docs

everything lives at [herdr.dev/docs](https://herdr.dev/docs/): [quick start](https://herdr.dev/docs/quick-start/) · [concepts](https://herdr.dev/docs/concepts/) · [supported agents](https://herdr.dev/docs/agents/) · [keyboard](https://herdr.dev/docs/keyboard/) · [configuration](https://herdr.dev/docs/configuration/) · [session state](https://herdr.dev/docs/session-state/) · [connecting machines](https://herdr.dev/docs/connecting-machines/) · [remote](https://herdr.dev/docs/persistence-remote/) · [integrations](https://herdr.dev/docs/integrations/) · [add herdr support to your agent](https://herdr.dev/docs/add-herdr-support/) · [plugins](https://herdr.dev/docs/plugins/) · [socket api](https://herdr.dev/docs/socket-api/)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

enterprise / partnership: hey@herdr.dev

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/herdrdev/herdr
cd herdr
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Herdr is licensed under the [Apache License 2.0](LICENSE).
