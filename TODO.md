# TODO

Open work only. A finished item leaves this file: its durable
decisions go to `DECISIONS.md`, the rest stays in the commit messages.
Parked ideas live in `TODO-deferred.md`.

## Next, in order

- [ ] `herdr tab create` without `--workspace` from inside a pane should
  create the tab in the caller's workspace (`$HERDR_WORKSPACE_ID`), not in
  the workspace the user is looking at. On 2026-10-06 a Claude session in
  music-mpd ran `herdr tab create --no-focus --label rormpc-test` while the
  user had herdr in front, and the tab landed in herdr
  (`src/app/api/tabs.rs:71` falls back to `state.active`). Default it in
  the CLI (`src/cli/tab.rs`), so calls from outside Herdr keep today's
  behaviour; this departs from upstream. Check the other create commands
  that fall back to the active workspace the same way.
- [ ] Tell agents to pass `--workspace "$HERDR_WORKSPACE_ID"` (or `--parent
  "$HERDR_TAB_ID"`) when they create tabs, in the herdr skill/integration
  guidance, as a belt-and-braces for servers without the fix above.

- [ ] Mark a pane whose agent ended its turn waiting for the user even when
  it could not run `herdr agent awaiting-reply` (user, 2026-10-06: why was
  the window not marked after the agent stopped with "I stopped halfway: the
  automatic permission check stopped answering and blocks every edit and
  command"). A Claude session in music-mpd ended its turn asking the user to
  write "dalej" (continue); Claude Code's auto-mode classifier gave no verdict
  for every Bash/Edit call, so the agent could not run the awaiting-reply
  command and the pane looked finished. The marker today depends on a tool
  call the agent makes; it needs a path that does not: e.g. the Stop hook
  (already installed by `herdr integration install claude`) marking the pane
  when the turn ended right after failed or blocked tool calls, or showing
  such a pane as "stopped with an error" instead of idle. Check what the hook
  input carries about the last tool results before choosing.

- [ ] Show what an agent asks, not only `?` (user, 2026-10-06: "some list
  where I see what the agent asks? now I only have a question mark"; queued
  next). Inspired by posts praising the T3 Code and Devin sidebars
  (https://x.com/kr0der/status/2107037327575208337: rows like "Approve
  phase 1" or "PR is ready" instead of a title and a coloured dot). Today the
  header's `?N` list names the tabs that wait for a reply, not the question.
  ```
   ⇅ manual        ◐3 [?2] ✉1
   ┌───────────────────────────┐
   │ ? Read the X post         │
   │   ↳ add it to TODO?       │
   │ ? Fix OAuth callback      │
   │   ↳ Install now?          │
   └───────────────────────────┘
  ```
  - First slice: `herdr agent awaiting-reply` takes an optional short text
    (the question in a few words); the integration hook asks for it. The
    text lives and is cleared with the `awaiting_reply` flag (the next user
    prompt), so no stale questions. The server caps it at ingest (about 40
    characters on a grapheme boundary, control and ANSI sequences stripped),
    never per frame. The `?` list draws it as a dim second row. Bump the
    Claude integration version once from the latest release.
    Done 2026-10-06 together with the inbox and Limited from "Orchestration
    direction": `pane.report_awaiting_reply` takes `question`; agents
    report `waiting_since_ms` (blocked, asked, limited) and the `?` list
    ranks by it, longest first, the wait (`3h`, `2m`) in the time column,
    the ask as a dim `↳` line (blocked: the hook message or `approval`).
    `pane.report_limit {kind: usage|credits, message}` (CLI `herdr agent
    limited`) from Claude's `StopFailure` hook (`rate_limit`,
    `billing_error`); shown like an awaiting-reply report and counted in
    `?N`; `resets_at` is the latest reset of the provider's full windows
    in the usage report (a limit report refreshes it). Integration stays v12
    (unreleased since v11). Open: a separate header count for limited
    agents, the second line under sidebar tab lines, limits for Codex/pi.
  - Later: the same second line under `?` tab lines in the sidebar (only
    for asking rows; working rows stay one line), and a "needs me" filter
    in the planned sidebar filter bar, never hiding rows by default
    (hiding breaks positional `Alt-1…9`, focus, and job child tabs).
  - Consulted sol and MiMo (round `20261006-021127-0eb7`). Agreed: keep the
    task title as the row's identity and put the ask in a second line (sol;
    rejected MiMo's ask replacing the title: three OAuth tabs become
    indistinguishable); explicit reporting, not a heuristic over
    `last_assistant_message` (preambles, the question in paragraph four,
    raw text landing in server metadata); server-side expiry. Open: sol
    shows outcomes ("ready for review") apart from actions, MiMo would not
    show outcomes at all (stale within a day); if outcomes come, show them
    only until the tab is viewed.
  - More users asking for it (2026-10-06, replies to the post above): "a
    colored dot tells me something's running, not that it's waiting on me
    ... most of my lost time with agents is hunting for the chat that's
    stuck on a yes" (@haonv2); Shika gives every card a second line in
    words: the CLI, the status, the branch, and the diff stat once it is
    ready to check (@hieuspringle). Candidate for a done row later: branch
    and `+N -M`, facts herdr can read itself instead of agent prose.
  - How T3 Code does it (read 2026-10-06, pingdotgg/t3code `4df84a7d`): a
    three-line card (project and status word, title, branch/PR/providers);
    status words Working, Waiting, Approval, Input, Limited (a usage limit,
    apart from Failed), Failed, Done (`resolveSidebarThreadStatus`,
    `apps/web/src/components/Sidebar.logic.ts:977`). The question text is
    not in the sidebar, only the label; it shows in a panel above the
    composer. Working rows are dimmed, an optional "working shelf" folds
    them away, and the inbox sorts by when a thread last came back to the
    user. Its diff stat is a stub (`latestRunDiff()` returns null,
    `Sidebar.tsx:2161`). Worth taking: a `Limited` state, which also tells
    when to hand a session over (see below).
  - Diff stat source, if it comes: T3 snapshots the tree at each turn end
    into hidden refs (`refs/t3/checkpoints`, through a temporary
    `GIT_INDEX_FILE`) and takes `git diff --numstat` between consecutive
    snapshots, so the stat is per turn, not the whole tree. In the shared
    checkout it would still count concurrent sessions' edits.

- [ ] Hand a session over to another agent (user, 2026-10-06: "the handoff
  would help, now I have to paste a link to the pi or claude session by
  hand"; queued after the `?` list above). Inspired by
  https://x.com/MahyadGhassemi/status/2107190376692056222 (T3 Code switches
  models mid-chat, useful when a usage limit runs out).
  - Idea: a tab menu item "Hand over to… Claude / Pi / Codex" opens a new
    tab in the same cwd with the chosen agent and a first prompt naming the
    source agent, its session id, its transcript path and the task title:
    "Continue the work from <agent> session <id>, transcript <path>, task
    <title>; read it first". Herdr already keeps agent session ids for
    resume (`src/agent_resume.rs`); the transcript path is derived per agent
    (Claude `~/.claude/projects/<cwd slug>/<id>.jsonl`, Pi's session file;
    verify both). The new agent reads the transcript itself, so herdr never
    parses private transcript formats, and it works after the source agent
    hit its limit.
  - Open: whether the source tab stays (likely yes, idle), a CLI/API form
    (`herdr agent handoff <pane> --to pi`) as a neutral server method, and
    what to do when the session id is unknown (say so, do not guess).
  - How T3 Code does it (2026-10-06): it drives agents through their
    protocols (Claude Agent SDK, `codex app-server`, `pi --mode rpc`, ACP),
    so it owns the event stream. On a provider switch it replays selected
    items, not a model-written summary: user and assistant text, commands
    with output, errors, changed file names, plans, within a 16k-token
    budget (`T3CODE_CONTEXT_HANDOFF_TOKEN_CAP`), only the delta since the
    target last saw the thread; natively for Codex (`thread/inject_items`),
    else as text before the user's message. Herdr has no event stream, so
    the new agent reading the transcript stays cheaper; reuse T3's list of
    what to carry over as the instruction in the first prompt. Subagents:
    T3 hides them from the sidebar too (shown in the parent's lineage
    panel; a terminal child wakes the parent with a synthetic message), so
    showing them in herdr is not urgent.
  - From the Fellowship post (below): keep it a provenance pointer (source
    agent, session id, transcript, repository, revision, time), not a
    shared context; the post moves context between sessions only "when it
    is useful".

Three gaps found in https://spznrf.dev/blog/the-fellowship-of-the-pane
(2026-10-03: a user runs five Pi agents in five visible Herdr panes, a main
session that writes all code and delegates to documenter, reviewer, qa and
ops-recon through `herdr agent prompt`, one of them on a remote host).
Consulted sol and MiMo twice (2026-10-06, rounds `20261006-023800-a481`
and `20261006-030215-b8ca`); both put the first two at the top.

- [ ] Resolve agent names within the caller's workspace first. Today
  `resolve_agent_target` (`src/app/terminal_targets.rs`) matches
  `agent_name` in every workspace, so the post names agents
  `<workspace-id>-<role>` and its main profile has to say "verify that each
  target belongs to the intended workspace and project. Never substitute an
  unnamed, focused, or unrelated agent." Keep an explicit way to address a
  name globally; an ambiguous name stays an error. Risk: callers that rely
  on global names from another workspace (sol).

- [ ] `herdr agent prompt --wait` that waits for the turn it started. Its
  help says "It does not track turns: if the agent is already working, that
  active turn's completion may match", and the post's whole delegation runs
  on it. Return a request id and report accepted, working and finished for
  that request. Screen detection cannot prove a turn ended, so this needs a
  signal from the integration; say so where an agent has none instead of
  guessing (sol, MiMo).

- [ ] Usage summed per workspace. The author asked every session for its
  `/session` accounting by hand and had an agent record the total. The
  fork's usage module has the numbers per agent. Risk: totals that disagree
  with the provider's bill, and resumed sessions counted twice (sol).

