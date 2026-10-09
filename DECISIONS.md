# Decisions

Durable design decisions from finished `TODO.md` items: what was chosen
or rejected and why, and what the user asked for. Read the matching
section before changing a behaviour listed here; add one when an item
from `TODO.md` is finished and its reasons would otherwise be lost.

### Worktree space nested under its creator tab
- Ownership is the server field `creator_tab` ("created here", not "owned now"), set by `worktree.create_from_pane` from the pane id; client never sends a tab id. Unowned worktrees go after the parent's tabs.
- No reassignment (no drag, no adopt) and no detaching when the creator tab starts a new session (user, 2026-10-03/04); `--resume` restores the old one. Rejected: re-attaching by pane cwd.
- Nested worktree folded by default; fold via the worktree's own `▾` (user, 2026-10-04), jobs `▸` stays jobs-only; folded line keeps aggregate status/attention. Connector `└─`, not the long trunk.

### Caller's workspace for tab create
- User, 2026-10-06 (TODO): `herdr tab create` without `--workspace` from inside a pane goes to `$HERDR_WORKSPACE_ID`, defaulted in the CLI (`caller_workspace_id()`, same guards as `caller_pane_id()`: none for remote targets or empty values), so calls from outside Herdr keep the server's active-workspace fallback. Departs from upstream. Other create commands with the same fallback (workspace create, worktree create/open) left as they are.

### Reopen closed tab brings back the agent
- Server-owned in-memory history (last 20), captured in the close handler before teardown; new method `tab.reopen_closed` rebuilds via the restart path with validated session refs; claim entry atomically.
- Rejected: client fetching agent_session before close, keeping processes alive for undo. Method exists but fails: no silent fallback to a fresh shell (only when not advertised or entry gone, then say so).
- Scope (decided): the client reopens only tabs it closed itself; recovers the conversation, not the process.

### Notification dropdown width
- Header lists size to the longest row (min 56, max screen-2 / 140), no preview footer (user, 2026-10-01); box shifts left when the right edge is short.

### Highlighted dropdown row
- Light accent tint plus `▌` bar, not a solid fill, so state icons keep their colour (user asked why the icon went white). Menus without icons keep the solid highlight.

### Envelope counter `✉N` vs history list
- `✉N` counts unread tabs (computable without a fetch); `•` marks only each unread tab's newest row. Row icon is the event kind at the time, not the tab's current state.
- Selection bar `▌` and unread `•` need separate columns (user, 2026-10-02). Rejected: background fetch at attach/on each notification (holds the command lane).

### Consult roster (DeepSeek / MiMo / Space Bunny)
- User, 2026-10-03: keep DeepSeek, drop Space Bunny, run MiMo as a third voice in a second trial (20 shared rounds, pass at >= +0.25 paired unique findings per call, rejected share at most 5 points above DeepSeek).

### consult-stats table alignment
- Column widths come from the widest cell, never truncated (the label is the key); ` via X` dropped for display only when X is the model's author. Rejected: hover bubble, terminal-size autosizing, right-ellipsis.

### Unfolding job squares near the bottom
- A click on the triangle scrolls the least that shows the line, its squares and the empty row after; folding never scrolls.

### Renamed tab in lists
- Custom tab name is shown first, agent task after it, in working/asking/history lists, looked up by tab id.

### Agent todo list visibility
- Structured events only, never screen scraping; no evidence means unknown, not zero. Shown as quiet `done/total` token on the tab line, no full list in the sidebar.

### Herdr toast position
- Set `[ui.toast.herdr] position = "top-right"` in the user's config.toml, not the fork's default (2026-10-01).

### Spaces filter bar
- Client-only, never persisted, no server requests; smart-case subsequence over space name, branch, tab label, agents (not cwd); tree order kept, folded groups open for display only; drag off while filtering. Opens by `prefix+/`, never steals printable keys.

### Job square tooltip dwell
- Same 450 ms dwell as the cut tab label tooltip, not at once (user request, 2026-09-29).

### Fork build disables upstream updates
- `HERDR_FORK_BUILD=1` disables the upstream version check and `herdr update` (points to `scripts/herdr_live.sh`); agent-manifest updates keep running; unit tests behave like upstream.

### Notification history
- Server ring of 100 entries, in memory, listed via advertised `notification.list`, fetched only when the dropdown opens. Never pick another pane that now sits where the target was.
- With `System` delivery and the window focused, herdr's own toast shows instead of the OS toast.

