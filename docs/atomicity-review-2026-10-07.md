# Atomicity review: the coordinator and headless workers (2026-10-07)

The user asked for the coordinator's operations to behave "like a database
transaction, not hop siup". This review reads `src/workers/`, the
`worker.*` API handlers, `plugins/job/herdr-job`, the `/todo` skill and the
coordinator's scratch wait script (`wait-worker-attention.py`), and asks of
each operation: where its commit point is, what a crash, a live handoff or a
concurrent call halfway leaves behind, whether it is idempotent, and what
can be lost or duplicated.

Method: own findings first (A1-A19 below, written before reading any model),
then one consult round `20261007-212232-daf8` with sol (GPT-6.1 Sol), MiMo
and DeepSeek, all given the same briefing and the same code. MiMo timed
out twice and gave no answer. Every claim was
checked against the code at `7318d147`. Line numbers refer to that commit.

## Operations

| Operation | Commit point | Failure halfway | Idempotent | Loss / duplication |
|---|---|---|---|---|
| `worker.start` (`mod.rs:635`) | In memory: registry insert (`:759`); on disk: the `started` record (`:741`). The prompt is sent last (`:799`). | Crash between spawn (`:706`) and `started`: empty journal, replayed as a `lost` worker with no pid or name; the CLI process is orphaned. Prompt send fails: the worker is registered and running, the caller gets an error. | No: no client key; a retry after a lost reply starts `wN+1` in the same worktree. | Duplicate worker; orphaned process. |
| `worker.prompt` (`:942`) | The pipe write (`:953`). | The `turn_ended` check runs on a cloned status outside the lock (`:946-947`); two callers both pass. `update` after the send can overwrite a fast `result`. | No. | A second user message queued; a state stuck at `working`. |
| `worker.answer` (`:995`) | Settling the question in memory under the lock (`:1034`), before the journal record (`:1039`) and the send (`:1041`). | Send fails: the question is gone, the journal says "answered", the CLI never got the response; a retry returns `worker_no_question`. | No. Without `--request` a retry answers the *next* question. | An approval applied to a question nobody saw; an answer journaled but never delivered. |
| `worker.interrupt` (`:960`) | The pipe write. | Repeated after the turn ended, it can interrupt the next turn. | No; id is `now_ms()`. | A later turn aborted. |
| `worker.stop` (`:976`) | `stop_requested_ms` in memory and journal (`:986-987`), before closing stdin and SIGTERM. | SIGTERM fails after the marker: later stops send nothing. Two concurrent stops both pass the check (cloned status). | Mostly (SIGTERM twice is harmless). | None material. |
| `worker.kill` (`:1049`) | None: signals, then journals what it killed. | Partial: a failed group kill returns before the tool sessions are killed. | Re-signals each call. On a `lost` worker it SIGKILLs every live process whose session id equals a recorded one. | Unrelated processes killed after session id reuse. |
| `begin_takeover` / `end_for_takeover` (`:1088`, `:1142`, `app/api/workers.rs:45`) | `takeover_ms` in memory under the lock (`:1128`), journaled after (`:1131`). | `takeover_ms` is never cleared: a failed thread spawn (`app/api/workers.rs:66`) or an error in `end_for_takeover` blocks every later takeover. On an `exited` worker the claim is not recorded at all (`apply` returns early, `:203`), so each click opens another tab. A handoff mid-takeover loses the tab. | No. | Two tabs resuming one session (forked transcript); a takeover stuck forever. |
| Journal write (`:391`) | `write_all` to an `O_APPEND` file; no error path, no fsync. | Errors (disk full) are logged and dropped; memory and journal diverge. A torn last line from a crash is skipped on replay; invalid UTF-8 fails the whole replay. | n/a | Questions, answers or exits missing after a restart. |
| Replay and `lost` (`:581`) | Appending `lost` to a journal that does not end in `exited`/`lost` (`:607-613`). | Runs in the *new* server during a live handoff, while the old server still owns the workers. An unreadable journal is skipped without reserving its number (`:600-606`). | Yes per journal. | A live worker recorded `lost`; its later `exited` ignored for ever (`:203`); a reused `wN` appending to an old journal. |
| `worker.wait` (`:898`) | Read only, under the lock; level-triggered on state. | A client disconnect or server stop ends it (`None`). | Yes. | A stale turn end: no sequence, so "the next turn end" is indistinguishable from the last one. |
| Client snapshot (`server/client_shell.rs:310-321`) | Two separate lock acquisitions (`pending_questions`, `summaries`). | A torn view between them; the next change notification repairs it. | Yes. | None lasting. |
| `herdr-job run` (`herdr-job:763`) | `meta.json` written under `LAUNCH_LOCK` after the tab is created (`:821`); `pane run` types `_exec` afterwards. | Crash after `tab create`: orphan tab. `pane run` delivered but its reply lost: the caller sees an error while the job runs. A launch that takes over 60 s is reported `lost` (`:148`) and may still run. | No key. | A duplicate job on retry. |
| `herdr-job _exec` (`:1098`) | `started` in `meta.json` under the job's flock (`:1117`); the result at the atomic `exit` file (`:1191`). | Executor dies: the command, in its own session (`start_new_session`), keeps running; the flock and the slot locks are released; status turns `lost`. | Yes: a second `_exec` refuses (`:1111`). | Overlapping builds in one slot; a job reported lost that still writes. |
| `herdr-job wait` (`:1318`) | Reads `exit` (atomic replace). | None. Polls every 2 s. | Yes. | None. |
| `herdr-job clean-tree` (`:567`) | The per-repository flock (`:522`) for the duration of the command. | HEAD (`:543`), the diff and the untracked files are read separately from a checkout others edit. The install from the clean tree runs later, outside the lock. If the Python process is killed, its child keeps running and the lock is gone. | Yes (reset to HEAD each run). | The wrong binary installed; a tree reset under a running build. |
| `herdr-job clean` (`:1615`) | Deleting the job directory file by file. | A concurrent `all_jobs()` reads a half-deleted directory and raises. | Yes. | A crashed `run` that printed no id. |
| Scratch wait script | The first `attention(status())` that is not `None`. | See finding 5: a missed wake, a hang after a handoff. | Yes. | A question that waits until the turn ends. |
| `/todo` claim (skill step 2) | `herdr agent rename` after checking `herdr agent list`. | Check-then-act: two sessions can both find none and both claim. | No. | Two coordinators. |
| Bringing a worker's commit in (rule) | `git cherry-pick` in the shared checkout. | A conflict leaves the shared tree in `CHERRY_PICKING` with markers, visible to every session. A dirty index makes git refuse (safe). | No. | Other sessions build or commit a half-applied tree. |
| TODO.md / DECISIONS.md edits (rule) | `git commit -- <paths>`. | Crash between the edit and the commit leaves an unattributed edit. Edit's read-before-write check catches concurrent edits. | n/a | Another session's commit sweeping the edit in. |

