# local.oracle

Skills that let a coding agent ask another model for a second opinion, and
statistics on which of those models actually helped.

- `gpt`: GPT via Codex CLI on the ChatGPT subscription (credentials from pi).
- `gemini`: Gemini via Antigravity CLI (`agy`) on the Google AI subscription.
- `deepseek`: DeepSeek API (key from pi).
- `oracle-stats`: every call is logged to `~/.local/state/oracle/log.jsonl`;
  the agent rates calls after triage (`useful`/`partial`/`useless`, findings,
  accepted, unique) and scores itself as coordinator. `oracle.py stats`
  compares the models.

Each skill's `SKILL.md` has the details. Inside herdr every call runs in its
own [herdr-job](../job/README.md) tab, so you can watch it.

## Install

```sh
herdr plugin link ~/personal_projects/herdr/plugins/oracle
~/personal_projects/herdr/plugins/oracle/install-skills   # links into ~/.claude/skills
```

`install-skills [DIR]` symlinks each skill into `DIR` (default
`~/.claude/skills`), so the skills follow this checkout. It never overwrites
an existing directory of the same name. Link the plugin rather than
`herdr plugin install` it: an installed plugin is a copy, and links into it
break when it is updated.

## In herdr

The plugin pane list has:

- **Oracle stats**: `oracle.py stats --pairs` in a popup (`q` closes).
- **Oracle: recent calls**: the latest calls with ids and ratings, and the
  rounds that still lack a coordinator entry.
- **Oracle: install skills**: runs `install-skills`.

This fork's herdr menu (the launcher at the top of the sidebar) also has a
**stats** item that opens the **Oracle stats** popup.

Popups run in the herdr server's environment, not your shell's, so they only
read the log; asking a model stays with the agent.