### awaiting-reply reminder
- Re-inject a short reminder every prompt (`UserPromptSubmit`) with an operational definition (the agent needs the user's answer). Rejected: a Stop-hook phrase matcher (misses, and fires on courtesy offers).

### Sidebar default width
- Fixed `sidebar_width` 32 / max 44 / min 18; rejected percentage-of-terminal and content auto-sizing (reflows agent panes). A dragged width stays as is.

### Dismiss `?` without typing
- `pane.clear_awaiting_reply`: global like typing, idempotent, one request per click; a later report shows `?` again. Dismissing does not mark the tab seen.
- Rejected: clicking the `?` icon itself, a space-wide dismiss, per-client acknowledgement.

### Close confirmation
- Ask only when a close would lose something (turn in progress, approval, background tasks, unanswered question, running job, unknown state); an idle or done agent, or a finished job's leftover agent, does not ask. Uncommitted files are no reason.

### Flaky federated client test
- Cause was input dropped per character while the endpoint is not online; the fix is in the test (Ctrl-C before retry). Do not raise timeouts.

### Push status chip and branch menu
- User, 2026-10-03: the space name line carries the push-status chip (`↑3`) instead of a separate branch row; clicking it opens a menu of the other local branches with their push state.
- The checked-out branch is not a menu row (the chip is the header). Branch rows only inform; a click closes the menu (checking out under running agents is a footgun).
- User, 2026-10-03: no morph animation; the menu opens fully in the click's frame. Branch list is fetched once on click (`git.branch_list`), never on events or timers (one request lane per machine).

### Tab context menu "Close jobs:" row
- The `Close jobs:` label is a muted, non-clickable label, never highlighted; chips that do not fit whole are left out, not cut.

### Tab tooltip hides the accent bar
- A tooltip is never clamped left past its target's left bound (tab labels start at `text_x`); the text is cut with `…` instead. Rejected: restoring cells under the box or redrawing the bar in the tooltip renderer.

### Agent-reported task for tab names
- Claude Code never retitles within a session (verified), so the tab name follows an agent-reported task (`pane.report_task`, `herdr agent set-task`) instead; precedence: user tab name, agent task, OSC title, program.
- Rejected: Herdr summarizing prompts with its own model call (cost, consent) and showing the truncated latest prompt (churn, leaks secrets). Reports are an enrichment, not guaranteed; reject stale-session writes.

### History rows layout
- One right-aligned time column, then one icon column, then the text, so words of all rows start in one column (mixed date/time formats kept, not day separators).

### Jump into a collapsed space
- User, 2026-10-01: a jump from a header list opens the collapsed space (and worktree group) persistently, like a manual toggle; a new tab the user creates also expands its space. Tabs created by API or agents never change the collapse.

### Silent job vs stuck job
- Show only honest evidence (elapsed, time since last output, busiest child, CPU delta labelled "CPU"), never a progress or health verdict. After 5 min of no output at under 2% CPU a job is marked idle with a still mauve `z` ("asleep"). It was a grey dotted ring (`◌`) at first, which rendered as a few faint dots; mauve alone did not help, and Sol and MiMo both proposed `z` (2026-10-06).

### Consult stats popup width
- `consult.py stats --width N` splits wide tables into bands repeating the name column; without `--width` output stays byte-identical. Rejected: `less --header`, shorter names, one block per model, hiding old models.

### Auto-unfolded job squares
- Squares unfolded because a job got focus fold again when focus leaves that parent's jobs; squares the user unfolded stay (temporary cue, no timers).

### Close confirmation for idle agent tabs
- User, 2026-10-01: an agent session in a normal tab asks before closing in every state, also idle ("not working" is not "disposable"); only the leftover agent of a finished job tab is exempt.

### Space bookmarks
- User, 2026-10-02: space bookmarks are a jump target only: they do not pin or reorder the space. One `★N` counts both; the dropdown lists Spaces first, then Tabs. API noun is `workspace`, not `space`.
- Space and tab bookmarks are independent; each server keeps its own bookmarks; no custom order, folders, tags or UI state on the wire.

### Close-tab focus and last-tab confirmation
- Closing the active tab focuses the previous sibling at the same nesting level, else the next, else the parent; never a nested job child by flat index. Tab numbers are monotonic and never reused.
- Esc on a tab or pane close confirmation just closes the dialog; only a workspace close returns to navigate mode.

### Native-graphics benchmark on macOS
- Source retention is unsupported outside Linux; the benchmark reports `status=unsupported` explicitly rather than failing or measuring a fallback.

### Fork push
- Plain fast-forward push of the fork's master needs no force; the rebase onto upstream with force-push is a separate step under the standing rule in AGENTS.md.

### Reopen closed tab
- `prefix+u` recreates the tab (same place, name, directory); processes are not restored and plain shells start fresh. Not `ctrl+shift+t` (terminals take it, legacy encoding cannot tell it from `ctrl+t`). v1 is client-local, at most 10, not persisted.

### Awaiting-reply marker
- "Unseen" and "awaiting my reply" are separate facts; a finished agent clears `Done` on view but the `?` flag stays. Rejected: keeping every `Done` until Enter, and trailing-`?` heuristics.
- User, 2026-09-28: agent reports explicitly via `herdr agent awaiting-reply` (allow rule in the Claude integration); the report holds until someone types into the pane. No time windows (user: race-prone). Never for `AskUserQuestion`.

### Right click in header lists
- A right click on a row of any header list opens that tab's menu without `New tab` and keeps the list open under it; a history row whose tab is gone gets no menu.

### Fold quiet agents
- User, 2026-10-06: a one-shot button (`⊟`), not a mode: folds idle agent tabs under an `N idle` line in every space; nothing folds by itself, so rows never move under the mouse. Plain shell tabs stay visible.
- Quiet means `idle` only; working, blocked, awaiting-reply, limited, done-unseen, the focused tab and bookmarked tabs never fold. A folded agent that starts working or asks leaves the fold at once.
- Folded tabs keep their `Alt-1..9` positions; the space chevron stays authoritative (folding never opens a collapsed space). Client-local state.

### Finished turn is quiet
- User, 2026-10-01: a finished turn for a tab you are not looking at gives no sound, toast or system notification, only a list row and an unread dot; `ui.toast.alert_on_finished` (default false) restores it. Questions and approvals are unaffected.

### Launch-agent button
- User, 2026-10-03: button is a bold `A` in the agent's colour (not a badge); picker lists the other agents plus the current one first with a trailing `✓`; each row starts with a coloured `A` ("teach people the colours").
- User, 2026-10-06: name line ends `[git chip]  +  A `; with vertical tabs there is no drag grip. Remembered agent is client memory only; the picker lists agents on the server's PATH.
- Rejected: distinct letters per agent, logo in the state column, a non-inverting highlight for every menu. Open: codex/copilot share green, gemini/agy blue.

### Status legend
- Legend is built from the same functions the sidebar draws with (no drift), reached via a global-menu item; no sidebar header button because `?` already means "awaits reply". One cell holds one glyph (no composites).
- A legend documents the shared agent-state/attention/job vocabulary; it does not fix it (splitting the axes is still open).

### Reopen closed tab with jobs
- Closing a tab with jobs records the parent; reopen gives a fresh shell and says jobs were not restored. Never silently fall through to an older tab; rerunning jobs belongs to the job tool. Every press answers. Bound to `cmd+shift+t`.

### Header sort button and indicators
- One sort button showing the current key (`⇅ name ↑`) opening a menu; never hide a non-zero indicator; keep `◐` and `?` adjacent (user: sessions that ask belong next to working ones).
- An open list's header button is a light accent tint with its own glyph colour kept, not a solid pill.

### Spaces filter
- User, 2026-10-01: filter is a `⌕` button in the header (no slash, it must not promise a key); the open filter bar takes the header row itself; arrow selection is an accent bar `▍`, not a fill. Bare `/` goes to the agent by design (`prefix+/` opens it).

### Claude background tasks and working state
- User, 2026-10-03: a monitor in Claude's footer means working; a shell alone stays idle, no badge. Read the persistent footer below the prompt box, never the draft or the history line.
- Rejected: decay to idle after 10 minutes; a hook writing the state into the title; a new `AgentStatus` variant (enum is append-closed).

### New space placement
- User, 2026-10-03: a UI-created space goes right after the active space's whole worktree family, via advertised `workspace.create_after`; `workspace.create` from CLI/API keeps appending (scripts rely on the end).

### Failed jobs after a cold restore
- Cold restore leaves out finished (succeeded or failed) herdr-job tabs; the outcome stays in `herdr-job list`. Interrupted jobs come back without status and the wrapper marks them failed.
- Rejected: a new `TabStatus` "Interrupted" (frozen codec), expiring by age, replaying the log into a shell. The server decides survival; the wrapper owns logs and retention.

### Relaunch programs after reboot
- User, 2026-10-02: stay with the fixed `plugins/relaunch` plugin; core support not started. Core design if reopened: allowlist `[session] relaunch_programs` (default empty), never "everything but agents and shells".
- Only zsh hooks; add bash or fish only on request (bash hooks cannot get the command text reliably).

### Working-agents list
- The `◐` count and list use the same predicate the sidebar icon draws with (`agent_mark`): working or waits-on-job, not asking; so they cannot drift.

### Space boundaries and focused tab
- Name row of a top-level space gets a `▍` bar (accent when focused), no grey band or fill; nested worktree spaces get no bar. Focused active tab: stronger accent fill plus `▌` bar, not solid (job counts must stay readable).

### Toast position
- User, 2026-10-03: toasts bottom-right but above the agent's input box: static `[ui.toast.herdr] bottom_margin` (default 0). Rejected: following the cursor, anchoring to the focused pane, placing over unfocused panes.

### Nested worktree tab rows
- Tab rows under a child worktree space shift 5 columns right (fills, hits, tooltips, squares together); a gutter click still selects the tab.

### Pi title
- Pi names its session only via `/name` or an extension; `plugins/pi-title` names it once from the first prompt, deterministically, no LLM call; keeps user names.

### Dragged space background
- A grabbed space gets a configurable `drag_bg` from the press on, on any space incl. the active one; none in 16-colour themes and with NO_COLOR.

### Focus after closing a tab
- Closing a child tab always returns to its parent (all models). `ui.focus_after_tab_close = "next"|"previous"`, default `next`, decided by me ("rób jak uważasz", user may change); `last_used` waits for tab history.
- Closing an inactive tab or a process-exited/job tab never moves focus unless it was the active one.

### DeepSeek stats alias
- Consult stats log the serving model version (`model_version`) and fingerprint beside the alias; do not backfill old entries by date.

### Cmd+W closes panes
- User, 2026-09-28: Ghostty `cmd+w=unbind`, herdr `close_pane = "cmd+w"`; no Ghostty split-close fallback key. Never use `cmd+w=csi:...`. `confirm_close_running` matters more than undo.

### Awaiting-reply across handoff
- The live handoff carries `awaiting_reply_reported`; a full restart from session file must not restore it (the agent is relaunched). The report clears only on typed input, agent exit, session change or Blocked.

### Job tab nesting
- A job tab is created already nested through a new method `tab.create_child` (shape of `tab.create` is frozen; old servers reject, herdr-job falls back). Click targets bind to the row id captured on pointer-down.

### Back/forward navigation
- User, 2026-10-06: client-local pane-id history (never server-side), ~50 entries, not persisted; `Alt-1..9` presses count as navigation. Server's `last_pane` toggle stays unchanged.

### Disk space of target/
- `sweep`/`guard` use cargo's own lock; never delete `target/` by hand; dev profile is `debug = "line-tables-only"`. Left out `incremental = false` and cargo-sweep.

### Flaky tests
- No nextest retries and no timeout inflation (the user disliked it): reproduce with a stress loop, resend dropped clicks until the effect shows, publish files via `.tmp` + `mv`.

### Rebase onto upstream
- Rebase, never merge; release tag `roherdr-v0.9.3.1` stays on the pre-rebase commit (tags never move); backup tag `backup/pre-rebase-20261002`.

### Fold state of job squares across client restart
- Per client (not shared server state; two clients would fight), saved in client preferences for the local server only; SSH servers not saved (user, 2026-10-03).
- Keyed by public tab id, NOT by server `boot_id`: tab ids survive handoff/restore and are never reused; `boot_id` changes on every install and would forget everything.

### Attach image from screenshot directory
- Picker first (`Attach image…`), never a blind "attach newest" (could send a private shot to a cloud model). Multi-select is core (user, 2026-10-03: "I sometimes take a few screenshots for one task").
- Delivery reuses the clipboard-image path (works for remote servers); magic bytes checked, never truncate. The destination pane is fixed when the picker opens. No CLI in v1.
- Rejected: clipboard route (shots go to disk, clipboard holds one image); a "skip files younger than 200 ms" wait (skip dotfiles instead; macOS renames a hidden file).
- Preview (user, 2026-10-06): Space opens Quick Look (`qlmanage -p`) as in Finder, marking moved to `x`; the panel takes the keyboard focus, so it is one owned child killed when the list closes, not a follow-the-highlight panel. Beside the list, the highlighted PNG is a kitty image sent by path (`t=f`), so Ghostty decodes and scales it; only for a local Ghostty client (not over SSH). One preview panel, not per-row thumbnails (consult Sol + MiMo: unreadable at 1-2 cells, N transfers). Rejected: `p` for preview keeping Space for marking (the user asked for Space).
- Other directories (user, 2026-10-06: "a simple file manager, go up a directory, show the last chosen one and a list of directories recently attached from"): `..`, shortcuts (up to 5 recent attach directories plus the screenshot one), images newest first, then subdirectories (below the images so a busy Desktop does not push the shots off screen). Opens in the last attach directory; up to 8 kept in the client preferences. Marks clear on a directory change (Sol: no unseen file attached). Consult preferred a `ui.image_dirs` config list switched with arrows, no browsing; the user chose browsing. Rejected: merging several directories into one list (a busy Downloads starves the 200 cap), the pane's working directory (it lives on the server).

### Purple vs orange status spinner speed
- Speed differs by design (job glyph 320 ms vs working 160 ms); consult said equalize tempo and distinguish by color/shape, since slow reads as lag.

### Folded job squares and focus
- Decided (user, 2026-10-03): a folded parent with a focused hidden child shows one extra row for that job carrying the selection bar; the parent line is dimmed, not blue. Squares stay folded.
- Auto-unfold on focus is removed (it prevented clicking between job and parent); only the arrow and counts unfold. Snapshot reconciliation/reattach must never unfold. Rejected: force-unfold while a child is focused.

### Header lists follow the tab look
- Rows use the tab's state icon and colours as in the sidebar; open-list button is a filled accent pill, one at a time; `★` is neutral because mauve means waiting on a job.
- Do not mix working and job-waiting colours in one count.

### Awaiting-reply report not made by small models
- Never execute text scraped from a message (injection, masks the bug). Stop hook only blocks once and nudges: "call the Bash tool with the command, never write it in your reply".

### `?` marks across restart
- The awaiting-reply flag is carried through the live handoff in its own field; a cold server restart still loses it (accepted, not done).

### Bubble busy spaces to top
- One-shot action (stable partition sent as one `workspace.move_block`), never a sort mode; busy = same set as `prio` plus a running job; worktree families move whole. Offer Undo right after.
- Placement (user, 2026-10-06): `⤒`/`↶` per space, on every top-level space in manual order, between triangle and name, shown on hover with the two columns always reserved. The header button was rejected ("works badly").

### Consult stats `--vs`
- Prints numbers only: no rule verdict and no cost column (no price table; never guess a price).

### Notification list times
- Decided (user, 2026-10-03): muted day separator rows (`Today`, `Oct 2`) with `HH:MM` on every row; separators take no row slot or hit target. No relative ages. Rejected: dated stamp on every row.

### Close dialog wording
- Title `Close pane?` / `Close tab?` (never "with running work", false for idle agents); name the tab by its sidebar label, never a number; `stops:` does not repeat it.

### Bookmark rows and menus
- Bookmark row label is exactly the sidebar tab label; the space name is appended only when it differs. Right-click menu of a row has only "Remove from bookmarks". Context menu items have one column of padding each side.

### Tab bookmarks
- Flag lives on the tab in the server (persisted, optional in snapshot, shared by all clients); the list is computed live from the tree in space order, nothing stored. Mutation is idempotent `tab.bookmark`.

### Wheel scrolling
- Spaces list and agents panel scroll 1 row per event (user, 2026-10-01); pane scrollback keeps `ui.mouse_scroll_lines = 3`.
- Direction is the OS's: herdr must never invert wheel direction. No adaptive rhythm step (the event carries no magnitude or device). `WHEEL_STEP_EVENTS = 3` is only the `agent read` harvester, not the user's wheel.

### Dragging tabs in the spaces list
- Drag unit is the whole block (line, squares, child tabs); within its own space only (cross-space is a separate feature); outside the space shows `release cancels · Esc`, never clamps.
- Drag starts after one row of vertical movement; sideways alone never starts it (also for spaces). Drop is measured against rows frozen at drag start; live splice preview with header hint, markers on a line were misleading and removed.

### Do all tests need to exist
- Optimise for confidence and upkeep, not count; prune in small batches. Do not rewrite or merge upstream's tests/code in the fork (makes rebases conflict); suggest it upstream. Platform tests stay in platform files.

### New tab position
- `ui.new_tab_position = "after_current" | "end_of_space"`, default `after_current` (user, 2026-10-02); "after" means after the whole group (never between a parent and its child). Current tab is the requesting client's, sent with the request.
- Not following it: `tab.create` from API, job child tabs, tab reopen, restore/handoff.

### Claude sessions lost at reboot
- Claude only: on process exit the session id moves to a save-only `exited_agent_session` instead of being dropped; the `SessionEnd` hook (`prompt_input_exit`) forgets it, SIGTERM (`other`) keeps it.
- Forget is guarded by the same per-source sequence as session reports (a separate guard was tried and reverted); it must not rely on the session id because `--resume` reuses it.

### Compact fork history before rebase
- Rebase fork as a few dependency-ordered commits, not 315 one by one; keep chronological blocks, extract a feature for upstream only when it is sent; fix existing features with `--fixup` + autosquash before each sync.

### `claude` consult skill
- Uses `claude -p --model` (subscription login, not API billing); Sonnet default, Opus explicit; never silently substitute a model when a limit rejects it.

### New tab button
- Draws `❏`, the mark of a tab without an agent, not `+`, which the user mixed up with the agent launch chip (user, 2026-10-06).
- Colour, not shape, tells it from a tab line: the accent, never the tab lines' dim grey nor an agent's badge colour (user, 2026-10-06: "maybe just a colour other than grey"). Fallback if that is not enough: `+❏`.

### Space `T` button and tab roles
- User, 2026-10-07: a `T` left of a space's `A` runs `ui.sidebar.spaces.todo_command` (the user's `~/scripts/todo-worker {space}`) on this machine, in the background, with a toast of its first output line; a failure stays until clicked. Unset means no button: nothing ships a default, so users without the user's TODO convention never get an agent committing through an unknown TODO (consult `20261007-010106-c5ac`). herdr starts no agent itself; the launcher does.
- Tabs carry a server-owned role, `coordinator` or `worker` (`tab.set_role`, `herdr tab role`), set by whoever opens the tab and never derived from the agent's name or title; it survives restarts and ends with the tab. A yes/no worker flag was replaced by the role before it shipped (user). Marks before the state glyph: `♛` coordinator and `⚒` worker, both chosen by the user from mockups (consults `20261007-010734-4c57`, `20261007-011027-5c82`). No reordering, no pin, no progress line (`?` and `↳` already mean other things; TODO counts are unreliable).
- `tab.create`'s shape is frozen, so the role is a separate method rather than a create option.

