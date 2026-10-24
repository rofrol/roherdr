//! The workers' durable state: one SQLite database per session's worker
//! directory (`workers.sqlite3`, WAL). Every event a supervisor records is
//! appended to `events`, which numbers it (`seq`, never reused), and the
//! projections it changes (`workers`, `questions`) are written in the same
//! transaction, so the database never holds an event without its effect or
//! an effect without its event. A supervisor rebuilds its workers from the
//! projections when it opens. Two servers (the old and the new one of a live
//! handoff) may write the same database; each worker's rows are written only
//! by the server that runs it.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{named_params, params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::Value;

use super::runs::{Attempt, Landing, Run, RunCheck, RunFinish};
use super::{lock, worker_number, Direction, Pending, Status, RESOLVED_QUESTIONS_KEPT};
use crate::api::schema::{
    CoordinatorOverride, CoordinatorWouldDeny, HistoryEvent, HistoryEventKind, TodoLanding,
    TodoLandingSource, TodoRunInfo, TodoRunStatus, TodoStep, WorkerLandedItem, WorkerState,
    WorkerTurnResult,
};

/// The database file, inside the session's worker directory.
pub(super) const STORE_FILE: &str = "workers.sqlite3";

/// How long a write waits for another server's write transaction on the
/// same database to end. This is only SQLite's lock wait (`busy_timeout`),
/// not a decision: a write that still finds the database locked fails, and
/// the worker is marked degraded with that error.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Schema migrations, in order; the database's `meta.schema_version` counts
/// how many have run. Never edit one that has shipped: append a new one.
const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    worker_id TEXT NOT NULL,
    direction TEXT NOT NULL,
    -- The event's `type`; NULL for a line that was not JSON, whose `body` is
    -- then that line as a JSON string.
    type TEXT,
    body TEXT NOT NULL,
    ts_ms INTEGER NOT NULL
);
CREATE INDEX events_by_worker ON events (worker_id, seq);
CREATE TABLE workers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    cwd TEXT NOT NULL,
    workspace_id TEXT,
    model TEXT,
    slot TEXT,
    state TEXT NOT NULL,
    pid INTEGER,
    session_id TEXT,
    turns INTEGER NOT NULL,
    last_result TEXT,
    rate_limit TEXT,
    tool_sessions TEXT NOT NULL,
    exit_code INTEGER,
    exit_signal INTEGER,
    stop_requested_ms INTEGER,
    takeover_ms INTEGER,
    takeover_tab TEXT,
    takeover_error TEXT,
    takeover_unfinished INTEGER NOT NULL,
    refusal TEXT,
    exited INTEGER NOT NULL,
    lost INTEGER NOT NULL,
    end_note TEXT,
    last_seq INTEGER NOT NULL
);
CREATE TABLE questions (
    worker_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'answered', 'cancelled', 'expired')),
    -- What ended it, as `worker_question_gone` says it.
    how TEXT,
    question TEXT NOT NULL,
    input TEXT NOT NULL,
    asked_seq INTEGER NOT NULL,
    settled_seq INTEGER,
    PRIMARY KEY (worker_id, request_id)
);
"#,
    r#"
-- What `worker.wait` with `until: attention` compares with `after`: the
-- user message that began the last turn, the event that ended a turn, the
-- one that made the worker gone. Filled from the events for workers
-- recorded before.
ALTER TABLE workers ADD COLUMN turn_seq INTEGER;
ALTER TABLE workers ADD COLUMN turn_end_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workers ADD COLUMN gone_seq INTEGER NOT NULL DEFAULT 0;
UPDATE workers SET
    turn_seq = (SELECT max(seq) FROM events
        WHERE worker_id = workers.id AND direction = 'in' AND type = 'user'),
    turn_end_seq = coalesce((SELECT max(seq) FROM events
        WHERE worker_id = workers.id
          AND ((direction = 'out' AND type = 'result')
            OR (direction = 'herdr' AND type IN ('exited', 'lost')))), 0),
    gone_seq = coalesce((SELECT max(seq) FROM events
        WHERE worker_id = workers.id
          AND direction = 'herdr' AND type IN ('exited', 'lost')), 0);
"#,
    r#"
-- `answering`: an answer's intent is stored and its control_response not
-- yet confirmed written (`answer_intent` .. `answer_sent`). SQLite cannot
-- change a CHECK in place, so the table is rebuilt.
CREATE TABLE questions_v3 (
    worker_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    text TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('pending', 'answering', 'answered', 'cancelled', 'expired')),
    how TEXT,
    question TEXT NOT NULL,
    input TEXT NOT NULL,
    asked_seq INTEGER NOT NULL,
    settled_seq INTEGER,
    PRIMARY KEY (worker_id, request_id)
);
INSERT INTO questions_v3 SELECT worker_id, request_id, kind, tool_name, text, state, how,
    question, input, asked_seq, settled_seq FROM questions ORDER BY rowid;
DROP TABLE questions;
ALTER TABLE questions_v3 RENAME TO questions;
-- One row per client command id: reserved with the command's first event,
-- settled with its outcome, which a repeated id gets back.
CREATE TABLE receipts (
    command_id TEXT PRIMARY KEY,
    method TEXT NOT NULL,
    -- The command's parameters without the id, as JSON: a reused id with
    -- other parameters is refused.
    params TEXT NOT NULL,
    worker_id TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'rejected')),
    -- The reply (accepted) or the error's code and message (rejected).
    result TEXT,
    created_ms INTEGER NOT NULL,
    settled_ms INTEGER
);
"#,
    r#"
-- The worker's owner (the pane and agent session that started it) and the
-- highest `seq` that owner acknowledged (`acked`). Workers recorded before
-- have no owner, so no obligations.
ALTER TABLE workers ADD COLUMN owner_pane TEXT;
ALTER TABLE workers ADD COLUMN owner_session TEXT;
ALTER TABLE workers ADD COLUMN acked_seq INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
-- Why a question of an owned worker went to the user (`escalated`,
-- `owner_gone`); NULL while it waits quietly for its owner. And why the
-- owner is gone for good, which makes its later questions go to the user.
ALTER TABLE questions ADD COLUMN escalated TEXT;
ALTER TABLE workers ADD COLUMN owner_gone TEXT;
"#,
    r#"
-- Why the worker's record is incomplete (a store or journal write failed),
-- stored with the first write that succeeds after the failure.
ALTER TABLE workers ADD COLUMN degraded TEXT;
"#,
    r#"
-- The TODO item a worker works on and its repository, so `worker.runs`
-- groups its run under that item; workers recorded before have none and are
-- listed unassigned. The run's start and end times and the questions it
-- asked and whether it ended in a turn are filled from the events of older
-- rows; the commits its turns named stay empty for them.
ALTER TABLE workers ADD COLUMN item TEXT;
ALTER TABLE workers ADD COLUMN repo TEXT;
ALTER TABLE workers ADD COLUMN started_ms INTEGER;
ALTER TABLE workers ADD COLUMN ended_ms INTEGER;
ALTER TABLE workers ADD COLUMN done_commits TEXT NOT NULL DEFAULT '[]';
ALTER TABLE workers ADD COLUMN questions_asked INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workers ADD COLUMN ended_mid_turn INTEGER NOT NULL DEFAULT 0;
UPDATE workers SET
    started_ms = (SELECT ts_ms FROM events
        WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'started'
        ORDER BY seq LIMIT 1),
    ended_ms = (SELECT ts_ms FROM events WHERE seq = workers.gone_seq),
    questions_asked = (SELECT count(*) FROM questions WHERE worker_id = workers.id),
    ended_mid_turn = gone_seq > 0 AND coalesce(turn_seq, 0) > coalesce((SELECT max(seq)
        FROM events WHERE worker_id = workers.id AND direction = 'out' AND type = 'result'), 0);
"#,
    r#"
-- The item's title in TODO.md when the worker started (`started`'s
-- `item_title`), since a finished item leaves the file. Filled from the
-- `started` event where it has one; older workers have none, and
-- `worker.runs` reads TODO.md for them.
ALTER TABLE workers ADD COLUMN item_title TEXT;
UPDATE workers SET item_title = (SELECT json_extract(body, '$.item_title') FROM events
    WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'started'
    ORDER BY seq LIMIT 1);
"#,
    r#"
-- The latest `worker.verify` verdict with its evidence (`verification`'s
-- `verification`), so `worker.runs` shows it. Older workers have none.
ALTER TABLE workers ADD COLUMN verification TEXT;
"#,
    r#"
-- The takeover claim's id (`takeover`'s `takeover_id`), which its tab
-- carries, so a restart can find the tab. Filled from the last `takeover`
-- event of a claim still held; older claims have none.
ALTER TABLE workers ADD COLUMN takeover_id TEXT;
UPDATE workers SET takeover_id = (SELECT json_extract(body, '$.takeover_id') FROM events
    WHERE worker_id = workers.id AND direction = 'herdr' AND type = 'takeover'
    ORDER BY seq DESC LIMIT 1)
    WHERE takeover_ms IS NOT NULL;
"#,
    r#"
-- Coordination tenures: who coordinates a repository's TODO, from when to
-- when, projected from the `coordinator_started` and `coordinator_ended`
-- events (whose `worker_id` is the tenure's id) in the same transaction.
-- Times are Unix milliseconds. `epoch` grows by one per tenure of the repo.
CREATE TABLE coordinators (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    item TEXT,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    end_reason TEXT,
    epoch INTEGER NOT NULL
);
-- One active coordinator per repository: this index is the claim.
CREATE UNIQUE INDEX coordinators_one_active_per_repo ON coordinators (repo)
    WHERE ended_at IS NULL;
-- The panes and agent sessions a tenure ran in; `to_at` is NULL while bound.
CREATE TABLE coordinator_bindings (
    coordinator_id TEXT NOT NULL REFERENCES coordinators (id),
    pane_id TEXT NOT NULL,
    session_id TEXT,
    from_at INTEGER NOT NULL,
    to_at INTEGER
);
CREATE INDEX coordinator_bindings_by_pane ON coordinator_bindings (pane_id)
    WHERE to_at IS NULL;
-- The tenure of the coordinator whose pane started the worker
-- (`started`'s `owner.coordinator_id`); older workers have none.
ALTER TABLE workers ADD COLUMN owner_coordinator_id TEXT;
"#,
    r#"
-- The broker that owns the worker's pipes (`started`'s `broker`: its pid,
-- socket and the worker's directory as asked for), as JSON, so a server
-- that starts while the worker runs re-attaches to it. Workers started
-- before the broker, or on a platform without one, have none.
ALTER TABLE workers ADD COLUMN broker TEXT;
"#,
    r#"
-- The broker sequence number of the last line of the worker's output that
-- is stored, written in the transaction of that line's event, so a server
-- that re-attaches asks its broker for the lines after it and skips one it
-- replays again. 0 for workers without a broker.
ALTER TABLE workers ADD COLUMN broker_seq INTEGER NOT NULL DEFAULT 0;
-- Why a re-attach could not prove the stored record whole
-- (`continuity_gap`'s `reason`); the worker then takes no prompt or takeover.
ALTER TABLE workers ADD COLUMN continuity_gap TEXT;
"#,
    r#"
-- `herdr todo run`'s runs: one TODO item driven from preflight to a
-- cherry-pick, projected from its `run_*` events (whose `worker_id` is the
-- run's id) in the same transaction. `paths` and `check_argv` are JSON
-- arrays; `check_argv` is the registered check as preflight read it, so a
-- commit cannot change its own check. Times are Unix milliseconds.
CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('running', 'waiting', 'blocked', 'done')),
    attempt INTEGER NOT NULL,
    base TEXT,
    worker_id TEXT,
    branch TEXT,
    task TEXT NOT NULL,
    message TEXT NOT NULL,
    paths TEXT NOT NULL,
    check_name TEXT NOT NULL,
    check_argv TEXT NOT NULL,
    owner_pane TEXT,
    owner_session TEXT,
    last_acked_seq INTEGER,
    -- The `run_event` waiting for `todo.resume`, while `waiting`.
    pending_event INTEGER,
    error TEXT,
    picked TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL
);
-- One run in progress per repository (it has one folder slot): this index
-- is the claim.
CREATE UNIQUE INDEX runs_one_active_per_repo ON runs (repo)
    WHERE status IN ('running', 'waiting');
"#,
    r#"
-- What a run does after the cherry-pick, as a JSON object: the registered
-- install as preflight read it, the coordinator's TODO note or closing
-- decision, and the results (the installed build, the TODO commit, the
-- pushed commit, the merged branches kept while checked out). NULL for runs
-- from before these steps.
ALTER TABLE runs ADD COLUMN finish TEXT;
"#,
    r#"
-- A run the coordinator aborted ends `aborted`. SQLite cannot change a
-- CHECK constraint, so the table is rebuilt with the same columns. A run at
-- its `abort` step (stopping its worker before it ends) no longer claims
-- its repository: the folder slot's busy check keeps a new run out while
-- that worker still runs.
CREATE TABLE runs_new (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    step TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('running', 'waiting', 'blocked', 'done', 'aborted')),
    attempt INTEGER NOT NULL,
    base TEXT,
    worker_id TEXT,
    branch TEXT,
    task TEXT NOT NULL,
    message TEXT NOT NULL,
    paths TEXT NOT NULL,
    check_name TEXT NOT NULL,
    check_argv TEXT NOT NULL,
    owner_pane TEXT,
    owner_session TEXT,
    last_acked_seq INTEGER,
    pending_event INTEGER,
    error TEXT,
    picked TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    finish TEXT
);
INSERT INTO runs_new (id, repo, item, step, status, attempt, base, worker_id, branch, task,
    message, paths, check_name, check_argv, owner_pane, owner_session, last_acked_seq,
    pending_event, error, picked, created_ms, updated_ms, finish)
    SELECT id, repo, item, step, status, attempt, base, worker_id, branch, task,
    message, paths, check_name, check_argv, owner_pane, owner_session, last_acked_seq,
    pending_event, error, picked, created_ms, updated_ms, finish FROM runs ORDER BY rowid;
DROP TABLE runs;
ALTER TABLE runs_new RENAME TO runs;
CREATE UNIQUE INDEX runs_one_active_per_repo ON runs (repo)
    WHERE status IN ('running', 'waiting') AND step != 'abort';
"#,
    r#"
-- The workspace of the pane that owns the run (`owner_pane`), which lists
-- the run's workers. Runs recorded before have none.
ALTER TABLE runs ADD COLUMN workspace TEXT;
"#,
    r#"
-- `herdr todo run`'s usage gate per repository: closed after it refused a
-- run for Claude's usage, until a fresh reading shows every window below
-- the reopening threshold. `body` is the last decision as a JSON object.
-- No row is an open gate.
CREATE TABLE usage_gates (
    repo TEXT PRIMARY KEY,
    closed INTEGER NOT NULL,
    body TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);
"#,
    r#"
-- A run's attempts, one row each; `runs` keeps pointing at the current one
-- (its `attempt`, `worker_id`, `branch` and `task`), and both are written in
-- the transaction of the run's event. `base` is the commit the attempt is
-- verified and reviewed against; `from_attempt` and `from_commit` the
-- previous attempt and its commit the attempt's branch starts from (none
-- for the first, or for one started from the base after a conflict);
-- `commit` the attempt's commit as the coordinator's decision saw it (the
-- approved one, or the one a retry carries on); `review_event`,
-- `review_decision` and `review_text` that decision (`approve`, `retry`,
-- `abort`; the retry's text); `verification` its latest verdict as JSON.
CREATE TABLE attempts (
    run_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    worker_id TEXT,
    branch TEXT,
    base TEXT,
    task TEXT NOT NULL,
    from_attempt INTEGER,
    from_commit TEXT,
    "commit" TEXT,
    review_event INTEGER,
    review_decision TEXT,
    review_text TEXT,
    verification TEXT,
    PRIMARY KEY (run_id, attempt)
);
-- Runs recorded before: filled from their events as far as they allow.
-- Attempt N's worker is what the restart to N + 1 replaced (the run's own
-- for its current attempt), its task the retry's text before that restart
-- (the run's first task for attempt 1), its decision the coordinator's last
-- approve, retry or abort of an event of that attempt, its commit that
-- event's last commit; none of them carried a commit.
WITH RECURSIVE numbered (run_id, attempt, last) AS (
    SELECT id, 1, attempt FROM runs
    UNION ALL SELECT run_id, attempt + 1, last FROM numbered WHERE attempt < last
)
INSERT INTO attempts (run_id, attempt, worker_id, branch, base, task)
SELECT n.run_id, n.attempt,
    coalesce((SELECT json_extract(e.body, '$.previous_worker_id') FROM events e
            WHERE e.worker_id = n.run_id AND e.type = 'run_restarted'
              AND json_extract(e.body, '$.attempt') = n.attempt + 1
            ORDER BY e.seq LIMIT 1),
        CASE WHEN n.attempt = n.last THEN r.worker_id END),
    CASE WHEN n.attempt = n.last AND r.branch IS NOT NULL THEN r.branch
        ELSE 'todo/' || r.item || '-' || r.id || '-' || n.attempt END,
    r.base,
    coalesce(CASE WHEN n.attempt = 1 THEN (SELECT json_extract(e.body, '$.task') FROM events e
                WHERE e.worker_id = n.run_id AND e.type = 'run_created' ORDER BY e.seq LIMIT 1)
            ELSE (SELECT json_extract(e.body, '$.task') FROM events e
                WHERE e.worker_id = n.run_id AND e.type = 'run_resumed'
                  AND json_extract(e.body, '$.action') = 'retry'
                  AND e.seq < (SELECT min(s.seq) FROM events s
                      WHERE s.worker_id = n.run_id AND s.type = 'run_restarted'
                        AND json_extract(s.body, '$.attempt') = n.attempt)
                ORDER BY e.seq DESC LIMIT 1) END,
        CASE WHEN n.attempt = n.last THEN r.task END, '')
FROM numbered n JOIN runs r ON r.id = n.run_id;
CREATE TEMP TABLE attempt_reviews AS
SELECT d.worker_id AS run_id, json_extract(a.body, '$.attempt') AS attempt, d.seq AS seq,
    json_extract(d.body, '$.event') AS review_event,
    json_extract(d.body, '$.action') AS review_decision,
    CASE WHEN json_extract(d.body, '$.action') = 'retry'
        THEN json_extract(d.body, '$.task') END AS review_text,
    json_extract(a.body, '$.event.commits[#-1]') AS reviewed
FROM events d JOIN events a
    ON a.seq = json_extract(d.body, '$.event') AND a.worker_id = d.worker_id
WHERE d.type = 'run_resumed'
  AND json_extract(d.body, '$.action') IN ('approve', 'retry', 'abort');
UPDATE attempts SET (review_event, review_decision, review_text, "commit") = (
    SELECT review_event, review_decision, review_text, reviewed FROM attempt_reviews v
    WHERE v.run_id = attempts.run_id AND v.attempt = attempts.attempt
    ORDER BY v.seq DESC LIMIT 1)
WHERE EXISTS (SELECT 1 FROM attempt_reviews v
    WHERE v.run_id = attempts.run_id AND v.attempt = attempts.attempt);
DROP TABLE attempt_reviews;
UPDATE attempts SET verification = (SELECT verification FROM workers
    WHERE id = attempts.worker_id) WHERE worker_id IS NOT NULL;
-- The commits runs landed on `master`, written with the cherry-pick's
-- event; `ts` is Unix milliseconds. Filled from the `run_picked` events of
-- runs recorded before (their commits carry no trailers).
CREATE TABLE landings (
    landed_sha TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    item TEXT NOT NULL,
    worker_commit TEXT,
    ts INTEGER NOT NULL
);
CREATE INDEX landings_by_worker_commit ON landings (worker_commit);
INSERT OR IGNORE INTO landings (landed_sha, run_id, attempt, item, worker_commit, ts)
SELECT json_extract(p.body, '$.commit'), r.id, r.attempt, r.item,
    (SELECT json_extract(v.body, '$.head') FROM events v
        WHERE v.worker_id = r.id AND v.type = 'run_verified' AND v.seq < p.seq
        ORDER BY v.seq DESC LIMIT 1),
    p.ts_ms
FROM events p JOIN runs r ON r.id = p.worker_id
WHERE p.type = 'run_picked' AND json_extract(p.body, '$.commit') IS NOT NULL;
"#,
    r#"
-- The life of each TODO item, one row per event, written with the run event
-- that caused it in the same transaction: `claimed` (`run_created`; its
-- `item_text` the item's text in TODO.md then, `ids_at_claim` the ids of
-- every item there then), `noted` and `closed` (`run_todo_committed`; `text`
-- the note or the decision, a close's `item_text` the item's last text and
-- `follow_ups` the ids in TODO.md after it that were not there at the
-- claim), `aborted` and `blocked` (the run's event of that kind; `text`
-- why). Rows name runs and attempts by id (landings by run and attempt);
-- they never copy them. Append-only: the triggers refuse every change.
-- Runs recorded before have no rows.
CREATE TABLE item_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('claimed', 'noted', 'closed', 'aborted', 'blocked')),
    run_id TEXT,
    attempt INTEGER,
    text TEXT,
    item_text TEXT,
    ids_at_claim TEXT NOT NULL DEFAULT '[]',
    follow_ups TEXT NOT NULL DEFAULT '[]',
    ts INTEGER NOT NULL
);
CREATE INDEX item_history_by_item ON item_history (repo, item, id);
CREATE TRIGGER item_history_no_update BEFORE UPDATE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
CREATE TRIGGER item_history_no_delete BEFORE DELETE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
"#,
    r#"
-- Exceptions to a coordinator tab's command allowlist: each tool call that
-- carried `# herdr-override: <reason>` and ran (`coordinator.record_override`).
-- `repo`, `coordinator_id` and `item` are the pane's active tenure then (the
-- repository of the agent's directory when it had none). `ts` is Unix
-- milliseconds. Append-only: the triggers refuse every change.
CREATE TABLE coordinator_overrides (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    repo TEXT,
    coordinator_id TEXT,
    item TEXT,
    pane_id TEXT NOT NULL,
    session_id TEXT,
    tool TEXT NOT NULL,
    command TEXT NOT NULL,
    reason TEXT NOT NULL
);
CREATE INDEX coordinator_overrides_by_repo ON coordinator_overrides (repo, id);
CREATE TRIGGER coordinator_overrides_no_update BEFORE UPDATE ON coordinator_overrides
BEGIN SELECT RAISE(ABORT, 'coordinator_overrides is append-only'); END;
CREATE TRIGGER coordinator_overrides_no_delete BEFORE DELETE ON coordinator_overrides
BEGIN SELECT RAISE(ABORT, 'coordinator_overrides is append-only'); END;
"#,
    r#"
-- The coordination tenure that owns the run (`run_created`'s
-- `owner_coordinator`), moved with the run's owner pane and session when the
-- tenure resumes in another pane or hands off (`run_owner_moved`). Runs
-- recorded before, or claimed from a pane without a tenure, have none.
ALTER TABLE runs ADD COLUMN owner_coordinator_id TEXT;
"#,
    r#"
-- A headless item coordinator's tenure (`todo.next`): no pane binds it; it
-- runs as worker `worker_id`, recorded once that worker started. Tenures of
-- a pane have none.
ALTER TABLE coordinators ADD COLUMN worker_id TEXT;
ALTER TABLE coordinators ADD COLUMN headless INTEGER NOT NULL DEFAULT 0;
-- The headless tenure a worker runs as (`started`'s `coordinator`): an item
-- coordinator, whose questions go to the user. Other workers have none.
ALTER TABLE workers ADD COLUMN coordinates TEXT;
-- Item history gains the item coordinator's start and end
-- (`coordinator_started`, `coordinator_ended`; `text` the outcome) and the
-- tenure each record names. SQLite cannot change a CHECK constraint, so the
-- table is rebuilt with the same rows and ids; dropping it fires no trigger.
CREATE TABLE item_history_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('claimed', 'noted', 'closed', 'aborted', 'blocked',
                        'coordinator_started', 'coordinator_ended')),
    run_id TEXT,
    attempt INTEGER,
    text TEXT,
    item_text TEXT,
    ids_at_claim TEXT NOT NULL DEFAULT '[]',
    follow_ups TEXT NOT NULL DEFAULT '[]',
    ts INTEGER NOT NULL,
    coordinator_id TEXT
);
INSERT INTO item_history_new (id, repo, item, kind, run_id, attempt, text, item_text,
    ids_at_claim, follow_ups, ts)
    SELECT id, repo, item, kind, run_id, attempt, text, item_text, ids_at_claim, follow_ups, ts
    FROM item_history ORDER BY id;
DROP TABLE item_history;
ALTER TABLE item_history_new RENAME TO item_history;
CREATE INDEX item_history_by_item ON item_history (repo, item, id);
CREATE TRIGGER item_history_no_update BEFORE UPDATE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
CREATE TRIGGER item_history_no_delete BEFORE DELETE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
-- `todo.next --continue`: a repository's chain of item coordinators, one
-- row per repository, active while `stopped_ms` is NULL. Its owner is the
-- pane (and agent session, workspace) that started it, which owns each
-- coordinator it starts. `advancing` is set in the transaction that ends a
-- coordinator whose item is done and cleared by the claim of the next one,
-- so a server that starts in between starts the next one. `last_item` and
-- `last_outcome` are the latest coordinator's. Times are Unix milliseconds.
CREATE TABLE todo_chains (
    repo TEXT PRIMARY KEY,
    owner_pane TEXT,
    owner_session TEXT,
    workspace TEXT,
    started_ms INTEGER NOT NULL,
    advancing INTEGER NOT NULL DEFAULT 0,
    last_item TEXT,
    last_outcome TEXT,
    stopped_ms INTEGER,
    stop_reason TEXT
);
"#,
    r#"