## Findings, by severity

Origin: **own** = in my list before the round; sol, MiMo, DS = raised by that
model. Verdict: **verified** against the code, **plausible** (the code
allows it, timing makes it rare), **dismissed**.

### 1. Critical: a live handoff abandons every running worker and lies about it

Own (A1), sol (critical), DS (partly: sticky `lost`). Verified.

`start_server_with_stop_control` opens the supervisor at once
(`api/server.rs:69-72`); `WorkerSupervisor::open` appends `lost` to every
journal that does not end in an exit (`workers/mod.rs:607-613`). In a live
handoff the new server does this while the old server still owns the
workers. Worker pipes are not handed over (`server/headless/lifecycle.rs`
carries only PTY runtimes), so when the old server exits the CLI loses
stdin/stdout and dies or is orphaned mid-turn, and its `setsid` tools keep
running. Consequences:

- every install (`scripts/herdr_live.sh install`, run by several sessions,
  several times a day, under standing approval) ends every running headless
  worker of every repository on this server;
- a rolled-back handoff leaves `lost` in journals of workers that still run
  under the old server;
- `apply` ignores everything after `lost` (`:203`), so a later `exited` from
  the old server is dropped for ever, and `begin_takeover` refuses `lost`
  (`:1097`): a worker that had finished cleanly can no longer be taken over.

Plausible (sol): during the handoff both servers can allocate the same next
`wN` from their own registries.

### 2. High: `worker answer` without `--request` answers whichever question is oldest

Own (A2), sol. Verified.

`answer` takes the oldest pending question when `request_id` is absent
(`mod.rs:1012-1019`, schema `api/schema/workers.rs:148`), and the `?` list
tells the user to type `herdr worker answer wN allow|deny` without it
(`client/shell/notification_log.rs:715`). Several questions can be pending
at once (parallel tool calls), and a question can be cancelled
(`control_cancel_request`, `:284`) and replaced while the user reads it.
Interleaving: the user reads question A (`rm …`), A is cancelled, B arrives,
the user types `allow`: B is allowed unseen. Same for a coordinator that
retries an answer whose reply it lost, or a coordinator and the user
answering together.

### 3. High: two takeovers of an exited worker open two tabs on one session

sol. Verified; not in my list.