### Fork name, CI and releases
- Name **roherdr**, the user's own pick (2026-10-02; first `roherd`, models advised against "herd" in the name, so the README says first thing that it is an unofficial fork, not affiliated). The binary, crate, `HERDR_*`, config paths, sockets, plugin ids and the `herdr` skill keep upstream's names, for compatibility and cheap rebases; only the repo, display branding and release asset names (`roherdr-<os>-<arch>`) change. Keep attribution and Apache-2.0.
- Releases: `.github/workflows/fork-release.yml`, never upstream's `release.yml` (maintainer gating, Homebrew, Nix, website, secrets); upstream's workflows are disabled on the fork with `gh workflow disable`. Tags `roherdr-v<upstream version>.<fork revision>` (`roherdr-v0.9.3.1`; four numbers because `0.9.3-1` is a semver prerelease below `0.9.3`), annotated, never reused; the revision counts published fork releases and restarts at 1 on a new upstream version; `Cargo.toml` keeps upstream's version. `--version` reads `herdr 0.9.3 (roherdr 0.9.3.1, an unofficial fork)`.
- CI on the fork runs only by dispatch (push and PR events do not start it): a throwaway `ci-dispatch-N` branch whose `ci.yml` adds `workflow_dispatch:`, then `gh workflow run ci.yml --ref ci-dispatch-N`. Green on Ubuntu, macOS and Windows since run 36957931855.
- Verified 2026-10-07: the `roherdr-v0.9.3.1` macOS arm64 asset downloads with `gh`, matches `SHA256SUMS` and runs (`--version`).

