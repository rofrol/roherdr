//! The self-service handoff of a coordination tenure to a fresh session of
//! the same agent kind: `coordinator.ready` freezes the tenure and records
//! the agent's note, `coordinator.handoff_successor` writes herdr's snapshot
//! and the handoff manifest and (in the app) starts the successor, and
//! `coordinator.ack` moves the tenure, its workers and runs to the successor
//! in one step.
//!
//! The pending handoffs are written to the worker directory after every
//! change (a temporary file and a rename) and read back when a server opens
//! it, so a restart or a live handoff keeps the old tenure frozen and the
//! successor's ack valid ([`WorkerSupervisor::restore_handoffs`]); the
//! successor's pane is among the owner panes, so a restored handoff whose
//! successor is gone aborts when the new server checks them. Nothing here
//! is driven by a timer: a successor that exits or reports a
//! limit before its ack aborts the handoff
//! ([`WorkerSupervisor::handoff_owner_event`]), `coordinator handoff
//! --abort` aborts one by hand, and a stalled one stays visible in
//! `coordinator status`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;

use super::registry::Registry;
use super::runs::Run;
use super::{lock, now_ms, OwnerEvent, WorkerError, WorkerSupervisor};
use crate::api::schema::{
    CoordinatorHandoffInfo, CoordinatorHandoffState, CoordinatorInfo, TodoRunStatus,
};

/// The version of the handoff manifest's JSON.
pub(crate) const MANIFEST_VERSION: u32 = 1;
/// The largest note herdr copies.
const NOTE_MAX_BYTES: u64 = 1024 * 1024;

/// The launch prompt of a successor of agent `kind`, naming the manifest:
/// one message that invokes the `todo` skill, which each kind spells in its
/// own way. A kind without a slash-skill invocation has none, and a handoff
/// of it is refused before anything starts.
pub(crate) fn launch_prompt(kind: &str, manifest: &Path) -> Option<String> {
    let manifest = manifest.display();
    match kind {
        "claude" => Some(format!("/todo --handoff {manifest}")),
        "pi" => Some(format!("/skill:todo --handoff {manifest}")),
        _ => None,
    }
}

/// The file of the pending handoffs and superseded sessions, in the
/// `handoffs` directory of the worker directory.
const STATE_FILE: &str = "pending.json";
/// The version of [`StateFile`].
const STATE_VERSION: u32 = 1;
/// How many superseded sessions are kept.
const SUPERSEDED_KEPT: usize = 50;

/// What the state file holds.
#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    version: u32,
    #[serde(default)]
    pending: Vec<Handoff>,
    #[serde(default)]
    superseded: Vec<Superseded>,
}

/// A session whose tenure its successor took over: the old pane and agent
/// session, recorded so that whatever they still send is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Superseded {
    /// The tenure that was handed over.
    pub(crate) coordinator_id: String,
    pub(crate) epoch: i64,
    /// The tenure the successor holds now.
    pub(crate) successor_coordinator_id: String,
    /// The old pane, while it exists: cleared once it closed, so a pane id
    /// that comes back is never fenced.
    pub(crate) pane_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) at_ms: u64,
}

/// Where a handoff has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    /// `ready` froze the tenure.
    Frozen,
    /// The successor's pane runs; its ack is awaited.
    SuccessorStarted,
}

/// One tenure's pending handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Handoff {
    pub(crate) coordinator_id: String,
    pub(crate) epoch: i64,
    pub(crate) old_pane: String,
    pub(crate) note_path: PathBuf,
    pub(crate) note_sha256: String,
    pub(crate) phase: Phase,
    pub(crate) kind: Option<String>,
    pub(crate) successor_pane: Option<String>,
    pub(crate) snapshot_sha256: Option<String>,
}

impl Handoff {
    fn info(&self) -> CoordinatorHandoffInfo {
        CoordinatorHandoffInfo {
            state: match self.phase {
                Phase::Frozen => CoordinatorHandoffState::Frozen,
                Phase::SuccessorStarted => CoordinatorHandoffState::SuccessorStarted,
            },
            epoch: self.epoch,
            successor_pane_id: self.successor_pane.clone(),
            agent_kind: self.kind.clone(),
            note_sha256: self.note_sha256.clone(),
            snapshot_sha256: self.snapshot_sha256.clone(),
        }
    }
}

/// What [`WorkerSupervisor::handoff_prepare`] wrote: the manifest the
/// successor's launch prompt names, and that prompt.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    pub(crate) coordinator_id: String,
    pub(crate) prompt: String,
}

/// What an acknowledged handoff changed.
#[derive(Debug, Clone)]
pub(crate) struct Acked {
    pub(crate) tenure: CoordinatorInfo,
    /// The old coordinator's pane, whose agent session ends now.
    pub(crate) old_pane: String,
}

/// What [`WorkerSupervisor::handoff_abort_request`] aborted.
#[derive(Debug, Clone)]
pub(crate) struct Aborted {
    /// The tenure that unfroze, as it is now.
    pub(crate) tenure: Option<CoordinatorInfo>,
    /// The successor's pane, when the caller was not that pane itself.
    pub(crate) close_pane: Option<String>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn io_error(error: impl ToString) -> WorkerError {
    WorkerError::Io(std::io::Error::other(error.to_string()))
}

/// Writes `bytes` to `path` whole: through a temporary file and a rename,
/// so a reader never sees half of it.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), WorkerError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// Test hook: each state write takes the first entry; `true` fails it.
    static WRITE_FAILURES: std::cell::RefCell<Vec<bool>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A note a pending coordinator handoff waits for: the copy herdr made of the
/// agent's note when the tenure froze, and the sha256 of that note's content.
/// The copy is not the agent's note, so the note is matched by content too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingNote {
    pub(crate) copy: PathBuf,
    pub(crate) sha256: String,
}

/// The notes pending coordinator handoffs wait for, from the persisted state
/// in the workers' directory: the successor takes such a note after the
/// tenure, so another session must not take it first (`herdr handoff take`
/// skips it). A store that cannot be read has no pending handoff.
pub(crate) fn pending_notes_in(workers_dir: &Path) -> Vec<PendingNote> {
    let path = workers_dir.join("handoffs").join(STATE_FILE);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<StateFile>(&text) else {
        return Vec::new();
    };
    state
        .pending
        .into_iter()
        .map(|handoff| PendingNote {
            copy: handoff.note_path,
            sha256: handoff.note_sha256,
        })
        .collect()
}