-- Protocol problems coordinators report (`report.record`), one row per
-- fingerprint (the kind and the summary with ids, paths, numbers and commit
-- hashes taken out), and one row per occurrence, written with the
-- `report_recorded` and `report_closed` events (whose `worker_id` is
-- `report:r-<id>`) in the same transaction. `count` is the number of
-- occurrences; a closed report's next occurrence reopens it (`reopened`
-- counts those) and clears its closure, which the events keep. Times are
-- Unix milliseconds. `evidence` is the server's copy of the reporter's
-- file, in the worker directory's `reports/`.
CREATE TABLE reports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    normalized TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('open', 'closed')),
    uncertain INTEGER NOT NULL DEFAULT 0,
    first_ms INTEGER NOT NULL,
    last_ms INTEGER NOT NULL,
    count INTEGER NOT NULL,
    reopened INTEGER NOT NULL DEFAULT 0,
    closed_ms INTEGER,
    closed_by TEXT,
    fix TEXT,
    not_reproducible INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE report_occurrences (
    report_id INTEGER NOT NULL REFERENCES reports (id),
    number INTEGER NOT NULL,
    ts INTEGER NOT NULL,
    summary TEXT NOT NULL,
    uncertain INTEGER NOT NULL DEFAULT 0,
    repo TEXT,
    pane_id TEXT,
    session_id TEXT,
    coordinator_id TEXT,
    command TEXT,
    evidence TEXT,
    evidence_source TEXT,
    evidence_truncated INTEGER NOT NULL DEFAULT 0,
    reopened TEXT,
    PRIMARY KEY (report_id, number)
);
"#,
    r#"
-- Item history gains `stopped`: the reason a close named no next item
-- (`todo.resume`'s `stop_reason`), written with the closing run's `done`.
-- The table is rebuilt with the same rows and ids, as above.
CREATE TABLE item_history_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('claimed', 'noted', 'closed', 'aborted', 'blocked',
                        'coordinator_started', 'coordinator_ended', 'stopped')),
    run_id TEXT,
    attempt INTEGER,
    text TEXT,
    item_text TEXT,
    ids_at_claim TEXT NOT NULL DEFAULT '[]',
    follow_ups TEXT NOT NULL DEFAULT '[]',
    ts INTEGER NOT NULL,
    coordinator_id TEXT
);
INSERT INTO item_history_new (id, repo, item, kind, run_id, attempt, text, item_text,
    ids_at_claim, follow_ups, ts, coordinator_id)
    SELECT id, repo, item, kind, run_id, attempt, text, item_text, ids_at_claim, follow_ups, ts,
        coordinator_id
    FROM item_history ORDER BY id;
DROP TABLE item_history;
ALTER TABLE item_history_new RENAME TO item_history;
CREATE INDEX item_history_by_item ON item_history (repo, item, id);
CREATE TRIGGER item_history_no_update BEFORE UPDATE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
CREATE TRIGGER item_history_no_delete BEFORE DELETE ON item_history
BEGIN SELECT RAISE(ABORT, 'item_history is append-only'); END;
"#,
    r#"
-- Queue mode (`todo.queue_set`), one row per repository: whether the server
-- starts the top runnable item of "Next, in order" itself (`mode`), why it
-- is paused, the fencing token every start of the queue claims and bumps in
-- the transaction that creates the run, the consecutive queue runs that
-- ended blocked or aborted (the circuit breaker), the last evaluation's
-- status and reason, and the pane that set the mode, which owns the runs
-- the queue starts. Times are Unix milliseconds.
CREATE TABLE todo_queues (
    repo TEXT PRIMARY KEY,
    mode TEXT NOT NULL CHECK (mode IN ('on', 'paused')),
    pause_reason TEXT,
    token INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    status TEXT,
    reason TEXT,
    owner_pane TEXT,
    owner_session TEXT,
    workspace TEXT,
    updated_ms INTEGER NOT NULL
);
-- Each run the queue started, with the token its start claimed and the
-- digest of the item's text then; `outcome` (done, blocked, aborted) and
-- `error` are set once, when the queue settles the run's end.
CREATE TABLE todo_queue_runs (
    run_id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    item TEXT NOT NULL,
    item_digest TEXT NOT NULL,
    token INTEGER NOT NULL,
    started_ms INTEGER NOT NULL,
    outcome TEXT,
    error TEXT,
    settled_ms INTEGER
);
CREATE INDEX todo_queue_runs_by_item ON todo_queue_runs (repo, item);
"#,
    r#"
-- Calls a coordinator tab's command allowlist refused in shadow mode
-- (`coordinator.allowlist_refusal`), which ran anyway: what enforce mode
-- would have denied. `reason` is the allowlist's; `repo`, `coordinator_id`
-- and `item` are the pane's active tenure then (the repository of the
-- agent's directory when it had none). `ts` is Unix milliseconds.
-- Append-only: the triggers refuse every change.
CREATE TABLE coordinator_would_deny (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    repo TEXT,
    coordinator_id TEXT,
    item TEXT,
    pane_id TEXT NOT NULL,
    session_id TEXT,
    tool TEXT NOT NULL,
    command TEXT NOT NULL,
    reason TEXT NOT NULL
);
CREATE INDEX coordinator_would_deny_by_repo ON coordinator_would_deny (repo, id);
CREATE TRIGGER coordinator_would_deny_no_update BEFORE UPDATE ON coordinator_would_deny
BEGIN SELECT RAISE(ABORT, 'coordinator_would_deny is append-only'); END;
CREATE TRIGGER coordinator_would_deny_no_delete BEFORE DELETE ON coordinator_would_deny
BEGIN SELECT RAISE(ABORT, 'coordinator_would_deny is append-only'); END;
"#,
    super::capabilities::GRANTS_MIGRATION,
    super::inbox::INBOX_MIGRATION,
    super::decisions::DECISIONS_MIGRATION,
    super::inbox::UNOWNED_INBOX_MIGRATION,
    super::capabilities::GRANT_LOG_MIGRATION,
    r#"
-- The worker ended by the exit its owner asked for (a stop or kill sent as
-- the owner): its owner owes no ack for that end. Workers recorded before
-- had such an exit acknowledged on their owner's behalf (`acked_seq`).
ALTER TABLE workers ADD COLUMN end_by_owner INTEGER NOT NULL DEFAULT 0;
"#,
    super::supervision::SUPERVISION_MIGRATION,
    super::supervision::SUPERVISION_INBOX_MIGRATION,
    super::supervision::SUPERVISION_DELIVERY_MIGRATION,
    super::supervision::SUPERVISION_HOLDS_MIGRATION,
    r#"
-- A tenure as the repository's lease (`coordinator.acquire`).
-- `token_sha256` is the sha256 (hex) of the token its holder was last given,
-- which `worker.run` must present; the token itself is never stored. NULL
-- until a token is issued. `holder_pid` and `holder_start` name a process
-- holder: its pid and its start token (`process_start_token`), which a later
-- process reusing the pid cannot match; NULL for a pane's or a headless
-- tenure.
ALTER TABLE coordinators ADD COLUMN token_sha256 TEXT;
ALTER TABLE coordinators ADD COLUMN holder_pid INTEGER;
ALTER TABLE coordinators ADD COLUMN holder_start INTEGER;
"#,
    r#"
-- The usage gate's admissions, for its cap on the workers of one provider
-- in one repository (`[usage] gate_max_workers`, per coordinator, which is
-- one per repository): one row per start
-- the gate admitted, written before the worker is spawned. `holder` is
-- `run:<run id>:<attempt>` for a `todo.run` attempt (`run_id`, `attempt`)
-- or `worker.run:<id>` for a `worker.run`; `worker_id` the worker it
-- started, set before the spawn. Whether a row still holds a slot is
-- derived from the rows it names (its worker's `exited` or `lost`, its
-- run's status and attempt), never counted; `released_ms` and `release`
-- end it for what those rows cannot show (a failed spawn, a refusal, a
-- server gone before the worker was recorded). `server_pid` and
-- `server_start` name the server process that admitted it. `ignored` is an
-- admission `--ignore-usage` made past the gate.
CREATE TABLE usage_admissions (
    holder TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    repo TEXT NOT NULL,
    run_id TEXT,
    attempt INTEGER,
    worker_id TEXT,
    server_pid INTEGER,
    server_start INTEGER,
    ignored INTEGER NOT NULL,
    admitted_ms INTEGER NOT NULL,
    released_ms INTEGER,
    release TEXT
);
CREATE INDEX usage_admissions_by_provider ON usage_admissions (provider, released_ms);
-- The high latch is the provider account's, not a repository's: one row per
-- provider (`provider:claude`, `provider:openai`). Each takes the most
-- restrictive of the provider's former rows (Claude's were keyed by the
-- repository, OpenAI's by `openai:` and the repository): a closed high one,
-- else a closed unknown one, else the latest open one; then those are gone.
INSERT INTO usage_gates (repo, closed, body, updated_ms)
    SELECT 'provider:claude', closed, body, updated_ms FROM usage_gates
    WHERE substr(repo, 1, 7) != 'openai:'
    ORDER BY (closed != 0 AND coalesce(CASE WHEN json_valid(body)
                  THEN json_extract(body, '$.state') END, '') != 'unknown') DESC,
             closed DESC, updated_ms DESC
    LIMIT 1;
INSERT INTO usage_gates (repo, closed, body, updated_ms)
    SELECT 'provider:openai', closed, body, updated_ms FROM usage_gates
    WHERE substr(repo, 1, 7) = 'openai:'
    ORDER BY (closed != 0 AND coalesce(CASE WHEN json_valid(body)
                  THEN json_extract(body, '$.state') END, '') != 'unknown') DESC,
             closed DESC, updated_ms DESC
    LIMIT 1;
DELETE FROM usage_gates WHERE substr(repo, 1, 9) != 'provider:';
"#,
    r#"
-- The real path of the worktree a queue was turned on from, and a chain of
-- item coordinators started from: its starts read TODO.md, the checks and
-- the branch there, as a `todo run` from it would. NULL for one recorded
-- before, which keeps using the repository's checkout.
ALTER TABLE todo_queues ADD COLUMN worktree TEXT;
ALTER TABLE todo_chains ADD COLUMN worktree TEXT;
"#,
    r#"
-- The usage gate reads a lost worker's latest `started` event to compare its
-- process start token; `events_by_worker` would walk that worker's journal
-- backwards to find it. This index seeks straight to the worker's `herdr`
-- events of one type in `seq` order, so a worker's own event volume does not
-- cost the cap's read.
CREATE INDEX events_by_worker_kind ON events (worker_id, direction, type, seq);
"#,
    r#"
-- The step a `todo.run` or a `worker.run` admitted and has not released
-- (`WorkerSupervisor::begin_landing`, `begin_start_step`): `kind` is the step
-- (`cherry-pick`, `install`, `TODO edit`, `push` or `start`), `coordinator_id`
-- and `epoch` the tenure the step was admitted under (none for work claimed
-- outside any tenure), `server_pid` and `server_start` the server that
-- admitted it. The row is written in the transaction that checks the tenure,
-- so a tenure change cannot fall between the check and the step; while one is
-- open its tenure ends only through this table (see `tenure_pending_ends`).
-- `retired_ms` and `retired_by_*` name the server that retired a step whose
-- server is gone, so the row says who released it.
CREATE TABLE tenure_steps (
    id TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    coordinator_id TEXT,
    epoch INTEGER,
    kind TEXT NOT NULL,
    run_id TEXT,
    server_pid INTEGER NOT NULL,
    server_start INTEGER,
    started_ms INTEGER NOT NULL,
    retired_ms INTEGER,
    retired_by_pid INTEGER,
    retired_by_start INTEGER
);
CREATE INDEX tenure_steps_by_coordinator ON tenure_steps (coordinator_id, epoch)
    WHERE retired_ms IS NULL;
-- `coordinator.acquire` and `coordinator.start` read the steps of a run
-- claimed outside any tenure by repository (`unowned_landing_steps`): a claim
-- waits for such a step to land.
CREATE INDEX tenure_steps_by_repo ON tenure_steps (repo)
    WHERE coordinator_id IS NULL AND retired_ms IS NULL;
-- The end of a tenure herdr saw and could not carry out at once (its holder
-- process exited, its pane closed, its agent exited): the reason and cause,
-- written before the end is attempted and deleted only once the tenure has
-- really ended. `epoch` names the tenure the end belongs to, so a stale end
-- cannot end a later tenure of the same id; the apply names it in its WHERE.
-- A reader of the tenures (a claim attempt, a server that starts, the release
-- of a landing step) ends the tenure of every row whose step is gone, so a
-- failed end is retried by the next event that reads it, never by a timer.
CREATE TABLE tenure_pending_ends (
    coordinator_id TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    reason TEXT NOT NULL,
    cause TEXT,
    requested_ms INTEGER NOT NULL,
    PRIMARY KEY (coordinator_id, epoch)
);
-- The tenure that admitted a `worker.run`, written in the transaction of its
-- usage admission: the token check and the row that authorizes the start
-- cannot be split by a tenure change. NULL for a start that no tenure
-- admitted, and for the admissions recorded before.
ALTER TABLE usage_admissions ADD COLUMN tenure_id TEXT;
ALTER TABLE usage_admissions ADD COLUMN tenure_epoch INTEGER;
"#,
    super::supervision::SUPERVISION_ANNOTATIONS_MIGRATION,
    super::supervision::SUPERVISION_FINDING_RULE_VERSION_MIGRATION,
    super::supervision::SUPERVISION_HOLD_EVENT_MIGRATION,
    r#"
-- The agent (`claude`, `pi` or `pi-anthropic`) and the pi model
-- (`provider/model`) the last `todo queue on` named for the runs the queue
-- starts. NULL when it named none, and for a queue recorded before: its runs
-- use `[workers] default_agent` and its defaults.
ALTER TABLE todo_queues ADD COLUMN agent TEXT;
ALTER TABLE todo_queues ADD COLUMN model TEXT;
"#,
    r#"
-- Clients list a done run with a stop reason until the user acknowledges it
-- (`finish.stop_acknowledged`). The runs stopped before that sent their
-- notice under the old rule: they count as acknowledged, so only the stops
-- recorded from now on show.
UPDATE runs SET finish = json_set(finish, '$.stop_acknowledged', json('true'))
    WHERE status = 'done' AND json_valid(finish)
        AND json_extract(finish, '$.stop_reason') IS NOT NULL;
"#,
    r#"
-- A run's landing as its transition core (`runs/core.rs`) holds it, as a
-- JSON object: the core's state with its cursor and the effect it waits
-- for, whether that effect's side effect may have begun and what its
-- intent recorded. Written with the run's row and its event in one
-- transaction. NULL before the run's landing starts, and for the runs
-- recorded before: the driver derives their state from `step`.
ALTER TABLE runs ADD COLUMN landing TEXT;
-- A run at its abort step that still has to take its commit off the target
-- branch keeps its repository's claim until that is done; one that only
-- stops its worker lets it go, as before. A landing that is not valid JSON
-- claims nothing here (`json_extract` would fail on it): the driver blocks
-- that run.
DROP INDEX runs_one_active_per_repo;
CREATE UNIQUE INDEX runs_one_active_per_repo ON runs (repo)
    WHERE status IN ('running', 'waiting')
        AND (step != 'abort'
            OR CASE WHEN json_valid(landing)
                THEN json_extract(landing, '$.core.pending') END IS NOT NULL);
"#,
    r#"
-- Each worker run's agent (`claude`, `pi`, or `pi-anthropic` for pi on the
-- Anthropic API credits), the model it reported at its start
-- (`system/init`'s `model`), its usage per model (`WorkerModelUsage` as a
-- JSON array) and its cost in US dollars, written with the event that
-- reports them: Claude's `result` (its `modelUsage` and `total_cost_usd`
-- cover the session so far) and pi's translated `assistant` reply
-- (`herdr_usage`, added up). NULL while unknown, never 0. Workers recorded
-- before are filled from their `started`, `system/init` and last `result`
-- events; a `result` whose cost is the credit ledger's for a whole run
-- (`credit_accounting`) says nothing about its worker. Their pi replies
-- carried no usage, so their usage stays unknown.
ALTER TABLE workers ADD COLUMN agent TEXT;
ALTER TABLE workers ADD COLUMN ran_model TEXT;
ALTER TABLE workers ADD COLUMN usage TEXT;
ALTER TABLE workers ADD COLUMN cost_usd REAL;
UPDATE workers SET agent = (SELECT CASE
        WHEN json_extract(e.body, '$.agent') = 'pi'
            AND substr(coalesce(json_extract(e.body, '$.model'), ''), 1, 16) = 'herdr-anthropic/'
        THEN 'pi-anthropic' ELSE json_extract(e.body, '$.agent') END
    FROM events e
    WHERE e.worker_id = workers.id AND e.direction = 'herdr' AND e.type = 'started'
      AND json_valid(e.body)
    ORDER BY e.seq LIMIT 1);
UPDATE workers SET ran_model = (SELECT json_extract(e.body, '$.model') FROM events e
    WHERE e.worker_id = workers.id AND e.direction = 'out' AND e.type = 'system'
      AND CASE WHEN json_valid(e.body) THEN json_extract(e.body, '$.subtype') = 'init'
          AND json_type(e.body, '$.model') = 'text' END
    ORDER BY e.seq DESC LIMIT 1);
CREATE TEMP TABLE claude_results AS
SELECT e.worker_id, e.seq, e.body FROM events e
WHERE e.direction = 'out' AND e.type = 'result'
  AND CASE WHEN json_valid(e.body)
      THEN json_extract(e.body, '$.credit_accounting') IS NULL END;
UPDATE workers SET usage = (
    SELECT json_group_array(json_object(
        'model', m.key,
        'input_tokens', coalesce(json_extract(m.value, '$.inputTokens'), 0),
        'output_tokens', coalesce(json_extract(m.value, '$.outputTokens'), 0),
        'cache_read_input_tokens', coalesce(json_extract(m.value, '$.cacheReadInputTokens'), 0),
        'cache_creation_input_tokens',
            coalesce(json_extract(m.value, '$.cacheCreationInputTokens'), 0),
        'cost_usd', json_extract(m.value, '$.costUSD')))
    FROM json_each((SELECT json_extract(r.body, '$.modelUsage') FROM claude_results r
        WHERE r.worker_id = workers.id AND json_type(r.body, '$.modelUsage') = 'object'
        ORDER BY r.seq DESC LIMIT 1)) m)
WHERE EXISTS (SELECT 1 FROM claude_results r
    WHERE r.worker_id = workers.id AND json_type(r.body, '$.modelUsage') = 'object');
UPDATE workers SET cost_usd = (SELECT json_extract(r.body, '$.total_cost_usd')
    FROM claude_results r
    WHERE r.worker_id = workers.id AND json_type(r.body, '$.total_cost_usd') IN ('real', 'integer')
    ORDER BY r.seq DESC LIMIT 1);
DROP TABLE claude_results;
"#,
];

/// The runs that claim their repository, as `runs_one_active_per_repo`
/// names them, over `runs r`.
const CLAIMING_RUN: &str = "r.status IN ('running', 'waiting') \
     AND (r.step != 'abort' OR CASE WHEN json_valid(r.landing) \
         THEN json_extract(r.landing, '$.core.pending') END IS NOT NULL)";

pub(super) type StoreResult<T> = rusqlite::Result<T>;

pub(super) struct Store {
    conn: Mutex<Connection>,
    /// The directory of the runs' lock files ([`run_lock_file`]): the
    /// store's own; none for a store in memory.
    run_locks: Option<PathBuf>,
}

/// The lock file of the run `run_id` in `dir`, which the server that drives
/// the run holds while it does.
pub(super) fn run_lock_file(dir: &Path, run_id: &str) -> PathBuf {
    dir.join(format!("run-{run_id}.lock"))
}

/// One event as it is appended.
pub(super) struct EventRow<'a> {
    pub(super) worker_id: &'a str,
    pub(super) direction: Direction,
    pub(super) record: &'a Recorded<'a>,
    pub(super) ts_ms: u64,
}

/// A client command's receipt as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredReceipt {
    pub(super) method: String,
    pub(super) params: String,
    pub(super) worker_id: Option<String>,
    pub(super) state: ReceiptState,
    pub(super) result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReceiptState {
    /// Reserved, outcome not stored: the command runs, or its server ended
    /// before it finished.
    Pending,
    Accepted,
    Rejected,
}

impl ReceiptState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            _ => Self::Pending,
        }
    }
}

/// A client command id with what it asked, as reserved and settled.
#[derive(Debug, Clone, Copy)]
pub(super) struct NewReceipt<'a> {
    pub(super) command_id: &'a str,
    pub(super) method: &'a str,
    pub(super) params: &'a str,
}

/// What a journal line carries: a JSON event, or a line that was not JSON.
#[derive(Debug, Clone, Copy)]
pub(super) enum Recorded<'a> {
    Event(&'a Value),
    Raw(&'a str),
}

impl Store {
    pub(super) fn open(path: &Path) -> StoreResult<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(LOCK_WAIT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // In WAL mode a server crash loses nothing committed; an OS crash
        // may lose the last commits, never tear one.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            run_locks: path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .map(Path::to_path_buf),
        })
    }

    /// Runs `write` in one write transaction: everything it wrote is
    /// committed, or nothing when it (or the commit) fails.
    ///
    /// A run it ends (done, blocked, aborted) lets go of its lock file
    /// before the commit, so whoever reads the end finds the lock gone. A
    /// commit that fails then leaves the run without its file while its
    /// driver still holds the lock it opened, as a driver that goes on after
    /// the end does.
    pub(super) fn transaction<T>(
        &self,
        write: impl FnOnce(&Tx<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = lock(&self.conn);
        // Immediate: the write lock is taken at the start, so the lock wait
        // applies; a deferred transaction upgraded later fails at once.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ended = RefCell::new(Vec::new());
        let value = write(&Tx {
            tx: &tx,
            ended: &ended,
        })?;
        if let Some(dir) = &self.run_locks {
            for run_id in ended.into_inner() {
                let _ = std::fs::remove_file(run_lock_file(dir, &run_id));
            }
        }
        tx.commit()?;
        Ok(value)
    }

    /// Every worker the projections hold, by number.
    pub(super) fn load_all(&self) -> StoreResult<BTreeMap<u64, Status>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!("SELECT {WORKER_COLUMNS} FROM workers"))?;
        let mut workers = BTreeMap::new();
        for status in statement.query_map([], status_from_row)? {
            let mut status = status?;
            let Some(number) = worker_number(&status.worker_id) else {
                continue;
            };
            load_questions(&conn, &mut status)?;
            load_event_facts(&conn, &mut status)?;
            workers.insert(number, status);
        }
        Ok(workers)
    }

    /// One worker from the projections, if they hold it.
    pub(super) fn load(&self, worker_id: &str) -> StoreResult<Option<Status>> {
        let conn = lock(&self.conn);
        let status = conn
            .query_row(
                &format!("SELECT {WORKER_COLUMNS} FROM workers WHERE id = ?1"),
                [worker_id],
                status_from_row,
            )
            .optional()?;
        match status {
            Some(mut status) => {
                load_questions(&conn, &mut status)?;
                load_event_facts(&conn, &mut status)?;
                Ok(Some(status))
            }
            None => Ok(None),
        }
    }

    /// The receipt of a client command id, if one was stored.
    pub(super) fn receipt(&self, command_id: &str) -> StoreResult<Option<StoredReceipt>> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT method, params, worker_id, state, result FROM receipts WHERE command_id = ?1",
            [command_id],
            |row| {
                Ok(StoredReceipt {
                    method: row.get(0)?,
                    params: row.get(1)?,
                    worker_id: row.get(2)?,
                    state: ReceiptState::parse(&row.get::<_, String>(3)?),
                    result: row.get(4)?,
                })
            },
        )
        .optional()
    }

    /// The response of the latest stored answer to one of a worker's
    /// questions (its `answer_intent`), to send it again after a restart;
    /// `None` when no intent of it holds one (none, or one from before
    /// intents stored their response).
    #[cfg(unix)]
    pub(super) fn answer_response(
        &self,
        worker_id: &str,
        request_id: &str,
    ) -> StoreResult<Option<Value>> {
        let conn = lock(&self.conn);
        let mut intents = conn.prepare(
            "SELECT body FROM events
             WHERE worker_id = ?1 AND direction = 'herdr' AND type = 'answer_intent'
             ORDER BY seq DESC",
        )?;
        let bodies = intents.query_map([worker_id], |row| row.get::<_, String>(0))?;
        for body in bodies {
            let Ok(intent) = serde_json::from_str::<Value>(&body?) else {
                continue;
            };
            if intent["request_id"].as_str() == Some(request_id) {
                return Ok(intent
                    .get("response")
                    .filter(|response| response.is_object())
                    .cloned());
            }
        }
        Ok(None)
    }

    /// The `can_use_tool` and `hook_callback` requests a worker made after
    /// `since` (the turn's first event) that were neither cancelled nor
    /// asked as a question, oldest first, each with the `control_response`
    /// recorded for it, if one was: for a server that re-attaches to settle
    /// what a gone server stored and may not have answered.
    #[cfg(unix)]
    pub(super) fn unasked_requests(
        &self,
        worker_id: &str,
        since: i64,
    ) -> StoreResult<Vec<(Value, Option<Value>)>> {
        let conn = lock(&self.conn);
        let mut events = conn.prepare(
            "SELECT direction, type, body FROM events
             WHERE worker_id = ?1 AND seq > ?2
               AND type IN ('control_request', 'control_response', 'control_cancel_request')
             ORDER BY seq",
        )?;
        let rows = events.query_map(params![worker_id, since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut requests: Vec<(Value, Option<Value>)> = Vec::new();
        for row in rows {
            let (direction, kind, body) = row?;
            let Ok(event) = serde_json::from_str::<Value>(&body) else {
                continue;
            };
            match (direction.as_str(), kind.as_str()) {
                ("out", "control_request")
                    if matches!(
                        event["request"]["subtype"].as_str(),
                        Some("can_use_tool" | "hook_callback")
                    ) =>
                {
                    requests.push((event, None));
                }
                ("in", "control_response") => {
                    let id = event["response"]["request_id"].as_str();
                    if let Some(request) = requests
                        .iter_mut()
                        .find(|(request, _)| request["request_id"].as_str() == id)
                    {
                        request.1 = Some(event);
                    }
                }
                ("out", "control_cancel_request") => {
                    let id = event["request_id"].as_str();
                    requests.retain(|(request, _)| request["request_id"].as_str() != id);
                }
                _ => {}
            }
        }
        let mut asked =
            conn.prepare("SELECT 1 FROM questions WHERE worker_id = ?1 AND request_id = ?2")?;
        let mut unasked = Vec::new();
        for (request, response) in requests {
            let id = request["request_id"].as_str().unwrap_or_default();
            if !asked.exists(params![worker_id, id])? {
                unasked.push((request, response));
            }
        }
        Ok(unasked)
    }

    /// How many events the store holds of one worker.
    pub(super) fn event_count(&self, worker_id: &str) -> StoreResult<i64> {
        let conn = lock(&self.conn);
        conn.query_row(
            "SELECT count(*) FROM events WHERE worker_id = ?1",
            [worker_id],
            |row| row.get(0),
        )
    }

    /// Runs `read` on the connection, for the tables a module of its own
    /// reads (`reports`).
    pub(super) fn read<T>(
        &self,
        read: impl FnOnce(&Connection) -> StoreResult<T>,
    ) -> StoreResult<T> {
        read(&lock(&self.conn))
    }

    #[cfg(test)]
    pub(super) fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        lock(&self.conn)
    }
}

