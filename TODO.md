# TODO

## Next, in order

- [x] Bug (user, 2026-10-01, screenshot: header shows `?1 ✉2`, three agents
  work, no `◐`): at the default 32 columns the sort buttons `manual name ↑ prio
  ↓` (21 columns) left room for one indicator, so the working count and the
  bookmark star were silently dropped. "Sessions that ask should be next to the
  working ones." Consulted DeepSeek, Opus and GPT (unanimous): one sort button
  that shows the current key and opens a choice, never hide a non-zero
  indicator, keep `◐` and `?` adjacent. Done: the header has `⇅ manual` /
  `⇅ name ↑` / `⇅ prio ↓` (the current key and direction); a click opens a
  small menu of the three keys (the active one marked, a repeat flips its
  direction); the indicators `★ ◐ ? ✉` fit beside it at 32 columns (test
  `at_32_columns_every_indicator_fits_beside_the_one_sort_button`). Not done:
  the models' fallback when counts of two digits still overflow (drop spaces,
  then a second row); today the leftmost indicator that does not fit is left
  out; checking `⇅` and `★` when the terminal draws ambiguous-width glyphs
  double.

- [x] Header lists follow the tab look (user, 2026-10-01, several messages):
  (1) the lists need the tab's state like the sidebar tab lines; (2) the
  bookmark list needs a right-click menu with "Remove from bookmarks"; (3) the
  history rows too show the tab's state; (4) "when I click bookmarks the
  background of ★4 should change to show it is open; and then the stars before
  the tab names can go; do the same for the others"; (5) "the colours must match
  the ones in the spaces". Consulted DeepSeek, Opus and GPT.
  - Done 2026-10-01: a row whose tab still exists shows the tab's state icon
    in its colour and animation (`space_tabs::tab_state_icon`: the agent state
    with the question or job mark, or the program mark) instead of the mark in
    the text (`★ ? ◐ ✓`); the history rows keep their time before it; rows
    whose tab is gone keep the mark. A right click on any list row opens that
    tab's menu (it has "Remove from bookmarks"). The button of an open list is
    a filled accent pill (one cell of padding each side, bold contrasting text),
    only one at a time; clicking it again closes it. Colours: `◐` the working
    yellow, `?` the question colour of the tab lines, `★` neutral (mauve means
    waiting on a job), `✉` the accent. Tests: `the_button_of_an_open_list_is_
    filled_with_the_accent`.
  - Not done: a hover tint, a dim zero instead of hiding, splitting `◐` into
    working and job-waiting counts (the models say do not mix colours in one
    count).

- [ ] Flaky tests under load (seen 2026-10-01 while other sessions built):
  `client_mode::federated_client_starts_without_local_and_survives_its_restart`
  (4 of the last 8 full runs, passes alone) and once `app::api::plugins::tests::
  plugin_pane_open_uses_plugin_root_title_env_and_target_context` ("bin path"
  panic at `plugins/mod.rs:1938`, passes alone). Both pass on a rerun; neither
  investigated. Done: nothing.

- [x] History rows were cramped and unaligned (user, 2026-10-02, screenshot: the
  time glued to the state icon; "some rows have only a date, some date and
  time, not one column"; yesterday's entries have date and time, today's only the
  time). Consulted DeepSeek and GPT: GPT keeps the mixed format right-aligned,
  DeepSeek groups by day with separators and only `HH:MM` per row.
  - Done 2026-10-02: the lists draw one time column (right-aligned to the
    widest time shown, so `00:22` and `Sep 29 00:05` end together), a space, one
    icon column, a space, the text; a row with no live tab puts its own mark
    (`✓`, `?`) in the icon column, so the words of all rows start in one
    column. Test: `history_times_and_icons_form_columns_whatever_their_format`.
  - Not done: day separators with times only (DeepSeek's variant, kept here if
    the mixed format still reads badly); a fixed 12-cell time column (GPT) so the
    text stays still when a new entry changes the widest time.

