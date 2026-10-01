# TODO

## Next, in order

Order consulted with DeepSeek, GPT-6 Astra and GPT-6 Luna on 2026-09-26.

- [x] Bug: a herdr-job child tab keeps ⧖ running after the job finished. The
  try-roguix job "Publish Roguix packages and channel" wrote exit 0 at 18:14,
  but its tab stayed running; the final `herdr tab status ... succeeded` is a
  single call with `check=False`, and the server was being live-handed-off
  repeatedly at that time.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): the wrapper
    already writes `exit` first; retry the status call with bounded backoff
    until it succeeds, and reconcile afterwards: `herdr-job` (e.g. on `list`,
    `wait` or a sweep) re-applies succeeded/failed to tabs still marked
    running whose job has an `exit` file. Never infer success merely from a
    missing process. Key updates by job id so a stale one cannot win.
  - Done 2026-09-26: `_exec` retries the final status for about a minute
    (a timeout no longer crashes it), and `run`, `wait`, `list`, `clean` and
    the `tab.closed` hook reconcile tabs still marked running. Tab ids are
    reused, so the newest job of a tab decides, and only while the tab keeps
    its label. No sweep on server start: herdr has no such plugin event.
- [x] Confirm before closing a tab or pane with running work. Closing a
  single job child tab (or a pane) now kills its job with no question; only a
  parent tab with children and the last tab of a workspace ask first.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): ask when
    the close would kill a live foreground process (the server already tracks
    it, see `foreground_program_name` in `src/pane.rs`) or the tab is marked
    `running`; tab status alone misses unwrapped builds and agents mid-turn,
    and a stale ⧖ (see the job status bug above) makes the dialog lie. Word it
    "marked running", not "closing stops it", unless liveness is verified.
  - One shared close-impact check for every path: mouse, keybind, close pane
    (also a non-last pane), close tab, parent tab and its subtree, workspace
    and worktree group, quit. One dialog per operation listing the running
    tabs it would kill (workspace close counts only panes now), Cancel by
    default. Re-check the impact when confirming; the snapshot can be stale.
    Detach does not ask.
  - Ideally the server computes the impact. API closes stay unconfirmed, but
    consider a guard for agents (reject closing running work without
    `force`), since an agent can close the wrong tab.
  - Its own setting beside `confirm_close`, so turning off the other
    confirmations keeps this one. Undo or "keep the job running" only once
    jobs outlive their tab; a delayed kill is not a real undo.
  - Done 2026-09-26, client-side for now: `ui.confirm_close_running` asks
    before a close that stops a tab marked running, a working or blocked
    agent, or a program the shell started (like Ghostty; idle agents and
    finished job tabs close at once). The server sends each pane's
    `running_program`. Still open: the API `force` guard and computing the
    impact on the server.
- [x] Clicking a top-level tab that has child tabs should open the most
  recently active tab of that group, not the first one.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): remember
    the last selection per group, the parent itself included, in the per-client
    location state (one client's navigation must not move another's). Fall back
    to the parent when there is no history or the remembered child was closed.
    Keep a way to select the parent directly (its entry in the second row).
  - Done 2026-09-26: the client remembers each group's last focused tab per
    endpoint; a main-row click, the tab number keys and the main-row wheel
    return there. The parent's entry in the second row selects the parent.
- [x] The consult stats should show the coordinator's actual model and
  reasoning effort (now Opus 5.5 at `medium`; self entries are logged just as
  `claude`), and add Opus at a lower effort as a participant to compare.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): snapshot
    the resolved model id, effort and its source per self entry
    (`CLAUDE_EFFORT` is set by Claude Code, but it is a hint); record
    `unknown`, never a guessed default, and the CLI version.
  - The self entry is not a fair peer (full context, repo access, rates
    itself): show it separately as a baseline. To measure effort, pair
    `claude -p --model <same id> --effort low` with a fresh-context call at
    the coordinator's effort, same prompt; "fresh" must also exclude project
    instructions and tools. It uses the same subscription, so it can starve
    the coordinator: log failures, never drop them.
  - Pilot 10 rounds, conclude after about 20-30 paired rounds. Rate blind
    where practical (same-family bias), and "unique" only relative to that
    round's roster.
  - Record the metadata now; the paired effort experiment is deferred (its
    own research project, and it uses the subscription quota).
  - Done 2026-09-26 (metadata only): `consult.py self` records the model id
    (`--model`, else `unknown`), the effort (`--effort`, else
    `$CLAUDE_EFFORT`, else `unknown`) with its source, and the Claude Code
    version; the coordinator table groups by `model@effort`. The effort
    experiment moved to Deferred.
