# local.consult

Skills that let a coding agent ask another model for a second opinion, and
statistics on which of those models actually helped.

- `consult`: the round itself: `consult.py round` asks the default set, which
  lives only in `consult-models.json` (read at every invocation, so no
  session runs a stale set), leaves out the caller's own model family, and
  the skill says how to rate and score.
- `gpt`: GPT via Codex CLI on the ChatGPT subscription (credentials from pi).
- `deepseek`: DeepSeek API (key from pi).
- `claude`: Claude Sonnet 5.5 by default, or explicit Opus 5.5 with
  `ask_claude.py -m claude-opus-5-5`, via Claude Code CLI. Subscription billing
  requires subscription login; API-key/cloud billing is not a subscription.
- `openrouter`: an OpenRouter model (default Xiaomi MiMo-V2.6-Pro pinned to the
  Xiaomi provider). MiMo is in the default set with sol and DeepSeek (DeepSeek
  left it on 2026-10-03 and rejoined on 2026-10-11). Gemini 4 Argon (`-m argon`,
  pinned to Google AI Studio) joined the set on 2026-10-15; other OpenRouter
  models only on request.
  Providers may retain what is sent, so the skill never sends secrets: it has no repo mode, `-f` attachments are vetted
  fail-closed outside its sandbox, and the prompt is hard-refused if it contains
  secret-shaped strings. The MiMo trials are described in its `SKILL.md`; the
  cloaked Space Bunny was removed on 2026-10-03 after failing its trial.
- `consult-stats`: every call is logged to `~/.local/state/consult/log.jsonl`;
  the agent rates calls after triage (`useful`/`partial`/`useless`, findings,
  accepted, unique) and scores itself as coordinator. `consult.py stats`
  compares the models.

## The default set

`consult-models.json` holds the default set (`default`), every model `round`
can ask (`models`: its family and the `ask_*` command, relative to `skills/`)
and the patterns that map a caller's model id to a family (`families`). Edit
only this file to change who joins a round; the skills name no models.

History of the set:

- 2026-10-15, the user: Gemini 4 Argon added next to sol, MiMo and DeepSeek
  ("add Gemini 4 Argon to ask"). agy does not offer it yet, so it goes through
  OpenRouter, pinned to Google AI Studio ($2/$10 per M tokens, several cents a
  consult). It counts as the gemini family, so a Gemini caller skips it.
  The same day the user removed the `gemini` skill (Gemini 3.8 Flash through
  agy): Argon is the only Gemini model the toolkit asks. Its past calls stay in
  the consult log and in `consult.py stats`.
- 2026-10-11, the user: sol 6.1 (effort medium, the gpt script's default),
  MiMo and DeepSeek ("add deepseek to the default set"; asked first on
  2026-10-07: it is fast, and on Claude Max 20 the tokens spent reading its
  answer matter less).
- 2026-10-03: DeepSeek left the set. In shared rounds it was the weakest of
  the three (sol +0.67 accepted unique findings per call over 161 rounds, MiMo
  +0.48 over 58 rounds and +0.45 in its 20-round trial), and its speed bought
  nothing because a round waits for sol anyway. Not dropped for cost (about a
  cent a call). Space Bunny was removed the same day (failed its trial).
- Astra is off by default, available on request. The 2026-09-27 astra/luna
  trial is the only paired evidence: luna@medium was clearly behind astra
  (0.58 vs 1.43 accepted unique findings per rated call, 22% vs 35% rejected
  findings; astra 69, luna@medium 33 rated calls), and gpt-6-sol never beat
  astra over ~12 paired rounds. Luna (`-e medium`) is only a fallback when a
  sol call fails or hits the Plus limit; Terra only on request.

## Quotas are per vendor

A limit in one skill does not block the others, and asking one model does not
spend another vendor's quota:

| Skill | Bills |
| ----- | ----- |
| `gpt` | the ChatGPT subscription, through Codex CLI |
| `claude` | the Anthropic account (Claude Code login; no API key is used) |
| `deepseek` | DeepSeek API credits, per token |
| `openrouter` | the OpenRouter account (per token; MiMo about half a cent a consult, Argon several cents) |

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

Availability is decided by the call, not by the dashboard. Each helper
attempts the requested call once without a quota preflight or availability
cache; the vendor's rejection is reported and logged. Herdr's usage panel reads
quotas for display, not to authorize consultations.
Claude Code reports its buckets for free, without spending tokens:

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

Both popups run through `page-consult`, which passes the popup's width as
`--width`: a table wider than the popup is split into bands that each repeat
the model column, so scrolling never leaves numbers without names. An error
is shown after the pager closes.
- **Consult: install skills**: runs `install-skills`.

This fork's herdr menu (the launcher at the top of the sidebar) also has a
**consult stats** item that opens the **Consult stats** popup; a click
outside the popup closes it.

Popups run in the herdr server's environment, not your shell's, so they only
read the log; asking a model stays with the agent.