### Model context size
- User, 2026-10-06: keep the model context small. Finished items leave `TODO.md` (decisions to `DECISIONS.md`, parked ideas to `TODO-deferred.md`); open items keep only their open part and the decisions that constrain it.
- A root `.ignore` hides the published doc snapshots and the duplicate changelog from ripgrep. Codex's `project_doc_max_bytes` is raised so it reads the fork sections of `AGENTS.md`, which stays as upstream writes it (user).
- No lint against `[x]` in `TODO.md`: the maintenance test list is upstream's justfile line, a rebase conflict magnet.

### Agent's question in the `?` list
- `herdr agent awaiting-reply` takes an optional short question (`pane.report_awaiting_reply` `question`); the server caps it at ingest (about 40 characters on a grapheme boundary, control and ANSI sequences stripped), never per frame; it lives and clears with the `awaiting_reply` flag, so no stale questions. The `?` list draws it as a dim `↳` line (blocked: the hook message or `approval`).
- The task title stays the row's identity, the ask goes in a second line (sol; rejected MiMo's ask replacing the title: three OAuth tabs become indistinguishable). Explicit reporting, never a heuristic over `last_assistant_message` (round `20261006-021127-0eb7`).
- Agents report `waiting_since_ms` (blocked, asked, limited); the list ranks longest wait first with the wait in the time column.
- `Limited` (taken from T3 Code): `pane.report_limit {kind: usage|credits, message}` (`herdr agent limited`) from Claude's `StopFailure` hook (`rate_limit`, `billing_error`), shown like an awaiting-reply report and counted in `?N`; `resets_at` is the latest reset of the provider's full windows in the usage report.
- 2026-10-07: with vertical tabs, an asking, blocked or limited tab line gets a dim `↳` row under it with the same text as the `?` list (`asking_detail`, the longest-waiting agent of the tab); the row is part of the tab's click target; working and idle rows stay one line.