`begin_takeover` refuses only `lost` (`:1097`), and records the claim with
`entry.status.apply` (`:1128`), which returns early for a gone worker
(`:203`). For an `exited` worker `takeover_ms` therefore stays `None`, the
"already being taken over" check (`:1107`) never fires, and every click runs
`end_for_takeover` (returns at once, `:1147`) and `open_taken_over_worker`,
which opens another `claude --resume <session>` tab. Two writers of one
session fork its transcript, the very thing the code comments say it
prevents (`app/api/workers.rs:71-73`).

### 4. High: the coordinator's wait script can miss a question and hangs after a handoff

Own (A3), sol; DS said it was correct (dismissed: see below). Verified.

- The reader journals the `permission` record with `decision: "ask"`
  (`mod.rs:1429`), then the `question` record (`:1446`), and only then
  applies the question to memory (`:1447`). The script wakes on the
  `permission` line and reads `herdr worker status` at once; if the reader
  has not reached `:1447` yet, the status has no question and the state
  `waiting_approval` is not in the script's list, so it goes back to
  reading. The next line is `type: "question"`, which it does not wake on.
  It then sleeps until the turn ends: the delay the stopgap was written to
  remove.
- It does not wake on `type: "lost"`: after a handoff the worker is gone
  and the script waits for ever.
- `tail -n0 -F` is started with `Popen` and the status is read right after;
  `tail` may open and seek the file later, so an event appended in between
  is never seen.
- The journal path is hard-coded to the unnamed session
  (`~/.local/state/herdr/workers`); `worker status` returns
  `journal_path`, which also covers `XDG_STATE_HOME` and named sessions.

### 5. High (safety, rare): `worker kill` can SIGKILL unrelated processes

Own (A5), sol, DS. Verified.

`kill` signals every live process whose session id equals one recorded in
`tool_sessions` (`mod.rs:1068-1076`, `platform/macos.rs:1272`), also for a
`lost` or `exited` worker, any time later. A session id is the leader's
pid; once that session ends, the number can belong to a new session leader,
for example the shell of a herdr pane (every PTY shell is one). No identity
(start time, parent) is checked.

### 6. Medium: an answer is committed before it is delivered

Own (A8), sol, DS (DS ranked it first). Verified, severity lowered.

The question is removed under the lock (`:1034`), then journaled (`:1039`),
then sent (`:1041`); a send error is returned but the question is gone and
a retry gets `worker_no_question`. The models describe a worker blocked for
ever; in the code `send` fails only when stdin is closed (`stop`,
`close_input`) or the pipe is broken, that is when the CLI is ending
anyway. What is real: the journal records an answer that was never
delivered, the caller cannot tell "already answered" from "never existed",
and no answer is idempotent by key. This is the reliability plan's step 3.

### 7. Medium: start, prompt and interrupt have no idempotency key or atomic check

Own (A6, A7), sol, DS. Verified.

- `start` (`:635`): no client key; a reply lost to a handoff makes a retry
  start a second worker in the same worktree. A failed prompt send (`:799`)
  returns an error for a worker that is registered and running.
- `prompt` (`:942-955`): the turn-ended check is on a cloned status outside
  the lock, the send and `update` after; two concurrent prompts both go
  through. `update(Direction::In, user)` after the send can overwrite a
  `result` the reader applied in between, leaving `working` for ever
  (plausible: a turn takes an API round trip).
- `interrupt` (`:960`): no turn binding; a repeat can abort the next turn.

### 8. Medium: a takeover claim is never released

Own (A9), sol. Verified.

