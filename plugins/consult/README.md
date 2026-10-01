# local.consult

Skills that let a coding agent ask another model for a second opinion, and
statistics on which of those models actually helped.

- `gpt`: GPT via Codex CLI on the ChatGPT subscription (credentials from pi).
- `gemini`: Gemini via Antigravity CLI (`agy`) on the Google AI subscription.
- `deepseek`: DeepSeek API (key from pi).
- `consult-stats`: every call is logged to `~/.local/state/consult/log.jsonl`;
  the agent rates calls after triage (`useful`/`partial`/`useless`, findings,
  accepted, unique) and scores itself as coordinator. `consult.py stats`
  compares the models.

Each skill's `SKILL.md` has the details. Inside herdr every call runs in its
own [herdr-job](../job/README.md) tab, so you can watch it.

## Install

```sh
herdr plugin link ~/personal_projects/herdr/plugins/consult
~/personal_projects/herdr/plugins/consult/install-skills   # links for Claude Code and pi
```

`install-skills [DIR...]` symlinks every skill into each `DIR` (default
`~/.claude/skills` for Claude Code and `~/.pi/agent/skills` for pi), so the
skills follow this checkout. The skills go in together: the scripts find
`consult-stats` as their sibling. It never overwrites an existing directory
of the same name, and it reports a same-named skill in `~/.agents/skills`,
which pi also reads and might use instead. The `SKILL.md` commands use the
skill's own directory, so they work from either agent. `consult.py self`
logs which agent coordinated (`claude-code`, `pi`); pi gives its model and
effort (`$PI_MODEL`, `$PI_REASONING_LEVEL`), Claude Code only its effort, so
there the skill asks for `--model`. Link the plugin rather than
`herdr plugin install` it: an installed plugin is a copy, and links into it
break when it is updated.

## In herdr

The plugin pane list has:

- **Consult stats**: `consult.py stats --pairs` in a popup (`q` closes).
- **Consult: recent calls**: the latest calls with ids and ratings, and the
  rounds that still lack a coordinator entry.
- **Consult: install skills**: runs `install-skills`.

This fork's herdr menu (the launcher at the top of the sidebar) also has a
**consult stats** item that opens the **Consult stats** popup; a click
outside the popup closes it.

Popups run in the herdr server's environment, not your shell's, so they only
read the log; asking a model stays with the agent.