- [x] No `?` although the agent "reported" (user, 2026-10-01, screenshot of a
  Haiku 4.5 session in job-seeker: the tab kept the idle ring). Diagnosis: the
  turn ended "Czekam na ITDS: Masz email od Barbary albo link do Lea
  screening?" and then a last paragraph that was the text `herdr agent
  awaiting-reply`, printed in the message and never run as a Bash call (so no
  report). The session also started before the Stop hook existed. Consulted
  DeepSeek and GPT (agree): small models take a command-like string for content;
  say "call the Bash tool", forbid writing it, give an example; the Stop hook
  should catch a final paragraph that is only the command; do not execute text
  scraped from a message (injection, masks the bug).
  - Done 2026-10-01: the Stop check treats a final paragraph equal to the
    command (also in backticks, a fence or after `$ `) as a missed report and
    blocks once with "you wrote the command in your message instead of running
    it; call the Bash tool with it"; the instruction in the SessionStart
    context, the per-prompt reminder (Claude, `.sh` and `.ps1`) and the Pi
    extension now says "call the Bash tool with `herdr agent awaiting-reply`
    (never write the command in your reply)"; the audit counts `printed` turns.
    Tests in `scripts/test_awaiting_reply_audit.py` (parity of the detector,
    block once, passes when run). Needs `herdr integration install claude` to
    reach `~/.claude`, and a restart of the sessions.
  - Not done: a usage-example line in the instruction; prevalence of the
    printed command by model (rerun the audit on new transcripts); the Pi side
    of the Stop check.

- [x] Dropdown without the preview at the bottom (user, 2026-10-01: "can we do
  without that preview below? maybe the bubble could grow to the right as with
  tab names?"). Consulted DeepSeek and GPT (agree: size to the longest row,
  stable while the highlight moves, left edge under the button and shifted left
  when the right edge is short, ellipsis for what still does not fit, no
  footer).
  - Done 2026-10-01: the header lists (history, working, asking, bookmarks) are
    as wide as their longest row (at least 56 columns, at most the screen less 2
    and 140) and the footer with the repeated row is gone; the box moves left
    when the right edge is short. Test: `the_notification_list_grows_to_the_
    right_to_show_a_long_row_whole`.
  - Not done: a row longer than the screen still gets an ellipsis (GPT: a
    tooltip or detail action for the full label; DeepSeek: wrap the highlighted
    row); the width follows the rows, so a refresh that brings a longer row
    changes it.

- [x] A jump into a collapsed space shows nothing (user, 2026-10-01, screenshot:
  he clicked a task with a `?` in the asking list; its space `~` was collapsed
  (`►`), so the tab focused but stayed hidden; "it should have expanded"). Third
  in his queue. Consulted DeepSeek and GPT: both scroll to the tab and extend
  the same behaviour to other focus moves; they differ on the kind of expansion:
  GPT persistent (an explicit jump means "take me there", like a manual toggle),
  DeepSeek transient (collapse again when focus leaves; persisting overrides a
  deliberate collapse).
  - Done 2026-10-01 (the persistent way, the simpler one): a click or Enter on a
    row of any header list (asking, working, bookmarks, history) opens the
    target's collapsed space and, for a worktree child, the collapsed group
    above it, saves that like a manual toggle, and scrolls the list to the tab.
    Test: `a_jump_from_a_list_opens_the_collapsed_space_it_lands_in`.
  - Also (user, same day: "creating a new tab should expand the space too";
    DeepSeek and GPT: persistent for tabs the user creates, none for
    background automation): done: every `TabCreate` that takes the focus and
    leaves the client shell (the `+`, the tab menu, the new-tab key, the rename
    prompt for a new tab, reopening a closed tab) opens the target's collapsed
    space and group first; tabs created by the API or an agent never pass
    through the client shell, so they leave the collapse alone. Test: `a_new_tab_
    opens_the_collapsed_space_it_is_created_in`.
  - Not done: the transient variant (kept here in case the persistent one is
    unwelcome: expand for the jump, collapse again when the focus leaves); other
    focus moves that land in a collapsed space (keyboard navigation, a toast
    click, `pane.focus`), where GPT wants an opt-out for background callers.

- [x] The open header button lost its colour too (user, 2026-10-01: "same
  here it loses its colour", screenshot of the `◐4` pill in solid blue with white
  text). Consulted DeepSeek (outline `▐◐4▌`) and GPT (tint); chose the tint, as
  in the dropdown rows. Done: an open list's button has a light accent tint
  (a sixth on light, a quarter on dark themes) and its glyph and count keep their
  colour, in bold; without an RGB palette the solid pill stays. Test: `the_
  button_of_an_open_list_is_tinted_and_keeps_its_own_colour`. Not done: a darker
  or lighter shade for a blue `?` or envelope if a theme's tint swallows it.

- [x] Three polish items (user, 2026-10-01, screenshots; consulted DeepSeek,
  Opus and GPT):
  (1) "the job summaries on a space row (`◐ 2 !2`) should be right-aligned":
  Done: on a space's name row the summary sits just left of the `+`, a column
  down the sidebar; the name is cut first, never the counts (test `the_job_
  summary_on_a_space_row_is_right_aligned`).
  (2) "the dropdown still vanishes and the remove popup stays": Done: the
  bookmark row's menu is now part of the list overlay (`log.menu`): it opens
  over the list, its top border on the clicked row; Esc, any key or a click
  elsewhere closes only the menu; Enter or a click on the item removes the
  bookmark and the list stays open (the row goes when the snapshot returns).
  Not done: keeping the highlight on the neighbour row after the removal,
  Shift+F10 for the keyboard (Delete or `x` already remove directly).
  (3) "why is the icon white when the row is highlighted, not its own colour?":
  the solid accent fill forced white; DeepSeek and GPT: do not recolour state
  icons when selected. Done: the highlighted row of the dropdown lists is a
  light accent tint (a sixth on light, a quarter on dark themes) with an accent
  bar `▌` in the first column and normal text, so every icon keeps its colour
  and animation; without an RGB palette the solid fill stays. The menus
  (context, sort) keep the solid highlight, they have no icons. Not done: a
  darker/lighter shade for an icon whose hue is too close to the tint (the
  models' contrast rule), the unread dot is hidden on the highlighted row.

- [x] The spaces filter (user, 2026-10-01, with a screenshot): (1) "why is there
  a `/` before `filter`: when I click in the spaces panel to focus it and press
  `/`, it types into the agent's command line"; (2) "I click filter at the
  bottom and the input appears at the top: move the whole control to the very
  top"; (3) "it highlights strangely when I start typing" (the arrow-selected
  space had a pale blue fill over its whole block). Consulted DeepSeek, Opus and
  GPT.
  - Done 2026-10-01: the bottom `/ filter` button is gone; a `⌕` button follows
    the sort button in the header (no slash, so it does not promise a key);
    while the filter is open its bar (`/ query`, the `shown/total` count, `×`)
    takes the header row itself, the sort button and indicators wait, and the
    list does not move down by a row; the space the arrows chose has an accent
    bar (`▍`) down its whole block instead of the pale fill. Test: `the_filter_
    lives_in_the_header_row_and_its_cursor_is_a_bar_not_a_fill`.
  - Not done (the models' further proposals): the default key is `prefix+/`
    (`filter_spaces`), so a bare `/` goes to the agent by design; `/` opening the filter when the
    spaces panel has keyboard focus (a sidebar-scoped binding; today a click in
    the panel focuses the pane, so `/` reaches the agent; needs a sidebar
    focus mode and Esc returning to the pane); dimming tab lines that do not
    match; showing the matching hidden field (the path or branch) as a dim line;
    `no spaces match "x"` with the count in the error colour for zero matches;
    two-stage Esc (clear, then close).

- [ ] A silent job looks the same as a stuck one (user, 2026-10-01, screenshot of
  the job "rescue builder VM": the pane shows only `$ ./builder-vm.sh` and a
  cursor, the running icon turns; "is anything executing here?"). Checked: yes.
  `herdr-job` (pid 10823) runs `./builder-vm.sh`, which started a
  `qemu-system-aarch64` (pid 10862, 1h48m old, 60% CPU, 138 CPU-minutes,
  `-display none`), so the pane has no output by design and the icon is
  honest, but nothing in herdr says so. Consulted DeepSeek, Opus and GPT; they
  agree on the design:
  - Honest evidence only, never a progress or health verdict: elapsed time;
    time since the last output byte (`quiet 1h48m`, from job start before any
    output); the busiest child by name (`qemu-system-aarch64`); CPU as the
    delta of the process tree's CPU time over a window (100% is one core),
    labelled "CPU", never "progress" (a hung loop burns CPU, a healthy
    network wait uses none); process state (stopped or zombie is a real
    signal). Heartbeats only if the command opts in.
  - Where: the job footer, `running 1h48m · quiet 1h48m · qemu 60% CPU`; the
    tab-line and job-square tooltips with a few processes; `herdr-job list
    --details` for pid, start, tree, CPU window, sample age. The tab bar
    stays as is.
  - Icon: after about 2-5 minutes without output the spinner becomes a static
    ring labelled `quiet`, still the running colour; it turns again on the first
    output byte; no "hung" verdict (Opus adds `quiet · idle` after 5 minutes at
    about 0% CPU).
  - Where it runs: the server, or herdr-job itself, samples only running jobs
    (every 5-10 s, adaptive; macOS `proc_listchildpids`/`proc_pidinfo`, Linux
    `/proc`; the tree capped at 64 processes; a process is a pid plus its start
    time against pid reuse) and pushes changed rounded values as events; the
    client reads the cache and never polls; per-pane rendering allocates
    nothing.
  - Tests: a silent `sleep 1000`, a busy loop, an idle wait, a QEMU-like child
    with no display, output resuming, exit states, `kill -STOP`, pid reuse,
    stale samples, 50 jobs within the budget, an idle client sending nothing.
  - Done: nothing yet.

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

- [x] Close dialog was cluttered and named the tab by a number (user,
  2026-10-02, screenshot: `Close pane with running work? / pane in 5 / stops:
  claude idle in 5`; "the task name as on the tab is not visible; too cluttered;
  why the number 5?"). Consulted DeepSeek and GPT: order title, target (the
  sidebar label), consequence, resume hint; short titles, no numbers.
  - Done 2026-10-02: the dialogs name tabs by their sidebar label (the given
    name, else the task); the title is `Close pane?` / `Close tab?` (no longer
    "with running work", which was false for an idle agent); the line under it is
    the label alone, and `stops:` no longer repeats it (`stops: claude waiting`).
    Test: `the_close_dialog_names_the_tab_by_its_task_and_does_not_repeat_it`.
  - Not done: the models' wording of the consequence ("ends the idle Claude
    session, unsent input and scrollback are lost, you can resume"), the resume
    hint, `Close space?` listing its tabs.

- [x] Closing a tab unfolds the jobs of the previous one (user, 2026-10-02, two
  screenshots: the "Herdr sessions…" line with 17 squares open after a job tab
  closed). Cause: focusing a job opens its parent's squares so the sidebar shows
  where you are, and the opening was kept; when the job closes the focus goes
  back to the parent, which stayed spread out. Done: what focusing a job opened
  (`auto_unfolded`) folds again as soon as the focus is no longer on one of that
  parent's jobs; squares the user unfolded stay. Test: `squares_opened_for_a_
  focused_job_fold_again_when_the_focus_leaves_the_job`. Consulted DeepSeek and
  GPT afterwards: the rule is right (a temporary cue, manual unfolds persist; no
  timers or pointer rules). Not done: restoring the auto-unfold from the active
  job after a restart (squares are not saved), DeepSeek's debounce when the focus
  only passes through the parent.

- [x] Bookmark rows and popups (user, 2026-10-01, three screenshots): (1) the
  bookmark list showed "1 · job-seeker" and "2 · herdr" where the sidebar says
  "Job search automation": "why a tab number or name when there is a task
  name"; (2) a right click on a bookmark row closed the list and left a full tab
  menu hanging, one line low; (3) "why is there this offset?" (the dropdown's
  left edge sat one column right of the pill). Consulted DeepSeek and GPT.
  - Done 2026-10-01: a bookmark row uses exactly the sidebar tab line's label
    (`sidebar_tab_label`: the given name, else the task, else the program, else
    the number) and appends the space name only when it differs from the label
    (the models want the space kept for ambiguous names such as two `zsh`, or
    group headers); the right-click menu of a bookmark row has one item,
    "Remove from bookmarks", with its top border at the clicked row (DeepSeek
    preferred the first item there, GPT the corner at the pointer, as the other
    menus do); the working, asking and bookmark buttons now report the pill
    (label and one cell each side) as their rect, so the dropdown's left edge
    meets the pill's. Test: `a_bookmark_row_shows_the_tabs_task_and_its_right_
    click_menu_only_removes_it`.
  - Not done: space group headers in the bookmark list; reopening the list
    after a removal from its menu (the list closes).
  - Also (user, same day, "no padding between Remove and the left edge, is it
    meant to be so?"; DeepSeek and GPT: no): context menu items now have one
    column of padding each side, the highlight bar spanning both
    (`render_context_menu`); the dropdown lists already have a gutter.

- [x] Unfolding a tab line's squares near the bottom shows nothing (user,
  2026-10-01: "I click and the jobs list does not unfold"; later "maybe I could
  before, but I had to scroll the spaces, like with a new tab"). The squares
  were below the visible rows. Consulted DeepSeek and GPT (agree): after an
  unfold scroll the least that shows the line, its squares and the empty row
  after them; a block taller than the list puts the line at the top; folding
  does not scroll; keyboard unfolds do the same. Done: a click on the triangle
  sets `reveal_unfolded_tab`, and the next render scrolls to
  `tab_line_extent`; test `unfolding_a_tab_line_near_the_bottom_scrolls_its_
  squares_into_view`. Not done: the multi-machine sidebar, and unfolding by
  keyboard (none exists).

- [x] Regression (user, 2026-10-01: "I was able to close a tab with a Claude
  session inside without confirmation"): my earlier change (no question for an
  idle agent with nothing pending) also let an idle interactive Claude session
  in a normal tab close silently. Consulted DeepSeek, Opus and GPT (agree: the
  rule confused "not working" with "disposable"): an agent session in a normal
  tab asks in every state, also idle; only the leftover agent of a finished job
  tab is exempt (it asks only while it works or has background tasks). Closing
  ends the live process and loses unsent input, queued context and scrollback;
  a resume brings back only the saved conversation.
  - Done 2026-10-01: `close_impact::pane_work` restored for normal tabs, the
    idle case tested (`an_idle_agent_session_in_a_normal_tab_is_still_asked_
    about`). Not done (the models' proposals): skipping the question for a
    truly fresh session (no turns, empty input), which needs data the snapshot
    lacks; the dialog text with the saved turn count and `claude --resume
    <id>`; a `confirm_close = always | work | never` enum replacing
    `confirm_close_running`.

- [x] A renamed tab keeps its old name in the lists (user, 2026-10-01, screenshot:
  the tab was renamed `try-roguix` but the working list and its detail line still
  said "Session import"). Consulted DeepSeek and GPT (agree): the live lists use
  the sidebar's label, and the agent task stays after it so a name does not hide
  what the agent does; history rows keep their stored task but also show the
  tab's current custom name while the tab exists. Done: a custom tab name is
  shown first (`? try-guix · Session import · claude · space`) in the working,
  asking and history lists, looked up by tab id; test `a_renamed_tab_shows_its_
  name_in_the_agent_lists_and_old_history_rows`. Not done: a name that
  `ui.tab_label` derives from the title is already the task, so nothing changes
  for it.

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
  - Done: nothing yet.

- [x] Tab bookmarks (user, 2026-10-01: "right click on a tab, add to
  bookmarks. It shows at the top, left of the `◓2`. The order in that list is
  the order of the spaces. Do it next."). Consulted DeepSeek, Opus and GPT;
  they agree on the core:
  - Storage: a flag on the tab in the server (`bookmarked`, optional in the
    snapshot and `tab.list`, absent means false), persisted with the session,
    shared by all clients; a closed tab or a deleted space takes its bookmark
    with it, a moved tab keeps it and the list re-sorts, reordering spaces
    reorders the list (computed live from the tree, nothing stored).
  - API (project rule: shared facts go through the server): idempotent
    `tab.bookmark {tab_id, enabled}`, CLI `herdr tab bookmark|unbookmark`,
    a change event for all clients.
  - Header button: `★N` in the accent colour left of the other indicators
    (`★2 ◓1 ?1 ✉3`); the models differ on an empty state (hide, or a dim `☆`
    clickable with a hint "Right-click a tab, Add to bookmarks"); at 32
    columns drop the count first, then the button; the live agent indicators
    keep priority.
  - Dropdown (the notification list's): rows ordered by space, then tab; the
    agent state icon, the tab title and the space name (DeepSeek and Opus group
    by space with dim headers, GPT keeps it flat to save height); the current
    tab highlighted; a click jumps and closes; a trailing `×` removes (middle
    click as a shortcut only).
  - Entry points: the context menu of a tab line and of a tab-bar tab
    ("Add to bookmarks" / "Remove from bookmarks", by state), a keybinding
    that toggles the focused tab; job tabs only when they are real tabs.
  - Tests: persistence over a restart, an old snapshot without the field, two
    clients, close, move, space delete and reorder, order, jump, remove,
    32-column degradation, empty state, CLI.
  - Done 2026-10-01 (committed, not installed): server `tab.bookmark`
    (idempotent; `bookmarked` in `tab.list`/`tab.get` and the client snapshot,
    persisted with the session, optional everywhere; CLI `herdr tab bookmark|
    unbookmark <id>`; the method is advertised to client shells and its shape
    frozen next to the other additive methods); the tab context menu (tab
    lines and tab-bar tabs) has "Add to bookmarks" / "Remove from bookmarks"
    when the server supports it; a `★N` button, hidden at zero, left of the
    other header indicators lists the bookmarked tabs in the order of the
    spaces, then their tabs; a click or Enter jumps, a middle click, Delete,
    Backspace or `x` removes. Not done: a keybinding that toggles the focused
    tab, the agent state icon in the rows, the empty-state hint, a change
    event for other clients (they pick it up with the next snapshot).

- [x] An agent's own todo list is invisible in herdr (user, 2026-10-01: "an
  instance has a list of things it will do from its todo, and I do not see it").
  Claude Code keeps it as TodoWrite (and newer TaskCreate/TaskUpdate), Pi and
  Codex have plans; each shows it only in its own pane. Consulted DeepSeek, Opus
  and GPT (agree on the approach):
  - Source: structured events only, never screen scraping (the detection rule
    is evidence-based): a Claude Code `PostToolUse` hook matching `TodoWrite`
    (`tool_input.todos` is the whole list each time; the task tools are deltas
    to fold in), a Pi extension that reports on every change, Codex's
    `update_plan` through its hook or, as an opt-in fallback, transcript
    tailing (private format, privacy and rotation costs). No evidence means
    unknown, not zero tasks. Verify the payloads with recorded fixtures first.
  - API (neutral, optional): `pane.report_plan {pane_id, items|null}` with a
    full replacement list, `pending|in_progress|completed`, and an optional
    `plan` on the agent info (absent unknown, empty cleared); limits of 50-100
    items, 200-512 characters each, 8-64 KiB per report; strip control and ANSI
    sequences, newlines become spaces; clear on a new session, `/clear` and
    agent exit; GPT adds `session_id` and a `revision` to reject stale reports.
  - Presentation: a quiet `3/7` token in the agent row (hidden when unknown or
    empty, dim when complete), the in-progress item in the focused pane's
    detail line, a keyboard-reachable dropdown of the items with `✓ ▸ ·` only
    on request; no full list in the sidebar.
  - Rollout: API and validation tests, the Claude hook with a recorded
    payload, the token and footer, the Pi extension, the dropdown, Codex.
  - Done 2026-10-02 (first slice, Claude Code only; uses the existing token API
    instead of a new method): a `PostToolUse` hook for `TodoWrite`
    (`herdr-agent-state.sh plan`, installed with the other hooks, matcher
    `TodoWrite`) reports `done/total` of the todo list as the `plan` token with
    `pane.report_metadata` (kept 6 hours, renewed by each `TodoWrite`; an empty
    list clears it; `HERDR_AGENT_PLAN=0` turns it off); the sidebar shows it dim
    at the right end of the tab's line (`agent tab      3/7`). Tests:
    `PlanHook` in `scripts/test_awaiting_reply_audit.py`, the install tests and
    `a_tab_line_shows_the_progress_of_the_agents_todo_list`. Not done: the
    in-progress item in a tooltip or dropdown; a clean-up at `SessionStart` or
    `/clear` (the token lapses after 6 hours); `TaskCreate`/`TaskUpdate`; Pi and
    Codex; the audit's note on how often agents use `TodoWrite`. Needs
    `herdr integration install claude` and a restart of sessions.

- [x] Bug (reported 2026-10-01 16:48 through another session, screenshot
  `~/.local/share/herdr-bug-reports/2026-10-01-working-agents-dropdown.png`):
  three agents look like they work in the sidebar but the new `◐` list showed
  two. The missing one had the purple icon: an agent that is idle or done
  while a child job tab it started still runs (the "waits on a job" mark),
  and the indicator counted only `status == Working`.
  - Consulted GPT sol and DeepSeek (agree): the count and the list must use
    the sidebar's own predicate, `(Working or waits-on-job) and not asking`,
    one row per agent, a job waiter marked `waits on a job` in the row.
  - Done 2026-10-01 (committed, not installed): `agent_is_working` uses
    `agent_mark`, the function that draws the icon, so they cannot drift;
    test `the_working_list_includes_an_agent_that_waits_on_a_running_job`.
    Not done: a mauve glyph in the list rows, an audit of other
    working-looking states (`bg` tokens, orphan jobs).

- [x] Spaces are hard to tell apart and the focused tab is too faint (user,
  2026-10-01, screenshot of the light theme: "I lose where a space starts and
  where it ends", "the light blue highlight of the active tab is too faint").
  Consulted DeepSeek, Opus and GPT; all three: a header band beats a rule or
  blank row (no extra rows), the focused tab needs an accent bar plus a
  stronger tint, not a solid fill (the job counts must stay readable).
  - Done 2026-10-01: a space's name row is a band (the text colour over the
    panel, a fifth on light and a sixth on dark themes; the focused space's
    band is tinted with the accent, a fifth or a quarter); the branch row stays
    plain; the focused active tab's fill is a third (light) or a half (dark)
    accent instead of a sixth or a third, with an accent bar `▌` in the fill's
    first column. Tests: `a_space_name_row_is_a_band_and_the_focused_active_
    tab_has_a_bar`. Not done: contrast ratios checked in tests, a monochrome
    or 256-colour fallback for the bar and band (they need RGB; other palettes
    keep the old look), the exact shades are untested on a real screen.
  - Revised 2026-10-01 after two more complaints with screenshots ("this grey
    is too dark, mark the boundary differently", "why does the background not
    reach the right edge", and the same stripe on a nested worktree space):
    the grey band is gone. Consulted DeepSeek, Opus and GPT again; Opus and
    GPT: (b) a bar in the name row's first column, no fill (DeepSeek preferred
    a very light accent-tinted band). Now: `▍` in the first column of a
    top-level space's name row, the accent for the focused space and the text
    colour half mixed into the panel for the others; the name is bold in the
    text colour (vertical tabs only); a nested worktree space keeps its tree
    connector and gets no bar. The tab lines keep their fills, so the filled
    rows are the tabs. The earlier right-edge problem is moot (no fill). Not
    done: checking `▍` in Ghostty/iTerm/Kitty with line spacing above 1.

- [x] Wheel scrolling in the spaces list moves one row per event (user,
  2026-10-01), like the agents panel; it was three. Consulted DeepSeek, Opus
  and GPT (agree: one row per event, a fixed constant, no config option,
  keyboard and scrollbar drag untouched). Done: `workspace_wheel_step` is 1;
  test `one_wheel_event_scrolls_the_spaces_list_by_one_row`.

- [ ] Scroll direction and speed per operating system (user, 2026-10-01: "is
  scrolling in herdr (spaces, the main screen of a Claude instance, etc.)
  natural like macOS or like Windows? It should not be configured; defaults
  by operating system? Ask the models."). Third item to do. To investigate:
  what herdr does with wheel events in every surface (spaces list, agents
  panel, panes, overlays, tab bar), what the terminals send on macOS (natural
  scrolling is applied by the OS before the terminal, so the direction is
  already right) and on Windows and Linux, whether the step (rows per event)
  should differ by platform, and whether a config option is needed at all.
  - Answer (consulted DeepSeek, Opus and GPT 2026-10-01, unanimous on the
    first point): the direction is not herdr's. The OS applies "natural
    scrolling" (macOS) or its inverse before the terminal reports a wheel
    event, and the terminal sends only "wheel up" or "wheel down" without a
    magnitude; herdr applies what it gets and must never invert, or a user
    gets a double inversion. So it is neither "like macOS" nor "like Windows":
    it follows the user's system setting. Not audited yet: that every surface
    (pane scrollback, spaces list, agents panel, overlay lists, tab bar, the
    selection moving in a list) maps wheel-up to "earlier or higher content"
    with the same sign; a test per surface should assert it.
  - Speed is where herdr is inconsistent: `ui.mouse_scroll_lines` (default 3)
    for pane scrollback, 1 row for the spaces list and agents panel, fixed 1
    or 3 for overlays, 3 synthetic wheel events per step for alt-screen reads.
    The models disagree on the cure: the platform is the wrong axis (the
    device matters, a mouse notch against a trackpad flick, and over SSH the
    server cannot see the client's OS; a client-side handshake could report the
    local OS but not the device); Opus and DeepSeek propose one step function
    by event rhythm (an isolated event is a notch and moves N rows, a burst
    under about 20-40 ms moves 1 row per event), lists always 1; GPT proposes a
    neutral default of 1 for everything herdr scrolls itself, forwarding one
    unchanged wheel event (not three) to a child that has mouse reporting, and
    no acceleration heuristics at first. All keep `ui.mouse_scroll_lines` as an
    advanced override rather than removing it.
  - Tests the models ask for: a pure `scroll_step` function with an injected
    clock, the sign on every surface, replayed raw SGR streams through a PTY,
    and a manual matrix (macOS Terminal.app, iTerm2, Ghostty with trackpad and
    mouse and natural scrolling on and off; Windows Terminal; Linux VTE and
    kitty; SSH from macOS and Windows to Linux).
  - Decision needed from the user: adaptive step (rhythm) or a plain 1 per
    event, and whether the alt-screen forward changes from 3 events to 1.
  - Done: nothing yet.

- Deferred Herdr behavior-context integrations: Pi and Claude Code are
  already implemented. The checkboxes below select future implementation
  scope, NOT completion status. All remaining agents start unchecked.
  Do not implement any of them until the user checks its box or explicitly
  requests that agent. Verify native context hooks first; support manual
  starts in arbitrary repositories without `AGENTS.md` edits. Preserve
  user hooks, truthful status reporting, opt-out and bounded/idempotent
  resume/compaction handling; no forced continuation or terminal injection.
  Test installed adapters inside/outside Herdr and document API limitations.
  - [ ] Codex (`codex`)
  - [ ] Gemini CLI (`gemini`)
  - [ ] Cursor Agent CLI (`cursor`)
  - [ ] Devin CLI (`devin`)
  - [ ] Antigravity CLI (`agy`)
  - [ ] Cline (`cline`)
  - [ ] Oh My Pi (`omp`)
  - [ ] MastraCode (`mastracode`)
  - [ ] OpenCode (`opencode`)
  - [ ] GitHub Copilot CLI (`copilot`)
  - [ ] Kimi Code CLI (`kimi`)
  - [ ] Kiro (`kiro`)
  - [ ] Droid (`droid`)
  - [ ] Amp (`amp`)
  - [ ] Grok CLI (`grok`)
  - [ ] Hermes Agent (`hermes`)
  - [ ] Kilo Code CLI (`kilo`)
  - [ ] Qoder CLI (`qodercli`)
  - [ ] Qwen Code (`qwen`)
  - [ ] Letta Code (`letta`)
  - [ ] Maki (`maki`)
  - [ ] Muse (`muse`)

- [x] Regression (reported 2026-09-30 16:05): closing a tab moved focus to
  the last herdr-job tab instead of a tab at the same nesting level.
  Hypothesis before the consult: `Workspace::close_tab` (`src/workspace.rs`)
  keeps the closed tab's flat index when the closed tab is the active one
  (`active_tab` stays, clamped to the new last tab), and
  `normalize_tab_groups` keeps each parent's children right after it, so
  that slot can hold a nested child. In `[agent, job₁…jobₙ, last main
  tab]` the last main tab's index is `n+1`; once it is removed
  `active_tab` clamps to `n`, the last job tab. herdr-job nests every job
  tab under the pane's tab (`herdr tab parent` in `plugins/job/herdr-job`),
  and `parse_tab_id` maps a tab id to its flat index, so the close uses
  that index. Closing a parent tab at index 0 focuses its first promoted
  job tab the same way (recorded hypothesis corrected by the consult: at a
  later index the flat-previous is the previous main tab's last child, so
  the landing tab differs, but it is still a nested child; the server
  refuses to close a tab that still has children, so that path is
  unreachable from a client — see the reproduction below). Rule to decide:
  the nearest tab at the same level (previous sibling, else the
  next one, else the row's parent) instead of the flat index. Consider
  `Alt-1…9` numbering, the sidebar's squares, and spaces whose only tabs
  are job tabs. Done 2026-10-01 (`faf566fa`): installed with live handoff
  and user-confirmed. Full `just check` passed (3808 tests plus lint,
  Windows lint, maintenance, integration and documentation checks).
  - Consulted DeepSeek 2026-10-01 (`7235da15`): agreed on previous sibling,
    then next sibling, then parent for an only child; preserve the active
    identity on inactive close. Highlighted stale raw parent links when
    state callers remove a parent directly: clear those links without
    reordering survivors. Implemented with stable tab numbers and tests
    for 14 focus scenarios, invalid/last-tab rejection and API projection.
    First-child close intentionally prefers its next sibling over its
    parent. No protocol or close-cascade changes.
  - Reproduced 2026-09-30 16:10 in a throwaway session
    (`herdr-throwaway-repro`, herdr 0.9.1, no agent tokens): tabs
    `[A, job1, job2, mainB]`, both jobs nested under A with
    `herdr tab parent`, `mainB` focused. `herdr tab close mainB` focused
    `job2`, the last job tab. Second layout `[X, x1, A]`, `x1` nested
    under X, `A` focused at index 2: closing `A` focused `x1`, a child of
    the previous main tab, not a tab at A's own level. Closing a tab that
    still has children is refused by the server (`tab_has_children`,
    `src/app/api/tabs.rs:233`) and the TUI closes a parent's children
    first (`request_parent_tab_close`), so the parent-promotion variant
    cannot be reached through the API; only `Workspace::close_tab` called
    directly, as `closing_a_parent_leaves_its_children_top_level` does,
    leaves that state. Session stopped, deleted, outer pane closed, no
    artifacts left.
  - Consulted Claude Sonnet 5.5 2026-09-30 at the user's request (consult
    id `fe6e9760`): the clamp explains the report only if the closed tab
    was the active one after a job group; the reproduction above confirms
    it. It corrected the recorded hypothesis: a parent closed at index 0
    moves focus to its first promoted job tab, but at a later index the
    flat-previous is the previous main tab's last child (checked in
    `close_tab`, still a nested child; that parent case is unreachable
    through the API, see the reproduction above). Recommended rule,
    server-side in
    `Workspace::close_tab`: focus changes only when the closed tab was
    active; a main tab without children -> previous main tab, else the next
    main tab, never a child; a child -> previous sibling, else the next
    sibling, else its parent; a parent -> compute the successor before its
    children are promoted and pick the previous main tab, else the next
    main tab, else the first promoted child (its only sensible choice as a
    lone parent). Choose the successor by identity (root pane), as
    `normalize_tab_groups` already remaps the active tab, instead of index
    arithmetic. Tests it asks for: the reported regression (last main tab
    after a job group), a parent closed at a later index, a parent at index
    0 (lone parent -> first promoted child), a child (siblings, then its
    parent), an inactive tab closed before/after/inside the active group,
    the workspace's last tab (`close_tab` returns false and the space close
    handles it), `Alt-1…9` after a close, the sidebar's squares following
    the newly active tab, and persistence of the active tab and the groups.
    Settled in the repo already: `tab.number` is monotonic and never reused
    (`tab_public_numbers_are_stable_and_not_reused_after_close`), so a
    dead parent link cannot adopt a new tab and `tab_parent_index` already
    treats such a child as top-level; the client sends no `tab.focus` of
    its own after a close in the paths checked. It did not run anything.

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
  - [ ] Why does the worktree group look like this (a bare `▼ name  +` row
    with a `└─` stub, unlike the tab rows above it)? Check which parent
    link and row kind the renderer uses for a worktree group.
  - [ ] Why does the worktree's `└─` connector hang under `ask gemini`, as
    if it were its child? Verify the real parent ids (`tab_parent_index`)
    versus a purely visual artefact of the connector drawing.
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

- [x] Dragging tabs in the spaces list does not work (2026-10-01, reported
  again; v1 done, see the entry below): pressing a tab line and moving starts no drag. The earlier entry
  "Dragging tabs in the spaces list does not work (2026-09-29)" below holds
  the design and the consultations; this one only records that the user
  still sees it and wants it done.

- [x] Show herdr's own toasts in the top right corner instead of the bottom
  right (2026-10-01). The option already exists: `[ui.toast.herdr]
  position = "top-right"` (values `top-left`, `top-right`, `bottom-left`,
  `bottom-right`; default `bottom-right`). The user's `config.toml` does not
  set it. Decide whether to set it there or to make the fork's default
  `top-right`; then check that a top-right toast does not hide the tab bar's
  right side or the notification button and that its dismiss click still
  works.
  - Set 2026-10-01 in `~/.config/herdr/config.toml` (`[ui.toast.herdr]
    position = "top-right"`), not in the fork's default. With
    `delivery = "system"` herdr's own toast shows only while the window is
    focused; the overlap checks above are still to do by eye.

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

- [x] Filter bar above the spaces list, like fzf (2026-10-01; v1 done, see below): a text field
  at the top of the sidebar that narrows the visible spaces and tabs as the
  user types. Not designed yet: decide what it matches (space names,
  branches, tab labels, agent names), the open/close key and mouse
  affordance, how a match is highlighted, what happens to folding and drag
  while a filter is active, what Enter selects, and where its state lives
  (client-only presentation state, not server state).
  - Consulted DeepSeek, Claude Opus 5.5 and Gemini (low/high) 2026-10-01;
    GPT sol hit the Plus limit again. Unanimous recommendations, not an
    approved design: match space name, branch, tab label and agent name
    (not cwd) with fzf-style fuzzy subsequence and smart case (a crate such
    as `nucleo-matcher`; Opus adds space-separated AND tokens and a `7/23`
    match counter). Keep tree order; score only picks the default selection.
    Keep the tree: a matching space shows all its tabs, a matching tab or
    child worktree shows its ancestors dimmed; collapsed groups expand for
    display only, without changing the stored fold state. Highlight matched
    characters. Open by clicking the bar or a key (`/` only when the
    sidebar has focus, or `prefix + /`; never steal printable keys from a
    focused pane). Up/Down move a selection kept by id, Enter focuses it
    and returns focus to the pane, Esc clears and then closes. Drag
    reordering is disabled while a filter is active. State is client-only,
    not persisted, no server requests; cache matches per
    (query, snapshot generation). Not in v1: cwd or scrollback matching,
    score ranking, fzf operator syntax, regex, saved queries, state filters.
  - v1 done 2026-10-01 (committed, not installed; `just check` passes):
    the `/ filter` button sits in the bottom row between `new` and `menu`
    (the header has no room at 26 columns) and opens a bar under the header,
    `/ text▏ ×`. While it is focused, typed text (printable keys,
    Backspace, Ctrl+U) goes to it; Esc clears the text, then closes; Enter
    opens the first matching space, or its first matching tab when the
    space itself does not match, and closes the bar. A click anywhere else
    blurs it (the filter stays on and the pane gets the keys); a click on the
    bar focuses it; `×` closes it. Matching is a smart-case subsequence
    (`space_filter::matches`) against a space's name, branch and agents, and
    a tab's label, its agents and the labels of the tabs nested under it. The
    list keeps its order; a matching space shows all its tabs, one shown
    for a tab only the matching tabs, a worktree parent stays for a
    matching child, folded groups open for the view only, `no match` when
    nothing fits. Space and tab drag are off while filtering (the header
    says `clear the filter to reorder`). Client-only, not saved.
  - Up/Down selection done 2026-10-01 (committed, not installed): Up and
    Down move a selected space (the list highlights it like the navigation
    selection and scrolls to it, no wrap), typing selects the first shown
    space again, and Enter opens the selected space, or its first matching
    tab when the space itself does not match.
  - Key done 2026-10-01 (committed, not installed): `keys.filter_spaces`
    (default `prefix+/`, documented in the sample config and the config
    reference) opens the bar for typing and shows a collapsed sidebar.
  - Done 2026-10-01 (committed): the bar shows `shown/total` spaces while a
    query is on, and the matched characters of a tab label are bold and
    underlined (`match_positions`).
  - Left for later: highlighting matches in space names and AND tokens, the multi-machine sidebar (it ignores the
    filter), the mobile layout, and a live check.

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

- [x] Indent vertical tab rows under nested worktree spaces (screenshot,
  2026-09-30 00:49). The `Job client footer` worktree header is indented,
  but its `zsh` tab aligns with the parent space's `pi - herdr` tab and
  appears to be a sibling rather than a child. Propagate the worktree
  depth consistently to tab rows, fills, job squares, tooltip anchors and
  hit rectangles; keep top-level spaces unchanged. Verify narrow widths,
  folding and list-scroll anchoring. Consulted DeepSeek 2026-09-30:
  confirmed that `entry.indented` reaches the worktree header but not tab-row
  geometry. Suggested one shared 5-column offset for child worktree tab
  rows, applied to measurement, render and focus reveal; subtract it from
  available width without moving the right edge. Shift fills, square/fold
  hits and tooltip anchors together. Test wrap boundaries with/without the
  scrollbar, narrow widths, partial rows and scroll anchoring. Decide gutter
  click behavior explicitly; do not create zero-width tooltip targets.
  - Done 2026-10-01 (committed, not yet installed): tab lines, fills, fold
    hits, tooltips and squares of an indented space move 5 columns right
    (`space_tabs::tab_indent`), and the width squares wrap in shrinks by the
    same amount in the measure, render and focus-reveal paths of both
    sidebars. A click in the gutter in front of the line still selects its
    tab. One scalar per entry, no new work per pane, so no scaling
    benchmark was run. `just check` passes.

- [x] Pi does not change its title to the task name as Claude CLI does:
  concurrent sidebar entries remain `π - herdr` (screenshot, 2026-09-29
  23:56). Investigate Pi's emitted terminal titles (OSC 0/2), available
  task/session metadata, and Herdr's title precedence before assigning a
  root cause. Consulted DeepSeek on 2026-09-29: use a manual OSC title
  probe to distinguish missing title emission from missing consumption;
  Claude may use a separate metadata integration. Prefer meaningful title
  or metadata updates at the source, not conversation-text scraping.
  Preserve explicit user names and avoid per-token title churn. Verify
  that two Pi sessions with different tasks have distinct labels, unknown
  tasks retain a fallback, and Claude's labels remain unchanged.
  - Root cause (verified 2026-10-01): Pi emits `π - <session name> - <cwd>`
    only once a session has a name, and never names one itself (only `/name`
    or an extension calling `pi.setSessionName`). The pane's
    `terminal_title` stayed `π - herdr` for the whole session; the label
    changed only after Pi exited because the shell then set its own title.
    Consulted DeepSeek and Gemini (low/high; medium returned nothing): all
    recommend a small Pi extension that names the session once from the first
    prompt, deterministic and without an LLM call.
  - Fixed (committed, not yet reloaded in a running Pi): `plugins/pi-title/`
    names the session once from the first interactive prompt (one line,
    controls and bidi removed, 48 graphemes), skips slash commands and keeps
    a name from `/name` or a resumed session. `plugins/pi-title/install`
    links it into `~/.pi/agent/extensions/`; run `/reload` in Pi. Unknown:
    whether Pi follows a symlinked extension file (checked only by unit
    tests with a mocked `pi`, not by a live Pi run).

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
- [x] A `claude` consult skill in `plugins/consult/skills/`, like `gpt` and
  `deepseek`: implemented as `ask_claude.py`, with Herdr-job visibility,
  consult-stats logging and links for Pi and Claude Code. Explicit Opus:
  `ask_claude.py -m claude-opus-5-5`; Sonnet remains the default. Both use
  `claude -p --model`, not a direct API client. Subscription billing requires
  Claude Code subscription login, not API-key/cloud-provider billing.
  - Opus argument forwarding and resolved-model logging have offline tests.
    Live attempt on 2026-10-01 was rejected by the weekly limit (reset 02:00
    Europe/Warsaw); this does not establish Opus availability. Do not retry
    before reset or silently substitute another model.
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
- [x] The job square's tooltip should appear after the same dwell as the
  cut tab label's (450 ms), not at once (2026-09-29, my request; consult
  GPT-6 Astra and DeepSeek first: DeepSeek had argued for "at once" since
  a square has no text). Done 2026-09-30 (`df8cf902`): shared dwell,
  reset on target change, pending dismissal tests; full check and release
  passed, installed and accepted. Consulted DeepSeek only at the user's
  request because Astra was near its usage limit.
- [x] Regression (2026-09-29): closing a tab asks whether to close the
  space, and cancelling leaves an odd highlight on the space. Probably
  the tab is the space's last one, so the close becomes a space close
  (`request_tab_close` opens the workspace confirmation when no other tab
  is left), and the cancelled confirmation leaves the space selected or
  highlighted. Reproduce, check whether it predates the vertical tabs, and
  consult (GPT-6 Astra, DeepSeek) on what closing the last tab should do.
  - Cause found and fixed 2026-10-01 (committed, not installed): Esc on a
    close confirmation always set navigate mode and highlighted the focused
    space, even when the confirmation came from closing a tab in the
    terminal. Now Esc on a tab or pane close just closes the dialog; a
    workspace close still returns to navigate mode. Mouse cancel already left
    the mode alone. Test: `last_tab_close_confirmation_can_be_cancelled`.
- [x] Disable upstream binary update notifications in fork builds
  (reported 2026-09-29, confirmed 2026-09-30). The fork is installed with
  `scripts/herdr_live.sh`; upstream `herdr update` would replace it.
  Screenshot `Screenshot 2026-09-30 at 12.58.09.png` shows "Herdr v0.9.3
  available" in notification history; clicking it produces no useful action.
  - Use an explicit fork build policy to disable upstream binary checks,
    update toasts and badges, and reject explicit upstream self-update with
    a clear explanation. Preserve upstream behavior in non-fork builds;
    a fork-owned release channel is out of scope.
  - Suppress stale upstream availability restored from pending release
    notes, and handle existing upstream update history entries without
    offering installation. Do not indiscriminately delete release notes or
    unrelated notifications, or disable agent-manifest updates.
  - Fix targetless notification-history activation separately: show the
    full title/body using existing UI patterns instead of silently closing.
    Never execute commands or install from notification text; preserve
    pane/tab/space focus for entries with live targets and the existing
    unavailable-target notice for stale targets.
  - Verify fresh and restored fork startup, explicit updater rejection,
    targetless mouse/keyboard activation, and normal agent-entry focus.
    Keep frozen generation-1 endpoint codecs unchanged.
  - Consulted DeepSeek 2026-09-30: agreed on fork-specific suppression,
    explicit updater protection, stale-state handling and informational
    activation. This remains unimplemented; no build or install performed.
  - Done 2026-10-01 (committed, not installed; `just check` passes), except
    the targetless notification activation: `HERDR_FORK_BUILD=1` in
    `.cargo/config.toml` marks builds from this checkout
    (`build_info::upstream_updates_disabled`; unit tests always behave like
    upstream). A fork build starts no background version check, ignores a
    restored upstream `update_available` (the saved release notes stay
    readable) and `herdr update` refuses with a pointer to
    `scripts/herdr_live.sh`. Agent manifest updates keep running. Untested
    live; an already open session keeps its current badge until the server
    is restarted by the install.
  - [ ] Still open: clicking a notification-history entry without a target
    (an old "update available" entry) still closes silently; show the full
    title and body with the existing UI patterns, never run commands from
    the text.
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
- [x] A dragged space that is not the active one gets a light grey
  background (2026-09-28), so the moving block stands out; today only the
  accent bar and name mark it, while the active space keeps its
  `active_row_bg`.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), both agreeing:
    not `active_row_bg` (it would look like the active space) nor
    `selection_bg` (the two grey blocks decided against above), but a new
    configurable `drag_bg` palette key with per-theme defaults; the accent
    bar, name colour and live position stay the primary cue. Only past the
    drag threshold, not on press (a press may still be a click; the dim bar
    covers it). Any dragged block, the active one too, uses the same
    background (dragged > selected > focused, one background per row, also
    over its worktree children and agent rows), so the gesture looks the
    same whichever space is grabbed. No background in 16-colour themes
    (`active_row_bg` is already DarkGray there) and with `NO_COLOR`.
  - Colour: Astra wants a neutral grey, a bit darker than "light" in the
    light theme (#e6e9ef active leaves almost no contrast) and checked
    against agent state colours; DeepSeek wants the accent blended ~10-15%
    into the sidebar background so it reads as lifted, not selected.
  - Done 2026-09-28: a `drag_bg` palette key (`theme.custom.drag_bg`),
    picked per theme (`surface1` matched the active row in nord, kanagawa
    and rose-pine), very light in light themes, none in `terminal`. Unlike
    the advice, it shows already on press, together with the grip's accent
    colour (my call after trying it), and on any grabbed space, the active
    one too.
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
- [x] Dragging tabs in the spaces list does not work (2026-09-29): pressing
  a tab line and moving starts no drag. Only the top tab bar reorders tabs
  (`tab_press` comes from `hits.tabs`, the bar), with a thin insertion
  marker, and only in the focused space. It should work like dragging a
  space: the tab line moves live in the list to where it would land, with
  the same accent bar, `drag_bg`, Esc, `release cancels · Esc` outside and
  auto-scroll.
  - Drag unit: the tab line with its unfolded job squares and its child
    tabs (not draggable themselves, their order follows the parent: no
    grip, no drop slot between a parent and its child). The agent rows are
    per space (sorted by `tab`/`prio`), so they stay put.
  - Within its own space only; the drop index counts main tabs, not rows,
    and a no-op drop sends nothing. Moving a tab to another space is a
    separate feature (ownership, worktree path, machine). Tabs of a space
    that is not focused too, so the client's `valid_drop` must stop
    requiring the focused space; pressing such a tab does not focus it
    until release without a drag.
  - Consulted (GPT-6 Astra, DeepSeek, 2026-09-29), both: the whole block,
    not the label alone; cross-space out of scope, but visible: leaving
    the source space shows `release cancels · Esc`, never clamps to its
    first or last slot; while dragging hold the geometry (agent prio sort,
    folding, closing job tabs leave blank slots as they already do under
    the pointer), state glyphs and counts may update; cancel if the tab
    vanishes; the fold triangle, squares and middle-click never start a
    drag; test a short tab dragged past a tall unfolded one both ways.
    DeepSeek also: a 3-cell threshold, since one cell eats clicks.
    Rejected: a tab line is one row high, so moving a tab one line would
    first need a detour. Decided instead: the drag starts after one row of
    vertical movement, and sideways movement alone never starts it (a
    vertical list reorders nothing sideways; a click's jitter is a column,
    rarely a whole row); the same for dragging spaces. DeepSeek also:
    auto-scroll clamped to the source space's rows, a look different from a space drag so it does not
    read as the space moving, and a target space id in the move API now.
    Astra: the top bar may keep its marker for now, but the same order,
    cancel and child-tab rules.
  - v1 done 2026-10-01 (committed, not installed; `just check` passes): a
    press on a tab line (not its triangle, counts or squares) now opens the
    tab on release; one row of vertical movement starts a drag (sideways
    alone never does). The drop slot is the one nearest the dragged line's
    top among the slots the others leave (as for spaces), within its own
    space and also in a space that is not focused; the pointer above the
    first line or below the space cancels (header `release cancels · Esc`,
    nothing clamped); Esc cancels; a drop at its own place sends nothing;
    children follow their parent (server normalises). Sends `tab.move` with
    the flat index of that space's tabs. Differences from the design above:
    the line does not move live; the lifted line takes `drag_bg` and accent
    text and a `▸` (before a line) or `▾` (after the last line) marks the
    slot in the gutter. Still open: live movement of the block, auto-scroll
    while dragging near the list's edge, the drag look for themes without
    `drag_bg`, a short tab dragged past a tall unfolded one (the lifted
    height is computed but untested), and a live check in a real terminal.
  - Reported 2026-10-01 after trying the installed build: dragging down, the
    triangle looked like "two positions" but the tab moved one. Diagnosis: the
    server is right (new test `every_drop_slot_of_a_top_level_tab_lands_where_asked`
    checks every slot with and without child tabs, and the client computes the
    marker and the drop with the same function). The marker `▸` on line k
    means "before line k", but once the dragged tab leaves, line k moves up
    one row, so a reader takes it as "ends up at k". The feedback is the bug;
    the user also wants to see which tab moves and where, and live movement.
  - Consulted DeepSeek, Claude Opus 5.5, GPT sol 6.1 and Gemini (low/high),
    2026-10-01; all five rank the same first. Proposals, not decided:
    1. Live splice preview (recommended for v2, as for spaces): the block
       (tab line, its unfolded squares, its child tabs) is taken out of the
       list and drawn at its landing slot, highlighted, with the original
       slot dimmed or empty. Hit-test against geometry frozen at drag start
       (never the reordered preview), change slots only after crossing the
       neighbour's middle (hysteresis), apply server updates without changing
       row counts. Header hint like spaces: `build: 2 → 4 · before review`,
       `no change`, `release cancels · Esc`; numbers count top-level tabs after
       the move. One pure `preview_order` function plus a small drag state;
       logic shared with the space drag.
    2. Cheap fix now: draw the marker between rows, a divider row or `──▸`
       in the gutter, not a glyph on a line, plus the same header hint and
       the source dimmed. Geometry stays frozen. Removes the off-by-one
       reading without live movement.
    3. Ghost: a one-row floating label at the pointer plus a gap at the
       landing slot (Gemini low; Opus and DeepSeek call it costly or
       redundant).
    Shared: `no change` dims the highlight and sends nothing; Esc or a drop
    outside restores the order and may flash `cancelled`.
  - Proposal 1 done 2026-10-01 and confirmed by the user on the installed
    build `45cadf68` ("działa ok") (committed; `just check` passes; the
    flaky `federated_client_starts_without_local…` failed once and passed on
    rerun): the dragged block is drawn at its landing slot with the drag
    background and accent text, the `▸`/`▾` markers are gone, the header says
    `2 → 4 · build · before review`, `no change · build · Esc` or
    `release cancels · Esc`. The drop is measured against the rows frozen at
    the drag start (`TabLineGeometry`), so the slot depends on the pointer
    alone and cannot flicker. Not done: the same header hint in the
    multi-machine sidebar, auto-scroll at the list's edge, scrolling the list
    with the wheel during a drag (the frozen rows would be stale), a live check.
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
- [x] Classify the native-graphics CoW retention benchmark failure.
  - Verified 2026-10-01: this machine is macOS (`uname -s`: Darwin).
    `src/platform/mod.rs::clone_native_image_source` deliberately returns
    Unsupported outside Linux. Both that contract and the benchmark's
    `source_file` assertion exist at `a4ec9556^`, before the job-footer fix.
    The ignored source-retention profile therefore requires a capability
    this platform does not implement. The master rerun alone did not prove
    this; the historical code comparison establishes the platform mismatch.
    No runtime benchmark was run on the parent revision. Other graphics
    regressions are not ruled out by this finding.
  - [x] Make the manual benchmark distinguish unsupported platform source
    retention from a regression. Test-only implementation capability lives
    in `src/platform/mod.rs`; scenario selection has deterministic tests.
    Non-Linux reports `status=unsupported` explicitly for 1 and 15 panes,
    without measuring decoded fallback as source retention. The other modes
    remain, including native export at both pane counts. DS reviewed the
    design. Verified 2026-10-01: `just check` passes 3809 Rust tests;
    `just bench-render-scale` passes all 8 profiles with two explicitly
    unsupported source-retention scenarios. No runtime or protocol change.
    Linux retains the real source assertion, so a filesystem without
    reflinks still fails visibly. Linux/reflink live validation remains
    unperformed; macOS CoW support is outside this task.
- [x] Reproduce the general render scaling comparison for the job-footer fix.
  - Measured 2026-10-01: exact baseline `f89ac503` (`a4ec9556^`) and candidate
    `a4ec9556`, three `just bench-render-scale` runs each, macOS, fixed
    120x40, 5 warmups / 40 samples for the pipeline profile. No concurrent
    project validation during the retained comparison. Seven profiles pass
    in each run; native-file source retention fails on both due to unsupported
    macOS CoW. The recipe itself exits 101; it is not an all-green benchmark.
  - Median of the three run medians, combined pipeline in microseconds:
    background 1 pane 564 -> 561 (-0.5%), 15 panes 603 -> 585 (-3.0%);
    active 1 pane 568 -> 555 (-2.3%), 15 panes 666 -> 658 (-1.2%).
    Within-revision 15/1 growth is background 6.9% -> 4.3%, active
    17.3% -> 18.6%. Earlier +7%/+17% described cardinality growth, not the
    overhead of the footer commit. These small sequential samples establish
    neither a speedup nor regression-free behavior; no general slowdown was
    demonstrated. DS reviewed this interpretation and agreed with those limits.
    Logs remain in `.local/prd/footer-perf-comparison/`.
  - [ ] Add an explicit occupied-job-footer profile: the general benchmark
    does not configure job metadata and does not isolate footer drawing.
    Use 1/15 populated panes with and without metadata and interleaved
    baseline/candidate samples before attributing any cost to the footer.
    Optimise only if repeatable measurements justify it.
- [x] Push the fork's pending commits after explicit user approval. (approved and pushed 2026-10-01, see the last bullet)
  - Fetch and refresh the ahead/behind comparison first; the earlier count
    of 52 unpushed commits is stale. Review the outgoing changes, follow
    the rebase-only fork sync rules in AGENTS.md, and never merge upstream
    into master. Do not push or rewrite remote history without approval.
  - Done 2026-10-01 ("tak na wszystko"): master was 32 commits ahead of
    `origin/master` and `origin/master` was its ancestor, so a plain
    fast-forward `git push origin master` was enough (no force). Upstream is
    47 commits ahead of the fork (312 fork commits on top): not synced; the
    rebase onto `upstream/master` is its own step with its own check. The
    user also approved a standing rule for that rebase and force-push, now in
    AGENTS.md (relayed by another session).
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
- [x] Notifications button above "spaces": clicking it opens a dropdown of
  past notifications with the time each arrived; clicking an entry
  navigates like clicking the toast. Today there is no history: a toast
  (5-12 s, queue of 8, same-pane replacement) is gone once it expires.
  - The button shows how many notifications I have not clicked whose tab I
    have not visited since; visiting the tab (or clicking the entry) clears
    them.
  - While the herdr window is focused, do not send the system (OS) toast;
    show herdr's own toast instead. Today `System` delivery is suppressed
    only when the target is the active tab and the window is focused
    (`suppress_external` in `tick_notifications`, from `outer_focused`,
    which is `None` when the terminal does not report focus).
    Done 2026-09-29: with `System` delivery and the window focused
    (`outer_focused == Some(true)`), herdr's own toast shows instead, for
    a target that is not the active tab; unfocused or unknown focus keeps
    the system toast.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), both: worth it as
    a plain event log, not a notification centre. History is a shared
    runtime fact: a server-side ring buffer per endpoint (about 100
    entries, in memory, lost on server restart) recorded when the server
    emits a notification, so events while detached and toasts suppressed
    for the active tab or dropped as stale are kept. A new advertised
    `notification.list` method returning a stable id and server timestamp
    per entry (the frozen `SemanticNotification` has neither); refetch on
    each live notification; hide the button when the method is missing.
    Do not reuse the toast's same-pane replacement; collapse only identical
    repeats (with a count) and rate-limit script floods later.
  - Both: absolute `HH:MM`, with the date for older days. Label entries as
    past events ("asked for attention"), not current state: the agents list
    owns the current state. Click reuses the toast path with the system
    toast's pane -> tab -> workspace fallback; if nothing survives, keep the
    entry and say "target no longer exists"; an offline machine reports "X
    is unavailable"; never pick another pane that now sits in the same
    place.
  - Both advised skipping an unread count in v1 because "opened the
    dropdown" does not mean "read". The unvisited-tab rule above answers
    that; keep it client-side (per client, last-seen id), so one client
    does not clear another's count.
  - Pitfalls: keyboard access (a key to open, arrows/j/k, Enter, Esc); a
    narrow sidebar clips a dropdown, so maybe an overlay; freeze the list
    while the pointer is over it so new entries do not move the click
    target; script bodies stay in history longer than in a toast.
  - Done 2026-09-29: the server keeps the last 100 notifications sent to
    client shells (sent or not, so detached ones count; `notification.show`
    without a shell client is still refused and not kept) and lists them
    with an advertised `notification.list` (id, `unix_ms`, kind, title,
    body, agent, workspace, tab, pane). The client fetches it only when the
    dropdown opens: a background fetch holds the machine's command lane,
    so a click in that moment was refused as busy (it broke the federated
    client test). `✉N` at the right end of the spaces header (hidden when
    the server lacks the method) counts the notifications this client
    received for tabs it has not shown since; the
    dropdown lists up to 15, newest first, `HH:MM` (with the date for older
    days), unread marked `•`; click or Enter opens its pane, else its tab,
    else its space, else says "target no longer exists"; j/k/arrows, Esc.
    Still open: a key to open it, the multi-machine sidebar's header, the
    list's own freeze while the pointer is over it, collapsing repeats.
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
- [x] Reopen the last closed tab, `prefix+u` ("undo close", configurable). (v1 done, see the last bullet)
  - Closing a tab kills its processes, so this recreates the tab rather
    than undoing the close: same place in the space, name, pane layout,
    working directories, and agents resumed through the existing session
    resume (`src/agent_resume.rs`, as after a restart); plain shells start
    fresh, never replaying their commands.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): a bounded,
    server-owned history of explicitly closed tabs per space, exposed
    through a new advertised method, not in routine snapshots; pane closes
    are left out at first; an entry stays in the history if restoring it
    fails. Define missing directories, a deleted space, partial failures
    and two clients reopening at once. Not `ctrl+shift+t`: terminals often
    take it, and legacy key encoding cannot tell it from `ctrl+t`;
    `prefix+shift+t` is rename.
  - Alternative to weigh (mine, not consulted): keep a closed tab's
    processes alive for a few seconds with an "undo" toast, which restores
    them exactly.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01 on
    a client-local v1: record only a close the server accepted, top-level
    tabs without jobs, a custom label only, at most 10, not persisted; skip
    entries whose space is gone; use an entry up when sent; document that
    other clients' closes and process exits are not covered; server-owned
    history later. Done and committed (not installed): `keys.reopen_tab`
    (default `prefix+u`) opens a new tab in the same space with the focused
    pane's directory and the tab's own name, then moves it after the tab
    that stood before it when that one is still there. A refused create
    keeps the entry. New file `src/client/shell/closed_tabs.rs`; no protocol
    change. Not done: a server-owned history, closes by other clients or
    exits, restoring splits or agents, a live check.
- [x] Which tab gets focus after closing the active one (done 2026-10-01; the user said to decide, so see the last bullet): should it be the
  next one (right) instead of the previous one (left), or should that be
  configurable? Today `Workspace::close_tab` focuses the previous tab (the
  new last one when the last tab closes).
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-29), who disagree.
    Astra: default to "next, else previous" (Chrome, Firefox), but a
    closed child tab should return to its parent. DeepSeek: keep
    "previous", because parent/child ordering makes it structurally
    right: closing a parent's first child lands on the parent, and "next"
    after closing a parent would land on its (now orphaned) child.
    Chrome and Firefox go right because their tabs are flat. Both: tmux
    returns to the previously used window, VS Code to the recently used
    editor (Astra, not verified).
  - Consulted again 2026-10-01 (DeepSeek, Opus 5.5, GPT sol 6.1, Gemini high):
    all four say closing the active child tab goes to its parent, always.
    Done and committed (not installed): `Workspace::tab_number_to_focus_
    after_close` returns the parent for any child, so closing a middle or
    last job no longer lands on a sibling job. Top-level direction: DeepSeek
    and Gemini want "next top-level, else previous" (Chrome); Opus and GPT
    want to keep "previous, else next" because a closed tab's own children
    follow it in the list. Split 2:2, so unchanged; it is the user's call.
    All four: closing or exiting an inactive tab never moves focus (true
    today); closing a parent closes its children first and the focus goes to
    a surviving top-level tab.
  - Decided by me 2026-10-01 ("rób jak uważasz", the user may change it):
    `ui.focus_after_tab_close = "next" | "previous"`, default `next`
    (committed, not installed): the user asked in this item whether the next
    tab (right) should win, the consulted models split 2:2, and both sides
    allowed this one option. `next` takes the next top-level tab (skipping
    the closed tab's own jobs), else the previous; `previous` the reverse. A
    closed child always returns to its parent. The choice is process-wide
    (`workspace::set_focus_next_after_close`, set at server start and on
    config reload); unit tests keep `previous`. A `last_used` mode waits for
    a tab history.
    `cross_area_detach_and_reattach_preserves_state` failed once in a full
    `just check` ("workspace with matching label should exist") and passed
    twice alone and on rerun: another flaky integration test.
  - Both: no speculative option matrix; if added,
    `focus_after_tab_close = "previous" | "next"`, and `"last_used"` only
    once there is an MRU history of tabs. Child to parent should be
    unconditional, not a config value. Today it only holds for the first
    child: closing a later child lands on its previous sibling.
  - Edge cases: closing a non-active tab keeps the same tab focused (it
    does today); a tab closing because its process exited, or a
    background job tab, must never move focus unless it was the active
    tab; another client's space only gets its stored active tab fixed.
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
- [x] An agent that finished and waits for me shows a blue dot (`Done`,
  unseen), but it turns green (`Idle`) as soon as I open its tab, before I
  answer: `mark_active_tab_seen` sets `pane.seen` when the tab becomes
  active. Should it stay blue until I reply?
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): "unseen" and
    "awaiting my reply" are two different facts; keeping blue until typing
    would silently change what blue means, and a finished agent does not
    always need an answer. Keep `seen` for toasts and sounds; if a reply
    marker is wanted, make it a separate per-pane flag (server state, since
    it is a runtime fact), cleared by a submitted message (Enter on
    non-empty input), not by any byte reaching the PTY (arrows, `ctrl+c`,
    scrolling), and dismissable by hand. No "focused for N seconds" timer.
    Decide whether it survives a restart. Astra: clear "unseen" on the pane
    being visible, not merely its tab being active.
  - Tried and rejected (2026-09-28): keeping every `Done` until Enter in the
    pane. An agent that only waits for the next task must not look like it
    needs a decision. Reverted, never committed; the diff is not kept.
  - Refined goal (user): mark only a turn that ended by asking me something
    (a question in plain text); a plain finish still clears on view. Menus
    and permission prompts are already `Blocked` (red) until answered.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28, two rounds):
    - An orthogonal, optional server-side flag ("awaiting reply"), not a
      new state: viewing clears `Done` but not the flag. Priority: blocked >
      working > awaiting reply > done > idle. Its own glyph (e.g. `?`).
    - A trailing `?` in the final message is only a hint: courtesy
      questions ("Anything else?") are false positives, "Please confirm
      before I proceed" or "Choose A or B." are misses.
    - Better (user's idea): the agent reports it itself. Herdr injects an
      instruction (Claude: `additionalContext` from the SessionStart hook
      herdr already installs), opt-in, visible, never written into repo
      instruction files. Report through a strict marker at the end of the
      final message, parsed by the Stop hook from `last_assistant_message`
      (verified in Claude Code docs), rather than a `herdr` CLI call: a
      Bash call may hit a permission prompt and turn the pane red while
      reporting (DeepSeek: then ship an allowlist entry).
    - Scope reports to the session and turn so a late report cannot
      revive a cleared flag; ignore subagent `Stop`. Clear on the next
      user prompt (Claude `UserPromptSubmit` hook), the next turn start,
      a turn ending without a report, session reset; never on view. Do not
      fall back silently to the `?` heuristic; other agents only through
      tested adapters. Store no question text at first.
    - Rule change needed: agent detection is screen-evidence based; this
      adds structured, turn-scoped hook events as a source.
  - Decided (user, 2026-09-28): the explicit `herdr agent awaiting-reply`
    command (cleanest engineering-wise), with the permission rule added
    by the consented Claude integration install. Done: server flag
    `awaiting_reply` (see the 2026-09-28 rework below for when it clears),
    `pane.report_awaiting_reply`, the TUI keeps the agent `Done`
    while it is set, Claude integration v11 injects the instruction
    (`HERDR_AWAITING_REPLY_INSTRUCTIONS=0` leaves it out). Tried live with
    Claude Haiku 4.5 (Claude Code 2.1.283): it ran the command without a
    prompt, but wrote the question twice, before and after the command.
  - Done (2026-09-28): its own glyph, `?` in the finished colour in both
    indicator styles, winning over the waiting-on-job mark; the state text
    reads "awaiting reply".
  - Done (2026-09-28, consulted GPT-6 Astra and DeepSeek): a wrong `?`
    appeared because Claude reported and then asked with `AskUserQuestion`,
    which the user answers inside the same turn; the report surfaced when
    the turn ended ten minutes later. Now the report holds until someone
    types into the pane (client keys, text, paste; `pane.send_*`,
    `agent.prompt`, `agent.send_keys`; not clicks, scrolling or focus),
    entering Blocked drops it, and exit or a session change clears it.
    `awaiting_reply` is derived as report && idle, so working hides it
    without using it up and a mid-turn idle flicker no longer loses it. The
    hook asks for the report only as the last command before a plain-text
    question, never for `AskUserQuestion`. No time windows: the user
    rejected them as race-prone.
- [x] Claude sometimes forgets `herdr agent awaiting-reply` (2026-09-28):
  Opus ended a long turn (build, install) with "commit after you check;
  let me know how it looks" and did not report, so no `?` appeared. The
  instruction came only from `SessionStart`, far back in the context, and
  the request had no question mark.
  - Consulted models (GPT-6 Astra, DeepSeek): first re-inject a short
    reminder every prompt (`UserPromptSubmit`), and define the case
    operationally: the agent needs the user's answer or decision to
    continue the work. A Stop hook that blocks the stop when the last
    message looks like a question would not have caught this miss (no
    `?`), and a phrase list broad enough to catch it also fires on
    courtesy offers, costing a whole extra turn.
  - Done: the `SessionStart` instruction uses that definition with this
    case as an example; integration v11 adds a `UserPromptSubmit` hook
    (`herdr-agent-state.sh reminder`) that prints a short reminder, managed
    apart from the canonical `SessionStart` hook like the permission rule.
  - Next, only if misses continue: a narrow Stop-hook backstop (terminal
    `?` or an imperative aimed at the user, and no mark set; ask herdr for
    the mark rather than parsing the transcript), one block at most.
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
- [x] Consult stats log DeepSeek under the alias it was called with
  (`deepseek-flash`, now V4.1), so when the alias moves to a new model the
  stats of both merge and we cannot tell which was which.
  - The streamed chunks carry `model` and `system_fingerprint`, which
    `ask_deepseek.py` ignores.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): log the requested
    alias, the reported model and the fingerprint (additive fields, older
    entries keep working), and let stats group by either; first check what
    the API really returns, since the reported name may itself be an alias.
    Do not backfill old entries as V4.1 by date unless DeepSeek's changelog
    gives the exact cutover; otherwise mark them `unknown`. Check GPT
    (Codex) and Gemini separately: what metadata they expose differs.
  - Done (2026-09-28): the chunks' `model` only echoes the alias; `/models`
    names the serving model (`DeepSeek-V4.1-Flash`). `ask_deepseek.py` logs
    it as `model_version`, with the fingerprint; stats group by it (older
    calls stay under the alias, version unknown), `stats --by-alias` merges.
    GPT and Gemini are called with explicit model ids.
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
- [x] Analyse whether all tests are needed.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): optimise for
    confidence and upkeep, not the test count. First find the slow, flaky
    and often-rewritten tests and those the rules forbid (freezing CLI
    agents' screen detection rules). Tests covering the same lines are not
    automatically duplicates: compare what they assert, and check suspects
    with a targeted mutation. Remove a test of implementation details only
    when a higher-level test is shown to cover it, in small batches, never
    one bulk prune. Some UI strings are contracts; keep frozen protocol
    fixtures.
  - Analysis (2026-09-28): 4070 tests in 283 files; 3741 run on macOS in
    about 29 s, all pass, no flaky ones seen. Nearly all are needed.
    - Exact duplicates to delete: `read_message_accepts_exact_payload`
      (`src/protocol/wire.rs`, same as `framing_small_message_roundtrip`)
      and `lone_escape_is_buffered_until_timeout_flush`
      (`src/raw_input.rs`, same as `flushes_lone_escape_after_timeout`).
    - Breaks the detection rule: `agent_explain_evaluates_with_server_manifest_cache`
      (`src/app/api.rs`) asserts Codex's bundled rule id
      `live_strong_blocker`; rewrite it with a synthetic override manifest.
    - Speed: `client_mode::federated_client_starts_without_local_and_survives_its_restart`
      alone takes 16.6 s and sets the wall-clock time; the
      `detect::manifest*` tests take about 1 s each because they reload
      all bundled manifests 2-4 times per test.
    - Merge, not delete: the 13 `install_*_errors_when_config_dir_missing`
      tests (`src/integration/tests.rs`) into one table-driven test; the
      same macOS and Linux `scrollback_editor_argv_*` test into one unix
      test.
  - Status 2026-09-29: the duplicates and the codex rule pin went on
    2026-09-28 (746cb3f6); the federated test is bounded (15 s waits).
    Not doing the merges or the manifest reload speed-up: those are
    upstream's tests and code, so rewriting them here only makes every
    rebase onto upstream conflict; the platform `scrollback_editor_argv`
    tests stay in their platform files by the repository's rule. Suggest
    them upstream instead.
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
- [x] Close herdr panes with Cmd+W, as Ghostty closes splits: the last
  pane closes its tab, the last tab its space.
  - Conflict: Ghostty binds Cmd+W to `close_surface` and handles its own
    keybinds before the program sees the key, and has no per-foreground
    program binding. `unconsumed:` still runs `close_surface` (it only also
    forwards the key) and `performable:` is always true for it, so neither
    routes Cmd+W to herdr (consulted GPT-6 Astra and DeepSeek, 2026-09-28).
  - Set up 2026-09-28: dotfiles Ghostty config has `cmd+w=unbind` (like
    Cmd+1..9); herdr config has `[keys] close_pane = "cmd+w"`. The
    `cmd+ctrl+w=close_surface` fallback was dropped on 2026-09-28 (the
    user's choice), so Ghostty splits have no close key; Cmd+Opt+W still
    closes a Ghostty tab. Don't use `cmd+w=csi:...`: plain shells would
    get the escape sequence as input.
  - Verify after reloading Ghostty: Cmd+W reaches herdr as `super+w` and
    closes the focused pane; `confirm_close_running` asks before closing a
    pane or tab with a working agent or job, with Cancel as the default.
    Reopening can't bring back killed processes, so the confirmation
    matters more than undo.
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

- [x] Raise the default sidebar width: it now carries spaces with branch
  and git status, agents with their task, job lines and tab lines, and
  truncates a lot at 26 columns. Plan: `sidebar_width` 26 → 32,
  `sidebar_max_width` 36 → 44 (so dragging can go wider), min stays 18.
  - Consulted GPT-6 Astra and DeepSeek, 2026-09-29: both chose a fixed 32
    (DeepSeek: max 40, min 20). Not a percentage of the terminal width
    (a resize would silently reflow the agent panes) and no auto-sizing
    from content (every rename, checkout or status change would move the
    dividers and rewrap agent output; at most a one-shot "fit to content"
    action that stores a dragged width). Budget: two 80-column agent panes
    next to a 32-column sidebar need at least 192 columns.
  - Only clients without a dragged width get the new default: the
    preferences file stores the width only after a drag
    (`sidebar_width_manual`), so a dragged 26 stays 26.
  - Narrow terminals: the mobile layout starts at 64 columns
    (`DEFAULT_MOBILE_WIDTH_THRESHOLD`), so a 70-column terminal would keep
    only 38 for panes. Consider capping the default at a share of the
    terminal (e.g. a third) below ~120 columns, without touching a
    dragged width.
  - Fix the stale `src/main.rs` config comment saying the width is
    "auto-scaled based on workspace names"; nothing scales it. Update the
    defaults in `src/config/model.rs`, the doc comments and the sample
    config together.
  - Done 2026-10-01 (committed, not installed): `sidebar_width` 32,
    `sidebar_max_width` 44, min 18; the sample config, the config reference
    and the stale "auto-scaled" comment are updated. Six layout tests that
    pinned columns for the old default now set 26 explicitly
    (`config_with_sidebar_width`). Not done: the cap at a share of the
    terminal below about 120 columns (still "consider"); a dragged width
    is unchanged.

## Deferred

- [ ] Consult stats: pair the coordinator with Opus at a lower effort
  (`claude -p --model <same id> --effort low`, fresh context without project
  instructions or tools) to measure what effort buys.
  - Method (consulted 2026-09-26): same prompt against a fresh-context call
    at the coordinator's effort; the self entry is only a baseline (full
    context, rates itself). Pilot 10 rounds, conclude after 20-30, rate blind
    where practical, "unique" only relative to that round's roster. It uses
    the same subscription, so log failures, never drop them.
- [ ] Usage widget: include minutes in reset countdowns (e.g. `2h 15m`,
  `45m`, `<1m`), not just whole hours. For weekly limits above 24h, show
  days plus remaining hours (`34h` → `1d 10h`), not just whole days.
  - Consulted DeepSeek 2026-10-01: share the footer/modal countdown policy:
    >=24h days + hours, >=1h hours + minutes, >=1m minutes, positive <1m
    `<1m`, expired `now`. Floor units and omit zero secondary units;
    `24h 30m` therefore shows `1d`. Check footer width and boundary tests
    (23h 59m, 24h, 34h, 48h). Presentation only; no provider/API changes.
  - Countdown format done 2026-10-01 (committed, not installed): the
    footer shows `1d10h`, `2h15m`, `45m`, `<1m`, `now` without inner spaces so
    a cell keeps five columns (cells start at columns 4 and 14); the modal
    shows `1d 10h`, `2h 15m`. Boundary tests cover 59s, 24h, 24h30m, 34h,
    48h and 23h59m. The two bullets below (reset entitlement research,
    redeemable resets, credits) are still open.
  - [ ] Investigate whether Anthropic offers a reset entitlement comparable
    to the user's ChatGPT Plus `Full reset (Weekly + 5 hr)` observation, and
    whether it could explain successful Sonnet calls at weekly 100%.
    This is a hypothesis, not an established Anthropic feature or cause.
    Distinguish scheduled renewal, a manually redeemed reset, model-specific
    allowance, delayed/aggregate telemetry and paid usage credits.
    Record the exact model, plan/auth mode, timestamps, displayed buckets,
    rejection/reset text and any actual redemption or billing evidence.
    Use official Claude/Claude Code subscription docs and account usage /
    billing UI, not API Console limits as proof of subscription semantics.
    Do not redeem anything, enable paid overage or expose credentials.
    Consulted DeepSeek and Gemini (low/medium/high) on 2026-10-01: successful
    calls alone cannot identify the mechanism; none verified an Anthropic
    reset grant. Gemini's API headers/Console suggestions are not evidence
    for Claude Code subscription quotas. Claude consultation was deferred
    after the actual weekly-limit rejection until 02:00 Europe/Warsaw.
  Also show how many redeemable quota resets are available, their types /
  scope, and when each expires; keep these separate from automatic limit
  renewals. Show a compact count in the footer and details in the usage modal.
  - User example: ChatGPT Plus shows `Full reset (Weekly + 5 hr)` and
    `Expires October 5` at https://chatgpt.com/settings/usage?tab=overview.
    This is a user observation, not a verified entitlement for every Plus
    account; do not invent the expiration year, time or timezone.
  - Consulted DeepSeek: unknown or unavailable reset data is not zero;
    count only available grants, not used or expired ones. Keep the footer
    and modal consistent, indicate stale data, and show an exact expiration
    with timezone when the source provides it.
  - First verify an authenticated, supported source for grant data; retrieval
    remains blocked until then. Do not invent endpoints or scrape browser
    credentials. Read-only display: redeeming a reset is out of scope.
  - Also distinguish subscription allowance from paid overage / usage
    credits. User example from Anthropic: `Turn on usage credits to keep
    using Claude if you hit a plan limit.` Settings page:
    https://claude.ai/new#settings/usage. Clearly indicate in the footer
    and usage modal when current usage is billed to credits rather than
    included in the subscription, with balance / spend when available.
    Distinguish credits disabled, enabled as a fallback, and actually in
    use; enabling credits alone does not prove paid usage. Require verified
    data for the same account; otherwise show unknown, never infer billing
    solely from an exhausted plan limit. Keep subscription overage separate
    from Anthropic Console API billing. Display only; do not enable credits
    or change billing settings.
- [ ] Usage modal (click the footer) / settings: checkboxes choosing which
  providers the usage footer shows. Also token-based usage?
  - Consulted models (DeepSeek, GPT-6 Astra, GPT-6 Luna, 2026-09-26): the
    checkboxes, yes. Tokens answer a different question ("what did this
    cost?") than the footer ("can I keep going?"): if ever, a separate usage
    details view, not the footer. Transcript scraping is brittle (resumed
    sessions, retries and cache tokens double-count), so only with a concrete
    need; split input, output and cache, and label estimates.
  - Deferred (consulted 2026-09-26): premature with few providers; built-in
    settings widgets are enough, no plugin settings framework needed.
  - Revised 2026-09-28: put the checkboxes in the usage modal that opens when
    I click the footer, not in settings. Also show usage of my other
    workspaces: I have extra workspaces in OpenAI (platform projects) and
    Anthropic (Console workspaces). Maybe per API key too.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): three different
    things, keep them apart: footer visibility, subscription allowance, API
    spend.
    - Checkboxes mean "show in footer", not "poll": hiding a row must not
      stop polling (`[usage].<provider>` stays the poll switch). It is TUI
      presentation state: persist it on the client side, never in
      `usage.read`. The modal keeps listing hidden rows so they can be turned
      back on, and tells apart hidden, polling off, no credentials and
      refresh failed.
    - Workspaces only through the admin APIs (`sk-ant-admin…`, `sk-admin…`),
      and those give spend and tokens, never a remaining balance or budget:
      Anthropic `GET /v1/organizations/cost_report` (group by `workspace_id`)
      and `usage_report/messages` (by `workspace_id`, `api_key_id`, `model`);
      OpenAI `GET /v1/organization/costs` (by `project_id`, `line_item`) and
      `usage/completions` (by `project_id`, `api_key_id`, `model`). Verify
      against the docs before building; neither model could fetch them.
    - Per API key: tokens only; cost per key would be an estimate from
      prices (cache, batch, price changes). Cut for now.
    - Workspace spend goes in a separate "API spend" section of the modal
      (month to date, currency, scope, when observed), never as footer rows:
      the footer stays a compact allowance strip. Label it "Anthropic API
      spend", distinct from the Claude subscription row. Cost reports lag by
      hours: own slow refresh, not the allowance poller.
    - Admin keys read org-wide billing: opt-in, own env var or auth file
      entry, server side only, never in API responses, logs or the shared
      `usage-cache.json`.
    - Order: footer checkboxes in the modal; then API spend per workspace
      (ties in with the OpenAI API row above); per-key usage only on real
      need.
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
- [ ] herdr > menu > settings: the consult skills (gpt, gemini, deepseek,
  consult-stats) get their own settings section, like Integrations (agent
  hooks) but a separate item: per agent (Claude Code, pi) whether the skills
  are installed, with an install button. Today only the "Consult: install
  skills" popup runs `plugins/consult/install-skills`.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both advise against
    a core section for now (one plugin, four coupled skills); the missing
    value is status, not placement. Start in the plugin: the popup shows the
    state per agent and installs or repairs.
  - If it goes into core, make it generic, not consult-specific: plugins
    declare skills in `herdr-plugin.toml`, the server advertises new methods
    (`skill.list`, `skill.install`), and agents are data-driven strings;
    never new `IntegrationTarget` variants (frozen enum). Installing is not
    enabling: per-model checkboxes and stats stay on the consults page above.
  - One row per bundle × agent (the four skills go together: the scripts find
    `consult-stats` as a sibling), expandable to skills. States: linked,
    missing, broken link, conflict (a real directory at the destination),
    mixed; warnings: possibly shadowed by `~/.agents/skills`, plugin
    installed as a copy (links into it break on update). Install/repair
    never overwrites foreign files; uninstall (later) removes only links
    that point into the plugin. Agents without a skills directory are
    "unsupported", not "missing".
  - The server writes into its own host's home: with remote endpoints show
    which host is affected. Windows symlinks need their own handling.
  - Smallest stage: read-only status plus bundle install/repair in the
    popup; a settings entry, if any, only opens that popup.
- [ ] A "consult models" checkbox in herdr's bottom bar (on/off), or instead
  checkboxes next to the models to consult (GPT Astra, DeepSeek, Gemini), so
  I choose in the UI whether and whom agents consult, instead of the rule in
  the agent's memory ("before design decisions consult GPT Astra +
  DeepSeek").
  - Open: how the state reaches a running agent (a state file the consult
    skills read, plus a hook such as Claude's `UserPromptSubmit` injecting
    "consult: on, models: astra, deepseek" so the agent knows before it
    decides); scope (global, per workspace or per agent pane); core footer
    or the consult plugin (plugins cannot draw widgets today). Overlaps the
    per-model "enabled" column on the deferred settings > consults page.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both agree. A
    checkbox promises more control than herdr has: it cannot force a
    running agent to consult, and "off" must beat the rule in the agent's
    memory. Per workspace (consult policy follows the project; global leaks
    into unrelated work, per pane gets lost when panes restart). One toggle
    first, the model list in the same state file; per-model checkboxes mix
    "whether" with "whom" (fallbacks, missing keys). In the consult plugin
    (menu action plus popup), not the core footer, which would give one
    plugin privileged UI. Delivery: a workspace state file as the source
    of truth, a prompt hook injecting `[herdr consult policy] enabled=…
    models=…` every turn, and the consult scripts re-reading it before
    sending, so "off" is enforced, not advisory. pi has no such hook: say
    so. Astra: label it "Auto-consult" and decide whether my explicit
    "consult X" bypasses off; show which providers get the code.
  - Smallest stage: that state file, a plugin menu toggle, the Claude hook
    and the dispatch-time check, logging policy against actual consults.
    Kill it if agents ignore it; a footer checkbox only if I flip it often.
  - Inject every turn, only for questions, or only on change? Consulted
    models (GPT-6 Astra, DeepSeek, 2026-09-28), both: not only for
    questions (the hook sees my prompt, not the agent's decision point;
    "implement X" hits design choices mid-turn, a classifier adds latency
    and misses). Not every turn either (repetition primes over-consulting).
    Inject at session start (startup, resume, clear, compact) and on
    `UserPromptSubmit` only when the policy's generation counter differs
    from the one last injected into that session (per session, not per
    workspace; a `PreCompact` dirty flag forces reinjection). Inject even
    when the policy matches the default, and replace the memory rule with
    "follow the herdr consult policy", so there is one source of truth.
    The scripts re-check the state file right before sending; a missing or
    broken file means "auto off" with a clear reason, not a silent "on".
    Subagents may never see the line: the script prints the policy on its
    first call. Explicit "consult X" bypasses auto-off and the model list
    (a separate hard "no external consult" switch, if ever needed, would
    not be bypassed); scripts take an `--explicit` flag, logged. Astra: one
    shared dispatch layer for all consult scripts instead of a brittle Bash
    `PreToolUse` matcher.
  - Idea: when a checked model's limit is exhausted, grey its checkbox out
    and leave it out of the injected policy, so the agent does not try it.
    herdr already has the signals in the usage footer (`usage.read`, cached
    in `usage-cache.json`): Codex rate-limit windows, DeepSeek balance
    (`is_available`), Gemini weekly quotas from `agy -p /quota`.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28): both: an
    availability hint, not a hard gate yet. A false "exhausted" silently
    drops a working model for hours, which is worse than one failed call;
    unknown means available. Keep the checkbox as my intent and show
    availability as a separate badge with the reason, when it was observed
    and the reset hint; a checked-but-grey box reads as "on but not on".
    Never gate GPT on `usage.read`: the footer reads Codex's own ChatGPT
    login, the gpt skill uses pi's `openai-codex` token, possibly another
    account. Better signal: the consult scripts' own classified failures
    (provider, model, credential hash, hard quota vs rate limit vs auth vs
    outage, timestamp) logged by consult-stats. DeepSeek `is_available =
    false` is trustworthy for a hard zero; Gemini's quota only when `agy`
    uses the same account and the group covers the model. Reset times are
    hints ("try again tomorrow" cleared in two hours): at reset go back to
    "unknown", allow one try, re-block with a bounded TTL; a "retry now"
    action.
  - Stages: classify and log failures in consult-stats; show the badge; the
    scripts fail fast only on a same-credential hard quota failure within
    the TTL (unless I ask explicitly); drop models from the hook only if the
    data shows agents wasting turns on exhausted ones.
  - Sort the model checkboxes by the consult-stats ranking (`consult.py
    stats`: accepted unique findings per rated call, e.g. DeepSeek 1.57,
    Astra 1.56, Gemini 0.57 on 2026-09-28), with the number next to each
    model; models under 5 rated calls go last, as in `stats`. The ranking
    depends on which models were asked together, so it is a hint, not a
    verdict. Do not reorder while the pointer is over the list (as in the
    agents' `prio` sort).
- [ ] Shared checkout awareness: agents in one checkout do not know about
  each other. On 2026-09-28 another session started editing `src/` minutes
  after this one checked `git status`; only commits by explicit path kept
  the two fixes apart. The policy ("ask whether to use a worktree when the
  checkout has code changes that are not yours") stays in `AGENTS.md`;
  herdr would add the facts only it knows and, when the checkout is shared
  or has code changes, tell the agent to ask me whether to create a
  worktree before it edits code (never create one on its own).
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-28), both: herdr gives
    facts, the repo gives the rule (what counts as code vs notes, whether
    to ask, warn or require a worktree). Never claim whose changes they
    are: after resume, `/clear` or compaction an agent can take its own
    edits for foreign ones, so say "ownership unknown".
  - Timing: a line at session start only when the checkout is already
    shared or has code changes (it goes stale in minutes); the real check
    before the session's first file edit (`PreToolUse` on `Edit`/`Write`,
    fresh state); not every prompt (noise). DeepSeek: optionally warn at
    commit when it includes files this session did not edit.
  - Limits: advisory, not a lock. Edit hooks miss shell edits (`sed`,
    scripts); hookless agents only get the CLI. Separate from the
    auto-consult injection above, which refreshes on a policy generation,
    not on checkout state. A worktree per agent is the real isolation but
    costs a cold `target/`.
  - Smallest stage, in the fork (plugin, opt-in), not upstream by default:
    `herdr checkout status` (other agent panes in the same git root and
    worktree, uncommitted code files, when observed), then the first-edit
    hook, e.g. "2 other agents share this checkout (panes 3, 7); 4
    uncommitted code files, ownership unknown. Ask the user whether to
    create a worktree before editing code."
  - Idea: a herdr setting for the worktree policy. Consulted models (GPT-6
    Astra, DeepSeek, 2026-09-28), both:
    - Per repo, keyed by the common git dir so linked worktrees share it;
      a global value only as the default; not per workspace (a UI
      grouping, not a checkout). Modes: `shared` (default), `ask`,
      `always` (a new worktree per new agent session). Not "never ask": it
      names the prompt, not where the agent works. DeepSeek: if the
      setting contradicts `AGENTS.md`, herdr says it overrides the repo
      rule, never silently.
    - `ask` triggers on another live agent in the same checkout OR
      uncommitted changes (tracked, staged or untracked; do not classify
      code vs notes). The 2026-09-28 race began from a clean `git status`,
      so "has changes" alone misses it.
    - herdr asks in its TUI and creates the worktree before the agent
      starts (pane cwd = worktree); an agent that moves itself later
      leaves its session and relative paths in the old checkout. The hook
      text then carries only facts. Serialize herdr's occupancy check so
      two launches cannot race.
    - `always` costs a cold `target/` per worktree: a shared
      `CARGO_TARGET_DIR` contends on cargo's lock and rebuilds on
      differing flags, sccache skips linking. Every fix must land on
      current `master` before `scripts/herdr_live.sh install`; never merge
      or install automatically.
  - Missed by both: herdr usually does not launch the agent (I type
    `claude` in a shell pane; herdr detects it after it starts) and cannot
    move a running agent's cwd. A launch-time prompt only works when herdr
    starts the agent (pane command, relaunch, `herdr worktree create`).
    Otherwise: a notification when a second agent is detected in the same
    checkout ("agent in pane 3 shares this checkout") with an action that
    creates a worktree and restarts the agent there, plus the first-edit
    hook as the fallback.
  - Stages: `herdr checkout status` without any setting; then a per-repo
    `shared`/`ask` setting (launch prompt, detection notification);
    `always` only once branch naming, resuming a session in its worktree
    and cleanup of finished worktrees are reliable. The setting makes me
    choose between instant visibility on `master` and isolation; it does
    not reconcile them.
- [ ] Workspace recipes (tmuxp-like): a TOML file under
  `~/.config/herdr/recipes/` naming a root, panes, splits and commands.
  - Consulted models (GPT-6 Astra, DeepSeek, 2026-09-27): a plugin built on
    the `dev-layout-bootstrap` example (`ogulcancelik/herdr-plugin-examples`),
    not core. Apply reconciles: create missing panes, leave running
    processes and hand-made panes alone, re-apply is a no-op, removal only
    with an explicit `--prune` (panes may hold uncommitted agent work).
  - Deferred: sessions already survive server restarts with 48 snapshots;
    recipes only help on a new machine or a fresh checkout. Build it when I
    notice rebuilding the same layout by hand.
- [x] `tests/client_mode.rs` can leak a `herdr server`: on 2026-09-28 a
  server from `/tmp/herdr-client-test-52393-…` (started 15:43) was still
  running at 17:30, orphaned (ppid 1) with its `sh` child, while the test
  process (52393) and the bridge in `bridge-pid` were gone; the test dir was
  left in `/tmp` too. `SpawnedHerdr::drop` kills only the client child, so a
  server the client spawned, or any process after an interrupted run (Drop
  does not run on SIGKILL), survives. Fix: tear down the server too (kill
  the process group or read the runtime dir's server pid), and have the next
  test run reap stale `/tmp/herdr-client-test-*` whose owner pid is dead.
  Also flaky: `federated_client_starts_without_local_and_survives_its_restart`
  failed once in `just check` on 2026-09-28 ("remote reconnect 2 must
  restore visible input", 12.8 s) and passed alone and on the rerun.
  It failed again on 2026-09-29 at "recovered Local must be selectable"
  (tests/client_mode.rs:1207), once in three full `just check` runs, and
  passes alone; the same assertion failed every other run while the
  client fetched `notification.list` in the background (fixed), so check
  whether something else still races the row click under load.
  - Done 2026-09-29: a test process now reaps `/tmp/herdr-client-test-*`
    bases whose test process (the pid in the name) is gone and that are
    over a minute old: it asks their servers to stop through their sockets
    and removes the dirs (157 stale dirs went on the first run). Normal
    runs already stopped servers through `cleanup_test_base`. The flaky
    wait for input after a reconnect is 15 s, like the screen wait before
    it (it failed twice at 8 s under a full parallel run).
- [ ] Explain the consult/ask naming mismatch: the plugin (`plugins/consult`,
  `local.consult`) and the stats skill (`consult-stats`, `consult.py`) say
  "consult", but the scripts inside the skills say "ask" (`ask_gpt.sh`,
  `ask_gemini.sh`, `ask_deepseek.py`). Decide whether it is deliberate (the
  verb an agent runs vs. the feature name) or should be unified, and on
  which name; consult the agents (DeepSeek, GPT-6 Astra) before renaming.

- [x] `target/` filled the disk (reported by another session, 2026-10-01): 66 GB
  (`target/debug/deps` 54 GB of stale hashed test binaries, 543,008 files),
  5.4 GiB free on a 460 GB disk, which broke a Guix builder VM elsewhere.
  - One-time cleanup done 2026-10-01 while nothing was building (no cargo or
    rustc process, only consults running): `target/debug` removed, 66 GiB free
    afterwards; the next `just check` rebuilt it (target/ is 8.2 GB after it,
    the cross targets kept).
  - Rule added (committed): `[profile.dev] debug = "line-tables-only"` in
    `Cargo.toml` (tests inherit it; backtraces keep file:line); `just sweep`
    and `just guard` (`scripts/target_sweep.py`, tests in
    `scripts/test_target_sweep.py`, part of `just maintenance-test`).
    `sweep` removes the debug profile, then the cross targets, until target/ is
    under 25 GiB, after taking cargo's own `target/<profile>/.cargo-lock`
    without waiting: if a build holds it, it gives up and says so (no process
    list guessing). `guard` runs before `just test` and `just ci` (so before
    `check`): under 15 GiB free it sweeps, rechecks and refuses to build if
    that is not enough. Consulted by the other session (GPT sol and DeepSeek):
    same plan; I left out `incremental = false` (7 GB, and `sweep` removes it),
    `cargo-sweep` (not installed; the debug profile is rebuilt on demand) and a
    launchd or git hook. Locking uses cargo's lock file, which is not a stable
    cargo interface (GPT's caveat): if cargo changes it, the sweep would stop
    protecting a running build, so keep the tests.
  - Agent instructions: after the user's "tak" (2026-10-01) `AGENTS.md` got two short
    sections (`CLAUDE.md` was removed the same day: Claude Code reads
    `AGENTS.md`), "Disk space: the
    shared `target/`" (use `just sweep`/`just guard`, never delete `target/`
    by hand, stop and report when the guard refuses) and "Waiting for a job"
    (the compact `herdr-job wait`).

- [x] Independent review of the day's client changes (2026-10-01): GPT sol 6.1
  (repo mode) and Opus 5.5 (diff) found real bugs, fixed and committed: (1)
  `prefix+u` with an unusable newest entry dropped the whole history; entries
  of another machine and a missing snapshot are now kept, only entries of a
  vanished space of this machine are dropped; (2) the footer countdown could
  be six columns (`23h59m`), now `23h`/`12d` once the first unit has two
  digits; (3) the filter bar took the key after a pending prefix (`prefix`
  then `u` typed into the bar) and did not take pasted text, key repeats or
  composed text (they reached the pane): it now only captures in terminal
  mode and handles paste, text commits and repeats; (4) `prefix+/` opened an
  invisible filter with several machines: it now says the filter is not
  available there; (5) a tab-line drag now cancels when the space's
  top-level tabs change under it; (6) a reopen's move is skipped when the
  active machine changed. Rejected: the stale `tab_press` after a drop (the
  release path clears it) and the changed click path for tab lines (no
  evidence). Not fixed: the filter's Up/Down order ignores the held sort
  order (minor). DeepSeek's answer came back empty.

- [ ] The notification history does not say what the agent asks (user,
  2026-10-01, screenshot `Screenshot 2026-10-01 at 14.19.39.png`): rows read
  `14:18 claude needs attention · email-assistant · 2 …` and `14:19 claude
  finished · email-assistant · 2 · 3`. The `2` is the workspace's position and
  the `3` an auto tab number, neither means anything to the user, and the task
  and the request never appear (the 56-column panel cuts the rest).
  - How it is built today: title `<agent> needs attention|finished`, body (the
    context) `<workspace> · <position> · <tab label if the workspace has
    several tabs>` (`notification_context`, `src/app/actions.rs`); the record
    has agent, workspace/tab/pane ids, kind and time but no task. macOS system
    notifications already use the agent's terminal title as their message.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01 (all
    four agree): row `time ? task · request` (`14:18 ? Fix IMAP retry loop ·
    Allow Bash: npm test`, `14:19 ✓ Fix IMAP retry loop`); add two optional
    fields to `NotificationRecord`, `task` (the terminal title captured when
    the notification fires, not the current one; treat `zsh`, a bare path or
    the agent name as none) and `request` (the agent's own message with the
    blocked state: Claude Code's notification hook message, Pi's reported
    message), both sanitised (no escapes or control characters, one line,
    about 80 and 160 characters) and optional so old clients ignore them
    (frozen generation-1 contract); no screen scraping (fragile, can copy
    secrets); drop the workspace position and auto tab numbers; keep one line
    per row (15 rows) and show the highlighted row's full task, request,
    agent, workspace and a meaningful tab label in a detail footer; truncate
    the workspace first, keep the request visible; fall back to `Input
    needed; open pane` when there is no request, never invent one.
  - Tests: old JSON without the fields; sanitising; useless titles; 56 / 40 /
    20 columns with CJK and emoji; every kind; `task` is a snapshot at
    notification time; auto tab numbers hidden; a click still jumps to the
    right pane; the 100-entry bound.
  - Done 2026-10-01: nothing yet (scoping the change in the server first).

- [ ] Too many notifications while the agent keeps working (user, 2026-10-01,
  screenshot of the history: `14:19 claude finished` and `14:19 claude needs
  attention` for the same pane): "what do I need this notification for, if
  the agent keeps working anyway?" For one Claude Code pane the history shows
  finished / needs attention / finished / needs attention x2 / finished within
  two minutes.
  - Cause (from the code, not reproduced): the in-app toast path waits
    `ui.toast.delay_seconds` (default 1 s) and notifies only if the pane is
    still in the same state (`pending_agent_notifications`), but the path that
    feeds client shells, the history list, system notifications and sounds
    (`forward_semantic_agent_transition`, `src/server/headless/notifications.rs`)
    sends and records at the transition itself, with no delay and no same-state
    check, and does not dedupe repeats.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01; they
    agree on: one eligibility policy before every channel (history, shell,
    system notification, sound, toast), one pending notification per pane that
    any state change replaces and a return to `working` cancels; a settle delay
    (needs attention 3-5 s, awaiting reply 1-2 s, idle/turn ended 10-30 s);
    dedupe a repeated needs-attention for the pane until the user interacts or
    the agent worked for about 5 s; suppress when the pane is visible and the
    client focused (not merely the active tab); history keeps only delivered
    notifications (superseded ones, if kept at all, in a separate diagnostic
    log); name the kinds honestly: "Reply needed" (awaiting reply), "Needs
    approval" (blocked), "Turn ended" (idle after a turn), and keep "Finished"
    for a trustworthy end (process exit, explicit completion); positively named
    options (`attention_delay_seconds`, `reply_delay_seconds`,
    `idle_delay_seconds`, `notify_while_viewing`, ...); the existing
    `ui.toast.delay_seconds` stays as an alias.
  - Tests (fake clock, a pure `NotificationPolicy` state machine): the user's
    14:17-14:19 sequence yields at most one notification; blocked then working
    after 1 s yields none and no history row; a blocked state held 4 s yields
    one; a repeat without interaction yields none; interaction rearms; awaiting
    reply is immediate; focus and visibility suppress; panes are independent;
    toast, shell and history get the same stream.
  - Done 2026-10-01 (committed, not installed; `just check` passes): the
    delay now applies to the semantic path as well: the client-shell
    notification and the history row are sent when the delay has run out and
    the pane is still in the same state (`forward_agent_notification_delivery`),
    so `blocked` then `working` within the delay leaves no notification and no
    history row. One wait for every kind, `delay_seconds`, default 3 (upstream
    1); `delay_seconds = 0` is still instant. (A separate
    `finished_delay_seconds` was added and removed the same day: the user did
    not understand two numbers, and the models say to have one internal
    stability check.) Test:
    `a_notification_the_agent_undoes_within_the_delay_reaches_nobody_...`. Not
    done from the list: dedupe of a repeated needs-attention until the user
    interacts, suppression by pane visibility plus client focus (still by
    active tab), honest kind names ("Reply needed", "Needs approval", "Turn
    ended"), the diagnostic log.
  - The user's question (2026-10-01): "I do not understand the 3 s / 10 s logic.
    Is it that something needs my attention, or that something finished but
    does not need me?" Consulted DeepSeek, Opus, GPT and Gemini: they agree on
    a model of three words: Needs you (a permission/approval prompt, an
    awaiting-reply question or a failure: one category, alert and keep the row
    until answered), Done (a turn or task ended without a question: quiet,
    history and a dot, no system notification or sound while the user is at the
    computer), Error (immediate). A turn that ends with a question is only
    Needs you, never a Finished/Needs-attention pair. Merge blocked and
    awaiting-reply. One internal ~3 s stability check, not a setting (if
    shown: "Only alert if it still waits for me after 3 seconds"). History:
    one row per agent episode, updated in place; a "Needs me" filter on top.
    Open decision for the user: should "finished" alert at all?
    Related: the entry above about what the history rows say.

- [ ] A sound but no notification for an approval prompt (user, 2026-10-01,
  screenshot): a Claude Code permission prompt ("This command requires
  approval", 1 Yes / 2 Yes and don't ask again / 3 No) appeared in the tab
  that was displayed; the sound played, nothing was shown.
  - Cause (client notification policy, `src/client/shell/notification_policy.rs`,
    read in the code and confirmed by the server's history, which recorded
    `needs_attention` for that pane at 14:43:56 and 14:44:58): with
    `delivery = "system"` and the Herdr window focused, `herdr_toast` is
    true, and a target tab that is the focused tab counts as "looking at it"
    (`target_active`): neither Herdr's toast (`herdr_toast && !target_active`)
    nor the system one (`!suppress_external`) is shown, while the needs-attention
    sound still plays (only Finished sounds are suppressed). By design, but the
    result is a phantom ping.
  - Consulted DeepSeek, Opus 5.5, GPT sol 6.1 and Gemini high 2026-10-01: all
    four say sound without a visible cue is wrong; focused tab is not focused
    pane is not a user looking; suppress the system toast (it would cover the
    window) but show a compact in-window cue: a Herdr toast or chip ("Claude
    needs approval · tab/pane", click to go there) plus a persistent mark on the
    sidebar row until answered; one alert per episode, no repeated toast or
    chime for a repeated needs-attention. Disagreement on whether recent input
    in that pane (15-30 s) should suppress the toast: DeepSeek, Opus and
    Gemini yes, GPT no (input is not proof it was seen).
  - The user's answer (2026-10-01): the toast need not appear when the tab
    is the active one, but the notification should appear at the top of the
    notification list (the `✉` dropdown). I tried a Herdr toast for the active
    tab and took it out again. What the list does today: the server records
    the `needs_attention` (14:43:56 and 14:44:58 are in `notification.list`),
    the dropdown fetches the list when it opens and shows the newest first, but
    the `✉` badge counts only tabs that are not shown (`notification_log_
    received`), so a question in the displayed tab raises no count, and
    nothing marks the row. Open: make the row stand out (an unread mark and
    the badge count until the prompt is answered or the tab's state changes),
    and say in the row what it is (see the next entry).
  - Not done: the persistent row mark in the sidebar, the input-recency rule,
    deduping repeats per episode.
- [ ] History rows for a repeated notification from one session (user,
  2026-10-01, screenshot: `14:42`, `14:39`, `14:38 claude finished · ~ · 7 · 4`):
  "I cannot see WHAT this claude finished", and "shouldn't a new notification
  from the same session clear the previous ones?"
  - Today the client already replaces a pending, queued or visible toast of
    the same pane when a new one arrives (`receive_notification`), but the
    server's history list keeps every record.
  - Consulted DeepSeek, Opus, GPT and Gemini 2026-10-01: merge, do not append:
    key by pane; a new unread event of the same kind replaces the older unread
    row (newest time, a counter `x3`, first time kept); a seen row stays as it
    was and the new event starts a fresh unread row; a different kind stays
    separate (a finished must never hide an unanswered needs-attention);
    needs-attention that was answered is marked resolved/greyed; the unread
    badge counts tabs with unread rows, not rows. Row text: `time mark task ·
    agent · cwd@branch · duration x3` from a snapshot taken when the event
    fires (terminal title, treating generic titles like `claude` as none), with
    the fallback agent summary, then `basename(cwd)@branch`, then cwd (`~`); no
    workspace position or auto tab number. Closed pane: keep the row, note it,
    click opens the tab or says so. Memory only. Tests as in the two entries
    above.
  - The user's rule (2026-10-01): in the displayed tab, seeing the agent's
    question, no toast is needed; but a sound from an inactive tab without a
    list entry is wrong. Consulted again: every sound, toast or system
    notification must have a matching list entry (one-way: an entry may exist
    without an alert); a needs-attention in the active tab is recorded too
    and counts as read at once (no badge); a finished in the active, focused
    tab makes no alert and so no entry. With the delayed path the entry is
    created exactly when the alert is delivered (`forward_agent_notification_
    delivery`), which keeps that invariant; the installed build still records
    at the state change.
  - Done 2026-10-01 (committed, not installed): `NotificationRecord` has two
    optional fields, `task` (the pane's cleaned terminal title when it fired;
    none for a shell or agent name, a path or an empty title; one line, 80
    characters) and `repeats`; a pane's newest entry of the same kind gives way
    to a new one that counts it (`x3`), a different kind stays separate; rows
    read `✓ Fix the login test · claude · herdr x3` (`?` for needs attention),
    without the workspace position or the tab number
    (`notification_row_text`); the generated API schema is updated. Not done:
    the `request` field (the agent's own message), the detail footer for the
    highlighted row, marking a row read or resolved in place, the badge rule for
    merged rows, a closed-pane note.
  - Done 2026-10-01 (second part): the list shows the highlighted row's whole
    text, wrapped to at most 3 lines, under a rule below the rows
    (`wrap_detail` in overlays.rs).
  - Done 2026-10-01 (third part): `NotificationRecord.request` (optional): for
    a needs-attention entry the server takes the agent's own hook-report
    message (`hook_authority.message`, the approval prompt or question),
    cleaned to one line of at most 300 characters; the row text appends it as
    `— "..."` and the footer shows it whole. A finished entry carries none, and
    a pane whose agent reported no message shows no request (agents that only
    block on screen text have none; a detection-based fallback would need the
    screen text and is not done). Still not done: marking a row read or
    resolved in place, the badge rule for merged rows, a closed-pane note.

- [x] A close confirmation when nothing is happening (user, 2026-10-01,
  screenshot): closing the tab "ask gemini 3.8-flash-low: Des…" asked `Close
  tab with running work? … stops: agy idle in ask gemini …`; the job had
  finished and the agent was idle. "Why does it ask me when nothing is going
  on?"
  - Cause: `close_impact::pane_work` counted a pane's agent in every state
    (a deliberate comment: an idle agent loses a draft, background tasks and
    its place), also in a finished job's tab.
  - Consulted DeepSeek, Opus, GPT and Gemini 2026-10-01 (unanimous): ask only
    when a close would lose something: a turn in progress, an approval
    waiting, background tasks, a question the user has yet to answer, a
    running job or program, an unknown state (to be safe); an idle or done
    agent with none of them is not worth a question; an agent in a finished
    job's tab is the one-shot command's leftover (it counts only if working,
    blocked or with background tasks, and an unknown state does not count
    there); uncommitted files survive a close, so they are no reason. They
    also suggest a `confirm_close = work | always | never` setting (the config
    already has `confirm_close` and `confirm_close_running`) and dialog lines
    that name the loss instead of the tab title.
  - Done 2026-10-01 (committed, not installed): the rules above in
    `close_impact.rs`; an agent that is idle with `awaiting_reply` reads
    `waiting for a reply`. Not done: the dialog wording.

- [ ] "What do I do with this update?" (user, 2026-10-01, screenshot): Claude
  Code shows `✓ Update installed · Restart to update` under its input box.
  - What it is: Claude Code updated its own binary in the background; the
    running session keeps the old version until it is restarted. Nothing is
    lost by waiting (the old version runs on); restarting picks up fixes and
    features, costs the process state (background tasks, an unsent draft) and
    a warm prompt cache, and the conversation comes back with `claude
    --resume <id>`.
  - What to do now (consulted DeepSeek, Opus, GPT and Gemini 2026-10-01, all
    agree): finish the current exchange, then, with the agent idle and the
    input box empty, run the `restart` plugin's menu action "Restart agent in
    this pane" (or "Restart idle agents in this workspace" for several); it
    sends SIGTERM, waits for the shell and runs the same command line again
    with `--resume <id>`, skipping working or blocked agents and a pane with
    unsent text. It is not urgent.
  - What Herdr should do (this extends the "Restart agents..." entry): detect
    the pending update from the version, not the screen text: record `claude
    --version` when the pane starts and compare it with the binary on disk
    (re-check when the file's mtime or the symlink target changes); use the
    "Update installed" text only as a hint to verify; no Claude hook reports an
    update. Default: a quiet "update pending" mark on the pane and its sidebar
    row plus a one-click restart and a preview/picker for a workspace; cleared
    only after the new version is confirmed. Automatic restart of idle agents
    is opt-in, never the default. Batches: one pane at a time, re-check
    "idle, empty input, not focused, no key in the last ~5 s" just before each
    SIGTERM, keep the exact argv, cwd and environment, replace conflicting
    resume arguments, never replay a prompt, stop the batch on the first
    failure and keep the pane, its scrollback and the session id; if resume
    fails, never start a fresh session silently.
  - Tests: version bump and mtime or symlink change set the mark, the same
    version does not; busy, blocked, draft and typing panes are skipped; the
    state changing between the check and the SIGTERM; flags survive; an
    unknown session id is an error, not a new session; a 10-pane batch runs
    in order and stops at the first failure.
  - Done: nothing yet (the plugin's stage 1 exists).

- [x] A finished turn is quiet by default (user, 2026-10-01, "Cicho: wiersz w
  liście + kropka"): `claude finished` for a tab you are not looking at no
  longer plays a sound or shows a toast or system notification; it adds the
  quiet row to the notification list and the unread dot. New option
  `ui.toast.alert_on_finished` (default `false`) restores the alert. A question
  or approval (needs attention) is unaffected. Done 2026-10-01 (client drops
  the effects in `receive_notification`; server skips its sound and toast in
  `forward_agent_notification_delivery`; history still records it).

- [x] Compact the fork's history before the upstream rebase (user,
  2026-10-01: "maybe compact the history so rebases are easier? we went one
  way, then another, and then conflicts"). State: 315 fork commits (132 docs/
  notes, 113 feat, 59 fix) on a base 47 upstream commits behind; `git
  merge-tree` shows 14 conflicting files; the hot ones (`state.rs`, `mouse.rs`,
  `sidebar.rs`) were rewritten by 35-44 fork commits each, upstream touched
  them 1-2 times; a per-commit rebase would stop on the same hunks again and
  again.
  - Consulted DeepSeek, Opus and GPT 2026-10-01 (all agree): do not resolve
    315 commits one by one and do not squash into one commit; compact to ~15-25
    dependency-ordered feature commits (docs/TODO notes folded into the feature
    or one trailing `docs: fork notes`), then rebase those on `upstream/master`
    (each conflicting hunk is resolved once). Opus's way: `git reset --mixed
    <merge-base>` and re-commit from the final tree with `git add -p`, so the
    wandering (feature, change, revert) never replays; the check is that `git
    diff <backup> HEAD` is empty before the rebase. Safety: tag the old master
    (`archive/pre-sync-20261001`) and push the tag, `git config rerere.enabled
    true`, push with an explicit lease `--force-with-lease=refs/heads/master:
    <SHA of origin/master>`, tests before the push, other clones `git fetch &&
    git reset --hard origin/master`. Cost: bisecting inside a feature is lost
    (the archive tag keeps the old history); SHAs quoted in notes stop being on
    master.
  - Going forward: sync weekly and at once when upstream touches a hot file;
    fix an existing feature with `git commit --fixup=<sha>` and `rebase -i
    --autosquash` before each sync; send generic features upstream so the
    permanent delta shrinks.
  - Done 2026-10-01 (the user said yes): 315 commits became 19 chronological
    blocks (each block's tree is exactly its last original commit's tree, made
    with `git commit-tree`) plus one fix commit; rebased on `upstream/master`
    (47 commits, 0.9.3): 8 of 19 steps conflicted, 14 files, all resolved
    (upstream's Windows actionable notifications beside the fork's click
    targets, `close_group` for worktree groups, `resume_argv`, the
    libghostty-vt crate split in `build.rs`). `just check` is green,
    including the Windows lint. `master` is now 21 commits on `upstream/master`
    and was force-pushed with an explicit lease (old origin SHA 90c99b85).
    Safety nets: tags `archive/pre-sync-20261001` (pushed) and
    `backup/master-before-compaction` (local) hold the old history.
  - Open (user asked, "19 chronological points, not functional?"; consulted
    DeepSeek, Opus and GPT, all agree): chronological blocks do not bisect
    well (an intermediate block may not compile) and cannot be sent upstream
    one feature at a time; functional commits are better for that but need
    hunk surgery because `state.rs`, `mouse.rs` and `sidebar.rs` are shared by
    most features. Recommendation: keep this result; when a feature goes
    upstream, extract it then (branch from `upstream/master`, `git diff
    upstream/master master -- <files> | git apply`, `git add -p`, check with
    `git rebase --exec 'cargo check --all-targets'`); at the next sync split
    the self-contained parts (plugins/, scripts/, docs, config options) into
    functional commits and leave the entangled core as blocks. Not done.

- [ ] Live "working" and "asking" indicators next to the notification button
  (user, 2026-10-01: "at the top next to the notification icon add a working
  icon and a count; I can click and a list opens. Same for those that ask.
  Do it next."). Consulted DeepSeek, Opus and GPT (all agree on the core):
  - Header, right to left: `✉n` (unchanged), `?n` (needs you), `◐n` (working,
    the existing animated half circle); counts derived from the snapshot the
    client already has, no background request; keep the sort/filter buttons,
    shorten the "Spaces" title first, then hide the working count.
    Open question between the models: hide a zero count (Opus, DeepSeek dims)
    or keep it dimmed for stable hit targets (GPT, DeepSeek).
  - Counts: distinct `pane_id`; asking = `Blocked || awaiting_reply`; working =
    `Working` and not asking (attention wins); herdr-job tabs are not agents,
    so they stay out (maybe a separate indicator later); the focused pane
    counts (looking at it is not an answer) but shows dimmed in the list.
  - Dropdown (one at a time, like the envelope list): one row per agent, task
    (terminal title) first, then `space / tab · agent`; an asking row says
    approval or reply; sorted by time in the state only when a reliable
    timestamp exists (else workspace/tab/pane order); about 8-10 rows, then
    scroll; the highlighted row shows its whole text below; Enter or click
    jumps to the pane's tab; Esc or an outside click closes.
  - Relation to others: the envelope is history, these are the live queues, so
    an overlap is fine; opening them does not mark notifications read; the
    hidden Agents panel stays the full inventory.
  - Tests: counts from snapshot fixtures (0, 1, many, Blocked plus awaiting
    on one pane counted once), layout at 32 and 44 columns, a row vanishing
    live while the list is open (highlight follows the pane id), jump target,
    no request sent.
  - Done 2026-10-01: `?n` (asking) and `◐n` (working, the animated glyph)
    beside `✉` in the spaces header; counts from the snapshot (distinct
    panes, attention wins over working, a zero count is hidden); a click opens
    the notification list's dropdown with the agents (task, agent, space,
    "approval" or "reply"), Enter or a click jumps to the pane (tab as a
    fallback), no request is sent. They sit right of the sort buttons and
    only when there is room: at the default 32 columns with `manual name ↑
    prio ↓` only the asking one fits (both from about 36). Not done: the
    time in the state (the snapshot has no timestamp), dimming the focused
    pane's row, the multi-machine sidebar header, narrowing the sort buttons
    to make room.

- [ ] Rebuild the fork's history as functional commits (user, 2026-10-01:
  "add a functional split of the git history to the todo"). Today `master` is
  19 chronological block commits plus fixes on `upstream/master`; the 315
  original commits are in the tag `archive/pre-sync-20261001`. Consulted
  DeepSeek, Opus and GPT; the plan they agree on:
  - Freeze: tag the compacted tip `final`, record `base` (the upstream commit
    under the blocks, `git merge-base HEAD upstream/master` at that time),
    rebuild on a disposable branch from `base`, never on newer upstream.
  - Inventory: `git diff --name-status base final`; labels from the archive
    subjects (`git log --format=%s archive/...`), co-change clusters from
    `git log --name-only`; an LLM proposes a feature for each hunk of the hot
    files (`state.rs`, `mouse.rs`, `sidebar.rs`, 35-44 commits each), a human
    reviews every assignment; order the features topologically (a symbol's
    definer comes first; merge cycles).
  - Manifest `split/features.toml`: feature, deps, exclusive path globs, hunk
    markers (function or struct regexes), archive SHAs. A script
    (`split/split.py`) parses `git diff -U0 base final`, assigns every hunk,
    emits `NN-feature.patch`, and fails on an unassigned or doubly assigned
    hunk or a failing `git apply --check`.
  - Mechanics: `git switch -c functional base`; per feature `git checkout
    final -- <exclusive files>` (handle deletions), `git apply --cached` for
    the shared hunks (or `git add -p` with final content in the worktree),
    commit; hand-fix hunks that compile only with a later feature. The other
    way, `git rebase -i` over the original 315, works only if most commits are
    feature-pure: trial once with `rerere` on and drop it when more than ~10%
    of the picks conflict in the hot files (expected here).
  - Verify: `git diff --exit-code final HEAD` empty; `git rebase --exec
    'cargo check --all-targets' base`; `just windows-lint` per commit if
    affordable; `just check` at the tip; test each snapshot in a clean
    worktree (unstaged final content hides missing dependencies).
  - Effort and stopping rule: estimates range from 1-2 days (Opus) to 40-80
    hours (DeepSeek) to 3-10 days (GPT). Timebox two days, reassess after two
    hard features, stop when more than ~15 hunks need rewriting to compile or
    the effort exceeds the value; fallback: extract a single feature on demand
    when it goes upstream (branch from `upstream/master`, take its files and
    hunks from `final`, `git add -p`).
  - Afterwards: one commit per feature, fixes as `git commit --fixup=<sha>`
    and `rebase -i --autosquash` before a sync, weekly `git rebase
    upstream/master` with `rerere` on, tag before every rewrite, keep the
    manifest as the feature index.
  - Done: nothing yet (recommended: not before a feature goes upstream).

- [ ] The flaky `federated_client_starts_without_local_and_survives_its_restart`
  fails more often now (2026-10-01): three full `just check` runs in a row
  at about 07:00 failed it ("recovered Local must be selectable", after
  about 25 s) while the machine had a load average of 18-31 (Chrome helpers
  at 100% CPU), and it passed on all 8 runs of the file or the test alone
  (about 15 s). Everything else passed (3836 tests, lint, docs). Likely a
  timing limit under load rather than a regression, but unproven: rerun on a
  quiet machine, and consider a longer wait or a deterministic wait in the
  test.
