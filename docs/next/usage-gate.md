# Usage-gated TODO starts

Each repository has an independent gate for each worker provider. The thresholds
are read from `[usage] gate_close_percent` and `gate_reopen_percent` at each
check (defaults: 90 and 80). Invalid settings retain the validated defaults.

- **Open:** a fresh read at or above the close threshold latches **high**.
- **Unknown:** a failed or empty read refuses starts. The first successful read
  with every window below the close threshold opens it.
- **High:** only a successful measurement with every window strictly below the
  reopen threshold opens it. Failed reads never clear this latch.

The last successful reading is displayed as history, never authorization. Older
stored closed gates without a state are conservatively treated as high.

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
eligible starts. Cached reports and reset timestamps never authorize dispatch.
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
(`worker.run`), which passes this same gate: the same server-side check, state
and thresholds for the repository of its directory and the agent's provider,
never a second gate. A refusal exits with its own status (4) and the gate's
reason on stderr, and nothing starts; the caller records it instead of retrying
on a timer. `--ignore-usage` has the same meaning as for `todo run`, and the
decision is recorded in the worker's journal.
