# TODO

Open work only. A finished item leaves this file: its durable
decisions go to `DECISIONS.md`, the rest stays in the commit messages.
Parked ideas live in `TODO-deferred.md`. An open item keeps only its
title with the user's words, what is still open, and the decisions that
constrain it.

## Next, in order

Agents may do these from the top without asking when the user tells them to
work through the TODO (the user's global agent rules, "Working through TODO.md").

- [ ] A coordinator waiting on a busy worker looks idle (user, 2026-10-07, [t-tmgddenp]
  screenshot: "this circle is grey, it looks as if the coordinator is not
  working"). Its only running job is `herdr agent wait <worker pane>`: no
  output and no CPU, so after 5 minutes herdr-job reports it `--activity
  idle` and the coordinator's state shows the grey still ring (`◌`, `z` once
  the uncommitted idle-mark change lands), while the worker it waits on
  works (`◐` next to `⚒`). A wait is idle by design; its liveness is the
  awaited target's. Options: herdr-job never marks a wait job (`agent wait`,
  `watch --pid`, `pane wait-output`) idle, or reports the awaited agent's
  state instead of its own CPU and output.
  Seen again 2026-10-10 (user, screenshot of the roguix-vps coordinator: "why
  does it have a green ring and look as if it is not working? ask the models").
  Cause checked by the coordinator: it waits in a Claude background shell
  `herdr-job wait <job>` on a job in its worker's tab; `herdr-bg-badge` skips
  every `herdr-job wait` assuming the same tab's jobs token counts that job,
  which is false across tabs, so the row shows nothing running. Round
  20261010-025204-a255 (sol, MiMo, DeepSeek): derive the wait from herdr's
  records, not from parsing commands: `herdr-job wait` records a wait edge
  (waiting pane, job id, the job's tab and target), the sidebar shows the
  waiting row as busy while the target lives ("waiting on <job or worker>"),
  counted once by job id; the agent status stays idle (no new variant). Approved
  by the user's request; moved to the top.

- [ ] A headless worker's line in the sidebar cannot be clicked (user, [t-3bsem3en]
  2026-10-07, with a screenshot of the `?` list showing `worker w1 ·
  header-arrows` and its Bash question: "a headless worker's entry cannot
  be clicked; ask the models").
  Checked: the `?` row of a worker question has no pane, tab or space
  (`worker_question_row` in `src/client/shell/notification_log.rs`), so a
  click only closes the list. Round `20261007-204431-83a8` (sol, DeepSeek; MiMo
  gave an empty answer), agreeing: a click opens a dialog in herdr's modal
  style bound to the request id (not the worker): worker name and task, the
  full command (monospace, newlines kept, scrollable, never truncated: an
  approval must not rest on a preview) or the question with its options as
  buttons; Allow once / Deny (optional deny message) / View log; no default
  action on Enter. If the request is answered elsewhere or the worker exits
  while it is open, the dialog shows that and disables its buttons; the
  server rejects stale request ids. Several pending requests: "1 of 3", the
  next opens after the answer. No "allow this pattern" for now (both: a
  single command does not show a safe pattern). Keyboard works too.
  Seen again 2026-10-10 (user, screenshot: worker w79 of job-seeker in the
  `?` list, plain black, not clickable; it was escalated because its
  coordinator's pane closed). Round 20261010-025204-a255: the request-id
  dialog stays right; add, for an escalated question: who owns it and why it
  was escalated, "Stop worker" (destructive, confirmed, resolving the pending
  request), and "Open owner" when the owner pane lives; "adopt into my pane"
  later, once ownership transfer is defined (tenure handoff exists). Until the
  dialog lands, the row must not look dead: a click opens a read-only view
  (command, log) at least. Moved to the top.

- [ ] Record coordinators in the server's SQLite (user, 2026-10-08: "is it [t-ikxxc5ca]
  written to SQL that there is now a coordinator with id X that started
  coordinating at T? ask the models"). Today: no; workers store only
  `owner_pane`/`owner_session`; being a coordinator is a tab role flag.
  Round `20261008-104011-d7f1` (sol, MiMo, DeepSeek), agreeing, chosen by the
  coordinator:
  - a coordination tenure with its own id (`c-...`), never the pane or
    Claude session id: those are bindings that change on resume or a move
    to another pane (a `coordinator_bindings` table: pane, session, from,
    to); a resume keeps the tenure, a handoff starts a new one;
  - events `coordinator_started` / `coordinator_ended` (with the reason) /
    later `handoff` in the same events log, the `coordinators` table their
    projection in the same transaction (repo, current item, started_at,
    ended_at, end reason, epoch);
  - one active coordinator per repository (the TODO rule), enforced by a
    partial unique index on the repo where `ended_at IS NULL`;
  - the server is the source of truth: setting the tab role coordinator
    calls `coordinator.start`, the crown is drawn from the table, ending it
    calls `coordinator.end`; `workers.owner_coordinator_id` points at the
    tenure, the pane/session stay for routing and escalation; obligations
    key on the tenure;
  - a crashed coordinator: closed on the owner events herdr already has
    (pane closed, agent exited) and re-evaluated at server start
    (`end_reason = orphaned`), no heartbeat reaper (DeepSeek's, dismissed
    under the user's events-only decision of 2026-10-08);
  - first slice: the table, the bindings, the two events, start/end calls
    wired to the tab role, the unique index, `owner_coordinator_id`;
    handoff epochs with the handoff item, item history later.
  First slice done 2026-10-08 by `w31` (`feat: coordinator tenures, one per
  repository`, verified, installed); the first tenure is `c-6xve3ubo`
  (the herdr coordinator's tab). Left: obligations keyed on the tenure,
  the current item, handoff epochs, resume keeping the tenure.
  Slice 2 done 2026-10-10 (run `r-yp27jex3`): obligations keyed on the tenure;
  a resume of the same session moves the binding (and reopens an orphaned
  latest tenure) with its workers and runs; `todo run` sets and clears the
  current item; `herdr coordinator handoff --to <pane>` (epoch + 1, one
  transaction, a `handoff` event); the driver no longer overwrites a run's
  owner columns. Decided by the coordinator for a later slice: a `/clear`
  gives the pane's agent a new session id, so a later `claude --resume` with
  it does not find the tenure; Claude's `SessionStart` hook reports
  `source: clear` with both ids (an observable event), so herdr can move the
  binding to the new session there. After an explicit `coordinator end`
  its workers leave `worker obligations --pane` (intended).
  The same commit also fixes a pre-existing shutdown race found by `verify`
  (`server_survives_hangup_and_logs_why_it_stops`, 19/20 under load): the
  server installed its SIGINT/SIGTERM handling only after the API socket
  listened, so a signal right after "listening" was lost; now the signals
  route to `ServerStop` before the socket listens (in a handoff just before
  the new server's socket) and a stop request wakes the event loop.

- [ ] A fresh coordinator per item instead of one long-lived session (user, [t-o6hf6tr3]
  2026-10-07: "can't it be compacted or cleared now and then? ask the
  models"; decided by the user 2026-10-07 from the menu: a fresh coordinator
  per item, state only in files, herdr owns the waits, a thin chat session
  stays for talking with the user). Slices, one worker each:
  1. Verified writes: a small tool the coordinator uses for TODO.md and
     DECISIONS.md edits that fails loudly when the anchor is missing or the
     text did not land (the 2026-10-07 lost edits), and the rule that a
     preference said only in chat goes to DECISIONS.md before the turn ends.
  2. Records a new coordinator can reconcile: each delegated item in
     TODO.md carries its worker id, worktree, branch, base SHA and the event
     waited for; open menus listed in TODO.md.
  3. The per-item coordinator: herdr starts a headless coordinator for the
     top item (its startup prompt rereads TODO.md, DECISIONS.md, `git log`,
     `herdr worker list`, `herdr-job list`), which ends after committing the
     item; the next starts on that event. The chat session only answers and
     queues. The `/todo` skill and the rule change accordingly.
  Background: the user asked 2026-10-07 ("can't it be compacted or cleared now
  and then? ask the models"). Measured on 2026-10-07: the coordinator made
  734 calls averaging ~530k tokens of context; cache reads (388M x 0.1 =
  39M) are most of its 44M weighted cost, against 24M for all 26 workers.
  Round `20261007-203033-73ed`: DeepSeek and MiMo a fresh process per item,
  sol `/clear` now and a fresh process later; all three reject `/compact`
  (a lossy summary, the session already lost track of failed TODO edits
  after one). Needed first under any choice: TODO/DECISIONS writes read
  back and verified (that bug), chat-only preferences written to
  DECISIONS.md before the turn ends, worker/job records a new coordinator
  can reconcile (worker id, worktree, branch, base SHA, the event waited
  for), pending menus in TODO.md.
  Slices 1 and 2 done by 2026-10-09: `scripts/todo_edit.py` and the driver's
  records (runs, attempts, landings, item history and reconcile, tenures).

- [ ] A state-based coordinator stop check, in shadow mode first [t-pzba6fio]
  (Decided by the user 2026-10-09 after consult round
  20261009-141652-8a76, sol + MiMo + DeepSeek: "shadow, then block").
  The 2026-10-09 audit found 3 silent coordinator stops the ABANDON
  wording missed. In the herdr Claude Stop check (coordinator tabs), log a
  would-block decision when all hold: no active `todo run` for the repo
  and approved runnable items exist (the driver's state, not TODO text);
  the Stop input's `background_tasks` is present and empty (absent =
  unknown = pass); no `awaiting-reply` marked and no question pending;
  the repo is not paused (new `herdr todo pause|resume <repo>`; a plain
  stop/pause message from the user sets it); at most one block per turn
  and three in a row per session. Shadow phase only logs; replay the 244
  audited turn ends (`scripts/coordinator_turn_audit.py`) as fixtures:
  target 3/3 silent stops caught and at most 1 false per 100; kill
  criterion more than 1 false per 20. Enforcing the block is a later
  decision of the user. Keep the worker-obligation block as it is.

- [ ] Event-driven worker waits, no timers (user, 2026-10-07: "a deadline of [t-osip4upq]
  about 30 minutes? too much? why any asynchronous workaround at all? make
  a TODO with the models to fix this and do it next"). Supersedes the
  polling/deadline design below (worker `w-worker-end`'s commit is not taken).
  Round `20261007-175803-0ec5` (sol, MiMo, DeepSeek), agreeing: a turn end
  is an event, a task end is a verdict. The coordinator starts the worker
  with `agent.prompt_turn` (or prompt + request id) and blocks on that
  request's end: Stop, StopFailure, interrupt, the agent process's exit
  (track the agent's process, not the pane's shell), or a server restart
  reported as such; then reads the transcript once and classifies: done
  (WORKER-DONE with its sha on the branch), blocked, awaiting input (a
  question: escalate), failed, crashed, tracking lost. No timer decides
  anything: no deadline, no idle debounce, no resend after N seconds; at
  most a human-facing "overdue" notice for a real agreed deadline.
  herdr needs: `agent.wait_turn <request id> [since <cursor>]` with an
  atomic check-and-subscribe, a terminal reason enum, process-exit events,
  and request ids that survive a server restart (or an explicit error);
  check that Esc-interrupt ends the turn (MiMo). Startup prompts need an
  acknowledged, idempotent delivery instead of "resend after 15 s".
  Done 2026-10-07 by a worker (`feat: wait for a prompt's turn by event`):
  `agent.wait_turn` (finished, failed with StopFailure's error, interrupted,
  exited, unknown_request), `agent.prompt_tracked`, `herdr agent
  wait-turn`, `herdr-job wait-agent <pane> --request <id>` reading the
  transcript once; screen polling and the deadline are gone. Gap: Claude
  sends no Stop on Esc, so an interrupted turn shows only when the next
  turn starts; a coordinator waiting on a worker the user interrupted waits
  until then. Left: acknowledged startup prompt delivery.
  Bug at first use (2026-10-07): the Keychain worker finished normally
  (WORKER-DONE at 16:43 UTC) but `wait_turn` returned `interrupted`. The
  inference "another turn started before this one reported its end" seems
  to fire when a background task's notification starts a turn inside the
  same prompt's work; check the hook order for task notifications and
  derive `interrupted` only from an explicit signal.
  Acknowledged startup prompt delivery done 2026-10-08 (`feat: agent
  prompts confirm that the agent accepted them`, installed). Left: the
  false `interrupted` above.
  Fixed 2026-10-08 by headless worker `w21` (`fix: wait_turn reports
  interrupted only on an explicit signal`): `interrupted` only from Esc or
  Ctrl-C sent through `agent.send_keys` or the agent's own interrupted
  report; a turn started by a background-task notification is a
  continuation (the Claude hook marks it); ambiguous order gives `unknown`.
  Left, decided by the coordinator: confirm live that Claude sends
  `UserPromptSubmit` for a background-task notification mid-turn; the
  user's own Esc in the pane now gives `unknown` (better than a false
  `interrupted`); an explicit signal for it could come from the
  transcript's `[Request interrupted by user` marker, verified live first.
  Moved to "Needs a decision" 2026-10-09 by the coordinator: the remaining
  live check needs an interactive Claude in a pane, which may hit a folder
  trust dialog that agents must not answer.
  Question: may a worker run the live check in a throwaway pane in an already trusted folder (the herdr checkout), or will you run it?
  Options: worker in the trusted herdr checkout, read-only prompts (Recommended) | I run it myself | drop the live check
  Decided by the user 2026-10-09: a worker runs the live check in the trusted
  herdr checkout (read-only prompts, a short background task), results into
  this item.

- [ ] One wait over all of a coordinator's workers (user, 2026-10-07: [t-cguvhgwu]
  "waiting for several workers at once was deferred: so work out a new TODO
  entry with the models"). Builds on "A coordinator is woken by its
  worker's question". Round `20261007-205522-d711` (sol, MiMo, DeepSeek), all
  three: not `worker.wait --any` or an id list (membership races: workers
  started, exiting or re-owned during a wait) but a per-coordinator inbox:
  1. `worker.events --owner <coordinator> --after <cursor> --wait`: blocks
     while empty, returns every pending event in order as a bounded batch
     with `next_cursor` (two questions at once arrive together; returning
     only the first lets one unanswered question starve the rest). Events:
     question, turn end, exit, joined, left, re-owned. `worker.wait` stays
     as a shorthand over it.
  2. One server-wide monotonic sequence filtered by owner, with the
     server's incarnation (`epoch, seq`); kept across live handoff; a
     cursor the server can no longer serve gets `resync_required` and a
     snapshot, never a silent skip.
  3. Reading is not resolving (sol): pending questions are durable state;
     a reconnecting or new coordinator gets a snapshot of its workers'
     states and pending questions first, even when their events are older
     than its cursor.
  4. Handing over, as a transaction, not "hop siup" (user, 2026-10-07: "the
     old coordinator's answers are rejected: what? why is there no handoff?
     It must be like a database transaction"). Round `20261007-210018-c710`
     (sol, MiMo, DeepSeek): workers belong to the item: a coordinator
     finishes and reviews its item's workers before it ends, so normally
     only the item is handed over, not live workers (sol, MiMo). When a
     worker must go on (a crash, a long task), `coordinator.handoff {note}`
     is one server transaction, serialized with answers: it stores the note
     (what was being done, decisions said only in chat, each worker's
     purpose and what to check in review, answers given and why), moves
     ownership and the epoch, and captures worker states, pending questions
     and the cursor; an answer lands either before it (and is in the
     record) or after it (and goes to the new owner). herdr's TODO runner,
     not the old coordinator, picks and starts the successor, which reads
     and acknowledges the package before acting; the old one exits after
     the commit. Workers keep running meanwhile; their questions stay
     queued. Fencing only for a crashed, hung or stale old coordinator: its
     late actions are refused with `ownership_transferred` naming the
     successor, and its late answers are kept and passed to the successor
     as information, never applied and never dropped.
  5. One background job per coordinator (`herdr-job run -- herdr worker
     events --owner ... --wait`), not one per worker; events arriving while
     the coordinator's turn is busy wait in the inbox; after a wake the
     coordinator drains the batch and re-arms from its last cursor.
  Tests: two simultaneous questions in one batch; an event between drain
  and re-arm; a worker started and one exiting during a wait; live handoff
  and restart keep the sequence; re-owning during a wait and a stale answer
  from the old owner; a wake while the coordinator's turn is busy.
  Added 2026-10-10 from Proposed (round 20261010-010542-7df5): one
  regression check that the old wait bugs are gone (t-jnema5dg `agent wait`
  EmptyResponse in herdr-job, t-gp6n6qbx `agent start` ready before input,
  t-bojbiegs a live handoff breaking other sessions' waits) against
  `herdr todo wait`'s reconnect and `agent prompt`'s acknowledgement; close
  each one only when its check passes.

- [ ] Report a tool call whose process exited while descendants hold its [t-oan63shb]
  output open (decided 2026-10-10: the user said "decide with the models";
  round 20261010-021633-4043, sol + MiMo + DeepSeek). The try-roguix worker
  hung ~2.5 h on `ssh guest <test>` because background processes in the
  guest kept the session's stderr open. All three: there is no local
  positive event for that hang (the local ssh stays alive, blocked on the
  remote channel); a silence threshold (B) or Claude's Bash timeout used as
  a trigger (D) are the rejected timer workarounds. The fix is the
  worker's command: the remote test's completion must be the event (a
  wrapper that captures the exit status, prints a completion line and
  exits; background processes given their own stdin/stdout/stderr, e.g.
  `setsid cmd </dev/null >log 2>&1 &`; `ssh -n` alone fixes only stdin) —
  sent to the try-roguix coordinator for its task texts. herdr adds only
  the complement that is a positive fact: when a worker's tool call's own
  process has exited but other processes still hold its stdout/stderr
  pipe, raise an attention event naming those pids and commands (the same
  class as the orphaned broker that held the job slot, 2026-10-09); a
  lint-grade signal, never a kill. Test with a stub tool call that leaves a
  detached child holding stderr.

- [ ] A live handoff breaks other sessions' waits. 2026-10-07: each [t-bojbiegs]
  `scripts/herdr_live.sh install` restarts the server, and the try-roguix
  coordinator's `herdr pane wait-output` on its worker failed with
  `server_unavailable` ("server is shutting down"); it then wrapped the wait in
  a retry loop of its own. CLI waits (`pane wait-output`, `agent wait`) could
  reconnect across a handoff instead of failing.

- [ ] herdr: `agent start` reports ready before Claude accepts typed input, [t-gp6n6qbx]
  and `agent prompt` returns `agent_prompted` without knowing the prompt
  arrived (2026-10-07: the bussiness-ideas coordinator's first prompt was
  lost; workers' prompts too until the coordinator resent them). A
  structured `awaiting_user_action` state from `agent start` instead of
  `agent_not_ready` for a startup prompt (sol, round
  `20261007-040046-b1c7`). The launcher (dotfiles `140fc7f`) works around
  both: it waits up to 10 minutes for the user to answer Claude's trust
  prompt and resends the first prompt once unless the agent turns working.

- [ ] `herdr agent wait <worker> --until ... --timeout 3600000` inside [t-jnema5dg]
  herdr-job failed with `Error: Custom { kind: Other, error: EmptyResponse }`
  after 4-5 minutes, four times, while the workers kept running (reported
  by the email-assistant coordinator, 2026-10-07). Long waits must survive;
  find where the socket returns an empty response (a server-side timeout?).

- [ ] Waiting for a pane worker's final report (reported by the try-roguix [t-audxade5]
  coordinator, pane w6:p87, 2026-10-10, at the user's request). It drives
  interactive Claude workers in panes and scrapes them. Its nine points,
  with what herdr has since (checked by the coordinator):
  1. No "worker finished its task" signal (idle is not finished): headless
     workers end their turn with `herdr worker wait --attention` and their
     `WORKER-DONE` in `last_result`; `herdr todo run` drives the whole item.
  2-3. Scraping false positives (its own prompt, the task example, old
     scrollback, `WORKER-DONE a b c |` not matching, a finished worker
     unnoticed for ~6 h): gone with headless workers (the final text is
     structured, not scraped); the driver's contract allows one sha.
  4. Waits end on a server restart: `herdr todo wait` reconnects (8eaec1f2);
     `pane wait-output`/`agent wait` still do not.
  5. `agent read --lines` fails while the agent works: open.
  6. A stuck worker is invisible (no output for 2.5 h): open; must be an
     observable fact, not a silence timer (the delay rule).
  7. Text the user typed into a worker's input box is lost when its tab or
     worktree closes, and a coordinator's prompt is appended to it: open.
  8. `worktree create` without `--cwd` uses the focused workspace's repo:
     open.
  9. `agent start` in a never-trusted folder returns `agent_not_ready`
     without naming the trust dialog: `agent prompt` names it since
     (`agent_prompt_blocked`); `agent start` should too.
  Proposed: move that coordinator to headless workers and `herdr todo run`
  (needs the driver to run in its repository: checks and install registered
  in its `.herdr/`), and fix 4 (pane waits), 5, 7, 8, 9 for pane workers.
  Approved by the user 2026-10-10 ("move it"): the try-roguix coordinator
  moves to headless workers and `herdr todo run` (its `.herdr/` checks and
  install registered in that repository), and 4, 5, 7, 8 and 9 are fixed for
  pane workers.
  Done 2026-10-10 by the herdr coordinator at the user's request: told the
  try-roguix coordinator (w6:p87) to run its workers headless from its next
  item (start, `worker wait --attention`, answer, ack, `verify` before a
  cherry-pick), and to propose registering its checks for `herdr todo run`
  to the user. Left here: the pane-worker fixes 4, 5, 7, 8, 9.
  Second report from the try-roguix coordinator (2026-10-10, pane w6:pAH,
  sent at the user's request; its round 20261010-025030-401e, sol + MiMo).
  It still needs pane (TUI) workers for VM items (the sandbox case). Seen:
  `herdr-job wait-agent <pane> --request <id>` exited 8 after a herdr
  restart while the worker kept working; the default wait fired on a
  transient idle (the worker woken by its own background-shell
  notification); `--until done|blocked` fired on a turn end while the
  worker's own background test ran; its stopgap (wait for working, then
  for done, re-armed per turn) can latch a stale state. Agreed there and
  taken into this item: the positive event is "this assignment produced a
  WORKER-DONE/WORKER-BLOCKED line, or the worker asks", not a pane state
  or a turn end; so `herdr-job wait-agent --until verdict`, scoped to the
  assignment (the prompt's request id), inspect-then-subscribe atomically
  (a verdict already in the transcript ends it at once), recovering after
  a herdr restart by re-reading the transcript instead of exiting 8; the
  `todo` skill then names that wait for pane workers and its exits; a
  worker-contract line "emit the WORKER line only after your own background
  jobs end" only as a complement.
  Third note from try-roguix (2026-10-10): `wait-agent --until done` exits 0
  both on a verdict and on a bare turn end, so the exit code cannot tell them
  apart; the verdict mode gets its own exit code.

- [ ] Waiting for a worker without shell state (user, 2026-10-08, after the [t-khw7lira]
  coordinator's `${SEQ:+--after $SEQ}` became one argument in zsh and the
  wait failed at once: "how is it armed? ask the models"). Round
  `20261008-104136-8de5` (sol, MiMo, DeepSeek): a shell-care rule alone is a
  wish; make it mechanical:
  1. `herdr worker wait <id> --attention` without `--after` starts after
     the caller's acknowledged seq when the caller owns the worker (the
     server keeps `acked_seq` since step 4), so the coordinator's loop is:
     wait, handle, `herdr worker ack <id> <seq>`, the same wait again; no
     variable in the shell (chosen by the coordinator: it reuses what the
     server already stores, instead of a second cursor file as DeepSeek
     proposed). User 2026-10-08: "what is --after for? ask the models";
     round `20261008-104542-e5d4`: the wait is level-triggered (nothing that
     happened before it began is missed), so a finished turn would wake it
     again for ever without a cursor; the cursor is needed, the flag is not
     for an owner (sol, DeepSeek): the default is the owner's acked seq,
     and an ack means handled, not seen, so a crash before handling wakes
     it again; non-owners do not inherit the owner's cursor; `--after`
     stays for recovery, tests and observers. MiMo's "clear the state when
     the coordinator acts" was dismissed: it can hide a question or exit
     that arrives at the same time (DeepSeek). The seq is durable (SQLite
     AUTOINCREMENT), so an old cursor stays valid.
  2. `--after=<seq>` accepted as one token; a stray argument names itself
     in the usage error (MiMo).
  3. `herdr-job run` prints the job id as its first stdout line (everything
     else to stderr), so nobody finds a job with `herdr-job list | grep`,
     which can pick the wrong one when several run (MiMo, sol).
  4. AGENTS.md: pass arguments literally; never build options with
     `${X:+...}` or unquoted expansions (zsh does not split them); use
     arrays for lists; treat a failed wait as a failure, not as the worker
     needing attention.
  The per-coordinator inbox (planned) replaces the two-layer wait later;
  no `--then-wait` glue until then (MiMo, sol).
  Ordered 2026-10-10 (user: coordinator items to the top; consult round
  20261010-010542-7df5, sol + MiMo + DeepSeek): allowlist, tenures, fresh
  coordinator per item, stop check, hook order, waits, decision menus,
  worker lines, capabilities, review queue, T3 comparison, crate, tracker.
  All three: `t-khw7lira` is superseded by `herdr todo wait`; decided by the
  coordinator: it is done together with `t-cguvhgwu`, then closed.

- [ ] While coordinating, which wins: "ask the models" (consult now, in this [t-hgs7p6b4]
  turn) or "a new request is queued, everything else goes to a worker"?
  Report from the email-assistant coordinator (2026-10-07): the user said
  "do todo: ... ask the models", it ran the consult at once before writing
  the TODO entry; the user asked why the coordinator works itself. The herdr
  coordinator ran its consults itself all day too. The rules do not say.
  Promoted 2026-10-10 (the user: "choose with the models"; sol and
  DeepSeek: settle it before the decision menus item; MiMo: a one-line
  policy decision).

- [ ] Coordinators present "Needs a decision" questions as clickable [t-vs3664vc]
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

- [ ] A headless worker's line shows how long it has been working and which [t-jfo7bcvb]
  TODO task it got (user, 2026-10-08: "I don't see how long a given worker
  has been working, nor which task from the TODO it got; ask the models").
  Round `20261008-031505-ffde` (sol, MiMo, DeepSeek), agreeing, chosen by the
  coordinator:
  - the line: a per-state glyph (with a text state in the tooltip, not
    colour alone), the task title (not the coordinator's nickname,
    truncated with `…`) and the age right-aligned (`<1m`, `12m`, `1h05`,
    `2d03h`), counted from the worker's start (it includes waiting; the
    current turn's time goes to the tooltip and the log);
  - the age refreshes with the sidebar's existing animation/minute
    redraw, computed from the server's timestamps at render, no per-row
    timer (render stays cheap);
  - the tooltip: full TODO title, item id, worker name, state, branch,
    folder slot, turns, current turn's time, last activity;
  - the log popup header: the same, then the full assigned task text
    (collapsible) above the log, and the takeover action visible there
    (DeepSeek: right-click alone is undiscoverable);
  - `worker.start --item <title or id>` separate from `--name`, stored as
    the worker's item; without it, the prompt's first line (recorded as
    inferred); the coordinator passes the TODO item title, and the id once
    items have `[t-...]` ids;
  - ended workers: their lines may go (item "Headless worker lines after
    their work is done") but stay reachable in a history view (sol,
    DeepSeek), which the item history can provide.

- [ ] Headless worker lines in the sidebar after their work is done [t-nulti42o]
  (proposed by the coordinator 2026-10-08 from the user's screenshot of the
  herdr space: "header arrows lost", "answers name their q… lost",
  "vendored mktemp us… exited", "takeover claimed o… exited", each with
  the same green circle):
  - `lost` is wrong for a worker that had finished its turn and only
    waited for a next prompt when an install restarted the server: show
    it as finished (ended by a server restart), not lost (relates to the
    atomicity review's finding 1);
  - the state glyph should differ by state (working, waiting for you,
    finished, failed, exited, lost), as tab lines do;
  - ended workers stay listed for ever: drop a line once its worker has
    exited and the coordinator took its commit (or after the user opens
    its log once), with the log still reachable via `herdr worker log`;
  - a worker that finished its turn keeps its process and its folder slot
    until `herdr worker stop`: decide whether the coordinator stops it
    when it brings the commit in.
  User 2026-10-08, with a screenshot of 19 worker lines under the
  coordinator: "fold idle agents does not work on the tabs the coordinator
  opened; why are they not closed at all? ask the models". Round `20261008-120458-71a7`
  (sol, MiMo, DeepSeek), agreeing, chosen by the coordinator: a worker line
  goes away once the worker is exited or finished, acked by its owner, and
  has no open question or unacked event (not tied to the commit reaching
  master: a cherry-pick can fail); acking a finished turn stops the worker
  and frees its folder slot (an ack means handled); the ⊟ fold button folds
  quiet worker lines with the quiet tabs, never one with a question or an
  unacked event; ended workers collapse into one line per owner tab ("18
  ended workers ▸", expanding on click); `herdr worker list --all`, the log
  and the item history keep them reachable; per-state glyphs instead of
  one green circle.

- [ ] The space row's `T` becomes the coordinator crown (user, 2026-10-09: [t-czovnhtt]
  "the T icon in the herdr space line should be a crown icon, like in the
  tab"). `TODO_LABEL` (" T ", src/client/shell/sidebar.rs) starts the
  space's `todo_command`, i.e. a coordinator; use the same crown glyph the
  sidebar shows for a `coordinator` tab, with the same width handling as
  the other chips (a glyph whose width differs between terminals must not
  shift the row), and keep its click and tooltip behavior.

- [ ] Worker capabilities in the coordination protocol, and a prepare step [t-ih3cmtjf]
  (user, 2026-10-09: "the coordinator-worker model is meant to become a
  protocol with an implementation in Odin etc., not hardcoded crates.io";
  consult round 20261009-190333-40fb, sol + MiMo + DeepSeek agreeing;
  decided by the user: "prepare + capabilities"). The coordination layer
  names no registry, language or tool:
  - Protocol (language-neutral, versioned): capabilities `net.egress{hosts}`,
    `fs.write{paths}`, `env{names}`, `exec{argv}`; a repository or item
    requests, the user's policy grants, the worker kind's adapter enforces;
    an unknown or unsupported capability is refused (fail closed); every
    run records its effective grants.
  - Repository (`.herdr/`), read by the driver from the run's base commit,
    never from the worker's tree: `[prepare]` argv (here `cargo fetch
    --locked`) with the capabilities it needs (here egress to crates.io and
    static.crates.io). A repository diff that changes requested
    capabilities is a question to the user, never self-granted.
  - Driver: prepare runs in its own sandbox (only the granted egress, no
    secrets, writes only the dependency cache); the worker stays offline
    and builds from that cache.
  - Worker kinds map grants to their own sandbox (Claude Code: its
    `sandbox` settings); a pi or Odin worker implements the same contract.
  Belongs with the item "Extract the coordination layer into its own crate";
  do this slice first, in today's code, as the crate's first protocol piece.
  Second case (try-roguix coordinator, 2026-10-10, headless worker `w78`,
  item t-w2soppgg, WORKER-BLOCKED): the worker sandbox stops that
  repository's real work: launching a test VM (codesign
  CSSMERR_TP_NOT_TRUSTED for its ad-hoc or locally signed QEMU/app), `ps`,
  `herdr-job` outside a herdr pane, and `make test` (temp-dir writes, PTYs,
  local sockets). Only docs and research items run there today. The
  capability vocabulary must cover these as named, user-granted requests
  per repository (e.g. `exec.unsandboxed{argv}` for a listed launcher,
  `proc.list`, `pty`, `net.local`), and a worker kind that cannot enforce a
  grant refuses the run (fail closed) so the coordinator routes it to a
  pane worker instead of the worker blocking mid-task.
  Host operations, 2026-10-10 (user: "a VM launcher in herdr is hardcoding
  again, like crates.io; the protocol must be implementable in Odin"; round
  20261010-030301-b507, sol + MiMo + DeepSeek agreeing): `[prepare]`
  generalizes into repository-declared operations; the protocol knows only
  operation, parameters, capabilities, grant and result, never VMs, QEMU or
  cargo. Points every implementation must follow:
  - An operation is a separately confined job, not a way out of
    confinement: repository code (e.g. `make vm-test`) is untrusted; the
    user's grant (per repository, operation and definition hash at the base
    commit) is the trust decision; confinement only contains damage.
  - Definitions and the scripts they run come from an immutable base-commit
    checkout; the worker's changes reach an operation only as explicit,
    untrusted input (otherwise a worker rewrites the Makefile the grant
    covers).
  - Each capability has a precise scope inherited by descendants; `exec`
    means arbitrary code inside the granted confinement, not a narrowing.
  - An implementation advertises what it can enforce; anything it cannot
    enforce as specified is refused; results say denied, failed or
    unsupported, plus exit status, bounded output and a job-scoped log.
  - Jobs have ids, cancellation, resource budgets and descendant cleanup;
    repeat invocations are budgeted.
  - A conformance test per capability, including "unsupported must refuse".
  First slice: `prepare` as the one operation (no parameters, argv only, no
  PTY or local sockets). VM work is slice two: it needs PTYs and local
  sockets, the hardest to confine; if an implementation cannot confine
  them, VM items stay unavailable to headless workers there rather than
  falling back silently.

- [ ] Review queue for agent commits, plus `herdr diff`. When an agent's turn [t-nwuo24w7]
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

- [ ] Compare the current headless worker implementation with T3 Code again, [t-ofobwsra]
  with the models (user, 2026-10-08: "in spare time, maybe give the models
  the current implementation to analyse again and let them compare it with
  t3code"). A worker reads `src/workers/` (store, wait, receipts, outbox,
  obligations, escalation, sandbox, folder slot, handoff) and
  `vendor/t3code` (orchestration-v2, the Claude and Codex adapters),
  briefs sol, MiMo and DeepSeek with code excerpts, verifies their claims
  and writes a report (`docs/headless-workers-vs-t3code-<date>.md`): what
  T3 does better, what we do better, gaps, and follow-up items in order.
  User 2026-10-08: "fragments? maybe modularise so a model can get a whole
  module? ask the models"; "so check modules one by one, without context
  rot?". Round `20261008-105941-b7f4` (sol, MiMo, DeepSeek), agreeing, so this
  item becomes two steps:
  1. Split `src/workers/mod.rs` (4038 lines; the god object is
     `WorkerSupervisor`, ~50 methods) into cohesive modules of 400-900
     lines, behaviour unchanged, tests green, re-exports keeping callers:
     `status.rs` first (Status, questions, `apply` returning effects
     instead of touching live processes or the registry: pure, the
     highest review value), then `journal.rs`, `live.rs`/`process.rs`,
     `registry.rs`, `types.rs`/`error.rs`, then `supervisor/` by verb
     (lifecycle, commands/questions, takeover, views) and the tests next to
     their modules. Mechanical moves only, one module per commit.
  2. Review module by module, each in a fresh request (no accumulated
     context): the whole module, its tests, and its neighbours'
     signatures with their contracts (invariants, lock order, transaction
     boundaries, failure semantics) plus a generated outline; T3 Code's
     matching part as its interface and the relevant whole file where it
     fits; per-model budget about 50-60k tokens of input (MiMo). Then one
     separate integration review of cross-module paths (start, answer,
     takeover, handoff), where atomicity bugs live (sol).

- [ ] Extract the coordination layer into its own crate (user, 2026-10-08: [t-ydd2vlwe]
  "the coordinator/worker code, the whole control, could be extracted as a
  new package; much less code than all of herdr; then various TUIs, GUIs
  could connect to it; analyse with the models"). Facts: `src/workers/` is
  ~17.5k lines (with ~5k of tests) of herdr's ~345k; it uses herdr's
  platform layer at 60 places (process groups, sessions, start tokens), API
  schema types and thread helpers; herdr reaches into it from API handlers,
  the coordinator tab role, takeover tabs, the live handoff and the client
  snapshot. Analysis, round `20261008-182824-1186` (sol, MiMo, DeepSeek), agreeing:
  - the boundary is real but not yet clean: generic orchestration (store,
    events, receipts, outbox, tenures, runs, broker, policy, verify, folder
    slots, item ids/titles, `todo run`) vs multiplexer integration (PTY
    takeover, coordinator as a tab role, sidebar, Items popup, live
    handoff); coordinator authority must not depend on tab identity;
  - a workspace crate first (e.g. `orchestrator-core`), compiler-enforced
    to import nothing from herdr, linked into herdr's server (still the
    only daemon and API host); platform needs behind ports the crate owns
    (`ProcessSpawner`, `SessionRegistry`, `StartToken`, clock, files),
    herdr implementing them, fakes in tests; decide whether Windows is in
    scope for those ports (MiMo);
  - a separate daemon only when a second client must run without herdr
    (a third long-lived process next to the server and the brokers would
    multiply recovery paths now);
  - a versioned protocol for TUI/GUI/web clients: commands with receipts,
    events with sequence numbers, replay and snapshots, capabilities (e.g.
    takeover); version the envelope, not a two-day-old schema; a web client
    needs an authenticated gateway, not the local socket;
  - when: after the driver's first slice lands (churn), together with the
    planned split of `src/workers/mod.rs` [t-ofobwsra].
  Approved by the user 2026-10-08 ("add it to the TODO"): after the
  driver's first slice [t-s3oaxcki].
  User 2026-10-08: "one could even design the protocol and make an
  implementation in Odin etc.": so the protocol is specified language-
  neutrally (a written spec plus JSON schemas for commands, events and
  receipts, and a conformance test suite any implementation runs against
  a socket), not defined by the Rust types; the Rust crate is the reference
  implementation, and a second one (Odin or another language) is possible
  once the spec and the conformance tests exist.

- [ ] The TODO as a tracker, like Jira or GitHub Issues (user, 2026-10-08: [t-dklsfhg4]
  "maybe it's time for the TODO to be in an SQL database? ask the models";
  "like jira or github issues"). Rounds `20261008-115256-4ae4` and `20261008-115345-2e9e` (sol, MiMo,
  DeepSeek): a private database as the source of truth would strand the
  ~10 repos using the global TODO rules, agents without herdr and git
  review; GitHub Issues in the fork is the closest to Jira (search,
  per-item history and threads, commit links, a board) but public, rate
  limited, and `gh` in this checkout resolves to upstream
  `herdrdev/herdr` by default (checked 2026-10-08), so every call must pin
  `--repo rofrol/herdr`. Decided by the user 2026-10-08, in order:
  1. stable ids `[t-...]` on every item in TODO.md (`todo_edit.py` mints
     them for new items and adds them to the 71 open ones, keeping the
     `?` + `Options:` question shape herdr parses);
  2. a read-only SQLite index in herdr's state dir (`herdr todo sync`: repo,
     id, section, order, title, content hash; no write-back), used by the
     item history and worker links;
  3. a pilot: about 5 items as GitHub Issues in the fork, `gh` pinned to
     `rofrol/herdr` (and a guard that refuses upstream), to feel the
     friction before any migration.
  User 2026-10-08: "there is Codeberg, based on Forgejo, so we could use
  Forgejo or copy something from its architecture; but for now a local SQL
  database is probably enough for us? ask the models". Round `20261008-115719-8c5f` (sol,
  DeepSeek; MiMo gave an empty answer): yes, local SQLite is enough; do not
  self-host Forgejo now (a service, auth, UI to maintain for one user);
  copy its concepts, not its schema: a stable global id apart from a
  per-repo number, title and Markdown body, state, timestamps and a
  version for stale-update detection, append-only comments with author,
  labels, external references (later Forgejo/GitHub ids as mappings),
  explicit order, and the decision questions stored as raw text plus
  parsed fields; defer milestones, boards, timeline, permissions,
  notifications, attachments. One authority per item: an item moved into
  the tracker is no longer edited in Markdown. Step 3 changes: the pilot
  of about 5 items runs in a local tracker in herdr's SQLite (`herdr todo`
  commands, transactional writes), with versioned JSON export/import and a
  Markdown export, instead of GitHub Issues.
  Reference source (user, 2026-10-08): a Forgejo checkout at
  `~/personal_projects/vendor/forgejo` (adfdb1a532); the tracker's model
  is read from `models/issues/` (issue.go, issue_index.go for the per-repo
  number, comment.go, content_history.go for edit history, issue_label.go,
  dependency.go for blocks/blocked-by, issue_xref.go for references),
  copying concepts, not code: Forgejo is GPL-3.0, herdr is Apache-2.0,
  so copied code would force the GPL onto herdr.
  Fossil (user, 2026-10-08: "fossil keeps issues in the repo"): its
  tickets are append-only change artifacts stored and synced with the
  repository, the current ticket table derived from them; the same
  event-and-projection shape as herdr's worker store, and a model for
  keeping tracker items with the repository rather than in a private
  database (git-bug is the git analogue).

- [ ] Bug (user, 2026-10-03, screenshot): "I closed the tab with the job, [t-xieekngb]
  but it did not close the job." The explicit close is fixed (`DECISIONS.md`,
  "Closing a parent's last pane"); a parent whose shell exits by itself
  keeps its jobs on purpose, but nothing shows it.
  Decided by the user 2026-10-06: when a parent exits on its own, show the
  "Parent <name> exited; N jobs kept running" notice and a `was <name>`
  mark on the orphaned rows (sol, DeepSeek, MiMo); no new "Close tab, keep
  jobs" button.

- [ ] The tab state does not show that something runs in the background [t-fyd7ts3f]
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

- [ ] Consult cost per model and the coordinator's extra spend (user, [t-4hgsm43b]
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

- [ ] Naming: `ask_*` scripts versus the `consult` plugin and `consult.py` [t-xl4wcb52]
  (user, 2026-10-03: "do we need to unify ask in one place and consult in
  another?"). `consult` names the bundle and the stats, `ask_*` are the
  per-vendor adapters; renaming would split the log keys (`skill` field).
  Decided by the user 2026-10-06: keep the names; add one README line
  explaining them.

- [ ] Consult stats default view: mixed rows, too much data, and why `astra [t-puirz3p4]
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

- [ ] Consult stats by lineup (user, 2026-10-03: "shouldn't consult stats [t-xxmtseue]
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

- [ ] "Consult: models" menu with checkboxes (user, 2026-10-03: "a simple [t-hdb3644s]
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

- [ ] Consult stats per model over time, to spot a silently "nerfed" model [t-62w3fpsp]
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

- [ ] No `?` on a tab that ended with a question (user, 2026-10-01, screenshot [t-goai2j6u]
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

- [ ] Consider adding a subtle gradient in the empty space between the job [t-azup3ly4]
  indicators and the next tab in the sidebar (screenshot, 2026-09-29 23:53).
  Show several visual variants in the terminal before choosing one; generate
  the demos with Python, as Claude did previously.
  Decided by the user 2026-10-06: the agent makes demos of a few variants
  for the user to view in a tab, then moves the item back under Needs a
  decision with the variants named, so the user picks one with a click.

- [ ] Remove the agents panel; fold agents into spaces. The spaces list [t-kuaojupm]
  already shows vertical tabs with job squares (`DECISIONS.md`, "Vertical
  tabs and job squares"); the old panel is only hidden by
  `ui.sidebar.show_agents_panel = false`.
  Decided by the user 2026-10-06: remove the panel and
  `show_agents_panel`; the attention counts (`◉1 ●1`: blocked, done and
  unseen; clicking switches to prio) move to the sidebar header; raise the
  white-on-accent contrast (`#4078F2`, 3.9:1) to at least 4.5:1.
  - Also open: space drag and drop in the multi-machine sidebar still works
    from the drawn spaces only.

- [ ] Dragging a space does not show where it will land (screenshot [t-llnkot6j]
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

- [ ] "Restart agents…": restart agent CLIs (Claude, pi) after they update, [t-wbgh3nrb]
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

- [ ] Does MiMo earn its slot in the default consult set? (user, 2026-10-06, [t-cdr6gn32]
  after a consult round on GLM and Kimi, `20261006-221442-cf9e`, where both
  models advised checking this before adding any model.) From consult-stats,
  compare MiMo with Sol over shared rounds (`consult.py stats --vs`):
  accepted unique findings per call, dismissed share, errors, latency. Then
  propose keep, replace or drop, with the numbers, under Needs a decision.

- [ ] Pin a tab: pinned tabs are marked with a pin icon (or similar) in [t-grbntule]
  the tab bar and stay at its start, before the unpinned tabs, like
  pinned tabs in Chrome or Firefox.
  Decided by the user 2026-10-06: like pinned spaces: pinned tabs come
  first in their space's vertical tab list; server-owned state; Pin/Unpin
  in the tab menu plus a keybinding; a 1-cell glyph in a fixed column; no
  drag across the pinned boundary.

- [ ] Pin a space, like a pinned tab: a pin icon on the space row, and [t-rqpbuado]
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

- [ ] Audit whether colours and symbols are consistent across the UI [t-atqpgouz]
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

- [ ] No view of how much memory and CPU spaces, tabs and jobs use (user, [t-xgvnwfyp]
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

- [ ] Track which buttons the user never clicks, to drop them from roherdr [t-4frwhqjr]
  (user, 2026-10-07: "it would be useful to somehow track which buttons I
  never click at all, so maybe I can throw them out of roherdr? like
  telemetry? ask the models"). Consult the default set first (local-only
  counters vs. anything sent out, where to store them, how to show the
  never-clicked list), then propose the design.

- [ ] A standing self-improvement process over agent sessions (user, [t-avp4qzo2]
  2026-10-07: "today's analysis of all Claude sessions was good, that most of
  the time is waiting for my decision. Also include pi in the analysis, and
  maybe other agents when I use them. A standing self-improvement process,
  but that's probably a separate TODO. Ask the models"). Find today's
  analysis and its script first, add pi's session files
  (`~/.pi/agent/sessions/`) and a per-agent reader so other agents can join,
  then consult the default set on making it a recurring process (how often,
  what it reports, where findings go).

- [ ] No tab line is lit for the focused tab of a collapsed worktree space [t-j543tp6e]
  (user, 2026-10-07, three screenshots: "why does this session have no
  highlighted tab? probably opened by Claude as a todo-worker; ask the
  models"). The focused pane was `worker: shuffle prev`, a worktree space a
  coordinator created (`herdr worktree create --no-focus`) nested under
  `rormpc-tools`; its name line shows `▶` (collapsed), so no tab line and no
  highlight anywhere in the sidebar marks where the user is. Check how
  collapse is chosen for API-created worktree spaces and what a collapsed
  space should show when it holds the focused tab; consult the default set.

- [ ] Navigation history survives a client restart (user, 2026-10-07: "the [t-77zz4rei]
  navigation history is cleared after a client restart, I can't go back").
  The header's back/forward (`focus_history.rs`) lives in client memory, so
  every reattach, and every install's live handoff, empties it.

- [ ] Do the consult popups need `less`? (user, 2026-10-03: "less used in [t-74laujmv]
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

- [ ] Phone notifications when I am away from the Mac (agent blocked, [t-p76rjpzl]
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

- [ ] `scripts/fork_demo/README.md` still says "oracle stats" where the menu [t-6aparioe]
  item is "consult stats" (left over from the dropped README animations).

- [ ] Force-quitting a quit Ghostty killed ~19 Claude agents in herdr panes, [t-upz2ppt4]
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

- [ ] An `×` that clears the spaces filter field (user, 2026-10-07: "in the [t-qrtnlvgf]
  filter for searching tabs and spaces, add some x to clear the field").

- [ ] The space lines are hard to read (user, 2026-10-07, screenshot of the [t-nssjdkde]
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
  Decided by the user 2026-10-07 (menu, mockup E): a space's title is its
  coordinator; workers are lines under their space, not nested spaces.
  Slices, one worker each:
  1. Clicking a space title focuses its coordinator tab (role
     `coordinator` in that space); with none, it starts one at once (the
     user chose that over a tab that waits for Start TODO), through the
     same launcher as the T button.
  2. Worker worktrees are no longer drawn as nested spaces: each shows as a
     line under the space it came from, named by its task (the worker
     agent's task title, else the tab label), with branch and worktree path
     only in its own view (tooltip).
  3. Clean the rows: no `❏ T A` buttons (new tab and agent launch move to the
     space's right-click menu), no role marks (`♛ ⚒`), no idle `○`, no row
     backgrounds except the selection; the header keeps only `?` and `!`.
  4. A space without `TODO.md` (`~`, scratch dirs): its title opens an
     overview offering "Set up coordinator" (create a TODO.md, then start
     one), not a coordinator.

- [ ] DeepSeek back in the default consult set (user, 2026-10-07: "add [t-sjngd5gy]
  DeepSeek to the consultations; it is fast, and since I have Claude Max 20
  the tokens Claude spends reading its answer do not hurt as much"). The
  `consult` skill (`plugins/consult/skills/consult/SKILL.md`) names sol +
  MiMo as the default set and says DeepSeek left it on 2026-10-03; make it
  sol + MiMo + DeepSeek. The coordinator asks DeepSeek in its rounds from
  now on.

- [ ] What the usage footer can take from Magpie (user, 2026-10-07: "what of [t-wdfmvf66]
  this for our usage widget, https://usemagpie.ai/? work out a TODO with
  the models if needed"). Magpie is a local MIT gateway agents route
  through: per-account used % and resets, tokens, cache hits and cost per
  agent and model, failover between accounts at limits. Round
  `20261007-180654-3509` (sol, MiMo, DeepSeek), agreeing: take decision
  support, not its accounting; herdr must not become a gateway.
  1. Near-limit alerts from the provider usage endpoints we already read:
     one notice when a window crosses a threshold, with its reset time,
     only for providers with running agents (all three).
  2. A pace-based forecast ("~3%/h, likely out in 30-60 min, resets 17:20"),
     coarse and labelled an estimate, suppressed when evidence is weak (all
     three; MiMo and DeepSeek rank it first).
  3. Next-worker hints: "Anthropic 95%, resets in 48 m: start the next
     worker with pi/GPT", which a coordinator can act on (sol, MiMo; MiMo:
     a one-key re-route of the next queued task).
  Skip: exact cost per agent/model and cache-hit rate (only reliable
  through a gateway; flat subscriptions make dollars noise), automatic
  failover. Later and labelled estimated: per-agent burn from local
  transcripts, Claude and Codex first (DeepSeek), which could feed the
  forecast's pace (MiMo).

- [ ] Clicking outside the popup does not close it, only Escape does (user, [t-aonxduu7]
  2026-10-07, with a screenshot of a headless worker's log popup titled
  "popup": "clicking outside the modal does not close it, only escape
  works; ask the models").
  Round `20261007-204532-f8ba` (sol, MiMo, DeepSeek): sol and DeepSeek: popups
  herdr opens itself (the worker log) close on an outside click; popups of
  custom commands (an editor may hold unsaved work) do not, and get an `[x]`
  in the border (DeepSeek: decide by who opened it, not by guessing the
  content; MiMo: ask first when the process still runs). All three: find
  out who owns Escape first (if herdr takes it, vim in a popup cannot leave
  insert mode; likely the log viewer exits on Escape itself), and title the
  popup by its content, e.g. `worker w1 · header arrows · log`, not
  "popup". Chosen by the coordinator from the agreement: sol's and
  DeepSeek's rule.

- [ ] DeepSeek joins the default consult round as a third model (user, [t-evfsfkkc]
  2026-10-09, relayed by the try-roguix coordinator: "add deepseek as an
  additional one to those two models" ... "in the skill"). In
  `plugins/consult/skills/consult/SKILL.md`, the default set becomes sol +
  MiMo + DeepSeek (`python3 "$D/../deepseek/ask_deepseek.py"`), replacing
  "DeepSeek left the default set on 2026-10-03"; keep the history line and
  the self-consultation pairs consistent (a MiMo or GPT coordinator still
  never asks itself). Check the installed copy under `~/.claude/skills`
  follows the plugin.

- [ ] Draft an upstream issue on Claude Code's shared sandbox `$TMPDIR` [t-uilmqtpb]
  (decided by the user 2026-10-09: "prepare a draft"): a worker writes the
  issue text to `docs/upstream/claude-code-sandbox-tmpdir.md` (current vs
  expected behavior, a minimal reproduction, version 2.1.293+, herdr's
  workaround of a private 0700 temp dir, `mktemp -d` ignoring `$TMPDIR` in
  a vendored script); the user reads and files it himself. Nothing is
  posted by an agent.

- [ ] The footer showed "OR $0.39 balance" after a $10 top-up (user, [t-mwwgqlah]
  2026-10-09: "why does the OR usage widget show $0.39 although I already
  topped up $10; add to TODO, ask the models"). Checked by the coordinator:
  the formula is right (`/credits` total minus usage); `usage.read` returned
  $10.39 a few minutes later. Cause: the server polls every 300 s and the
  footer shows no age, so a fresh top-up looks lost. Consult round
  20261009-233730-c86a (sol, MiMo, DeepSeek): all three: show the reading's
  age in the footer row (e.g. `OR $10.39 · 4m`) and offer a manual refresh
  through the existing rate-limited `usage.read --refresh` path, never a
  shorter interval or a refresh on focus or timers. Diverged on where:
  sol: a Refresh button in the details popup (the row's click keeps
  opening details); MiMo, DeepSeek: clicking the row refreshes. Decided by
  the coordinator: the age in the row and a Refresh action in the details
  popup, with feedback (in flight, done "checked just now", or "refreshed
  20s ago, next in 40s" when the minimum gap refuses). A failed refresh
  keeps the last successful reading's time; label a `limit_remaining`
  fallback as the key's limit, not the account balance.

- [ ] Consult helpers: clearer refusals for two caller mistakes (reported [t-65oyloai]
  by the rormpc coordinator, 2026-10-09). (1) `ask_gpt.sh` reads stdin only
  with `-f -` (on purpose: a background job can inherit a stdin that never
  closes), but a piped brief without it just prints "Empty prompt"; when the
  prompt is empty and stdin is not a TTY, say "stdin is read only with
  -f -". (2) `ask_openrouter.sh -f <file>` refuses a file outside a git
  repository (deliberate vetting), while agents keep drafts in their
  scratchpad: name the way out in the refusal (pass the brief inline or via
  `-f -`) and say it in the consult and openrouter SKILL.md files.
  Approved by the user 2026-10-09 ("zgoda jest").

## Proposed

Items agents add. Not approved until the user moves them up.

- [ ] Agents name tabs by ids the user cannot see (user, 2026-10-07, [t-7a2ext64]
  screenshot of the `?` list: "how do I know which tab that is?" for
  "approve the edit in tab w4:t6Z"). The sidebar shows space names and tab
  labels, never `w4:t6Z`. A coordinator (and any agent) should name a tab
  as the user sees it, space plus label (or the worker's role mark), and a
  worker that needs the user should be the one marked, so its own tab shows
  `?` instead of the coordinator's. Options for herdr: a `herdr tab focus`
  link in the ask, or a click on the `↳` line that jumps to a tab named in
  it.

- [ ] A working Claude agent's detected state flickers to done/idle. 2026-10-07: [t-evgrqenz]
  `herdr agent wait <worker> --until done --until blocked` returned twice
  while the worker kept working (its screen showed "Thundering…" with a
  running shell, `agent get` said working right after), so a coordinator
  waiting on the state alone reviews a worker that is not finished. Capture
  the detection screen at the flicker (`herdr agent explain --json`) to find
  which rule matches between tool calls. The coordinator's workaround: wait
  for the `WORKER-` line, debounce done/idle for 120 s.

- [ ] Coordinator gaps reported by the rormpc coordinator (todo-rormpc, [t-e5tslo5h]
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

- [ ] `workspace list` shows a space's repository only when the space is in a [t-uckv7wa5]
  worktree family (its `worktree` object), so tools read `.worktree` as "the
  space's repo" and fail for plain git spaces (2026-10-07: the todo launcher
  found no repo for job-seeker). Expose the cached `git_space()` on every
  space as its own object (`repo_root`, `repo_name`, explicit null for a
  non-git space) and document `worktree` as worktree-family membership only
  (consult round `20261007-015141-b75b`, MiMo).

- [ ] `herdr tab rename <tab> ""` leaves an empty custom label instead of [t-dxhugzpt]
  clearing it, so the tab shows nothing rather than its automatic name (the
  agent's task or the terminal title); there is no way to return to the
  automatic name (2026-10-07: the todo-worker launcher's `--label TODO`
  hid the coordinator's task, and clearing it left a blank row).
  `handle_tab_rename` calls `set_custom_name(Some(label))`; an empty or
  whitespace-only label should clear it (`None`), in the TUI rename too.

- [ ] `herdr-job clean-tree` should refuse a path that matches nothing. [t-bd7wsdn6]
  2026-10-07: from zsh, `clean-tree $PATHS -- just check` with `PATHS="a b"`
  passed one path with spaces (zsh does not split words); the tree got
  "0 changed paths" and the check ran on bare `HEAD` without a warning.

- [ ] Replace job pinning and folding with an explicit pin (user, [t-5rsujy34]
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

- [ ] Flatten worktree spaces instead of nesting them under the creator [t-g7igxlgs]
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

- [ ] Header `?` list: tell the kinds of waiting apart and use the row's [t-dp7pa7y5]
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

- [ ] Every header button looks like a button (user, 2026-10-06: "all [t-6o6ow3qe]
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

- [ ] Headless pi workers in `herdr todo run` (from the usage gate decision, [t-wcrqag77]
  2026-10-09): a second worker kind with its own stream parser, tool policy
  and sandbox equivalent and broker support, so a run can go to pi/sol while
  Claude's windows are at 90% (then the old 90/80 switch). All three models
  advised building it only as a reviewed backend, not a quota stopgap, and
  MiMo to measure first how often a Claude window reaches 90%.

- [ ] A coordinator that hits a gap in its own protocol (proposed by the [t-aketwwun]
  try-roguix coordinator, 2026-10-10): it uses the smallest reversible
  stopgap the existing rules allow, says so in its reply, and records a
  Proposed item (a tool or skill change) for the user, rather than silently
  inventing machinery or stopping while the user is away. A line in the
  `todo` skill (the user's agent config: a text edit that widens nothing,
  so no question needed per DECISIONS.md, but the user approves the item).

- [ ] The `todo` skill's worker step reads as herdr-only (user, 2026-10-10: [t-cutymffq]
  "why did the roguix-vps coordinator not use headless workers? ask the
  models"; round 20261010-025447-fba3, sol + MiMo + DeepSeek agreeing).
  Step 4 gates headless workers on "where herdr has headless workers (...;
  the herdr fork)" and then gives a herdr-only recipe (`--folder-slot
  worker`, `git rev-parse master`, "the repository's notes on headless
  workers"); in roguix-vps none of these exist, so the coordinator took the
  "Elsewhere ... TUI worker" path without stating a reason (its VM items
  would have needed a pane worker anyway, but that was not why). Fix, text
  only (no widening): headless in any repository when `herdr worker --help`
  lists `verify` and the item's build and checks fit the worker sandbox
  (`herdr worker start --cwd <worktree>`, base from the repository's
  default branch; `--folder-slot worker` only where that slot exists);
  otherwise, or when the item needs what the sandbox blocks (VM launches,
  PTYs, local sockets, `ps`), a pane worker, with the reason said in the
  reply. The global rule in ~/.claude/CLAUDE.md uses the same condition.

- [ ] Smaller herdr and skill gaps reported by the try-roguix coordinator [t-3schklfu]
  (2026-10-10, pane w6:pAH, at the user's request):
  1. A pane worker's workspace was gone after a herdr restart while its
     worktree and uncommitted work stayed; `herdr worktree open --path <it>`
     failed with `not_git_worktree` until `--cwd <main checkout>`: the error
     should say it checks the calling pane's cwd (or use `--path`'s repo).
  2. `herdr agent list` (~37 KB) and `herdr worker list` (~390 KB, whole
     journals) are too big to use: filters (`--repo`, `--cwd`, `--name`)
     and a compact default.
  3. The `todo` skill's step 5 says "exactly this message, no body or
     trailers", but some repositories require a body or trailers: it
     should say "the message the repository's rules require" (herdr's own
     verify keeps subject-only for herdr).
  4. `herdr tab role <tab> worker` is easy to forget: a `--role worker` on
     `worktree create` or `agent start`.
  5. The uncommitted-edits Stop hook fired mid-rebase listing conflict
     resolutions as possibly other sessions' edits: during a rebase, merge
     or cherry-pick it should say so instead.
  6. Claude Code refused `herdr-job run -- env X=Y bash -c "<script>"` (it
     cannot inspect a `bash -c` script for `rm`); a script file passed:
     document "pass a script file, not `bash -c`" with herdr-job.

## Needs a decision

Moved here in the 2026-10-06 triage: each item's last line states what the
user needs to decide or do.

- [ ] [t-oj2cnjt5] Until worker capabilities exist, how should the try-roguix coordinator run items that build or run the app, a VM or the builder?
  Options: pane workers for those items, headless for docs and research (Recommended) | wait for the capabilities item before any such item | a per-repository widened worker sandbox now, by hand
  Checked: its first headless worker (w78, 2026-10-10) blocked on codesign of the ad-hoc QEMU/app, `ps`, `herdr-job` outside a pane and `make test` (temp dir, PTYs, local sockets); the capabilities item [t-ih3cmtjf] now names these cases.

- [ ] [t-ra4i7neu] Install herdr's Claude integration with the coordinator allowlist (`herdr integration install claude`)?
  Options: install now and commit the settings.json diff in the dotfiles (Recommended) | install after one coordinator session tried it in shadow (log only) | not now
  Checked: the allowlist (run `r-5ft5ezc4`, 2026-10-10) is in herdr's integration assets; the installer rewrites its hook script and entries in ~/.claude/settings.json, a hook change that needs the user's approval of the whole patch (AGENTS.md "Installing a fix"). Until then coordinator tabs run unrestricted. Also active for the try-roguix, rormpc and other coordinators once installed.

- [ ] Hard boundaries instead of text rules (user, 2026-10-08: "constantly [t-6mbbnkor]
  baby-sitting the models through rules in AGENTS.md etc.; where are the
  hard boundaries? ask the models"). Round `20261008-133828-f6d6` (sol, MiMo, DeepSeek):
  the coordinator, holding push and install rights, is the riskiest actor;
  a boundary is hard only if the actor cannot remove it (a hook in `.git`
  is not; a GitHub ruleset or a credential the agent lacks is). Their
  five, in order: protect the fork's history (a ruleset against
  force-push and deletion, which conflicts with the rebase-and-force-push
  fork sync unless agents get a separate credential without bypass);
  remove upstream write paths; install only through `just clean-install`
  (a PreToolUse deny for anything else); restrict the coordinator's writes
  to TODO.md/DECISIONS.md through the verified tool (PreToolUse); CI for
  commit messages (no co-author lines) and `unwrap()` in production code.
  Guidance stays guidance for consult rounds, language and "do not touch
  others' hunks". Decided by the user 2026-10-08: the upstream cut-off
  now; done the same day by the coordinator in this checkout's
  `.git/config` (a sandboxed worker cannot write it): `upstream`'s push
  URL is `DISABLED: ...` (a push fails) and `gh repo set-default
  rofrol/roherdr` (gh defaulted to herdrdev/herdr before). Not chosen now,
  kept here: a `gh` wrapper refusing other repositories (soft: anyone may
  open issues upstream, and PATH wrappers are bypassable), the fork
  ruleset, the install and push allowlist for the coordinator, its write
  scope, and the CI checks.
  Promoted 2026-10-10 (the user: "choose with the models"; round
  20261010-010542-7df5): the coordinator's write scope and install/push only
  through allowed commands go into the allowlist slice of `t-s3oaxcki`;
  GitHub branch protection and CI stay here as their own slice.
  Moved to "Needs a decision" 2026-10-10 by the coordinator: the coordinator's
  write scope and install/push paths are done in the allowlist (e08a6305).
  Left: GitHub protection of the fork and CI. CI alone is not a hard boundary
  (an agent can still push without it); it becomes one only with a ruleset that
  requires its checks, and a ruleset against force-push conflicts with the
  fork sync, which rebases onto upstream and force-pushes `master`.
  Question: protect the fork's master on GitHub, and how does the fork sync push then?
  Options: a ruleset requiring CI checks and blocking deletion, force-push allowed only to a separate credential the agents do not have (Recommended) | CI checks only, no ruleset (visible, not enforced) | nothing on GitHub; the local allowlist and verify are enough

### Decide

- [ ] A legend explaining the UI's dots and symbols (agent state dots, [t-m2eyheg2]
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