### Closing a parent's last pane
- An explicit close of a parent tab's last pane (cmd+w) is a close of the tab: it asks and closes its job tabs too. A parent whose shell exits or crashes keeps its jobs (an agent may exit after starting a long build on purpose). Consult sol, DeepSeek, MiMo, unanimous (2026-10-03).

### Waits go through herdr-job
- User, 2026-10-06 ("I want visibility"): an agent waited for a process it did not start with a native background shell, read as outside "work over a minute". The rule is by intent: use herdr-job for background work or waits whose end gates the next step, including processes you did not start (`herdr-job watch --pid`). Its success means "the process disappeared", not "it succeeded" (no exit status of a foreign process). Rejected: a PreToolUse deny of `run_in_background` (trains workarounds), a PostToolUse registry of native shells (ghost jobs without an exit hook).

### Consult roster outcome
- `astra -r` is not better than plain astra (2026-10-03, verified in the log): 11 rated calls, mostly code reviews beside weaker companions; only paired rounds can show a repo-mode gain.
- MiMo's second trial passed (+0.45, CI +0.00..+0.85; rejected +3.1 points). The user replaced DeepSeek with MiMo (2026-10-03): default set sol + MiMo, for quality, not cost (DeepSeek cost about a cent a call); DeepSeek on request.

### Stop-hook check for unreported questions
- Order chosen 2026-10-01 (user: "choose yourself"): V1 shadow log, V5 a stronger instruction, V2 a blocking Stop-hook reminder, V3 inference only if V2 is not enough. The offline audit (`scripts/awaiting_reply_audit.py`) replaced V1: it measures misses from existing transcripts (a screen to review, not ground truth).
- V2 for Claude Code: `herdr-agent-state.sh stop-check` blocks the stop once (`stop_hook_active`) when the final paragraph looks like a question (the audit's bilingual heuristic, kept equal by a parity test) and the turn ran no `herdr agent awaiting-reply`. Decisions log to `~/.local/state/herdr/awaiting-reply-stop.jsonl`; `HERDR_AWAITING_REPLY_STOP=0` turns it off, `=shadow` only logs.

### Vertical tabs and job squares
- 2026-09-28: `ui.sidebar.spaces.tabs` replaced agents under each space: one line per top-level tab (plain shells too), the tab's state icon and label; both horizontal tab rows are hidden. Agentless tabs show `❏`. Disclosure triangles `▼`/`►` (not `▶`, which has an emoji form); a worktree parent's triangle collapses its children too.
- A tab's job tabs are 3-column squares under its line, glyph only (no `Alt` number, the user's choice over both models), in the theme's own status colours (the user rejected darkening them to 3:1), followed by an empty row. Click opens the job, the open square again goes back; middle-click closes, a running job asks first. The line counts `⧖ ! ✓`. A succeeded square closes after 10 s, never while focused or while the pointer is over the sidebar; while it is, closed squares leave blank inert slots so nothing moves under the mouse.
- The job's top line (`←`, name, `--why`, starter, id, `×`) is drawn by herdr-job as a pinned row, so no protocol change (DeepSeek's; Astra wanted a herdr-drawn row and a new codec).
- The spaces list scrolls by rows, not whole spaces, and keeps its top row anchored when rows above change. Space sort buttons `manual name ↑ prio ↓`; only `manual` drags; a sorted list holds its drawn order while the pointer is over it.
- Agent state shapes (style `shapes`, the fork's default): `◐` working, `◉` blocked, `●` done and unseen, `○` idle; colours stay. Rejected `◷` for waiting on a job (reads as a moon next to `◐`).

### Dragging spaces
- Live reorder while dragging (prototype variant C, decided 2026-09-26): the dragged block gets an accent bar instead of grey, the list shows where it lands; target is the slot nearest the block's top in the list without it, so it does not flicker. Never collapse spaces while dragging; worktree families move whole.
- The header keeps its sort buttons while dragging; only `release cancels · Esc` and a refusal's reason are shown (user, 2026-09-29). Dragging onto the list's edge auto-scrolls a row every 60 ms.
- Keyboard reorder: `keys.move_space_previous` / `move_space_next`, unset by default, manual sort only, whole family, no wrap.

### Restart agents
- Called "Restart agents…", not "reload" (reads as a config reload). Herdr restarts the agent; never ask the agent (it costs context and cannot replace its own process).
- Stage 1 is the `plugins/restart` plugin, not core: focused pane or the workspace's idle agents; launch flags come from the agent process's own argv (old resume arguments and prompts dropped), so nothing is recorded; Claude with a draft is skipped; SIGTERM, wait for the shell, then `claude --resume <id>` / `pi --session <path>`.

### Awaiting-reply for pi
- `pi-awaiting-reply.ts` (linked by `plugins/pi-title/install`) adds the instruction as a named system-prompt section in Herdr's TUI mode: pi has no command allowlist, and the managed `herdr-agent-state.ts` is overwritten on reinstall (a version bump would drift from upstream's numbering).