fn migrate(conn: &mut Connection) -> StoreResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )?;
    let version: usize = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse().unwrap_or(usize::MAX))
        .unwrap_or(0);
    if version > MIGRATIONS.len() {
        // Written by a newer herdr: refused rather than misread; the
        // supervisor then runs from the JSONL journals, degraded.
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
            Some(format!(
                "worker store schema version {version} is newer than this herdr's {}",
                MIGRATIONS.len()
            )),
        ));
    }
    for migration in &MIGRATIONS[version..] {
        tx.execute_batch(migration)?;
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [MIGRATIONS.len().to_string()],
    )?;
    tx.commit()
}

/// A write transaction ([`Store::transaction`]).
pub(super) struct Tx<'a> {
    tx: &'a rusqlite::Transaction<'a>,
    /// The runs this transaction ended, whose lock files go before it
    /// commits.
    ended: &'a RefCell<Vec<String>>,
}

impl Tx<'_> {
    /// The transaction's connection, for the tables a module of its own
    /// writes (`reports`).
    pub(super) fn connection(&self) -> &Connection {
        self.tx
    }

    /// Appends one event and returns its `seq`.
    pub(super) fn event(&self, event: &EventRow<'_>) -> StoreResult<i64> {
        let (kind, body) = match event.record {
            Recorded::Event(value) => {
                (value.get("type").and_then(Value::as_str), value.to_string())
            }
            Recorded::Raw(line) => (None, Value::String((*line).to_owned()).to_string()),
        };
        self.tx.execute(
            "INSERT INTO events (worker_id, direction, type, body, ts_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.worker_id,
                event.direction.as_str(),
                kind,
                body,
                event.ts_ms as i64
            ],
        )?;
        Ok(self.tx.last_insert_rowid())
    }

    /// Writes the questions an event asked, began answering or settled:
    /// `before` are the ids open before it, each with whether its answer was
    /// in flight, `status` the state after it.
    ///
    /// Every change is guarded by the state the row had (T3 Code's guard):
    /// a cleanup that cancels open questions (a turn's end, the exit)
    /// settles only rows still `pending`, so it never overwrites an answer
    /// that is `answering` or `answered`, whatever this server's memory
    /// holds; only the answer's own outcome settles an `answering` row.
    pub(super) fn questions(
        &self,
        seq: i64,
        before: &[(String, bool)],
        status: &Status,
    ) -> StoreResult<()> {
        let state_of = |answering: bool| if answering { "answering" } else { "pending" };
        for pending in &status.questions {
            let question = &pending.question;
            let known = before.iter().find(|(id, _)| *id == question.request_id);
            if let (Some(_), Some(escalated)) = (known, &pending.escalated) {
                self.tx.execute(
                    "UPDATE questions SET escalated = ?3
                     WHERE worker_id = ?1 AND request_id = ?2 AND escalated IS NULL",
                    params![status.worker_id, question.request_id, escalated],
                )?;
            }
            match known {
                Some((_, was)) if *was == pending.answering => {}
                Some((_, was)) => {
                    self.tx.execute(
                        "UPDATE questions SET state = ?3
                         WHERE worker_id = ?1 AND request_id = ?2 AND state = ?4",
                        params![
                            status.worker_id,
                            question.request_id,
                            state_of(pending.answering),
                            state_of(*was),
                        ],
                    )?;
                }
                None => {
                    self.tx.execute(
                        "INSERT INTO questions (worker_id, request_id, kind, tool_name, text,
                             state, how, question, input, asked_seq, settled_seq, escalated)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?9, NULL, ?6, ?7, ?8, NULL, ?10)
                         ON CONFLICT (worker_id, request_id) DO UPDATE SET
                             kind = excluded.kind, tool_name = excluded.tool_name,
                             text = excluded.text, state = excluded.state, how = NULL,
                             question = excluded.question, input = excluded.input,
                             asked_seq = excluded.asked_seq, settled_seq = NULL,
                             escalated = excluded.escalated",
                        params![
                            status.worker_id,
                            question.request_id,
                            enum_text(&question.kind),
                            question.tool_name,
                            question.text,
                            serde_json::to_string(question).unwrap_or_else(|_| "{}".into()),
                            pending.input.to_string(),
                            seq,
                            state_of(pending.answering),
                            pending.escalated,
                        ],
                    )?;
                }
            }
        }
        for (request_id, was_answering) in before {
            if status
                .questions
                .iter()
                .any(|pending| pending.question.request_id == *request_id)
            {
                continue;
            }
            let how = status.resolution(request_id).unwrap_or("cancelled");
            self.tx.execute(
                "UPDATE questions SET state = ?3, how = ?4, settled_seq = ?5
                 WHERE worker_id = ?1 AND request_id = ?2 AND state = ?6",
                params![
                    status.worker_id,
                    request_id,
                    question_state(how),
                    how,
                    seq,
                    state_of(*was_answering),
                ],
            )?;
        }
        Ok(())
    }

    /// Reserves a client command's id with its first event: inserted only
    /// if absent. The caller has checked under the registry lock that the
    /// id is new; a row another server wrote meanwhile stays as it is.
    pub(super) fn reserve_receipt(
        &self,
        receipt: &NewReceipt<'_>,
        worker_id: &str,
        ts_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO receipts (command_id, method, params, worker_id, state, created_ms)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5)
             ON CONFLICT (command_id) DO NOTHING",
            params![
                receipt.command_id,
                receipt.method,
                receipt.params,
                worker_id,
                ts_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Stores a command's outcome, reserving its id first when no event of
    /// the command did.
    pub(super) fn settle_receipt(
        &self,
        receipt: &NewReceipt<'_>,
        worker_id: Option<&str>,
        state: ReceiptState,
        result: &str,
        ts_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO receipts (command_id, method, params, worker_id, state, result,
                 created_ms, settled_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT (command_id) DO UPDATE SET
                 worker_id = coalesce(receipts.worker_id, excluded.worker_id),
                 state = excluded.state, result = excluded.result,
                 settled_ms = excluded.settled_ms
             WHERE receipts.state = 'pending'",
            params![
                receipt.command_id,
                receipt.method,
                receipt.params,
                worker_id,
                state.as_str(),
                result,
                ts_ms as i64
            ],
        )?;
        Ok(())
    }

    /// Records `coordinator_started` and its projection: the tenure's row,
    /// with the repository's next epoch, and its first binding. The unique
    /// index refuses a second active tenure of the repository, also one
    /// another server wrote; the caller checks first with
    /// [`Self::active_coordinator`] to refuse it by name.
    pub(super) fn coordinator_started(
        &self,
        tenure: &NewTenure<'_>,
        at_ms: u64,
    ) -> StoreResult<StoredTenure> {
        let epoch: i64 = self.tx.query_row(
            "SELECT coalesce(max(epoch), 0) + 1 FROM coordinators WHERE repo = ?1",
            [tenure.repo],
            |row| row.get(0),
        )?;
        let headless = tenure.pane_id.is_none() && tenure.holder.is_none();
        let event = serde_json::json!({
            "type": "coordinator_started",
            "coordinator_id": tenure.id,
            "repo": tenure.repo,
            "pane_id": tenure.pane_id,
            "session_id": tenure.session_id,
            "epoch": epoch,
            "headless": headless,
            "holder_pid": tenure.holder.map(|holder| holder.pid),
        });
        self.coordinator_event(tenure.id, &event, at_ms)?;
        self.tx.execute(
            "INSERT INTO coordinators (id, repo, item, started_at, ended_at, end_reason, epoch,
                 headless, holder_pid, holder_start)
             VALUES (?1, ?2, NULL, ?3, NULL, NULL, ?4, ?5, ?6, ?7)",
            params![
                tenure.id,
                tenure.repo,
                at_ms as i64,
                epoch,
                i64::from(headless),
                tenure.holder.map(|holder| i64::from(holder.pid)),
                tenure.holder.map(|holder| holder.start as i64),
            ],
        )?;
        if let Some(pane_id) = tenure.pane_id {
            self.tx.execute(
                "INSERT INTO coordinator_bindings (coordinator_id, pane_id, session_id, from_at,
                     to_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![tenure.id, pane_id, tenure.session_id, at_ms as i64],
            )?;
        }
        tenure_by_id(self.tx, tenure.id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Records `coordinator_ended` with its reason (and `cause`, what herdr
    /// saw) and ends the tenure and its binding; nothing when it has ended
    /// already. Returns the tenure as it is now.
    pub(super) fn coordinator_ended(
        &self,
        id: &str,
        reason: &str,
        cause: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<Option<StoredTenure>> {
        let Some(tenure) = tenure_by_id(self.tx, id)? else {
            return Ok(None);
        };
        self.coordinator_ended_at(id, tenure.epoch, reason, cause, at_ms)
    }

    /// [`Self::coordinator_ended`] for the tenure `(id, epoch)`: an epoch the
    /// tenure does not have now is stale and ends nothing ([`PendingEnd`]).
    pub(super) fn coordinator_ended_at(
        &self,
        id: &str,
        epoch: i64,
        reason: &str,
        cause: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<Option<StoredTenure>> {
        let Some(tenure) = tenure_by_id(self.tx, id)? else {
            return Ok(None);
        };
        if tenure.epoch != epoch {
            return Ok(None);
        }
        if tenure.ended_at.is_some() {
            return Ok(Some(tenure));
        }
        let event = serde_json::json!({
            "type": "coordinator_ended",
            "coordinator_id": id,
            "reason": reason,
            "cause": cause,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET ended_at = ?3, end_reason = ?4
             WHERE id = ?1 AND epoch = ?2 AND ended_at IS NULL",
            params![id, epoch, at_ms as i64, reason],
        )?;
        self.tx.execute(
            "UPDATE coordinator_bindings SET to_at = ?2
             WHERE coordinator_id = ?1 AND to_at IS NULL",
            params![id, at_ms as i64],
        )?;
        tenure_by_id(self.tx, id)
    }

    /// The repository's active tenure, as this transaction sees it.
    pub(super) fn active_coordinator(&self, repo: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(self.tx, "t.repo = ?1 AND t.ended_at IS NULL", [repo])?.pop())
    }

    /// [`tenure_token_check`] in this transaction, which also records the
    /// work the check admits.
    pub(super) fn tenure_token_check(
        &self,
        repo: &str,
        token: Option<&str>,
    ) -> StoreResult<Result<Option<(String, i64)>, String>> {
        tenure_token_check(self.tx, repo, token)
    }

    /// [`tenure_fence_check`] in this transaction.
    pub(super) fn tenure_fence(
        &self,
        repo: &str,
        owner: Option<&str>,
    ) -> StoreResult<Result<(), String>> {
        tenure_fence_check(self.tx, repo, owner)
    }

    /// Records a landing step `step` the transaction admitted, in the same
    /// transaction as its fence check: from here the step's tenure ends only
    /// through [`Self::settle_pending_ends`].
    pub(super) fn tenure_step(&self, step: &NewTenureStep<'_>) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO tenure_steps (id, repo, coordinator_id, epoch, kind, run_id,
                 server_pid, server_start, started_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                step.id,
                step.repo,
                step.coordinator.map(|(id, _)| id),
                step.coordinator.map(|(_, epoch)| epoch),
                step.kind,
                step.run_id,
                step.server.0,
                step.server.1.map(|start| start as i64),
                step.at_ms as i64,
            ],
        )?;
        Ok(())
    }

    /// Releases landing step `id` and carries out the pending ends it was
    /// holding back ([`Self::settle_pending_ends`]): the tenure of a step
    /// that just landed can end from now on.
    pub(super) fn tenure_step_released(
        &self,
        id: &str,
        at_ms: u64,
    ) -> StoreResult<Vec<StoredTenure>> {
        self.tx
            .execute("DELETE FROM tenure_steps WHERE id = ?1", [id])?;
        self.settle_pending_ends(at_ms)
    }

    /// The landing steps of `repo` admitted under no tenure, by kind: a run
    /// claimed while no tenure was active may be landing one, and a claim
    /// waits for it (see [`WorkerSupervisor::begin_landing`]). The query uses
    /// the partial index `tenure_steps_by_repo`.
    pub(super) fn unowned_landing_steps(&self, repo: &str) -> StoreResult<Vec<String>> {
        let mut statement = self.tx.prepare(
            "SELECT kind FROM tenure_steps WHERE repo = ?1 AND coordinator_id IS NULL
                 AND retired_ms IS NULL
             ORDER BY started_ms, kind",
        )?;
        let mut kinds: Vec<String> = statement
            .query_map([repo], |row| row.get(0))?
            .collect::<StoreResult<_>>()?;
        kinds.dedup();
        Ok(kinds)
    }

    /// The landing steps tenure `(coordinator_id, epoch)` admitted and has
    /// not released, by kind, the first first. The epoch is part of the key:
    /// a step of an earlier tenure cannot hold a later one back.
    pub(super) fn open_tenure_steps(
        &self,
        coordinator_id: &str,
        epoch: i64,
    ) -> StoreResult<Vec<String>> {
        let mut statement = self.tx.prepare(
            "SELECT kind FROM tenure_steps WHERE coordinator_id = ?1 AND epoch = ?2
                 AND retired_ms IS NULL
             ORDER BY started_ms, kind",
        )?;
        let mut kinds: Vec<String> = statement
            .query_map(params![coordinator_id, epoch], |row| row.get(0))?
            .collect::<StoreResult<_>>()?;
        kinds.dedup();
        Ok(kinds)
    }

    /// Records the end of a tenure herdr saw ([`PendingEnd`]), keeping an
    /// earlier request of the same tenure and epoch: the first observation is
    /// its cause, and a repeated one changes nothing.
    pub(super) fn request_tenure_end(&self, end: &PendingEnd) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO tenure_pending_ends (coordinator_id, epoch, reason, cause, requested_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (coordinator_id, epoch) DO NOTHING",
            params![
                end.coordinator_id,
                end.epoch,
                end.reason,
                end.cause,
                end.requested_ms as i64
            ],
        )?;
        Ok(())
    }

    /// The ends herdr saw and has not carried out yet.
    pub(super) fn pending_ends(&self) -> StoreResult<Vec<PendingEnd>> {
        pending_ends_where(self.tx, "", &[])
    }

    /// Forgets the pending ends of tenure `coordinator_id`, whatever their
    /// epoch: a tenure that comes back is not ending.
    pub(super) fn clear_pending_end(&self, coordinator_id: &str) -> StoreResult<()> {
        self.tx.execute(
            "DELETE FROM tenure_pending_ends WHERE coordinator_id = ?1",
            [coordinator_id],
        )?;
        Ok(())
    }

    /// Carries out the pending ends whose tenure has no landing step in
    /// flight, in `at_ms`: each tenure ends with the recorded reason and
    /// cause, and its pending row goes in the same transaction that applies
    /// it, so a failed delete cannot end a later tenure. The end is applied
    /// with `WHERE coordinator_id = ? AND epoch = ? AND ended_at IS NULL`
    /// against the tenure actually active: a stale end recorded for an old
    /// epoch ends nothing. Returns the tenures that ended. A pending end whose
    /// step is still in flight stays: the step lands under its own tenure, and
    /// its release calls this again.
    pub(super) fn settle_pending_ends(&self, at_ms: u64) -> StoreResult<Vec<StoredTenure>> {
        let mut ended = Vec::new();
        for end in self.pending_ends()? {
            if !self
                .open_tenure_steps(&end.coordinator_id, end.epoch)?
                .is_empty()
            {
                continue;
            }
            let applied = self.coordinator_ended_at(
                &end.coordinator_id,
                end.epoch,
                &end.reason,
                end.cause.as_deref(),
                at_ms,
            )?;
            self.tx.execute(
                "DELETE FROM tenure_pending_ends WHERE coordinator_id = ?1 AND epoch = ?2",
                params![end.coordinator_id, end.epoch],
            )?;
            if let Some(tenure) = applied {
                ended.push(tenure);
            }
        }
        Ok(ended)
    }

    /// Retires landing steps `ids`, none of whose servers can release them any
    /// more, recording the server that retires them, and carries out the
    /// pending ends that leaves (`at_ms`); returns the tenures that ended.
    pub(super) fn retire_tenure_steps(
        &self,
        ids: &[String],
        at_ms: u64,
        retired_by: (u32, Option<u64>),
    ) -> StoreResult<Vec<StoredTenure>> {
        for id in ids {
            self.tx.execute(
                "UPDATE tenure_steps SET retired_ms = ?2, retired_by_pid = ?3, retired_by_start = ?4
                 WHERE id = ?1 AND retired_ms IS NULL",
                params![
                    id,
                    at_ms as i64,
                    retired_by.0,
                    retired_by.1.map(|start| start as i64)
                ],
            )?;
        }
        self.settle_pending_ends(at_ms)
    }

    /// The active tenure bound to `pane_id`, as this transaction sees it.
    pub(super) fn coordinator_of_pane(&self, pane_id: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(self.tx, ACTIVE_OF_PANE, [pane_id])?.pop())
    }

    /// Records `coordinator_item` and sets tenure `id`'s current item
    /// (none clears it); nothing when the tenure has ended or already has
    /// that item.
    pub(super) fn coordinator_item(
        &self,
        id: &str,
        item: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<()> {
        let Some(tenure) = tenure_by_id(self.tx, id)? else {
            return Ok(());
        };
        if tenure.ended_at.is_some() || tenure.item.as_deref() == item {
            return Ok(());
        }
        let event = serde_json::json!({
            "type": "coordinator_item",
            "coordinator_id": id,
            "item": item,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET item = ?2 WHERE id = ?1",
            params![id, item],
        )?;
        Ok(())
    }

    /// The tenure agent session `session_id` coordinated, which comes back
    /// with it: the active one whose latest binding has that session, else
    /// one that ended `orphaned` (its pane closed or its agent exited) with
    /// that session last, while it is its repository's latest tenure and
    /// the repository has no active one.
    pub(super) fn resumable_coordinator(
        &self,
        session_id: &str,
    ) -> StoreResult<Option<StoredTenure>> {
        let candidates = tenures(
            self.tx,
            "b.session_id = ?1 AND (t.ended_at IS NULL OR (t.end_reason = ?2 \
             AND t.epoch = (SELECT max(epoch) FROM coordinators WHERE repo = t.repo)))",
            params![session_id, super::coordinators::ORPHANED],
        )?;
        Ok(candidates
            .iter()
            .find(|tenure| tenure.ended_at.is_none())
            .or_else(|| candidates.last())
            .cloned())
    }

    /// Records `coordinator_resumed` and moves tenure `id`'s binding to
    /// `pane_id` and `session_id`: its open binding ends and a new one
    /// starts, and an orphaned tenure is active again. The tenure stays
    /// the same. The caller checks that the pane is free and, for an
    /// orphaned tenure, that its repository has no active one.
    pub(super) fn coordinator_resumed(
        &self,
        id: &str,
        pane_id: &str,
        session_id: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<StoredTenure> {
        let tenure = tenure_by_id(self.tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let event = serde_json::json!({
            "type": "coordinator_resumed",
            "coordinator_id": id,
            "from_pane": tenure.pane_id,
            "from_session": tenure.session_id,
            "to_pane": pane_id,
            "session_id": session_id,
            "reopened": tenure.ended_at.is_some(),
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET ended_at = NULL, end_reason = NULL WHERE id = ?1",
            [id],
        )?;
        // An end herdr saw and never carried out is void: the tenure is back.
        self.clear_pending_end(id)?;
        self.rebind(id, pane_id, session_id, at_ms)?;
        tenure_by_id(self.tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Records `coordinator_session_cleared` and moves tenure `id`'s binding
    /// in its pane `pane_id` from agent session `from_session` to
    /// `to_session`, the one the agent cleared it into (`/clear`): its open
    /// binding ends and a new one starts. The caller checks that the tenure
    /// is active and bound to that pane and session.
    pub(super) fn coordinator_session_cleared(
        &self,
        id: &str,
        pane_id: &str,
        from_session: Option<&str>,
        to_session: &str,
        at_ms: u64,
    ) -> StoreResult<StoredTenure> {
        let event = serde_json::json!({
            "type": "coordinator_session_cleared",
            "coordinator_id": id,
            "pane_id": pane_id,
            "from_session": from_session,
            "session_id": to_session,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.rebind(id, pane_id, Some(to_session), at_ms)?;
        tenure_by_id(self.tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Ends tenure `id`'s open binding and starts one to `pane_id`.
    fn rebind(
        &self,
        id: &str,
        pane_id: &str,
        session_id: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "UPDATE coordinator_bindings SET to_at = ?2
             WHERE coordinator_id = ?1 AND to_at IS NULL",
            params![id, at_ms as i64],
        )?;
        self.tx.execute(
            "INSERT INTO coordinator_bindings (coordinator_id, pane_id, session_id, from_at, to_at)
             VALUES (?1, ?2, ?3, ?4, NULL)",
            params![id, pane_id, session_id, at_ms as i64],
        )?;
        Ok(())
    }

    /// Hands active tenure `from` over to a new tenure `to` of the same
    /// repository: `from` ends `handed_off`, `to` starts with the next
    /// epoch and `from`'s current item, and a `handoff` event (on `from`)
    /// names both. Returns both as they are now.
    pub(super) fn coordinator_handoff(
        &self,
        from: &StoredTenure,
        to: &NewTenure<'_>,
        at_ms: u64,
    ) -> StoreResult<(StoredTenure, StoredTenure)> {
        let cause = format!(
            "handed off to {} in pane {}",
            to.id,
            to.pane_id.unwrap_or("none")
        );
        let ended = self
            .coordinator_ended(
                &from.id,
                super::coordinators::HANDED_OFF,
                Some(&cause),
                at_ms,
            )?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        let started = self.coordinator_started(to, at_ms)?;
        // The end that was carried out here is the only one: a pending end
        // recorded for the tenure before it has nothing left to end.
        self.clear_pending_end(&from.id)?;
        let event = serde_json::json!({
            "type": "handoff",
            "coordinator_id": from.id,
            "to_coordinator_id": started.id,
            "repo": from.repo,
            "from_pane": from.pane_id,
            "from_session": from.session_id,
            "to_pane": to.pane_id,
            "to_session": to.session_id,
            "from_epoch": from.epoch,
            "epoch": started.epoch,
            "item": from.item,
        });
        self.coordinator_event(&from.id, &event, at_ms)?;
        self.coordinator_item(&started.id, from.item.as_deref(), at_ms)?;
        let started =
            tenure_by_id(self.tx, &started.id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok((ended, started))
    }

    /// Tenure `id` as this transaction sees it.
    pub(super) fn coordinator(&self, id: &str) -> StoreResult<Option<StoredTenure>> {
        tenure_by_id(self.tx, id)
    }

    /// Records `coordinator_token` and makes `token_sha256` tenure `id`'s
    /// current token: the one its holder was given last, which replaces any
    /// earlier one. The event names no token or hash.
    pub(super) fn coordinator_token(
        &self,
        id: &str,
        token_sha256: &str,
        at_ms: u64,
    ) -> StoreResult<StoredTenure> {
        let event = serde_json::json!({
            "type": "coordinator_token",
            "coordinator_id": id,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET token_sha256 = ?2 WHERE id = ?1",
            params![id, token_sha256],
        )?;
        tenure_by_id(self.tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Records `coordinator_worker`: headless tenure `id` runs as worker
    /// `worker_id` from now on.
    pub(super) fn coordinator_worker(
        &self,
        id: &str,
        worker_id: &str,
        at_ms: u64,
    ) -> StoreResult<()> {
        let event = serde_json::json!({
            "type": "coordinator_worker",
            "coordinator_id": id,
            "worker_id": worker_id,
        });
        self.coordinator_event(id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE coordinators SET worker_id = ?2 WHERE id = ?1",
            params![id, worker_id],
        )?;
        Ok(())
    }

    /// The run in progress of `repo`, if any, as this transaction sees it.
    pub(super) fn active_run(&self, repo: &str) -> StoreResult<Option<Run>> {
        self.tx
            .query_row(
                &run_select(&format!("WHERE r.repo = ?1 AND {CLAIMING_RUN}")),
                [repo],
                run_from_row,
            )
            .optional()
    }

    /// `repo`'s chain of item coordinators, active or stopped.
    pub(super) fn chain(&self, repo: &str) -> StoreResult<Option<StoredChain>> {
        chain_of(self.tx, repo)
    }

    /// Starts `repo`'s chain from `worktree`, owned by `owner`, or keeps the
    /// active one with its owner and worktree; returns it.
    pub(super) fn chain_started(
        &self,
        repo: &str,
        worktree: &str,
        owner: &RunOwner<'_>,
        at_ms: u64,
    ) -> StoreResult<StoredChain> {
        if chain_of(self.tx, repo)?.is_some_and(|chain| chain.stopped_ms.is_none()) {
            return chain_of(self.tx, repo)?.ok_or(rusqlite::Error::QueryReturnedNoRows);
        }
        self.tx.execute(
            "INSERT INTO todo_chains (repo, owner_pane, owner_session, workspace, started_ms,
                 advancing, last_item, last_outcome, stopped_ms, stop_reason, worktree)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, NULL, NULL, NULL, ?6)
             ON CONFLICT (repo) DO UPDATE SET owner_pane = excluded.owner_pane,
                 owner_session = excluded.owner_session, workspace = excluded.workspace,
                 started_ms = excluded.started_ms, advancing = 0, last_item = NULL,
                 last_outcome = NULL, stopped_ms = NULL, stop_reason = NULL,
                 worktree = excluded.worktree",
            params![
                repo,
                owner.pane_id,
                owner.session_id,
                owner.workspace,
                at_ms as i64,
                worktree
            ],
        )?;
        chain_of(self.tx, repo)?.ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    /// Records the latest item coordinator of `repo`'s active chain: its
    /// item and outcome, and whether the next one is to start.
    pub(super) fn chain_outcome(
        &self,
        repo: &str,
        item: &str,
        outcome: &str,
        advancing: bool,
    ) -> StoreResult<()> {
        self.tx.execute(
            "UPDATE todo_chains SET last_item = ?2, last_outcome = ?3, advancing = ?4
             WHERE repo = ?1 AND stopped_ms IS NULL",
            params![repo, item, outcome, i64::from(advancing)],
        )?;
        Ok(())
    }

    /// The next item coordinator of `repo`'s chain claimed its tenure.
    pub(super) fn chain_advanced(&self, repo: &str) -> StoreResult<()> {
        self.tx.execute(
            "UPDATE todo_chains SET advancing = 0 WHERE repo = ?1",
            [repo],
        )?;
        Ok(())
    }

    /// Stops `repo`'s active chain with `reason`; returns it as it is now,
    /// none when the repository never had one.
    pub(super) fn chain_stopped(
        &self,
        repo: &str,
        reason: &str,
        at_ms: u64,
    ) -> StoreResult<Option<StoredChain>> {
        self.tx.execute(
            "UPDATE todo_chains SET stopped_ms = ?2, stop_reason = ?3, advancing = 0
             WHERE repo = ?1 AND stopped_ms IS NULL",
            params![repo, at_ms as i64, reason],
        )?;
        chain_of(self.tx, repo)
    }

    fn coordinator_event(&self, id: &str, event: &Value, at_ms: u64) -> StoreResult<i64> {
        self.event(&EventRow {
            worker_id: id,
            direction: super::Direction::Herdr,
            record: &Recorded::Event(event),
            ts_ms: at_ms,
        })
    }

    /// Writes the worker's projection row as of event `seq`.
    pub(super) fn worker(&self, status: &Status, seq: i64) -> StoreResult<()> {
        let tool_sessions: Vec<(u32, Option<u64>)> = status
            .tool_sessions
            .iter()
            .map(|(session, start)| (*session, *start))
            .collect();
        self.tx.execute(
            &format!(
                "INSERT INTO workers ({WORKER_COLUMNS}) VALUES (
                    :id, :name, :cwd, :workspace_id, :model, :slot, :state, :pid,
                    :session_id, :turns, :last_result, :rate_limit, :tool_sessions,
                    :exit_code, :exit_signal, :stop_requested_ms, :takeover_ms,
                    :takeover_tab, :takeover_error, :takeover_unfinished, :refusal,
                    :exited, :lost, :end_note, :last_seq, :turn_seq, :turn_end_seq,
                    :gone_seq, :owner_pane, :owner_session, :acked_seq, :owner_gone, :degraded,
                    :item, :repo, :started_ms, :ended_ms, :done_commits, :questions_asked,
                    :ended_mid_turn, :item_title, :verification, :takeover_id,
                    :owner_coordinator_id, :broker, :broker_seq, :continuity_gap,
                    :coordinates, :end_by_owner, :agent, :ran_model, :usage, :cost_usd)
                 ON CONFLICT (id) DO UPDATE SET {}",
                WORKER_COLUMNS
                    .split(", ")
                    .filter(|column| *column != "id")
                    .map(|column| match column {
                        // Only grows, whichever server writes the row.
                        "acked_seq" =>
                            "acked_seq = max(workers.acked_seq, excluded.acked_seq)".to_owned(),
                        "broker_seq" =>
                            "broker_seq = max(workers.broker_seq, excluded.broker_seq)".to_owned(),
                        _ => format!("{column} = excluded.{column}"),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            named_params! {
                ":id": status.worker_id,
                ":name": status.name,
                ":cwd": status.cwd,
                ":workspace_id": status.workspace_id,
                ":model": status.model,
                ":slot": status.slot,
                ":state": enum_text(&status.state),
                ":pid": status.pid,
                ":session_id": status.session_id,
                ":turns": status.turns,
                ":last_result": status
                    .last_result
                    .as_ref()
                    .and_then(|result| serde_json::to_string(result).ok()),
                ":rate_limit": status.rate_limit.as_ref().map(Value::to_string),
                ":tool_sessions": serde_json::to_string(&tool_sessions)
                    .unwrap_or_else(|_| "[]".into()),
                ":exit_code": status.exit_code,
                ":exit_signal": status.exit_signal,
                ":stop_requested_ms": status.stop_requested_ms.map(|ms| ms as i64),
                ":takeover_ms": status.takeover_ms.map(|ms| ms as i64),
                ":takeover_tab": status.takeover_tab,
                ":takeover_error": status.takeover_error,
                ":takeover_unfinished": status.takeover_unfinished,
                ":refusal": status.refusal,
                ":exited": status.exited,
                ":lost": status.lost,
                ":end_note": status.end_note,
                ":last_seq": seq,
                ":turn_seq": status.turn_seq,
                ":turn_end_seq": status.turn_end_seq,
                ":gone_seq": status.gone_seq,
                ":owner_pane": status.owner_pane,
                ":owner_session": status.owner_session,
                ":acked_seq": status.acked_seq,
                ":owner_gone": status.owner_gone,
                ":degraded": status.degraded,
                ":item": status.item,
                ":repo": status.repo,
                ":started_ms": status.started_ms.map(|ms| ms as i64),
                ":ended_ms": status.ended_ms.map(|ms| ms as i64),
                ":done_commits": serde_json::to_string(&status.done_commits)
                    .unwrap_or_else(|_| "[]".into()),
                ":questions_asked": status.questions_asked,
                ":ended_mid_turn": status.ended_mid_turn,
                ":item_title": status.item_title,
                ":verification": status
                    .verification
                    .as_ref()
                    .and_then(|verification| serde_json::to_string(verification).ok()),
                ":takeover_id": status.takeover_id,
                ":owner_coordinator_id": status.owner_coordinator,
                ":broker": status
                    .broker
                    .as_ref()
                    .and_then(|broker| serde_json::to_string(broker).ok()),
                ":broker_seq": status.broker_seq as i64,
                ":continuity_gap": status.continuity_gap,
                ":coordinates": status.coordinates,
                ":end_by_owner": status.end_by_owner,
                ":agent": status.agent,
                ":ran_model": status.ran_model,
                ":usage": status
                    .usage
                    .as_ref()
                    .and_then(|usage| serde_json::to_string(usage).ok()),
                ":cost_usd": status.cost_usd,
            },
        )?;
        Ok(())
    }
}

/// A repository's chain of item coordinators (`todo_chains`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredChain {
    pub(super) repo: String,
    pub(super) owner_pane: Option<String>,
    pub(super) owner_session: Option<String>,
    pub(super) workspace: Option<String>,
    pub(super) started_ms: u64,
    pub(super) advancing: bool,
    pub(super) last_item: Option<String>,
    pub(super) last_outcome: Option<String>,
    pub(super) stopped_ms: Option<u64>,
    pub(super) stop_reason: Option<String>,
    /// The real path of the worktree the chain was started from, where its
    /// coordinators read TODO.md and start their runs; none for a chain
    /// started before chains stored it (the repository's checkout).
    pub(super) worktree: Option<String>,
}

fn chain_of(conn: &Connection, repo: &str) -> StoreResult<Option<StoredChain>> {
    conn.query_row(
        "SELECT repo, owner_pane, owner_session, workspace, started_ms, advancing, last_item,
             last_outcome, stopped_ms, stop_reason, worktree
         FROM todo_chains WHERE repo = ?1",
        [repo],
        |row| {
            Ok(StoredChain {
                repo: row.get(0)?,
                owner_pane: row.get(1)?,
                owner_session: row.get(2)?,
                workspace: row.get(3)?,
                started_ms: row.get::<_, i64>(4)? as u64,
                advancing: row.get::<_, i64>(5)? != 0,
                last_item: row.get(6)?,
                last_outcome: row.get(7)?,
                stopped_ms: row.get::<_, Option<i64>>(8)?.map(|ms| ms as u64),
                stop_reason: row.get(9)?,
                worktree: row.get(10)?,
            })
        },
    )
    .optional()
}

/// Who a run moves to ([`Tx::move_run_owner`]).
pub(super) struct RunOwner<'a> {
    pub(super) pane_id: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    pub(super) workspace: Option<&'a str>,
    pub(super) coordinator_id: Option<&'a str>,
}

/// A tenure as `coordinator.start` (or `coordinator.acquire`) asks for it.
pub(super) struct NewTenure<'a> {
    pub(super) id: &'a str,
    pub(super) repo: &'a str,
    /// None for a headless item coordinator's tenure, which no pane binds,
    /// and for a process holder's.
    pub(super) pane_id: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    /// The process that holds the tenure when no pane does.
    pub(super) holder: Option<HolderProcess>,
}

/// One admission of the usage gate (`usage_admissions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Admission {
    pub(super) holder: String,
    pub(super) repo: String,
    pub(super) run_id: Option<String>,
    pub(super) attempt: Option<u32>,
    pub(super) worker_id: Option<String>,
    pub(super) server_pid: Option<u32>,
    pub(super) server_start: Option<u64>,
    pub(super) provider: String,
    pub(super) admitted_ms: u64,
}

/// Whether [`Store::bind_admission`] named the admission's worker, or found
/// it already ended (retired by a handoff's reconciliation, or by a failed
/// spawn): a start in that state must not spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindOutcome {
    Bound,
    Ended,
}

/// An admission as the gate writes it.
pub(super) struct NewAdmission<'a> {
    pub(super) holder: &'a str,
    pub(super) provider: &'a str,
    pub(super) repo: &'a str,
    pub(super) run: Option<(&'a str, u32)>,
    pub(super) server: (u32, Option<u64>),
    pub(super) ignored: bool,
    pub(super) at_ms: u64,
    /// The tenure whose token the admission's transaction checked, which the
    /// row records so the start it authorizes belongs to that tenure even
    /// when the tenure changes afterwards.
    pub(super) tenure: Option<(&'a str, i64)>,
}

const ADMISSION_COLUMNS: &str =
    "a.holder, a.repo, a.run_id, a.attempt, a.worker_id, a.server_pid, a.server_start, a.provider, a.admitted_ms";

fn admission_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Admission> {
    Ok(Admission {
        holder: row.get(0)?,
        repo: row.get(1)?,
        run_id: row.get(2)?,
        attempt: row.get(3)?,
        worker_id: row.get(4)?,
        server_pid: row.get(5)?,
        server_start: row.get::<_, Option<i64>>(6)?.map(|start| start as u64),
        provider: row.get(7)?,
        admitted_ms: row.get::<_, i64>(8)? as u64,
    })
}

/// [`held_admissions`]'s query, which a test asks SQLite's plan for.
fn held_admissions_sql() -> String {
    format!(
        "SELECT {ADMISSION_COLUMNS}, w.lost, w.pid,
             CASE WHEN w.lost = 1 THEN
                 (SELECT json_extract(e.body, '$.process_start') FROM events e
                  WHERE e.worker_id = w.id AND e.direction = 'herdr' AND e.type = 'started'
                  ORDER BY e.seq DESC LIMIT 1)
             END
         FROM usage_admissions a
         LEFT JOIN workers w ON w.id = a.worker_id
         LEFT JOIN runs r ON r.id = a.run_id
         WHERE a.provider = ?1 AND a.repo = ?2 AND a.released_ms IS NULL
           AND CASE
               WHEN a.worker_id IS NOT NULL THEN w.id IS NULL OR w.exited = 0
               WHEN a.run_id IS NOT NULL THEN coalesce(r.status IN ('running', 'waiting')
                   AND r.attempt <= a.attempt, 0)
               ELSE 1
           END
         ORDER BY a.admitted_ms, a.holder"
    )
}

/// The admissions of `provider` in `repo` that hold a slot, oldest first:
/// not ended, and, by what the rows they name show now, one whose worker has
/// not exited (a lost worker counts while its process lives; a worker the
/// store does not hold yet is being spawned), one of a run in progress
/// whose worker is not started yet, at that attempt or a later one admitted
/// for a retry, or a `worker.run` not yet bound to its worker.
fn held_admissions(conn: &Connection, provider: &str, repo: &str) -> StoreResult<Vec<Admission>> {
    let mut statement = conn.prepare(&held_admissions_sql())?;
    let rows = statement.query_map([provider, repo], |row| {
        let admission = admission_from_row(row)?;
        let lost = row.get::<_, Option<bool>>(9)?.unwrap_or(false);
        let pid = row.get::<_, Option<u32>>(10)?;
        let start = row.get::<_, Option<i64>>(11)?.map(|start| start as u64);
        Ok((admission, lost, pid, start))
    })?;
    let mut held = Vec::new();
    for row in rows {
        let (admission, lost, pid, start) = row?;
        // The start token tells this worker's process from a later process
        // that got its pid, which `events_by_worker_kind` leads straight to.
        // A journal from a build before the token, and a worker whose start
        // event carried none, have only the pid: while any process of the
        // group led by it lives, the slot is kept, so a process group that
        // reused the pid keeps it too (a documented over-count, never a
        // slot released under a worker that still runs).
        let alive = || {
            pid.is_some_and(|pid| match start {
                Some(start) => crate::platform::process_start_token(pid) == Some(start),
                None => crate::platform::process_group_alive(pid),
            })
        };
        if !lost || alive() {
            held.push(admission);
        }
    }
    Ok(held)
}

impl Tx<'_> {
    /// [`held_admissions`] in this transaction.
    pub(super) fn held_admissions(
        &self,
        provider: &str,
        repo: &str,
    ) -> StoreResult<Vec<Admission>> {
        held_admissions(self.tx, provider, repo)
    }

    /// The tenure an unreleased admission of `holder` was written under, as
    /// this transaction sees it: `Some(None)` for a start admitted while no
    /// tenure was active, `None` when the admission is gone. A `worker.run`
    /// reads it at the spawn ([`crate::workers::supervisor::gated_run`]) to
    /// re-check the tenure the admission named, before the process exists.
    pub(super) fn admission_tenure(
        &self,
        holder: &str,
    ) -> StoreResult<Option<Option<(String, i64)>>> {
        let row: Option<(Option<String>, Option<i64>)> = self
            .tx
            .query_row(
                "SELECT tenure_id, tenure_epoch FROM usage_admissions
                 WHERE holder = ?1 AND released_ms IS NULL",
                [holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(id, epoch)| id.zip(epoch)))
    }

    /// Writes an admission, replacing an ended one of the same holder.
    pub(super) fn admit(&self, admission: &NewAdmission<'_>) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO usage_admissions (holder, provider, repo, run_id, attempt, worker_id,
                 server_pid, server_start, ignored, admitted_ms, released_ms, release, tenure_id,
                 tenure_epoch)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, NULL, NULL, ?10, ?11)
             ON CONFLICT (holder) DO UPDATE SET provider = excluded.provider,
                 repo = excluded.repo, run_id = excluded.run_id, attempt = excluded.attempt,
                 worker_id = NULL, server_pid = excluded.server_pid,
                 server_start = excluded.server_start, ignored = excluded.ignored,
                 admitted_ms = excluded.admitted_ms, released_ms = NULL, release = NULL,
                 tenure_id = excluded.tenure_id, tenure_epoch = excluded.tenure_epoch",
            params![
                admission.holder,
                admission.provider,
                admission.repo,
                admission.run.map(|(run_id, _)| run_id),
                admission.run.map(|(_, attempt)| attempt),
                admission.server.0,
                admission.server.1.map(|start| start as i64),
                admission.ignored,
                admission.at_ms as i64,
                admission.tenure.map(|(id, _)| id),
                admission.tenure.map(|(_, epoch)| epoch),
            ],
        )?;
        Ok(())
    }
}

/// A process that holds a tenure (`coordinator.acquire` with `holder_pid`):
/// its pid and the start token a later process reusing the pid cannot
/// match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct HolderProcess {
    pub(super) pid: u32,
    pub(super) start: u64,
}

/// One coordination tenure with its latest binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredTenure {
    pub(super) id: String,
    pub(super) repo: String,
    pub(super) item: Option<String>,
    pub(super) started_at: u64,
    pub(super) ended_at: Option<u64>,
    pub(super) end_reason: Option<String>,
    pub(super) epoch: i64,
    pub(super) pane_id: Option<String>,
    pub(super) session_id: Option<String>,
    /// A headless item coordinator's tenure (`todo.next`), and the worker
    /// it runs as once that started.
    pub(super) headless: bool,
    pub(super) worker_id: Option<String>,
    /// The process that holds it, when no pane does.
    pub(super) holder: Option<HolderProcess>,
    /// The sha256 of the token its holder was last given.
    pub(super) token_sha256: Option<String>,
}

const ACTIVE_OF_PANE: &str = "t.ended_at IS NULL AND t.id IN (SELECT coordinator_id \
    FROM coordinator_bindings WHERE pane_id = ?1 AND to_at IS NULL)";

/// The tenures `filter` (over `coordinators t`) selects, oldest first, each
/// with its latest binding.
fn tenures(
    conn: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
) -> StoreResult<Vec<StoredTenure>> {
    let mut statement = conn.prepare(&format!(
        "SELECT t.id, t.repo, t.item, t.started_at, t.ended_at, t.end_reason, t.epoch,
             b.pane_id, b.session_id, t.headless, t.worker_id, t.holder_pid, t.holder_start,
             t.token_sha256
         FROM coordinators t
         LEFT JOIN coordinator_bindings b ON b.rowid = (SELECT rowid FROM coordinator_bindings
             WHERE coordinator_id = t.id ORDER BY from_at DESC, rowid DESC LIMIT 1)
         WHERE {filter}
         ORDER BY t.started_at, t.rowid"
    ))?;
    let rows = statement.query_map(params, |row| {
        Ok(StoredTenure {
            id: row.get(0)?,
            repo: row.get(1)?,
            item: row.get(2)?,
            started_at: row.get::<_, i64>(3)? as u64,
            ended_at: row.get::<_, Option<i64>>(4)?.map(|ms| ms as u64),
            end_reason: row.get(5)?,
            epoch: row.get(6)?,
            pane_id: row.get(7)?,
            session_id: row.get(8)?,
            headless: row.get::<_, i64>(9)? != 0,
            worker_id: row.get(10)?,
            holder: match (
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<i64>>(12)?,
            ) {
                (Some(pid), Some(start)) => Some(HolderProcess {
                    pid: pid as u32,
                    start: start as u64,
                }),
                _ => None,
            },
            token_sha256: row.get(13)?,
        })
    })?;
    rows.collect()
}

fn tenure_by_id(conn: &Connection, id: &str) -> StoreResult<Option<StoredTenure>> {
    Ok(tenures(conn, "t.id = ?1", [id])?.pop())
}

fn active_coordinator_of(conn: &Connection, repo: &str) -> StoreResult<Option<StoredTenure>> {
    Ok(tenures(conn, "t.ended_at IS NULL AND t.repo = ?1", [repo])?.pop())
}

/// The tenure `repo`'s `worker.run` presents `token` for: none while no
/// tenure is active; the tenure that admits the start when the token is its
/// current one; the refusal text, naming the holder, when it is not (none
/// given, another tenure's, or one its holder was given before acquiring
/// again).
///
/// The caller runs this in the transaction that records the work the start is
/// authorized by (its usage admission), so no tenure change can fall between
/// the check and the start it lets through.
pub(super) fn tenure_token_check(
    conn: &Connection,
    repo: &str,
    token: Option<&str>,
) -> StoreResult<Result<Option<(String, i64)>, String>> {
    let Some(active) = active_coordinator_of(conn, repo)? else {
        return Ok(Ok(None));
    };
    let holder = super::coordinators::holder_description(&active);
    match (token, active.token_sha256.as_deref()) {
        (Some(token), Some(current)) if super::coordinators::token_hash(token) == current => {
            Ok(Ok(Some((active.id, active.epoch))))
        }
        (None, _) => Ok(Err(format!(
            "repository {repo} is coordinated by {holder}; a start there needs that \
             tenure's token (`herdr coordinator acquire`, or `herdr coordinator start \
             --token` in its pane), and none was given; nothing started"
        ))),
        (Some(_), _) => Ok(Err(format!(
            "repository {repo} is coordinated by {holder}, and the token given is not that \
             tenure's current one (another tenure's, or one replaced by a later acquire); \
             nothing started"
        ))),
    }
}

/// Whether a `todo.run` of `repo` claimed under tenure `owner` (none for a run
/// claimed outside any tenure) may take a step that lands (cherry-pick,
/// install, TODO edit, push): only while that tenure is still the
/// repository's active one, or, for a run without a tenure, while none is
/// active. The refusal names the current holder; a store that cannot say
/// refuses too. The caller runs this in the transaction that admits the step
/// ([`Tx::tenure_step`]), so the step and the tenure it was admitted under
/// cannot be split by a tenure change.
pub(super) fn tenure_fence_check(
    conn: &Connection,
    repo: &str,
    owner: Option<&str>,
) -> StoreResult<Result<(), String>> {
    let Some(owner) = owner else {
        return match active_coordinator_of(conn, repo)? {
            None => Ok(Ok(())),
            Some(active) => Ok(Err(format!(
                "it was claimed under no coordinator tenure, and repository {repo}'s tenure \
                 is held by {} now; the run lands nothing while another holder holds it",
                super::coordinators::holder_description(&active)
            ))),
        };
    };
    // An active tenure is its repository's only one (the store's unique
    // index), so the owner's own row decides.
    let tenure = tenure_by_id(conn, owner)?;
    let tenure_repo = match &tenure {
        Some(tenure) if tenure.ended_at.is_none() => return Ok(Ok(())),
        Some(tenure) => tenure.repo.clone(),
        None => repo.to_owned(),
    };
    let held_by = match active_coordinator_of(conn, &tenure_repo)? {
        Some(active) => format!(
            "it is held by {}",
            super::coordinators::holder_description(&active)
        ),
        None => "no coordinator holds it now".to_owned(),
    };
    Ok(Err(format!(
        "its coordinator {owner} no longer holds repository {tenure_repo}'s tenure \
         ({held_by}); the run lands nothing for a coordinator that lost the tenure"
    )))
}

/// A landing step a tenure admitted and has not released
/// ([`Tx::tenure_step`]).
pub(super) struct NewTenureStep<'a> {
    pub(super) id: &'a str,
    pub(super) repo: &'a str,
    /// The tenure the step was admitted under; none for a run claimed
    /// outside any tenure.
    pub(super) coordinator: Option<(&'a str, i64)>,
    pub(super) kind: &'a str,
    pub(super) run_id: &'a str,
    pub(super) server: (u32, Option<u64>),
    pub(super) at_ms: u64,
}

/// The end of a tenure herdr saw and has not carried out yet
/// ([`Tx::request_tenure_end`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingEnd {
    pub(super) coordinator_id: String,
    /// The tenure's epoch, so a stale end cannot end a later tenure of the
    /// same id.
    pub(super) epoch: i64,
    pub(super) reason: String,
    pub(super) cause: Option<String>,
    pub(super) requested_ms: u64,
}

fn pending_ends_where(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> StoreResult<Vec<PendingEnd>> {
    let mut statement = conn.prepare(&format!(
        "SELECT coordinator_id, epoch, reason, cause, requested_ms FROM tenure_pending_ends \
         {filter} ORDER BY requested_ms, coordinator_id"
    ))?;
    let ends = statement
        .query_map(params, |row| {
            Ok(PendingEnd {
                coordinator_id: row.get(0)?,
                epoch: row.get(1)?,
                reason: row.get(2)?,
                cause: row.get(3)?,
                requested_ms: row.get::<_, i64>(4)? as u64,
            })
        })?
        .collect::<StoreResult<Vec<PendingEnd>>>()?;
    Ok(ends)
}

impl Store {
    /// The active tenures, of one repository when given.
    pub(super) fn active_coordinators(&self, repo: Option<&str>) -> StoreResult<Vec<StoredTenure>> {
        let conn = lock(&self.conn);
        match repo {
            Some(repo) => tenures(&conn, "t.ended_at IS NULL AND t.repo = ?1", [repo]),
            None => tenures(&conn, "t.ended_at IS NULL", []),
        }
    }

    pub(super) fn coordinator(&self, id: &str) -> StoreResult<Option<StoredTenure>> {
        tenure_by_id(&lock(&self.conn), id)
    }

    /// The active tenure of `repo`, if any.
    pub(super) fn active_coordinator_of(&self, repo: &str) -> StoreResult<Option<StoredTenure>> {
        active_coordinator_of(&lock(&self.conn), repo)
    }

    /// The tenure `repo`'s `worker.run` presents `token` for
    /// ([`tenure_token_check`]), as the store sees it now: the precheck of a
    /// start, whose authoritative check and admission are one transaction.
    pub(super) fn tenure_token_check(
        &self,
        repo: &str,
        token: Option<&str>,
    ) -> StoreResult<Result<Option<(String, i64)>, String>> {
        self.read(|conn| tenure_token_check(conn, repo, token))
    }

    /// One tenure's pending end for `(coordinator_id, epoch)`, if herdr saw
    /// one.
    pub(super) fn pending_end(
        &self,
        coordinator_id: &str,
        epoch: i64,
    ) -> StoreResult<Option<PendingEnd>> {
        self.read(|conn| {
            Ok(pending_ends_where(
                conn,
                "WHERE coordinator_id = ?1 AND epoch = ?2",
                &[&coordinator_id, &epoch],
            )?
            .pop())
        })
    }

    /// Releases landing step `id` and carries out the pending ends it held
    /// back ([`Tx::tenure_step_released`]); returns the tenures that ended.
    pub(super) fn finish_tenure_step(
        &self,
        id: &str,
        at_ms: u64,
    ) -> StoreResult<Vec<StoredTenure>> {
        self.transaction(|tx| tx.tenure_step_released(id, at_ms))
    }

    /// Carries out the pending ends no landing step holds back
    /// ([`Tx::settle_pending_ends`]); returns the tenures that ended.
    pub(super) fn settle_pending_ends(&self, at_ms: u64) -> StoreResult<Vec<StoredTenure>> {
        self.transaction(|tx| tx.settle_pending_ends(at_ms))
    }

    /// Retires landing steps `ids`, recorded by servers that cannot release
    /// them, records the server that retired them, and carries out the pending
    /// ends that leaves ([`Tx::retire_tenure_steps`]).
    pub(super) fn retire_tenure_steps(
        &self,
        ids: &[String],
        at_ms: u64,
        retired_by: (u32, Option<u64>),
    ) -> StoreResult<Vec<StoredTenure>> {
        self.transaction(|tx| tx.retire_tenure_steps(ids, at_ms, retired_by))
    }

    /// The landing steps still open, each with the server that admitted it
    /// ([`Tx::tenure_step_servers`]).
    pub(super) fn tenure_step_servers(&self) -> StoreResult<Vec<(String, u32, Option<u64>)>> {
        self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, server_pid, server_start FROM tenure_steps WHERE retired_ms IS NULL",
            )?;
            let steps = statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i64>(1)? as u32,
                        row.get::<_, Option<i64>>(2)?.map(|start| start as u64),
                    ))
                })?
                .collect::<StoreResult<Vec<_>>>()?;
            Ok(steps)
        })
    }

    /// The chains of item coordinators, active or stopped, of `repo` when
    /// given.
    pub(super) fn chains(&self, repo: Option<&str>) -> StoreResult<Vec<StoredChain>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT repo FROM todo_chains WHERE (?1 IS NULL OR repo = ?1) ORDER BY repo",
        )?;
        let repos: Vec<String> = statement
            .query_map([repo], |row| row.get(0))?
            .collect::<StoreResult<_>>()?;
        let mut chains = Vec::new();
        for repo in repos {
            chains.extend(chain_of(&conn, &repo)?);
        }
        Ok(chains)
    }

    /// The active tenure bound to `pane_id`.
    pub(super) fn coordinator_of_pane(&self, pane_id: &str) -> StoreResult<Option<StoredTenure>> {
        Ok(tenures(&lock(&self.conn), ACTIVE_OF_PANE, [pane_id])?.pop())
    }
}