- [ ] Remove the agents panel; fold agents into spaces. The sort toggle moves
  to the right of the "spaces" header (like the agents panel's
  grouped/priority). Grouped: `<space> <git branch> <git status>`, then per
  agent a line with its state dot and what it works on, then a line with its
  herdr-job statuses.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): yes, but
    it loses an attention queue visible while browsing spaces, so show the
    agents waiting for me in the header, with the agent state circles
    (`◉1 ●1`: one blocked, one done and unseen; `!` stays reserved for
    failed herdr jobs); clicking it switches to priority.
  - Agent states are circles that differ by shape, not only colour (now
    working, blocked and done are all `●`): `◐` working, `◉` blocked (as on
    mobile already), `●` done and unseen, `○` idle; colours stay.
    Priority lists agents, not spaces (a space-sorted list buries several
    urgent agents), with the space on the second line; spaces without
    agents collapse into "other spaces" at the bottom. Freeze the order
    while the pointer is over the list and re-sort only on real state
    transitions (blocked > done-unseen > working > idle, stable ties).
  - The jobs line only when the agent has jobs; jobs without an owner get
    their own row in the space, never an arbitrary agent. Ownership comes
    from the server (`owner_pane` is already in herdr-job metadata); never
    count a job twice. An agent without
    a task title shows `claude · no task`. Every agent is listed, no
    `+N agents` cap: the spaces list scrolls; a chevron collapses a space.
    When tight, one line per agent with counts appended. Truncate the branch first; keep state and counts.
  - Clicks: header toggle switches the view; space row opens its last
    focused pane; chevron collapses; agent row focuses its pane; job counts
    open that agent's jobs. Remember scroll per view.
  - The "menu" and "new" buttons move above "spaces", in swapped order:
    "menu" at the left edge, "new" at the right edge (now "new" is left and
    "menu" right, below the spaces list).
  - Mockups (28 columns):

    ```text
    menu                   new
    spaces     ◉1 ●1  grouped
    ▾ herdr  master ↑4 ±7
      ◐ Name unnamed tabs aft…
        ⧖ 1  ✓ 2
      ● TODO consults
      ○ claude · no task
    ▾ try-roguix  main ±4
      ◉ Build Hyprland portal…
        ! 1  ✓ 3
      ◐ Publish Roguix packag…
        ⧖ 2
      ⚙ jobs  ⧖ 1
    ▸ job-seeker  main
    ▸ music-mpd  main
    ```

    ```text
    menu                   new
    spaces    ◉1 ●1  priority
    ◉ Build Hyprland portal…
      try-roguix · ! 1  ✓ 3
    ● TODO consults
      herdr
    ◐ Name unnamed tabs aft…
      herdr · ⧖ 1  ✓ 2
    ◐ Publish Roguix packag…
      try-roguix · ⧖ 2
    ○ claude · no task
      herdr
    ▸ other spaces (2)
    ```

    `◐` working, `◉` blocked, `●` done and unseen, `○` idle; `!` failed
    job, `⧖` running, `✓` succeeded, `±7` uncommitted changes, `↑4` ahead
    of upstream.
  - Ship in stages (consulted 2026-09-26): grouped view with the state
    circles and job lines; then the attention header and the priority view;
    then clicks and scroll memory. Remove the old panel only after the new
    view works in daily use.
  - Stage 1 in progress (2026-09-26): `ui.sidebar.spaces.agents = true`
    (off by default) lists each space's agents under it, with the state
    icon and task (`claude · no task` without one) and a line with the
    herdr-job counts from the `$jobs` token; clicking an agent or job line
    focuses that agent (local endpoint only). Still to do in stage 1: the
    new state circle shapes, unowned jobs, the chevron.
