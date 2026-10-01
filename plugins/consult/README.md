# local.consult

Skills that let a coding agent ask another model for a second opinion, and
statistics on which of those models actually helped.

- `gpt`: GPT via Codex CLI on the ChatGPT subscription (credentials from pi).
- `gemini`: Gemini via Antigravity CLI (`agy`) on the Google AI subscription.
- `deepseek`: DeepSeek API (key from pi).
- `claude`: Claude Sonnet 5.5 via Claude Code CLI (its configured authentication).
- `consult-stats`: every call is logged to `~/.local/state/consult/log.jsonl`;
  the agent rates calls after triage (`useful`/`partial`/`useless`, findings,
  accepted, unique) and scores itself as coordinator. `consult.py stats`
  compares the models.

## Quotas are per vendor

A limit in one skill does not block the others, and asking one model does not
spend another vendor's quota:

| Skill | Bills |
| ----- | ----- |
| `gpt` | the ChatGPT subscription, through Codex CLI |
| `claude` | the Anthropic account (Claude Code login; no API key is used) |
| `gemini` | the Google AI subscription, through `agy` |
| `deepseek` | DeepSeek API credits, per token |

So a Codex/ChatGPT limit (`You've hit your usage limit`) says nothing about
Sonnet, and vice versa. After a limit error the call is logged with `status
error` and no answer: check `consult.py recent` before blaming a skill. Limits
also decide which models a round uses: consult the pair that excludes the model
the session itself runs on, and fall back to another vendor when one is
exhausted.

Do not persist a quota verdict ("blocked until X") in a state file: the provider
can reset a window early, and the stale block then outlives the limit. Probe
live when the probe is free (`agy -p /quota`, `claude -p "/usage"`), block only
on a verified zero, and cache only what costs a real request. The full rule is
in the repository `AGENTS.md`.

Vendors run out independently, but a percentage in a usage panel is not proof
of unavailability: Anthropic's own view showed 100% used with an 11-hour
renewal while Claude Code answered three consultations in a row, because plan
limits are separate buckets (rolling windows, and model-specific allowances),
and an explicitly requested model can still have room.

Availability is decided by the call, not by the dashboard. Claude Code reports
its buckets for free, without spending tokens:

```sh
claude -p "/usage"
# Current session: 0% used · resets Sep 30 at 7:29pm
# Current week (all models): 100% used · resets Oct 1 at 1:59am
# Current week (Fable): 1% used · resets Oct 1 at 1:59am
```

On 2026-09-30 that weekly all-models bucket stood at 100% while two Sonnet
consultations and a fresh probe call (`Reply with exactly: SONNET-OK`) all
succeeded, so the percentage is a warning, not a wall, and a rejected call is
the only proof. Treat a vendor as unavailable after a limit error, quote that
error and the reset time it names, and never silently substitute another model.
That also keeps a self-consultation from being dressed up as a second opinion.

When the endpoint reports a critical limit, a consultation is still worth
attempting, but single-shot and small: no retry loops against that vendor.
After the first limit rejection, remember that vendor as unavailable until the
reset time the error names, switch to another one, and tell the user which is
unavailable and until when instead of quietly changing the round.


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