### Auto-resume of agents stopped by a signal
- User, 2026-10-06 (overrides "ask first"): a Claude agent killed by a signal resumes automatically in the same pane with a notice "Resumed N agents stopped by the system" and its `?` restored; `claude --resume` only loads the conversation, and herdr already resumes after a restart.
- The `SessionEnd` hook reports `pane.report_agent_stopped` on reason `other`, joined with the exit of the same run by report `seq`; the server types Ctrl-U plus `claude --resume <id>` into the idle shell (a busy shell waits in the queue, up to 10 s). Refused with a toast: the session runs in another pane, it already auto-resumed in this server run, the shell stays busy. Never after `/exit`. Notices follow `ui.toast.delivery`. Off with `resume_agents_on_restore = false`.
- Rounds `20261006-185542-a111`, `20261006-191356-247c`. Rejected: resuming every exited session without the hook (races `/exit`), resuming only on a mass stop. Accepted risk: `reason: "other"` also covers a deliberate external `kill`.
- A restored `?` is the normal mark, not a distinct one: the question is still the last unanswered message (user asked "why not?").

### A turn ending in blocked tool calls
- User, 2026-10-06: when a turn's last batch of tool calls all failed or were denied (a permission prompt, the auto-mode classifier), the Stop hook marks the pane awaiting a reply itself (question `blocked tool calls`) and lets the turn end; the agent may have been unable to run `herdr agent awaiting-reply`. No separate error state. It never asks the agent to run a command its tools may deny again, and it may mark the second stop of a turn it already blocked.

### Decided questions, 2026-10-07
- No `--workspace` hint for agents: `herdr tab create` defaults to the caller's workspace in the fork (`39be3b9b`), and the hint would cost tokens on every turn or wait for a stable release of the herdr skill (user).
- Fork builds keep the self-updater off; the user installs from `master` with `scripts/herdr_live.sh` (user).
- Follow-ups of the ask line (a "needs me" filter, a limited-agents count, outcomes) are parked in `TODO-deferred.md` (user: "none for now").
- Windows checks stay in the fork's CI, run by dispatch on demand; no local Windows SDK (xwin) on the Mac (user, 2026-10-07).
- Cmd+T opens a herdr tab on macOS and File > New Tab still opens a Ghostty tab (checked by the user); the `env` tab title no longer shows (user).
- No looping animations in the README's "Fork changes": the text and the long MP4 walkthrough stay (user).
- Awaiting-reply instructions only for the agents the user runs (Claude, pi); the other 16 integrations are parked (user).
- Closed as no longer seen or not bothering the user: the 2026-09-29 tab-close regression (asking to close the space, odd highlight on cancel) and pi's multiline copy inserting newlines (user).
- Ghostty's Force Quit killing agents: reproduce with disposable agents before moving the server to a launchd LaunchAgent (user).

### Handing a session over
- 2026-10-07 (`07b6b605`): `agent.handoff` (`herdr agent handoff <pane> --to claude|pi|codex`, and "Hand over to…" in a tab's menu) opens a tab after the source tab, in its cwd, and starts the agent with a first prompt that points at the source transcript, lists what to carry over and records provenance. herdr never parses the transcript, so it works after the source hit its limit; an unknown session or a missing transcript is an error and no tab opens; the source tab stays. A new method, not a `tab.create` change (frozen shape).

### Prompt turns
- 2026-10-07 (`05af1241`): `agent.prompt` returns a request id followed through `pane.report_turn` from the Claude hooks (UserPromptSubmit started, Stop/StopFailure finished) and the pi extension (v11): accepted, working, finished; agents without turn reports answer `unsupported`, never a guess from the screen. `agent.prompt_turn` types and waits for its own turn; `herdr agent prompt --wait` uses it and falls back to the old state wait for untracked agents or older servers. A prompt is matched to its turn by its text; a prompt typed into a working pi joins the running turn and is not followed.
- Agent names resolve in the caller's workspace first, then globally when the caller's workspace has none; `--global` for every workspace; names are unique per workspace (`879b067e`). The optional `prefer_workspace_id` is safe with older servers: a name unique everywhere resolves the same; a shared one is ambiguous, never another agent.

### Upstream rebase, 2026-10-07
- Rebased 384 fork commits on upstream `a124eed7` (21 upstream commits) by a worker in its own worktree with `git rerere` on; 9 commits conflicted, 16 changed in `git range-diff` (382 identical); upstream's version won three contradictions and the fork's intent was re-applied on top (pane probe shell guard, Windows clippy `--all-targets`, the agent prompt validate/submit split with `prompt_request`). `just check` green (4193 tests); master moved with `--force-with-lease`; the old master is `backup/pre-rebase-20261007`.

### Coordinator process, 2026-10-07
- Open points a worker or agent reports are decided after a consult and written into the TODO item, or moved to "Needs a decision" with `Options:`; never left only in an agent's output (user; global rule and `/todo` skill, dotfiles `c348422`).
- A worker's end is an event: `agent.wait_turn` on the prompt's request (finished, failed, interrupted, exited, unknown_request), then one transcript read for the verdict; no screen polling, deadlines or idle debounces (user: "why any asynchronous workaround?"; `7a82f292`). A first screen-based `wait-agent --worker-line` missed a finished worker for 3.5 h because an unwatched pane's screen was stale.
- Toasts top right again (user; dotfiles `ae7cea1`), stacked under the endpoint notice and banners, at most two cards plus "+N more", 48 columns at most, a sticky error moves to the notification log (`76aa9c54`).

## The header keeps its back/forward arrows (2026-10-07)

The user saw the `‹ ›` arrows disappear on a narrow sidebar. The header now
reserves the sort, filter, back/forward and fold buttons first; counters use
what is left and drop whole buttons in a fixed order (★ bookmarks, ◐
working, ✉ history), never `?`. An arrow with no history is drawn dim, not
hidden (rule from sol, with MiMo's "? never drops"). Done by the first
headless worker run.

## Atomicity review of the coordinator and headless workers (2026-10-07)

The user asked for the coordinator's operations to behave like database
transactions. A worker reviewed `src/workers/`, herdr-job, the `/todo` steps
and the coordinator's wait script with sol and DeepSeek (MiMo timed out):
21 operations, 15 verified findings, report in
`docs/atomicity-review-2026-10-07.md`. Critical: a live handoff (every
install) ends all running headless workers and marks them `lost`. The fixes
are queued as one TODO item plus steps of the reliability plan.

## Headless workers: sandboxed Bash, one persistent folder (2026-10-08)

Headless workers run Bash in Claude Code's sandbox (manual mode, no network,
credential reads denied, `allowUnsandboxedCommands: false`), so herdr allows
their Bash requests itself (Claude Code's compound-command check asked about
harmless `cd ...;` commands seven times on one item); only classifier
escalations and paths outside the roots reach the user. Workers do not work
around the sandbox for tests; the coordinator runs `just check`. They build
in one persistent worktree (`herdr worker start --folder-slot worker`),
decided by the user because Cargo and Zig caches record absolute paths, so a
copied cache is cold: measured 134 s cold, 27 s warm. The vendored
`libsystem_override.sh` gets a tracked patch (0008) so `mktemp` honours
`$TMPDIR` inside the sandbox, rather than widening the sandbox to the system
temp dir. The first item done this way (that patch) asked no questions.

## Reliable coordination of headless workers (2026-10-07..08)

The user asked for coordination that works "like a database transaction, not
hop siup", after worker questions sat unanswered and TODO edits were lost. All
seven steps of the plan are in: Claude Code's Bash sandbox (workers ask almost
never), worker state in SQLite (events with a sequence, projections, command
receipts, an answer outbox), `worker.wait --attention --after <seq>`
(level-triggered, subscribe before check), obligations that keep a
coordinator from stopping while its workers need it, questions quiet until
the coordinator escalates them or an event shows it is gone (the user rejected
a 15-minute lease: events only; a hung coordinator is a visible known
limitation), a verified TODO/DECISIONS editing tool, and fault-injection tests
(`docs/headless-worker-fault-tests.md`). Reviews that shaped it:
`docs/atomicity-review-2026-10-07.md` and the comparison with T3 Code
(adopted its receipts, pending-only answers and catch-up after a cursor; not
its unbounded approvals or cancelling agents at restart).

