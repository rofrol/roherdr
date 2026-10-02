# local.relaunch

Herdr restores panes after a server restart as plain shells. This plugin makes
the programs that were running in them come back, with nothing to configure
and nothing running in the background.

- `relaunch.zsh` (sourced from `~/.zshrc` in herdr panes) writes the command
  you start to `~/.local/state/herdr/plugins/local.relaunch/<socket>/<pane>`
  (preexec) and deletes it when the prompt returns (precmd). A restart kills
  the program before precmd, so its record survives. A shell deletes only a
  record it wrote itself: precmd also runs before a new shell's first prompt,
  and a shell restored after a reboot must leave the old record for the hook.
  Files are 0600 and may contain command-line secrets; treat them like shell
  history.
- The `[[startup]]` hook (`relaunch.js`) reruns each record in the same pane if
  it has the same id and tab, is not an agent pane and is an idle shell.

Each run appends what it did (`ran`, `skip <reason>`, `drop`) to
`~/.local/state/herdr/plugins/local.relaunch/relaunch.log` (0600, trimmed past
256 KiB), because the server keeps hook output only until its next start.

Limits: programs start fresh (no in-app state); only commands typed in zsh are
seen; a one-shot command killed mid-way (e.g. a migration) is run again.

## Why a shell hook, and why zsh only

Only the shell knows the command line you typed. Herdr could read the
foreground process from the OS instead, but that loses aliases, functions and
wrappers: `lg` (an alias for lazygit) or `npm run dev` show up as the process
they start, so the wrong thing would come back. So this plugin hooks into the
shell, and that hook has to be written for each shell.

zsh has `preexec` and `precmd` built in, which is all the hook needs. bash has
neither: it needs a `DEBUG` trap plus `PROMPT_COMMAND` (what the bash-preexec
library does) or `PS0`, which bash 3.2, the one macOS ships, lacks. A `DEBUG`
trap also competes with other tools that set one (starship, atuin), so bash
support needs testing on bash 3.2 and 5.x next to those tools. fish has
`fish_preexec`/`fish_postexec` events and would be simpler.

Neither is written yet because no pane here runs them. In bash or fish panes
the plugin records nothing and those panes come back as empty shells after a
reboot. Open an issue at
https://github.com/rofrol/roherdr/issues if you need one.

Herdr itself has no shell integration for zsh, bash or fish (unlike Ghostty or
kitty, it does not inject rc files or read OSC 133 prompt marks). Building that
into herdr only for relaunching was judged too big; OSC 133 marks command
boundaries but does not carry the command text anyway.

Preview: `node relaunch.js --dry-run`.

Setup (zsh only; other shells are not recorded):

```sh
herdr plugin install rofrol/herdr/plugins/relaunch   # or: herdr plugin link plugins/relaunch
```

then add the zsh hook with the "Relaunch: install zsh hook" popup, or run
`./install` in the plugin directory. It appends a marked block to `~/.zshrc`
(`$ZDOTDIR/.zshrc` when set) that sources a link in the state directory
above; the script and every server start point that link at the plugin, so
the block keeps working when the plugin moves or updates. An rc file that
already sources `relaunch.zsh` is left alone. Panes opened after that record
their programs; open ones need a new shell (`exec zsh`).

Disable: `./install --uninstall` (removes only the marked block) and
`herdr plugin uninstall local.relaunch` (or `unlink` for a linked checkout).