const RUN_COLUMNS: &str = "id, repo, item, step, status, attempt, base, worker_id, branch, task, \
message, paths, check_name, check_argv, owner_pane, owner_session, last_acked_seq, pending_event, \
error, picked, created_ms, updated_ms, finish, workspace, owner_coordinator_id, landing";

fn run_from_row(row: &rusqlite::Row<'_>) -> StoreResult<Run> {
    let list = |index: usize| -> StoreResult<Vec<String>> {
        Ok(serde_json::from_str(&row.get::<_, String>(index)?).unwrap_or_default())
    };
    let parsed = |index: usize| -> StoreResult<Value> { Ok(Value::String(row.get(index)?)) };
    // `check_argv` holds the run's checks with their names; a run of the
    // first slice has one check, its argv there and its name in
    // `check_name`.
    let check_argv: String = row.get(13)?;
    let checks = serde_json::from_str::<Vec<RunCheck>>(&check_argv).unwrap_or_else(|_| {
        vec![RunCheck {
            name: row.get(12).unwrap_or_default(),
            argv: serde_json::from_str(&check_argv).unwrap_or_default(),
        }]
    });
    let finish: RunFinish = row
        .get::<_, Option<String>>(22)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    // A landing that does not parse is kept as it is, with the error: the
    // run fails closed instead of deriving its state from `step`.
    let (landing, landing_invalid) = match row.get::<_, Option<String>>(25)? {
        None => (None, None),
        Some(text) => match serde_json::from_str::<Landing>(&text) {
            Ok(landing) => (Some(landing), None),
            Err(error) => (None, Some((text, error.to_string()))),
        },
    };
    Ok(Run {
        info: TodoRunInfo {
            run_id: row.get(0)?,
            repo: row.get(1)?,
            item: row.get(2)?,
            step: serde_json::from_value(parsed(3)?).unwrap_or(TodoStep::Unknown),
            status: serde_json::from_value(parsed(4)?).unwrap_or(TodoRunStatus::Unknown),
            attempt: row.get(5)?,
            base: row.get(6)?,
            worker_id: row.get(7)?,
            branch: row.get(8)?,
            task: row.get(9)?,
            message: row.get(10)?,
            paths: list(11)?,
            checks: checks.iter().map(|check| check.name.clone()).collect(),
            last_acked_seq: row.get(16)?,
            pending_event: row.get(17)?,
            error: row.get(18)?,
            picked: row.get(19)?,
            created_ms: row.get::<_, i64>(20)? as u64,
            updated_ms: row.get::<_, i64>(21)? as u64,
            installed_build: finish.installed_build.clone(),
            todo_commit: finish.todo_commit.clone(),
            pushed: finish.pushed.clone(),
            push_skipped: landing
                .as_ref()
                .and_then(|landing| landing.core.push_skipped.clone()),
            target_branch: finish.target_branch.clone(),
            landing_worktree: finish.landing_worktree.clone(),
            push: finish.push.clone(),
            kept_branches: finish.kept_branches.clone(),
            next_item: finish.next.as_ref().map(|next| next.item.clone()),
            next_run_id: finish.next_run_id.clone(),
            next_refusal: finish.next_refusal.clone(),
            stop_reason: finish.stop_reason.clone(),
            continue_self: finish.continue_self,
            continues: finish.continues.clone(),
            continued_by: finish.continued_by.clone(),
            auto_review: finish.auto_review,
            auto_answer: finish.auto_answer,
            new_api: finish.new_api,
            drafted: finish.draft,
            queued: finish.queued,
            grants: finish.run_grants(),
            step_started_ms: None,
            step_command: None,
            step_log: None,
            agent: finish.ran_agent.clone(),
            model: finish.ran_model.clone(),
            degraded: landing
                .as_ref()
                .map(|landing| super::runs::degraded_of(&landing.core))
                .unwrap_or_default(),
        },
        checks,
        finish,
        owner_pane: row.get(14)?,
        owner_session: row.get(15)?,
        workspace: row.get(23)?,
        owner_coordinator: row.get(24)?,
        landing,
        landing_invalid,
        current: attempt_from_row(row, 26)?,
    })
}