## Headless workers became the default for coordinators (2026-10-08)

The `todo` skill and the global TODO rule (dotfiles `359a178`) start
headless workers in the persistent `worker` folder, wait with `--attention`,
ack after handling, and stop workers after bringing their commits in. TUI
agents confirm prompt delivery (`agent.prompt_confirmed`, and `agent.prompt
--wait` waits for the same acknowledgement instead of a 5-second timer that
decided the outcome). Twenty headless workers (w1-w20) did the reliability
work on 2026-10-07..08; after the sandbox change, none asked the user.

## Changes to the user's agent configuration (2026-10-08)

The user asked why a worker editing his agent configuration ran without auto
mode ("handling was supposed to be without me"). Round `20261008-132031-ac90` (sol, MiMo,
DeepSeek): `~/.claude` is the agents' own control plane (rules, skills,
hooks that run on every tool call); a coordinator's review is not an
independent check. Decided by the user 2026-10-08: a standing approval for a
narrow class: text of skills and of the global rules that does not broaden
permissions, disable or weaken a safeguard, grant trust, or widen this
approval; a worker may make such edits without asking, the coordinator
reviews the diff and commits it in the dotfiles by path. Everything else
under `~/.claude` (hooks, `settings.json`, permissions, integrations,
safeguards) goes through one approval of the whole patch: a worker prepares
it in a trusted repository (no edits under `~`, so no folder-trust
dialog), the coordinator shows the diff and risks in one menu and applies it
after a yes. Agents never answer trust dialogs or write trust config.

## Worker runs reached through their TODO item (2026-10-08)

The user objected to 18 "exited" worker lines that never went away and asked
to reach a TODO item's worker logs from the item, several runs per item.
Agreed with sol, MiMo and DeepSeek: the sidebar lists only live workers (an
ended one leaves once its owner acked it; unowned ones leave when they end);
the coordinator's line has an "Items" button with a badge, a popup of items
with their runs (worker, times, outcome, turns, commits, questions) and an
"Unassigned" bucket, a run opening its log; runs link to TODO items through
`herdr worker start --item <id>` and stable `[t-xxxxxxxx]` ids that
`scripts/todo_edit.py` mints; the item's title is stored at start because
finished items leave TODO.md; never grouped by worker name.

## herdr decides a worker's verdict (2026-10-08)

The user felt control was inverted: herdr ran the worker process but the
model's own `WORKER-DONE` line decided readiness. sol, MiMo and DeepSeek
agreed; exit codes alone describe the CLI's turn, not the task. Now herdr
decides: `herdr worker verify <id> --base --message --paths [--cmd]
[--generated]` checks one commit descending from the base, the exact
message with no body or trailers, allowed paths, a clean tree, no worker
process left, regenerated files, and runs the command in the caller's
environment; verdict `verified | failed | unavailable` (an infrastructure
failure is `unavailable`, never `verified`), journaled and shown with the
run. The todo skill (dotfiles `f7a238d`) cherry-picks only on `verified`;
a failure starts a new attempt with the evidence. First real use caught a
missing `ZIG` in the server's environment, fixed the same day.

## Headless workers survive installs and server crashes (2026-10-08)

The user asked why an install refused while a headless worker was in a turn
("I thought the work was coordinated on the server"). The store held the
state, but a worker's life was tied to the server's pipes. Built with sol,
MiMo and DeepSeek, in four slices: an install queue that waits for turns by
events; a per-worker broker (`herdr __worker-broker`, its own session) that
owns the worker's pipes and outlives the server; a stdout spool with
sequence numbers, acked after the store commit, replayed on re-attach, a
re-attach without proven continuity marking the worker degraded (a fold
that synthesized Working was removed after review); stdin through the
broker once per id, and a live handoff that keeps brokered workers. Live
test 2026-10-08: a worker reading 19 files survived a server handoff
mid-turn and finished its turn through the new server, not degraded.
Windows keeps direct pipes (a gap); workers started before the broker keep
the install queue.

## Quieter uncommitted-files Stop hook

The (2026-10-09, worker w48, approved by the user):
  `~/.claude/hooks/uncommitted-notes.sh check` stays quiet when the final message
  names every dirty file (`last_assistant_message`, else the transcript), blocks at
  most once per unchanged dirty set (content hashes in `<ledger>.acked`), and its
  reason is one line that keeps "grants no permission to commit, push or continue
  stopped work". Tests: `uncommitted-notes-test.sh` (bash 3.2). Dotfiles d4c8412.

## Worker brokers drop inherited descriptors

