# herdr

## Fork changes

Personal fork of [herdrdev/herdr](https://github.com/herdrdev/herdr); these
commits sit on top of upstream `master` and are not meant for upstream PRs.

- **Usage widget.** A sidebar footer below the agents list shows how much of
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
  hidden without a key. The server refreshes every 5 minutes
  (`refresh_interval_secs`); set `claude`, `codex`, `gemini`, `deepseek` or
  `openrouter` to `false` to hide a provider. All herdr instances share one
  cache (`~/.local/state/herdr/usage-cache.json`): a recent observation is
  reused instead of refetched, and a rate-limited provider backs off
  (5 min doubling to 1 h) while the footer keeps its last good values.
- **Job tabs from the [job plugin](plugins/job/README.md).** Each
  `herdr-job` job gets a child tab of the tab that started it. While a
  workspace has child tabs, a second row under the tab bar lists the active
  tab's own content and its jobs (`claude  ⏳ tests  ! build`), and the parent
  tab shows a summary such as `⏳1 !2 ✓3`. The space's sidebar row counts
  its running and failed tabs (`○ repo ⏳1`, the `tab_jobs` token), so
  background work stays visible while the agent is idle, and the agent that
  started the jobs shows their counts (`2⏳ 1✗ 1✓`, the `$jobs` token).
  Closing a parent tab asks first and closes its jobs too. Child tabs and
  statuses are ordinary API (`herdr tab parent`, `herdr tab status`), usable
  by any script.
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
  and shows `⏳ Build`, then `✓`/`✗ <code>`, in the sidebar of the pane that
  started it; `herdr-job wait <id>` follows the log and exits with the
  command's code. It keeps state in files, so it works for any agent (Claude
  Code, pi, ...) or by hand. Also has `herdr-bg-badge`, a Claude Code hook
  that shows Claude's own background tasks in the sidebar.
- [**relaunch**](plugins/relaunch/README.md): reruns the programs panes were
  running (lazygit, editors, ...) after a server restart or reboot; herdr
  itself brings them back as empty shells.

The demo video below is recorded with `scripts/fork_demo/record.sh`; see
[scripts/fork_demo/README.md](scripts/fork_demo/README.md) to re-record it.

Install from source (needs Zig 0.16.0) with `cargo install --path . --locked`.
Don't run `herdr update`: it replaces the fork with the upstream release.
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

https://github.com/user-attachments/assets/af7d3889-cb5b-4fbf-986c-e9751884a48c

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