impl WorkerSupervisor {
    fn handoffs_dir(&self) -> Result<PathBuf, WorkerError> {
        let dir = self.shared.dir.join("handoffs");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn state_path(&self) -> PathBuf {
        self.shared.dir.join("handoffs").join(STATE_FILE)
    }

    /// Writes the pending handoffs and superseded sessions whole, under the
    /// registry lock the caller holds, so the file is never older than a
    /// state another call saw.
    fn persist_locked(&self, registry: &Registry) -> Result<(), WorkerError> {
        self.write_state(
            registry.handoffs.values().cloned().collect(),
            registry.superseded.clone(),
        )
    }

    /// Writes the given state whole. Callers that must not change anything
    /// when the write fails write the new state first and apply it after.
    fn write_state(
        &self,
        pending: Vec<Handoff>,
        superseded: Vec<Superseded>,
    ) -> Result<(), WorkerError> {
        #[cfg(test)]
        if WRITE_FAILURES.with(|failures| {
            let mut failures = failures.borrow_mut();
            !failures.is_empty() && failures.remove(0)
        }) {
            return Err(io_error("injected write failure"));
        }
        let path = self.state_path();
        if pending.is_empty() && superseded.is_empty() && !path.exists() {
            return Ok(());
        }
        let state = StateFile {
            version: STATE_VERSION,
            pending,
            superseded,
        };
        let bytes = serde_json::to_vec_pretty(&state).map_err(io_error)?;
        self.handoffs_dir()?;
        write_whole(&path, &bytes)
    }

    /// [`Self::persist_locked`] for a change that is made already: the
    /// state in memory goes on when the write fails, and the failure is
    /// logged and recorded for tenure `tenure_id` (`coordinator status`
    /// shows it as `handoff_persist_error`) until a write succeeds.
    fn persist_or_warn(&self, registry: &mut Registry, tenure_id: &str) {
        match self.persist_locked(registry) {
            Ok(()) => registry.handoff_persist_errors.clear(),
            Err(error) => {
                warn!(%error, "cannot write the pending coordinator handoffs");
                registry
                    .handoff_persist_errors
                    .insert(tenure_id.to_owned(), error.to_string());
            }
        }
    }

    /// Reads the state file a previous server wrote, when this server opens
    /// the worker directory. A pending handoff whose tenure is no longer the
    /// active one at its epoch (it ended while no server ran) is dropped;
    /// the others keep their old tenure frozen. A file that cannot be read
    /// is logged and ignored.
    pub(super) fn restore_handoffs(&self) {
        let path = self.state_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                warn!(%error, path = %path.display(), "cannot read the pending coordinator handoffs");
                return;
            }
        };
        let state: StateFile = match serde_json::from_slice(&bytes) {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, path = %path.display(), "the pending coordinator handoffs are unreadable");
                return;
            }
        };
        let mut registry = lock(&self.shared.registry);
        // A record whose tenure is still active at its epoch claims a
        // supersession that never happened (an ack whose transfer failed
        // and whose restoring write failed too): it is not restored.
        let mut superseded = state.superseded;
        let before = superseded.len();
        superseded.retain(|record| {
            !self
                .tenure_store()
                .ok()
                .and_then(|store| store.coordinator(&record.coordinator_id).ok().flatten())
                .is_some_and(|tenure| tenure.ended_at.is_none() && tenure.epoch == record.epoch)
        });
        let mut dropped = superseded.len() != before;
        registry.superseded = superseded;
        // A handoff whose recorded phase no live successor can match is
        // aborted below: a successor-started phase without a pane, or a
        // prepared one (the successor's start was not recorded; the write
        // may have failed). A successor pane that is gone is aborted by
        // [`Self::owners_at_start`].
        let mut inconsistent = Vec::new();
        for handoff in state.pending {
            let active = self
                .tenure_store()
                .ok()
                .and_then(|store| store.coordinator(&handoff.coordinator_id).ok().flatten())
                .is_some_and(|tenure| tenure.ended_at.is_none() && tenure.epoch == handoff.epoch);
            if active {
                if handoff.successor_pane.is_none()
                    && (handoff.phase == Phase::SuccessorStarted
                        || handoff.kind.is_some()
                        || handoff.snapshot_sha256.is_some())
                {
                    inconsistent.push(handoff.coordinator_id.clone());
                }
                registry
                    .handoffs
                    .insert(handoff.coordinator_id.clone(), handoff);
            } else {
                dropped = true;
            }
        }
        if dropped {
            self.persist_or_warn(&mut registry, "");
        }
        drop(registry);
        for coordinator_id in inconsistent {
            self.handoff_abort(
                &coordinator_id,
                "no live successor matches its recorded phase after a restart",
            );
        }
    }

    /// `info` with the tenure's pending handoff.
    pub(crate) fn with_handoff(&self, mut info: CoordinatorInfo) -> CoordinatorInfo {
        let registry = lock(&self.shared.registry);
        info.handoff = registry
            .handoffs
            .get(&info.coordinator_id)
            .map(Handoff::info);
        info.handoff_persist_error = registry
            .handoff_persist_errors
            .get(&info.coordinator_id)
            .cloned();
        info
    }

    fn handoff_of_old_pane(&self, pane_id: &str) -> Option<Handoff> {
        lock(&self.shared.registry)
            .handoffs
            .values()
            .find(|handoff| handoff.old_pane == pane_id)
            .cloned()
    }

    /// `coordinator.ready`: the tenure bound to `pane_id` freezes, with a
    /// copy of the note at `note` (a file of at most 1 MiB) and its hash.
    /// A frozen tenure that has no successor yet takes a new note.
    pub(crate) fn handoff_ready(
        &self,
        pane_id: &str,
        note: &Path,
    ) -> Result<CoordinatorInfo, WorkerError> {
        // The server reads the file as the agent named it: a relative path
        // would be resolved against the server's directory, not the agent's.
        if !note.is_absolute() {
            return Err(WorkerError::Invalid(format!(
                "the note path {} must be absolute",
                note.display()
            )));
        }
        let tenure = self.coordinator_of_pane(pane_id).ok_or_else(|| {
            WorkerError::CoordinatorNotFound(format!(
                "pane {pane_id} is bound to no active coordinator"
            ))
        })?;
        if tenure.headless {
            return Err(WorkerError::HandoffRefused(format!(
                "coordinator {} is headless: it has no session to hand over",
                tenure.coordinator_id
            )));
        }
        if let Some(pending) = lock(&self.shared.registry)
            .handoffs
            .get(&tenure.coordinator_id)
        {
            if pending.phase != Phase::Frozen {
                return Err(WorkerError::HandoffRefused(format!(
                    "coordinator {} already has a successor started in pane {}",
                    tenure.coordinator_id,
                    pending.successor_pane.as_deref().unwrap_or("?")
                )));
            }
        }
        let metadata = std::fs::metadata(note).map_err(|error| {
            WorkerError::Invalid(format!("cannot read the note {}: {error}", note.display()))
        })?;
        if !metadata.is_file() || metadata.len() > NOTE_MAX_BYTES {
            return Err(WorkerError::Invalid(format!(
                "the note {} must be a file of at most {NOTE_MAX_BYTES} bytes",
                note.display()
            )));
        }
        let bytes = std::fs::read(note)?;
        let copy = self.handoffs_dir()?.join(format!(
            "{}-e{}.note.md",
            tenure.coordinator_id, tenure.epoch
        ));
        write_whole(&copy, &bytes)?;
        let handoff = Handoff {
            coordinator_id: tenure.coordinator_id.clone(),
            epoch: tenure.epoch,
            old_pane: pane_id.to_owned(),
            note_path: copy,
            note_sha256: sha256_hex(&bytes),
            phase: Phase::Frozen,
            kind: None,
            successor_pane: None,
            snapshot_sha256: None,
        };
        {
            let mut registry = lock(&self.shared.registry);
            let before = registry
                .handoffs
                .insert(tenure.coordinator_id.clone(), handoff);
            if let Err(error) = self.persist_locked(&registry) {
                // A freeze that a restart would lose is not made.
                match before {
                    Some(before) => registry
                        .handoffs
                        .insert(tenure.coordinator_id.clone(), before),
                    None => registry.handoffs.remove(&tenure.coordinator_id),
                };
                return Err(error);
            }
        }
        self.shared.changed.notify_all();
        super::notify_clients();
        Ok(self.with_handoff(tenure))
    }

    /// Herdr's own snapshot of `tenure`: its runs in flight with their last
    /// event ids, its workers and their pending questions.
    fn tenure_snapshot(&self, tenure: &CoordinatorInfo) -> Result<Value, WorkerError> {
        let store = self.run_store()?;
        let runs: Vec<Run> = store
            .runs(Some(&tenure.repo))
            .map_err(super::coordinators::store_error)?
            .into_iter()
            .filter(|run| {
                run.owner_coordinator.as_deref() == Some(tenure.coordinator_id.as_str())
                    && matches!(
                        run.info.status,
                        TodoRunStatus::Running | TodoRunStatus::Waiting | TodoRunStatus::Blocked
                    )
            })
            .collect();
        let mut run_rows = Vec::new();
        for run in &runs {
            let last_event = store
                .latest_run_event(&run.info.run_id)
                .map_err(super::coordinators::store_error)?
                .map(|(seq, _)| seq);
            run_rows.push(json!({
                "run_id": run.info.run_id,
                "item": run.info.item,
                "status": run.info.status,
                "attempt": run.info.attempt,
                "worker_id": run.info.worker_id,
                "pending_event": run.info.pending_event,
                "last_event_id": last_event,
            }));
        }
        let workers: Vec<_> = self
            .list()
            .into_iter()
            .filter(|worker| {
                worker.owner_coordinator_id.as_deref() == Some(tenure.coordinator_id.as_str())
            })
            .collect();
        let worker_rows: Vec<Value> = workers
            .iter()
            .map(|worker| {
                json!({
                    "worker_id": worker.worker_id,
                    "state": worker.state,
                    "item": worker.item,
                    "seq": worker.seq,
                    "acked_seq": worker.acked_seq,
                })
            })
            .collect();
        let questions: Vec<Value> = workers
            .iter()
            .flat_map(|worker| {
                worker.questions.iter().map(|question| {
                    json!({
                        "worker_id": worker.worker_id,
                        "request_id": question.request_id,
                    })
                })
            })
            .collect();
        Ok(json!({
            "coordinator_id": tenure.coordinator_id,
            "repository": tenure.repo,
            "epoch": tenure.epoch,
            "item": tenure.item,
            "runs": run_rows,
            "workers": worker_rows,
            "pending_questions": questions,
        }))
    }

    /// First half of `coordinator.handoff_successor`: checks that the
    /// tenure bound to `pane_id` is frozen and that agent `kind` has a
    /// launch prompt, then writes herdr's snapshot and the manifest. Starts
    /// nothing; a refusal changes nothing.
    pub(crate) fn handoff_prepare(
        &self,
        pane_id: &str,
        kind: &str,
    ) -> Result<Prepared, WorkerError> {
        let handoff = self.handoff_of_old_pane(pane_id).ok_or_else(|| {
            WorkerError::HandoffRefused(format!(
                "pane {pane_id}'s tenure is not frozen: run `herdr coordinator ready --note FILE` first"
            ))
        })?;
        if handoff.phase != Phase::Frozen {
            return Err(WorkerError::HandoffRefused(format!(
                "coordinator {} already has a successor in pane {}",
                handoff.coordinator_id,
                handoff.successor_pane.as_deref().unwrap_or("?")
            )));
        }
        // Checked against the table before anything is written: fail closed.
        if launch_prompt(kind, Path::new("/")).is_none() {
            return Err(WorkerError::HandoffRefused(format!(
                "agent kind {kind} cannot take a coordinator over by itself: only claude and pi \
                 start with a skill invocation"
            )));
        }
        let tenure = self
            .coordinator_of_pane(pane_id)
            .filter(|tenure| {
                tenure.coordinator_id == handoff.coordinator_id && tenure.epoch == handoff.epoch
            })
            .ok_or_else(|| {
                WorkerError::HandoffRefused(format!(
                    "coordinator {} no longer holds pane {pane_id} at epoch {}",
                    handoff.coordinator_id, handoff.epoch
                ))
            })?;
        let snapshot = self.tenure_snapshot(&tenure)?;
        let snapshot_bytes = serde_json::to_vec_pretty(&snapshot).map_err(io_error)?;
        let snapshot_sha = sha256_hex(&snapshot_bytes);
        let dir = self.handoffs_dir()?;
        let stem = format!("{}-e{}", tenure.coordinator_id, tenure.epoch);
        let snapshot_path = dir.join(format!("{stem}.snapshot.json"));
        write_whole(&snapshot_path, &snapshot_bytes)?;
        let manifest_path = dir.join(format!("{stem}.manifest.json"));
        let manifest = json!({
            "version": MANIFEST_VERSION,
            "created_ms": now_ms(),
            "epoch": tenure.epoch,
            "coordinator_id": tenure.coordinator_id,
            "repository": tenure.repo,
            "agent_kind": kind,
            "old_pane_id": pane_id,
            "tenure": snapshot,
            "snapshot": {"path": snapshot_path, "sha256": snapshot_sha},
            "note": {"path": handoff.note_path, "sha256": handoff.note_sha256},
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(io_error)?;
        write_whole(&manifest_path, &manifest_bytes)?;
        let prompt = launch_prompt(kind, &manifest_path).ok_or_else(|| {
            WorkerError::HandoffRefused(format!("agent kind {kind} has no launch prompt"))
        })?;
        {
            let mut registry = lock(&self.shared.registry);
            if let Some(pending) = registry.handoffs.get_mut(&tenure.coordinator_id) {
                pending.kind = Some(kind.to_owned());
                pending.snapshot_sha256 = Some(snapshot_sha);
            }
            self.persist_or_warn(&mut registry, &tenure.coordinator_id);
        }
        Ok(Prepared {
            coordinator_id: tenure.coordinator_id,
            prompt,
        })
    }

    /// Second half: the successor's agent was started in `successor_pane`.
    pub(crate) fn handoff_successor_started(
        &self,
        coordinator_id: &str,
        successor_pane: &str,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let old_pane = {
            let mut registry = lock(&self.shared.registry);
            let handoff = registry.handoffs.get_mut(coordinator_id).ok_or_else(|| {
                WorkerError::HandoffRefused(format!(
                    "coordinator {coordinator_id} has no pending handoff"
                ))
            })?;
            handoff.phase = Phase::SuccessorStarted;
            handoff.successor_pane = Some(successor_pane.to_owned());
            let old_pane = handoff.old_pane.clone();
            self.persist_or_warn(&mut registry, coordinator_id);
            old_pane
        };
        self.shared.changed.notify_all();
        super::notify_clients();
        let tenure = self.coordinator_of_pane(&old_pane).ok_or_else(|| {
            WorkerError::CoordinatorNotFound(format!("coordinator {coordinator_id} is not active"))
        })?;
        Ok(self.with_handoff(tenure))
    }

    /// Cancels the pending handoff of `coordinator_id`: the tenure unfreezes
    /// and keeps its authority, and one notice tells the user why. Nothing
    /// when there is none.
    pub(crate) fn handoff_abort(&self, coordinator_id: &str, cause: &str) {
        if let Err(error) = self.handoff_abort_inner(coordinator_id, cause, false) {
            warn!(%error, "cannot abort the coordinator handoff");
        }
    }

    /// [`Self::handoff_abort`] that reports a failed write. `strict`: the
    /// state file is written first and a failure changes nothing (a
    /// request from a user is refused with the error); otherwise the
    /// handoff is dropped in memory whatever the write says, because an
    /// event cannot be refused and the old tenure must have its authority
    /// back; the failure is logged as an error.
    fn handoff_abort_inner(
        &self,
        coordinator_id: &str,
        cause: &str,
        strict: bool,
    ) -> Result<(), WorkerError> {
        let removed = {
            let mut registry = lock(&self.shared.registry);
            if registry.handoffs.contains_key(coordinator_id) {
                let pending: Vec<Handoff> = registry
                    .handoffs
                    .values()
                    .filter(|handoff| handoff.coordinator_id != coordinator_id)
                    .cloned()
                    .collect();
                match self.write_state(pending, registry.superseded.clone()) {
                    Err(error) if strict => return Err(error),
                    Err(error) => {
                        tracing::error!(%error, "cannot write the aborted coordinator handoff; a restart may restore it");
                        registry
                            .handoff_persist_errors
                            .insert(coordinator_id.to_owned(), error.to_string());
                    }
                    Ok(()) => registry.handoff_persist_errors.clear(),
                }
                registry.handoffs.remove(coordinator_id)
            } else {
                None
            }
        };
        let Some(handoff) = removed else {
            return Ok(());
        };
        self.shared.changed.notify_all();
        super::notify_clients();
        super::notify_user(super::UserNotice {
            title: format!("coordinator {}: handoff aborted", handoff.coordinator_id),
            body: format!(
                "{cause}; coordinator {} keeps its tenure in pane {}",
                handoff.coordinator_id, handoff.old_pane
            ),
            pane_id: Some(handoff.old_pane),
        });
        Ok(())
    }

    /// `coordinator.handoff --abort`: cancels the pending handoff named by
    /// `coordinator_id`, else the one `pane_id` is the old or the successor
    /// pane of. The tenure unfreezes and keeps its authority; one notice
    /// says so. The successor's pane is returned for the caller to close
    /// unless it is the successor itself asking.
    pub(crate) fn handoff_abort_request(
        &self,
        pane_id: Option<&str>,
        coordinator_id: Option<&str>,
    ) -> Result<Aborted, WorkerError> {
        let found = {
            let registry = lock(&self.shared.registry);
            registry
                .handoffs
                .values()
                .find(|handoff| match (coordinator_id, pane_id) {
                    (Some(id), _) => handoff.coordinator_id == id,
                    (None, Some(pane)) => {
                        handoff.old_pane == pane || handoff.successor_pane.as_deref() == Some(pane)
                    }
                    (None, None) => false,
                })
                .cloned()
        };
        let handoff = found.ok_or_else(|| {
            WorkerError::HandoffRefused(match (coordinator_id, pane_id) {
                (None, None) => "coordinator handoff --abort needs a pane or --id".to_owned(),
                (Some(id), _) => format!("coordinator {id} has no pending handoff"),
                (None, Some(pane)) => format!("pane {pane} is part of no pending handoff"),
            })
        })?;
        let by_successor = pane_id.is_some()
            && coordinator_id.is_none()
            && handoff.successor_pane.as_deref() == pane_id;
        let cause = if by_successor {
            "the successor aborted it"
        } else {
            "it was aborted by hand"
        };
        self.handoff_abort_inner(&handoff.coordinator_id, cause, true)?;
        Ok(Aborted {
            tenure: self
                .coordinator_of_pane(&handoff.old_pane)
                .map(|tenure| self.with_handoff(tenure)),
            close_pane: if by_successor {
                None
            } else {
                handoff.successor_pane
            },
        })
    }

    /// `coordinator.ack`: the successor in `pane_id` takes the tenure over.
    /// `epoch` and `snapshot_sha256` must be the pending handoff's, and the
    /// caller the pane its successor was started in; anything else is
    /// refused and changes nothing. In one step, under one registry lock,
    /// the epoch advances, the tenure, its workers and runs move to the
    /// successor ([`Self::coordinator_handoff_locked`]), the handoff is
    /// removed and the old pane and agent session are recorded as
    /// superseded. The caller closes the old pane.
    pub(crate) fn handoff_ack(
        &self,
        pane_id: &str,
        epoch: i64,
        snapshot_sha256: &str,
        to_session: Option<&str>,
        to_workspace: Option<&str>,
    ) -> Result<Acked, WorkerError> {
        let mut registry = lock(&self.shared.registry);
        let pending = registry
            .handoffs
            .values()
            .find(|handoff| handoff.successor_pane.as_deref() == Some(pane_id))
            .cloned();
        let handoff = pending.ok_or_else(|| {
            WorkerError::HandoffRefused(format!(
                "no pending handoff started a successor in pane {pane_id}"
            ))
        })?;
        if handoff.phase != Phase::SuccessorStarted
            || handoff.epoch != epoch
            || handoff.snapshot_sha256.as_deref() != Some(snapshot_sha256)
        {
            return Err(WorkerError::HandoffRefused(format!(
                "the ack does not match coordinator {}'s pending handoff (epoch {}); read its \
                 manifest again",
                handoff.coordinator_id, handoff.epoch
            )));
        }
        let current = self
            .coordinator_of_pane(&handoff.old_pane)
            .filter(|tenure| {
                tenure.coordinator_id == handoff.coordinator_id && tenure.epoch == handoff.epoch
            })
            .ok_or_else(|| {
                WorkerError::HandoffRefused(format!(
                    "coordinator {} is no longer the active tenure at epoch {}",
                    handoff.coordinator_id, handoff.epoch
                ))
            })?;
        // The new state is written first: an ack whose write fails is
        // refused and transfers nothing. The successor's tenure id is not
        // known before the transfer, so the record names it afterwards and
        // is written again; the first write already holds the removed
        // handoff and the superseded pane and session.
        let mut superseded = registry.superseded.clone();
        let mut record = Superseded {
            coordinator_id: current.coordinator_id.clone(),
            epoch: current.epoch,
            successor_coordinator_id: String::new(),
            pane_id: Some(handoff.old_pane.clone()),
            session_id: current.session_id.clone(),
            at_ms: now_ms(),
        };
        let mut staged = superseded.clone();
        staged.push(record.clone());
        let excess = staged.len().saturating_sub(SUPERSEDED_KEPT);
        staged.drain(..excess);
        let remaining: Vec<Handoff> = registry
            .handoffs
            .values()
            .filter(|other| other.coordinator_id != handoff.coordinator_id)
            .cloned()
            .collect();
        self.write_state(remaining, staged)?;
        let tenure = match self.coordinator_handoff_locked(
            &mut registry,
            Some(&current.coordinator_id),
            None,
            pane_id,
            to_session,
            to_workspace,
        ) {
            Ok(tenure) => tenure,
            Err(error) => {
                // Nothing moved: the file goes back to what memory holds.
                // When that write fails too, the file names a supersession
                // memory does not hold: the failure is recorded for the
                // tenure, and a restart does not restore a record whose
                // tenure is still active ([`Self::restore_handoffs`]).
                if let Err(write) = self.persist_locked(&registry) {
                    tracing::error!(%write, "cannot restore the coordinator handoff state after a refused ack");
                    registry
                        .handoff_persist_errors
                        .insert(handoff.coordinator_id.clone(), write.to_string());
                }
                return Err(error);
            }
        };
        registry.handoffs.remove(&handoff.coordinator_id);
        record.successor_coordinator_id = tenure.coordinator_id.clone();
        superseded.push(record);
        let excess = superseded.len().saturating_sub(SUPERSEDED_KEPT);
        superseded.drain(..excess);
        registry.superseded = superseded;
        // The names are complete now; a failure here leaves the first
        // write, which fences the same pane and session.
        self.persist_or_warn(&mut registry, &tenure.coordinator_id);
        drop(registry);
        self.shared.changed.notify_all();
        super::notify_clients();
        Ok(Acked {
            tenure,
            old_pane: handoff.old_pane,
        })
    }

    /// The old pane of an acknowledged handoff is gone: it can send nothing
    /// any more, and its id is free again.
    pub(crate) fn handoff_old_pane_closed(&self, pane_id: &str) {
        let mut registry = lock(&self.shared.registry);
        registry.pane_sessions.remove(pane_id);
        let mut changed = None;
        for superseded in &mut registry.superseded {
            if superseded.pane_id.as_deref() == Some(pane_id) {
                superseded.pane_id = None;
                changed = Some(superseded.successor_coordinator_id.clone());
            }
        }
        if let Some(tenure_id) = changed {
            self.persist_or_warn(&mut registry, &tenure_id);
        }
    }

    /// An agent event of pane `pane_id` as far as pending handoffs care: a
    /// successor that exits, loses its pane or reports a limit
    /// before its ack aborts the handoff, and so does the old coordinator's
    /// pane closing or its agent exiting. A superseded pane that closes
    /// needs no fence any more.
    pub(super) fn handoff_owner_event(&self, pane_id: &str, event: OwnerEvent) {
        let gone = matches!(event, OwnerEvent::PaneClosed | OwnerEvent::AgentExited);
        if event == OwnerEvent::PaneClosed {
            self.handoff_old_pane_closed(pane_id);
        }
        let aborts: Vec<(String, &'static str)> = lock(&self.shared.registry)
            .handoffs
            .values()
            .filter_map(|handoff| {
                let successor = handoff.successor_pane.as_deref() == Some(pane_id);
                if successor && gone {
                    Some((
                        handoff.coordinator_id.clone(),
                        "the successor exited before it acknowledged",
                    ))
                } else if successor && event == OwnerEvent::Limited {
                    Some((
                        handoff.coordinator_id.clone(),
                        "the successor reported a limit before it acknowledged",
                    ))
                } else if handoff.old_pane == pane_id && gone {
                    Some((
                        handoff.coordinator_id.clone(),
                        "the coordinator's own session ended",
                    ))
                } else {
                    None
                }
            })
            .collect();
        for (coordinator_id, cause) in aborts {
            self.handoff_abort(&coordinator_id, cause);
        }
    }

    /// The panes of started successors and of superseded sessions that may
    /// still run, which [`Self::owner_panes`] lists so their closing is
    /// reported.
    /// Whether `pane_id` is the pane of a started, not yet acknowledged
    /// successor.
    pub(crate) fn is_pending_successor(&self, pane_id: &str) -> bool {
        lock(&self.shared.registry)
            .handoffs
            .values()
            .any(|handoff| handoff.successor_pane.as_deref() == Some(pane_id))
    }

    pub(super) fn handoff_successor_panes(&self) -> Vec<String> {
        let registry = lock(&self.shared.registry);
        registry
            .handoffs
            .values()
            .filter_map(|handoff| handoff.successor_pane.clone())
            .chain(
                registry
                    .superseded
                    .iter()
                    .filter_map(|superseded| superseded.pane_id.clone()),
            )
            .collect()
    }

    fn frozen_error(coordinator_id: &str) -> WorkerError {
        WorkerError::CoordinatorFrozen(format!(
            "coordinator {coordinator_id} is frozen for a handoff to a fresh session; its \
             successor takes over with `herdr coordinator ack`, or `herdr coordinator handoff \
             --abort` ends the handoff and it goes on"
        ))
    }

    fn superseded_error(superseded: &Superseded) -> WorkerError {
        WorkerError::CoordinatorSuperseded(format!(
            "this session's coordinator tenure {} was handed over to {}; it has no authority \
             over the repository's workers and runs any more",
            superseded.coordinator_id, superseded.successor_coordinator_id
        ))
    }

    /// Refuses tenure `coordinator_id` (when given) while it is frozen.
    pub(crate) fn fence_tenure(&self, coordinator_id: Option<&str>) -> Result<(), WorkerError> {
        match coordinator_id {
            Some(id) if lock(&self.shared.registry).handoffs.contains_key(id) => {
                Err(Self::frozen_error(id))
            }
            _ => Ok(()),
        }
    }

    /// Records the agent session `pane_id` runs now (`None`: none), which
    /// [`Self::fence_pane`] compares with the superseded sessions.
    pub(crate) fn note_pane_session(&self, pane_id: &str, session: Option<&str>) {
        let mut registry = lock(&self.shared.registry);
        match session {
            Some(session) => {
                registry
                    .pane_sessions
                    .insert(pane_id.to_owned(), session.to_owned());
            }
            None => {
                registry.pane_sessions.remove(pane_id);
            }
        }
    }

    /// Forgets the sessions of panes that are not in `live` (the public ids
    /// of the panes that exist at startup).
    pub(crate) fn prune_pane_sessions(&self, live: &[String]) {
        lock(&self.shared.registry)
            .pane_sessions
            .retain(|pane, _| live.contains(pane));
    }

    /// [`Self::fence_pane`] for a caller whose session the app knows.
    pub(crate) fn fence_pane_session(
        &self,
        caller: Option<&str>,
        session: Option<&str>,
    ) -> Result<(), WorkerError> {
        if let Some(pane) = caller.filter(|pane| !pane.trim().is_empty()) {
            self.note_pane_session(pane, session);
        }
        self.fence_pane(caller)
    }

    /// Refuses a mutation sent from `caller` (a pane id; none for a caller
    /// outside any pane, such as the user's own shell) while the tenure
    /// bound to that pane is frozen, and always from a pane whose tenure was
    /// handed over. By caller, never by owner: the user's own commands about
    /// a frozen coordinator's workers stay allowed. Decided under one
    /// registry lock, so an ack cannot land between the checks.
    pub(crate) fn fence_pane(&self, caller: Option<&str>) -> Result<(), WorkerError> {
        let Some(pane_id) = caller.filter(|pane| !pane.trim().is_empty()) else {
            return Ok(());
        };
        let registry = lock(&self.shared.registry);
        let session = registry.pane_sessions.get(pane_id).map(String::as_str);
        // The record matches the pane and the agent session: the session
        // wherever it runs (`--resume` elsewhere), and the pane only while
        // its current session is the old one (any session when the record
        // knows none); never a new or an unknown session on a pane id that
        // was closed and reopened.
        if let Some(superseded) = registry.superseded.iter().find(|superseded| {
            let same_session = session.is_some() && superseded.session_id.as_deref() == session;
            let same_pane = superseded.pane_id.as_deref() == Some(pane_id)
                && (superseded.session_id.is_none() || same_session);
            same_session || same_pane
        }) {
            return Err(Self::superseded_error(superseded));
        }
        if registry.handoffs.is_empty() {
            return Ok(());
        }
        if let Some(handoff) = registry
            .handoffs
            .values()
            .find(|handoff| handoff.old_pane == pane_id)
        {
            return Err(Self::frozen_error(&handoff.coordinator_id));
        }
        match self.coordinator_of_pane(pane_id) {
            Some(tenure) if registry.handoffs.contains_key(&tenure.coordinator_id) => {
                Err(Self::frozen_error(&tenure.coordinator_id))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> (PathBuf, WorkerSupervisor, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-handoff-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let note = root.join("note.md");
        std::fs::write(&note, "continue with the queue\n").unwrap();
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        (root, supervisor, note)
    }

    fn code(result: Result<impl std::fmt::Debug, WorkerError>) -> &'static str {
        result.expect_err("expected a refusal").code()
    }

    fn state(supervisor: &WorkerSupervisor, pane: &str) -> Option<CoordinatorHandoffState> {
        supervisor
            .with_handoff(supervisor.coordinator_of_pane(pane)?)
            .handoff
            .map(|handoff| handoff.state)
    }

    /// Starts a tenure in p1 and runs it to `SuccessorStarted` in p2.
    fn started(supervisor: &WorkerSupervisor, note: &Path) -> (CoordinatorInfo, String) {
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", note).unwrap();
        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        let sha = supervisor
            .with_handoff(supervisor.coordinator_of_pane("p1").unwrap())
            .handoff
            .unwrap()
            .snapshot_sha256
            .unwrap();
        (tenure, sha)
    }

    #[test]
    fn the_launch_prompt_table_has_claude_and_pi_only() {
        let manifest = Path::new("/state/m.json");
        assert_eq!(
            launch_prompt("claude", manifest).as_deref(),
            Some("/todo --handoff /state/m.json")
        );
        assert_eq!(
            launch_prompt("pi", manifest).as_deref(),
            Some("/skill:todo --handoff /state/m.json")
        );
        for kind in ["codex", "agy", "gemini", "", "Claude"] {
            assert_eq!(launch_prompt(kind, manifest), None, "{kind}");
        }
    }

    #[test]
    fn a_handoff_goes_from_active_through_frozen_and_started_to_acked() {
        let (root, supervisor, note) = scratch("full");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(state(&supervisor, "p1"), None);
        supervisor.fence_pane(Some("p1")).unwrap();

        let ready = supervisor.handoff_ready("p1", &note).unwrap();
        assert_eq!(
            ready.handoff.unwrap().state,
            CoordinatorHandoffState::Frozen
        );
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_frozen"
        );
        assert_eq!(
            code(supervisor.fence_tenure(Some(&tenure.coordinator_id))),
            "coordinator_frozen"
        );
        // Other panes are not fenced.
        supervisor.fence_pane(Some("p9")).unwrap();
        // No successor yet: nothing to ack.
        assert_eq!(
            code(supervisor.handoff_ack("p2", 1, "x", None, None)),
            "handoff_refused"
        );

        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        assert!(prepared.prompt.starts_with("/todo --handoff "));
        let manifest_path = prepared.prompt.trim_start_matches("/todo --handoff ");
        let manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["version"], MANIFEST_VERSION);
        assert_eq!(manifest["epoch"], 1);
        assert_eq!(manifest["agent_kind"], "claude");
        assert_eq!(manifest["old_pane_id"], "p1");
        let snapshot_path = manifest["snapshot"]["path"].as_str().unwrap();
        let snapshot_sha = manifest["snapshot"]["sha256"].as_str().unwrap().to_owned();
        assert_eq!(
            sha256_hex(&std::fs::read(snapshot_path).unwrap()),
            snapshot_sha
        );
        let note_copy = manifest["note"]["path"].as_str().unwrap();
        assert_eq!(
            sha256_hex(&std::fs::read(note_copy).unwrap()),
            manifest["note"]["sha256"].as_str().unwrap()
        );
        assert_ne!(Path::new(note_copy), note.as_path());

        let pending = supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        let pending = pending.handoff.unwrap();
        assert_eq!(pending.state, CoordinatorHandoffState::SuccessorStarted);
        assert_eq!(pending.successor_pane_id.as_deref(), Some("p2"));
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::SuccessorStarted)
        );

        let acked = supervisor
            .handoff_ack("p2", 1, &snapshot_sha, None, None)
            .unwrap();
        assert_eq!(acked.old_pane, "p1");
        assert_eq!(acked.tenure.epoch, 2);
        assert_eq!(acked.tenure.pane_id.as_deref(), Some("p2"));
        assert!(supervisor.coordinator_of_pane("p1").is_none());
        assert_eq!(state(&supervisor, "p2"), None);
        supervisor.fence_pane(Some("p2")).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_stale_or_wrong_ack_is_refused_and_changes_nothing() {
        let (root, supervisor, note) = scratch("stale");
        let (tenure, sha) = started(&supervisor, &note);
        let wrong_sha = "0".repeat(64);
        for (pane, epoch, sha) in [
            ("p2", 2, sha.as_str()),
            ("p2", 0, sha.as_str()),
            ("p2", 1, wrong_sha.as_str()),
            ("p1", 1, sha.as_str()),
            ("p3", 1, sha.as_str()),
        ] {
            assert_eq!(
                code(supervisor.handoff_ack(pane, epoch, sha, None, None)),
                "handoff_refused",
                "{pane} {epoch}"
            );
            let current = supervisor.coordinator_of_pane("p1").unwrap();
            assert_eq!(current.coordinator_id, tenure.coordinator_id);
            assert_eq!(current.epoch, 1);
            assert_eq!(
                state(&supervisor, "p1"),
                Some(CoordinatorHandoffState::SuccessorStarted)
            );
        }
        // The right ack still works afterwards, once.
        supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        assert_eq!(
            code(supervisor.handoff_ack("p2", 1, &sha, None, None)),
            "handoff_refused"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ready_needs_a_tenure_a_readable_note_and_no_started_successor() {
        let (root, supervisor, note) = scratch("ready");
        assert_eq!(
            code(supervisor.handoff_ready("p1", &note)),
            "coordinator_not_found"
        );
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(
            code(supervisor.handoff_ready("p1", &root.join("missing.md"))),
            "invalid_request"
        );
        assert_eq!(state(&supervisor, "p1"), None);
        assert_eq!(
            code(supervisor.handoff_ready("p1", &root)),
            "invalid_request"
        );
        // Frozen: a new note replaces the old one.
        supervisor.handoff_ready("p1", &note).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        let prepared = supervisor.handoff_prepare("p1", "pi").unwrap();
        assert!(prepared.prompt.starts_with("/skill:todo --handoff "));
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        assert_eq!(
            code(supervisor.handoff_ready("p1", &note)),
            "handoff_refused"
        );
        assert_eq!(
            code(supervisor.handoff_prepare("p1", "pi")),
            "handoff_refused"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn handoff_successor_needs_a_frozen_tenure_and_a_known_kind() {
        let (root, supervisor, note) = scratch("kind");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(
            code(supervisor.handoff_prepare("p1", "claude")),
            "handoff_refused"
        );
        supervisor.handoff_ready("p1", &note).unwrap();
        for kind in ["codex", "agy", "droid", ""] {
            assert_eq!(
                code(supervisor.handoff_prepare("p1", kind)),
                "handoff_refused"
            );
        }
        // Nothing was written, and the tenure is still frozen, unchanged.
        assert!(!root
            .join("workers/handoffs")
            .read_dir()
            .unwrap()
            .any(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                name.ends_with(".manifest.json") || name.ends_with(".snapshot.json")
            }));
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::Frozen)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// The successor ends before its ack, in each of the ways herdr hears
    /// of: the old tenure unfreezes, keeps its authority, and one notice
    /// says so; the old pane's own end drops the handoff too.
    #[test]
    fn an_aborted_handoff_unfreezes_the_old_tenure_with_one_notice() {
        for (name, pane, event, started_first) in [
            ("exit", "p2", OwnerEvent::AgentExited, true),
            ("closed", "p2", OwnerEvent::PaneClosed, true),
            ("limit", "p2", OwnerEvent::Limited, true),
            ("old-closed", "p1", OwnerEvent::PaneClosed, true),
            ("frozen-limit", "p2", OwnerEvent::Limited, false),
        ] {
            let (root, supervisor, note) = scratch(name);
            let tenure = if started_first {
                started(&supervisor, &note).0
            } else {
                let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
                supervisor.handoff_ready("p1", &note).unwrap();
                tenure
            };
            let _ = crate::workers::take_user_notices();
            supervisor.handoff_owner_event(pane, event);
            let aborted = !started_first && pane == "p2";
            if aborted {
                // A frozen tenure has no successor: the event is not about it.
                assert_eq!(
                    state(&supervisor, "p1"),
                    Some(CoordinatorHandoffState::Frozen)
                );
                continue;
            }
            let notices: Vec<_> = crate::workers::take_user_notices()
                .into_iter()
                .filter(|notice| notice.body.contains(&tenure.coordinator_id))
                .collect();
            assert_eq!(notices.len(), 1, "{name}");
            assert_eq!(
                notices[0].title,
                format!("coordinator {}: handoff aborted", tenure.coordinator_id)
            );
            supervisor.fence_pane(Some("p1")).unwrap();
            if pane == "p2" {
                let current = supervisor.coordinator_of_pane("p1").unwrap();
                assert_eq!(current.epoch, 1, "{name}");
                assert_eq!(current.handoff, None);
            }
            assert_eq!(
                code(supervisor.handoff_ack("p2", 1, "x", None, None)),
                "handoff_refused"
            );
            // Aborting again is silent.
            supervisor.handoff_owner_event(pane, event);
            assert!(crate::workers::take_user_notices().is_empty(), "{name}");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn ending_a_tenure_drops_its_pending_handoff() {
        let (root, supervisor, note) = scratch("end");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        supervisor
            .coordinator_end(&tenure.coordinator_id, "ended", None)
            .unwrap();
        supervisor
            .fence_tenure(Some(&tenure.coordinator_id))
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_relative_note_is_refused_by_the_server() {
        let (root, supervisor, _) = scratch("relative");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(
            code(supervisor.handoff_ready("p1", Path::new("note.md"))),
            "invalid_request"
        );
        assert_eq!(state(&supervisor, "p1"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// After the ack the old pane and session are superseded: refused with
    /// their own code until the pane closes, and the successor is not.
    #[test]
    fn the_old_pane_is_fenced_as_superseded_until_it_closes() {
        let (root, supervisor, note) = scratch("superseded");
        let (tenure, sha) = started(&supervisor, &note);
        supervisor
            .tenure_store()
            .unwrap()
            .transaction(|_| Ok(()))
            .unwrap();
        let acked = supervisor
            .handoff_ack("p2", 1, &sha, Some("s2"), None)
            .unwrap();
        assert_eq!(acked.old_pane, "p1");
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_superseded"
        );
        supervisor.fence_pane(Some("p2")).unwrap();
        supervisor.fence_pane(None).unwrap();
        {
            let registry = lock(&supervisor.shared.registry);
            let superseded = &registry.superseded[0];
            assert_eq!(superseded.coordinator_id, tenure.coordinator_id);
            assert_eq!(
                superseded.successor_coordinator_id,
                acked.tenure.coordinator_id
            );
            assert_eq!(superseded.epoch, 1);
        }
        // The old pane is still among the panes whose closing is reported.
        assert!(supervisor.owner_panes().contains(&"p1".to_owned()));
        supervisor.owner_event("p1", OwnerEvent::PaneClosed, "");
        supervisor.fence_pane(Some("p1")).unwrap();
        assert!(!supervisor.owner_panes().contains(&"p1".to_owned()));
        // The superseded record survives a restart.
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(lock(&supervisor.shared.registry).superseded.len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A successor that is blocked on a permission or trust dialog is
    /// starting normally: the handoff stays pending and its ack succeeds.
    #[test]
    fn a_blocked_successor_keeps_the_handoff_pending_until_its_ack() {
        let (root, supervisor, note) = scratch("blocked-ack");
        let (_, sha) = started(&supervisor, &note);
        let _ = crate::workers::take_user_notices();
        supervisor.handoff_owner_event("p2", OwnerEvent::Blocked);
        assert!(crate::workers::take_user_notices().is_empty());
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::SuccessorStarted)
        );
        assert!(supervisor.is_pending_successor("p2"));
        supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        assert!(!supervisor.is_pending_successor("p2"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A superseded record matches the pane and the session: the old
    /// session is fenced in any pane, a new session in the old pane id is
    /// not.
    #[test]
    fn a_superseded_session_is_fenced_wherever_it_runs() {
        let (root, supervisor, note) = scratch("session-fence");
        supervisor
            .coordinator_start("/repo", "p1", Some("s1"))
            .unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        let sha = supervisor
            .with_handoff(supervisor.coordinator_of_pane("p1").unwrap())
            .handoff
            .unwrap()
            .snapshot_sha256
            .unwrap();
        supervisor
            .handoff_ack("p2", 1, &sha, Some("s2"), None)
            .unwrap();
        // `claude --resume s1` in another pane.
        supervisor.note_pane_session("p9", Some("s1"));
        assert_eq!(
            code(supervisor.fence_pane(Some("p9"))),
            "coordinator_superseded"
        );
        // The old pane still runs the old session.
        supervisor.note_pane_session("p1", Some("s1"));
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_superseded"
        );
        // A new session that reuses the old pane id after a restart.
        supervisor.note_pane_session("p1", Some("s7"));
        supervisor.fence_pane(Some("p1")).unwrap();
        supervisor
            .fence_pane_session(Some("p2"), Some("s2"))
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// An ack or an abort whose state write fails changes nothing and says
    /// why.
    #[test]
    fn a_failed_write_refuses_the_ack_and_the_abort() {
        let (root, supervisor, note) = scratch("write-fails");
        let (_, sha) = started(&supervisor, &note);
        let path = supervisor.state_path();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(supervisor.handoff_ack("p2", 1, &sha, None, None).is_err());
        assert!(supervisor.handoff_abort_request(Some("p1"), None).is_err());
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::SuccessorStarted)
        );
        assert_eq!(supervisor.coordinator_of_pane("p1").unwrap().epoch, 1);
        supervisor.fence_pane(Some("p2")).unwrap();
        std::fs::remove_dir(&path).unwrap();
        supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        assert_eq!(supervisor.coordinator_of_pane("p2").unwrap().epoch, 2);
        let _ = std::fs::remove_dir_all(root);
    }

    /// At a restart an undetected successor agent keeps the handoff pending
    /// (the app asks [`WorkerSupervisor::is_pending_successor`]); a gone
    /// pane aborts it.
    #[test]
    fn a_pending_successor_pane_is_known_to_the_restart_check() {
        let (root, supervisor, note) = scratch("restart-undetected");
        started(&supervisor, &note);
        assert!(supervisor.is_pending_successor("p2"));
        assert!(!supervisor.is_pending_successor("p1"));
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert!(supervisor.is_pending_successor("p2"));
        // The app's check returns None for the undetected agent.
        supervisor.owners_at_start(|pane| match pane {
            "p2" => None,
            _ => Some(OwnerEvent::Working),
        });
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::SuccessorStarted)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_pane_that_cannot_close_stays_fenced() {
        let (root, supervisor, note) = scratch("stays-fenced");
        let (_, sha) = started(&supervisor, &note);
        supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        // No pane-closed event came: the fence holds, also after a restart.
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_superseded"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A restart or a live handoff keeps the tenure frozen and the
    /// successor's ack valid.
    #[test]
    fn a_restored_pending_handoff_accepts_the_ack() {
        let (root, supervisor, note) = scratch("restore-ack");
        let (_, sha) = started(&supervisor, &note);
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::SuccessorStarted)
        );
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_frozen"
        );
        // The successor's pane is watched again, so its exit is reported.
        assert!(supervisor.owner_panes().contains(&"p2".to_owned()));
        // A wrong ack changes nothing, the right one moves the tenure.
        assert_eq!(
            code(supervisor.handoff_ack("p2", 1, &"0".repeat(64), None, None)),
            "handoff_refused"
        );
        let acked = supervisor.handoff_ack("p2", 1, &sha, None, None).unwrap();
        assert_eq!(acked.tenure.epoch, 2);
        // The state file no longer holds the handoff.
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(state(&supervisor, "p2"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_restored_handoff_whose_successor_pane_is_gone_aborts_with_one_notice() {
        let (root, supervisor, note) = scratch("restore-gone");
        let (tenure, _) = started(&supervisor, &note);
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        let _ = crate::workers::take_user_notices();
        supervisor.owners_at_start(|pane| match pane {
            "p2" => Some(OwnerEvent::PaneClosed),
            _ => None,
        });
        let notices: Vec<_> = crate::workers::take_user_notices()
            .into_iter()
            .filter(|notice| notice.body.contains(&tenure.coordinator_id))
            .collect();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].title,
            format!("coordinator {}: handoff aborted", tenure.coordinator_id)
        );
        supervisor.fence_pane(Some("p1")).unwrap();
        // The abort is persisted too.
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(state(&supervisor, "p1"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_restored_frozen_tenure_that_ended_meanwhile_is_dropped() {
        let (root, supervisor, note) = scratch("restore-ended");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        drop(supervisor);
        // The tenure is gone when the server starts again: its state names
        // a tenure no one holds.
        let file = root.join("workers").join("handoffs").join("pending.json");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(&tenure.coordinator_id));
        std::fs::write(&file, text.replace(&tenure.coordinator_id, "c-gone0000")).unwrap();
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert!(lock(&supervisor.shared.registry).handoffs.is_empty());
        supervisor.fence_pane(Some("p1")).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_restored_frozen_tenure_can_still_be_aborted_or_given_a_successor() {
        let (root, supervisor, note) = scratch("restore-frozen");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(
            state(&supervisor, "p1"),
            Some(CoordinatorHandoffState::Frozen)
        );
        supervisor.handoff_prepare("p1", "claude").unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// `handoff --abort` from each place that may send it, and its refusals.
    #[test]
    fn a_handoff_is_aborted_by_hand_from_either_pane_or_by_id() {
        for (name, pane, id, closes) in [
            ("old", Some("p1"), None, Some("p2")),
            ("successor", Some("p2"), None, None),
            ("id", None, Some(true), Some("p2")),
        ] {
            let (root, supervisor, note) = scratch(name);
            let (tenure, sha) = started(&supervisor, &note);
            let _ = crate::workers::take_user_notices();
            let id = id.map(|_| tenure.coordinator_id.as_str());
            let aborted = supervisor.handoff_abort_request(pane, id).unwrap();
            assert_eq!(aborted.close_pane.as_deref(), closes, "{name}");
            let current = aborted.tenure.unwrap();
            assert_eq!(current.epoch, 1);
            assert_eq!(current.handoff, None);
            assert_eq!(crate::workers::take_user_notices().len(), 1, "{name}");
            supervisor.fence_pane(Some("p1")).unwrap();
            assert_eq!(
                code(supervisor.handoff_ack("p2", 1, &sha, None, None)),
                "handoff_refused"
            );
            // Nothing left to abort.
            assert_eq!(
                code(supervisor.handoff_abort_request(pane, id)),
                "handoff_refused"
            );
            let _ = std::fs::remove_dir_all(root);
        }
        let (root, supervisor, _) = scratch("abort-none");
        assert_eq!(
            code(supervisor.handoff_abort_request(None, None)),
            "handoff_refused"
        );
        assert_eq!(
            code(supervisor.handoff_abort_request(Some("p7"), None)),
            "handoff_refused"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_frozen_handoff_with_no_successor_is_aborted_from_the_old_pane() {
        let (root, supervisor, note) = scratch("abort-frozen");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        let aborted = supervisor.handoff_abort_request(Some("p1"), None).unwrap();
        assert_eq!(aborted.close_pane, None);
        supervisor.fence_pane(Some("p1")).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A write that fails on an event path is shown on the tenure.
    #[test]
    fn a_failed_event_write_is_shown_on_the_tenure() {
        let (root, supervisor, note) = scratch("event-write");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        WRITE_FAILURES.with(|f| *f.borrow_mut() = vec![true]);
        let info = supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        assert!(info.handoff_persist_error.is_some());
        WRITE_FAILURES.with(|f| *f.borrow_mut() = vec![true]);
        supervisor.handoff_owner_event("p2", OwnerEvent::PaneClosed);
        let tenure = supervisor.with_handoff(supervisor.coordinator_of_pane("p1").unwrap());
        assert!(tenure.handoff.is_none());
        assert!(tenure.handoff_persist_error.is_some());
        // The next write that succeeds clears it.
        supervisor.handoff_ready("p1", &note).unwrap();
        supervisor.handoff_prepare("p1", "claude").unwrap();
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p3")
            .unwrap();
        let tenure = supervisor.with_handoff(supervisor.coordinator_of_pane("p1").unwrap());
        assert_eq!(tenure.handoff_persist_error, None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A restart aborts a handoff whose phase no live successor matches.
    #[test]
    fn a_restored_handoff_without_its_recorded_successor_aborts() {
        let (root, supervisor, note) = scratch("restore-lost-start");
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        supervisor.handoff_prepare("p1", "claude").unwrap();
        // The successor-started write failed: the file stays `frozen`.
        drop(supervisor);
        let _ = crate::workers::take_user_notices();
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(state(&supervisor, "p1"), None);
        supervisor.fence_pane(Some("p1")).unwrap();
        assert_eq!(
            crate::workers::take_user_notices()
                .into_iter()
                .filter(|notice| notice.title.starts_with("coordinator ")
                    && notice.title.ends_with(": handoff aborted"))
                .count(),
            1
        );
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(state(&supervisor, "p1"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The ack's transfer fails and so does the write that restores the
    /// file: memory keeps the old tenure and a restart does not fence it.
    #[test]
    fn a_double_failure_in_the_ack_never_supersedes_the_live_coordinator() {
        let (root, supervisor, note) = scratch("ack-double");
        let (tenure, sha) = started(&supervisor, &note);
        // The successor's pane coordinates another repository: the
        // transfer is refused.
        supervisor.coordinator_start("/other", "p2", None).unwrap();
        WRITE_FAILURES.with(|f| *f.borrow_mut() = vec![false, true]);
        assert!(supervisor.handoff_ack("p2", 1, &sha, None, None).is_err());
        assert!(lock(&supervisor.shared.registry).superseded.is_empty());
        supervisor.coordinator_of_pane("p1").unwrap();
        let shown = supervisor.with_handoff(supervisor.coordinator_of_pane("p1").unwrap());
        assert!(shown.handoff_persist_error.is_some());
        // The file claims the supersession; a restart ignores it.
        let text = std::fs::read_to_string(supervisor.state_path()).unwrap();
        assert!(text.contains("successor_coordinator_id"));
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert!(lock(&supervisor.shared.registry).superseded.is_empty());
        supervisor.fence_pane(Some("p1")).unwrap();
        assert_eq!(
            supervisor.coordinator_of_pane("p1").unwrap().coordinator_id,
            tenure.coordinator_id
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// An unknown session on the old pane id is not fenced; the old
    /// session is.
    #[test]
    fn an_unknown_session_on_a_reused_old_pane_is_not_fenced() {
        let (root, supervisor, note) = scratch("unknown-session");
        supervisor
            .coordinator_start("/repo", "p1", Some("s1"))
            .unwrap();
        supervisor.handoff_ready("p1", &note).unwrap();
        let prepared = supervisor.handoff_prepare("p1", "claude").unwrap();
        supervisor
            .handoff_successor_started(&prepared.coordinator_id, "p2")
            .unwrap();
        let sha = supervisor
            .with_handoff(supervisor.coordinator_of_pane("p1").unwrap())
            .handoff
            .unwrap()
            .snapshot_sha256
            .unwrap();
        supervisor
            .handoff_ack("p2", 1, &sha, Some("s2"), None)
            .unwrap();
        supervisor.note_pane_session("p1", Some("s1"));
        assert_eq!(
            code(supervisor.fence_pane(Some("p1"))),
            "coordinator_superseded"
        );
        supervisor.note_pane_session("p1", None);
        supervisor.fence_pane(Some("p1")).unwrap();
        supervisor.fence_pane_session(Some("p1"), None).unwrap();
        assert_eq!(
            code(supervisor.fence_pane_session(Some("p1"), Some("s1"))),
            "coordinator_superseded"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A closed pane's session entry goes, and startup drops the rest.
    #[test]
    fn the_pane_session_map_is_pruned() {
        let (root, supervisor, _) = scratch("prune");
        supervisor.note_pane_session("p1", Some("s1"));
        supervisor.note_pane_session("p2", Some("s2"));
        supervisor.note_pane_session("p3", Some("s3"));
        supervisor.owner_event("p1", OwnerEvent::PaneClosed, "");
        supervisor.handoff_old_pane_closed("p2");
        let keys = |s: &WorkerSupervisor| -> Vec<String> {
            lock(&s.shared.registry)
                .pane_sessions
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(keys(&supervisor), vec!["p3".to_owned()]);
        supervisor.note_pane_session("p4", Some("s4"));
        supervisor.prune_pane_sessions(&["p4".to_owned()]);
        assert_eq!(keys(&supervisor), vec!["p4".to_owned()]);
        let _ = std::fs::remove_dir_all(root);
    }
}