/// The run's columns and its current attempt's ([`ATTEMPT_COLUMNS`]),
/// which [`run_from_row`] reads, then `filter` over `runs r`.
fn run_select(filter: &str) -> String {
    let columns = RUN_COLUMNS
        .split(", ")
        .map(|column| format!("r.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let attempt = ATTEMPT_COLUMNS
        .split(", ")
        .map(|column| format!("a.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT {columns}, {attempt} FROM runs r \
         LEFT JOIN attempts a ON a.run_id = r.id AND a.attempt = r.attempt {filter}"
    )
}

/// What `attempts` holds beside the run's own columns.
const ATTEMPT_COLUMNS: &str = "base, from_attempt, from_commit, \"commit\", review_event, \
review_decision, review_text, verification";

fn attempt_from_row(row: &rusqlite::Row<'_>, at: usize) -> StoreResult<Attempt> {
    Ok(Attempt {
        base: row.get(at)?,
        from_attempt: row.get(at + 1)?,
        from_commit: row.get(at + 2)?,
        commit: row.get(at + 3)?,
        review_event: row.get(at + 4)?,
        review_decision: row.get(at + 5)?,
        review_text: row.get(at + 6)?,
        verification: row.get(at + 7)?,
    })
}

/// A record of an item's life as it is appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NewHistory {
    pub(super) repo: String,
    pub(super) item: String,
    pub(super) kind: HistoryEventKind,
    pub(super) run_id: Option<String>,
    pub(super) attempt: Option<u32>,
    pub(super) text: Option<String>,
    pub(super) item_text: Option<String>,
    pub(super) ids_at_claim: Vec<String>,
    pub(super) follow_ups: Vec<String>,
    /// The tenure the record names: the run's owner, or the item
    /// coordinator that started or ended.
    pub(super) coordinator_id: Option<String>,
}

/// The record a run's event writes, if any: the claim with `run_created`
/// (which carries the item's text and the ids in TODO.md), the note or close
/// with `run_todo_committed` (a close carries the item's last text and the
/// follow-ups), the run's `blocked` and `aborted` events, and its `done`
/// event when its close gave a stop reason.
fn history_of(run: &Run, event: &Value) -> Option<NewHistory> {
    let strings = |value: &Value| -> Vec<String> {
        serde_json::from_value(value.clone()).unwrap_or_default()
    };
    let text = |value: &Value| value.as_str().map(str::to_owned);
    let record = |kind| NewHistory {
        repo: run.info.repo.clone(),
        item: run.info.item.clone(),
        kind,
        run_id: Some(run.info.run_id.clone()),
        attempt: Some(run.info.attempt),
        text: None,
        item_text: None,
        ids_at_claim: Vec::new(),
        follow_ups: Vec::new(),
        coordinator_id: run.owner_coordinator.clone(),
    };
    match event["type"].as_str()? {
        "run_created" => Some(NewHistory {
            item_text: text(&event["item_text"]),
            ids_at_claim: strings(&event["item_ids"]),
            ..record(HistoryEventKind::Claimed)
        }),
        "run_todo_committed" => match (&run.finish.note, &run.finish.close) {
            (Some(note), _) => Some(NewHistory {
                text: Some(note.clone()),
                ..record(HistoryEventKind::Noted)
            }),
            (None, Some(decision)) => Some(NewHistory {
                text: Some(decision.clone()),
                item_text: text(&event["item_text"]),
                follow_ups: strings(&event["follow_ups"]),
                ..record(HistoryEventKind::Closed)
            }),
            (None, None) => None,
        },
        // A run its coordinator was to continue itself, gone before it did.
        "run_continuation_lost" => Some(NewHistory {
            text: text(&event["reason"]),
            ..record(HistoryEventKind::Stopped)
        }),
        "run_event" => {
            let kind = match event["kind"].as_str()? {
                "blocked" => HistoryEventKind::Blocked,
                "aborted" => HistoryEventKind::Aborted,
                // A close that named no next item: why, with its run's end.
                "done" => {
                    return run.finish.stop_reason.as_ref().map(|reason| NewHistory {
                        text: Some(reason.clone()),
                        ..record(HistoryEventKind::Stopped)
                    })
                }
                _ => return None,
            };
            Some(NewHistory {
                text: text(&event["event"]["error"]),
                ..record(kind)
            })
        }
        _ => None,
    }
}

/// A stored record of an item's life, with the ids TODO.md had at a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredHistory {
    pub(super) event: HistoryEvent,
    pub(super) ids_at_claim: Vec<String>,
}

impl Store {
    /// Appends one allowlist exception and returns it as stored.
    pub(super) fn record_override(
        &self,
        record: &CoordinatorOverride,
    ) -> StoreResult<CoordinatorOverride> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO coordinator_overrides (ts, repo, coordinator_id, item, pane_id, \
             session_id, tool, command, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.ts_ms as i64,
                record.repo,
                record.coordinator_id,
                record.item,
                record.pane_id,
                record.session_id,
                record.tool,
                record.command,
                record.reason
            ],
        )?;
        Ok(CoordinatorOverride {
            id: conn.last_insert_rowid(),
            ..record.clone()
        })
    }

    /// Appends one call the allowlist would have denied and returns it as
    /// stored.
    pub(super) fn record_would_deny(
        &self,
        record: &CoordinatorWouldDeny,
    ) -> StoreResult<CoordinatorWouldDeny> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO coordinator_would_deny (ts, repo, coordinator_id, item, pane_id, \
             session_id, tool, command, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.ts_ms as i64,
                record.repo,
                record.coordinator_id,
                record.item,
                record.pane_id,
                record.session_id,
                record.tool,
                record.command,
                record.reason
            ],
        )?;
        Ok(CoordinatorWouldDeny {
            id: conn.last_insert_rowid(),
            ..record.clone()
        })
    }

    /// The calls the allowlist would have denied, oldest first: of `repo`
    /// when given.
    pub(super) fn would_deny(&self, repo: Option<&str>) -> StoreResult<Vec<CoordinatorWouldDeny>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT id, ts, repo, coordinator_id, item, pane_id, session_id, tool, command, \
             reason FROM coordinator_would_deny WHERE (?1 IS NULL OR repo = ?1) ORDER BY id",
        )?;
        let rows = statement.query_map(params![repo], |row| {
            Ok(CoordinatorWouldDeny {
                id: row.get(0)?,
                ts_ms: row.get::<_, i64>(1)? as u64,
                repo: row.get(2)?,
                coordinator_id: row.get(3)?,
                item: row.get(4)?,
                pane_id: row.get(5)?,
                session_id: row.get(6)?,
                tool: row.get(7)?,
                command: row.get(8)?,
                reason: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// The allowlist exceptions, oldest first: of `repo` when given.
    pub(super) fn overrides(&self, repo: Option<&str>) -> StoreResult<Vec<CoordinatorOverride>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT id, ts, repo, coordinator_id, item, pane_id, session_id, tool, command, \
             reason FROM coordinator_overrides WHERE (?1 IS NULL OR repo = ?1) ORDER BY id",
        )?;
        let rows = statement.query_map(params![repo], |row| {
            Ok(CoordinatorOverride {
                id: row.get(0)?,
                ts_ms: row.get::<_, i64>(1)? as u64,
                repo: row.get(2)?,
                coordinator_id: row.get(3)?,
                item: row.get(4)?,
                pane_id: row.get(5)?,
                session_id: row.get(6)?,
                tool: row.get(7)?,
                command: row.get(8)?,
                reason: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    /// The records of items' lives, oldest first: of `repo` and `item` when
    /// given.
    pub(super) fn item_history(
        &self,
        repo: Option<&str>,
        item: Option<&str>,
    ) -> StoreResult<Vec<StoredHistory>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT id, repo, item, kind, ts, run_id, attempt, text, item_text, ids_at_claim, \
             follow_ups, coordinator_id FROM item_history
             WHERE (?1 IS NULL OR repo = ?1) AND (?2 IS NULL OR item = ?2) ORDER BY id",
        )?;
        let rows = statement.query_map(params![repo, item], |row| {
            let list = |index: usize| -> StoreResult<Vec<String>> {
                Ok(serde_json::from_str(&row.get::<_, String>(index)?).unwrap_or_default())
            };
            Ok(StoredHistory {
                event: HistoryEvent {
                    id: row.get(0)?,
                    repo: row.get(1)?,
                    item: row.get(2)?,
                    kind: serde_json::from_value(Value::String(row.get(3)?))
                        .unwrap_or(HistoryEventKind::Unknown),
                    ts_ms: row.get::<_, i64>(4)? as u64,
                    run_id: row.get(5)?,
                    attempt: row.get(6)?,
                    text: row.get(7)?,
                    item_text: row.get(8)?,
                    follow_ups: list(10)?,
                    coordinator_id: row.get(11)?,
                },
                ids_at_claim: list(9)?,
            })
        })?;
        rows.collect()
    }

    /// Each `todo.run` attempt's worker with its run, its attempt, its own
    /// verify verdict and the previous attempt's verdict and decision, which
    /// say why it ran; by worker id.
    pub(super) fn attempts_by_worker(&self) -> StoreResult<HashMap<String, WorkerAttempt>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT a.worker_id, a.run_id, a.attempt,
                 CASE WHEN json_valid(a.verification)
                     THEN json_extract(a.verification, '$.verdict') END,
                 p.attempt IS NOT NULL,
                 CASE WHEN json_valid(p.verification)
                     THEN json_extract(p.verification, '$.verdict') END,
                 p.review_decision
             FROM attempts a
             LEFT JOIN attempts p ON p.run_id = a.run_id AND p.attempt = a.attempt - 1
             WHERE a.worker_id IS NOT NULL
             ORDER BY a.run_id, a.attempt",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                WorkerAttempt {
                    run_id: row.get(1)?,
                    attempt: row.get(2)?,
                    verdict: row.get(3)?,
                    has_previous: row.get(4)?,
                    previous_verdict: row.get(5)?,
                    previous_decision: row.get(6)?,
                },
            ))
        })?;
        rows.collect()
    }

    /// The commits `todo.run` landed from the runs that ended `done`, the
    /// first landed first: of `repo` and `item` when given, landed in
    /// `[since_ms, until_ms)`.
    pub(super) fn landed(
        &self,
        repo: Option<&str>,
        item: Option<&str>,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> StoreResult<Vec<WorkerLandedItem>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT l.item, r.repo, l.run_id, l.attempt, l.landed_sha, l.ts
             FROM landings l JOIN runs r ON r.id = l.run_id
             WHERE r.status = 'done'
               AND (?1 IS NULL OR r.repo = ?1) AND (?2 IS NULL OR l.item = ?2)
               AND (?3 IS NULL OR l.ts >= ?3) AND (?4 IS NULL OR l.ts < ?4)
             ORDER BY l.ts, l.landed_sha",
        )?;
        let rows = statement.query_map(
            params![
                repo,
                item,
                since_ms.map(|ms| ms as i64),
                until_ms.map(|ms| ms as i64)
            ],
            |row| {
                Ok(WorkerLandedItem {
                    item: row.get(0)?,
                    repo: row.get(1)?,
                    todo_run_id: row.get(2)?,
                    attempt: row.get(3)?,
                    landed_sha: row.get(4)?,
                    landed_ms: row.get::<_, i64>(5)? as u64,
                })
            },
        )?;
        rows.collect()
    }

    /// The commits a run landed, by attempt.
    pub(super) fn landings_of_run(&self, run_id: &str) -> StoreResult<Vec<(u32, String)>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT attempt, landed_sha FROM landings WHERE run_id = ?1 ORDER BY ts, landed_sha",
        )?;
        let rows = statement.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }
}

/// A `todo.run` attempt as its worker's run record names it
/// ([`Store::attempts_by_worker`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkerAttempt {
    pub(super) run_id: String,
    pub(super) attempt: u32,
    /// The verdict of its verify, once the driver recorded one.
    pub(super) verdict: Option<String>,
    /// The previous attempt is recorded, with its verdict and decision.
    pub(super) has_previous: bool,
    pub(super) previous_verdict: Option<String>,
    pub(super) previous_decision: Option<String>,
}

/// One row of `attempts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttemptRow {
    pub(super) number: u32,
    pub(super) worker_id: Option<String>,
    pub(super) branch: Option<String>,
    pub(super) task: String,
    pub(super) attempt: Attempt,
}

