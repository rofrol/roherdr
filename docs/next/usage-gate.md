# Usage-gated TODO starts

Each worker provider (Claude, and OpenAI's ChatGPT plan for pi workers) has one
gate for all repositories, since the subscription belongs to the account: a
latch closed from one repository refuses starts in every other. The thresholds
are read from `[usage] gate_close_percent` and `gate_reopen_percent` at each
check (defaults: 90 and 80). Invalid settings retain the validated defaults.
Gates stored per repository by older versions become the provider's one latch
when the store is upgraded: it is closed when any of them was, high when any
closed one was high.

- **Open:** a fresh read at or above the close threshold latches **high**.
- **Unknown:** a failed or empty read refuses starts. The first successful read
  with every window below the close threshold opens it.
- **High:** only a successful measurement with every window strictly below the
  reopen threshold opens it. Failed reads never clear this latch.

The last successful reading is displayed as history, never authorization. Older
stored closed gates without a state are conservatively treated as high.

## Concurrent workers

The gate also admits at most `[usage] gate_max_workers` workers of one provider
at a time (default 2, at least 1), counted across all repositories: `todo run`
attempts and `worker run` runs alike. A headless item coordinator passes the
usage check only and takes no slot, since its own runs need them. Under the provider's
lock it counts the starts it admitted whose worker has not exited, and records
the new admission before the worker is spawned, so two starts reading usage at
the same moment cannot both take the last slot. The count is derived from the
worker store's rows (each admission, the worker it started and that worker's
exit, or the run whose worker is not started yet), never kept as a counter.

A slot is freed by the worker's stored exit (or its loss at a server restart),
by a spawn that failed, or by a start that waits for usage again. A worker that
survives a restart keeps its slot. A server's start ends the admissions of
starts a gone server left unrecorded. Nothing expires on a timer.

A start refused for the cap is refused with `usage_gate`, like one refused for
usage, and the message names the provider, the cap and every start that holds
a slot. `herdr todo run` waits for it as a pending start; a worker's exit wakes
it, and its dispatch reads usage and counts again. A retry's new attempt is not
refused for the slot its own run's worker holds, since that worker is replaced.
`--ignore-usage` admits past the cap as well; the admission still counts, and
the gate's recorded decision (`cap`) says that the override was used.

`herdr todo run` uses the new advertised `todo.run_wait` method. A usage refusal
returns a durable pending run ID instead of an error. Repeating the same item
and parameters returns that ID; conflicting parameters are refused. The pending
start occupies the repository's existing single-active-run slot, creates no
worker, and survives restart and live handoff. Other preflight failures still
refuse the request.

Wait with `herdr todo wait <run-id>`. It blocks through usage measurements until
the worker has a coordinator-facing event or the run ends. Cancel with
`herdr todo cancel-pending <run-id>`, or the pending start's `?` entry. Repeated
cancellation is idempotent. Cancellation also pauses an automatic queue, so it
cannot immediately submit the canceled item again; `todo queue on` resumes it.
Cancellation and dispatch are serialized by a store transaction; a dispatched
start must instead be aborted through its run event.

Existing periodic usage measurement and successful direct provider reads wake
eligible starts; a worker's exit wakes the starts waiting while their provider's
latch is open. Cached reports and reset timestamps never authorize dispatch.
Dispatch repeats preflight and reads the provider again immediately before the
worker start. The existing run lock and worker start command ID prevent a second
worker after concurrent wakeups, restart or handoff. No additional timer or
blind retry is added.

The sidebar and the `?` entry show the failure or high usage, the applicable
recovery threshold, and the last good measurement as history. The entry cannot
approve a start or bypass usage; its only action cancels it.

The published `todo.run` method retains its previous refusal behavior and v1
parameter shape. Older servers that do not advertise `todo.run_wait` cannot
perform this new action. No frozen client codec is changed: the existing run
and question snapshots carry the pending state and reason. Environments are
kept in memory and handed off, never persisted; after a cold restart, a later
verification may need the environment supplied by `todo resume` as other runs do.

## External orchestrators

An orchestrator outside the TODO driver runs its agents with `herdr worker run`
(`worker.run`), which passes this same gate: the same server-side check, state,
thresholds and cap for the agent's provider, never a second gate. Each
`worker.run` call is a new admission; the method takes no identity of the
caller's, so a call the caller repeats is a second run. A refusal exits with its own status (4) and the gate's
reason on stderr, and nothing starts; the caller records it instead of retrying
on a timer. `--ignore-usage` has the same meaning as for `todo run`, and the
decision is recorded in the worker's journal.