- [ ] Dragging a space does not show where it will land (screenshot
  2026-09-26, dragging `herdr`). The dragged space keeps a grey background
  much like the selected row, so two grey blocks are on screen; the drop
  marker is a thin accent line in the gap row, which could belong to either
  neighbour; and no-op slots (right above or below the dragged space) show a
  marker like real targets. Here it sat under `herdr`, a drop that changes
  nothing. Do it together with, or right after, the spaces redesign above,
  whose blocks are taller.
  - Decided 2026-09-26 after trying a prototype
    (https://claude.ai/artifact/MNP3sXkyHSZSwCNnUDCiTX): variant C, live
    swap, without collapsing. Past the drag threshold the block lifts
    (accent bar in its first column, raised background, not the selection
    grey) and follows the pointer row by row. When its middle passes a
    neighbour's middle, the neighbour slides past it one row per frame
    (~40 ms/row); swapping back needs the middle to pass the neighbour's new
    middle, which gives hysteresis, so it does not flicker. On release the
    block settles into its slot; on Esc or release outside the sidebar it
    slides back. A fixed hint line says `move herdr before try-roguix · Esc`
    or `no change`.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26) preferred
    a still list with a labelled marker (Astra, DeepSeek) or a guarded live
    preview (Luna); their constraints still apply to C: swaps happen only
    between whole blocks (never inside a worktree family), and redraw only
    when the order or the block's row changes.
  - Do not collapse spaces while dragging (it moves the target as the user
    aims). Worktree children move with the parent, labelled `herdr (+2)`.
  - Done in part 2026-09-26: the list shows the drop live (the dragged
    space and its worktrees move to where they would land), the dragged
    block gets an accent bar instead of the grey, the thin line is gone,
    the header says `herdr → before try-roguix`, `herdr → end` or
    `no change · Esc`, and Esc cancels. The target is the landing slot
    nearest the block's top (grabbed row kept) in the list without the
    dragged block, so it does not flicker. Still open: the row-by-row
    slide animation, auto-scroll near the edges, keyboard reorder, the
    priority-view rule, and the local sidebar only (with remote endpoints
    the aggregate sidebar keeps the old look).
  - Drag starts only from the space's name line after a small threshold, so
    clicks, chevrons and agent/job rows keep working; a click is suppressed
    after a drag. Esc cancels. Time-based auto-scroll near the list edges.
  - Priority view: no reordering, with a hint to switch to grouped.
  - Keyboard reorder (move space up/down, whole family); none exists now.
  - The move is sent by ids (`move X before Y`); if another client changed
    the order or the anchor vanished, cancel with a notice.
- [ ] "Restart agents…": restart agent CLIs (Claude, pi) after they update,
  resuming their sessions, e.g. when Claude reports that a new version is
  available. Should herdr tell the instances to restart once they finish
  their work?
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
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): call it
    "Restart agents…", not "reload" (that reads as a config reload). A scoped
    picker: current agent, selected agents, or the workspace, showing which
    support resume; from the agent's menu and the global menu. Restart
    idle agents, queue busy ones until idle; interrupting work needs an
    explicit choice. The server orchestrates, agent adapters know resume.
  - Stages (consulted 2026-09-26): first a manual restart of a selected idle
    agent, after checking that launch flags are recorded and resume works;
    then version detection, the "restart pending" queue and bulk restart.
  - Stage 1 done 2026-09-26 as the `plugins/restart` plugin (not core):
    menu actions for the focused pane and for the workspace's idle agents.
    Launch flags come from the agent process's own argv (old resume
    arguments and prompts dropped), so nothing has to be recorded; Claude
    with a draft (non-dim text after `❯`) is skipped; SIGTERM, wait for the
    shell, then `claude --resume <id>` / `pi --session <path>`. Tested live
    on a throwaway Claude session. Still to do: version detection, the
    restart-pending queue, a preview/picker, pi's draft check.
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
  - Start with a spike (consulted 2026-09-26): does herdr-job completion
    reach the event stream, and can "away" be detected without core
    changes? Then the plugin.
  - Spike done 2026-09-26, no core change needed: agent blocked/done comes
    as `pane.agent_status_changed`, which runs plugin `[[events]]` hooks (no
    daemon). herdr-job completion has no event (tab status changes emit
    none), but herdr-job already runs `notify()` at the end, so it can call
    the plugin's sender itself. The API knows nothing about attached
    clients or their idleness; "away from the Mac" is better read from the
    OS: macOS `ioreg -c IOHIDSystem` `HIDIdleTime` (keyboard/mouse idle),
    on Linux logind's `IdleHint` or `xprintidle`, plus a manual away/mute
    action writing a state file. Blocked on: a bot token and `chat_id` from
    me, to test sending.