impl Tx<'_> {
    /// Appends a run's event and writes its row as it is after it: a new
    /// row, or the existing one replaced. With `pending`, the event is the
    /// one the run waits on (`pending_event`). Returns the event's `seq`.
    /// The unique index refuses a second run in progress of a repository.
    pub(super) fn run_event(
        &self,
        run: &mut Run,
        event: &Value,
        pending: bool,
        at_ms: u64,
    ) -> StoreResult<i64> {
        let seq = self.event(&EventRow {
            worker_id: &run.info.run_id,
            direction: super::Direction::Herdr,
            record: &Recorded::Event(event),
            ts_ms: at_ms,
        })?;
        run.info.pending_event = pending.then_some(seq);
        run.info.updated_ms = at_ms;
        let info = &run.info;
        self.tx.execute(
            // Not `OR REPLACE`: that would delete another run in progress
            // of the repository instead of letting the index refuse this one.
            &format!(
                "INSERT INTO runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, \
                 ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, \
                 ?23, ?24, ?25, ?26) \
                 ON CONFLICT (id) DO UPDATE SET {}",
                RUN_COLUMNS
                    .split(", ")
                    // The owner changes only with its own event
                    // ([`Self::move_run_owner`]): a driver's copy of the
                    // run, read before a move, must not put it back.
                    .filter(|column| !matches!(
                        *column,
                        "id" | "owner_pane"
                            | "owner_session"
                            | "workspace"
                            | "owner_coordinator_id"
                    ))
                    .map(|column| format!("{column} = excluded.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            params![
                info.run_id,
                info.repo,
                info.item,
                enum_text(&info.step),
                enum_text(&info.status),
                info.attempt,
                info.base,
                info.worker_id,
                info.branch,
                info.task,
                info.message,
                serde_json::to_string(&info.paths).unwrap_or_else(|_| "[]".into()),
                info.checks.join(" "),
                serde_json::to_string(&run.checks).unwrap_or_else(|_| "[]".into()),
                run.owner_pane,
                run.owner_session,
                info.last_acked_seq,
                info.pending_event,
                info.error,
                info.picked,
                info.created_ms as i64,
                info.updated_ms as i64,
                serde_json::to_string(&run.finish_with_results()).ok(),
                run.workspace,
                run.owner_coordinator,
                match (&run.landing, &run.landing_invalid) {
                    (Some(landing), _) => serde_json::to_string(landing).ok(),
                    (None, Some((text, _))) => Some(text.clone()),
                    (None, None) => None,
                },
            ],
        )?;
        // The owner as stored, which a move may have changed meanwhile.
        (
            run.owner_pane,
            run.owner_session,
            run.workspace,
            run.owner_coordinator,
        ) = self.tx.query_row(
            "SELECT owner_pane, owner_session, workspace, owner_coordinator_id FROM runs
             WHERE id = ?1",
            [&info.run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let attempt = &run.current;
        self.tx.execute(
            &format!(
                "INSERT INTO attempts (run_id, attempt, worker_id, branch, task, {ATTEMPT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT (run_id, attempt) DO UPDATE SET worker_id = excluded.worker_id, \
                 branch = excluded.branch, task = excluded.task, {}",
                ATTEMPT_COLUMNS
                    .split(", ")
                    .map(|column| format!("{column} = excluded.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            params![
                info.run_id,
                info.attempt,
                info.worker_id,
                info.branch,
                info.task,
                attempt.base,
                attempt.from_attempt,
                attempt.from_commit,
                attempt.commit,
                attempt.review_event,
                attempt.review_decision,
                attempt.review_text,
                attempt.verification,
            ],
        )?;
        if let Some(record) = history_of(run, event) {
            self.item_history(&record, at_ms)?;
        }
        // The owning tenure works on the run's item from its claim to its
        // end.
        if let Some(tenure) = &run.owner_coordinator {
            if event["type"].as_str() == Some("run_created") {
                self.coordinator_item(tenure, Some(&run.info.item), at_ms)?;
            } else if matches!(
                run.info.status,
                TodoRunStatus::Done | TodoRunStatus::Aborted
            ) && tenure_by_id(self.tx, tenure)?
                .and_then(|tenure| tenure.item)
                .as_deref()
                == Some(run.info.item.as_str())
            {
                self.coordinator_item(tenure, None, at_ms)?;
            }
        }
        // The run as this transaction leaves it decides: one it ends lets
        // go of its lock file before the end is committed.
        let mut ended = self.ended.borrow_mut();
        ended.retain(|run_id| *run_id != run.info.run_id);
        if matches!(
            run.info.status,
            TodoRunStatus::Done | TodoRunStatus::Blocked | TodoRunStatus::Aborted
        ) {
            ended.push(run.info.run_id.clone());
        }
        Ok(seq)
    }

    /// Moves run `run_id` to another owner (pane, agent session, workspace
    /// and tenure), recorded as `run_owner_moved` with why (`cause`). The
    /// only write of the owner after the run's claim: [`Self::run_event`]
    /// keeps the stored one. A `workspace` of `None` keeps the run's.
    pub(super) fn move_run_owner(
        &self,
        run_id: &str,
        owner: &RunOwner<'_>,
        cause: &str,
        at_ms: u64,
    ) -> StoreResult<()> {
        let Some(current) = self.run(run_id)? else {
            return Ok(());
        };
        let workspace = owner.workspace.or(current.workspace.as_deref());
        let event = serde_json::json!({
            "type": "run_owner_moved",
            "from_pane": current.owner_pane,
            "from_session": current.owner_session,
            "from_workspace": current.workspace,
            "from_coordinator": current.owner_coordinator,
            "to_pane": owner.pane_id,
            "to_session": owner.session_id,
            "to_workspace": workspace,
            "to_coordinator": owner.coordinator_id,
            "cause": cause,
        });
        self.run_note(run_id, &event, at_ms)?;
        self.tx.execute(
            "UPDATE runs SET owner_pane = ?2, owner_session = ?3, workspace = ?4,
                 owner_coordinator_id = ?5
             WHERE id = ?1",
            params![
                run_id,
                owner.pane_id,
                owner.session_id,
                workspace,
                owner.coordinator_id
            ],
        )?;
        Ok(())
    }

    /// The runs tenure `coordinator_id` owns that have not ended.
    pub(super) fn open_runs_of_coordinator(&self, coordinator_id: &str) -> StoreResult<Vec<Run>> {
        let mut statement = self.tx.prepare(&run_select(
            "WHERE r.owner_coordinator_id = ?1 AND r.status IN ('running', 'waiting', 'blocked') \
             ORDER BY r.created_ms, r.rowid",
        ))?;
        let runs = statement.query_map([coordinator_id], run_from_row)?;
        runs.collect()
    }

    /// The done runs tenure `coordinator_id` owns.
    pub(super) fn done_runs_of_coordinator(&self, coordinator_id: &str) -> StoreResult<Vec<Run>> {
        let mut statement = self.tx.prepare(&run_select(
            "WHERE r.owner_coordinator_id = ?1 AND r.status = 'done' \
             ORDER BY r.created_ms, r.rowid",
        ))?;
        let runs = statement.query_map([coordinator_id], run_from_row)?;
        runs.collect()
    }

    /// Appends one record of an item's life.
    pub(super) fn item_history(&self, record: &NewHistory, at_ms: u64) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO item_history (repo, item, kind, run_id, attempt, text, item_text, \
             ids_at_claim, follow_ups, ts, coordinator_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                record.repo,
                record.item,
                enum_text(&record.kind),
                record.run_id,
                record.attempt,
                record.text,
                record.item_text,
                serde_json::to_string(&record.ids_at_claim).unwrap_or_else(|_| "[]".into()),
                serde_json::to_string(&record.follow_ups).unwrap_or_else(|_| "[]".into()),
                at_ms as i64,
                record.coordinator_id,
            ],
        )?;
        Ok(())
    }

    /// Records a commit the run landed on its target branch; a landing recorded
    /// already (a step a crash cut off) is replaced by the same row.
    pub(super) fn landing(&self, landing: &TodoLanding) -> StoreResult<()> {
        self.tx.execute(
            "INSERT OR REPLACE INTO landings (landed_sha, run_id, attempt, item, worker_commit, ts) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                landing.landed_sha,
                landing.run_id,
                landing.attempt,
                landing.item,
                landing.worker_commit,
                landing.ts_ms.unwrap_or_default() as i64,
            ],
        )?;
        Ok(())
    }

    /// Appends an event to a run without writing its row: all a driver
    /// that does not hold the run's lock may write.
    /// [`Self::run_event`] for a run that keeps waiting on the event it
    /// waits on: its row is written, its `pending_event` stays.
    pub(super) fn run_event_keeping_pending(
        &self,
        run: &mut Run,
        event: &Value,
        at_ms: u64,
    ) -> StoreResult<i64> {
        let pending = run.info.pending_event;
        let seq = self.run_event(run, event, false, at_ms)?;
        run.info.pending_event = pending;
        self.tx.execute(
            "UPDATE runs SET pending_event = ?1 WHERE id = ?2",
            params![pending, run.info.run_id],
        )?;
        Ok(seq)
    }

    pub(super) fn run_note(&self, run_id: &str, event: &Value, at_ms: u64) -> StoreResult<i64> {
        self.event(&EventRow {
            worker_id: run_id,
            direction: super::Direction::Herdr,
            record: &Recorded::Event(event),
            ts_ms: at_ms,
        })
    }

    /// The run as this transaction sees it.
    pub(super) fn run(&self, run_id: &str) -> StoreResult<Option<Run>> {
        self.tx
            .query_row(&run_select("WHERE r.id = ?1"), [run_id], run_from_row)
            .optional()
    }
}

impl Store {
    pub(super) fn run(&self, run_id: &str) -> StoreResult<Option<Run>> {
        lock(&self.conn)
            .query_row(&run_select("WHERE r.id = ?1"), [run_id], run_from_row)
            .optional()
    }

    /// The runs, of one repository when given, oldest first.
    pub(super) fn runs(&self, repo: Option<&str>) -> StoreResult<Vec<Run>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&run_select(
            "WHERE ?1 IS NULL OR r.repo = ?1 ORDER BY r.created_ms, r.rowid",
        ))?;
        let runs = statement.query_map([repo], run_from_row)?;
        runs.collect()
    }

    /// The runs in progress (running, or waiting for their coordinator),
    /// oldest first: at most one per repository.
    pub(super) fn runs_in_progress(&self) -> StoreResult<Vec<Run>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&run_select(
            "WHERE r.status IN ('running', 'waiting') ORDER BY r.created_ms, r.rowid",
        ))?;
        let runs = statement.query_map([], run_from_row)?;
        runs.collect()
    }

    /// The runs clients list, oldest first: those in progress, the done
    /// runs with a stop reason the user has not acknowledged
    /// ([`Run::stop_shown`]), and the done runs between runs
    /// ([`Run::continuation_pending`]).
    pub(super) fn runs_shown(&self) -> StoreResult<Vec<Run>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&run_select(
            "WHERE r.status IN ('running', 'waiting') \
             OR (r.status = 'done' AND CASE WHEN json_valid(r.finish) THEN \
                 (json_extract(r.finish, '$.stop_reason') IS NOT NULL \
                  AND coalesce(json_extract(r.finish, '$.stop_acknowledged'), 0) = 0) \
                 OR (coalesce(json_extract(r.finish, '$.continue_self'), 0) = 1 \
                  AND json_extract(r.finish, '$.continued_by') IS NULL \
                  AND json_extract(r.finish, '$.stop_reason') IS NULL) \
                 ELSE 0 END) \
             ORDER BY r.created_ms, r.rowid",
        ))?;
        let runs = statement.query_map([], run_from_row)?;
        runs.collect()
    }

    /// Whether `repo`'s usage gate is closed; an unknown repository's is
    /// open.
    pub(super) fn usage_gate_closed(&self, repo: &str) -> StoreResult<bool> {
        Ok(lock(&self.conn)
            .query_row(
                "SELECT closed FROM usage_gates WHERE repo = ?1",
                [repo],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub(super) fn usage_gate_keys(&self) -> StoreResult<Vec<String>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare("SELECT repo FROM usage_gates")?;
        let keys = statement.query_map([], |row| row.get(0))?.collect();
        keys
    }

    /// The durable reason and last successful measurement, not authorization.
    pub(super) fn usage_gate_body(&self, repo: &str) -> StoreResult<Value> {
        let body = lock(&self.conn)
            .query_row(
                "SELECT body FROM usage_gates WHERE repo = ?1",
                [repo],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(body
            .and_then(|body| serde_json::from_str(&body).ok())
            .unwrap_or(Value::Null))
    }

    /// Records `repo`'s usage gate and the decision that set it.
    pub(super) fn set_usage_gate(
        &self,
        repo: &str,
        closed: bool,
        body: &Value,
        at_ms: u64,
    ) -> StoreResult<()> {
        lock(&self.conn).execute(
            "INSERT INTO usage_gates (repo, closed, body, updated_ms) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (repo) DO UPDATE SET closed = excluded.closed, body = excluded.body,
                 updated_ms = excluded.updated_ms",
            params![repo, closed, body.to_string(), at_ms as i64],
        )?;
        Ok(())
    }

    /// The admissions of `provider` in `repo` that hold a slot of the usage
    /// gate's cap now ([`held_admissions`]).
    #[cfg(all(test, unix))]
    pub(super) fn held_admissions(
        &self,
        provider: &str,
        repo: &str,
    ) -> StoreResult<Vec<Admission>> {
        held_admissions(&lock(&self.conn), provider, repo)
    }

    /// Every provider's held admissions in a repository, including starts
    /// not yet bound to a worker. Uses the same predicate as the cap.
    pub(super) fn repository_admissions(&self, repo: &str) -> StoreResult<Vec<Admission>> {
        let mut conn = lock(&self.conn);
        // One read snapshot even when a different server writes through
        // its own connection during a live handoff.
        let tx = conn.transaction()?;
        let providers = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT provider FROM usage_admissions WHERE repo = ?1 AND released_ms IS NULL",
            )?;
            let rows = statement.query_map([repo], |row| row.get::<_, String>(0))?;
            rows.collect::<StoreResult<Vec<_>>>()?
        };
        let mut admissions = Vec::new();
        for provider in providers {
            admissions.extend(held_admissions(&tx, &provider, repo)?);
        }
        tx.commit()?;
        admissions.sort_by(|a, b| (a.admitted_ms, &a.holder).cmp(&(b.admitted_ms, &b.holder)));
        Ok(admissions)
    }

    /// Names the worker before its spawn. A handoff that already retired
    /// this admission fences the old server's delayed spawn: it then names
    /// no worker ([`BindOutcome::Ended`]), which the caller refuses.
    pub(super) fn bind_admission(&self, holder: &str, worker_id: &str) -> StoreResult<BindOutcome> {
        let changed = lock(&self.conn).execute(
            "UPDATE usage_admissions SET worker_id = ?2
             WHERE holder = ?1 AND released_ms IS NULL",
            params![holder, worker_id],
        )?;
        Ok(if changed == 0 {
            BindOutcome::Ended
        } else {
            BindOutcome::Bound
        })
    }

    /// Retires a start whose worker row the store does not hold, atomically
    /// with respect to writing that row: a row that is bound while this
    /// runs keeps its worker's slot, and a bound row whose worker appears
    /// afterwards is never retired. A start of a recorded run is left to
    /// the run's own state.
    pub(super) fn retire_unrecorded_admission(
        &self,
        holder: &str,
        why: &str,
        at_ms: u64,
    ) -> StoreResult<bool> {
        lock(&self.conn)
            .execute(
                "UPDATE usage_admissions SET released_ms = ?2, release = ?3
             WHERE holder = ?1 AND released_ms IS NULL
               AND ((worker_id IS NULL AND run_id IS NULL)
                 OR (worker_id IS NOT NULL
                     AND NOT EXISTS (SELECT 1 FROM workers w
                                     WHERE w.id = usage_admissions.worker_id)))",
                params![holder, at_ms as i64, why],
            )
            .map(|changed| changed != 0)
    }

    /// Ends an admission for `why` (a failed spawn, a refusal); one already
    /// ended keeps its first end.
    pub(super) fn release_admission(&self, holder: &str, why: &str, at_ms: u64) -> StoreResult<()> {
        lock(&self.conn).execute(
            "UPDATE usage_admissions SET released_ms = ?2, release = ?3
             WHERE holder = ?1 AND released_ms IS NULL",
            params![holder, at_ms as i64, why],
        )?;
        Ok(())
    }

    /// The admissions not ended whose start no row records yet: a
    /// `worker.run` admitted but not bound to a worker, or one bound to a
    /// worker the store does not hold. A server's start and a committed
    /// handoff reconcile them
    /// ([`crate::workers::WorkerSupervisor::reconcile_admissions`]).
    pub(super) fn unrecorded_admissions(&self) -> StoreResult<Vec<Admission>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!(
            "SELECT {ADMISSION_COLUMNS} FROM usage_admissions a
             WHERE a.released_ms IS NULL
               AND ((a.worker_id IS NULL AND a.run_id IS NULL)
                 OR (a.worker_id IS NOT NULL
                     AND NOT EXISTS (SELECT 1 FROM workers w WHERE w.id = a.worker_id)))
             ORDER BY a.admitted_ms, a.holder"
        ))?;
        let rows = statement.query_map([], admission_from_row)?;
        rows.collect()
    }

    /// The run in progress of `repo`, if any.
    pub(super) fn active_run(&self, repo: &str) -> StoreResult<Option<Run>> {
        lock(&self.conn)
            .query_row(
                &run_select(&format!("WHERE r.repo = ?1 AND {CLAIMING_RUN}")),
                [repo],
                run_from_row,
            )
            .optional()
    }

    /// The landing of `commit`: a full sha, or a prefix of at least 7 hex
    /// characters, of the landed commit or of the worker's commit it was
    /// picked from. The newest when a prefix names several.
    pub(super) fn landing_of(&self, commit: &str) -> StoreResult<Option<TodoLanding>> {
        let conn = lock(&self.conn);
        let landing = conn
            .query_row(
                "SELECT landed_sha, run_id, attempt, item, worker_commit, ts FROM landings
                 WHERE landed_sha = ?1 OR worker_commit = ?1
                    OR substr(landed_sha, 1, length(?1)) = ?1
                    OR substr(worker_commit, 1, length(?1)) = ?1
                 ORDER BY (landed_sha = ?1 OR worker_commit = ?1) DESC, ts DESC LIMIT 1",
                [commit],
                |row| {
                    Ok(TodoLanding {
                        landed_sha: row.get(0)?,
                        run_id: row.get(1)?,
                        attempt: row.get(2)?,
                        item: row.get(3)?,
                        worker_commit: row.get(4)?,
                        worker_id: None,
                        ts_ms: Some(row.get::<_, i64>(5)? as u64),
                        source: TodoLandingSource::Store,
                    })
                },
            )
            .optional()?;
        drop(conn);
        match landing {
            Some(mut landing) => {
                landing.worker_id = self
                    .attempts(&landing.run_id)?
                    .into_iter()
                    .find(|row| row.number == landing.attempt)
                    .and_then(|row| row.worker_id);
                Ok(Some(landing))
            }
            None => Ok(None),
        }
    }

    /// The run's attempts, first first.
    pub(super) fn attempts(&self, run_id: &str) -> StoreResult<Vec<AttemptRow>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!(
            "SELECT attempt, worker_id, branch, task, {ATTEMPT_COLUMNS} FROM attempts \
             WHERE run_id = ?1 ORDER BY attempt"
        ))?;
        let rows = statement.query_map([run_id], |row| {
            Ok(AttemptRow {
                number: row.get(0)?,
                worker_id: row.get(1)?,
                branch: row.get(2)?,
                task: row.get(3)?,
                attempt: attempt_from_row(row, 4)?,
            })
        })?;
        rows.collect()
    }

    /// The run's latest coordinator-facing event (`run_event`): its `seq`
    /// and body.
    pub(super) fn latest_run_event(&self, run_id: &str) -> StoreResult<Option<(i64, Value)>> {
        let row: Option<(i64, String)> = lock(&self.conn)
            .query_row(
                "SELECT seq, body FROM events
                 WHERE worker_id = ?1 AND direction = 'herdr' AND type = 'run_event'
                 ORDER BY seq DESC LIMIT 1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(seq, body)| (seq, serde_json::from_str(&body).unwrap_or(Value::Null))))
    }

    /// Every event of a run of `kind`, oldest first, as its body.
    pub(super) fn run_events_of(&self, run_id: &str, kind: &str) -> StoreResult<Vec<Value>> {
        let conn = lock(&self.conn);
        let mut statement = conn
            .prepare("SELECT body FROM events WHERE worker_id = ?1 AND type = ?2 ORDER BY seq")?;
        let rows = statement.query_map([run_id, kind], |row| row.get::<_, String>(0))?;
        rows.map(|body| Ok(serde_json::from_str(&body?).unwrap_or(Value::Null)))
            .collect()
    }

    /// Every event of a run, oldest first, as `(seq, type)`.
    #[cfg(all(test, unix))]
    pub(super) fn run_event_types(&self, run_id: &str) -> StoreResult<Vec<(i64, String)>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(
            "SELECT seq, coalesce(type, '') FROM events WHERE worker_id = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }
}

const WORKER_COLUMNS: &str = "id, name, cwd, workspace_id, model, slot, state, pid, \
session_id, turns, last_result, rate_limit, tool_sessions, exit_code, exit_signal, \
stop_requested_ms, takeover_ms, takeover_tab, takeover_error, takeover_unfinished, refusal, \
exited, lost, end_note, last_seq, turn_seq, turn_end_seq, gone_seq, owner_pane, owner_session, \
acked_seq, owner_gone, degraded, item, repo, started_ms, ended_ms, done_commits, questions_asked, \
ended_mid_turn, item_title, verification, takeover_id, owner_coordinator_id, broker, broker_seq, \
continuity_gap, coordinates, end_by_owner, agent, ran_model, usage, cost_usd";

fn status_from_row(row: &rusqlite::Row<'_>) -> StoreResult<Status> {
    let json = |index: usize| -> StoreResult<Option<Value>> {
        Ok(row
            .get::<_, Option<String>>(index)?
            .and_then(|text| serde_json::from_str(&text).ok()))
    };
    let mut status = Status::new(row.get(0)?);
    status.name = row.get(1)?;
    status.cwd = row.get(2)?;
    status.workspace_id = row.get(3)?;
    status.model = row.get(4)?;
    status.slot = row.get(5)?;
    status.state =
        serde_json::from_value(Value::String(row.get(6)?)).unwrap_or(WorkerState::Unknown);
    status.pid = row.get(7)?;
    status.session_id = row.get(8)?;
    status.turns = row.get(9)?;
    status.last_result =
        json(10)?.and_then(|value| serde_json::from_value::<WorkerTurnResult>(value).ok());
    status.rate_limit = json(11)?;
    status.tool_sessions = json(12)?
        .and_then(|value| serde_json::from_value::<Vec<(u32, Option<u64>)>>(value).ok())
        .unwrap_or_default()
        .into_iter()
        .collect();
    status.exit_code = row.get(13)?;
    status.exit_signal = row.get(14)?;
    status.stop_requested_ms = row.get::<_, Option<i64>>(15)?.map(|ms| ms as u64);
    status.takeover_ms = row.get::<_, Option<i64>>(16)?.map(|ms| ms as u64);
    status.takeover_tab = row.get(17)?;
    status.takeover_error = row.get(18)?;
    status.takeover_unfinished = row.get(19)?;
    status.refusal = row.get(20)?;
    status.exited = row.get(21)?;
    status.lost = row.get(22)?;
    status.end_note = row.get(23)?;
    status.last_seq = row.get(24)?;
    status.turn_seq = row.get(25)?;
    status.turn_end_seq = row.get(26)?;
    status.gone_seq = row.get(27)?;
    status.owner_pane = row.get(28)?;
    status.owner_session = row.get(29)?;
    status.acked_seq = row.get(30)?;
    status.owner_gone = row.get(31)?;
    status.degraded = row.get(32)?;
    status.item = row.get(33)?;
    status.repo = row.get(34)?;
    status.started_ms = row.get::<_, Option<i64>>(35)?.map(|ms| ms as u64);
    status.ended_ms = row.get::<_, Option<i64>>(36)?.map(|ms| ms as u64);
    status.done_commits = json(37)?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    status.questions_asked = row.get(38)?;
    status.ended_mid_turn = row.get(39)?;
    status.item_title = row.get(40)?;
    status.verification = json(41)?.and_then(|value| serde_json::from_value(value).ok());
    status.takeover_id = row.get(42)?;
    status.owner_coordinator = row.get(43)?;
    status.broker = json(44)?.and_then(|value| serde_json::from_value(value).ok());
    status.broker_seq = row.get::<_, i64>(45)? as u64;
    status.continuity_gap = row.get(46)?;
    status.coordinates = row.get(47)?;
    status.end_by_owner = row.get(48)?;
    status.agent = row.get(49)?;
    status.ran_model = row.get(50)?;
    status.usage = json(51)?.and_then(|value| serde_json::from_value(value).ok());
    status.cost_usd = row.get(52)?;
    Ok(status)
}

/// Fills what a status keeps only in memory from its events: its slot's
/// branch (`started`), when its current or last turn began (`turn_seq`)
/// and when its latest event was recorded (`last_seq`). A `seq` counted on
/// in memory after a failed write names no stored event and leaves its
/// time unknown.
fn load_event_facts(conn: &Connection, status: &mut Status) -> StoreResult<()> {
    status.branch = conn
        .query_row(
            "SELECT json_extract(body, '$.folder_slot.branch') FROM events
             WHERE worker_id = ?1 AND direction = 'herdr' AND type = 'started'
             ORDER BY seq LIMIT 1",
            [&status.worker_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let worker_id = status.worker_id.clone();
    let time_of = |seq: i64| -> StoreResult<Option<u64>> {
        Ok(conn
            .query_row(
                "SELECT ts_ms FROM events WHERE seq = ?1 AND worker_id = ?2",
                rusqlite::params![seq, worker_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|ms| ms as u64))
    };
    status.turn_started_ms = match status.turn_seq {
        Some(seq) => time_of(seq)?,
        None => None,
    };
    status.last_event_ms = time_of(status.last_seq)?;
    Ok(())
}

/// Fills a status's open questions (pending or answering), oldest first,
/// and the most recently settled ones.
fn load_questions(conn: &Connection, status: &mut Status) -> StoreResult<()> {
    let mut pending = conn.prepare(
        "SELECT question, input, asked_seq, state, escalated FROM questions
         WHERE worker_id = ?1 AND state IN ('pending', 'answering')
         ORDER BY asked_seq, rowid",
    )?;
    let rows = pending.query_map([&status.worker_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (question, input, asked_seq, state, escalated) = row?;
        let Ok(question) = serde_json::from_str(&question) else {
            continue;
        };
        status.questions.push(Pending {
            question,
            input: serde_json::from_str(&input)
                .unwrap_or_else(|_| Value::Object(Default::default())),
            asked_seq,
            answering: state == "answering",
            cleared: None,
            escalated,
        });
    }
    let mut settled = conn.prepare(
        "SELECT request_id, how FROM questions
         WHERE worker_id = ?1 AND state NOT IN ('pending', 'answering')
         ORDER BY settled_seq DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = settled.query_map(
        params![status.worker_id, RESOLVED_QUESTIONS_KEPT as i64],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )?;
    let mut resolved = VecDeque::new();
    for row in rows {
        let (request_id, how) = row?;
        resolved.push_front((request_id, how.unwrap_or_else(|| "cancelled".into())));
    }
    status.resolved = resolved;
    Ok(())
}

/// A serde enum's wire name (`waiting_approval`), as the API spells it.
fn enum_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A repository's queue (`todo_queues`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredQueue {
    pub(super) repo: String,
    /// `on` or `paused`.
    pub(super) mode: String,
    pub(super) pause_reason: Option<String>,
    /// The fencing token the next start of the queue must claim.
    pub(super) token: i64,
    pub(super) failures: u32,
    pub(super) status: Option<String>,
    pub(super) reason: Option<String>,
    pub(super) owner_pane: Option<String>,
    pub(super) owner_session: Option<String>,
    pub(super) workspace: Option<String>,
    pub(super) updated_ms: u64,
    /// The real path of the worktree the queue was last turned on from,
    /// whose TODO.md, checks and branch its starts use; none for a queue
    /// turned on before queues stored it (the repository's checkout).
    pub(super) worktree: Option<String>,
    /// The agent the last `queue on` named for the queue's runs (`claude`,
    /// `pi` or `pi-anthropic`); none for `[workers] default_agent`.
    pub(super) agent: Option<String>,
    /// The pi model (`provider/model`) the last `queue on` named; none for
    /// the pi worker's default.
    pub(super) model: Option<String>,
}

/// A run the queue started (`todo_queue_runs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueueRun {
    pub(super) run_id: String,
    pub(super) item: String,
    pub(super) item_digest: String,
    pub(super) token: i64,
    pub(super) started_ms: u64,
    /// `done`, `blocked` or `aborted` once settled.
    pub(super) outcome: Option<String>,
    pub(super) error: Option<String>,
}

const QUEUE_COLUMNS: &str = "repo, mode, pause_reason, token, failures, status, reason, \
owner_pane, owner_session, workspace, updated_ms, worktree, agent, model";

fn queue_from_row(row: &rusqlite::Row<'_>) -> StoreResult<StoredQueue> {
    Ok(StoredQueue {
        repo: row.get(0)?,
        mode: row.get(1)?,
        pause_reason: row.get(2)?,
        token: row.get(3)?,
        failures: row.get(4)?,
        status: row.get(5)?,
        reason: row.get(6)?,
        owner_pane: row.get(7)?,
        owner_session: row.get(8)?,
        workspace: row.get(9)?,
        updated_ms: row.get::<_, i64>(10)? as u64,
        worktree: row.get(11)?,
        agent: row.get(12)?,
        model: row.get(13)?,
    })
}

const QUEUE_RUN_COLUMNS: &str = "run_id, item, item_digest, token, started_ms, outcome, error";

fn queue_run_from_row(row: &rusqlite::Row<'_>) -> StoreResult<QueueRun> {
    Ok(QueueRun {
        run_id: row.get(0)?,
        item: row.get(1)?,
        item_digest: row.get(2)?,
        token: row.get(3)?,
        started_ms: row.get::<_, i64>(4)? as u64,
        outcome: row.get(5)?,
        error: row.get(6)?,
    })
}

fn queue_of(conn: &Connection, repo: &str) -> StoreResult<Option<StoredQueue>> {
    conn.query_row(
        &format!("SELECT {QUEUE_COLUMNS} FROM todo_queues WHERE repo = ?1"),
        [repo],
        queue_from_row,
    )
    .optional()
}

impl Tx<'_> {
    pub(super) fn queue(&self, repo: &str) -> StoreResult<Option<StoredQueue>> {
        queue_of(self.tx, repo)
    }

    /// Turns `repo`'s queue on from `worktree`, owned by `owner`, its runs
    /// started with `agent` and `model` (which replace the ones an earlier
    /// `queue on` named): its pause and its circuit breaker's count are
    /// cleared.
    pub(super) fn queue_on(
        &self,
        repo: &str,
        worktree: &str,
        owner: &RunOwner<'_>,
        agent: Option<&str>,
        model: Option<&str>,
        at_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO todo_queues (repo, mode, pause_reason, token, failures, status, reason,
                 owner_pane, owner_session, workspace, updated_ms, worktree, agent, model)
             VALUES (?1, 'on', NULL, 0, 0, NULL, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (repo) DO UPDATE SET mode = 'on', pause_reason = NULL, failures = 0,
                 owner_pane = excluded.owner_pane, owner_session = excluded.owner_session,
                 workspace = excluded.workspace, updated_ms = excluded.updated_ms,
                 worktree = excluded.worktree, agent = excluded.agent, model = excluded.model",
            params![
                repo,
                owner.pane_id,
                owner.session_id,
                owner.workspace,
                at_ms as i64,
                worktree,
                agent,
                model
            ],
        )?;
        Ok(())
    }

    /// Pauses `repo`'s queue with `reason`; a repository without a queue
    /// gets a paused one.
    pub(super) fn queue_paused(&self, repo: &str, reason: &str, at_ms: u64) -> StoreResult<()> {
        self.tx.execute(
            "INSERT INTO todo_queues (repo, mode, pause_reason, token, failures, status, reason,
                 owner_pane, owner_session, workspace, updated_ms)
             VALUES (?1, 'paused', ?2, 0, 0, NULL, NULL, NULL, NULL, NULL, ?3)
             ON CONFLICT (repo) DO UPDATE SET mode = 'paused', pause_reason = excluded.pause_reason,
                 updated_ms = excluded.updated_ms",
            params![repo, reason, at_ms as i64],
        )?;
        Ok(())
    }

    /// Records the queue's last evaluation.
    pub(super) fn queue_evaluated(
        &self,
        repo: &str,
        status: &str,
        reason: &str,
        at_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "UPDATE todo_queues SET status = ?2, reason = ?3, updated_ms = ?4 WHERE repo = ?1",
            params![repo, status, reason, at_ms as i64],
        )?;
        Ok(())
    }

    /// Claims the queue's start of `run_id` with the fencing token `token`:
    /// only while the queue is on and its token is still `token`, which the
    /// claim moves on; records the run as the queue's. False when another
    /// start claimed it first or the queue was paused meanwhile.
    pub(super) fn queue_claim(
        &self,
        repo: &str,
        token: i64,
        run: &QueueRun,
        reason: &str,
        at_ms: u64,
    ) -> StoreResult<bool> {
        let moved = self.tx.execute(
            "UPDATE todo_queues SET token = token + 1, status = 'running', reason = ?3,
                 updated_ms = ?4
             WHERE repo = ?1 AND token = ?2 AND mode = 'on'",
            params![repo, token, reason, at_ms as i64],
        )?;
        if moved != 1 {
            return Ok(false);
        }
        self.tx.execute(
            "INSERT INTO todo_queue_runs (run_id, repo, item, item_digest, token, started_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run.run_id,
                repo,
                run.item,
                run.item_digest,
                token + 1,
                at_ms as i64
            ],
        )?;
        Ok(true)
    }

    /// The queue's runs of `repo` that ended and are not settled yet, oldest
    /// first, each with the run's status and error.
    pub(super) fn queue_unsettled(
        &self,
        repo: &str,
    ) -> StoreResult<Vec<(QueueRun, String, Option<String>)>> {
        let mut statement = self.tx.prepare(
            "SELECT q.run_id, q.item, q.item_digest, q.token, q.started_ms, q.outcome, q.error,
                 r.status, r.error
             FROM todo_queue_runs q JOIN runs r ON r.id = q.run_id
             WHERE q.repo = ?1 AND q.outcome IS NULL
                 AND r.status IN ('done', 'blocked', 'aborted')
             ORDER BY q.started_ms, q.rowid",
        )?;
        let rows = statement.query_map([repo], |row| {
            Ok((queue_run_from_row(row)?, row.get(7)?, row.get(8)?))
        })?;
        rows.collect()
    }

    /// Settles the queue's run with its outcome and the circuit breaker's
    /// count after it.
    pub(super) fn queue_settled(
        &self,
        repo: &str,
        run_id: &str,
        outcome: &str,
        error: Option<&str>,
        failures: u32,
        at_ms: u64,
    ) -> StoreResult<()> {
        self.tx.execute(
            "UPDATE todo_queue_runs SET outcome = ?2, error = ?3, settled_ms = ?4
             WHERE run_id = ?1 AND outcome IS NULL",
            params![run_id, outcome, error, at_ms as i64],
        )?;
        self.tx.execute(
            "UPDATE todo_queues SET failures = ?2 WHERE repo = ?1",
            params![repo, failures],
        )?;
        Ok(())
    }
}

