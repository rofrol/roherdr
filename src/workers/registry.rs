//! The server's workers in memory ([`Registry`]): each worker's [`Entry`]
//! by its number, the command ids and owner facts this server holds, the
//! drain, and the lookups by worker id.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::schema::WorkerPipeHolder;

use super::journal::Journal;
use super::live::Live;
use super::status::Status;
use super::{inbox, lock, WorkerError, WorkerSupervisor};

pub(super) struct Entry {
    pub(super) status: Status,
    pub(super) journal_path: PathBuf,
    pub(super) live: Option<Arc<Live>>,
    /// Another server runs the worker (the old server of a live handoff)
    /// and writes its projection; this one appends only events for it
    /// until that server lets go ([`WorkerSupervisor::adopt_when_released`]).
    pub(super) foreign: bool,
    /// The JSONL export, opened on the first event this server records.
    pub(super) export: Option<Arc<Journal>>,
    /// Held output found mid-turn before the reader read the tool call it
    /// belongs to: the exited child and its holders, recorded once that
    /// call's line is read ([`WorkerSupervisor::flush_held_output`]).
    pub(super) held_pending: Option<(u32, Vec<WorkerPipeHolder>)>,
}

impl Entry {
    /// Whether a live handoff keeps the worker running: its broker owns
    /// its pipes.
    pub(super) fn survives_handoff(&self) -> bool {
        self.status.broker.is_some()
    }

    pub(super) fn new(status: Status, journal_path: PathBuf) -> Self {
        Self {
            status,
            journal_path,
            live: None,
            foreign: false,
            export: None,
            held_pending: None,
        }
    }
}

#[derive(Default)]
pub(super) struct Registry {
    pub(super) next_number: u64,
    pub(super) workers: BTreeMap<u64, Entry>,
    /// The client command ids this server runs now, each with its method
    /// and parameters; a repeat waits for the outcome ([`WorkerSupervisor::command`]).
    pub(super) commands: BTreeMap<String, (&'static str, String)>,
    /// Owner panes whose agent cannot handle a question now (it hit a limit,
    /// or is blocked on its own question to the user), with why: a question
    /// its workers ask meanwhile goes to the user at once. Only this
    /// server's memory: herdr re-evaluates the owners when it starts
    /// ([`owners_at_start`]).
    pub(super) stuck_owners: BTreeMap<String, String>,
    /// When each owner pane last showed an event herdr saw (its agent's
    /// state, a limit, an ack), Unix milliseconds. Shown next to a quiet
    /// question; it decides nothing.
    pub(super) owner_seen_ms: BTreeMap<String, u64>,
    /// Owner panes whose pane closed or whose agent exited, with why: a
    /// `todo.resume` from another pane takes over a run they own. Only this
    /// server's memory: herdr re-evaluates the owners when it starts
    /// ([`owners_at_start`]), the runs' owners among them.
    pub(super) gone_owners: BTreeMap<String, String>,
    /// The drain before an install, while new turns are refused. Only this
    /// server's memory: the server a handoff starts does not drain.
    pub(super) drain: Option<Drain>,
    /// Worker starts that passed the drain check and have not registered
    /// their worker (or failed) yet; a drain waits for them too.
    pub(super) starts_admitted: u32,
    /// The inbox rows whose store write failed, per tenure: its next
    /// `worker.events` answers `resync_required` ([`inbox::Losses`]).
    pub(super) inbox_losses: inbox::Losses,
    /// The tenures frozen for a self-service handoff, by tenure id. Written
    /// to the worker directory after every change and restored when a
    /// server opens it (a restart, a live handoff).
    pub(super) handoffs: BTreeMap<String, super::coordinator_handoff::Handoff>,
    /// The sessions whose tenure a successor took over: their mutations are
    /// refused ([`WorkerSupervisor::fence_pane`]). Persisted with `handoffs`.
    pub(super) superseded: Vec<super::coordinator_handoff::Superseded>,
    /// The agent session each pane runs now, as the app last told
    /// ([`WorkerSupervisor::note_pane_session`]); not persisted.
    pub(super) pane_sessions: BTreeMap<String, String>,
}

/// What drains, and since when ([`WorkerSupervisor::drain`]).
#[derive(Debug, Clone)]
pub(super) struct Drain {
    pub(super) reason: String,
    pub(super) started_ms: u64,
}

/// A worker start admitted past the drain check, counted in
/// [`Registry::starts_admitted`] until dropped.
pub(super) struct StartAdmission<'a> {
    pub(super) supervisor: &'a WorkerSupervisor,
}

impl Drop for StartAdmission<'_> {
    fn drop(&mut self) {
        let mut registry = lock(&self.supervisor.shared.registry);
        registry.starts_admitted = registry.starts_admitted.saturating_sub(1);
        drop(registry);
        self.supervisor.shared.changed.notify_all();
    }
}

pub(super) fn worker_number(worker_id: &str) -> Option<u64> {
    worker_id.strip_prefix('w')?.parse().ok()
}

impl WorkerSupervisor {
    pub(super) fn entry_number(registry: &Registry, worker_id: &str) -> Result<u64, WorkerError> {
        worker_number(worker_id)
            .filter(|number| registry.workers.contains_key(number))
            .ok_or_else(|| WorkerError::NotFound(worker_id.to_owned()))
    }

    pub(super) fn with_entry<T>(
        &self,
        worker_id: &str,
        read: impl FnOnce(&Entry) -> T,
    ) -> Result<T, WorkerError> {
        let registry = lock(&self.shared.registry);
        let number = Self::entry_number(&registry, worker_id)?;
        Ok(read(&registry.workers[&number]))
    }
}
