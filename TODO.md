# TODO

Open work only. A finished item leaves this file: its durable
decisions go to `DECISIONS.md`, the rest stays in the commit messages.
Parked ideas live in `TODO-deferred.md`. An open item keeps only its
title with the user's words, what is still open, and the decisions that
constrain it.

## Next, in order

Agents may do these from the top without asking when the user tells them to
work through the TODO (the user's global agent rules, "Working through TODO.md").

- [ ] A coordinator stops between items without being asked (user,
  2026-10-07: "why aren't you delegating anything? ... explain why; we want
  to improve the process, not have you start working now and forget"). Twice
  the herdr coordinator answered the user's mid-turn questions, wrote a
  status report and ended its turn with "I will delegate the next item when
  you say continue", although the items were approved. Causes: (1) the rule
  says questions are answered, not queued, but not that the coordinator
  goes on afterwards, and a report reads as a natural end of turn; (2) a
  Claude coordinator only acts inside a turn: once the turn ends, nothing
  starts the next item or reviews a finished worker until the user writes.
  Candidates: a rule line ("answering or queuing never ends the turn; end
  it only when 'Next, in order' is empty or every item waits on the user");
  waiting for a worker with a background wait that wakes the session when
  it ends (done since 2026-10-07 in this session); a Stop hook that blocks a
  coordinator-role tab's stop once while "Next, in order" has items and no
  worker runs; herdr flagging an idle `♛` tab with open items.
  Consult round `20261007-024331-bde5` (sol, MiMo; the user: "how would I
  know what works? ask the models, do tests"). Both: the rule line is the
  direct fix but only probabilistic, and must say what legitimately ends a
  turn (cancellation, a decision whose answer changes the diff, failed
  checks, missing credentials), with the blocking question written into
  "Needs a decision" so a silent stop is checkable; "a mid-turn question
  does not revoke approval" (sol). The background wait fixes a different
  failure (nothing reviews a finished worker), not this one: here no worker
  ran (both). The Stop hook is the only deterministic backstop, but one nudge
  only (`stop_hook_active`) and it costs a turn per false positive: ship it
  only with its predicate unit-tested on synthetic hook input to zero false
  positives (both). An idle-`♛` flag is observability, not prevention.
  Bigger alternatives: a durable item state machine outside the model (sol)
  or a shell driver that runs the queue and calls Claude per item to review
  (MiMo).
  Test plan, cheapest first: (1) an offline audit of coordinator
  transcripts that labels each turn end (finished, blocked on the user,
  abandoned with items left and no worker), the baseline; (2) the rule
  wording, then the audit on later transcripts; (3) the Stop hook with its
  predicate tested on the audit's cases; (4) headless runs on a throwaway
  repo with three trivial items and a stub worker, with "what is happening?"
  injected mid-turn, many runs (costs Claude usage; ask first); (5) a field
  metric: abandoned turn ends per 100 coordinator turn ends, before and after.
  Step 1 done 2026-10-07 (`78167f03`, `scripts/coordinator_turn_audit.py`):
  baseline over this machine's transcripts, 3 coordinator sessions, 36 turn
  ends: asked 1, waiting 32, abandoned 2 (both in the herdr coordinator,
  the known case), other 1, so 5.6 abandoned per 100. Small sample; rerun
  after the rule change.
  Step 2 done 2026-10-07 (dotfiles `45c6466`, approved by the user in the
  worker's pane after the auto-mode classifier blocked a worker): a
  mid-work question does not withdraw the approval; a turn ends only when
  every item waits on the user, Next is empty, the user says stop, or a
  background wait on a running worker will wake the session.
  Step 3 done 2026-10-07 (`e238621c`, integration reinstalled with the
  user's consent): the Stop hook blocks a `coordinator`-role tab's stop once
  when its final text waits for a go-ahead (the audit's `ABANDON`
  expression, parity-tested) and no background task of the session runs;
  fails open. Left: step 4 (headless runs, ask first) and step 5 (rerun the
  audit after a few days of coordinator work and compare with 5.6 per 100).

Three gaps found in https://spznrf.dev/blog/the-fellowship-of-the-pane
(2026-10-03: a user runs five Pi agents in five visible Herdr panes, a main
session that writes all code and delegates to documenter, reviewer, qa and
ops-recon through `herdr agent prompt`, one of them on a remote host).
Consulted sol and MiMo twice (2026-10-06, rounds `20261006-023800-a481`
and `20261006-030215-b8ca`); both put the first two at the top.

- [ ] Rebase the fork on upstream (user, 2026-10-07: "rebase on upstream?").
  2026-10-07: 21 upstream commits behind, 372 fork commits on top
  (upstream `a124eed7`, "route all pane key encoding through libghostty").
  The standing approval in AGENTS.md ("Fork Sync") covers the rebase and the
  leased force-push of `master`. Do it at a boundary: no worker based on the
  old `master`, the shared checkout clean (another session's idle-mark edits
  are uncommitted there), then `just check`, build, install.
  In progress 2026-10-07 with worker `w-rebase` (branch
  `todo/rebase-upstream`, `git rerere` on); the shared checkout is clean.

- [ ] Usage summed per workspace. The author asked every session for its
  `/session` accounting by hand and had an agent record the total. The
  fork's usage module has the numbers per agent. Risk: totals that disagree
  with the provider's bill, and resumed sessions counted twice (sol).
  Decided by the user 2026-10-06: CLI first (`herdr usage --workspace` or
  similar), resumed sessions counted once by session id, labelled an
  estimate, not the bill; no sidebar total yet.

- [ ] Bug (user, 2026-10-03, screenshot): "I closed the tab with the job,
  but it did not close the job." The explicit close is fixed (`DECISIONS.md`,
  "Closing a parent's last pane"); a parent whose shell exits by itself
  keeps its jobs on purpose, but nothing shows it.
  Decided by the user 2026-10-06: when a parent exits on its own, show the
  "Parent <name> exited; N jobs kept running" notice and a `was <name>`
  mark on the orphaned rows (sol, DeepSeek, MiMo); no new "Close tab, keep
  jobs" button.

- [ ] The tab state does not show that something runs in the background
  (user, 2026-10-06, screenshot: "the tab state doesn't show that something
  is running in the background"). Space `music-mpd`: the tab showed idle
  while Claude waited for a finite `musicdb update` (about 2 minutes), its
  footer saying `1 shell still running`. The 2026-10-03 decision left a
  shell alone idle on purpose (`DECISIONS.md`, "Claude background tasks and
  working state"); this revisits it.
  Decided by the user 2026-10-06: a `bg:N` observed-count badge on the
  tab, dev servers included; a `herdr-job wait` counts as work; no
  separate awaiting-background flag.
  - Shape: an optional runtime field (pane background task counts, parsed
    from the footer below the prompt box) in the JSON API, drawn by the TUI
    on the tab line; never a new `AgentStatus` variant (append-closed in
    frozen codecs). Report unknown (no footer seen, agents such as Codex)
    apart from an observed zero; show the count next to every state; notify
    only the final done, never on shell exit (sol). A bare count never makes
    a space busy (bubble, header).
  - Glyph: not `◑` (herdr-job jobs; it promises a tab, a log and an exit
    code), not `⧖`, not `○` (the idle glyph). Dimming must be carried by
    text, not colour (16-colour themes, `NO_COLOR`). Consult calls
    `557d6c84`, `56fe2bac`, `f2132f2c`, `9c834783`.

- [ ] Consult cost per model and the coordinator's extra spend (user,
  2026-10-03: "how much money/tokens a model used on a consult, and how much
  more the coordinator burned by asking it"). Today every call logs normalized
  usage, but no money, and the coordinator's own tokens are not logged at all.
  Round `20261003-013157-b88d` (Sol, DeepSeek, MiMo, Space Bunny agree):
  - Money only where money exists: a versioned, dated price table (input,
    cached input, output; reasoning billed as output, never twice). Subscription
    models (GPT, Claude, Gemini) show tokens and "included in subscription",
    never a made-up price; an API-list-price equivalent only as a separately
    labelled column. A free preview model is `$0` for now, not for good.
  - Coordinator: log the Claude Code session id and the round's start and end
    (`new-round` to the last `rate`/`self`), then sum that window's per-message
    usage from the session transcript, cache reads apart, per round, not split
    per model. Do not add the answers again (already in the tool-result
    input). `answer_chars` is only a fallback proxy: it misses reasoning.
  - The true "how much more" needs a few matched tasks with and without a
    consult; a one-off audit, not a stats column.
  Decided by the user 2026-10-06: approved as proposed: a dated price
  table with `$` only for DeepSeek and OpenRouter, and the coordinator's
  usage summed per round from the transcript, labelled "consult-
  associated".

- [ ] Naming: `ask_*` scripts versus the `consult` plugin and `consult.py`
  (user, 2026-10-03: "do we need to unify ask in one place and consult in
  another?"). `consult` names the bundle and the stats, `ask_*` are the
  per-vendor adapters; renaming would split the log keys (`skill` field).
  Decided by the user 2026-10-06: keep the names; add one README line
  explaining them.

- [ ] Consult stats default view: mixed rows, too much data, and why `astra
  -r` ranks above `astra` (user, 2026-10-03: "astra -r better than astra, why?
  how do you rate these models now? The table is mixed up, deepseek is third;
  maybe show last week as the first table. Very much data; is it needed? ask
  the models"). Round `20261003-124630-aaf5`; findings in `DECISIONS.md`
  ("Consult roster").
  - The mix-up: the default table pools all time and sorts by uniq/call, but
    unique depends on who else was asked; `--days 7` alone still shows the
    09-26..09-28 rows.
  - Plan: current configurations first (the default set and running trials,
    in configured order), last 7 days with the dates printed; retired models
    and rows under 5 rated calls collapse into one footer line; rows from
    another coordinator marked or split. Keep: rated/calls, uniq/call,
    rejected share, err, p50. Cut from the default: call dates, the 8-line
    legend (two lines plus `--legend`), anecdotal rows. All of it stays
    behind `--all`.
  Decided by the user 2026-10-06: default to the current set over the last
  7 days; merge the unknown-version DeepSeek alias row into V4.1.

- [ ] Consult stats by lineup (user, 2026-10-03: "shouldn't consult stats
  show which models were tested together, e.g. sol ds mimo, and now a new
  stage sol mimo? ask the models"). Unique per call only compares models
  asked beside the same companions; the log has 32 distinct lineups. Round
  `20261003-130240-6583`. Plan:
  - `new-round` records the requested lineup (`--models sol,mimo`, the
    consult skill passes the default set), because dates cannot assign
    stages (the MiMo and Space Bunny trials ran inside the sol+ds period).
    Older rounds get a lineup derived from their calls, marked derived.
  - `stats --lineups`: one block per lineup with dates, coordinator, rounds,
    full rounds; per model calls ok/failed, findings, accepted, rejected,
    unique per answered call, p50. Lineups under 5 rounds fold into one line.
  - Default `stats`: the current lineup's block first; no ranking across
    lineups.
  - Kept apart, each with a count so nothing is silently dropped: one-model
    asks, rounds where a companion failed (its outage inflates the other's
    unique), rounds run by another coordinator, rounds with an extra model
    asked on request.
  Decided by the user 2026-10-06: approved: `new-round --models`, `stats
  --lineups`, the current lineup first by default; no named stages (the
  consult skill records set changes).

- [ ] "Consult: models" menu with checkboxes (user, 2026-10-03: "a simple
  menu: which models are used for consultation now, a checkbox to enable or
  disable, its rank, uniqueness, error rate, and maybe how much the
  coordinator's token cost increases"). Narrows the deferred settings >
  consults page and the auto-consult toggle. Round `20261003-145404-dae6`.
  Decided by the user 2026-10-06: unblocked; the native modal gets its
  data from `consult.py ... --json` through the server, so the statistics
  logic stays in one place.
  - A native herdr modal in Rust (user, 2026-10-03: "a script? I want it in
    Rust"), in the existing dialog style, mouse-first, replacing the menu's
    **consult stats** item. New advertised API methods with neutral names
    (e.g. `consult.models.list`, `consult.models.set`), so it works against a
    remote server; an older server disables only this item. Rows `[x] model
    | uniq/call (n) | wrong% | err% | rated/calls | last used`; a toggle
    shows only after the server confirms it is persisted.
  - State: one global file `~/.local/state/consult/models.json`, written
    atomically. `consult.py models` prints the enabled set and is the single
    source: the skill's default when the file is missing, an empty list means
    consulting is off, a malformed file is an error, not a silent default.
    The consult skill runs it at each round. An explicit request ("ask
    DeepSeek") bypasses the checkbox but never the self-consultation rule or
    a missing key.
  - `new-round` records the enabled set and whether the round was automatic
    or explicitly requested (feeds the lineup item above).
  - No rank column: one number moves when another row is toggled. Metrics
    from rounds of the actual lineup, with n shown, hidden under 5 rated
    calls; `stats --vs` stays the comparison.
  - Coordinator cost, stage 1: the answer tokens each round injects
    (`answer_chars`), labelled a lower bound; the full number waits for the
    cost item above. Subscription models show "included", never `$0`.
  - Later: a `doctor` mark for an enabled model without a key or CLI.

- [ ] Consult stats per model over time, to spot a silently "nerfed" model
  (user, 2026-10-03: "what if we showed stats for a model over time? we could
  detect a nerfed model. How to display those graphs then? ask the models").
  `model_version` exists for DeepSeek, MiMo and Claude, never for the GPT
  models (Codex does not report it). Round `20261003-023646-4385`.
  Decided by the user 2026-10-06: only the smallest first step now: a
  weekly list of `model_version`/fingerprint per model; the full
  `consult.py trend` waits. When it comes: a drift report, not a "nerf
  detector" (no composite score, no alerts); the paired difference against
  a reference model over shared rounds (three-model rounds tell which side
  moved); equal-n blocks with CIs and "insufficient n" below the minimum, a
  rule fixed in advance; text in the `page-consult` popup, no kitty-graphics
  PNG (`less -R` strips it).

- [ ] No `?` on a tab that ended with a question (user, 2026-10-01, screenshot
  of this very session: the tab showed the idle green ring after a turn that
  ended "Install this build, push the commits, or fix the flaky test first?").
  The Claude Code Stop-hook check is done (`DECISIONS.md`, "Stop-hook check
  for unreported questions"). Open: Pi (no `Stop` equivalent found; it needs
  an `agent_end` extension), V3 inference (the hook marks the pane itself,
  `source = inferred`, per-turn generation, cleared on `UserPromptSubmit`,
  typing and the next tool use), an LLM judge for the audit (rubric: "does
  the final message ask the user for a decision or an answer before work can
  continue", two judges, blind to the report status). Thresholds: V3 only
  when the inferred precision's lower bound is above 98-99% and V2 is not
  enough.
  Decided by the user 2026-10-06: investigate the pi part with a consult
  round (should pi get an `agent_end` nudge although it cannot block a
  stop, or does this close with V3?). If the models agree, do what they
  agree on; if not, move the item back under Needs a decision with their
  findings.

- [ ] Consider adding a subtle gradient in the empty space between the job
  indicators and the next tab in the sidebar (screenshot, 2026-09-29 23:53).
  Show several visual variants in the terminal before choosing one; generate
  the demos with Python, as Claude did previously.
  Decided by the user 2026-10-06: the agent makes demos of a few variants
  for the user to view in a tab, then moves the item back under Needs a
  decision with the variants named, so the user picks one with a click.

- [ ] Remove the agents panel; fold agents into spaces. The spaces list
  already shows vertical tabs with job squares (`DECISIONS.md`, "Vertical
  tabs and job squares"); the old panel is only hidden by
  `ui.sidebar.show_agents_panel = false`.
  Decided by the user 2026-10-06: remove the panel and
  `show_agents_panel`; the attention counts (`◉1 ●1`: blocked, done and
  unseen; clicking switches to prio) move to the sidebar header; raise the
  white-on-accent contrast (`#4078F2`, 3.9:1) to at least 4.5:1.
  - Also open: space drag and drop in the multi-machine sidebar still works
    from the drawn spaces only.

- [ ] Dragging a space does not show where it will land (screenshot
  2026-09-26, dragging `herdr`). The live reorder is done (`DECISIONS.md`,
  "Dragging spaces"). Open:
  - A collapsed space does not show that it is the focused one (screenshot
    2026-09-29). A collapsed space is one line: no branch line, its git
    status on the name line without the branch (`► herdr ↑2 ⧖ 1 !3`), and
    the focused collapsed space's name line gets the focused active tab's
    solid accent fill. Consulted (GPT-6 Astra, DeepSeek): fill only the
    focused collapsed space; the collapsed line gets its own configurable
    token list (default `workspace, git_status, tab_jobs`); the triangle,
    `+`, grip and counts need readable colours on the accent; hover must
    differ from the focus fill; a collapsed worktree parent whose child is
    focused gets the fill plus a "focus inside" cue, and its git status must
    not pass off one child's as the group's. Git status before the job
    counts; truncate the name first, then drop the git status, never the
    counts. Worktree children keep no branch.
  - Not done: the row-by-row slide animation, a per-job menu on a square
    (open, close), the drag in the multi-machine sidebar (keeps the old
    look), the priority view (no reordering, a hint to switch). A move is
    sent by ids (`move X before Y`); if another client changed the order or
    the anchor vanished, cancel with a notice.
  Decided by the user 2026-10-06: the agent makes demos of the collapsed
  one-line space and the "focus inside" cue for worktree parents, then
  moves the item back under Needs a decision with the variants named, so
  the user picks with a click.

- [ ] "Restart agents…": restart agent CLIs (Claude, pi) after they update,
  resuming their sessions, e.g. when Claude reports that a new version is
  available. Should herdr tell the instances to restart once they finish
  their work? The manual restart exists (`DECISIONS.md`, "Restart agents").
  Decided by the user 2026-10-06: when an update is detected, only mark
  the agents "restart pending"; the user restarts them from the menu; no
  automatic restart.
  - Version: record `claude --version` when the pane starts, compare with
    the binary on disk (mtime only as a hint).
  - Still to do: version detection and the restart-pending mark, a
    preview/picker (how many idle / working / blocked; current agent,
    selected agents or the workspace, showing which support resume), pi's
    draft check. Restart only an idle pane: not blocked, no draft, no
    subagents (`SubagentStop` hook), no jobs; one at a time.

- [ ] Review queue for agent commits, plus `herdr diff`. When an agent's turn
  ends with new commits, list them as "to review" until I acknowledge them.
  Decided by the user 2026-10-06: start with lazygit in a popup on the
  tab's repository, no new code beyond that; a herdr-native list only if
  that falls short. For that list (GPT-6 Astra, DeepSeek, 2026-09-27): the
  unit is the commit (uncommitted changes in the shared checkout cannot be
  attributed); at turn start record the session id and HEAD, at turn end
  find new commits carrying `Claude-Session: <id>`; show each commit's own
  patch, never `git diff first^..last` (other agents' commits fall in the
  range); only an explicit acknowledgement clears "to review". Codex and pi
  need an equivalent of the trailer.

- [ ] Does MiMo earn its slot in the default consult set? (user, 2026-10-06,
  after a consult round on GLM and Kimi, `20261006-221442-cf9e`, where both
  models advised checking this before adding any model.) From consult-stats,
  compare MiMo with Sol over shared rounds (`consult.py stats --vs`):
  accepted unique findings per call, dismissed share, errors, latency. Then
  propose keep, replace or drop, with the numbers, under Needs a decision.

- [ ] Pin a tab: pinned tabs are marked with a pin icon (or similar) in
  the tab bar and stay at its start, before the unpinned tabs, like
  pinned tabs in Chrome or Firefox.
  Decided by the user 2026-10-06: like pinned spaces: pinned tabs come
  first in their space's vertical tab list; server-owned state; Pin/Unpin
  in the tab menu plus a keybinding; a 1-cell glyph in a fixed column; no
  drag across the pinned boundary.

- [ ] Pin a space, like a pinned tab: a pin icon on the space row, and
  pinned spaces stay at the top of the spaces list. Consulted (GPT-6 Astra,
  DeepSeek, 2026-09-28), both agreed on:
  - Pinned first in every sort mode (manual, name, prio); the sort and its
    direction apply inside each tier.
  - A 1-cell narrow glyph (ASCII `*` or `▪`), not 📌 (double width, emoji)
    and no nerd-font requirement; in a fixed leading column, so names do
    not shift when a space gets pinned. The icon is only an indicator.
  - Server-owned session state (like the manual order), in the JSON API;
    the sort mode stays client-only. In the multi-machine sidebar pins apply
    per server.
  - Pin/Unpin in the space's context menu, plus a keybinding; no drag to
    pin. Worktree families are pinned whole; a child's menu says "Pin
    family". Unpinning keeps the underlying manual order.
  - Cost to weigh: in prio an idle pinned space sits above an unpinned
    blocked one; urgent unpinned agents need another cue (the header
    attention counts).
  Decided by the user 2026-10-06: dragging a pinned space out of the
  pinned group is refused, and a separator line divides pinned from
  unpinned spaces; unpin from the menu. Pinned spaces come first in every
  sort mode, as consulted.

- [ ] Audit whether colours and symbols are consistent across the UI
  (sidebar, mobile layout, tabs, toasts, job statuses `⧖ ✓ !`, state dots).
  Consulted (GPT-6 Astra, DeepSeek, 2026-09-28): one colour meaning
  different things in different contexts is not automatically a conflict;
  check it without colour, in light and dark themes and narrow layouts.
  - Audit (2026-09-28), conflicts by severity:
    1. `Done` is teal in `status_color` (`src/client/shell.rs`) but blue in
       the mobile summary (`mobile.rs`) and finished toasts
       (`notifications.rs`); in most themes blue equals `accent`.
    2. The Dots style draws working, blocked, done and waiting-on-job all
       as `●`: colour alone tells them apart.
    3. Blocked has three glyphs: `●` (Dots), `×` (Symbols), `◉` (mobile);
       other red problems use `!`.
    4. `◐` is both agent working and endpoint connecting, both yellow.
    5. Green is both agent idle and job succeeded; `✓` is also agent done
       (Symbols style, teal).
    6. Theme collisions: in `Palette::terminal()` mauve equals overlay0
       (waiting-on-job looks unknown) and peach equals yellow; in Dracula
       blue equals teal.
    7. Mauve means waiting-on-job, focused branch, resize mode and help keys.
    8. Unknown reads "unknown" in `status_text` but "idle" in the agent
       sidebar and mobile.
  - Duplicated mappings: status text (`shell.rs`, `agent_sidebar.rs`,
    `mobile.rs`), glyph and colour overrides in `mobile.rs`, job glyphs and
    colours in `tab_groups.rs`, `tabs.rs` and `ui/sidebar.rs`, toast colours
    twice in `notifications.rs`. Model to follow:
    `endpoint_status_presentation` (`endpoints.rs`) returns glyph, label
    and colour together. Next: one `status_style` module per domain (agent,
    job, endpoint, notification) and semantic palette roles.
  Decided by the user 2026-10-06: the agent makes demos of a few palette-
  role variants (e.g. Done teal vs blue) for the user to view in a tab,
  then moves the item back under Needs a decision with the variants named,
  so the user picks with a click; the legend item waits for that choice.

- [ ] No view of how much memory and CPU spaces, tabs and jobs use (user,
  2026-10-03). Round `20261003-163010-4ae0` (sol, MiMo).
  Decided by the user 2026-10-06: start with a `herdr top` prototype
  computed from `ps` and each pane's process tree, with no new API
  contract (space > tab > pane totals, process count, CPU, memory, sample
  age; `--sort cpu|mem`, `--json`, `--watch`); the native sampler and
  `resources.snapshot` wait. Constraints for both:
  - One process enumeration per tick for the whole machine, one parent
    graph, each `(pid, start time)` assigned once; never one walk per pane.
  - Attribution: the pane's PTY child tree plus processes still holding its
    controlling tty; daemons that escaped (cargo build server,
    rust-analyzer, docker) go into a `shared / unattributed` row, never onto
    a pane. herdr's own server and clients get their own row. Jobs are tabs,
    not a separate bucket.
  - CPU: per-process deltas before summing, labelled "% of one core". Not
    `ps cputime` on Linux (whole seconds).
  - Memory: macOS `phys_footprint` labelled "footprint", Linux RSS labelled
    "RSS" (the `ps` prototype: an estimate); never mix metrics in one total,
    never call a sum "memory freed by closing this space".
  - The native sampler later: server-owned, runs only while subscribed,
    pushes `resources.sampled`, measures its own cost.
  - Tests: aggregation over a synthetic process graph (reparenting, pid
    reuse, a shared daemon, tty holders).

- [ ] Track which buttons the user never clicks, to drop them from roherdr
  (user, 2026-10-07: "it would be useful to somehow track which buttons I
  never click at all, so maybe I can throw them out of roherdr? like
  telemetry? ask the models"). Consult the default set first (local-only
  counters vs. anything sent out, where to store them, how to show the
  never-clicked list), then propose the design.

- [ ] A standing self-improvement process over agent sessions (user,
  2026-10-07: "today's analysis of all Claude sessions was good, that most of
  the time is waiting for my decision. Also include pi in the analysis, and
  maybe other agents when I use them. A standing self-improvement process,
  but that's probably a separate TODO. Ask the models"). Find today's
  analysis and its script first, add pi's session files
  (`~/.pi/agent/sessions/`) and a per-agent reader so other agents can join,
  then consult the default set on making it a recurring process (how often,
  what it reports, where findings go).

- [ ] No tab line is lit for the focused tab of a collapsed worktree space
  (user, 2026-10-07, three screenshots: "why does this session have no
  highlighted tab? probably opened by Claude as a todo-worker; ask the
  models"). The focused pane was `worker: shuffle prev`, a worktree space a
  coordinator created (`herdr worktree create --no-focus`) nested under
  `rormpc-tools`; its name line shows `▶` (collapsed), so no tab line and no
  highlight anywhere in the sidebar marks where the user is. Check how
  collapse is chosen for API-created worktree spaces and what a collapsed
  space should show when it holds the focused tab; consult the default set.

- [ ] Navigation history survives a client restart (user, 2026-10-07: "the
  navigation history is cleared after a client restart, I can't go back").
  The header's back/forward (`focus_history.rs`) lives in client memory, so
  every reattach, and every install's live handoff, empties it.

- [ ] A coordinator waiting on a busy worker looks idle (user, 2026-10-07,
  screenshot: "this circle is grey, it looks as if the coordinator is not
  working"). Its only running job is `herdr agent wait <worker pane>`: no
  output and no CPU, so after 5 minutes herdr-job reports it `--activity
  idle` and the coordinator's state shows the grey still ring (`◌`, `z` once
  the uncommitted idle-mark change lands), while the worker it waits on
  works (`◐` next to `⚒`). A wait is idle by design; its liveness is the
  awaited target's. Options: herdr-job never marks a wait job (`agent wait`,
  `watch --pid`, `pane wait-output`) idle, or reports the awaited agent's
  state instead of its own CPU and output.

- [ ] Coordinators present "Needs a decision" questions as clickable
  options (user via the try-roguix coordinator, 2026-10-07: it moved four
  items there and only mentioned them; "fix the process so a coordinator
  that records questions also presents them as clickable options at once or
  at a defined point; ask the models"; the user: "does the collector have to
  be in herdr? earlier you also said something had to be in herdr and the
  /todo skill was enough"). The herdr coordinator did the same with three
  questions on 2026-10-07. Consult rounds `20261007-025516-0631` and
  `20261007-025602-9f40` (sol, MiMo). Both: no herdr collector needed now;
  TODO.md is already the durable state; a script plus a `/decisions` skill
  first, herdr only after lost updates or unseen questions are observed.
  - Each question carries its options in the TODO line (`Options: a | b |
    c`); a skill never invents choices (MiMo); a short id or hash lets the
    asker re-read and skip a question changed meanwhile (both).
  - Timing in the `/todo` skill: right after delegating an item (the worker
    runs meanwhile), ask the new questions with the multiple-choice tool, 4
    per call; if unanswered or on the phone, plain text plus `herdr agent
    awaiting-reply "N decisions"`, which already shows in the `?` list, so no
    herdr badge is needed (MiMo wanted a badge for visibility).
  - Answers are written back as "Decided by the user <date>: ..." and the
    item moves to "Next, in order" only on an explicit approval; writers
    re-read before writing and commit `TODO.md` by path (both). Coordinators
    re-read `TODO.md` before each item, never from memory (MiMo).
  - Done 2026-10-07 (user chose it from a menu): the global rule and the
    `/todo` skill (dotfiles `9b851dd`) require `Options:` on each question,
    ask new ones right after starting the next worker, fall back to plain
    text plus `awaiting-reply`, write answers back, re-read `TODO.md`; the
    false "herdr collects them" is gone. Left: the cross-repository script
    plus `/decisions` skill, and the test below.
  - (sol, before:) The global rule's "herdr collects them from there" was not
    true and had to go. Fix the coordinator stopping between items first, or
    stalls get blamed on the wrong change (both, round 1).
  - Test: a fixture repo with one trivial item and two questions, no user
    present: the item still lands and both questions are asked once; an
    answer written back is not asked again after a restart.
  - Second rormpc report (2026-10-07, forwarded by the user; "fix the
    process, consult the models"): its coordinator piled up 13 questions in
    "Needs a decision" and never asked them; with "Next, in order" down to
    one item it waited on a worker and reported, so work would have
    stopped; only after the user asked "is nothing left? why don't you hand
    it to workers?" did it ask 4 in a menu, which unblocked 3 items at once.
    Expected: ask as soon as they are added, at the latest while a worker
    runs, so "Next" never runs dry while answerable questions wait; record
    answers and queue the work. The rule of `9b851dd` covers only new
    questions, not a backlog of old ones without `Options:`.
  - Other languages (user, 2026-10-07: "what if I also start in Odin? other
    rules than for Rust"): how workers build (worktree or not, a shared
    cache) depends on the repository; it belongs in each repository's
    AGENTS.md, the global rule stays language-neutral.

- [ ] Do the consult popups need `less`? (user, 2026-10-03: "less used in
  consult stats? we have Rust. ask the models"). `page-consult` pages
  `consult.py` output with `less -R`; a popup is a real PTY pane
  (`spawn_popup_command`, `src/app/popup.rs`). Round `20261003-022724-693a`
  (Sol, DeepSeek, MiMo), unanimous: keep `less` for now. Rejected: a herdr
  pager subcommand (rebuilds `less` inside a PTY), rewriting `consult.py
  stats` in Rust inside herdr.
  - First step, a spike: a temporary popup with `command = ["seq", "1",
    "300"]`. Does it keep scrollback, scroll with the wheel, start at the
    top, get SIGWINCH on resize? If yes, drop `less` from `page-consult`
    (print, then wait for Enter; no `/` search). If popups do not scroll,
    that is a herdr defect worth fixing on its own.
  - Later, only if several plugins want it: a manifest text popup whose
    command's stdout herdr renders itself (a new pane type).
  - Known limit either way: tables are fitted to the width at launch.
  Triage 2026-10-06 (manual): The first step is a live popup spike (scrollback, wheel scroll, start at the top, SIGWINCH) that needs you watching the real UI; a popup steals focus.
  Decided by the user 2026-10-07 ("I don't know. A popup spike?"): do the
  spike; a worker prepares the test popup, the user watches it scroll.

- [ ] Phone notifications when I am away from the Mac (agent blocked,
  agent done, herdr-job finished).
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Sol, 2026-09-26): a
    Telegram bot sending to my private chat (`chat_id`); Instagram rejected
    (no API for it). Bot chats are not end-to-end encrypted. ntfy or
    Pushover as alternatives.
  - A plugin, not core: agent blocked/done comes as
    `pane.agent_status_changed` through plugin `[[events]]` hooks; herdr-job
    completion has no event, so herdr-job's `notify()` calls the plugin's
    sender. Bot token and `chat_id` in the plugin config, never in payloads.
    Transitions only; dedup per pane and approval request, coalesce bursts,
    drop a stale alert.
  - Away: read it from the OS (macOS `ioreg -c IOHIDSystem` `HIDIdleTime`,
    Linux logind's `IdleHint` or `xprintidle`) plus a manual away/mute
    action writing a state file; the API knows nothing about idle clients.
  - Content: the toast text (`claude finished` plus `workspace · 1 · tab`):
    no paths, prompts or agent output. Later: approve/deny buttons answered
    through the herdr socket, accepting callbacks only from my own user id.
  Triage 2026-10-06 (manual): The spike is done; give a bot token and `chat_id` so sending can be tested.
  Decided by the user 2026-10-07: an Android app, ntfy (no account or
  token: a topic the user subscribes to in the ntfy app). The plugin design
  above stands, with ntfy instead of the Telegram bot; Telegram rejected for
  now.

- [ ] `scripts/fork_demo/README.md` still says "oracle stats" where the menu
  item is "consult stats" (left over from the dropped README animations).

- [ ] Force-quitting a quit Ghostty killed ~19 Claude agents in herdr panes,
  and their `?` marks did not come back after `claude --resume` (user,
  2026-10-03, screenshots of job-seeker and email-assistant showing "Resume
  this session with:"). At 15:17:35 loginwindow opened the Force Quit panel
  for Ghostty "because it still has background processes"; at 15:17:39-40
  ~19 `claude` processes exited gracefully at once, while their zsh shells,
  the herdr server and ~12 other claude survived. Auto-resume of such agents
  is done (`DECISIONS.md`, "Auto-resume of agents stopped by a signal").
  - Cause: the herdr server, every pane shell and agent sit in the resource
    coalition of the Ghostty that started the server
    (`proc_pidinfo(PROC_PIDCOALITIONINFO)`); live handoff inherits it. Why
    ~12 agents in the same coalition survived is unexplained.
  - Not done: reproduce with disposable agents (quit Ghostty, force-quit
    its entry, record the signal in a wrapper, `sudo launchctl procinfo` on
    the server, shells and agents before and after); the fix waits for it.
  - Fix, if the reproduction confirms it (sol, MiMo, `20261003-153704-1042`):
    start the macOS server as a per-user LaunchAgent (own coalition):
    `launchctl bootstrap gui/$UID <plist>`, then `kickstart` (never `-k`),
    wait for the socket; `RunAtLoad=false`, `ProcessType=Interactive`, no
    plain `KeepAlive=true`. Costs: send the client's `PATH`,
    `SSH_AUTH_SOCK` and the like per new pane; TCC grants move to herdr
    (sign with a stable self-signed identity); `bootstrap` fails over SSH,
    keep direct spawn as fallback; start the handoff successor through
    launchd too, or exec in place; existing panes stay in the old
    coalition. Rejected: `responsibility_spawnattrs_setdisclaim`,
    `posix_spawnattr_setcoalition_np`, `launchctl submit`.
  - Gaps of the auto-resume: Windows (the PowerShell hook does not report
    the stop), SIGKILL (no hook runs), and the limit and the `?` are not
    saved across a server restart (persist the report keyed by session id,
    restore only when that session resumes with no user prompt after the
    question). Consider a "the agent exited, resume?" hint on panes whose
    agent died.
  - Until then, the user's side: do not Force Quit a "Ghostty" entry that
    shows up after Ghostty has quit; Cmd+Q is enough.
  Triage 2026-10-06 (manual): Auto-resume is done (b2f3adb9, 3a8e7f66); reproduce by quitting and force-quitting Ghostty with `sudo launchctl procinfo`; the LaunchAgent fix waits for that.
  Decided by the user 2026-10-07: reproduce first; a worker prepares the
  script with disposable agents, the user quits and force-quits Ghostty once.

- [ ] An `×` that clears the spaces filter field (user, 2026-10-07: "in the
  filter for searching tabs and spaces, add some x to clear the field").

- [ ] The space lines are hard to read (user, 2026-10-07, screenshot of the
  sidebar: "this can hardly be read; propose something, ask the models, show
  nice visualisations, in the browser?"). Every space name line carries the
  branch, `↑N`, job counts and three buttons (`❏ T A`); nested worktree
  spaces cut their names to a few letters ("worker: re…", "try-roguix AX…").
  Consult the default set, then show variants as browser mockups and let
  the user choose.
  Consult round `20261007-043713-4b75` (sol, MiMo); mockups (artifact):
  https://claude.ai/artifact/N1pThPneSMEGcsq5XQhctp. Variants: A buttons only
  on hover, B one `⋯` per space (MiMo), C a quiet list with one action bar at
  the bottom (sol), D two-line space headings. Both: counts once, on the
  line that owns them; worker lines show the role mark and branch, never a
  cut name.
  The user rejected all four (2026-10-07): "none. Make it so a space always
  has a coordinator, and clicking the space title opens its tab. Do not nest
  worktrees. Clean the UI of unneeded things. Consult the models, maybe even
  Astra."
  Round `20261007-044327-2f1f` (sol, Astra, MiMo), agreeing: "always a
  coordinator" means a permanent destination, not a running process: the
  title click opens the coordinator's tab without starting Claude (no
  quota, no trust prompt); the agent starts on an explicit Start TODO;
  `❏ T A` leave the rows (menus, coordinator); branch and worktree path go
  to the worker's own view. Split: workers as rows under the space named by
  their task (Astra, MiMo) or only "↳ N worker" (sol); role marks and
  header totals removed (sol, Astra) or kept (MiMo); idle tabs folded (MiMo)
  or never hidden (sol, Astra); spaces without a TODO get no coordinator
  (MiMo) or an overview with "set up coordinator" (sol, Astra). Mockups E
  and F added to the artifact.

## Proposed

Items agents add. Not approved until the user moves them up.

- [ ] herdr: `agent start` reports ready before Claude accepts typed input,
  and `agent prompt` returns `agent_prompted` without knowing the prompt
  arrived (2026-10-07: the bussiness-ideas coordinator's first prompt was
  lost; workers' prompts too until the coordinator resent them). A
  structured `awaiting_user_action` state from `agent start` instead of
  `agent_not_ready` for a startup prompt (sol, round
  `20261007-040046-b1c7`). The launcher (dotfiles `140fc7f`) works around
  both: it waits up to 10 minutes for the user to answer Claude's trust
  prompt and resends the first prompt once unless the agent turns working.

- [ ] Agents name tabs by ids the user cannot see (user, 2026-10-07,
  screenshot of the `?` list: "how do I know which tab that is?" for
  "approve the edit in tab w4:t6Z"). The sidebar shows space names and tab
  labels, never `w4:t6Z`. A coordinator (and any agent) should name a tab
  as the user sees it, space plus label (or the worker's role mark), and a
  worker that needs the user should be the one marked, so its own tab shows
  `?` instead of the coordinator's. Options for herdr: a `herdr tab focus`
  link in the ask, or a click on the `↳` line that jumps to a tab named in
  it.

- [ ] A working Claude agent's detected state flickers to done/idle. 2026-10-07:
  `herdr agent wait <worker> --until done --until blocked` returned twice
  while the worker kept working (its screen showed "Thundering…" with a
  running shell, `agent get` said working right after), so a coordinator
  waiting on the state alone reviews a worker that is not finished. Capture
  the detection screen at the flicker (`herdr agent explain --json`) to find
  which rule matches between tool calls. The coordinator's workaround: wait
  for the `WORKER-` line, debounce done/idle for 120 s.

- [ ] A live handoff breaks other sessions' waits. 2026-10-07: each
  `scripts/herdr_live.sh install` restarts the server, and the try-roguix
  coordinator's `herdr pane wait-output` on its worker failed with
  `server_unavailable` ("server is shutting down"); it then wrapped the wait in
  a retry loop of its own. CLI waits (`pane wait-output`, `agent wait`) could
  reconnect across a handoff instead of failing.

- [ ] Coordinator gaps reported by the rormpc coordinator (todo-rormpc,
  2026-10-07, forwarded by the user). After the user answered two "Needs a
  decision" questions with "do it" (release rormpc-tools, install rormpc, add
  config lines), the coordinator bumped the version, committed and tagged
  itself; the user asked why the coordinator worked itself. The global rule
  "Working through TODO.md" does not say: (1) whether an answer to a "Needs a
  decision" question is a new request to queue first or an approval to act
  now; (2) who does release steps workers may not do (push, tag, install,
  editing the user's config after an install): the coordinator, a worker
  with that permission, or the user; (3) a rule changed mid-session reaches
  a running coordinator only partly (it had already done one item itself).
  Also seen: `herdr agent prompt` right after `herdr agent start` in a fresh
  worktree was swallowed by Claude's folder-trust dialog (`agent_prompted`
  returned, the prompt never arrived) until `herdr worktree create` got
  `--trust-repository`; `herdr pane wait-output` has a short default timeout,
  so a herdr-job waiting for a worker's last line failed early.
  The herdr coordinator showed gap (2) too (user, 2026-10-07: "why do you
  make changes yourself when you are the coordinator?"): after the rule
  change it still edited itself the global rule and `/todo` skill (about 15
  lines), a code fix (the role mark's gap, with tests, build and install), a
  flaky test, AGENTS.md notes and the launcher, reading "few-line fixes" as
  covering them. The rule needs a line saying what the coordinator does
  itself (TODO.md and DECISIONS.md edits, triage, answering, integrating
  workers' commits, installs and pushes it is approved for) and that
  everything else, code, tests and rule text included, goes to a worker,
  however small.
  Done 2026-10-07 (user: "plug this gap"), by a worker: dotfiles `a24546d`
  lists what the coordinator does itself and sends everything else to a
  worker, also after a "do it" answer (gaps 1 and 2 above). Left: gap 3 and
  the two herdr observations.
  Also (user, 2026-10-07): a worker needs its own worktree for code
  changes; for documentation and rule text in files nobody else is editing,
  the shared checkout with a commit by path is enough. Seen the same day: a
  dotfiles worktree had to be made by hand (`herdr worktree create` does not
  see the home repository, whose git dir lives elsewhere), and Claude
  stopped in it on the folder-trust prompt.
  Build cost of a worktree per worker (user: "won't the worktree slow down
  building the Rust code? ask the models"; round `20261007-030604-5a6b`,
  sol and MiMo). Measured 2026-10-07: one worker's cold `target/` reached
  5.2 GB, clean-check's is 10 GB, 25 GiB were free (95% used) against the
  15 GiB guard, so disk binds first (MiMo). Both: one shared worker
  `CARGO_TARGET_DIR` (workers run one at a time), clean-check kept separate
  as the correctness gate, shared-checkout work rejected for code. MiMo:
  Cargo fingerprints path dependencies by absolute path, so a new worktree
  path rebuilds `crates/ghostty-vt` (Zig) and `vendor/portable-pty`
  (verified: both are path deps; Zig's own `~/.cache/zig` may soften it).
  My addition: one persistent worker worktree at a fixed path, reset to
  `master` per item like clean-check, keeps paths stable. Fallback (MiMo):
  APFS clones (`cp -Rc`) of a warm target. Measure first: `cargo build
  --timings` cold, `du` of `target/{debug,incremental,nextest}`, `cargo
  check -v` after switching worktrees (dirty units, did build.rs rerun).

- [ ] `workspace list` shows a space's repository only when the space is in a
  worktree family (its `worktree` object), so tools read `.worktree` as "the
  space's repo" and fail for plain git spaces (2026-10-07: the todo launcher
  found no repo for job-seeker). Expose the cached `git_space()` on every
  space as its own object (`repo_root`, `repo_name`, explicit null for a
  non-git space) and document `worktree` as worktree-family membership only
  (consult round `20261007-015141-b75b`, MiMo).

- [ ] `herdr tab rename <tab> ""` leaves an empty custom label instead of
  clearing it, so the tab shows nothing rather than its automatic name (the
  agent's task or the terminal title); there is no way to return to the
  automatic name (2026-10-07: the todo-worker launcher's `--label TODO`
  hid the coordinator's task, and clearing it left a blank row).
  `handle_tab_rename` calls `set_custom_name(Some(label))`; an empty or
  whitespace-only label should clear it (`None`), in the TUI rename too.

- [ ] `herdr-job clean-tree` should refuse a path that matches nothing.
  2026-10-07: from zsh, `clean-tree $PATHS -- just check` with `PATHS="a b"`
  passed one path with spaces (zsh does not split words); the tree got
  "0 changed paths" and the check ran on bare `HEAD` without a warning.

- [ ] Replace job pinning and folding with an explicit pin (user,
  2026-10-06: "I don't like this pinning of herdr jobs and the whole
  folding logic. Throw it out."). Remove the automatic attachment of job
  rows to their parent and the fold/unfold logic for job squares (see
  `DECISIONS.md`: "Unfolding job squares near the bottom", "Auto-unfolded
  job squares", "Fold state of job squares across client restart",
  "Folded job squares and focus"). Add only: right click on a job, "Pin"
  in its context menu. A pinned job stays visible the way it is shown now,
  perhaps with an icon that acts as an unpin button, and "Unpin" in the
  same context menu. Before starting, record in `DECISIONS.md` which
  earlier fold decisions this supersedes.

- [ ] Flatten worktree spaces instead of nesting them under the creator
  tab (user, 2026-10-06: "why is a worktree tab indented and with
  different logic? Could it be flattened and just marked as a worktree?"
  Example: this session created the worktree space "pi: tab create" for a
  delegated pi agent, and that space's agent starts job tabs of its own).
  This reverses "Worktree space nested under its creator tab" in
  `DECISIONS.md` (user, 2026-10-03/04); the user decides. Consulted Sol
  and MiMo: both recommend flattening. A nested space is the only row
  type with its own header (no name bar, no git details, branch label),
  family drag/pin, a fold toggle and job counts aggregated into the
  parent; space -> tab -> worktree -> tab -> job is deep and narrows the
  mouse targets. Proposal: a worktree space is an ordinary top-level row
  (same header, git details, job counts, collapse, independent drag/pin)
  with a worktree glyph and a secondary `from herdr/7` reference that
  jumps to the creator tab (or reads as history when it is gone);
  `creator_tab` stays as provenance, not hierarchy. A UI-created space
  still lands right after the active space's family, which then is just
  its worktrees. Rejected: nesting as a config option (two navigation
  models, double layout and tests). Note: a nested space already shows
  its own running/failed job counts (`workspace_rows` gets `tab_jobs`);
  the parent's counts include a folded worktree's jobs, which flattening
  removes.
  Screenshot (2026-10-06): tab `Koordynacja pi: tab…` with its job child
  `wait: pi tab create` and the parent's job count, then `└ ▼ pi: tab
  create` (worktree space, `□ A` on the right) and its pi tab `π - You are
  working i…`: three levels of indent for one delegation, and the
  worktree's own line shows no job count until its agent starts a job.

- [ ] Header `?` list: tell the kinds of waiting apart and use the row's
  agent, not the tab (user, 2026-10-06: "now you cannot tell which tab is
  a plain `?` question and which is a question with a choice"; agreed to
  the mockup). Rows draw `tab_state_icon`, the tab's dominant state, so in
  a split tab a pi asking a plain question shows the `×` of a Claude
  blocked on a choice next to it. Proposal (Sol, 2026-10-06): the icon of
  the row's own agent, with `?` awaiting reply, `×` blocked on an
  approval or choice (existing glyph, `src/client/shell.rs:206`) and `!`
  limited, plus the text detail so colour is not required. The sidebar
  tab line keeps the aggregate icon. Separate from the icon column bug
  fixed on branch `pi/list-icons` (`(.., icon)` bound `detail` after
  7f55e2c5).

- [ ] Every header button looks like a button (user, 2026-10-06: "all
  buttons in the top bar should be like buttons: a space on the left and
  right, a highlight on hover"). Consulted Sol (read the code) and MiMo.
  - Padding: one space each side, both cells part of the button's paint
    and click rectangle; always reserved, never only on hover (the row
    would jitter). Agent indicators already reserve two cells
    (`sidebar.rs:496`); sort has a glyph-only hitbox (`space_sort.rs:121`),
    so this is partly a consistency fix. Rejected: one space shared by
    two neighbours (MiMo; Sol: a cell cannot belong predictably to two
    buttons).
  - Styles: hover a subtle neutral background; an open list keeps its
    accent tint and glyph colour (`DECISIONS.md`, header sort button);
    open and hovered a slightly stronger tint. Not bold alone (asking
    counts are already bold). Check indexed-colour themes, where the open
    style falls back to solid accent (`sidebar.rs:1910`).
  - Hover: clear it on focus loss (`input.rs:256`), overlay change and
    re-layout; repaint only when the hovered button changes
    (`endpoint_navigation.rs:91`); without motion reporting only hover is
    lost. Pointer exit without an event cannot be detected.
  - Overflow, when the padded buttons do not fit: shorten the sort key to
    its first letter (`⇅ m↑` for manual, `n` name, `p` prior…; user,
    2026-10-07: the full name shows on click, in the sort menu), then to
    `⇅` alone, then move disabled navigation, fold and zero-count history into
    an overflow menu, a second row as last resort. Bug found on the way:
    `sidebar.rs:482` silently skips a non-zero indicator when it does not
    fit, against "never hide a non-zero indicator".

## Needs a decision

Moved here in the 2026-10-06 triage: each item's last line states what the
user needs to decide or do.

### Decide

- [ ] A legend explaining the UI's dots and symbols (agent state dots,
  job counts like `!2` / `⧖ 1` / `✓3`, git tokens `↑4` `±7`, endpoint
  states, sort buttons, the grip, footer provider codes). A status legend
  for agent and job states exists (`DECISIONS.md`, "Status legend",
  `src/client/shell/status_legend.rs`); the other domains are open.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): a full modal,
    never a bare `?` (it belongs to the agent's terminal); hover tooltips
    only as a supplement; no first-run hint.
  - Content per domain (agent, job, git, endpoint, controls, usage):
    glyph, label, one-line meaning and a swatch in the theme's actually
    rendered colour, never a colour name. Explain the counts by example
    (`!2` two failed jobs, `↑4` four commits ahead); define what `±7`
    counts (files or lines); never describe planned glyphs as current.
  - Generated from the per-domain `status_style` mapping of the audit item,
    with a test that every state variant has an entry. Unknown states from
    older remote servers show as "unknown status". DeepSeek: an "on screen
    now" filter; a warning when the theme gives two states the same colour.
  - Order: DeepSeek after the consolidation and the shape redesign; Astra
    together with the consolidation (my preference: a generated legend
    follows the redesign for free).
  Triage 2026-10-06 (depends): Generated from the per-domain `status_style` mapping of the item above (colours and symbols audit); the ordering also needs your confirmation.
  Decided by the user 2026-10-07: after the colours and symbols audit,
  generated from its style map; waits for that audit.

### Needs you to act or watch