A broker started by a test inherited `herdr-job`'s slot flock through `cargo
nextest` and outlived it, so an install waited 30 minutes for a slot nobody
held (2026-10-09). The broker now closes every inherited descriptor without
`FD_CLOEXEC` when it starts (`platform::close_inherited_descriptors`; it opens
its own pipes afterwards), and broker tests end and reap every broker they
start through a `Drop` reaper, also when they fail (499bcb02, run `r-ziisd3ny`).

## A todo run's worker belongs to its coordinator

`herdr todo run` workers once appeared in the `~` space with no owner, so
their questions had no obligations or escalation (user's screenshot,
2026-10-09). A run now stores the caller's pane, session and workspace; every
attempt's worker belongs to that pane, workspace and coordinator tenure; a
resume from another live pane is refused (`run_owned_elsewhere`), and after
the owner pane or agent is gone (herdr's own events, no timer) a resume takes
the run over as a `run_owner_taken` event (658e3924, run `r-6edkrlwq`).

## Coordinators stopping between items

Coordinators ended turns between approved items without being asked
(user, 2026-10-03 onwards). Done: the rule line in the global instructions,
the coordinator Stop hook (worker obligations, the ABANDON wording), the
headless trial (2026-10-07: 5/5 runs, 0 abandoned) and the audit of real
transcripts (2026-10-09, `docs/coordinator-audit-2026-10-09.md`: 244 turn
ends, 0 caught by wording, 3 silent stops by hand, 1.2 per 100 vs 5.6
before). The follow-up is the item "A state-based coordinator stop check,
in shadow mode first" (decided by the user 2026-10-09).

## Enforcing the added-delay rule

The global rule "added delay is a bug signal" (dotfiles c348422) did not
hold on its own, so it got enforcement: a PreToolUse hook
(`~/.claude/hooks/delay-check.py`, dotfiles be4a352) denies waits and
give-up times without a `delay: <reason>` marker; zero-length values pass
since 2026-10-09 (dotfiles 903e9f0, which also stopped `TIMEOUT = 0.5`
slipping through). Headless workers start with `disableAllHooks`, so herdr
runs `[workers] pre_tool_checks` through an SDK PreToolUse callback instead
(d2c166a6; failures fail open as `pre_tool_check_failed` events), and the
user's herdr config sets it to the delay check (dotfiles 9c70342, approved
2026-10-09).

## Headless workers do not wait in the background

A headless worker ran its tests with `run_in_background`, armed a Monitor
and ended its `claude -p` turn; nothing wakes a headless worker, so its
work sat uncommitted (run `r-idd6p7io`, 2026-10-09). herdr now denies every
headless worker `run_in_background` (Bash, Agent), `Monitor`,
`ScheduleWakeup` and `CronCreate`, in its policy and in an always-registered
PreToolUse callback, with the reason that nothing wakes it; the driver's
task contract says the same (5be67f20, run `r-xelv6o5o`).

## Broker tests fail fast in a sandbox

Broker tests waited 600 s in a worker's sandbox (no Unix sockets) on a
`recv` that never ended, and their processes outlived the worker. They now
fail at once naming the sandbox when a socket is refused (measured 311 µs
and 11.8 ms), and a failing test reaps its brokers before anything else
(15ef5230, run `r-a2kcuznp`). Eight orphaned test brokers from 2026-10-08,
from before the reaper, were ended by hand on 2026-10-09.

## Guards against a runaway wait loop

A coordinator's retry loop around `herdr-job run` opened ~400 tabs and used
up the Mac's PTYs (2026-10-07). Guards: herdr-job caps running and failed
jobs per owner pane and globally before a tab exists and refuses a name
that failed 3 times in 10 minutes (02796a77); `herdr-job wait-agent` waits
in one job and retries only transport errors; herdr refuses a new pane
cleanly as `pty_exhausted` below 64 free PTYs and reports `server.pty_usage`
(5de0c204, cca46c1f). The agent rule "one wait command per wait, no retry
loop around herdr-job; on a failing wait check status once and report;
bound every loop" is in ~/.claude/CLAUDE.md (dotfiles 082d7ab) and Rule 10
of ~/personal_projects/agents.md/AGENTS.md (7151ac3), 2026-10-09.

## Agent accounts and usage limits

From Omarchy's "agent account" (2026-10-07): Claude's Keychain item name
honours `CLAUDE_CONFIG_DIR` (a4c5885b); `usage.read` reports the account
and, per window, `observed_at` and `fresh | stale | failed` (stale after two
poll intervals or a passed reset; never shown as 0% or 100%; f03549b8);
`herdr todo run` refuses a new run while a Claude window is fresh and >=90%
or the reading is stale or failed, admits again on a fresh reading below 80%
everywhere, never stops an admitted run, `--ignore-usage` overrides
(142c64a0; the user chose "refuse" for unknown data, 2026-10-09; the old
"switch to pi" plan was unreachable because the driver starts only headless
Claude). Named accounts (`--account`) wait until the user has a second
account per provider.

## From the T3 Code comparison to headless workers

Comparing T3 Code (agents through their SDKs, not a PTY) led to herdr's
headless workers (2026-10-07): `claude -p` stream-json under herdr's tool
policy and sandbox, sidebar lines, a log popup and takeover. The open parts
of that item are done since: headless workers became the coordinators'
default (the `todo` skill and rule), fewer questions (policy, sandbox,
folder slot), wake on questions (`worker wait --attention`), commit rules
enforced by `herdr worker verify`, and the driver `herdr todo run`
(2026-10-08/09). A second, module-by-module comparison is its own item.

## Atomicity fixes from the 2026-10-07 review

The atomicity review (`docs/atomicity-review-2026-10-07.md`; user: "it must
be like a database transaction") produced eight fixes, all done by
2026-10-09: answers name their question; takeovers claimed once, released
on failure, their tab carrying a takeover id; a live handoff keeps
headless workers (later: brokers that survive it); `worker kill` checks
process identity; build and install under one lock (`just clean-install`);
herdr-job locks follow the command; one coordinator per repository
(tenures) and a driver whose cherry-pick, install, TODO edit and push each
record intent and result; and herdr-job writes a job's record, holding a
launch lock, before creating its tab, so a dead launcher leaves no orphan
tab (98cf1d11). The other findings were folded into the reliability plan.

## Headless workers rarely ask

Headless workers ask the user almost never (2026-10-07/09): Claude Code's
Bash sandbox with herdr's policy (sandboxed Bash allowed, `.env` and
credential paths denied at any depth, credential env vars removed, a private
temp dir, no network), the persistent folder slot with a warm `target/`,
waits that wake on a worker's question (`worker wait --attention`), and the
post-turn checks in `herdr worker verify` (one commit, exact subject,
allowed paths, clean tree, no leftover process). A real smoke run asked 0
questions; the driver runs since have asked none.

## The usage gate tests read one clock

A usage gate test failed once under load: the test and the gate each read the
clock, and a second could pass between them ("read 31s ago"). The gate's
tests now judge readings at one fixed test instant; reproduced
deterministically by a temporary loop waiting for the next second, 30/30
stress passes after (df272dc1, the first commit landed with `Herdr-Item` and
`Herdr-Run` trailers).