- [ ] Usage footer: show OpenAI API (platform, pay-as-you-go) credits, and
  consider Kimi, GLM and other popular providers.
  - Consulted models (DeepSeek, GPT-6 Luna, 2026-09-26; GPT-6 Astra and Gemini
    hit usage limits): there is no documented way to read the remaining
    OpenAI prepaid balance, with a project key or an admin key.
    `/v1/dashboard/billing/credit_grants` is legacy and undocumented; do not
    build on it. The Admin API (`GET /v1/organization/costs`, needs an
    `sk-admin` key) gives spend only, so show month-to-date spend, optionally
    against a budget set in `[usage]`, labeled "spend", never "credits left".
    An admin key reads org-wide billing: opt-in, its own env var or
    `auth.json` entry, never logged.
  - Kimi (Moonshot): documented `GET https://api.moonshot.ai/v1/users/me/balance`
    (Bearer key) returns `available_balance`, `voucher_balance`,
    `cash_balance` (cash can go negative). Same shape as `deepseek.rs`; the
    easiest one. `api.moonshot.cn` accounts are separate and in CNY: make the
    host configurable, never mix currencies.
  - GLM (Z.ai / Zhipu): no documented balance API. The GLM Coding Plan quota
    (5h window and weekly, plan tier) comes from the undocumented
    `GET https://api.z.ai/api/monitor/usage/quota/limit` (`open.bigmodel.cn`
    for CN keys; raw key in `Authorization`, no `Bearer`), used by many
    third-party trackers. Opt-in, off by default, parse defensively (it
    already changed once: `CREDIT_LIMIT` rows appeared).
  - Skip for now: MiniMax, Mistral, xAI, Groq (no balance endpoint anyone
    could vouch for); Qwen/DashScope only through Alibaba Cloud BSS
    `QueryAccountBalance` with signed AccessKey requests, out of scope.
  - Order: Kimi balance, then OpenAI spend (admin key), then GLM Coding Plan.
  - Only Kimi is next; OpenAI spend and GLM wait until there is a real need.
  - Kimi done 2026-09-26: `usage.kimi` (row `KM`, hidden without a key in
    `MOONSHOT_API_KEY` or `kimi` in the auth file) and `usage.kimi_host`
    (`api.moonshot.cn` bills in CNY). Endpoint checked (401 without a key);
    not tried with a real key, since there is none on this Mac.