impl Store {
    pub(super) fn queue(&self, repo: &str) -> StoreResult<Option<StoredQueue>> {
        queue_of(&lock(&self.conn), repo)
    }

    /// Every repository's queue.
    pub(super) fn queues(&self) -> StoreResult<Vec<StoredQueue>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!(
            "SELECT {QUEUE_COLUMNS} FROM todo_queues ORDER BY repo"
        ))?;
        let rows = statement.query_map([], queue_from_row)?;
        rows.collect()
    }

    /// The runs the queue of `repo` started, oldest first.
    pub(super) fn queue_runs(&self, repo: &str) -> StoreResult<Vec<QueueRun>> {
        let conn = lock(&self.conn);
        let mut statement = conn.prepare(&format!(
            "SELECT {QUEUE_RUN_COLUMNS} FROM todo_queue_runs WHERE repo = ?1
             ORDER BY started_ms, rowid"
        ))?;
        let rows = statement.query_map([repo], queue_run_from_row)?;
        rows.collect()
    }
}

/// The `questions.state` of a question that ended `how`.
pub(super) fn question_state(how: &str) -> &'static str {
    match how {
        "answered" => "answered",
        "cancelled" => "cancelled",
        _ => "expired",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::workers::{import_journal, now_ms, question_from_request, replay_journal};

    /// Takes the `runs` table back to before the landing column, for a test
    /// that runs the migrations after it again.
    const UNDO_RUN_LANDING: &str = "DROP INDEX runs_one_active_per_repo;
         ALTER TABLE runs DROP COLUMN landing;
         CREATE UNIQUE INDEX runs_one_active_per_repo ON runs (repo)
             WHERE status IN ('running', 'waiting') AND step != 'abort';";

    #[test]
    fn a_run_recorded_before_the_landing_column_derives_its_landing_from_its_step() {
        let dir = scratch("landing-column");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        store
            .connection()
            .execute_batch(&format!(
                "{UNDO_RUN_LANDING}
                 INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                     paths, check_name, check_argv, picked, created_ms, updated_ms, finish)
                 VALUES ('r-pushing1', '/repo', 't-abcd2345', 'push', 'running', 1, 'task',
                     'feat: x', '[]', 'ok', '[]', 'p1', 1, 1, '{{}}');
                 INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                     paths, check_name, check_argv, picked, created_ms, updated_ms, finish)
                 VALUES ('r-install1', '/other', 't-abcd2345', 'install', 'waiting', 1, 'task',
                     'feat: x', '[]', 'ok', '[]', 'p2', 1, 1, '{{}}');
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 2
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let pushing = store.run("r-pushing1").unwrap().unwrap();
        assert_eq!(pushing.landing, None);
        let derived = pushing.landing_state().unwrap();
        assert_eq!(derived.core.step, TodoStep::Push);
        assert_eq!(derived.core.status, TodoRunStatus::Running);
        assert_eq!(
            serde_json::to_value(derived.core.pending).unwrap(),
            json!("push")
        );
        assert_eq!(derived.core.picked.as_deref(), Some("p1"));
        assert!(derived.core.slot_held);
        assert!(
            derived.started,
            "a running step recorded before the core may have begun: it is reconciled from \
             git, not run again blindly"
        );
        let waiting = store.run("r-install1").unwrap().unwrap();
        let derived = waiting.landing_state().unwrap();
        assert_eq!(derived.core.pending, None);
        assert!(!derived.started);
        assert_eq!(
            serde_json::to_value(derived.core.attention).unwrap(),
            json!("install_failed")
        );
        // Once written, the run keeps the core's state in its column.
        let mut written = pushing.clone();
        written.landing = Some(written.landing_state().unwrap());
        store
            .transaction(|tx| tx.run_event(&mut written, &json!({"type": "t"}), false, 2))
            .unwrap();
        assert_eq!(
            store.run("r-pushing1").unwrap().unwrap().landing,
            written.landing
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_run_at_its_abort_claims_its_repository_only_while_it_restores_its_branch() {
        let dir = scratch("abort-claim");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let insert = |id: &str, landing: Option<&str>| {
            store.connection().execute(
                "INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                     paths, check_name, check_argv, created_ms, updated_ms, landing)
                 VALUES (?1, '/repo', 't-abcd2345', 'abort', 'running', 1, 'task', 'feat: x',
                     '[]', 'ok', '[]', 1, 1, ?2)",
                rusqlite::params![id, landing],
            )
        };
        // Stopping its worker only: the repository is free.
        insert("r-stopping", None).unwrap();
        assert!(store.active_run("/repo").unwrap().is_none());
        let restoring = r#"{"core": {"step": "abort", "status": "running",
            "fence": {"tenure_epoch": 0, "pane_generation": 0}, "slot_held": true,
            "pending": "restore_branch"}, "started": false}"#;
        insert("r-restore1", Some(restoring)).unwrap();
        assert_eq!(
            store.active_run("/repo").unwrap().unwrap().info.run_id,
            "r-restore1"
        );
        assert!(insert("r-restore2", Some(restoring)).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_landing_that_does_not_parse_fails_closed_and_is_kept_as_it_is() {
        let dir = scratch("landing-invalid");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        // Not JSON at all: the claim index and the active run's query must
        // not fail on it, and it claims nothing at its abort step.
        store
            .connection()
            .execute(
                "INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                     paths, check_name, check_argv, created_ms, updated_ms, landing)
                 VALUES ('r-garbled1', '/repo', 't-abcd2345', 'abort', 'running', 1, 'task',
                     'feat: x', '[]', 'ok', '[]', 1, 1, '{not json')",
                [],
            )
            .unwrap();
        assert!(store.active_run("/repo").unwrap().is_none());
        // JSON of another shape: not taken for a landing either.
        store
            .connection()
            .execute(
                "INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                     paths, check_name, check_argv, picked, created_ms, updated_ms, landing)
                 VALUES ('r-garbled2', '/other', 't-abcd2345', 'push', 'running', 1, 'task',
                     'feat: x', '[]', 'ok', '[]', 'p1', 1, 1, '{\"core\": 1}')",
                [],
            )
            .unwrap();
        for id in ["r-garbled1", "r-garbled2"] {
            let run = store.run(id).unwrap().unwrap();
            assert_eq!(run.landing, None, "{id}");
            assert!(run.landing_invalid.is_some(), "{id}");
            // Nothing is derived from its step instead.
            assert_eq!(run.landing_state(), None, "{id}");
            assert!(!run.in_landing(), "{id}");
        }
        // Written back, it keeps its text: nothing is lost or replaced.
        let mut run = store.run("r-garbled2").unwrap().unwrap();
        store
            .transaction(|tx| tx.run_event(&mut run, &json!({"type": "t"}), false, 2))
            .unwrap();
        let text: String = store
            .connection()
            .query_row(
                "SELECT landing FROM runs WHERE id = 'r-garbled2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(text, r#"{"core": 1}"#);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-worker-store-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn count(store: &Store, table: &str) -> i64 {
        store
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn append(store: &Store, worker_id: &str, event: &Value) -> StoreResult<i64> {
        let mut status = Status::new(worker_id.to_owned());
        status.apply(Direction::Herdr, event);
        store.transaction(|tx| {
            let seq = tx.event(&EventRow {
                worker_id,
                direction: Direction::Herdr,
                record: &Recorded::Event(event),
                ts_ms: 1,
            })?;
            tx.questions(seq, &[], &status)?;
            tx.worker(&status, seq)?;
            Ok(seq)
        })
    }

    #[cfg(unix)]
    #[test]
    fn in_memory_lost_admissions_follow_process_identity_and_list_pending_starts() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let pid = std::process::id();
        let start =
            crate::platform::process_start_token(pid).expect("this process has a start token");
        append(
            &store,
            "w1",
            &json!({
                "type": "started", "pid": pid, "process_start": start,
                "cwd": "/repo", "name": "lost worker",
            }),
        )
        .unwrap();
        store
            .connection()
            .execute("UPDATE workers SET lost = 1 WHERE id = 'w1'", [])
            .unwrap();
        for holder in ["bound", "pending"] {
            store
                .transaction(|tx| {
                    tx.admit(&NewAdmission {
                        tenure: None,
                        holder,
                        provider: "claude",
                        repo: "/repo",
                        run: None,
                        server: (pid, Some(start)),
                        ignored: false,
                        at_ms: 42,
                    })
                })
                .unwrap();
        }
        store.bind_admission("bound", "w1").unwrap();
        let rows = store.repository_admissions("/repo").unwrap();
        assert_eq!(
            rows.len(),
            2,
            "a lost but living worker still holds its slot"
        );
        assert_eq!(rows[0].worker_id.as_deref(), Some("w1"));
        assert_eq!(rows[1].worker_id, None);
        assert_eq!(rows[1].provider, "claude");
        assert_eq!(rows[1].admitted_ms, 42);
        assert_eq!(rows[1].server_start, Some(start));
        // A recycled PID must not keep the old process's slot.
        store.connection().execute(
            "UPDATE events SET body = json_set(body, '$.process_start', ?1) WHERE type = 'started'",
            [start.wrapping_add(1) as i64],
        ).unwrap();
        assert_eq!(store.held_admissions("claude", "/repo").unwrap().len(), 1);
        // Also force an actual process exit, not just a token mismatch.
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let child_start = crate::platform::process_start_token(child_pid).unwrap();
        store
            .connection()
            .execute("UPDATE workers SET pid = ?1 WHERE id = 'w1'", [child_pid])
            .unwrap();
        store.connection().execute(
            "UPDATE events SET body = json_set(body, '$.process_start', ?1) WHERE type = 'started'",
            [child_start as i64],
        ).unwrap();
        assert_eq!(store.held_admissions("claude", "/repo").unwrap().len(), 2);
        drop(child.stdin.take());
        child.wait().unwrap();
        assert_eq!(store.held_admissions("claude", "/repo").unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_lost_workers_row_without_a_start_token_follows_its_process_group() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        // In a process group of its own, as a worker's CLI is (its pid
        // leads the group).
        let mut command = std::process::Command::new("cat");
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        // A journal from before herdr recorded a start token: the pid is
        // all it has.
        append(
            &store,
            "w-legacy",
            &json!({"type": "started", "pid": pid, "cwd": "/repo", "name": "legacy"}),
        )
        .unwrap();
        store
            .connection()
            .execute("UPDATE workers SET lost = 1 WHERE id = 'w-legacy'", [])
            .unwrap();
        store
            .transaction(|tx| {
                tx.admit(&NewAdmission {
                    tenure: None,
                    holder: "bound",
                    provider: "claude",
                    repo: "/repo",
                    run: None,
                    server: (1, Some(1)),
                    ignored: false,
                    at_ms: 1,
                })
            })
            .unwrap();
        store.bind_admission("bound", "w-legacy").unwrap();
        // Documented limit: with no start token only the pid decides, so a
        // process group that reused the pid keeps the slot too. What it
        // never does is free the slot under a worker that still runs.
        assert_eq!(store.held_admissions("claude", "/repo").unwrap().len(), 1);
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(store.held_admissions("claude", "/repo").unwrap().is_empty());
    }

    #[test]
    fn the_lost_workers_start_event_is_looked_up_by_its_own_index() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        let plan = {
            let conn = lock(&store.conn);
            let mut statement = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {}", held_admissions_sql()))
                .unwrap();
            let rows = statement
                .query_map(["claude", "/repo"], |row| row.get::<_, String>(3))
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n")
        };
        assert!(
            plan.contains("events_by_worker_kind"),
            "the start-token lookup walks the worker's journal: {plan}"
        );
    }

    #[test]
    fn retirement_and_binding_are_mutually_exclusive() {
        let store = Store::open(Path::new(":memory:")).unwrap();
        // The worker of one row the store holds, and none for the others.
        append(
            &store,
            "w2",
            &json!({"type": "started", "pid": 1, "cwd": "/repo", "name": "w2"}),
        )
        .unwrap();
        for holder in ["retire-first", "bound-unrecorded", "bound-recorded"] {
            store
                .transaction(|tx| {
                    tx.admit(&NewAdmission {
                        tenure: None,
                        holder,
                        provider: "claude",
                        repo: "/repo",
                        run: None,
                        server: (1, Some(1)),
                        ignored: false,
                        at_ms: 1,
                    })
                })
                .unwrap();
        }
        // A retired start can never name its worker, so a delayed spawn of
        // the server that admitted it takes no second slot.
        assert!(store
            .retire_unrecorded_admission("retire-first", "handoff", 2)
            .unwrap());
        assert_eq!(
            store.bind_admission("retire-first", "w1").unwrap(),
            BindOutcome::Ended,
            "a retired admission cannot spawn later"
        );
        // A row that names a worker the store does not hold is still
        // unrecorded: retiring it is the caller's decision, taken from the
        // evidence that its server is gone.
        store.bind_admission("bound-unrecorded", "w-never").unwrap();
        assert!(store
            .retire_unrecorded_admission("bound-unrecorded", "handoff", 2)
            .unwrap());
        // Once the worker's row exists the row is recorded, and a retire
        // that races the write cannot free a running worker's slot.
        store.bind_admission("bound-recorded", "w2").unwrap();
        assert!(
            !store
                .retire_unrecorded_admission("bound-recorded", "handoff", 2)
                .unwrap(),
            "a recorded worker keeps its slot"
        );
        let rows = store.repository_admissions("/repo").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].holder, "bound-recorded");
        assert_eq!(rows[0].worker_id.as_deref(), Some("w2"));
    }

    #[test]
    fn a_fresh_store_has_the_schema_and_no_workers() {
        let dir = scratch("fresh");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        assert!(store.load_all().unwrap().is_empty());
        let version: String = store
            .connection()
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, MIGRATIONS.len().to_string());
        let mode: String = store
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        drop(store);
        // Opening again runs no migration twice.
        let store = Store::open(&path).unwrap();
        assert_eq!(count(&store, "events"), 0);

        // A database from a newer herdr is refused, not misread.
        store
            .connection()
            .execute(
                "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        drop(store);
        let refused = Store::open(&path).err().unwrap().to_string();
        assert!(refused.contains("newer"), "{refused}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_event_and_its_projection_commit_together_or_not_at_all() {
        let dir = scratch("atomic");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let started = json!({"type": "started", "cwd": "/repo", "name": "one"});
        let seq = append(&store, "w1", &started).unwrap();
        assert_eq!((count(&store, "events"), count(&store, "workers")), (1, 1));
        assert_eq!(store.load("w1").unwrap().unwrap().last_seq, seq);

        // The projection write fails after the event was inserted: neither
        // stays.
        store
            .connection()
            .execute_batch(
                "CREATE TRIGGER forced BEFORE UPDATE ON workers
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();
        let error = append(
            &store,
            "w1",
            &json!({"type": "signal", "signal": "SIGTERM"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("forced failure"), "{error}");
        assert_eq!((count(&store, "events"), count(&store, "workers")), (1, 1));
        assert_eq!(store.load("w1").unwrap().unwrap().last_seq, seq);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_load_reads_the_slots_branch_and_the_turns_and_last_events_times() {
        let dir = scratch("event-facts");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let events = [
            (
                Direction::Herdr,
                json!({"type": "started", "cwd": "/repo",
                    "folder_slot": {"name": "worker", "branch": "todo/t-abcd2345-1"}}),
            ),
            (
                Direction::In,
                json!({"type": "user", "message": {"content": "go"}}),
            ),
            (
                Direction::Out,
                json!({"type": "assistant", "message": {"content": []}}),
            ),
        ];
        let mut status = Status::new("w1".to_owned());
        for (index, (direction, event)) in events.iter().enumerate() {
            status.apply(*direction, event);
            store
                .transaction(|tx| {
                    let seq = tx.event(&EventRow {
                        worker_id: "w1",
                        direction: *direction,
                        record: &Recorded::Event(event),
                        ts_ms: 1_000 * (index as u64 + 1),
                    })?;
                    if *direction == Direction::In {
                        status.turn_seq = Some(seq);
                    }
                    tx.worker(&status, seq)?;
                    Ok(seq)
                })
                .unwrap();
        }
        let loaded = Store::open(&path).unwrap().load("w1").unwrap().unwrap();
        assert_eq!(loaded.slot.as_deref(), Some("worker"));
        assert_eq!(loaded.branch.as_deref(), Some("todo/t-abcd2345-1"));
        assert_eq!(
            (loaded.turn_started_ms, loaded.last_event_ms),
            (Some(2_000), Some(3_000))
        );
        let all = Store::open(&path).unwrap().load_all().unwrap();
        let loaded = all.values().next().unwrap();
        assert_eq!(loaded.branch.as_deref(), Some("todo/t-abcd2345-1"));
        assert_eq!(loaded.turn_started_ms, Some(2_000));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Writes each event with the worker's row as it is after it, as the
    /// supervisor does in one transaction.
    fn record_all(store: &Store, worker_id: &str, events: &[(Direction, Value)]) -> Status {
        let mut status = Status::new(worker_id.to_owned());
        for (direction, event) in events {
            status.apply(*direction, event);
            store
                .transaction(|tx| {
                    let seq = tx.event(&EventRow {
                        worker_id,
                        direction: *direction,
                        record: &Recorded::Event(event),
                        ts_ms: 1,
                    })?;
                    tx.worker(&status, seq)
                })
                .unwrap();
        }
        status
    }

    /// Claude's `result` reports the session's usage so far: the row holds
    /// the latest, written with that event, and a worker recorded before the
    /// columns gets the same from its events. A result whose cost is the
    /// credit ledger's for a whole run, and a pi worker's, leave its cost
    /// unknown, never 0.
    #[test]
    fn a_runs_usage_is_written_at_its_result_and_filled_for_older_rows() {
        let dir = scratch("run-usage");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let usage = |output: u64, cost: f64| {
            json!({"type": "result", "subtype": "success", "total_cost_usd": cost,
                "modelUsage": {"claude-opus-5-5": {"inputTokens": 10, "outputTokens": output,
                "cacheReadInputTokens": 100, "cacheCreationInputTokens": 20, "costUSD": cost}}})
        };
        let claude = [
            (
                Direction::Herdr,
                json!({"type": "started", "cwd": "/repo", "agent": "claude"}),
            ),
            (
                Direction::Out,
                json!({"type": "system", "subtype": "init", "model": "claude-opus-5-5"}),
            ),
            (Direction::Out, usage(5, 0.5)),
            (Direction::Out, usage(8, 0.75)),
        ];
        let pi_credits = [
            (
                Direction::Herdr,
                json!({"type": "started", "cwd": "/repo", "agent": "pi",
                    "model": "herdr-anthropic/claude-sonnet-5-5"}),
            ),
            (
                Direction::Out,
                json!({"type": "result", "subtype": "success", "total_cost_usd": 3.0,
                    "credit_accounting": {"spent_usd": 3.0, "worker": "r-1"}}),
            ),
        ];
        let written = record_all(&store, "w1", &claude);
        record_all(&store, "w2", &pi_credits);
        record_all(
            &store,
            "w3",
            &[(Direction::Herdr, json!({"type": "started", "cwd": "/repo"}))],
        );
        let check = |store: &Store| {
            let one = store.load("w1").unwrap().unwrap();
            assert_eq!(one.agent.as_deref(), Some("claude"));
            assert_eq!(one.ran_model.as_deref(), Some("claude-opus-5-5"));
            assert_eq!(one.cost_usd, Some(0.75));
            assert_eq!(one.usage, written.usage);
            let usage = one.usage.unwrap();
            assert_eq!(usage.len(), 1, "{usage:?}");
            assert_eq!(
                (
                    usage[0].input_tokens,
                    usage[0].cache_creation_input_tokens,
                    usage[0].cache_read_input_tokens,
                    usage[0].output_tokens,
                    usage[0].cost_usd
                ),
                (10, 20, 100, 8, Some(0.75))
            );
            let two = store.load("w2").unwrap().unwrap();
            assert_eq!(two.agent.as_deref(), Some("pi-anthropic"));
            assert_eq!((two.usage, two.cost_usd), (None, None));
            let three = store.load("w3").unwrap().unwrap();
            assert_eq!(
                (three.agent, three.ran_model, three.usage, three.cost_usd),
                (None, None, None, None)
            );
        };
        check(&store);
        // A database from before the columns.
        store
            .connection()
            .execute_batch(&format!(
                "ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 1
            ))
            .unwrap();
        drop(store);
        check(&Store::open(&path).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_attention_columns_are_filled_from_the_events_of_older_rows() {
        let dir = scratch("attention-columns");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let events = [
            (Direction::Herdr, json!({"type": "started", "cwd": "/repo"})),
            (
                Direction::In,
                json!({"type": "user", "message": {"content": "go"}}),
            ),
            (
                Direction::Out,
                json!({"type": "result", "subtype": "success", "is_error": false}),
            ),
            (Direction::Herdr, json!({"type": "exited", "code": 0})),
        ];
        let mut status = Status::new("w1".to_owned());
        let mut seqs = Vec::new();
        for (index, (direction, event)) in events.iter().enumerate() {
            status.apply(*direction, event);
            seqs.push(
                store
                    .transaction(|tx| {
                        let seq = tx.event(&EventRow {
                            worker_id: "w1",
                            direction: *direction,
                            record: &Recorded::Event(event),
                            ts_ms: 10 + index as u64,
                        })?;
                        tx.worker(&status, seq)?;
                        Ok(seq)
                    })
                    .unwrap(),
            );
        }
        // A database from before the columns: written as the first
        // migration left it.
        store
            .connection()
            .execute_batch(
                "ALTER TABLE workers DROP COLUMN turn_seq;
                 ALTER TABLE workers DROP COLUMN turn_end_seq;
                 ALTER TABLE workers DROP COLUMN gone_seq;
                 DROP TABLE receipts;
                 ALTER TABLE workers DROP COLUMN owner_pane;
                 ALTER TABLE workers DROP COLUMN owner_session;
                 ALTER TABLE workers DROP COLUMN acked_seq;
                 ALTER TABLE workers DROP COLUMN owner_gone;
                 ALTER TABLE workers DROP COLUMN degraded;
                 ALTER TABLE questions DROP COLUMN escalated;
                 ALTER TABLE workers DROP COLUMN item;
                 ALTER TABLE workers DROP COLUMN repo;
                 ALTER TABLE workers DROP COLUMN started_ms;
                 ALTER TABLE workers DROP COLUMN ended_ms;
                 ALTER TABLE workers DROP COLUMN done_commits;
                 ALTER TABLE workers DROP COLUMN questions_asked;
                 ALTER TABLE workers DROP COLUMN ended_mid_turn;
                 ALTER TABLE workers DROP COLUMN item_title;
                 ALTER TABLE workers DROP COLUMN verification;
                 ALTER TABLE workers DROP COLUMN takeover_id;
                 DROP TABLE coordinator_bindings;
                 DROP TABLE coordinators;
                 ALTER TABLE workers DROP COLUMN owner_coordinator_id;
                 ALTER TABLE workers DROP COLUMN broker;
                 ALTER TABLE workers DROP COLUMN broker_seq;
                 ALTER TABLE workers DROP COLUMN continuity_gap;
                 DROP TABLE runs;
                 DROP TABLE usage_gates;
                 DROP TABLE attempts;
                 DROP TABLE landings;
                 DROP TABLE item_history;
                 DROP TABLE coordinator_overrides;
                 DROP TABLE todo_chains;
                 ALTER TABLE workers DROP COLUMN coordinates;
                 DROP TABLE report_occurrences;
                 DROP TABLE reports;
                 DROP TABLE todo_queue_runs;
                 DROP TABLE todo_queues;
                 DROP TABLE coordinator_would_deny;
                 DROP TABLE capability_grants;
                 DROP TABLE inbox;
                 DELETE FROM meta WHERE key IN ('incarnation', 'inbox_from');
                 DROP TABLE decisions;
                 DROP TABLE capability_grant_log;
                 ALTER TABLE workers DROP COLUMN end_by_owner;
                 DROP TABLE supervision_holds;
                 DROP TABLE supervision_findings;
                 DROP TABLE supervision_annotations;
                 DROP TABLE supervision_deliveries;
                 DROP TABLE usage_admissions;
                 DROP TABLE tenure_steps;
                 DROP TABLE tenure_pending_ends;
                 DROP INDEX events_by_worker_kind;
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
        drop(store);
        let loaded = Store::open(&path).unwrap().load("w1").unwrap().unwrap();
        assert_eq!(
            (loaded.turn_seq, loaded.turn_end_seq, loaded.gone_seq),
            (Some(seqs[1]), seqs[3], seqs[3])
        );
        // The run's times come from its events; an older worker has no
        // item, so `worker.runs` lists it unassigned.
        assert_eq!(
            (loaded.started_ms, loaded.ended_ms, loaded.ended_mid_turn),
            (Some(10), Some(13), false)
        );
        assert_eq!((loaded.item, loaded.repo), (None, None));
        assert_eq!(loaded.item_title, None);
        assert!(loaded.done_commits.is_empty());
        assert_eq!(loaded.verification, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_usage_gates_per_repository_become_one_latch_per_provider_account() {
        let dir = scratch("usage-gate-per-account");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        // A database from before the migration, with each repository's
        // gate: Claude's keyed by the repository, OpenAI's by `openai:`.
        store
            .connection()
            .execute_batch(&format!(
                "{UNDO_RUN_LANDING}
                 DROP TABLE usage_admissions;
                 DROP TABLE tenure_steps;
                 DROP TABLE tenure_pending_ends;
                 DROP TABLE supervision_annotations;
                 ALTER TABLE supervision_findings DROP COLUMN rule_version;
                 ALTER TABLE supervision_holds DROP COLUMN expected_event;
                 ALTER TABLE todo_queues DROP COLUMN agent;
                 ALTER TABLE todo_queues DROP COLUMN model;
                 DROP INDEX events_by_worker_kind;
                 INSERT INTO usage_gates (repo, closed, body, updated_ms) VALUES
                     ('/a', 0, '{{\"state\": \"open\"}}', 40),
                     ('/b', 1, '{{\"state\": \"unknown\"}}', 30),
                     ('/c', 1, '{{\"state\": \"high\", \"last_good\": 1}}', 10),
                     ('/d', 1, '{{\"decision\": \"refuse\"}}', 5),
                     ('openai:/a', 1, '{{\"state\": \"unknown\"}}', 20),
                     ('openai:/b', 0, '{{\"state\": \"open\"}}', 50);
                 ALTER TABLE todo_queues DROP COLUMN worktree;
                 ALTER TABLE todo_chains DROP COLUMN worktree;
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 11
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut keys = store.usage_gate_keys().unwrap();
        keys.sort();
        assert_eq!(keys, ["provider:claude", "provider:openai"]);
        // Closed if any was: high over unknown, the latest high first.
        assert!(store.usage_gate_closed("provider:claude").unwrap());
        assert_eq!(
            store.usage_gate_body("provider:claude").unwrap(),
            json!({"state": "high", "last_good": 1})
        );
        assert!(store.usage_gate_closed("provider:openai").unwrap());
        assert_eq!(
            store.usage_gate_body("provider:openai").unwrap()["state"],
            "unknown"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_item_title_is_kept_and_filled_from_the_started_event_of_older_rows() {
        let dir = scratch("item-title");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let started = json!({"type": "started", "cwd": "/repo", "item": "t-abcd2345",
            "item_title": "Items popup"});
        append(&store, "w1", &started).unwrap();
        append(&store, "w2", &json!({"type": "started", "cwd": "/repo"})).unwrap();
        assert_eq!(
            store.load("w1").unwrap().unwrap().item_title.as_deref(),
            Some("Items popup")
        );
        // A database from before the column: the migration fills it from
        // the `started` event.
        store
            .connection()
            .execute_batch(&format!(
                "ALTER TABLE workers DROP COLUMN item_title;
                 ALTER TABLE workers DROP COLUMN verification;
                 ALTER TABLE workers DROP COLUMN takeover_id;
                 DROP TABLE coordinator_bindings;
                 DROP TABLE coordinators;
                 ALTER TABLE workers DROP COLUMN owner_coordinator_id;
                 ALTER TABLE workers DROP COLUMN broker;
                 ALTER TABLE workers DROP COLUMN broker_seq;
                 ALTER TABLE workers DROP COLUMN continuity_gap;
                 DROP TABLE runs;
                 DROP TABLE usage_gates;
                 DROP TABLE attempts;
                 DROP TABLE landings;
                 DROP TABLE item_history;
                 DROP TABLE coordinator_overrides;
                 DROP TABLE todo_chains;
                 ALTER TABLE workers DROP COLUMN coordinates;
                 DROP TABLE report_occurrences;
                 DROP TABLE reports;
                 DROP TABLE todo_queue_runs;
                 DROP TABLE todo_queues;
                 DROP TABLE coordinator_would_deny;
                 DROP TABLE capability_grants;
                 DROP TABLE inbox;
                 DELETE FROM meta WHERE key IN ('incarnation', 'inbox_from');
                 DROP TABLE decisions;
                 DROP TABLE capability_grant_log;
                 ALTER TABLE workers DROP COLUMN end_by_owner;
                 DROP TABLE supervision_holds;
                 DROP TABLE supervision_findings;
                 DROP TABLE supervision_annotations;
                 DROP TABLE supervision_deliveries;
                 DROP TABLE usage_admissions;
                 DROP TABLE tenure_steps;
                 DROP TABLE tenure_pending_ends;
                 DROP INDEX events_by_worker_kind;
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 42
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.load("w1").unwrap().unwrap().item_title.as_deref(),
            Some("Items popup")
        );
        assert_eq!(store.load("w2").unwrap().unwrap().item_title, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn attempts_and_landings_are_filled_from_the_events_of_older_runs() {
        let dir = scratch("attempts");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        append(&store, "w1", &json!({"type": "started", "cwd": "/repo"})).unwrap();
        append(&store, "w2", &json!({"type": "started", "cwd": "/repo"})).unwrap();
        let run = "r-legacy01";
        let note = |event: Value| store.transaction(|tx| tx.run_note(run, &event, 7)).unwrap();
        note(json!({"type": "run_created", "task": "task one", "base": "base0"}));
        let first = note(json!({"type": "run_event", "kind": "review", "attempt": 1,
            "event": {"kind": "review", "commits": ["c0", "c1"]}}));
        note(
            json!({"type": "run_resumed", "event": first, "action": "retry",
            "task": "review one"}),
        );
        note(json!({"type": "run_restarted", "previous_worker_id": "w1", "attempt": 2}));
        let second = note(json!({"type": "run_event", "kind": "review", "attempt": 2,
            "event": {"kind": "review", "commits": ["c2"]}}));
        note(json!({"type": "run_resumed", "event": second, "action": "approve"}));
        note(json!({"type": "run_verified", "head": "c2"}));
        note(json!({"type": "run_picked", "commit": "landed1"}));
        store
            .connection()
            .execute_batch(&format!(
                "{UNDO_RUN_LANDING}
                 INSERT INTO runs (id, repo, item, step, status, attempt, base, worker_id, branch,
                     task, message, paths, check_name, check_argv, created_ms, updated_ms)
                 VALUES ('{run}', '/repo', 't-abcd2345', 'done', 'done', 2, 'base0', 'w2',
                     'todo/t-abcd2345-{run}-2', 'review one', 'feat: x', '[]', 'ok', '[]', 1, 1);
                 UPDATE workers SET verification = '{{\"verdict\":\"verified\"}}' WHERE id = 'w2';
                 DROP TABLE attempts;
                 DROP TABLE landings;
                 DROP TABLE item_history;
                 DROP TABLE coordinator_overrides;
                 ALTER TABLE runs DROP COLUMN owner_coordinator_id;
                 ALTER TABLE workers DROP COLUMN coordinates;
                 ALTER TABLE coordinators DROP COLUMN worker_id;
                 ALTER TABLE coordinators DROP COLUMN headless;
                 ALTER TABLE coordinators DROP COLUMN token_sha256;
                 ALTER TABLE coordinators DROP COLUMN holder_pid;
                 ALTER TABLE coordinators DROP COLUMN holder_start;
                 DROP TABLE todo_chains;
                 DROP TABLE report_occurrences;
                 DROP TABLE reports;
                 DROP TABLE todo_queue_runs;
                 DROP TABLE todo_queues;
                 DROP TABLE coordinator_would_deny;
                 DROP TABLE capability_grants;
                 DROP TABLE inbox;
                 DELETE FROM meta WHERE key IN ('incarnation', 'inbox_from');
                 DROP TABLE decisions;
                 DROP TABLE capability_grant_log;
                 ALTER TABLE workers DROP COLUMN end_by_owner;
                 DROP TABLE supervision_holds;
                 DROP TABLE supervision_findings;
                 DROP TABLE supervision_annotations;
                 DROP TABLE supervision_deliveries;
                 DROP TABLE usage_admissions;
                 DROP TABLE tenure_steps;
                 DROP TABLE tenure_pending_ends;
                 DROP INDEX events_by_worker_kind;
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 31
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let attempts = store.attempts(run).unwrap();
        assert_eq!(attempts.len(), 2, "{attempts:#?}");
        let (one, two) = (&attempts[0], &attempts[1]);
        assert_eq!(
            (one.number, one.worker_id.as_deref(), one.task.as_str()),
            (1, Some("w1"), "task one")
        );
        assert_eq!(
            one.branch.as_deref(),
            Some(format!("todo/t-abcd2345-{run}-1").as_str())
        );
        assert_eq!(
            one.attempt,
            Attempt {
                base: Some("base0".into()),
                commit: Some("c1".into()),
                review_event: Some(first),
                review_decision: Some("retry".into()),
                review_text: Some("review one".into()),
                ..Attempt::default()
            }
        );
        assert_eq!(
            (two.number, two.worker_id.as_deref(), two.task.as_str()),
            (2, Some("w2"), "review one")
        );
        assert_eq!(
            two.attempt,
            Attempt {
                base: Some("base0".into()),
                commit: Some("c2".into()),
                review_event: Some(second),
                review_decision: Some("approve".into()),
                verification: Some("{\"verdict\":\"verified\"}".into()),
                ..Attempt::default()
            }
        );
        // The run reads its current attempt.
        assert_eq!(store.run(run).unwrap().unwrap().current, two.attempt);
        let landing = store.landing_of("landed1").unwrap().unwrap();
        assert_eq!(
            (
                landing.run_id.as_str(),
                landing.attempt,
                landing.item.as_str(),
                landing.worker_commit.as_deref(),
                landing.worker_id.as_deref(),
                landing.ts_ms,
            ),
            (run, 2, "t-abcd2345", Some("c2"), Some("w2"), Some(7))
        );
        assert_eq!(
            store.landing_of("c2").unwrap().unwrap().landed_sha,
            "landed1"
        );
        assert_eq!(store.landing_of("c1").unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_stopped_before_the_acknowledgement_are_not_shown_after_the_migration() {
        let dir = scratch("stop-acknowledged");
        let path = dir.join(STORE_FILE);
        let store = Store::open(&path).unwrap();
        let insert = |store: &Store, id: &str, status: &str, finish: &str| {
            store
                .connection()
                .execute(
                    "INSERT INTO runs (id, repo, item, step, status, attempt, task, message,
                         paths, check_name, check_argv, created_ms, updated_ms, finish)
                     VALUES (?1, ?1, 't-abcd2345', 'done', ?2, 1, 'task', 'feat: x', '[]',
                         'ok', '[]', 1, 1, ?3)",
                    rusqlite::params![id, status, finish],
                )
                .unwrap();
        };
        let shown = |store: &Store| -> Vec<String> {
            store
                .runs_shown()
                .unwrap()
                .into_iter()
                .map(|run| run.info.run_id)
                .collect()
        };
        // A database from before the migration: a stop that sent its notice
        // under the old rule, a done run without a stop reason, and one
        // blocked with a reason it cannot have reached the close with.
        insert(
            &store,
            "r-stopped1",
            "done",
            r#"{"stop_reason": "why", "stop_notified": true}"#,
        );
        insert(&store, "r-landed01", "done", r#"{"note": "n"}"#);
        insert(&store, "r-blocked1", "blocked", r#"{"stop_reason": "why"}"#);
        store
            .connection()
            .execute_batch(&format!(
                "{UNDO_RUN_LANDING}
                 ALTER TABLE workers DROP COLUMN agent;
                 ALTER TABLE workers DROP COLUMN ran_model;
                 ALTER TABLE workers DROP COLUMN usage;
                 ALTER TABLE workers DROP COLUMN cost_usd;
                 UPDATE meta SET value = '{}' WHERE key = 'schema_version';",
                MIGRATIONS.len() - 3
            ))
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        assert!(shown(&store).is_empty());
        let stopped = store.run("r-stopped1").unwrap().unwrap();
        assert!(stopped.finish.stop_acknowledged);
        assert_eq!(stopped.finish.stop_reason.as_deref(), Some("why"));
        assert!(
            !store
                .run("r-landed01")
                .unwrap()
                .unwrap()
                .finish
                .stop_acknowledged
        );
        assert!(
            !store
                .run("r-blocked1")
                .unwrap()
                .unwrap()
                .finish
                .stop_acknowledged
        );
        // A stop recorded after the migration shows until acknowledged.
        insert(&store, "r-stopped2", "done", r#"{"stop_reason": "why"}"#);
        assert_eq!(shown(&store), ["r-stopped2"]);
        drop(store);
        // Opening again runs no migration twice.
        let store = Store::open(&path).unwrap();
        assert_eq!(shown(&store), ["r-stopped2"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_runs_events_append_its_items_history_which_takes_no_change() {
        let dir = scratch("item-history");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        store
            .connection()
            .execute_batch(
                "INSERT INTO runs (id, repo, item, step, status, attempt, task, message, paths,
                     check_name, check_argv, created_ms, updated_ms)
                 VALUES ('r-aaaaaaaa', '/repo', 't-abcd2345', 'start', 'running', 1, 'task',
                     'feat: x', '[]', 'ok', '[]', 1, 1);",
            )
            .unwrap();
        let mut run = store.run("r-aaaaaaaa").unwrap().unwrap();
        let write = |run: &mut Run, event: Value, at: u64| {
            store
                .transaction(|tx| tx.run_event(run, &event, false, at))
                .unwrap();
        };
        write(
            &mut run,
            json!({"type": "run_created", "item_text": "- [ ] X [t-abcd2345]\n",
                "item_ids": ["t-abcd2345", "t-bcde3456"]}),
            10,
        );
        // Events that are not part of the item's life write no record.
        write(&mut run, json!({"type": "run_installed"}), 11);
        write(
            &mut run,
            json!({"type": "run_event", "kind": "review", "event": {}}),
            12,
        );
        run.info.status = TodoRunStatus::Blocked;
        write(
            &mut run,
            json!({"type": "run_event", "kind": "blocked", "event": {"error": "no base"}}),
            13,
        );
        run.finish.close = Some("## Decided\n".into());
        write(
            &mut run,
            json!({"type": "run_todo_committed", "item_text": "- [ ] X [t-abcd2345]\n  more\n",
                "follow_ups": ["t-cdef4567"]}),
            14,
        );
        let records = store
            .item_history(Some("/repo"), Some("t-abcd2345"))
            .unwrap();
        let kinds: Vec<_> = records.iter().map(|record| record.event.kind).collect();
        assert_eq!(
            kinds,
            [
                HistoryEventKind::Claimed,
                HistoryEventKind::Blocked,
                HistoryEventKind::Closed
            ]
        );
        let (claimed, blocked, closed) = (&records[0], &records[1], &records[2]);
        assert_eq!(claimed.ids_at_claim, ["t-abcd2345", "t-bcde3456"]);
        assert_eq!(
            (
                claimed.event.item_text.as_deref(),
                claimed.event.ts_ms,
                claimed.event.run_id.as_deref(),
                claimed.event.attempt
            ),
            (
                Some("- [ ] X [t-abcd2345]\n"),
                10,
                Some("r-aaaaaaaa"),
                Some(1)
            )
        );
        assert_eq!(blocked.event.text.as_deref(), Some("no base"));
        assert_eq!(closed.event.text.as_deref(), Some("## Decided\n"));
        assert_eq!(
            closed.event.item_text.as_deref(),
            Some("- [ ] X [t-abcd2345]\n  more\n")
        );
        assert_eq!(closed.event.follow_ups, ["t-cdef4567"]);
        assert!(store.item_history(Some("/other"), None).unwrap().is_empty());

        // Append-only: a change or a deletion is refused.
        for change in [
            "UPDATE item_history SET text = 'x'",
            "DELETE FROM item_history",
        ] {
            let refused = store
                .connection()
                .execute(change, [])
                .unwrap_err()
                .to_string();
            assert!(refused.contains("append-only"), "{refused}");
        }
        assert_eq!(count(&store, "item_history"), 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_cleanup_settles_only_rows_still_pending() {
        let dir = scratch("cleanup-guard");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let question = json!({"type": "question", "input": {},
            "question": question_from_request(
                "q1", &json!({"tool_name": "Bash", "input": {"command": "ls"}}), "asked")});
        append(&store, "w1", &question).unwrap();
        // Another writer began answering it; this status has not seen that.
        store
            .connection()
            .execute("UPDATE questions SET state = 'answering'", [])
            .unwrap();
        let mut status = Status::new("w1".to_owned());
        status.apply(Direction::Herdr, &question);
        let before = status.open_questions();
        let ended = json!({"type": "result", "subtype": "success", "is_error": false});
        status.apply(Direction::Out, &ended);
        assert!(status.questions.is_empty());
        store
            .transaction(|tx| {
                let seq = tx.event(&EventRow {
                    worker_id: "w1",
                    direction: Direction::Out,
                    record: &Recorded::Event(&ended),
                    ts_ms: 1,
                })?;
                tx.questions(seq, &before, &status)
            })
            .unwrap();
        let state: String = store
            .connection()
            .query_row("SELECT state FROM questions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(state, "answering");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seq_is_never_reused() {
        let dir = scratch("seq");
        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let event = json!({"type": "started", "cwd": "/repo"});
        let first = append(&store, "w1", &event).unwrap();
        let second = append(&store, "w2", &event).unwrap();
        assert!(second > first);
        store
            .connection()
            .execute("DELETE FROM events WHERE seq = ?1", [second])
            .unwrap();
        assert!(append(&store, "w1", &event).unwrap() > second);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_servers_append_concurrently_without_losing_or_reusing_a_seq() {
        const PER_THREAD: usize = 200;
        let dir = scratch("concurrent");
        let path = dir.join(STORE_FILE);
        Store::open(&path).unwrap();
        // One connection per thread, as two servers of a handoff have.
        let threads: Vec<_> = ["w1", "w2"]
            .into_iter()
            .map(|worker_id| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let store = Store::open(&path).unwrap();
                    let event = json!({"type": "tool_sessions", "sessions": []});
                    (0..PER_THREAD)
                        .map(|_| append(&store, worker_id, &event).unwrap())
                        .collect::<Vec<i64>>()
                })
            })
            .collect();
        let mut all = Vec::new();
        for thread in threads {
            let seqs = thread.join().unwrap();
            assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
            all.extend(seqs);
        }
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 2 * PER_THREAD);
        let store = Store::open(&path).unwrap();
        assert_eq!(count(&store, "events"), (2 * PER_THREAD) as i64);
        assert_eq!(count(&store, "workers"), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_projections_rebuild_the_folded_state() {
        let dir = scratch("rebuild");
        let journal = dir.join("w1.jsonl");
        let question = |id: &str| {
            json!({"type": "question", "input": {"command": "ls"},
                "question": question_from_request(
                    id, &json!({"tool_name": "Bash", "input": {"command": "ls"}}), "asked")})
        };
        let lines = [
            (
                "herdr",
                json!({"type": "started", "cwd": "/repo", "name": "task",
                "workspace_id": "ws_1", "model": "opus", "pid": 4242,
                "broker": {"pid": 4241, "socket": "/state/w1.sock", "cwd": "/link"},
                "folder_slot": {"name": "worker", "branch": "b"}}),
            ),
            (
                "out",
                json!({"type": "system", "subtype": "init", "session_id": "s-1"}),
            ),
            (
                "out",
                json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}),
            ),
            (
                "herdr",
                json!({"type": "tool_sessions",
                "sessions": [{"session": 7, "leader_start": 99}, 8]}),
            ),
            (
                "out",
                json!({"type": "result", "subtype": "success", "is_error": false,
                "terminal_reason": "completed", "result": "first"}),
            ),
            (
                "in",
                json!({"type": "user", "message": {"content": "next"}}),
            ),
            (
                "out",
                json!({"type": "control_request", "request_id": "q1",
                "request": {"subtype": "can_use_tool"}}),
            ),
            ("herdr", question("q1")),
            ("herdr", question("q2")),
            ("herdr", question("q3")),
            ("herdr", json!({"type": "answer", "request_id": "q2"})),
            (
                "out",
                json!({"type": "control_cancel_request", "request_id": "q3"}),
            ),
            (
                "herdr",
                json!({"type": "takeover", "at_ms": 5, "takeover_id": "tk-w1-5"}),
            ),
            (
                "herdr",
                json!({"type": "signal", "signal": "SIGTERM", "at_ms": 6}),
            ),
        ];
        let text: String = lines
            .iter()
            .map(|(dir, event)| format!("{}\n", json!({"ts_ms": 1, "dir": dir, "event": event})))
            .collect();
        std::fs::write(&journal, text + "{torn\n").unwrap();

        let store = Store::open(&dir.join(STORE_FILE)).unwrap();
        let imported = import_journal(&store, "w1", &journal).unwrap();
        assert_eq!(imported.questions.len(), 1);
        assert_eq!(imported.resolution("q2"), Some("answered"));
        assert_eq!(imported.resolution("q3"), Some("cancelled"));
        assert_eq!(count(&store, "events"), lines.len() as i64);

        let mut loaded = store.load("w1").unwrap().unwrap();
        loaded.mark_unfinished_takeover();
        assert_eq!(format!("{loaded:?}"), format!("{imported:?}"));
        // The import folds as the in-memory replay does.
        let mut replayed = replay_journal("w1", &journal).unwrap();
        replayed.last_seq = imported.last_seq;
        assert_eq!(format!("{replayed:?}"), format!("{imported:?}"));
        let states: Vec<(String, String)> = store
            .connection()
            .prepare("SELECT request_id, state FROM questions ORDER BY request_id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<StoreResult<_>>()
            .unwrap();
        assert_eq!(
            states,
            [("q1", "pending"), ("q2", "answered"), ("q3", "cancelled")]
                .map(|(id, state)| (id.to_owned(), state.to_owned()))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
