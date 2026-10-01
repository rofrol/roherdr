# TODO

## Next, in order

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
    mark is `◷` there, as in symbols. Consulted (GPT-6 Astra, DeepSeek):
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
    same night), so both use one unit. Still open: row offsets jump when
    squares above fold or close (DeepSeek: anchor on the space and its
    row); space drag and drop there still works from the drawn spaces. Consulted
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
  - Noted 2026-09-28: the footer still has no row for an OpenAI API key
    (platform, pay-as-you-go); only Codex's ChatGPT limits show.
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
    `just check` passes before the fork section of CLAUDE.md requires it.
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
- [ ] Notifications button above "spaces": clicking it opens a dropdown of
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
- [ ] Reopen the last closed tab, `prefix+u` ("undo close", configurable).
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
- [ ] Awaiting reply for agents other than Claude, the same way as their
  integrations (user, 2026-09-28): each integration that can add session
  context (a session-start hook, an extension, a plugin) injects the same
  instruction, and where the agent has a command allowlist the install
  adds `herdr agent awaiting-reply` to it, so reporting never stops at a
  permission prompt. Integrations today: antigravity_cli, codex, copilot,
  cursor, devin, droid, grok, hermes, kilo, kimi, letta, mastracode, omp,
  opencode, pi, qodercli, qwen. Check per agent what it offers; bump each
  changed integration's version once; try each live.
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

## Deferred

- [ ] Consult stats: pair the coordinator with Opus at a lower effort
  (`claude -p --model <same id> --effort low`, fresh context without project
  instructions or tools) to measure what effort buys.
  - Method (consulted 2026-09-26): same prompt against a fresh-context call
    at the coordinator's effort; the self entry is only a baseline (full
    context, rates itself). Pilot 10 rounds, conclude after 20-30, rate blind
    where practical, "unique" only relative to that round's roster. It uses
    the same subscription, so log failures, never drop them.
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