Nothing clears `takeover_ms`. If the takeover thread cannot be spawned
(`app/api/workers.rs:66`) or `end_for_takeover` fails (an interrupt or
stop error), `open_taken_over_worker` only logs (`app/api/workers.rs:80-89`), and the worker
can never be taken over again. During a takeover `prompt` and `answer` are
still accepted, so a coordinator can start a new turn between the interrupt
and the stop. A handoff mid-takeover loses the tab (no durable "open the
tab" step).

### 9. Medium: journal writes fail silently; an unreadable journal loses its number

Own (A10, A4), sol, DS (no fsync). Verified.

`Journal::write` logs and drops write errors (`:395-397`); this repository
runs out of disk space in practice (TODO "disk space blocks checks"). Replay
uses `BufRead::lines`, which fails on invalid UTF-8, so a line torn inside a
multi-byte character makes `replay_journal` fail and `open` skip the file
before raising `next_number` (`:600-606`): the next `start` reuses `wN` and
appends to the old journal (`OpenOptions::append`, `:377`). The order in
which a record is journaled and folded differs between the reader
(journal, then fold) and `answer`/`begin_takeover` (fold, then journal), so
replay order can differ from the in-memory order (sol); harmless for the
current events.

No fsync: only an OS crash loses acknowledged lines; noted, not urgent.

### 10. Medium: herdr-job's locks do not cover the command they protect

Own (A13), sol. Verified.

`_exec` starts the command with `start_new_session=True` (`herdr-job:1158`)
and holds the job flock and the slot flocks itself. If the executor dies
(SIGKILL, an exception writing to a closed PTY), the command keeps running,
the slot is free for another build in the same `target/`, and `wait`
reports `lost` for a job that still writes. `clean-tree` has the same
shape: its flock lives in the Python process, the build is its child.

### 11. Medium: clean-tree's install is outside its lock

Own (A14), sol (snapshot). Verified.

AGENTS.md has the install run as a second command,
`"$(herdr-job clean-tree --path)"/scripts/herdr_live.sh install`, after
`just clean-release` released the lock. Another session's `clean-tree` run
in between resets the tree and rebuilds `target/release/herdr` with its own
paths, and the install ships that build. Within one run, HEAD (`:543`), the
diff and the untracked files are three reads of a checkout other sessions
commit to.

### 12. Medium: waits have no sequence

Own (A11), sol. Verified. `wait` (`:898`) and the scratch script are
level-triggered on the current state only: after a finished turn, waiting
for "the next turn end" returns the old one at once. This is the
reliability plan's step 2.

### 13. Low: herdr-job run is not idempotent and judges by a timer

Own (A12), sol, DS. Verified. A job whose executor has not started within
60 s of `created` is `lost` (`herdr-job:148`), a verdict by duration; it
may then start and run. A `pane run` whose reply is lost raises, the caller
retries, and the job runs twice. An orphan tab is left when the process
dies between `tab create` and `write_meta`.

### 14. Low: the /todo claim and the cherry-pick are check-then-act

Own (A16, A17). Verified against the skill text. Two `/todo` sessions
started together can both see no `todo-*` agent and both claim. A
cherry-pick that conflicts leaves the shared checkout mid-operation until
the coordinator's next step.

### 15. Low: smaller items

- Torn client snapshot (own A18, sol): two lock acquisitions; self-healing.
- `herdr-job clean` deletes a job directory file by file (own A15, sol); a
  concurrent `all_jobs()` raises on the half-deleted directory.
- `start` leaves an empty journal when the spawn fails (own A19, DS), which
  replays as a nameless `lost` worker.
- `stop` keeps its marker when SIGTERM fails, so a later stop sends nothing
  (sol, DS). Plausible only for EPERM.
- `record_tool_sessions` scans every process on each stdout line
  (`:1395`): not an atomicity defect, but per-line work in a per-worker
  loop.

### Dismissed

- DS: "`wait` does client I/O under the registry lock, a wedged client
  blocks every worker". `keep_waiting` calls `should_stop_connection`,
  which probes with a non-blocking read (`ipc.rs:212-230`; Windows
  `PeekNamedPipe`); it cannot block. Holding the lock for one syscall every
  100 ms is not a defect.
- DS: "`write_meta`'s fixed `.tmp` name lets two writers tear `meta.json`".
  Only `start_job` and `_exec` write it, in sequence, and `_exec` is
  single-flight under the job flock; `reconcile_tabs` only reads.
- DS: "the scratch script is correctly level-triggered (tail starts before
  the first status read)". `Popen` returns before `tail` opens the file;
  see finding 4.
- DS: "`prompt` is fine". Its Busy check is not atomic (finding 7).
- DS: `PR_SET_PDEATHSIG` as the fix for orphaned workers: Linux only; macOS,
  the main platform here, has no equivalent, and it would turn every
  handoff into a kill instead of fixing finding 1.
- sol: "`begin_takeover` commits in memory before the journal". True, but
  a crash in that window also loses the server, and replay marks the
  worker `lost`; no state is lost that matters.

## Fixes, in order