- [ ] Up arrow in a Claude pane recalls other panes' prompts (user,
  2026-10-06: "when I press up in some Claude instance, commands from the
  history of other instances show up instead of this one"). Cause, read
  from the Claude Code 2.1.291 binary (`readForProject`): every prompt goes
  to the global `~/.claude/history.jsonl` with `project` and `sessionId`;
  Up takes the 100 newest entries of the project (all sessions), then
  lists the current session's first and the others after them. With ~15
  panes in one checkout those 100 entries span 13.6 hours (measured
  2026-10-06), so a pane quiet for half a day has none of its own left,
  and a busy one reaches others' after its few. Not herdr's bug: herdr's
  `claude --resume <id>` keeps the session id (60 recent transcripts, one
  id each), and the history file has no corrupt lines. `/clear` starts a
  new id, so prompts before it count as another session's. Upstream
  anthropics/claude-code#24751 ("scope Up-arrow history per session") is
  closed; the session-first order is probably its fix, the limit applied
  before the split is what remains. Consulted sol and MiMo (round
  `20261006-160655-e30d`).  Second report (user, same day, screenshot `History 97/100`): in a session
  with two prompts, the third Up showed another session's prompt. That is
  the designed fallback, not the limit: once the session's own prompts run
  out, Up goes on to other sessions' prompts with no marker between them.
  The user remembers it differently "before"; versions 2.1.289-291 have
  the same code, and older ones are no longer on disk to compare.
  Upstream already tracks it: anthropics/claude-code#15631 ("Option to
  disable cross-session command history in up-arrow"), open since
  2025-12-29 with many +1s (user, 2026-10-06, linked it); no new issue,
  at most a thumbs-up there. Options:
  - Workaround in herdr: herdr already stores each Claude pane's session
    id (`session_ref` in `src/agent_resume.rs`), so a popup could list only
    that pane's prompts from `history.jsonl`, newest first, and type the
    chosen one into the pane. Prior art from that issue:
    https://github.com/pjs7678/claude-session-history (tmux `prefix + H`,
    a SessionStart hook records the session, fzf popup, Enter copies).
    Read the file read-only and skip malformed lines.
    Design (consulted sol and MiMo, round `20261006-162148-0704`;
    mockups shown to the user 2026-10-06): an overlay like the image
    picker, newest first, filter as you type, rows `time  first line
    (+N lines) [paste]`, groups `Previous session · <date>` below the
    current one. The text inserted is the resolved prompt: pastes come
    from `pastedContents` (inline `content`) or `~/.claude/paste-cache/`
    (by hash), and an entry that cannot be resolved is shown as
    incomplete, never typed as `[Pasted text #1]` (both). Enter types it
    at the cursor without Enter only when the pane's agent is idle
    (`agent_status`); while it works or asks, Enter copies instead (sol).
    The server reads the file for a pane id, never a client-sent session
    id or path (sol). Cheapest first step, no core change: a plugin
    popup, since `herdr pane get` already returns `agent_session`, a
    plugin's context carries `focused_pane_id`, and `herdr pane send-text`
    exists (MiMo; fzf is installed).
  - Native Up instead of a picker (user, 2026-10-06: "why can't it work
    with Claude already running in the tab? some proxy that filters by
    sessionId?"; consulted sol and MiMo, round `20261006-162725-cdd2`).
    Preferred: a keystroke proxy in herdr, which sees every key before
    Claude. When the pane's Claude is idle and its prompt box is empty
    (detection snapshot), herdr swallows Up/Down and cycles through that
    session's own prompts from `history.jsonl` (resolved pastes): clear
    the input, bracketed-paste the prompt. Any other key leaves this mode
    and is forwarded. Clearing is reliable because Claude has a
    `chat:clearInput` action that `~/.claude/keybindings.json` can bind to
    a key herdr sends (defaults there: `up` `history:previous`, `down`
    `history:next`). Risks to test (MiMo): arrows in menus, permission
    dialogs, completions, Ctrl+R search and `!` mode must pass through;
    typed or multi-line input must keep Claude's own Up; a redraw must not
    leave the mode stuck. The load-bearing part is detecting an empty
    prompt box from the screen. Rejected: a per-pane `CLAUDE_CONFIG_DIR`
    of symlinks (sol's pick in a copied form): tmp+rename writes of
    `settings.local.json` (every "always allow") and `~/.claude.json` turn
    a symlink into a private copy, plus a daemon and lock per directory,
    and it needs a relaunch; patching the JS in the signed Bun binary
    (bytecode, re-signing, every update, terms of use); rewriting
    `project` in the shared file (one file cannot show different views to
    different panes); a FUSE view (too heavy).
  - Better, and proven live (user, 2026-10-06: "another claude binary in
    $PATH that intercepts the requests, so the real claude gets only its
    session's history"): Claude's Bun binary honours the `BUN_OPTIONS` env
    var, so a `claude` wrapper can run it with `--preload <filter.js>`,
    without touching the signed binary. Up reads `history.jsonl` through
    `open(path, "r")` from `fs/promises`; the preload wraps that open and
    hands Claude a filtered copy holding only its own sessions' lines. The
    call stack names the caller (method names survive minification):
    `readForProject` (Up) and `countForProject` (the `History N/M`
    counter) get the filtered view, `readTimestamped` (Ctrl+R, "Search
    prompts · everywhere") keeps the whole file. Own sessions: the id
    from `--resume`/`--session-id`, plus every `sessionId` this process
    appends (a `/clear` starts a new id). Tested in a throwaway tab on
    2.1.291: a fresh session's Up showed nothing, after one prompt
    `History 1/1`, after `/clear` and another prompt both of this pane's
    prompts and no others. Consulted sol and MiMo (round
    `20261006-163930-a40c`). Built as its own project (user, 2026-10-06:
    it must also work outside herdr): `~/personal_projects/claude-own-history`;
    session ids come from argv, a `SessionStart` hook the wrapper adds with
    `--settings` (covers `--continue` and the resume picker; round
    `20261006-164924-9465`) and the appended records. Live-tested: two
    sessions in one directory and `--continue` each see only their own
    prompts, plain `claude` sees both. Left for herdr: nothing, unless it
    should offer installing that wrapper. pi needs nothing: its Up history
    lives in each process's editor, seeded from the current session's
    messages, with no shared file (round `20261006-170039-5739`). The
    review notes that went into it:
    - Bail out unless `process.argv[1]` is Claude's `/$bunfs/root/cli`,
      and delete `BUN_OPTIONS` from `process.env` at once, so the Bash
      tool's `bun` and other Bun programs never load it (both).
    - Match the exact history path, not the basename; learn ids only
      from appends, never from rewrites such as retention pruning (sol).
    - Whole body in try/catch; on any error hand back the real file
      (native behaviour), never crash Claude (MiMo; sol preferred empty
      history, but today's behaviour is the safe fallback).
    - The filtered copy lives in a private 0700 directory, 0600, removed
      on exit, orphans swept at start (both); cache it by size and mtime.
    - The id for `--continue` or a re-exec: ask herdr (`herdr pane get
      $HERDR_PANE_ID` has `agent_session`) or learn it from the
      transcript this process appends to (sol: SessionStart identity).
    - Known limits: two panes resuming one id share their history (sol);
      a Claude update that reads the file another way silently restores
      the shared history, so log which call sites open it.
  - Check that herdr never resumes one session id in two panes: both
    panes would then share "own" history and interleave transcripts (sol).
  - Not worth it: a per-pane `CLAUDE_CONFIG_DIR` (splits settings,
    transcripts, plugins and login), or a worktree per pane only for this
    (and only if Claude keys `project` by the worktree root, unverified).

- [ ] Bug (user, 2026-10-03, screenshot): "I closed the tab with the job,
  but it did not close the job." Closing a parent tab's last pane (cmd+w)
  checked only the parent for running work, and the server kept its child
  job tabs running as top-level tabs. Consulted sol, DeepSeek and MiMo
  (unanimous): an explicit close of the last pane is a close of the tab;
  a parent whose shell exits or crashes keeps its jobs (an agent may exit
  after starting a long build on purpose). Done: close-pane on a parent's
  last pane asks and closes like closing the tab (test
  `closing_a_parents_last_pane_closes_the_tab_with_its_children`). Not done:
  make the kept jobs visible when the parent exits by itself: a notice
  "Parent <name> exited; N jobs kept running" and a `was <name>` mark on
  the orphaned rows (sol, DeepSeek, MiMo); MiMo's "Close tab, keep jobs"
  button in the parent's close dialog.

- [ ] The tab state does not show that something runs in the background
  (user, 2026-10-06, screenshot: "the tab state doesn't show that something
  is running in the background"). Space `music-mpd`, tab "Testy,
  ReplayGain, plan j…" shows idle `o` while Claude waits for a finite
  `musicdb update` (about 2 minutes) and said it would continue when it
  ends:
  ```
  * Worked for 1m 2s · done 12:21 PM · 1 shell still running
  ❯ ok, czekam
    ⏵⏵ auto mode on · 1 shell · ← for agents
  ```
  This is the case the 2026-10-03 decision above left idle on purpose
  (a shell alone is not working, no badge: an endless `npm run dev` would
  pin the pane). The user still wants to see it, so revisit the rejected
  option: an orthogonal background badge next to the idle state (e.g. a
  dim `1 shell` or a glyph with the count), not a new `AgentStatus`
  variant (append-closed in frozen codecs). It needs an optional runtime
  field (pane background task counts, parsed from the footer below the
  prompt box) in the JSON API, then the TUI draws it in the tab line and
  maybe the space header counts. Open: whether a long-lived dev server
  should show the same badge (probably yes: it is true, just not urgent).
  Consulted sol and MiMo (2026-10-06, calls `557d6c84`, `56fe2bac`):
  - Both: a footer count shows that something runs, not that the agent
    waits for it. Split the two: `background_tasks` (observed counts) and
    an optional "awaiting background" flag only an explicit signal sets.
    Like `awaiting-reply`, the integration could tell the agent to run
    `herdr agent awaiting-background "<what>"` when it ends a turn waiting
    for a task, cleared on the next prompt or working. Only that flag
    may count as busy (bubble, header); a bare count never does.
  - MiMo: a badge on every dev-server pane gets tuned out; draw it only
    for awaited tasks, or dim the detached ones.
  - sol: report unknown (no footer seen, other agents such as Codex) apart
    from an observed zero, with source and freshness; show the count next
    to every state, blocked included; notify only the final done
    (idle+bg -> working -> done), never on shell exit.
  - Glyph: not `⧗`/`⧖`, which already marks herdr-job jobs in space
    squares (`space_tabs.rs`); sol prefers a plain `bg:1`.
  - Noted: had the agent run `musicdb update` through `herdr-job`, the
    space would already show `⧖ 1`; this case is a plain
    `run_in_background` shell.
  - Dismissed: MiMo's 5 s debounce of working (a delay hides the cause,
    Rule 10) and its claim that bubble's "running job" covers background
    shells (it means herdr-job jobs).
  Why no herdr-job here (user, 2026-10-06: "why didn't the agent open this
  background task as a herdr job? I want visibility"): `musicdb update` was
  started by the hourly launchd job, not by the agent. The agent (which
  used `herdr-job` for its own long commands in the same session) only
  waited for that pid with a native background shell
  (`until ! ps -p 95192 …`), reading the rule "run work that takes more
  than a minute with herdr-job" as covering its own work, not waits.
  Herdr jobs are drawn as a counter-rotating circle (`◑ 1`,
  `src/ui/motion.rs` `JOB_FRAMES`), not `⧖`; the `⧖` in the doc comments
  of `src/client/shell/space_tabs.rs` is stale.
  Second round, sol and MiMo (calls `f2132f2c`, `9c834783`):
  - Both rank: instruction change plus footer badge (A+D) first; a
    PostToolUse registry of native shells without an exit hook leaves
    ghost jobs; a PreToolUse deny of `run_in_background` trains
    workarounds (worst, MiMo).
  - Instruction by intent, not minutes (sol): "use herdr-job for
    background work or waits whose end gates your next step, including
    processes you did not start". MiMo: make that path cheaper than a
    native shell, e.g. `herdr-job watch --pid N --name …`. Both: its
    success means "the process disappeared", not "it succeeded" (no exit
    status of a foreign process; pid reuse), so show it as such.
  - Do not reuse `◑` for native shells (both): it promises a tab, a log
    and an exit code. "Dimmed" must be carried by text, not colour
    (16-colour themes, `NO_COLOR`): sol `bg1` (observed) vs `wait1`
    (declared awaited), secondary foreground on top. Dismissed MiMo's
    `○N`: `○` is the idle glyph in every state-icon theme.
  - Agent waiting on a herdr job with its turn ended: sol keeps idle plus
    the job circle (today's behaviour), MiMo wants working with a frozen
    spinner. Undecided.

- [ ] Consult cost per model and the coordinator's extra spend (user,
  2026-10-03: "how much money/tokens a model used on a consult, and how much
  more the coordinator burned by asking it"). Today every call logs normalized
  usage, but no money, and the coordinator's own tokens are not logged at all.
  Consulted Sol, DeepSeek, MiMo and Space Bunny (round `20261003-013157-b88d`,
  agree on the shape):
  - Money only where money exists: a versioned, dated price table (input,
    cached input, output; reasoning billed as output, never twice since output
    already includes it), `$` per call for DeepSeek and OpenRouter. Subscription
    models (GPT, Claude, Gemini) show tokens and "included in subscription", not
    a made-up per-token price; an API-list-price equivalent only as a separately
    labelled column. A free preview model is `$0` for now, not for good.
  - Coordinator: log the Claude Code session id and the round's start and end
    (`new-round` to the last `rate`/`self`), then sum that window's per-message
    usage from the session transcript, keeping cache reads apart. Label it
    "consult-associated usage", not "extra": those turns also carry the existing
    context (Sol, Space Bunny). Keep it per round, not split per model. Do not
    add the answers again: they are already in the tool-result input (Space
    Bunny). `answer_chars` is only a fallback proxy: it misses reasoning tokens.
  - The true "how much more" needs a few matched tasks with and without a
    consult; a one-off audit, not a stats column.
  - DeepSeek: a later trial could score coordinator tokens per accepted unique
    finding, which is what a shorter answer saves.

- [ ] Naming: `ask_*` scripts versus the `consult` plugin and `consult.py`
  (user, 2026-10-03: "do we need to unify ask in one place and consult in
  another?"). All four consulted models (same round): leave it. `consult` names
  the bundle and the stats, `ask_*` are the per-vendor adapters, and renaming
  skills would split the log keys (`skill` field) and break muscle memory. At
  most one README line stating the convention. Awaiting the user's decision.

- [ ] Consult stats default view: mixed rows, too much data, and why `astra
  -r` ranks above `astra` (user, 2026-10-03: "astra -r better than astra, why?
  how do you rate these models now? The table is mixed up, deepseek is third;
  maybe show last week as the first table. Very much data; is it needed? ask
  the models"). Consulted sol, DeepSeek and MiMo (round
  `20261003-124630-aaf5`).
  - `astra -r` is not better (all three agree, verified in the log): 11 rated
    calls, mostly code reviews, three of them beside only `luna -r`. In the
    same window plain astra had 115 rated calls with uniq 1.57 versus 1.82,
    the same 6.7 findings per call, but more accepted (5.1 versus 4.1) and
    fewer rejected (24% versus 39%). Only paired rounds (same prompt, astra
    and astra -r, same companions) could show a repo-mode gain.
  - The mix-up: the default table pools all time and sorts by uniq/call, but
    unique depends on who else was asked. The `deepseek-flash` alias row
    (pre-2026-09-28, beside gpt-6-sol, terra, gemini) sits third; the current
    DeepSeek-V4.1 row (0.86) is depressed by stronger companions (sol, MiMo).
    `--days 7` alone does not fix it: it still shows the 09-26..09-28 rows.
  - Proposed default (sol's framing; DeepSeek and MiMo close): current
    configurations first (the default set and running trials, in configured
    order), last 7 days with the dates printed; retired models, alias rows of
    unknown version and rows under 5 rated calls collapse into one footer
    line. Do not merge the unknown-version alias into V4.1 (sol; DeepSeek and
    MiMo would merge with a footnote). MiMo: put the head-to-head of the
    current set first, since only shared rounds control for companions.
    Rows from another coordinator (Sonnet, asked by a DeepSeek-run agent)
    are marked or split. Keep: rated/calls, uniq/call, rejected share, err,
    p50. Cut from the default: call dates, the 8-line legend (two lines plus
    `--legend`), anecdotal rows. All of it stays behind `--all`.
  - Model ranking from shared rounds: sol 6.1 and MiMo tie on unique (60
    rounds, -0.07, CI -0.28..+0.13, W/T/L 15/29/16), sol rejects 7 points
    less, is faster (p50 38 s versus 47 s) and uses a quarter of the output
    tokens. Both beat DeepSeek-V4.1 (sol +0.67 over 161 rounds, MiMo +0.48
    over 58), DeepSeek is fastest (p50 16 s). Sonnet, Opus, Gemini, astra
    `-r`: not comparable or too few. Keep sol + DeepSeek and finish the MiMo
    trial; whether MiMo replaces DeepSeek is the trial's question.
  - Cost (user, 2026-10-03: "and DeepSeek cost-wise? I think it has to be
    turned off"): negligible. DeepSeek-V4.1 used 0.51M input and 2.52M
    output tokens over 334 calls, $1.6 to $3.2 at the current off-peak and
    peak prices (about a cent a call; $7.56 left on the account); MiMo cost
    $0.22 over 71 calls (OpenRouter's own cost field). MiMo's second trial
    passed (+0.45, CI +0.00..+0.85; rejected +3.1 points). Done 2026-10-03:
    the user replaced DeepSeek with MiMo, default set sol + MiMo, for
    quality, not cost; DeepSeek on request.

- [ ] Consult stats by lineup (user, 2026-10-03: "shouldn't consult stats
  show which models were tested together, e.g. sol ds mimo, and now a new
  stage sol mimo? ask the models"). Unique per call only compares models
  asked beside the same companions. Lineups derived from the log's rounds
  (all calls, failed ones included): 32 distinct, led by astra+ds 116 rounds
  (09-26..09-28), ds+sol 90 (09-30..10-03), ds alone 63, ds+mimo+sol 42,
  ds+luna 31, astra+ds+luna 29, sonnet alone 27, bunny+ds+mimo+sol 22.
  Consulted sol and MiMo (round `20261003-130240-6583`). Plan:
  - `new-round` records the requested lineup (`--models sol,mimo`, the
    consult skill passes the default set), because dates cannot assign
    stages: the MiMo and Space Bunny trials ran inside the sol+ds period
    (MiMo). Older rounds get a lineup derived from their calls, marked
    derived.
  - `stats --lineups`: one block per lineup with dates, coordinator, rounds,
    full rounds; per model calls ok/failed, findings, accepted, rejected,
    unique per answered call, p50. Lineups under 5 rounds fold into one line.
  - Default `stats`: the current lineup's block first; no ranking across
    lineups.
  - Kept apart, each with a count so nothing is silently dropped: one-model
    asks (unique is near tautological there), rounds where a companion
    failed (its outage inflates the other's unique, sol), rounds run by
    another coordinator, and rounds with an extra model asked on request.
  - Named stages with a reason (`stage start sol+mimo --note ...`): only if
    the why is worth keeping in the tool; the consult skill already records
    each default-set change (sol). MiMo argued `--vs` already controls for
    companions and this is bookkeeping; true for a two-model verdict, but
    the user wants the history of what was tested.

- [ ] "Consult: models" menu with checkboxes (user, 2026-10-03: "a simple
  menu: which models are used for consultation now, a checkbox to enable or
  disable, its rank, uniqueness, error rate, and maybe how much the
  coordinator's token cost increases"). Narrows the deferred settings >
  consults page and the auto-consult toggle (both below, under herdr > menu >
  settings). Consulted sol and MiMo (round `20261003-145404-dae6`). Not
  started: another session is working nearby (user, 2026-10-03: "don't do it
  for now, another session is on it; only the TODO"). Plan:
  - A native herdr modal in Rust (user, 2026-10-03: "a script? I want it in
    Rust"; chose the native modal over a ratatui binary in the plugin), in
    the existing dialog style, mouse-first: clickable checkboxes. It replaces
    the menu's **consult stats** item. The server reads the log and the state
    file and exposes them through new advertised API methods (neutral names,
    e.g. `consult.models.list`, `consult.models.set`), so the modal also
    works against a remote server; an older server without them disables
    only this item. Rows `[x] model | uniq/call (n) | wrong% | err% |
    rated/calls | last used`; a toggle shows only after the server confirms
    it is persisted. The statistics logic lives in `consult.py` today: decide
    whether the server ports it or calls `consult.py ... --json`.
  - State: one global file `~/.local/state/consult/models.json`, written
    atomically. `consult.py models` prints the enabled set and is the single
    source: it prints the skill's default when the file is missing (MiMo),
    an empty list means consulting is off, a malformed file is an error, not
    a silent default (sol). The consult skill runs it at each round instead
    of the prose default set. An explicit request ("ask DeepSeek") bypasses
    the checkbox but never the self-consultation rule or a missing key.
  - `new-round` records the enabled set and whether the round was automatic
    or explicitly requested, which also feeds "Consult stats by lineup".
  - No rank column (both models): one number per model moves when another
    row is toggled (companion effect). Numbers come from rounds of the actual
    lineup, with n shown and metrics hidden under 5 rated calls; the paired
    `stats --vs` stays the comparison.
  - Coordinator cost, stage 1: the answer tokens each round injects into the
    coordinator's context (already logged as `answer_chars`), labelled a lower
    bound: they are re-read as cached input on every later turn, and the
    coordinator's own reasoning is not counted. The full number waits for
    "Consult cost per model and the coordinator's extra spend". No column
    that reads "n/a"; subscription models show "included", never `$0`.
  - Later: a `doctor` mark for an enabled model without a key or CLI, so it
    does not burn calls into err%.

- [ ] Do the consult popups need `less`? (user, 2026-10-03: "less used in
  consult stats? we have Rust. ask the models"). `page-consult` pages
  `consult.py` output with `less -R`; a popup is a real PTY pane
  (`spawn_popup_command`, `src/app/popup.rs`). Consulted Sol, DeepSeek and MiMo
  (round `20261003-022724-693a`), unanimous: keep `less` for now; "we have Rust"
  is not a reason by itself, since the problem is viewing text, not the language.
  - Reject a herdr pager subcommand (`herdr pager FILE`): it rebuilds `less`
    (search, keys, ANSI, resize, mouse) and still runs inside a PTY, so it
    gains nothing at the runtime/client boundary.
  - Reject rewriting `consult.py stats` in Rust inside herdr: orthogonal, and it
    couples personal analytics to the multiplexer.
  - First step, a spike: a temporary popup with `command = ["seq", "1", "300"]`.
    Does the popup keep scrollback, scroll with the mouse wheel and start at the
    top? Does it get SIGWINCH on resize? If yes, drop `less` from
    `page-consult` (print, then wait for Enter): mouse-first, no external pager,
    but no `/` search. `less` runs on the alternate screen, so herdr's
    scrollback sees nothing while it runs. If popups do not scroll, that is a
    herdr defect worth fixing on its own.
  - Later, only if several plugins want it (DeepSeek, MiMo): a manifest text
    popup whose command's stdout herdr renders itself (no PTY, works on Windows
    and remote clients). It is a new pane type: server-owned content,
    client-owned viewport, reflow on resize, output limits, stderr and exit
    status.
  - Known limit either way: the tables are fitted to the width at launch; a
    resized popup does not regenerate them.

- [ ] Consult stats per model over time, to spot a silently "nerfed" model
  (user, 2026-10-03: "what if we showed stats for a model over time? we could
  detect a nerfed model. How to display those graphs then? ask the models").
  Log on 2026-10-03: about 7.5 days, DeepSeek ~460 calls, Sol ~175, MiMo 39.
  `model_version` exists for DeepSeek (`DeepSeek-V4.1-Flash`, one fingerprint),
  MiMo and Claude, never for the GPT models (Codex does not report it); `usage`
  has `reasoning` tokens for every vendor. Consulted Sol, DeepSeek and MiMo
  (round `20261003-023646-4385`), agreeing on:
  - A drift report, not a "nerf detector": the data can show a change, not
    its cause. No composite score, no alerts, no all-pairs dashboard.
  - Primary series: the paired difference against a reference model over
    shared rounds (reuses `--vs` and its round bootstrap), since pooled rates
    move with the question mix. My addition: a pair alone cannot say which side
    moved; rounds with three models (Sol, DeepSeek, MiMo) can, because the side
    shared by both shifted differences is the one that changed.
  - Objective companions: output and reasoning tokens per 1k prompt chars
    (missing is not zero), error rate, latency only as a hint. Version and
    fingerprint changes are markers on the time axis, not a series.
  - Demote `unique` per call (depends on who else answered) and pooled useful
    share (the rater is an LLM and drifts too; MiMo: check whether verdicts
    correlate with answer length).
  - Buckets: equal-n blocks (Sol: 50 rated calls; MiMo: rolling 50 shared
    rounds, at least 30), labelled with their date span, with `n`, rating
    coverage and a CI (Wilson for rates, round bootstrap for paired
    differences). Below the minimum print "insufficient n", do not draw.
    Fix the rule in advance (MiMo: |Δ| >= 15 points with the CI excluding 0 in
    two consecutive blocks); no change-point detection yet.
  - Display: text first, as `consult.py trend [--vs A B]` in the existing
    `page-consult` popup, width-aware like `stats`: one row per block
    (`span | n | Δ useful [CI] | coverage | errors | tokens | latency`), with
    version changes marked. Sparklines at most as an extra column (they hide
    the CI). No kitty-graphics PNG: `less -R` strips graphics escapes, and it
    would need matplotlib. HTML only for one-off exploration.
  - Smallest first step (DeepSeek): list `model_version`/fingerprint per model
    per week; a version bump answers the question without statistics.

- [ ] No `?` on a tab that ended with a question (user, 2026-10-01, screenshot
  of this very session: the tab showed the idle green ring after a turn that
  ended "Install this build, push the commits, or fix the flaky test first?").
  Cause, verified: the `?` mark comes only from the agent running `herdr agent
  awaiting-reply` as the last command of its turn (the hook reminder asks for
  it); the agent in that turn did not run it. Nothing in herdr infers a
  question. Consulted DeepSeek, Opus and GPT; they agree the explicit command
  stays authoritative and that screen scraping is out; they differ on the
  fallback:
  - DeepSeek: a Claude Code `Stop` hook reads `last_assistant_message`
    (or `transcript_path`), strips code, quotes and URLs, and when the final
    paragraph is a direct question and nothing was reported it either marks
    the pane itself with a high-confidence rule or, if ambiguous, blocks the
    stop once (`stop_hook_active` false) with "if you are waiting for the user,
    run `herdr agent awaiting-reply`".
  - Opus: only the blocking reminder (the agent decides; no inference, no
    model calls); a false alarm costs one short extra turn and sets no mark.
  - GPT: the hook marks the pane itself as an inferred state (`source =
    stop-heuristic`, with the matched evidence), conservative bilingual rules
    (a direct request for a choice, confirmation or information, not just a
    `?`), ambiguous means idle; no blocking, because it restarts the agent for
    bookkeeping.
  - Common: per-turn generation so a stale report cannot stick; clear on
    `UserPromptSubmit`, typing, the next tool use or turn; run in shadow mode
    first (log the would-be marks next to the real reports), then enable per
    integration behind a flag; fixtures in English and Polish with code,
    quotes, rhetorical questions, "let me know if", lists of options, and the
    reported sentence as a positive case.
  - Decision (the user said "choose yourself", 2026-10-01): order V1 shadow
    logging, then V5 a stronger instruction, then V2 the blocking Stop-hook
    reminder as a canary, V3 inference only if V2 is not enough (all three
    models, second round). The offline audit made V1 unnecessary: it measures
    the misses from existing transcripts.
  - Audit (`scripts/awaiting_reply_audit.py`, tests in
    `scripts/test_awaiting_reply_audit.py`; read only; Claude Code and Pi
    transcripts; a bilingual question heuristic; per model: question-like
    turns, reported, missed, false reports, order violations, Wilson interval).
    First numbers, turns since 2026-10-01 12:40 (when every integration sent
    the instruction): Claude Sonnet 5.5 (this session): 15 question-like turns,
    11 missed (73%, CI 48-89%); Claude Opus 5.5: 8 question-like, 1 missed
    (12%, CI 2-47%); Claude Haiku 4.5: 8 question-like, 7 missed (88%). Older
    turns, before the instruction, are 90-100% misses for every model, so they
    prove nothing about compliance. No Pi turn since the extension was
    installed was in the transcripts yet (rerun after some Pi use). Caveats: the
    heuristic gives false reports too (reported but the last paragraph is not a
    question: 12-28 per model), it is a screen to review, not ground truth.
  - Models' thresholds for moving on: V2 when the lower bound of the miss rate
    is above 2-5% and the heuristic's false positive rate is at most 2%; V3 only
    when the inferred precision's lower bound is above 98-99% and V2 is not
    enough; rubric for an LLM judge: "does the final message ask the user for
    a decision or an answer before work can continue" (not courtesy offers,
    rhetorical or quoted questions), two judges, blind to the report status.
  - Other variants kept here for when it happens again: V1 shadow log from a
    Stop hook (`last_assistant_message`, else `transcript_path`); V2 block once
    (`stop_hook_active`, "if you wait for the user run `herdr agent
    awaiting-reply`, otherwise just stop"); V3 the hook marks the pane itself
    (`source = inferred`, per-turn generation, cleared on `UserPromptSubmit`,
    typing and the next tool use); Pi has no `Stop` hook found yet, so it needs
    an `agent_end` extension that does the same.
  - Done 2026-10-01 (V2 for Claude Code; installed into `~/.claude` the same day with
    `herdr integration install claude`, committed in the dotfiles repo; sessions
    started before that keep their old hooks until restarted): a `Stop` hook
    (`herdr-agent-state.sh stop-check`, added and removed with the reminder in
    `claude_settings.rs`): when the final message's last paragraph looks like a
    question for the user (the audit's bilingual heuristic, a parity test keeps
    them equal) and the turn ran no `herdr agent awaiting-reply`, it blocks the
    stop once (`stop_hook_active` guards the loop) with "run `herdr agent
    awaiting-reply` now as the only command, then stop without repeating your
    message; if you are not waiting for the user just stop"; every decision is
    logged to `~/.local/state/herdr/awaiting-reply-stop.jsonl`;
    `HERDR_AWAITING_REPLY_STOP=0` turns it off, `=shadow` only logs. Tests: the
    install/uninstall tests, and `StopHook` in `scripts/test_awaiting_reply_
    audit.py` (block once, reported and statement pass, an earlier turn's
    report does not count). The integration version stays 11 (not yet
    released). Not done: Pi (no `Stop` equivalent found; needs an `agent_end`
    extension), V3 inference, an LLM judge for the audit.

- [ ] Update check for the fork (deferred 2026-10-02, the user: not announced yet, so
  probably not needed; DeepSeek and GPT agree: defer). Today `herdr_live.sh` (backup,
  rollback) is the update path of the only user, and the updater is off for fork builds.
  Trigger to do it: the first outside user relying on the published binaries, or the
  public announcement. Then in two steps: (1) notify only: compare `(0.9.3, revision)`
  from the embedded `ROHERDR_VERSION` with the newest `roherdr-v*` release of
  `rofrol/roherdr`, show "newer release available" and the download command, nothing
  replaced; local builds (hash instead of a number) do not check. (2) Only when several
  binary users need it, after the upstream rebase: download `roherdr-<os>-<arch>`, verify
  `SHA256SUMS`, stage the file and swap it after the process exits, with a tested rollback;
  if the fork gets a Homebrew tap, leave upgrades to Homebrew instead. Not before the
  upstream rebase (rebase debt). Done: nothing.

- [ ] The fork's name, green Windows CI and releases (user, 2026-10-01: "pick
  a name for the herdr fork, I already have roguix, maybe follow similar
  conventions; make the Windows tests pass; do we build releases on GitHub
  Actions like upstream herdr? Ask the models. Do it as item 4."). Consulted
  DeepSeek, Opus and GPT. Facts: the fork `rofrol/herdr` has Actions enabled
  and no runs yet; `ci.yml` has a `windows-latest` job (ConPTY smoke test);
  `release.yml`, `preview.yml`, `distribution.yml`, `pr-gate.yml`,
  `label-next-release-issues.yml` and `website-deploy.yml` are upstream's.
  - Name: all three keep the binary, crate, `HERDR_*`, `~/.config/herdr`,
    socket names, plugin ids and the `herdr` skill (compatibility with
    plugins, scripts and agents, and cheap upstream rebases) and rename only
    the repo, the display branding (README title, `--version` text, window
    titles) and the release asset names, in one small commit on top; keep
    attribution and the licence (`LICENSE` is Apache-2.0, verified; keep it and any
    NOTICE), no upstream logo, domain or
    "official" claim, disable or repoint the self-updater. Candidates after
    "roguix" (read as ro(frol) + guix): `drovix`/`roherd`/`flockx`/`corralx`
    (Opus, recommends `drovix`: drover, herd driver), `rogux`/`frolux`/
    `panix`/`tabrix`/`muxix` (GPT, recommends `rogux`), `herdix`/`herdux`/
    `herdrix` (DeepSeek; closest to the mark, most confusion). Check GitHub,
    crates.io, npm and domain availability and trademark before choosing; the
    choice is the user's.
  - Windows CI: run `ci.yml` on the fork (first a baseline on the upstream tag
    to see what fails without the fork's commits, then on `master`), read
    `gh run view <id> --log-failed`, iterate; suspects in the fork's code:
    `std::os::unix`, Unix sockets, `chmod`, signals, `sh -c` in plugins and
    scripts, `$HOME`, `/` in assertions, CRLF, ConPTY timing. Gate genuinely
    Unix-only tests with `#[cfg(unix)]` and give Windows an equivalent; use
    `#[cfg_attr(windows, ignore = "reason")]` only with a reason; keep
    `just windows-lint` before every push.
  - Releases: do not reuse upstream's `release.yml` (maintainer gating,
    Homebrew, Nix, website, secrets). Disable upstream's workflows on the fork
    (`gh workflow disable`, no diff to rebase) and add `fork-release.yml`
    on tags like `fork-v*`: matrix `macos` aarch64 and `ubuntu` x86_64
    (Windows optional, its zip needs the ConPTY runtime), `cargo build
    --release --locked`, archives with checksums, one publish job with
    `gh release create` and `contents: write`, no custom secrets. Public repos
    get standard runners free. Test with a prerelease tag: download on the Mac,
    `herdr --version`, checksum, a smoke run.
  - Order: baseline CI, disable the upstream workflows, green Windows, the
    name commit, then the release workflow and a prerelease.
  - Progress 2026-10-02: the fork's workflows do not run on `push` or
    `pull_request` (0 runs after many pushes and a throwaway PR; only a
    `workflow_dispatch` runs), so CI is run by dispatch from a branch whose
    `ci.yml` has `workflow_dispatch:` added (`ci-dispatch*`, never merged; the
    first run: Build artifacts (manual) for Linux green in 6 min). First CI run
    on the fork (run 36954449725): Windows `check` failed to compile the tests
    (`running_program` missing in two `ClientShellPane` literals in
    `activation_tests.rs`; a `System { .. }` pattern without `target` in
    `shell/notifications.rs`), Ubuntu failed
    `cases::sessions::integration_commands_run_locally_when_server_is_missing`
    (`tests/cli/sessions.rs` expected Pi `v9`, the fork's is `v10`; that test is
    Linux-only, so `just check` on macOS never ran it). All fixed;
    `just windows-lint` now runs `cargo clippy --all-targets`, so Windows test
    compile errors show up locally. Still to do: rerun CI until `check` is green
    on all three systems (nextest stops at the first failure), then the name and
    the release workflow.
  - Done 2026-10-02: CI is green on the fork on all three systems (run
    36957931855: Ubuntu, macOS, Windows `check`, ConPTY package, conventional
    commits; all 3580 Windows tests pass). More fixes after the first runs:
    `target_sweep.py` and its tests run without `fcntl` on Windows; the fork's
    Unix-only plugin tests (`fork-plugin-test`) run on Unix only; the macOS
    symlink test is skipped on Windows. To run CI by hand (push and PR events do
    not start it on this fork): a throwaway branch `ci-dispatch-N` whose `ci.yml`
    has `workflow_dispatch:`, then `gh workflow run ci.yml --ref ci-dispatch-N`.
    Upstream's workflows (preview, release, pr-gate, label-next-release-issues,
    distribution, website-deploy, nix, windows-arm64) are disabled on the fork
    (`gh workflow disable`, reversible); `ci.yml` and `build-artifacts-manual.yml`
    stay. New `.github/workflows/fork-release.yml`: a `fork-v*` tag or a manual
    dispatch with a tag builds Linux x86_64/aarch64 (static musl), macOS
    arm64/x86_64 and the Windows zip with its ConPTY runtime, and publishes a
    prerelease with `SHA256SUMS`; the asset names use `FORK_NAME` (now
    `herdr-fork`). First dispatch built macOS and Windows; Linux aarch64 failed on
    an `ldd` check (replaced by `file`), rerun in progress.
  - Name consultation 2026-10-02 (DeepSeek, Opus, GPT, Gemini; availability
    checked the same day on crates.io, npm and GitHub, `.dev` by DNS; trademark
    NOT checked). All four: keep `herdr` out of the repo name, say "unofficial
    fork of herdr" in the description, README line one and the topics, keep
    attribution and Apache-2.0, `--version` like `herdr 0.9.3 (<name> fork)`.
    Candidates by support: `drovix` (drover + ix; Opus's pick; crates and npm
    free, a GitHub user `drovix` and 8 repos exist, `drovix.dev` has no DNS
    record); `roherdix` (ro + herd + ix; Gemini's pick, DeepSeek's 4th; free
    everywhere I looked, but contains "herd", which Opus and DeepSeek avoid for
    confusion); `rofherdix` (DeepSeek's pick, same objection, clumsy);
    `ropanix` (ro + pane + ix; GPT's pick, free, a pun on panes, reads like
    "panics"); `corralix` and `flockix` (herding puns; users `Corralix` and
    `Flockix` exist); `rogux` (closest to roguix, too close). Rejected by all:
    `herdix`, `herdrix`, `herdux`. My order: `drovix`, `ropanix`, `roherdix`.
  - Name chosen 2026-10-02 by the user: first **roherd**, an hour later
    **roherdr** (it keeps the `-r` of herdr, so it is closer to upstream's name
    than the models advised; the README disclaimer carries the weight). Everything
    below was done for `roherd` and renamed to `roherdr`. Original note: **roherd** (his own pick, not one of the
    models' lists; contains "herd", which Opus and DeepSeek advised against, so
    the README says first thing that it is unofficial and not affiliated).
    Checked free the same day: crates.io, npm, Homebrew, GitHub user and repo
    names; trademark not checked. Done: `--version` prints `herdr 0.9.3 (roherd,
    an unofficial fork)` for fork builds; README title and first paragraph;
    release assets `roherd-<os>-<arch>`; the GitHub repo is renamed to
    `rofrol/roherd` with a description and topics after the first release run.
  - Versioning policy decided 2026-10-02 (DeepSeek and GPT agree; the try-roguix
    session explained its own, which differs): tags `roherdr-v<upstream version>.<fork
    revision>`, e.g. `roherdr-v0.9.3.1` (changed from `fork-v0.9.3-1` the same day: the user
    asked for the name in the tag, DeepSeek and GPT agreed: no collision with upstream's
    `v*`/`preview-*`, matches the asset names; four numbers because `0.9.3-1` is a
    semver prerelease below `0.9.3`); the revision counts published fork releases (not
    commits or rebases), restarts at 1 when upstream's version changes, tags are
    annotated and never reused; `Cargo.toml` stays at upstream's version (fewer rebase
    conflicts). try-roguix uses independent semver `vX.Y.Z` (it has no upstream release
    to track; `bNN` there are image build counters, not releases) and records upstream
    in the release title, with upstream tags fetched into a namespace; roherdr's
    upstream tags are already namespaced `upstream/*`. First release: `roherdr-v0.9.3.1`. `--version` now shows the revision
    (`herdr 0.9.3 (roherdr 0.9.3.1, an unofficial fork)`; CI sets `ROHERDR_VERSION` from the tag,
    local builds show the commit hash, `+` when dirty; the `herdr <semver>` prefix is unchanged;
    DeepSeek and GPT agree). The first release `roherdr-v0.9.3.1` was built before this, so
    it prints no revision; the next release carries it. Not done: a self-update
    check against the fork's releases (the updater is off for fork builds).
  - Still open: the name itself (the user's choice; then `FORK_NAME`, the README
    title, `--version` text), the self-updater pointing at upstream, trying a
    prerelease download on the Mac.
  - Done: nothing yet.

- [ ] Diagnose four sidebar tab-tree oddities (2026-10-01).
  - Screenshot: `/Users/romanfrolow/Screenshots/Screenshot 2026-10-01 at 01.41.46.png`
    (workspace `herdr`, branch `master`). Rows in order: `lazygit`,
    `Zakładki poziome n…`, `Anthropic limit wyczerp…`, `π - herdr`,
    `ask claude claude-opus-…`, `ask gemini 3.8-flash-lo…`, then a worktree
    group `▼ Pi compact job act…` with a `└─` connector, then `zsh`.
  - [x] (fixed: `herdr-job run` nests under the owner tab's top-level parent
    and reports a refused `tab parent`) Why are `ask claude` and `ask gemini` (consult helpers) shown as
    ordinary top-level tabs instead of inside the herdr-job group? They are
    presumably launched by `plugins/consult` outside `herdr-job run`; check
    whether they should go through it (see `herdr-job` in the global rules).
  - [x] (fixed 2026-10-03 with the trunk below) Why does the worktree group
    look like this (a bare `▼ name  +` row with a `└─` stub, unlike the tab
    rows above it)? Check which parent link and row kind the renderer uses
    for a worktree group.
  - [x] (fixed 2026-10-03, reported again with `Pusty job na karcie` above
    the worktree) Why does the worktree's `└─` connector hang under `ask
    gemini`, as if it were its child? It was purely visual: the child's
    `   └─ ` prefix put the connector in column 3, the column of the tab
    lines' state icons, with nothing linking it to the parent space's name.
    Now `render_worktree_trunk` draws a `│` trunk in column 1 down the
    parent's rows and tab lines (and down a worktree that is not the last),
    and the child's prefix is ` └─── `, so it hangs from the space, not the
    tab. Consulted GPT sol, MiMo and Space Bunny (all chose the trunk) and
    DeepSeek (chose moving the connector without a trunk, fearing the
    selected tab's fill would cover the trunk; it starts at column 5).
    Left open from the consults: a space's name text and its tab lines'
    text do not share a left edge.
  - [x] (fixed by the worktree tab indent below; installed build pending)
    Why is `zsh` after the worktree group not indented like the other
    tabs? Check whether it is a child of the group or a top-level tab drawn
    at the wrong depth.
  - Findings from `herdr tab list --workspace wR` and the code (2026-10-01):
    - Q1: both `ask claude` (`wR:tZ7`) and `ask gemini` (`wR:t07`) ARE
      herdr-job tabs (they carry `job`), but have no `parent_tab_id`.
      `herdr-job` nests a job under its owner's tab with `herdr tab parent`
      and ignores a failure (`herdr_ok`, `plugins/job/herdr-job`). Nesting
      is one level only (`Workspace::set_tab_parent`: "the parent must be a
      top-level tab"). `ask gemini` ran from pane `wR:p0M`, which now lives
      in job tab `wR:t03` (itself a child of `wR:tVM`), so the parent
      request was refused: the likely cause. `ask claude` ran from pane
      `wR:pZP`, which no longer exists (its tab was closed, and a closed
      parent leaves the child top-level): unverified for the exact close.
      Fix idea to consider: nest under the owner's top-level ancestor, and
      log a refused `tab parent` instead of ignoring it.
    - Q2-Q4: `Pi compact job act…` is not a tab: it is the separate
      workspace `w1B` (linked worktree of `herdr`), drawn as a child of the
      repo workspace, and `zsh` is that workspace's only tab (`w1B:t1`).
      The tabs were not indented below the worktree header (fixed). The
      `└─` stub is the worktree's tree connector to its parent space; it
      starts under `ask gemini` only because that is the parent space's
      last tab line. Whether the stub needs more separation is a design
      question for the user. Read the sidebar renderer
      (`src/ui`) for worktree-workspace rows before judging.
  - Consulted DeepSeek (generic hypotheses, nothing the data above did not
    settle better); GPT sol hit the Plus usage limit this round.

- [ ] Diagnose multiline copy in Pi versus Claude CLI (2026-10-01).
  - User reports Claude CLI selection copies as expected, whereas Pi inserts
    newline characters into copied multiline text. Determine whether these
    are extra breaks at visual wraps rather than intentional paragraph/code
    breaks. No exact reproduction or clipboard-byte comparison yet.
  - Installed Pi 0.99.1 fullscreen `getActiveSelectionText()` reads rendered
    rows and joins them with `\n` in `pi-tui/dist/tui-alt-screen.js`.
    This is a plausible mechanism in fullscreen, not proof for regular mode.
    Global settings currently omit `tuiMode` (default regular); CLI/project
    overrides and the user's actual gesture remain unknown. Do not assume
    Claude's selection implementation without inspecting/reproducing it.
  - Consulted DeepSeek and Gemini (low/medium/high): compare the same
    synthetic paragraph, real-newline code block, unwrapped control and
    Unicode text at 80/120 columns, in Pi regular/fullscreen and Claude CLI.
    Record terminal/version, resize geometry, mouse modifiers and whether
    copying uses terminal selection, Pi copy-on-select or OSC 52/native
    clipboard. Compare exact LF/CRLF bytes, not just pasted appearance.
    Preserve real newlines, indentation, graphemes and trailing spaces;
    never fix this by blindly joining every selected row. Do not inspect or
    overwrite the user's existing clipboard without permission; use a
    disposable synthetic reproduction. No upstream issue without reproduction.

- [ ] Compact job presentation for the agents the user runs: Pi, Claude Code,
  others (asked 2026-10-01). Today only Pi has it: the Pi activity extension
  (`plugins/job/pi`) folds every tool call (bash, read, edit, write,
  codemode) into one row, whatever the model, and a job's row is a Ctrl+click
  link to the job tab. Claude Code has no tool-rendering API: its Bash result
  is collapsed by Claude Code itself (Ctrl+O expands), but the model still
  receives the whole output, and `herdr-job wait` used to stream the job log
  into it. Other agents (codex, cursor, gemini, opencode, ...) are not used.
  - Consulted DeepSeek, Opus, GPT and Gemini 2026-10-01: all rank the same
    first: make `herdr-job wait` compact at the source, which helps every
    agent without per-agent code, then advice in the agent instructions; a
    PreToolUse hook rewriting `wait` is a brittle fallback; PostToolUse
    cannot change what the UI shows; Monitor is for sparse state changes, not
    log tails; do not build per-agent renderers for unused agents.
  - Done 2026-10-01 (committed, not installed): `herdr-job wait <id>` prints
    one start line (with `herdr tab focus <tab>`), nothing while the job
    runs, and the unchanged final line `herdr-job <id> (<name>): <state>,
    exit <code>`; a failure adds the last 40 lines (at most 8 KiB, escapes
    removed) before it. `--stream` (or `HERDR_JOB_WAIT_STREAM=1`) restores
    the old whole-log streaming; `--quiet` prints only the final line. Exit
    codes are unchanged. Tests: `WaitTests` in `plugins/job/test_herdr_job.py`.
  - Open: tell Claude Code and Pi to prefer the compact `wait` and to read the
    log path only on failure (the user's global instructions already say to
    wait with `herdr-job wait`); a Claude Code `PreToolUse` hook is optional;
    an OSC 8 job link in the Claude output was not added (Claude Code may not
    pass it through); per-agent rows for codex, cursor, gemini and opencode
    stay deferred until the user runs one.

- [ ] Compact Pi activity rows with click-through to herdr-job details. (implemented and merged 2026-10-01, opt-in, not activated: see the last bullet)
  - User request and screenshot, 2026-10-01:
    `/Users/romanfrolow/Screenshots/Screenshot 2026-10-01 at 01.12.20.png`.
    The main Pi transcript should show successive short status rows, each
    with an animated half-circle indicator while work actually runs and a
    small summary of the activity. Clicking a row should focus its herdr-job
    pane. Keep available verbose tool input/output, job logs and explicitly
    emitted model progress in the detail view, not walls of JSON and `wait`
    output in the main conversation. Do not claim to expose or relocate
    hidden internal model reasoning.
  - Presentation belongs in the Pi/client extension; process state, job
    identity and logs belong to Herdr runtime. First verify supported Pi
    render/mouse hooks and Herdr navigation APIs before choosing a design.
    Reuse existing job identity and focus behavior; do not infer identity
    from tab titles or create a process/job for every token.
  - Show real running/done/failed/blocked/cancelled state, stop animation
    when work settles, retain meaningful completion/error summaries and
    keyboard/expandable fallback when clicking is unavailable. Do not hide
    permission prompts or lose original tool results needed for review.
    Handle reconnect, resume, deleted job tabs and unavailable servers.
  - No timer/background endpoint requests: use received job state, and
    navigate only on explicit user action. Bound redraw frequency and keep
    hidden rows idle. Do not copy credentials or unredacted sensitive tool
    output into new logs. Outside Herdr preserve ordinary Pi rendering.
    This is a planning task; no transcript replacement implemented yet.
  - Consulted DeepSeek and Gemini (medium/high), 2026-10-01: preserve
    scrollback, selection/copy, narrow-width reflow, session export and raw
    transcript evidence; bound summaries and sanitise control sequences.
    Provide reduced-motion/static indicators and a way back to the origin.
    Renderer/navigation failures must not stop tools or cancel jobs. Use
    supported Pi rendering rather than injecting carriage-return/escape
    sequences into its transcript. Test concurrent jobs with explicit
    tool-call/job mappings, not an assumed universal one-to-one relationship.
    Gemini low was rejected for exhausted capacity (reported reset 0s);
    no retry was made and no answer is attributed to that attempt.
  - Merged to `master` 2026-10-01 as `430f8b13` (rebased, fast-forward; the
    `task/pi-job-activity` branch and its worktree stay until you have tried
    it): `plugins/job/pi/` is an opt-in Pi extension (README there), with its
    tests in `just pi-activity-test` (part of `just check`) against a mocked
    Pi, plus a smoke run against the installed Pi SDK. It is NOT linked into
    Pi: try it with `pi -e ./plugins/job/pi/index.ts`, or link the
    directory into `~/.pi/agent/extensions/herdr-activity` and `/reload`.
    Not verified in a live Pi session: `/activity` navigation and the
    detail-viewer job.
  - Tried live 2026-10-01 in a Pi tab (deepseek-flash): rows, the turning
    half circle and the finished `✓` work. A plain click on a row did nothing
    (regular Pi gets no mouse reports). Fixed the same day: a row with a job
    now ends in `open job (ctrl+click)`, an OSC 8 link `herdr-job://<id>`
    handled by a Herdr plugin link handler in `plugins/job/herdr-plugin.toml`
    (`herdr-job open --from-click`; strict id check, only `tab focus`, refuses
    gone or reused tabs). Verified end to end through `pane.link.activate` on
    the demo pane (`handled: true`, the job tab got focus); the physical
    Ctrl+click on macOS is not verified. Consulted DeepSeek, Opus, GPT and
    Gemini: all chose the link; a plain-click handler in Herdr itself
    (a `herdr-tab:` scheme resolved in `pane.link.activate`) is the later
    option. Linked into `~/.pi/agent/extensions/herdr-activity` and the
    built-in `codemode` switched off in `~/.pi/agent/settings.json`
    (`-builtin:codemode`; the extension now registers `codemode` even outside
    Herdr, so it is not lost there).
  - Follow-up from the user's first try (2026-10-01): (1) only the tail
    `open job (ctrl+click)` was clickable: the whole row is the link now
    (checked at the badge, the middle and the tail; past the end is not);
    (2) after the jump the sidebar did not show where you are: focusing a job
    now unfolds its parent's squares once per focus change
    (`unfold_focused_job`; folding by hand sticks); (3) a delay between click
    and focus: `herdr-job open` scanned all 1209 stored jobs (0.2-0.35 s) and
    now asks `tab get` for the one tab and checks its job id (0.1 s); from the
    API call to the focus change measured 0.12-0.25 s. The physical
    Ctrl+click on macOS: confirmed by the user ("działa", 2026-10-01) on
    the installed build `47ba72b8`.

- [ ] Add easily accessible advisor checkboxes in Herdr so it injects
  `Consult with <selected agents>` into coding-agent requests. Let the user
  select advisors (for example DeepSeek) and disable the instruction easily.
  Consulted DeepSeek 2026-09-30: start with a per-pane/session picker opened
  from a visible `Advisors` control, showing the selected advisors. Inject
  only on an explicit user send, preserve the user's text, preview the added
  instruction and avoid duplicates; do not trigger background consultations.
  Verify each CLI's supported injection path; use a visible, copyable prefix
  rather than silent PTY keystrokes when safe injection is unavailable.
  Decide scope, persistence, timing (every prompt or first turn), advisor
  identity/invocation and multi-client ownership before implementation.
  Make remote-provider privacy and cost implications explicit. These are
  recommendations, not an approved UI design or implementation.

- [ ] Update automatic terminal/tab titles to reflect current activity, as
  in other terminals (screenshot, 2026-09-30 02:14). The selected sidebar
  tab says `env` while its pane runs `brew update` / `brew upgrade --formula`.
  Investigate the source of `env` and title precedence before assigning a
  cause: launch label, shell-emitted OSC 0/2, explicit name, or stale state.
  Consulted DeepSeek 2026-09-30: honor shell-provided titles first; do not
  assume every terminal infers foreground commands. Preserve explicit user
  names. Consider a foreground-command fallback only when reliable and no
  meaningful emitted title is available; launch wrappers must not remain
  the automatic label when a better source exists. Verify command-to-prompt
  restoration, consecutive commands, empty OSC titles, explicit names and
  shells with/without title emission. Sanitize and bound title text; avoid
  flicker, output-driven churn and per-render process-tree polling. Check
  many-pane idle overhead if fallback detection is added. No root cause
  verified and no implementation approved yet.
  - Probable cause found 2026-10-01 (not reproduced live): the tab label comes
    from the program leading the pane's foreground group (`TerminalState::
    running_label`), and `ForegroundProgramTracker` looks that name up once
    per new group. A command like `env VAR=1 brew upgrade` starts as `env`,
    which then execs the real program inside the same group, so the group
    kept the name `env`. Mitigation committed (not installed): wrappers
    (`env`, `command`, `exec`, `nice`, `nohup`, `time`, `timeout`, `sudo`,
    `doas`) are looked up again for up to six ticks per group, then believed.
    Bounded extra work, only for panes running a wrapper. Not done: the rest
    of this item (a title or foreground-command fallback beyond the program
    name, OSC title precedence), and the user should say whether `env` still
    appears after the next install.

- [ ] Explore a subtle animated indicator while an agent instance is working,
  instead of a static status glyph (screenshots, 2026-09-30 01:09). The
  screenshots show the half-filled working circle `◐`; the user described
  a possible hourglass replacement and has not chosen a design yet. Consult
  DeepSeek and show terminal demos before deciding: slow circle rotation,
  a compact spinner, or retaining the static indicator. Keep blocked,
  idle and done distinguishable and static; motion must not imply actual
  progress. Keep cell width, row alignment and hit rectangles stable. Animate
  only visible active indicators in the client, with no background endpoint
  requests or additional server/PTY work; support disabling motion with a
  positively named option if implemented. No implementation approved yet.
  Consulted DeepSeek 2026-09-30: keep static `◐` as the default for now;
  demo opt-in motion on only the focused/selected working row before
  considering broader animation. Compare a slow normal/dim color pulse
  (1 second per state) with circle rotation `◐ ◓ ◑ ◒` (500 ms per frame).
  A pulse avoids glyph-width changes but needs theme/contrast checks;
  rotation requires font and one-cell-width verification. Avoid a busy
  compact spinner by default. Stop animation immediately on blocked,
  idle or done, and pause it when hidden/unfocused or motion is disabled.
  Verify one shared timer, no ticks without visible animated indicators,
  unchanged mouse targets and idle CPU behavior. Demo both variants before
  choosing; this is a recommendation, not a decision to implement.
  - User follow-up: consider replacing the hourglass with a two-frame
    vertical circle `◒/◓` or horizontal circle `◐/◑`. Clarify whether this
    targets the waiting-on-job hourglass or the working indicator; keep
    those states distinguishable. DeepSeek recommends demoing `◐/◑` first,
    alongside four-frame rotation `◐ ◓ ◑ ◒`: two-frame alternation may
    look like blinking rather than rotation. Its suggested 250-400 ms per
    frame is only a prototype starting point, not a measured result.
    Verify font rendering, baseline and cell width (including CJK), and
    retain a static fallback when motion is disabled. No variant chosen.
  - User follow-up 2026-10-01 (screenshots): `◐` for a working agent does
    not animate today, and the hourglass `⧖` should become an animated half
    circle everywhere it appears. Where it appears now (from the code): the
    tab line's state icon when the tab waits on a job (`AgentMark::WaitsOnJob`,
    mauve, `src/client/shell.rs`); the running count `⧖ 1` next to `!6` and
    `✓2` in tab lines and space rows (`tab_groups.rs`, `ui/sidebar.rs`); a
    running job's square (`tab_groups.rs`); the job pane footer
    (`job_footer.rs`); the tab bar; the context menu item `⧖ N`.
  - Consulted DeepSeek, Claude Opus 5.5, GPT sol 6.1 and Gemini (low/high),
    2026-10-01. Agreed: one family of half circles; one shared client timer
    whose deadline is `None` when no animated glyph is drawn (collapsed
    sidebar, unfocused client, `ui.animations = false`); frame from the
    monotonic clock (`(now / period) % 4`) so all glyphs stay in step and
    missed frames are skipped; the glyph is one cell in every frame, so
    counts, columns and hit rects do not move; blocked, idle and done stay
    static; no server requests. Proposed frames (not decided): working
    `◐ ◓ ◑ ◒` clockwise at 125-200 ms in the working colour; waiting on a
    job and running-job counts `◐ ◒ ◑ ◓` counter-clockwise at 250-400 ms in
    mauve (Opus, GPT, Gemini low). DeepSeek instead: ping-pong `◐ ↔ ◑` at
    400 ms. Gemini high: quadrant circles `◴ ◵ ◶ ◷` for jobs, which the
    others reject (weaker font coverage, reads as 25%). Static fallback when
    `ui.animations = false`: working `◐`, job `◑` in mauve. Risks: `◐◑` are
    East Asian Ambiguous width (wide in CJK terminals), screen readers, ssh
    and tmux bandwidth (diff only). GPT: direction and colour alone are weak
    cues, so keep text labels in details. First step: a Python demo of both
    loops side by side, monochrome and reduced motion.
  - Done 2026-10-01 at the user's request (committed; `just check` passes;
    no Python demo was made, the user chose the implementation directly):
    `ui.animations` (default true). Working `◐ ◓ ◑ ◒` clockwise, 160 ms a
    frame; a running job `◐ ◒ ◑ ◓` counter-clockwise, 320 ms a frame, still
    mauve; the hourglass `⧖` is gone from the client UI (state icon, counts
    in tab lines and space rows, squares, job footer, tab menu, the legacy
    `ui` sidebar token). Without animations: `◐` working, `◑` job. One timer
    deadline at the next frame change, only while an agent works or a job
    runs (`motion_active`, computed in compose); `ui::motion` holds the
    frames and a thread-local phase set around each compose, so the many
    `status_icon` call sites need no new argument. Not changed: the plugin
    texts (`herdr-job` labels for old builds, the `$jobs` token, README and
    plugin docs still say `⧖`); the confirm-close dialog text, built when it
    opens, shows the static `◑`; Dots style keeps `●`. Open: a live look at
    the cadence in a real terminal, ambiguous-width terminals, the 100 ms
    wake-up that already existed (the new deadline only adds the exact frame
    boundaries).
  - Installed as `b4c56cec` and the speeds confirmed by the user on a live
    demo job ("ok", 2026-10-01): installed build, working clockwise at 160 ms
    and job counter-clockwise at 320 ms.

- [ ] Consider adding a subtle gradient in the empty space between the job
  indicators and the next tab in the sidebar (screenshot, 2026-09-29 23:53).
  Show several visual variants in the terminal before choosing one; generate
  the demos with Python, as Claude did previously.

- [ ] Handoff 2026-09-29 (from the Claude session; its limit ran out). Read
  AGENTS.md first: consult GPT-6 Astra + DeepSeek on design choices,
  `herdr-job` for anything over a minute, `just check` before committing,
  commit each stage, build release and ask before `scripts/herdr_live.sh
  install` (it disconnects clients).
  1. Done and user-confirmed 2026-09-30 (`d6fee21c`): restored the footer,
     verified live scrolling to line 1 of 200, and fixed narrow-width and
     growing-pane footer regressions. Original scope: move the line from the
     job pane's first row to its last row. Why: the header set a scroll
     region 2..N, and lines scrolled off a region whose top is not row 1
     never reach the scrollback, so job output cannot be scrolled up. The
     footer (region 1..N-1) keeps scrollback. Keep the same content
     (` ← `, state, name, `--why`, origin, job id, ` × `): in
     `plugins/job/herdr-job` turn `Header` back into a footer on the last
     row; in `src/client/shell/mouse.rs` `job_header_click` match the
     pane's last row instead of its first; update
     `the_job_headers_ends_go_back_and_close` and plugins/job/README.md.
     Test live: a job printing 200 lines must scroll back to line 1.
     Then add a TODO: long term, herdr draws the line as client chrome in
     a reserved row (DeepSeek), which needs `--why`/job id in a new codec;
     a click on the footer's ends while a full-screen program (vim, less)
     runs in the job would close it (DeepSeek's warning), so consider
     skipping the click mapping on the alternate screen.
     - [ ] Long term: draw the job status as client chrome in a reserved
       row; expose `--why` and job id through a new codec (DeepSeek).
       Consider skipping footer-end click mappings on the alternate screen:
       a full-screen program such as vim or less could otherwise be closed
       by a click intended for its own bottom row.
       Done 2026-10-01 for the legacy PTY footer (committed): its end
       buttons are ignored while the job's pane is on the alternate screen.
       The client-chrome footer is a reserved row outside the pane, so a
       full-screen program never shares it and needed no change. Still open:
       the long-term client-chrome row with `--why` and the job id in a new
       codec.
  2. Done: points 8-14 walked through and accepted by the user on
     2026-09-29/30. Job square tooltips now share the 450 ms dwell
     (`df8cf902`), installed and user-confirmed. Original walkthrough:
     the rest of what was done on 2026-09-28/29
     (points 1-7 confirmed): 8 a sorted spaces list (name/prio) holds its
     order while the pointer is over it; 9 the `shapes` indicator style
     (default: `◐` working, `◉` blocked, `●` done, `○` idle, `◷`→`⧖`
     waiting on a job); 10 the `+` on a space's name line opens a tab
     there; 11 tooltips: cut tab label (450 ms) and the build line; 12 the
     spaces list keeps its top row on the same space when squares above
     fold; 13 with `ui.toast.delivery = "system"` and the window focused,
     herdr's own toast shows instead; 14 the notification history `✉N`
     at the right of the spaces header. Show each, ask "ok?", fix what I
     reject (with a terminal demo in Python when it is about looks).
  3. Open items added on 2026-09-29, below in this file: the job square
     tooltip delay (same 450 ms as tab labels; consult first), the
     regression "closing a tab asks to close the space, cancelling leaves
     an odd highlight" (my screenshots were the wrong ones; ask me to
     reproduce), the upstream "update ready" badge in fork builds, the
     flaky `federated_client_starts_without_local_and_survives_its_restart`
     under a full `just check`, a per-job menu on a square (open, close).
     Terminal demos from that session: scratchpad scripts are gone; write
     new ones as needed (run them with `herdr-job run --keep`).

Order consulted with DeepSeek, GPT-6 Astra and GPT-6 Luna on 2026-09-26.

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
    Done 2026-09-29: a `shapes` indicator style, the fork's default
    (settings > indicators offers dots, symbols, shapes); a waiting-on-job
    mark is `⧖` there, as in symbols (was `◷` until 2026-09-29: at a small
    font it read as a moon, close to `◐` working; GPT-6 Astra and DeepSeek
    both picked `⧖`, the job line's running mark). Consulted (GPT-6 Astra, DeepSeek):
    both chose a new style over changing `dots`; both warned `◉` and `●`
    blur at small font sizes (DeepSeek: use the symbols' `×` for blocked);
    kept `◉` as decided, the colour differs too.
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
  - 2026-09-28: with `spaces.agents` the space row drops its own aggregate
    state icon (redundant next to the agents' icons), for every space, also
    those without agents, so the name does not shift as agents come and go;
    the name starts where the icon was. Consulted (GPT-6 Astra, DeepSeek):
    Astra proposed this; DeepSeek proposed keeping the icon on spaces without
    visible agents and reserving the column for the chevron. When the
    chevron lands it takes the name's place in front of it.
  - 2026-09-28: the job line under an agent counts the tabs nested under its
    tab by their status (`⧖ 1 !1 ✓2`), not herdr-job's `$jobs` pane token,
    which expires and goes with its pane; a tab with several agents shows
    them under the first. The space's `tab_jobs` leaves those tabs out, so
    each job shows in one place and the space row keeps only jobs without a
    listed agent (its pane closed, filtered out, a status set by hand).
    Consulted (GPT-6 Astra, DeepSeek) on dropping `tab_jobs` with agents
    listed: both said no while the two counts come from different sources
    (a failed job showed only as the space's `!1`); hide it only for jobs
    shown under an agent, matched by tab id, never by subtracting counts.
  - 2026-09-28: the remaining jobs (no listed agent) moved from the space row
    to an `other jobs ⧖ 1 !1` line below the agents: they matter less than
    the agents' own. Consulted (GPT-6 Astra, DeepSeek), both agreed on:
    `other jobs`, not `unowned` (the owner may just not be listed); label at
    the agents' icon column, dimmed, counts coloured; running and failed
    only; a click focuses the first failed tab, else the first running; a
    space without listed agents keeps the counts on its row. When the
    chevron collapses a space's agents, its counts go back to the space row.
  - 2026-09-28: tasks fall back to the agent's terminal title (was always
    `claude · no task`). Decided by me instead of the header toggle above:
    the "spaces" title is now `cust  name ↑  prio ↓`, sorting the spaces
    themselves (cust = manual order, the only mode that drags; prio = most
    urgent agent in the space); clicking the active button flips its
    direction; a client preference, keyboard navigation follows it; the
    multi-machine sidebar keeps the manual order. Consulted (GPT-6 Astra,
    DeepSeek): both preferred keeping "spaces" with a dropdown and an agent
    list for priority; overruled. `ui.sidebar.show_agents_panel = false`
    hides the old panel (kept in code for cheap rebases). Still open: the
    attention counts (`◉1 ●1`) have no place in the header now; the
    multi-machine sidebar.
  - Done 2026-09-28: a sorted list (name or prio) holds its order while the
    pointer is over it: the order drawn last stays, new spaces come last,
    closed ones drop out, and leaving the list (or the window losing
    focus) applies the live order; keyboard navigation follows the held
    order. The sort header is outside the list, so clicking it re-sorts at
    once. Consulted (GPT-6 Astra, DeepSeek): both wanted a true freeze
    (no re-sort on real state changes either) and keyboard order to match;
    Astra wanted name frozen too (chosen), DeepSeek prio only.
  - Missing (screenshot 2026-09-28): a sort button for the agents listed
    under each space, like the one for spaces. Their order still comes from
    the hidden agents panel's `agent_panel_sort` (config only, no UI).
    Consulted (GPT-6 Astra, DeepSeek, 2026-09-28): Astra proposed one global
    second row under the spaces header, `agents  tab  prio`, shown only with
    `spaces.agents`; DeepSeek proposed no new control, with agents following
    the space sort key. Decided: the second row (it is what I asked for, and
    urgent spaces first with agents in tab order is a valid combination).
    Two modes, no direction toggle: `tab` (tab bar order; never reorders the
    tabs) and `prio` (blocked > done-unseen > working > idle, ties by tab
    order, not by the latest state change, which reshuffles on every
    change). Freeze the order while the pointer is over the list and apply
    it on leave; take the clicked agent from mouse-down, so a re-sort cannot
    make the release hit another row; keep the selection by pane id. A
    client preference like the spaces sort; `agent_panel_sort` only seeds it
    when no preference is saved.
  - Colour the agent rows under a space like the tabs: blue for the agent
    selected in its space, grey for the others. Today only the globally
    focused agent's task is `text`, the rest `subtext0`, barely different.
    Consulted (GPT-6 Astra, DeepSeek, 2026-09-28): both: selected means the
    focused pane of the space's active tab (other agents split into that tab
    stay grey; none is blue when that pane runs no agent); blue is accent
    foreground, bold, on the task text only, grey is `overlay1`; no accent
    background (it fights the grey selected-space row and hides the state
    colours in ~28 columns); the state icon keeps its colour and the jobs
    line stays secondary. They differ on background spaces: Astra shows
    their selected agent blue too, which matches the request ("selected in
    its space") but needs a new optional per-agent flag from the server
    (`focused` is global; generation-1 codecs are frozen, so a compatible
    extension, falling back to `focused` on older servers); DeepSeek shows
    blue only in the current space, derivable from `focused` with no
    protocol change. Decided: every space shows its selected agent blue
    (Astra's), not only the current one.
  - 2026-09-28: decided to replace the agents under each space with plain
    vertical tabs, which supersedes the agents sort row, the per-agent
    selected flag, the `other jobs` line and the agent colouring above.
    `ui.sidebar.spaces.tabs = true` replaces `spaces.agents`: one line per
    top-level tab in tab order (plain shells too; a tab with several agents
    is one line), with the tab's state icon and the tab bar's label. Job
    tabs nested under a tab are not listed; clicking the tab enters its
    group's last focused tab and the child row at the top shows them. The
    line ends with the running and failed counts of its nested jobs
    (`⧖1 !1`), so a failed job in a background space stays visible;
    truncate the label first. The active tab of every space is accent
    foreground, bold (its group: an active child marks its parent's line);
    `active_tab_id` gives this with no protocol change. With vertical tabs
    on, the main tab row is hidden and the child row takes the top. A
    collapsed worktree group shows no tabs. Consulted (GPT-6 Astra,
    DeepSeek): both wanted some job signal (Astra counts, DeepSeek a
    single `!` on failure only; counts chosen), agreed on hiding the main
    row, listing shells, and the new option name. DeepSeek: if middle-click
    close comes, refuse it when the tab has running jobs.
  - Done 2026-09-28: the vertical tabs, and a disclosure triangle in front
    of the space's name (`▼`/`►`, grey, two-column hit) that hides its tab
    lines, as in tree-style tab lists; a worktree parent's triangle
    collapses its child spaces and tabs together, replacing its right-edge
    chevron. Consulted (GPT-6 Astra, DeepSeek): both chose `▼`/`►` (not
    `▶`, which has an emoji form) and a dim colour; Astra merged the
    parent's two collapses, DeepSeek wanted them separate. Hiding the tab
    rows is done (job squares, below).
  - Done 2026-09-29: a dim `+` at the right end of every space's name line
    (2-column hit) opens and focuses a new tab in that space, whichever
    space is focused, and expands a collapsed space; the drag grip moved a
    column left, with a blank column between them, so the name line keeps
    four columns free. Consulted (GPT-6 Astra, DeepSeek): Astra wanted it
    always visible (chosen: a space that is not focused needs it most),
    DeepSeek on hover only and apart from the grip (the gap column).
  - Done 2026-09-28: only the tab lines have a background, in the tab
    bar's colours, from the tab indent to one column before the right
    edge: inactive `surface0`, the focused space's active tab accent-filled
    (icon and counts in its text colour), other spaces' active tab the
    accent tint; the focused space lost its grey block. Tabs without an
    agent show `❏` (U+274F), job counts are right-aligned. Chosen from
    mockups and real-terminal demos; tried and rejected gaps between tabs
    (terminal cells fill the whole row; an underline in the background
    colour or an empty row were the options). Consulted (GPT-6 Astra,
    DeepSeek): both chose `surface0` (text contrast 4.66:1), preferred no
    icon for agentless tabs, then `▣` (Astra) or `❏` (DeepSeek). Open
    from the consults: white on accent `#4078F2` is 3.9:1; on the accent
    fill working, done and waiting all show a white `●`.
  - Decided 2026-09-28 (mockup
    https://claude.ai/artifact/8Bu831GPSzW7F5kfoQ1U3W): job squares replace
    the horizontal tab rows. Both rows go (main and child); a tab's job tabs
    (its child tabs) show as squares on the lines under its vertical tab,
    in start order, wrapping, 3 columns each (` ⧖ `, glyph in the state
    colour on `surface0`, bold `!` and `✓`) with a 1-column gap. The open
    job's square gets the accent tint (the parent tab's tint), glyph keeps
    its state colour; its parent tab line is tinted too. Click a square to
    open its job, click the open square again to go back to the parent tab.
    Middle-click closes; a running job asks first (`Stop and close` /
    Cancel). The `⧖1 !1` counts stay at the end of the tab line, and on
    the space row when the space is collapsed. The squares are folded by default: clicking
    an inactive tab only opens it; clicking the tab you are on unfolds its
    squares, clicking it again folds them (the counts stay); with a job
    open, clicking the parent tab goes back to the agent. Hovering a
    square names it (and a failed job's exit code) on the sidebar's bottom
    line. A succeeded square closes after 10 s, never while it is open or
    while the pointer is over the sidebar, so squares never shift under the
    mouse. No drag and drop. With the sidebar hidden there is no job
    navigation for now: show the sidebar to switch.
  - Over the open job one top line: ` ← `, the state glyph, the job name,
    `--why`, the agent and space that started it and the job id (cut from
    the right when narrow), and ` × ` at the right end. herdr-job's pinned
    footer goes (the top line holds all of it).
  - Done 2026-09-28: the squares (folded by default, client-local, not
    saved), both tab rows hidden with `spaces.tabs`, square clicks and
    middle-click close, and the top line. herdr-job draws the top line
    itself as a pinned first row (a scroll region, as the footer was), and
    herdr only turns clicks on its first and last three columns into back
    and close, for a focused tab with a parent and a status. Consulted
    (GPT-6 Astra, DeepSeek) on where the line lives: Astra wanted a
    herdr-drawn row reserved while the workspace has nested tabs, with
    `--why` and the job id sent to clients (a new codec: generation-1
    codecs are frozen); DeepSeek wanted herdr-job's own row (no resize, no
    protocol change). Chose DeepSeek's: a pane-owned row can be wiped by a
    program that clears the screen, as the footer could. Both: unfolded
    state client-local, not in the saved collapsed set; measure the square
    rows once for layout and drawing, and again with the scrollbar column
    when the list overflows. The follow-ups (hover name, held slots,
    multi-machine squares) are done below.
  - Changed 2026-09-28 (mockup updated, same link): a disclosure triangle
    right before the counts, `► ⧖ 1 !1` (`▼` unfolded, dim grey, inside
    the fill), folds and unfolds the squares; its hit runs from the
    triangle to the fill's end. The rest of the line always opens the tab
    itself, also from one of its jobs, never the job its group had open
    last. A tab with only succeeded jobs shows the triangle alone; the
    label is cut first, then the counts, the triangle last. Squares start
    where the fill starts (column 5), not under the state icon. Consulted
    (GPT-6 Astra, DeepSeek): both preferred this to clicking the active tab
    (one meaning per target), the triangle on the right inside the fill
    (the icon column stays the agent's state, and it cannot pass for the
    space's triangle), no auto-unfold of failed jobs (it moves rows under
    the pointer). Succeeded-only: Astra the triangle alone (chosen),
    DeepSeek nothing.
  - Changed 2026-09-29 at my request: the tab lines' fill (and the square
    rows) reach the right edge, level with the space name line's `+`,
    instead of stopping a column short.
  - 2026-09-29: the square glyphs keep the theme's own status colours.
    Compared in a terminal demo against darkening them to 3:1, 3.5:1 and
    4.5:1, mixing toward the text colour, more saturation, a darker tint
    and an accent frame (consulted GPT-6 Astra and DeepSeek: both chose
    darkening to 3:1); I chose the original colours.
  - Bug (2026-09-29, screenshot): a tab whose only job succeeded (kept
    open with `--keep`) shows the `▼` triangle but no count, since the
    summary counts only running and failed jobs; it should count succeeded
    ones too (`✓1`). Fixed the same day: the line counts `⧖ !` and `✓`.
  - Changed 2026-09-29 at my request, after a terminal demo of five
    placements: a hovered square's job is named at once in a tooltip on
    the square's row, right of the square, instead of in place of the tab
    line's label (cut at ~12 columns). It takes no hover, so moving onto a
    square it covers names that job; leaving the squares hides it.
    Consulted (GPT-6 Astra, DeepSeek): both wanted it past the sidebar's
    edge so it covers no square, and no label swap; Astra with the 450 ms
    dwell, DeepSeek at once (chosen). The tooltip then lost the glyph (the
    square shows the state) and took the square's fill, so the two read as
    one.
  - Changed 2026-09-29 at my request: a tooltip's text starts where its
    target's text does (its padding column sits left of the target), a
    cut tab label's tooltip keeps the line's own fill (tint, grey), and a
    tab line's fill runs under the scrollbar, whose thin `▕` otherwise left
    a white gap after it.
  - Changed 2026-09-29 at my request, after a terminal demo of seven ways:
    unfolded squares are followed by an empty row, so they do not run into
    the next tab line. Consulted (GPT-6 Astra, DeepSeek): Astra wanted the
    squares indented under the label, DeepSeek the parent's fill behind
    them (both to spend no row); I chose the empty row.
  - Bug (2026-09-29): the `new` button at the bottom creates a space and
    scrolls the list to it, but a new tab (`+` or the new-tab key) in a
    space low in the list does not scroll to the new tab line. Fixed the
    same day: a change of the focused tab, not only of the focused space,
    reveals it in the list.
  - Done 2026-09-28: a succeeded job's tab does not close while it is the
    focused tab (herdr's `focused`, the tab shown); herdr-job checks every
    2 s and closes it once you leave it.
  - Done 2026-09-28: hovering a square names its job (glyph and label) in
    place of its tab line's label, the nearest stable row. A cap of three
    square rows with a `+N` slot was tried and removed the same night at
    my request: every square shows, and the list scrolls to them.
  - Done 2026-09-28: the local spaces list scrolls by rows, not whole
    spaces, so a space taller than the list scrolls through to its last
    square; the wheel moves three rows. A space cut at the list's top or
    bottom is drawn off screen and its visible rows copied (only those
    one or two spaces per frame); its hits are moved and clipped. Every
    space's row span, drawn or not, goes into the hit map, so space drag
    and drop and revealing a space work with a space scrolled half out.
    Revealing the focused space shows its name row and its active tab line
    (or the open job's square), and only the deeper one when both do not
    fit. Consulted (GPT-6 Astra, DeepSeek): both wanted row scrolling and
    a renderer that takes a row offset instead of an off-screen copy;
    chose the copy for the one or two cut spaces, as the renderers draw
    into a rect. The multi-machine sidebar scrolls by rows too (done the
    same night), so both use one unit. The local list also keeps its top
    row on the same space and row when rows above come or go (squares
    folding or closing), unless it was scrolled since (DeepSeek's anchor).
    Still open: space drag and drop in the multi-machine sidebar still
    works from the drawn spaces. Consulted
    (GPT-6 Astra, DeepSeek): Astra chose the sidebar's footer for the name,
    DeepSeek the tab line (chosen: next to the pointer, no chrome hidden).
    For a space taller than the list, Astra wanted the list to scroll by
    rows instead of whole spaces, DeepSeek the cap now and row scrolling
    later (chosen: row scrolling touches drag and drop, reveal and the
    scrollbar); the cap was then dropped for row scrolling (below).
  - Done 2026-09-28: while the pointer is over the spaces list, a job tab
    that closes (a success after 10 s, a close elsewhere) leaves a blank,
    inert slot, so the other squares do not move under the pointer; new
    jobs come last; leaving the list (or the window losing focus) closes
    the gaps. The order is the one drawn last frame, not a snapshot taken
    when the pointer enters (DeepSeek: no enter edge to miss). A blank slot
    takes no click, so a middle-click there cannot fall through to closing
    the space (Astra). A tab line that closes still moves the rest.
  - Done 2026-09-28: the multi-machine sidebar shows squares for the
    active machine's tab lines, which now take clicks like the local
    sidebar's (focus, fold, squares); another machine's lines stay
    folded and select its space. The unfolded tabs are kept per machine.
    Both consults: active machine only, keyed by machine.
  - Consulted (GPT-6 Astra, DeepSeek, 2026-09-28): both called the squares
    fine but removing the rows risky (no navigation with the sidebar
    hidden, keyboard). Both wanted a per-tab number in the square (`1⧖`,
    for `Alt-1…9`); I chose the glyph only. Both found "click the open
    square again goes back" surprising; kept because I asked for it, with
    `←` in the top line as a visible way back. Both: no drag and drop,
    keep counts on a collapsed space, never reflow squares under the
    pointer (a middle-click could stop the wrong job). DeepSeek wanted the
    footer dropped (chosen); Astra wanted the top line and footer to split
    the fields.

- [ ] Add a model-selection review workflow for the consult/ask skills.
  - Use official model announcements, CLI release notes and authentication /
    subscription availability first. Terminal-Bench and SWE-bench Verified /
    Pro are candidate sources, not automatic rankings for a read-only
    consultation task. Record benchmark version, date, model snapshot and
    harness/agent settings; do not compare unlike evaluation setups.
  - Treat these user-supplied links as unverified leads, not evidence that
    Opus is better than Sonnet:
    https://www.reddit.com/r/Anthropic/comments/1wso4lj/silly_question_if_sonnet_opus_55_is_better_than/
    https://x.com/BalegaNorbert/status/2102451570608853211
  - Additional sources read in the browser on 2026-10-01, including their
    attached images (claims not independently reproduced):
    https://x.com/BalegaNorbert/status/2102280368909111497 compares dated
    MiMo V2.6 Command Code/OpenCode promotions, including 72-hour / one-week
    windows. Track plan, provider, expiry, actual quotas, overage and normal
    non-promotional pricing; an offer multiplier is not a quality score.
    https://x.com/BalegaNorbert/status/2102055662087786534 claims Qwen 27B
    reproduces an earlier proprietary frontier about six months later.
    Its chart attributes scores to Artificial Analysis Intelligence Index
    v4.3, with current re-evaluations plotted against original release dates
    and roughly 4-bit models in the single-24GB class. Verify the primary
    model pages, index methodology, model/version and deployment details.
    Neither score differences nor parameter counts establish the post's
    "1000x" claim or parity for coding consultations.
  - Also read on 2026-10-01:
    https://www.reddit.com/r/singularity/comments/1wspt5z/gpt6_sol_vs_sonnet_55_at_the_same_cost_per_task/
    The author plots claimed Artificial Analysis scores against API cost per
    task at different effort settings: Sol is claimed more efficient at
    overlapping budgets, Sonnet has a higher maximum-effort ceiling. The
    post separately cites Terminal-Bench 4.0 scores; those are not the same
    metric as the composite Intelligence Index. Verify primary data and
    token accounting (including reasoning/cache) before adopting conclusions.
    Equal token prices do not imply equal task costs, and effort labels are
    not comparable across providers. API dollars/task do not establish
    subscription quota consumption. Do not transfer GPT-6 Sol results to
    GPT-6.1 Sol without matching the exact model snapshot. User comments
    and unverified scores are leads, not grounds for switching defaults.
  - Keep quality, total cost and delivery route separate. Tag CLI subscription,
    hosted API and local weights distinctly; provider wrappers can alter
    harnesses, privacy terms and quotas. For local candidates record hardware,
    quantization, memory/context headroom, latency and throughput; local
    serving is not cost-free merely because there is no API invoice.
    Evaluate read-only consultations separately from tool-using coding
    agents. No purchases, default switches or new provider integration based
    solely on these posts. Verify offers again at decision time.
  - Consulted DeepSeek and Gemini (low/medium/high), 2026-10-01: distinguish
    temporary promotion value from quality; verify primary benchmark data
    and local consultation usefulness, with delivery/privacy constraints.
    Do not treat a screenshot, composite chart or marketing multiplier as
    a reproducible result.
  - Before switching a skill default, verify the exact model through its
    subscribed CLI and run a small representative local evaluation. Compare
    accepted/unique findings, incorrect advice, latency and quota consumption
    using consult-stats. Record the decision and a rollback path; do not
    auto-switch defaults based on leaderboard or social-media claims.
  - Consulted DeepSeek on 2026-10-01: prioritise primary sources, exact model
    identities and local usefulness; preserve explicit selection and report
    unavailable models without silent fallback. No scheduled polling or
    paid benchmark/model calls until the workflow is designed and approved.
  - New leads (user, 2026-10-02), folded in as unverified leads, not grounds to
    switch a default:
    - SuperGrok's "160x more in the subscription than in tokens"
      (https://x.com/PawelHuryn/status/2105703147184239042): a cost/access
      ratio, not a quality signal. It compares a flat subscription's
      theoretical token ceiling with marginal API price and ignores rate
      limits/fair-use, that a sub may be a loss-leader, and that real
      consumption sits far below the cap. "How many tokens do I get" in Claude
      Max 5x vs 20x vs a GPT sub is throughput (how many consultations), not
      competence; tokens of different models are not one unit of useful work.
    - Artificial Analysis AA-Omniscience
      (https://x.com/ArtificialAnlys/status/2105392625788637299): Gemini 4 Argon
      15% hallucination (lowest among models scoring 45+ on the Intelligence
      Index), vs GPT-6 Astra 51% and GPT-6.1 Sol 54% at max effort. A
      general-knowledge hallucination benchmark, not reasoning over an unknown
      codebase. Low hallucination suggests better uncertainty calibration (more
      "I don't know / show me the file", fewer confident false positives),
      genuinely useful for a devil's advocate, but it does not transfer the
      percentages to code review, and a cautious model can also miss more real
      bugs. One recent third-party score is a lead, not a default switch.
    - Consulted GPT Astra (9cf5c878) and DeepSeek (c883180b) 2026-10-02 as
      devil's advocates (both agree): neither argument measures quality. The
      deciding metric stays per-consult verifiable value-add — accepted/unique
      findings, plus false-positives-per-accepted, finding severity, cost per
      accepted finding, and calibration (does it admit "I don't know" and ask
      for evidence) — measured by blind A/B on the same unknown repo with the
      same prompt, and by also scoring misses on cases with known bugs, never a
      leaderboard or a subscription multiplier.
  - More leads (user, 2026-10-02):
    - TerminalBench 4.0 cost/task (https://artificialanalysis.ai/evaluations/
      terminalbench-4-0): user cited Grok 4.7 (xhigh) $14.6, GPT-6.1 Sol (max)
      $1.82, Claude Opus 5.5 (high, with fallback) $5.12. More relevant than
      AA-Omniscience (agentic coding, not trivia) but still not our role:
      TerminalBench is a tool-using agent that solves tasks, we run a read-only
      second opinion. Cost without the paired score is half the picture — on the
      page's score chart the top is Claude Sonnet 5.5 (max, fallback) 63.6%,
      then Opus 5.5 59.6% (Sol's score not surfaced in the fetch), so "cheapest"
      is not "best". Effort labels (xhigh/max/high) are not comparable across
      providers, "with fallback" means the figure is not pure Opus, and API
      $/task is not our subscription-quota consumption (consults bill to the CLI
      subscription).
    - "Space Bunny Alpha", free now on OpenRouter
      (https://openrouter.ai/rankings#leaderboard-table), guessed to be
      MiniMax-M3.1 (https://www.reddit.com/r/SillyTavernAI/comments/1wo8csn/
      comment/pbmo076/): a cloaked model. "Free" is a promo / data-collection
      phase, not a quality score; the identity is a Reddit guess, so it fails
      this item's "exact model identity" rule and can be swapped under us
      (consult-stats could not log the real version). Privacy red flag: consults
      send code, and `-r` repo mode sends the whole checkout including untracked
      files, to an unknown provider with unknown retention (MiniMax is a China
      lab, like DeepSeek). Worth an A/B only through a route that pins the exact
      model id, and only after deciding what code it may see; never the default,
      never for `-r` with secrets.

- [ ] Run the untrusted/cloaked OpenRouter consult (`ask-bunny`, Space Bunny /
  MiMo) so a secret can never reach the logging provider (user, 2026-10-02).
  - Done so far (committed `1d71dabf`): `ask-bunny` runs `ask_openrouter.py`
    under macOS `sandbox-exec` (`bunny.sb`, `allow default` + `deny file-read*`
    of `~/.pi`, `~/.ssh`, `~/.config`, `~/personal_projects`, `*.env`/`*.key`/
    `credentials`/`auth.json`), token passed via `OPENROUTER_BEARER` from
    outside the sandbox, clean cwd `/tmp/bunny`. Verified all those reads return
    `PermissionError`. This removes the path where the model/script reads a
    secret itself.
  - NOT closed by the sandbox: the orchestrating agent can paste a secret into
    the prompt (`ask-bunny "$(cat ~/.env)"` — the `cat` runs in the caller's
    shell, before the sandbox).
  - Decision (web research + consulted GPT Astra `117cec6b` and DeepSeek
    `223dab7f`, 2026-10-02, agree): the only real close without a human
    approval gate (which the user rejected) is capability split — the component
    that talks to the model has no secrets, the component with secrets has no
    channel to it. Egress DLP / secret-value registry / canary tokens are a
    sensor, not a wall (base64, split, paraphrase, "key starts with sk-, 40
    chars" bypass them); keep only as defense-in-depth.
  - Build options (not built; both need a system change, so ask first):
    - A separate macOS user `agentuser` (`dscl`), Bunny consults run as
      `sudo -u agentuser` against a cleaned/secret-free workspace, so the
      primary user's `~/.env`/Keychain/`~/.pi` are unreadable at the kernel
      level — nothing to paste. Secret-needing work stays with the primary user
      behind a narrow broker (unix socket, whitelisted operations) that returns
      a result, not the value.
    - Or run the whole agent in a Linux VM (Colima/Lima) with no home mount,
      only a cleaned repo copy; credentials outside the VM via the broker.
  - Irreducible (both models, explicit): while an agent can both read a secret
    and send arbitrary text to the untrusted model, there is no 100% guarantee
    without a human gate or removing its read access. So the chosen direction is
    removing read access (B), not policy-only (C).
  - Sources: specstory agent-secrets; dev.to "never see your API keys"; DZone
    "4 ways agents exfiltrate secrets"; github.com/fabriziosalmi/aidlp.

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
    slide animation, the priority-view rule, and the local sidebar only
    (with remote endpoints the aggregate sidebar keeps the old look).
  - Done 2026-09-28: auto-scroll. A space dragged onto the list's top or
    bottom row (or past it) scrolls the list a row every 60 ms, retargeting
    the drop with the pointer where it is, and stops back inside the list;
    local sidebar only (it scrolls by rows).
  - Changed 2026-09-29 at my request: while a space is dragged the header
    keeps its sort buttons; the `→ before …`, `→ end` and `no change`
    hints went (the live reorder shows where it lands). Left:
    `release cancels · Esc` outside the list and a refusal's reason. The
    own order's button is `manual`, not `cust`, and the header's buttons
    are one column apart, so `manual name ↑ prio ↓` leaves room for `✉N`.
    Consulted (GPT-6 Astra, DeepSeek): both said to keep only those
    exceptional hints; Astra chose `manual` (chosen), DeepSeek `custom`.
  - Done 2026-09-29: right-click anywhere on a tab line (its summary
    too) opens the tab menu with a `Close jobs:` row of chips, as the line
    counts them, `⧖ 2  !1  ✓3`: a chip closes that state's job tabs (the
    statuses when clicked, not when the menu opened); `⧖` asks first
    ("Stop 2 running jobs?", naming them) and keeps the tab; chips appear
    only for states with jobs. Keyboard moves through the chips as items.
    Consulted (GPT-6 Astra, DeepSeek): both wanted one tab menu, no menu of
    its own on the summary, and "close finished jobs"; Astra also a
    confirmed "stop all", DeepSeek no stopping at all. I asked for
    succeeded, failed and running separately, as chips on one row. Still
    open: a per-job menu on a square (open, close).
  - Next (screenshot 2026-09-29): a collapsed space does not show that it
    is the focused one (the only focus mark is its active tab's fill, and
    the tabs are hidden). A collapsed space is one line: no branch line,
    its git status moves onto the name line without the branch name
    (`► herdr ↑2 ⧖ 1 !3`), and the focused collapsed space's name line
    gets the focused active tab's solid accent fill (same span as a tab
    line). Consulted (GPT-6 Astra, DeepSeek): both: fill only the focused
    collapsed space, no tint or grey on the others (nearly every space has
    an active tab, so a tint says nothing, and grey reads as a tab); give
    the collapsed line its own configurable token list (default
    `workspace, git_status, tab_jobs`) instead of merging arbitrary row-2
    tokens; the triangle, `+`, grip and job counts need readable colours on
    the accent (as the tab line's `on_accent`); hover must differ from the
    focus fill; a collapsed worktree parent whose child space is focused
    gets the fill but needs a "focus inside" cue, and its git status must
    not pass off one child's as the group's. They differ on order: Astra
    git status before the job counts (as asked, chosen), DeepSeek jobs
    first; both truncate the name first, then drop the git status, never
    the job counts. DeepSeek also wanted the branch kept for worktree
    children (rejected: their names already tell them apart).
  - Done 2026-09-28: keyboard reorder. `keys.move_space_previous` and
    `keys.move_space_next` (unset by default, e.g. `alt+shift+up/down`)
    move the focused space one place in the sidebar's own order, with its
    worktrees (a focused worktree moves its parent's family); only in cust
    sort, like dragging; no wrap at either end; the list then reveals it.
  - Drag starts only from the space's name line after a small threshold, so
    clicks, chevrons and agent/job rows keep working; a click is suppressed
    after a drag. Esc cancels. Time-based auto-scroll near the list edges.
  - Priority view: no reordering, with a hint to switch to grouped.
  - Keyboard reorder (move space up/down, whole family); none exists now.
  - The move is sent by ids (`move X before Y`); if another client changed
    the order or the anchor vanished, cancel with a notice.

- [ ] The space's name line gives no feedback that it can be dragged
  (2026-09-28). Now: pressing it changes nothing until the pointer moves;
  in prio/name sort, on a remote endpoint or on a linked worktree the drag
  is silently ignored; when the pointer leaves the list (header, "new space"
  button, another endpoint's rows) the target becomes None and every drag
  visual disappears although the drag is still active (release there
  cancels).
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), in this order:
  - Keep the drag visible outside the list: separate "drag active" from
    "valid drop target"; keep the accent bar on the source (the order may
    snap back) and put `release cancels · Esc` in the header. An active
    drag that looks idle reads as "the drag died". Astra ranked this first.
  - Explain refused drags in the header slot instead of doing nothing:
    `sort by cust to reorder`, `worktree moves with its space`, and a
    reason for remote spaces. Only after the pointer passes the threshold
    (DeepSeek wanted it on press; Astra: not on a plain click). Never
    switch the sort automatically.
  - Press feedback on a draggable name line only: a subtle pressed look
    (underline or the dim bar), not the full lifted look, which stays for
    a real drag past the threshold; releasing in place still selects. Not
    a raised background alone: invisible in 16-colour and `NO_COLOR`
    themes; glyph and position, never colour alone.
  - A grip glyph on the name line in cust sort, right-aligned (column 0
    belongs to the `▌` bar), a simple tested glyph rather than `⠿`; the
    name truncates, it never shifts. Hover works here (herdr enables mouse
    mode 1003), but DeepSeek suggests a persistent dim grip instead of
    hover-only, since tmux and some terminals drop plain motion. The grip
    must match the hit test: only the name line starts a drag.
  - Last, optional: OSC 22 pointer shapes (grab / grabbing /
    not-allowed) in terminals that support it; it must be reset on every
    exit path (drop, Esc, release outside, focus loss, quit, panic), and a
    stuck cursor is worse than none.
  - Done 2026-09-28 in the local sidebar: outside the list the block keeps
    its accent bar and the header says `release cancels · Esc`; a refused
    drag says `sort by cust to reorder` or `moves with its parent` (the
    remote reason exists but the multi-endpoint sidebar does not show it
    yet); a press on a draggable space draws a dim bar before any move; a
    grip `⋮` shows in the first column of the hovered draggable space.
    Hover-only after all: a grip on every row is noise, and the first
    column costs no width. Still open: OSC 22, and all of this in the
    multi-endpoint sidebar.
  - Changed the same day at my request: no bars. The grip `⋮` sits at the
    name line's right edge (the spacer column left of the group chevron):
    grey on hover; on press the grip and the name turn accent; while
    dragged, mauve and bold, also outside the list. Consulted models (GPT-6
    Astra, DeepSeek): both read "its colour" as the grip's, and both said
    to recolour the name text too, since a one-cell cue is lost when the
    list reorders live; never a new row background (selection and focus
    own those). DeepSeek warned that `⋮▾` side by side invites toggling
    the group by mistake; the chevron's hit cell stays separate, so watch
    for that.
    Then, also at my request: one colour, accent blue, while pressed and
    while dragged (mauve read as a git branch; a darker grey and a darker
    blue were tried and dropped).

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
    An admin key reads org-wide billing: opt-in and disabled by default.
    Use a dedicated credential file outside the repository, readable only
    by its owner (0600), containing a restricted Usage Read key. Do not
    reuse Pi's shared `auth.json`; the server alone reads this credential,
    and must never expose it in logs, errors, client snapshots or prompts.
    Separate storage is not a sandbox against agents running as the same
    OS user. The key has not been created; implementation remains pending
    an explicit user decision. Label costs as spend, not prepaid balance.
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
  - Noted 2026-09-28: the footer still has no row for an OpenAI API key
    (platform, pay-as-you-go); only Codex's ChatGPT limits show.
  - Noted 2026-10-03: show OpenAI API token usage too, not only spend
    (input, cached and output tokens, month to date), from the Admin API's
    `GET /v1/organization/usage/completions` with the same opt-in admin key.
    Key stored 2026-10-03 in `~/.config/herdr/openai-admin-key` (0600,
    one line); both `/v1/organization/costs` and
    `/v1/organization/usage/completions` answer 200 with it.
  - OpenAI spend and completion tokens done 2026-10-03: `usage.openai_api`
    (off by default, row `OA` under Codex's), key only from `usage.openai_admin_key_file`
    (refused unless owner-only), polled at most every 15 min. Consulted Sol,
    DeepSeek and MiMo (round `20261003-054331-c102`). Accepted: no env var
    (agent panes would inherit it), explicit opt-in, `input_tokens` already
    includes cached ones, label tokens "completions only" and spend
    "organization-wide", sum every result per bucket, a scope hint on
    401/403. Rejected: decimal crate (f64 over at most 31 buckets is exact
    to the cent), partial-success status per endpoint (both must succeed,
    else the last good values stay), a cross-process refresh lease.
  - Left for later: other usage endpoints (embeddings, images, audio),
    filtering by project.
  - Row code (round `20261003-114021-c950`, Sol, DeepSeek, MiMo unanimous):
    codes name vendors, so the API row is a second `OA` right under Codex,
    told apart by `$ spend` versus `%`; a future Anthropic API row is a
    second `AN`. `OP` read as a new vendor; `O$` would start a symbol class.
  - Budget: do not take a number from `[usage]`. OpenAI has
    `GET /v1/organization/spend_limit` (Sol; verified 2026-10-03: 404 "No
    organization spend limit is configured" with our admin key). When a
    limit is set, show `$4.20/20` (money first, never a bare `%` next to
    subscription percentages) and in the details "spend limit from OpenAI",
    budget used and the period end. Built 2026-10-03: the limit is read
    from OpenAI each refresh (a failed read only adds a note); no config
    number.
  - Follow-ups (round `20261003-115359-3a57`, Sol, DeepSeek, MiMo agree):
    - [x] Done 2026-10-03 (`8d5d016d`): a Usage section in the Settings overlay (reuse `ConfigEdit`
      and the reload flow) with the master switch and one toggle per
      provider, mirroring `[usage]` keys exactly. Show credential state next
      to keyed providers ("on, no key") instead of hiding them silently.
      Turning `openai_api` on shows the admin-key warning first; never a
      text field for the key.
    - [x] Done 2026-10-03 (`8d5d016d`): when `openai_api` is on and the key file is missing, the row
      says `OA setup needed` and the details say: create an Admin key at
      platform.openai.com → Organization settings → Admin keys, save it as
      one line in the configured path, mode 0600. Herdr only reads costs,
      completions usage and the spend limit. Do not promise a read-only key:
      the Admin API's key creation takes only a name and expiry (checked in
      openai-python 2026-10-03), so the key may carry admin authority.
      Nothing is shown while `openai_api` is off.
    - [ ] Later, on demand: spend per project (`group_by=project_id`, flat
      list in the details, org total and limit kept) and per line item
      (`group_by=line_item`). Never a config project filter: filtered spend
      would read as the org total next to the org limit.
    - Never: polling the other usage endpoints (embeddings, images, audio,
      vector stores, code interpreter; all answer 200) for the footer.
      Costs already include their dollars; their units do not mix.

- [ ] Review queue for agent commits, plus `herdr diff`. When an agent's turn
  ends with new commits, list them as "to review" until I acknowledge them.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-27): a plugin with a
    popup, no new core state. Uncommitted changes in the shared checkout
    cannot be attributed to one agent (neither HEAD nor file mtimes tell who
    changed what), so the unit is the commit.
  - At turn start record the session id and HEAD; at turn end find new
    commits carrying `Claude-Session: <id>`. Enqueue only when there are
    commits, not on every finished turn. Viewing the pane clears "done" as
    today; only an explicit acknowledgement clears "to review".
  - Show each commit's own patch (delta or lazygit), never
    `git diff first^..last`: with other agents committing to master the range
    includes their commits. Leftover uncommitted files get one line:
    "N uncommitted (unattributed)".
  - Sidebar token like `review 3c / 5f`; on the phone one item at a time
    with next/previous, no side-by-side diffs.
  - Open questions: the trailer is per session, not per turn, and only
    Claude adds it; Codex and pi need an equivalent (or hook-reported
    commits). Prototype a plain commit list first: maybe lazygit in a popup
    is already enough.

- [ ] Run the Windows checks for fork commits. Nobody does today: the
  Windows SDK for `just windows-lint` is not set up on the Mac (no `xwin`),
  so `just check` fails there and agents run narrower checks, and the fork
  has never had a GitHub Actions run although `ci.yml` has a
  `windows-latest` job (`just check` in pwsh plus the ConPTY smoke test).
  - Also try the Windows Claude hook live (`herdr-agent-state.ps1`,
    integration v11): the awaiting-reply instruction it prints from
    `SessionStart` and the `Bash(herdr agent awaiting-reply)` rule are
    untested there (Claude may run commands through PowerShell).
  - Local: `cargo install xwin --locked`, then `just setup-windows-cross`
    (the user accepts Microsoft's SDK license), and prove a full
    `just check` passes before the fork section of AGENTS.md requires it.
    Cross-clippy only catches compile and lint errors in `cfg(windows)`
    code; it runs no Windows tests.
  - CI: activate Actions in the fork's Actions tab and verify that a push
    to `master` really starts a CI run. Native Windows CI is the only
    runtime check (tests, ConPTY, paths), and shared TUI code can break
    there without touching `cfg` code.
  - Before activating, disable the workflows that would fail or misfire on
    the fork with `gh workflow disable` (UI state, so no rebase conflicts
    with upstream): `label-next-release-issues.yml` and
    `website-deploy.yml` have no `github.repository == 'herdrdev/herdr'`
    gate and need upstream secrets. `preview`, `release` and `pr-gate` are
    gated; the rest are PR- or path-triggered. After each upstream rebase,
    check for new workflows.
  - Ownership: the agent that pushes a commit watches that SHA's run
    (`herdr-job run -- gh run watch <id> --exit-status`) and fixes a red
    run before pushing more.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both say local
    cross-lint is necessary but not sufficient; Astra added verifying the
    activation and the per-SHA ownership, DeepSeek the post-rebase workflow
    check and that a fresh machine without the SDK fails `just check`.

- [ ] Child tab row styled like the main row. Now the main row has separate
  tabs (`surface1` background, a 1-column `panel_bg` gap between them),
  while the child row is one continuous accent-tint band with plain text
  entries split by `│` (`render_child_tab_bar` in
  `src/client/shell/tabs.rs`), so it looks like a different widget.
  - Decided (2026-09-28, after mockups): copy the main row exactly. Drop
    the band: the row background and the 1-column gaps are `panel_bg`;
    each unfocused child is drawn like an inactive main tab (`surface1`
    background, `overlay1` text, same padding); drop the `│` dividers. The
    focused child stays the only full-accent block, and the tinted parent
    above plus the `◆` entry keep the link between the rows.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28) both preferred
    keeping the band behind the tabs, so the row stays visibly tied to the
    tinted parent; I chose full consistency with the main row instead. Both
    rejected tabs in a stronger accent tint: they read as "half selected"
    and weaken the red/yellow status icons.
  - Pitfalls: truncate labels before status icons; the whole tab including
    padding is the hit target, gaps are not; red/yellow icons must stay
    readable on `surface1` in both light and dark themes.
  - Skipped 2026-10-04 while going through the list: with
    `ui.sidebar.spaces.tabs = true` (the user's setup) neither tab row is
    drawn (`show_tab_bar` in `src/client/shell/config.rs`), so this styling
    is invisible; do it only if the horizontal rows come back into use.

- [ ] Tooltips: hovering a tab shows its full text. There is no tooltip
  system yet, so build one small client-side layer first (presentation
  state, no protocol change): target id, anchor rect, lines; ~400-500 ms
  dwell, not restarted by motion within the same target; drawn last,
  clamped to the screen, display-width aware, never intercepting clicks;
  hidden on key, click, scroll, drag, modal, resize, target removal, and
  after a maximum time (a lost leave event must not leave it stuck).
  - Tabs, both the main and the child row: only when the label is
    actually truncated.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): hover must not be
    the only way to see the full text, since tmux and some terminals drop
    plain motion events (mode 1003); the rename dialog already shows it.
    Sanitize control characters in tooltip text.
  - Done 2026-09-29: the layer (`src/client/shell/tooltip.rs`: 450 ms
    dwell, drawn last on the target's row and shifted left to stay on
    screen, no hits, gone on a key, a click, a scroll, a drag, an overlay,
    when its target is not drawn, and after 10 s), used by the sidebar's
    vertical tab lines whose label is cut. Still open: the horizontal tab
    rows (shown only without vertical tabs).

- [ ] Build line (bottom left of the sidebar): hover shows the full commit
  message, click opens a modal with the full commit info (full hash,
  subject, body, author, date, dirty flag, version and channel), scrollable,
  Esc closes.
  - The data does not exist yet: `HERDR_GIT_COMMIT_LINE` holds only
    `<short hash> <subject>`. Embed structured commit metadata at build
    time (handle builds without git); never ask the git repo of the
    current space, which is another project.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): the client and
    the server builds can differ after a live handoff, so the modal shows
    both, labelled "server" and "client", and marks a mismatch. Server
    details come from a new advertised build-info method (the snapshot's
    `build_commit` stays as is); an old server shows "details unavailable",
    never the client's data in its place. Astra: the tooltip shows the
    subject only, the body belongs in the modal.
  - Done 2026-09-29: hovering the build line shows its whole commit line
    (hash and subject) in a tooltip, and both builds when the client's
    differs (`server <line> · client <hash>`). Still open: the modal and
    the build metadata it needs.

- [ ] Pin a tab: pinned tabs are marked with a pin icon (or similar) in
  the tab bar and stay at its start, before the unpinned tabs, like
  pinned tabs in Chrome or Firefox.

- [ ] Pin a space, like a pinned tab: a pin icon on the space row, and
  pinned spaces stay at the top of the spaces list. Consulted (GPT-6 Astra,
  DeepSeek, 2026-09-28), both agreed on:
  - Pinned first in every sort mode (cust, name, prio); the sort and its
    direction apply inside each tier. If it only worked in cust it would
    duplicate the manual order. Maybe a separator line between the tiers,
    so `name ↑` honestly sorts only the unpinned ones (DeepSeek).
  - A 1-cell narrow glyph (ASCII `*` or `▪`), not 📌 (double width, emoji)
    and no nerd-font requirement; in a fixed leading column, so names do
    not shift when a space gets pinned.
  - Server-owned session state (like the manual order), in the JSON API;
    the sort mode stays client-only. Pins affect every client. In the
    multi-machine sidebar pins apply per server.
  - Pin/Unpin in the space's context menu, plus a keybinding; no drag to
    pin. The icon is only an indicator (1 cell is a poor click target).
  - Worktree families are pinned whole; a child's menu says "Pin family".
    Drag in cust moves within a tier (Astra: refuse crossing the boundary;
    DeepSeek: dragging out unpins); unpinning keeps the underlying manual
    order. Pinned spaces never go into `other spaces (N)`.
  - Cost to weigh: in prio an idle pinned space sits above an unpinned
    blocked one; urgent unpinned agents need another cue (the header
    attention counts, still without a place).

- [ ] Awaiting reply for agents other than Claude (pi done 2026-10-01; the rest open), the same way as their
  integrations (user, 2026-09-28): each integration that can add session
  context (a session-start hook, an extension, a plugin) injects the same
  instruction, and where the agent has a command allowlist the install
  adds `herdr agent awaiting-reply` to it, so reporting never stops at a
  permission prompt. Integrations today: antigravity_cli, codex, copilot,
  cursor, devin, droid, grok, hermes, kilo, kimi, letta, mastracode, omp,
  opencode, pi, qodercli, qwen. Check per agent what it offers; bump each
  changed integration's version once; try each live.
  - Pi done 2026-10-01 (committed; linked into `~/.pi/agent/extensions/` by
    `plugins/pi-title/install`, active after `/reload` or a new session):
    `pi-awaiting-reply.ts` adds the instruction as a named system-prompt
    section in Herdr's TUI mode, since Pi has no command allowlist to edit and
    the managed `herdr-agent-state.ts` is overwritten on reinstall (an
    integration-version bump would also drift from upstream's numbering).
    Not verified in a live Pi session. The other integrations (antigravity,
    codex, copilot, cursor, devin, droid, grok, hermes, kilo, kimi, letta,
    mastracode, omp, opencode, qodercli, qwen) are untouched: each needs its
    own live check, which I cannot do here.

- [ ] Audit whether colours and symbols are consistent across the UI
  (sidebar, mobile layout, tabs, toasts, job statuses `⧖ ✓ !`, state dots).
  - Plan: an inventory (glyph or colour, meaning, where used), then
    conflicts (one colour with two meanings, one meaning with two glyphs),
    then a single mapping in code.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): fold it into the
    planned state-shape redesign above; one colour meaning different things
    in different contexts is not automatically a conflict. Check it without
    colour (colour-blind users, monochrome), in light and dark themes and
    narrow layouts. Generate the legend from the code, not by hand, or it
    drifts.
  - Audit (2026-09-28), conflicts by severity:
    1. `Done` is teal in `status_color` (`src/client/shell.rs`) but blue in
       the mobile summary (`mobile.rs`) and finished toasts
       (`notifications.rs`); in most themes blue equals `accent`.
    2. The default Dots style draws working, blocked, done and
       waiting-on-job all as `●`: colour alone tells them apart.
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
    job, endpoint, notification) and semantic palette roles, decided
    together with the state-shape redesign.

- [ ] A legend explaining the UI's dots and symbols (agent state dots,
  job counts like `!2` / `⧖ 1` / `✓3`, git tokens `↑4` `±7`, endpoint
  states, sort buttons, the grip, footer provider codes). Nothing in the UI
  explains them today. Ties in with the colour and symbol audit above: the
  legend should come from the same glyph/label/colour mapping.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), both: a full
    modal (the sidebar's 28 columns cannot hold explanations), opened by a
    `?` button in the sidebar header, plus a menu entry and a prefix
    keybinding; never a bare `?`, which belongs to the agent's terminal.
    Hover tooltips come later as a supplement, never the only way (tmux
    drops motion, no keyboard access, and five glyphs cannot be compared
    at once). No first-run hint (gone before it is needed).
  - Content per domain (agent, job, git, endpoint, controls, usage):
    glyph, label, one-line meaning and a swatch in the theme's actually
    rendered colour, never a colour name ("yellow" lies when a theme
    collapses peach into yellow). Explain the counts by example (`!2` two
    failed jobs, `↑4` four commits ahead); Astra: define what `±7` counts
    (files or lines) and never describe planned glyphs as current.
  - Generated from the per-domain `status_style` mapping of the audit above
    (glyph, label, colour, explanation), with a test that every state
    variant has an entry, so a new state cannot ship unexplained. Unknown
    states from older remote servers show as "unknown status".
  - DeepSeek: an "on screen now" filter at the top of the modal; a warning
    when the theme gives two states the same colour. Both: a legend exposes
    colour-only meaning (Dots draws four states as `●`) but does not fix
    it; the shape redesign must.
  - Order: they differ. DeepSeek: after the consolidation and the shape
    redesign. Astra: together with the consolidation, not waiting for the
    redesign. Astra's, I think: a generated legend follows the redesign
    for free, and it helps now, while the glyphs are most confusing.

- [ ] Refresh the README's "Fork changes" so it says how the fork differs
  now, with a small looping animation under each change.
  - Text part done 2026-10-01 (committed): the README now lists tab-line
    drag, the spaces filter, the animated glyphs (the hourglass is gone), the
    fork build's refusal of upstream updates and the `pi-title` plugin; the
    demo scripts say `consult` instead of `oracle` (the plugin was renamed;
    `record.sh` linked a `plugins/oracle` that no longer exists). Still
    open: the looping clips (animated WebP pilot), which need a recording
    session in a real terminal and browser.
  - Audit first (vertical tabs, the disclosure triangle and
    `show_agents_panel` are in the README since 2026-09-28);
    `scripts/fork_demo/README.md` still
    says "oracle stats" where the menu item is "consult stats".
  - Format (consulted GPT-6 Astra and DeepSeek, 2026-09-28): a video can't
    autoplay or loop on github.com (the sanitizer drops `autoplay`/`loop`,
    user-attachments videos are click-to-play), so use animated WebP as an
    `<img>`: far smaller than GIF, loops, Safari 14+. Avoid animated AVIF
    (patchy support). Pilot one clip in the real rendered README (Chrome,
    Safari, GitHub mobile app) and compare it with a GIF before making the
    rest. Keep the long MP4 as the full walkthrough.
  - Clips: each scene of `record.py` runnable on its own from a fresh
    state, so one changed feature re-records one clip; crop to the feature
    with enough context; 3-6 s, 8-12 fps, a hold before and after the
    action so the loop seam is calm; about 500 KB each, under 4 MB total.
    No caption bar (the bullet is the caption); alt text on every image.
    Keep scene, crop and encoder settings in the script, not done by hand.
  - Text must stay readable at README width, desktop and mobile: don't
    downscale the 104-column window below 1:1, or record fewer columns or
    a bigger font instead.
  - Storage: files under `assets/fork/` with relative links, new file names
    on re-record (camo caches). Each re-record adds its size to git
    history; if that grows, move them to an orphan `assets` branch.
  - Risk: seven loops at once are distracting and ignore reduced-motion;
    if it looks busy, use a static frame per bullet linking to its clip.

- [ ] Open a herdr tab with Cmd+T (macOS), as Cmd+W closes panes.
  - Set up 2026-09-28: dotfiles Ghostty config has `cmd+t=unbind`, with no
    replacement key (the user's choice); herdr config has
    `[keys] new_tab = ["prefix+c", "cmd+t"]` (prefix+c kept for SSH and
    terminals without super key reporting). Consulted GPT-6 Astra and
    DeepSeek: no objections.
  - Verify after reloading Ghostty: Cmd+T opens a herdr tab in the current
    space; File > New Tab still opens a Ghostty tab;
    Cmd+Shift+T is still Ghostty's undo, not a herdr tab.
  - Linux: Ctrl+Shift+T opens a tab and Ctrl+Shift+W closes a pane, the
    keys Ghostty uses there; plain Ctrl+T/W stay shell keys (fzf file
    picker, transpose-chars, backward-kill-word). herdr's kitty keyboard
    flags keep Ctrl+Shift+T apart from Ctrl+T. The dotfiles config is
    shared and herdr has no per-OS keys, so herdr takes Ctrl+Shift+T/W on
    macOS too, and Cmd+T/W on Linux. Ghostty: `ctrl+shift+t/w=unbind`,
    with no replacement keys (the user's choice). Consulted GPT-6
    Astra and DeepSeek, 2026-09-28: no blockers. Costs: Ghostty loses
    Ctrl+Shift+T/W outside herdr (a plain shell may get them as ^T/^W),
    and programs inside herdr never see them.
  - Checked in Ghostty v1.3.1 `src/config/Config.zig` (non-Darwin
    defaults): `ctrl+shift+t=new_tab`; `ctrl+shift+w` is put twice,
    `close_surface` then `close_tab:this`, and the later put wins, so it
    closes the tab.
  - Verify on Linux: plain Ctrl+T/W still reach the shell inside herdr.

- [ ] Force-quitting a quit Ghostty killed ~19 Claude agents in herdr panes,
  and their `?` marks did not come back after `claude --resume` (user,
  2026-10-03, screenshots of job-seeker and email-assistant showing "Resume
  this session with:"). Timeline from the logs (local time):
  - 15:16:27 the user quit Ghostty (the Esc-debugging session told him the
    herdr panes would survive); the herdr client logged its exit, the server
    kept running.
  - 15:17:35 loginwindow opened the Force Quit panel (Cmd+Opt+Esc) and logged
    "Adding Ghostty to apps because it still has background processes".
  - 15:17:39-40 every one of ~19 `claude` processes exited at once, with the
    graceful resume hint (not SIGKILL). Their interactive zsh shells survived
    (zsh ignores SIGTERM), so did the herdr server and ~12 other claude
    processes. 15:18:20 the user wrote "zabiłem ghostty" ("I killed
    Ghostty"). The log does not record the kill itself, so the click on
    Force Quit is inferred.
  - Start times do not split dead from alive: dead agents started from
    2026-09-24 to 2026-10-03 11:47, survivors 2026-10-02 23:50 and
    2026-10-03 12:16-15:03; live handoffs were at 14:07, 14:19 and 14:45.
    Which processes macOS counts as Ghostty's "background processes"
    (responsible pid, coalition, process group) is still unknown.
  - Plan: reproduce with disposable agents (quit Ghostty, force-quit its
    entry, record the signal in a wrapper, `sudo launchctl procinfo` on the
    server, shells and agents before and after). Only then choose a fix:
    disclaim responsibility when spawning the server and each handoff server
    (`responsibility_spawnattrs_setdisclaim`, private API, works only at
    spawn), or run the server as a launchd job. Both can move TCC prompts
    from the terminal to herdr; test with the signed release binary and test
    logout separately.
  - Measured afterwards with `proc_pidinfo(PROC_PIDCOALITIONINFO)` (no root
    needed): the herdr server, all 69 pane shells and every live claude,
    survivors and resumed ones alike, are in resource coalition 2647; the
    new Ghostty and the new herdr client are in 14055. So the whole server
    tree still belongs to the dead Ghostty, and a Force Quit of its entry can
    hit it again. Live handoff does not help: the successor is spawned by
    the old server and inherits its coalition, and so does every pane it
    creates later. Survivors in the same coalition mean the kill set was not
    simply the coalition; still unexplained.
  - Fix, after the second round (sol, MiMo, `20261003-153704-1042`, both
    agree): start the macOS server as a per-user LaunchAgent, which gets its
    own coalition without entitlements. Write the plist on first use,
    `launchctl bootstrap gui/$UID <plist>` when it is not loaded, then
    `launchctl kickstart gui/$UID/<label>` (never `-k`), and wait for the
    socket. `RunAtLoad=false`, `ProcessType=Interactive`, no plain
    `KeepAlive=true` (a broken build would respawn in a loop). Costs:
    - The environment of launchd jobs is minimal: send the client's
      `PATH`, `SSH_AUTH_SOCK` and the like over the socket for each new
      pane instead of freezing them in the plist.
    - TCC: permissions then belong to herdr, not Ghostty, and an ad hoc
      signature changes its cdhash on every build, so grants may prompt
      again after every `herdr_live.sh install`; sign with a stable
      self-signed identity (MiMo).
    - `bootstrap gui/$UID` fails over SSH; keep today's direct spawn as the
      fallback (MiMo).
    - Handoff: a successor spawned by the job's process leaves launchd
      tracking a PID that exits; start the successor through launchd too,
      or exec in place (sol).
    - Panes that exist before the switch stay in the old coalition; they
      move only by resuming the agent in a new pane.
    Rejected: `responsibility_spawnattrs_setdisclaim` and
    `posix_spawnattr_setcoalition_np` as the fix (they change attribution,
    not coalition, or need private entitlements); `launchctl submit`
    (legacy); MiMo's `waitid` on the agent to log its signal (the agent is
    the shell's child, not herdr's); auto-resume without the user's click.
  - Recovery, decided by the user on 2026-10-06 (overrides the earlier
    "ask first" plan): resume a Claude agent killed by a signal
    automatically, in the same pane, without asking, and show a short
    notice "resumed N agents" with the `?` restored. `claude --resume` only
    loads the conversation and runs nothing, and herdr already resumes
    agents on its own after a restart and a logout, so asking only here
    would be inconsistent. Guards: never when the same session already
    runs in another pane; at most once per session, then a notice instead
    (a `claude` that dies on start must not loop); never after `/exit`
    (the `SessionEnd` hook has forgotten the session by then).
    Implemented 2026-10-06 (uncommitted while the user tries the build):
    the Claude `SessionEnd` hook calls the new `pane.report_agent_stopped`
    on reason `other`; the terminal joins that report with the exit of the
    same run (report `seq` above the run's last report), the server types
    Ctrl-U plus `claude --resume <id>` into the idle shell, spaced by
    `startup_per_agent_delay_ms`, and toasts "Resumed N agents stopped by
    the system". Refused with a toast: the session runs in another pane, it
    already auto-resumed in this server run (an accepted forget lifts
    that), the shell is busy. Off with `resume_agents_on_restore = false`.
    The `?` is stashed at the exit and put back when the same session runs
    in the pane again, also after a manual `claude --resume`. Rounds:
    design `20261006-185542-a111`, implementation review
    `20261006-191356-247c` (sol, MiMo). Rejected there: skipping the hook
    and resuming every exited session (races `/exit`), resuming only on a
    mass stop (against the user's decision), MiMo's "the TUI loop never
    drains the queue" (only the headless server runs `App`). Gaps: Windows
    (the PowerShell hook does not report the stop), SIGKILL (no hook runs;
    a restart still resumes it), the limit and the `?` are not saved across
    a server restart, and a live check with a real SIGTERM is still to do.
  - Not done: reproduce the kill with disposable agents (plan above), and
    explain why ~12 agents in the same coalition survived. The LaunchAgent
    fix below waits for that reproduction.
  - Until then, the user's side: do not Force Quit a "Ghostty" entry that
    shows up after Ghostty has quit; Cmd+Q is enough.
  - Status 2026-10-06 (after the reboot of 10-05): the server, all 74 zsh
    and all 48 claude are now in coalition 1206, the coalition of the live
    Ghostty, so the coupling is unchanged; it will become a "dead Ghostty"
    group again the next time Ghostty quits. The "mass exit a minute after a
    client detach, cause unknown" noted by the signal-exit work (`0ddaa676`)
    is this same 10-03 incident, not a second trigger. Third round (sol,
    MiMo, `20261006-182514-d451`): do the auto-resume first and keep the
    LaunchAgent deferred until the reproduction (sol); MiMo's "the live
    coalition contradicts the premise" and "resume runs on by itself" are
    wrong. Risks for the auto-resume, accepted from both: SessionEnd
    `reason: "other"` also covers a deliberate external `kill`, which
    auto-resume would undo; `claude --resume` runs SessionStart hooks; cap
    how many agents resume at once.
  - The `?` mark: today it survives a live handoff but not the agent's exit
    and resume. Restore the normal `?` on resume: the question is still the
    last message of the resumed conversation and still unanswered, which is
    exactly what `?` means (revised after the user asked "why not?"; the
    first plan, a distinct stale mark, distinguished nothing the user
    needs). Persist the report keyed by the agent session id, put it back
    only when that same session id resumes with no user prompt after the
    question (check the transcript, so an answer sent from another resume
    or from the phone clears it), and clear it as today on the next prompt.
    Separately, consider a "the agent exited, resume?" hint on panes whose
    agent died.
  Consulted sol and MiMo (round `20261003-153021-f65d`). Both: the mechanism
  is plausible but unproven, disclaiming fixes nothing if macOS selects by
  coalition or process group. Rejected: both models' "a restored `?` must be
  a distinct mark" (see above); MiMo's "survivors are those spawned after the handoffs"
  (it mixed UTC and local time; the start times contradict it) and its
  reading of `?` as "agent mid-turn".

- [ ] No view of how much memory and CPU spaces, tabs and jobs use (user,
  2026-10-03). Consulted sol and MiMo (round `20261003-163010-4ae0`); both
  keep a server-owned sampler and "CLI first, modal later". Plan:
  - Sampler in the server, running only while someone subscribed (a CLI
    `--watch` or an open view), pushing `resources.sampled` events; no
    always-on cost, no client timer requests on the command lane. One worker,
    no overlapping scans, cached snapshots with timestamp, interval, metric
    kind and partial/error status; measure its own cost.
  - One process enumeration per tick for the whole machine (macOS
    `proc_listallpids` + `proc_pidinfo`, Linux `/proc`), one parent graph,
    each `(pid, start time)` assigned once; never one walk per pane (sol).
  - Attribution: the pane's PTY child tree, plus processes still holding the
    pane's controlling tty (MiMo); process groups and sessions are no use
    (`setsid` resets both). Daemons that escaped (cargo build server,
    rust-analyzer, docker, a detached qemu) go into a `shared / unattributed`
    row, not onto a pane, or the totals lie. The `HERDR_PANE_ID` env marker
    (already set in `src/pane.rs`) needs `KERN_PROCARGS2` per pid on macOS:
    later, benchmark first, never show env contents. Linux cgroups per pane:
    later. herdr's own server and clients get their own row.
  - CPU: per-process deltas of native counters (macOS task info ns, Linux
    utime+stime) before summing, never a difference of changing tree totals;
    label "% of one core" (sums above 100% are normal). Not `ps cputime` on
    Linux (whole seconds; macOS `ps` has centiseconds).
  - Memory: macOS `phys_footprint` labelled "footprint"; Linux RSS labelled
    "RSS" by default (MiMo: `smaps_rollup` is costly on large processes),
    PSS only on an explicit refresh; never mix metrics in one total, and
    never call a sum "memory freed by closing this space".
  - Jobs are tabs flagged as jobs, not a separate bucket.
  - First slice: API method `resources.snapshot` + subscription, and
    `herdr top` (space > tab > pane totals, process count, CPU, memory, sample
    age; `--sort cpu|mem`, `--json`, `--watch`). A cheaper prototype (sol):
    one `ps` per tick in the server plus the same graph aggregation, RSS
    labelled as an estimate. Later: a Resources modal (sortable tree), top
    processes per pane, tab tooltips (they force sampling on hover).
  - Tests: aggregation over a synthetic process graph (reparenting, pid
    reuse, a shared daemon, tty holders), no sampling without a subscriber,
    the sampler stops after the last subscriber disconnects.

- [ ] Orchestration direction (user, 2026-10-06: "analyse how to do this
  orchestration best ... is there a point in using the Claude SDK etc., how
  does T3 Code do it?"). Read T3 Code `4df84a7d`; consulted sol and MiMo
  (round `20261006-033911-ff97`). Decision pending with the user.
  - T3 Code has no PTY for agents: Claude through `@anthropic-ai/claude-agent-sdk`
    (spawns the user's `claude`, uses the subscription login, `canUseTool`
    for approvals and questions, `rate_limit_event` for a Limited state
    with auto-resume), `codex app-server`, `pi --mode rpc`, ACP. One local
    HTTP MCP server with per-session tokens (`delegate_task` async|wait,
    `task_status`, `task_cancel` depth-first); a finished child wakes the
    parent with an injected message; children get a brief, not history; no
    agent concurrency cap. Its gap: a child stuck on a permission request
    looks idle.
  - Terms: Anthropic's Agent SDK docs forbid unapproved third-party products
    from offering claude.ai login; on 2026-06-15 Anthropic paused moving SDK
    and `claude -p` use to separate credits, so both still draw on the
    subscription. Wrapping `claude -p` instead of the SDK is no loophole
    (sol). An interactive `claude` in a PTY is plain terminal use.
  - Both models: stay PTY-first; no SDK in herdr. Headless only for bounded
    child tasks nobody watches (`claude -p --output-format stream-json`,
    `codex exec --json`, `pi --mode rpc`), one adapter proven before the
    next, and one state record fed by both screen detection and stream or
    hook events (MiMo), so hybrid does not double the state machine.
  - Order: (1) a truthful task state: idle is not done; awaiting permission,
    awaiting answer, limited, failed, with question text and reset time from
    hooks where available (user, 2026-10-06: "looks ok"); (2) clean
    validation, not worktrees per child (see below); (3) a child-task primitive: parent link, brief,
    completion that wakes the parent at a safe input boundary (never typed
    into a permission dialog), subtree cancel, recursion bounds; (4)
    handoff; (5) MCP only as a thin facade over the API.
  - Worktrees for children? (user, 2026-10-06: "what do we need them for;
    if they slow things down, is manual handoff not better?"; round
    `20261006-035159-d484`, sol and MiMo agree): no, the shared master stays.
    Measured: one swept-hunk incident in 276 commits over 8 days, but every
    `just check` and install builds whatever another session left half done
    in `src/` (it was the case while asking). Worktree checks: median 3.0
    min against 1.9 (7 runs, 7.8 cold). So:
    - Build and test from a clean tree: a reusable detached worktree at the
      `master` SHA plus only this session's own patch (the fix is not
      committed before the user tries it), sharing `CARGO_TARGET_DIR` with
      the main checkout under one lock; install from there. Measure two
      builds sharing the target first (cargo rebuilds local crates per
      source path).
    - Handoff has nothing to do with worktrees (same task, one after the
      other): automate only the pointer (session id, transcript, task,
      SHA); ownership moves once the first agent stops writing.
    - Children: read-only ones (review, research) in the shared checkout,
      reviews of a committed snapshot; writing ones sequential on master
      with path claims; a worktree only when two writers really run at
      once or the work is long or exploratory, as AGENTS.md already says.
    - A pre-commit hook that refuses a commit without paths (MiMo); note
      `git commit -- <path>` also takes others' unstaged edits in that file.
  - Order and praise (user, 2026-10-06: "would these changes make people
    praise roherdr like T3 Code?"; round `20261006-035823-bbe8`, sol and
    MiMo): the praise is less supervision, not looks: "who needs me now",
    ranked by how long they have waited, one click to the question. So:
    - [x] First the clean build tree, time-boxed to an evening: one persistent
      check worktree (not a fresh one per run: cold Rust builds) reset to
      the `master` SHA, plus a patch of explicitly named paths of this
      session (`git diff` of the shared checkout carries other sessions'
      edits, so it cannot be the input), then the existing checks there.
      Done 2026-10-06: `herdr-job clean-tree [PATHS] -- CMD` (any repository;
      first as `scripts/clean_tree.py`, generalized the same day at the
      user's request, with a rule in the global Claude and pi instructions:
      use it when `git status` shows changes that are not yours),
      `just clean-check <paths>`, `just clean-release <paths>`, AGENTS.md
      install flow. Own `target/`;
      a shared one is not measured (cargo keys local and vendored path
      crates by source path, so they may rebuild on every switch): cold `just clean-check` 6.0 min and 3.6 GB, warm 1.8 min.
    - Then the task state as an attention inbox: waiting agents ranked by
      when they started waiting, the question text inline, jump to it;
      mark hook-confirmed states apart from screen-inferred ones; never
      call silence "done".
    - Limited: only an agent stopped by a limit gets the state (not an
      account that is nearly used up); say which limit (rate, credits,
      context full: different remedies); `limited · resets 14:32` as the
      second line and in the header counts next to `?N`; no implied
      auto-resume. Herdr already reads the reset times (`src/usage/`).
    - Beyond features (user decides): a demo with six agents, two needing
      the user and one limited, solved without hunting; README positioning
      "run your real agent CLIs, find every agent waiting on you", one
      install path; both models call the name "roherdr" hard to say and
      search; MiMo: signed releases, since a one-person fork that replaces
      its server binary live reads as a supply-chain risk.
    - Child tasks, MCP: deferred until supervision is trustworthy.
  - OptMem and OptChat (user, 2026-10-06; github.com/VictorTaelin/OptMem, no
    license; rounds `20261006-042739-ce28`, `20261006-044634-0c2b`, sol and
    MiMo): do not adopt it for our agents. One global log mixes projects into
    mushy summaries, "age" counts later notes rather than time, agents
    compress inline (about one compression per note) and every session pays
    about 8k tokens at wake, and `forget` never erases raw notes. Project
    lessons stay in AGENTS.md. John Ash ran the same tree for two years and
    dropped it: errors stack up and temporal reasoning is weak.
    - If herdr ever keeps an event log (handoff, child briefs): provenance
      first (who, when, pane, transcript link), validity times for
      decisions that can be revoked, summaries last, per task and off the
      agent's turn (like activegraph.ai's replay and explain, Apache 2.0,
      as an idea, not a dependency). After the attention inbox.
      CorpusMap (arXiv 2609.37226, preprint) measured it: summary layers
      (LLM wiki, topic trees) often lose to the raw corpus, while entity pages
      that link to untouched documents beat it with 34-57% fewer tokens, for a
      plain find/grep agent.
    - Child context (step 3): a brief by default, not inherited history
      (Taelin's spawn-by-inherit assumes a single writer). A child that
      continues the same work may get a native fork (`claude --resume <id>
      --fork-session`, `codex fork <id>`), always by explicit id and never
      resumed in place or via `--last`. Writing children re-read only the
      files they edit; reviewers get acceptance criteria and the diff but
      not the parent's diagnosis. Completion returns changed paths, tests,
      blockers and what remains; cancel never blindly reverts. Defer a
      read-files ledger with hashes; a short list of relevant files in the
      brief is enough.
  - Not to build: a chat GUI, a universal conversation schema, a scheduler
    or quotas, auto-approval, auto-merge, default auto-resume after a limit.
  - Slots: freeze them (6 min of overlap in 14 days); what contends is the
    shared checkout and the subscription limits, not CPU. Measure harm
    (failed or slowed runs), not overlap.

- [ ] Ideas from pstack-t3 (user, 2026-10-06; https://github.com/creedants/pstack-t3,
  a 3-day-old port of Lauren Tan's pstack to T3 Code's orchestrator; not
  installed: its orchestration only runs inside T3 Code, and 55 skill
  descriptions cost about 3k tokens per session). Consulted sol and MiMo
  (round of 2026-10-06, both: skip the install, borrow these):
  - Not the full landing queue: it needs a worktree per writer, against the
    fork's "work on master in the shared checkout" rule (user, 2026-10-06:
    keep the rule). Take only what fixes the real incident (2026-10-02: a
    bare `git commit` swept another session's staged hunks):
    - A commit lock: `flock` on a file under `.git/`, so one session commits
      at a time, in a script that does what AGENTS.md describes (a patch of
      only its own hunks, a temporary index) and refuses a commit without
      paths.
    - Light path claims: a session announces the files it edits; another
      session gets a warning before editing the same file, not a refusal.
    - Notes from pstack's `land.py`: `flock` is released when the process
      dies, so no stale lock; after a rebase compare `HEAD^{tree}` with the
      reviewed tree (MiMo); claims do not stop an agent that bypasses them
      (sol). The full queue with worktrees stays for long or risky work,
      where AGENTS.md already asks for a worktree.
    - The Fellowship post (see the three gaps near the top) avoids
      concurrent writers by role instead: only the main session writes
      code, the reviewer never edits, qa works in throwaway worktrees. A
      cheaper first step is that rule in `AGENTS.md`, though it lives only
      in prose.
  - [x] Machine-wide slots for builds and tests: extend `herdr-job` (and next to
    `just guard`) with N slots plus an exclusive mode for benchmarks, so
    several sessions do not thrash one `target/` or skew measurements.
    Done 2026-10-06 (round `20261006-030357-bd7d`, sol and MiMo):
    `herdr-job slot [--exclusive] -- CMD`, `run --slot/--exclusive`,
    `herdr-job slots`; the `just` build, test and clippy lines take a slot,
    benchmarks every slot. One slot by default (both: cargo and nextest each
    use every core, so two slots let two full-machine loads run); MiMo: cargo's
    `target/` lock does not cover it, nextest runs tests after releasing it;
    an exclusive request inside a slot fails at once (it would wait for its
    own ancestor); no gate lock (moot with one slot). Not done: re-run
    `just guard` after a long slot wait (sol), and the CPU and output idle
    detector still cannot tell a job blocked on cargo's lock (MiMo).
  - [x] Structured dispositions in `consult`: classify each finding Act on /
    Consider / Noted / Dismissed (pstack's `$interrogate`), with evidence and
    whether it was verified, next to the existing per-call ratings.
    Done 2026-10-06 as counts, not per-finding records (sol and MiMo: records
    keep the same judgment and cost much more bookkeeping):
    `rate --act --consider --noted --dismissed` (all four, adding up to
    `--findings`; a rejected finding is dismissed), `act/call` in
    `stats --all`, and the `consult` skill reports to the user in the four
    buckets. Later, if wanted: link `act` findings to the commits that
    landed them (MiMo).

- [ ] Keep the model context small, second pass (user, 2026-10-06: "plan
  for cleaning unneeded files from the repo, so the model's context doesn't
  swell too much"). Done on 2026-10-06: finished items left `TODO.md`
  (402 KB to about 170 KB), their decisions went to `DECISIONS.md`, Deferred
  to `TODO-deferred.md`; a root `.ignore` hides published doc snapshots and
  the duplicate changelog from ripgrep; Codex's `project_doc_max_bytes` was
  raised so it reads the fork sections of `AGENTS.md`, which stays as
  upstream writes it (user).
  - Left: open items still carry long histories of their finished slices;
    condense each to its open part plus decisions. A lint against `[x]` in
    `TODO.md` was skipped: the maintenance test list is upstream's justfile
    line, a rebase conflict magnet.