- [ ] Agent status shows a green/teal circle while the agent waits on its
  herdr-job: its turn ended, so herdr detects it as idle/done, and it reads
  as "agent finished". Use a different symbol for "waiting on a running job".
  - Consulted models (GPT-6 Astra, GPT-6 Luna, DeepSeek, 2026-09-26; Gemini
    hit its weekly quota): all three recommend `◌` (dotted circle), in both
    the Dots and Symbols styles; Astra and Luna in yellow ("still in
    progress"), DeepSeek in blue, which no other state uses. `◔` is too close
    to `◐` (working); `⌛` is double-width in many fonts.
  - Decided 2026-09-26, after a second round (GPT-6 Astra, GPT-6 Luna,
    DeepSeek): a filled `●` in mauve in the Dots style, `◷` in mauve in the
    Symbols style. A filled dot is easier to spot in a long list than `◌`,
    and yellow already means working. Not blue (Luna's and DeepSeek's pick):
    blue is the accent, it means finished in the mobile view and in
    notifications, and it is close to teal (done). Check mauve's contrast on
    light themes. Yellow `◌` is the fallback.
  - A TUI presentation override, not a new `AgentStatus`: apply it only when
    the detected status is idle or done and the pane has running jobs; working
    and blocked still win. A running job does not prove the agent waits on
    it, so do not add a `Waiting` status to the frozen API enum.
  - Derive it from a structured server fact (e.g. a running-jobs count in pane
    metadata), not by parsing the rendered `$jobs` token text.
- [ ] Make the consult skills (`plugins/consult`: gpt, gemini, deepseek,
  consult-stats) work in pi too, not only in Claude Code. pi 0.87.1
  implements the Agent Skills spec and reads `~/.pi/agent/skills/` (also
  `~/.agents/skills/`), but every `SKILL.md` hardcodes
  `~/.claude/skills/...` paths, and `consult.py self` logs the coordinator as
  `claude` by default. The scripts already find their siblings through
  `realpath "$0"`, so only the instructions and the logging need changes.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-26; Gemini hit its
    weekly quota): `install-skills` links into both `~/.claude/skills` and
    `~/.pi/agent/skills`, not `~/.agents/skills` (not verified that Claude
    Code reads it). Refuse when the same skill name is already visible to pi
    from another directory: pi keeps the first one found and only warns, so
    a stale copy would shadow updates.
  - `SKILL.md` commands use the skill's own directory (pi tells the model
    where a skill lives), with quoting; `consult-stats` stays a sibling of
    the others, as an installer invariant. No `~/.local/bin` wrappers.
  - Log the coordinator's agent (`claude-code`, `pi`) and its model id
    separately, `unknown` when not known, never a guessed default. Spike
    first: does pi expose its model to the bash tool (env, session file)?
    Otherwise the skill tells the coordinator to pass `--model`. Overlaps
    the coordinator-metadata item above; do them together.
  - Descriptions must route: pi's own `oracle` skill (pi-fabric reviewer)
    also reads as "second opinion"; reword the descriptions so the model can
    tell them apart, drop "Claude" from them. Keep `oracle` separate for now.
  - Out of scope: the Claude-only `herdr-bg-badge` hook and
    `herdr-peer-token` (`~/.claude/sessions`).
  - Verify in a live pi session: every skill, a round with ratings and
    `self`, an unrelated cwd, inside and outside herdr; then check Claude
    Code still works.

## Deferred

- [ ] Consult stats: pair the coordinator with Opus at a lower effort
  (`claude -p --model <same id> --effort low`, fresh context without project
  instructions or tools) to measure what effort buys; see the done
  coordinator item above for the method (pilot 10 rounds, conclude after
  20-30, blind ratings where practical, log failures).
- [ ] herdr > menu > settings > usage: checkboxes choosing which providers the
  usage footer shows. Also token-based usage?
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): the
    checkboxes, yes. Tokens answer a different question ("what did this
    cost?") than the footer ("can I keep going?"): if ever, a separate usage
    details view, not the footer. Transcript scraping is brittle (resumed
    sessions, retries and cache tokens double-count), so only with a concrete
    need; split input, output and cache, and label estimates.
  - Deferred (consulted 2026-09-26): premature with few providers; built-in
    settings widgets are enough, no plugin settings framework needed.
- [ ] herdr > menu > settings > consults: an "enabled" checkbox column per
  model (which models get consulted), and next to it the consult stats
  columns. Then drop the separate "consult stats" menu item.
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): consult is
    a plugin, so core needs a declarative plugin settings page (a versioned
    schema of tables, checkbox fields, loading/error states, actions proxied
    over the endpoint); core renders standard widgets, no plugin-drawn TUI and
    no consult-specific code in core. Keep latency/tokens from crushing the
    checkbox and model name (a detail view per model).
  - Deferred: one consumer does not justify a plugin settings framework yet.
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