Mapped onto the reliability plan in TODO.md ("Make coordinating headless
workers reliable, not another patch", steps 1-7). Each is TODO-ready.

1. **Answers name their question (finding 2; before plan step 2).**
   `worker.answer` requires `request_id`; the CLI's `herdr worker answer`
   takes `--request` as mandatory (or refuses when more than one question
   is pending), and the `?` list prints the command with the request id.
   An answer whose id is no longer pending is refused with
   `worker_question_gone` naming what happened to it (answered, cancelled,
   turn ended).

2. **Takeover claims are recorded for every state and released on
   failure (findings 3, 8).** Keep `takeover_ms` outside `Status::apply`'s
   gone-guard (or refuse `exited` workers that already have a takeover
   tab), clear the claim with a journaled `takeover_failed` when the thread
   cannot start or `end_for_takeover` errors, refuse `prompt` and `answer`
   while a takeover runs, and record the tab-open step so a restart
   finishes or reports it.

3. **Live handoff does not kill or mislabel workers (finding 1).** First
   step: refuse or postpone a live handoff while a worker is running (the
   install script asks), and have `open` mark `lost` only when no other
   server owns the journal (an exclusive lock file per journal held by the
   owning server; the new server takes it after the old one exits).
   Second step, a design choice for the user: hand worker pipes over like
   PTY fds, or run workers under a small per-worker broker that outlives
   the server. Let a later `exited` replace `lost` on replay.

4. **Fix the stopgap wait script now (finding 4).** Wake on `type:
   "question"`, `"lost"` and `"exited"`; open the journal in Python and
   seek to its end before the first status read instead of starting
   `tail`; take the path from `worker status`'s `journal_path`. Dropped
   with the script when plan step 2 lands.

5. **Plan step 2, with a sequence on every journal record (findings 7,
   12).** One per-worker sequencer that journals and folds under one lock
   (so replay order equals memory order) and assigns the sequence number
   `worker.wait --attention --after <seq>` uses. `prompt` checks and sends
   under that lock and returns the turn's sequence; `interrupt` names the
   turn it interrupts.

6. **Plan step 3, idempotent answers and starts (findings 6, 7).** The
   answer is journaled as an intent, sent, then settled; a send failure
   records `answer_failed` and keeps the question pending; a repeated
   answer with the same request id and decision returns the stored
   outcome. `worker.start` takes a client key (the coordinator uses the
   TODO item and worktree) and returns the existing worker for a repeated
   key.

7. **Journal failures are visible (finding 9).** A failed journal write
   marks the worker `degraded` in its status and the `?` list; replay
   reads bytes and skips only the broken line (lossy UTF-8), and `open`
   reserves the number of every `wN.jsonl` it sees, readable or not.

8. **`worker kill` checks identity (finding 5).** Record each tool
   session with its leader's start time; kill a session only when its
   leader still has that start time (or, on macOS, when the process's
   ancestry still leads to the recorded worker); on a `lost` worker list
   what would be killed and ask.

9. **herdr-job's locks follow the command (findings 10, 13).** The command
   inherits the job and slot lock descriptors (pass them through
   `pass_fds`), so a slot stays taken until the build itself ends, and
   `status` reports `running` while any holder lives. A job not started yet
   stays `pending` until its tab is gone, instead of `lost` after 60 s.
   `run` takes an optional `--key` and returns the existing job for it.

10. **Install the build that was checked (finding 11).** `clean-tree`
    takes `--then <command>` (or a recipe `just clean-install`) so build
    and install run under one lock, and `herdr_live.sh install` refuses a
    binary whose `--build-commit` label is not the expected
    `<HEAD>~<tree>`. Read HEAD, the diff and the untracked files from one
    snapshot (`git stash create`-style tree object) instead of three
    reads.

11. **The coordinator's own steps (finding 14; plan step 6).** The `/todo`
    claim takes an exclusive lock (`flock` on a file in the repository's
    git common dir, held by a `herdr-job` slot-style holder for the
    session's life) instead of check-then-rename. A worker's commit is
    brought in with `git cherry-pick` only after `git merge-tree
    --write-tree` shows it applies cleanly; on a conflict the coordinator
    hands it back to the worker instead of leaving the shared tree
    mid-pick.

12. **Plan step 7 covers the fault-injection tests**, and should add: a
    live handoff with a running worker (finding 1), two answers without a
    request id (finding 2), two takeovers of an exited worker (finding 3),
    an event between the status read and the wait's arming (finding 4), a
    disk-full journal write (finding 9).

## Model calls

| Model | Call | Rating | Notes |
|---|---|---|---|
| sol | `59ff02b0` | useful | Matched most own findings; unique and verified: the double takeover of an exited worker (finding 3), clean-tree's lock dying with its process, interrupt hitting a later turn. One minor claim dismissed. |
| DeepSeek | `a2b38b32` | partial | Sticky `lost` dropping a later `exited` (part of finding 1) and the empty-journal ghost were useful; three claims dismissed (wait under lock, `meta.json` writers, script correctly armed); overrated the undelivered answer. |
| MiMo | `94b60a6a`, `52026d07` | useless | No answer within the wrapper's 420 s limit, twice (the full 88k-character bundle, then a 45k-character excerpt). MiMo sat this round out; no finding in this review comes from it. |
