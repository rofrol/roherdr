# TODO

- [ ] Do I still need the Agents panel? Maybe an Agents tab next to Spaces,
  sorted by priority by default, where a click switches to the right tab in
  Spaces.
  - Consulted models (DeepSeek, GPT-6 Luna, 2026-09-26): keep Agents as an attention
    inbox in a `Spaces | Agents` sidebar tab; Spaces keeps its badges.
    Clicking an agent focuses its pane but the sidebar stays on Agents
    (otherwise every click loses the queue); "reveal in Spaces" separately.
    Priority order must not jump while the pointer is over the list.
    Attention count on the tab label.
- [ ] Claude reports that a new version is available. A herdr menu item that
  restarts Claude instances when possible? How: send the instances a message
  to restart once they finish their work? Same for pi.
  - Consulted models: do not ask the agent (it costs context and cannot replace its
    own process); herdr restarts it. Version: record `claude --version` when
    the pane starts, compare with the binary on disk (mtime only as a hint).
  - Restart only when the pane is idle, not blocked, with no draft in the
    input box, no subagents (`SubagentStop` hook) and no jobs; otherwise mark
    it "restart pending". Then `/exit` and resume with the plan from
    `src/agent_resume.rs` (`claude --resume <id>`, `pi --session <path>`),
    one at a time.
  - Menu with a preview: how many idle / working / blocked, pick which.
    Launch flags (permission mode, model, env) must be recorded; resume does
    not restore them. For pi, check that `--session` restores everything.
- [x] Make the tabs consistent (two rows: main `1 ⧖ 1 | lazygit | 3 | +`,
  below it `claude:` and the job tabs).
  - Consulted models: only the active main tab gets the filled blue background; the
    active entry in the second row gets an underline or bold (now there are
    two blue highlights at once). The parent entry without the colon and the
    grey block, e.g. `claude` with an icon, then a `│` separator. One rule
    for dimming. A number on every main tab or on none.
  - Done 2026-09-26 as variant D: full accent only on the entry on screen.
- [ ] Publish the consult stats (`consult.py stats`) through a separate
  project, `consultstats` (its own repo, e.g. `~/personal_projects/consultstats/`).
  Nothing about where it is published belongs in this repo or in that
  project's code: the host, path and deploy command come from its config
  (e.g. an ignored `.env`), so anyone can publish their own stats anywhere.
  Mine will go to `consultstats.frolow.dev`.
  - Name (DeepSeek, GPT-6 Luna, 2026-09-26): not `llmstats` (JEV, a System 1
    model, and other non-LLM systems come later), not `skilloraclestats`
    (long, and "oracle" reads as the company).
  - Split: this plugin only gets an export (e.g. `consult.py export`) that
    writes the allowlisted aggregates as JSON; `consultstats` turns that JSON
    into a static site and deploys it. The raw log
    (`~/.local/state/consult/log.jsonl`) never leaves the machine.
  - Consulted models (DeepSeek, GPT-6 Luna, 2026-09-26): a static site built
    locally, deployed by rsync of the output only. For my frolow.dev: like
    `frolow.dev/deploy.sh`, subdomain like `matchalove.frolow.dev` (Porkbun A
    record, nginx `conf.d`, `certbot --nginx`); that setup lives in my
    frolow.dev repo, not in `consultstats`.
  - Export from an allowlist of aggregates only: no prompts, answers, notes,
    cwd, round ids or exact timestamps (weekly/monthly at most); hide groups
    with few calls. A test on a fake log checks that forbidden fields never
    reach the output.
  - Honest presentation: `n` next to every rate, confidence intervals
    (Wilson), "preliminary" below ~5 rated calls, a note on bias (self-chosen
    tasks, non-blind ratings, `unique` depends on who else was asked).
    accepted/findings is an acceptance rate, not recall; check the name.
  - Update manually first (export, review the diff, deploy); launchd later.
- [ ] Telegram notifications when I am away from the Mac (agent blocked,
  agent done, herdr-job finished).
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Sol, 2026-09-26): Telegram
    is a good fit: a bot sends to my private chat (`chat_id`), free, reliable
    Android push, no Meta-style restrictions (Instagram was rejected: no API
    for broadcast channels, DMs need app review and a 24h reply window).
    Bot chats are not end-to-end encrypted. ntfy or Pushover as alternatives.
  - A plugin subscribing to the socket API events, not core; bot token and
    `chat_id` in the plugin config, never in payloads. Check whether
    herdr-job completion reaches that event stream. Transitions only: to
    blocked, to done, job finished/failed; dedup per pane and approval
    request, coalesce bursts, drop an alert that is stale (agent resumed).
  - Send only when away: no attached client or all clients idle for N
    minutes, plus an explicit away/mute toggle.
  - Content: the same text as the toast (`claude finished` plus
    `workspace · 1 · tab`, see `notification_context`); it has no paths,
    prompts or agent output, which is fine for a private bot chat.
  - Later: inline keyboard buttons (approve / deny) answered through the
    herdr socket, accepting callbacks only from my own user id.
