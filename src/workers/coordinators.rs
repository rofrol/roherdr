//! Coordination tenures: which pane coordinates a repository's TODO, from
//! when to when. A tenure has an id of its own (`c-...`), never a pane's or
//! an agent session's: those are its bindings. They live in the worker
//! store, as projections of the `coordinator_started` and
//! `coordinator_ended` events written in the same transaction, and its
//! partial unique index allows one active tenure per repository: that index
//! is the claim `herdr coordinator start` makes. A tenure ends when asked,
//! when its tab's coordinator role is cleared, and, as `orphaned`, on the
//! events herdr already has about its pane (it closed, its agent exited),
//! which a server re-evaluates when it starts. No timer ends one.

use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tracing::warn;

use super::store::{NewTenure, StoredTenure};
use super::{notify_clients, now_ms, WorkerError, WorkerSupervisor, SUPERVISOR};
use crate::api::schema::{CoordinatorInfo, CoordinatorOverride, CoordinatorRecordOverrideParams};

/// `coordinator_ended`'s reason when its pane closed or its agent exited.
pub(crate) const ORPHANED: &str = "orphaned";
/// `coordinator_ended`'s reason when its tab's coordinator role was cleared.
pub(crate) const ROLE_CLEARED: &str = "role_cleared";
/// `coordinator.end`'s reason when none is given.
pub(crate) const ENDED: &str = "ended";
/// How many characters of an allowlist exception's command are kept.
const OVERRIDE_COMMAND_CHARS: usize = 4000;

#[cfg(test)]
thread_local! {
    static TEST_SUPERVISOR: std::cell::Cell<Option<&'static WorkerSupervisor>> =
        const { std::cell::Cell::new(None) };
}

/// The supervisor whose store holds the tenures: the server's, opened on
/// first use. A test gets only the one it set with
/// [`set_test_coordinators`] (none otherwise), so no test writes the
/// user's state directory.
pub(crate) fn coordinators() -> Option<&'static WorkerSupervisor> {
    #[cfg(test)]
    {
        TEST_SUPERVISOR.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        Some(super::supervisor())
    }
}

/// The supervisor herdr's pane and agent events go to, when one is open.
pub(super) fn installed() -> Option<&'static WorkerSupervisor> {
    #[cfg(test)]
    if let Some(supervisor) = TEST_SUPERVISOR.with(std::cell::Cell::get) {
        return Some(supervisor);
    }
    SUPERVISOR.get()
}

/// Makes `supervisor` this test thread's [`coordinators`].
#[cfg(test)]
pub(crate) fn set_test_coordinators(supervisor: WorkerSupervisor) {
    let leaked: &'static WorkerSupervisor = Box::leak(Box::new(supervisor));
    TEST_SUPERVISOR.with(|cell| cell.set(Some(leaked)));
}

/// A new tenure id: `c-` and 8 lowercase base32 characters, from a hash of
/// the time, this process, a counter and the claim.
fn new_tenure_id(repo: &str, pane_id: &str) -> String {
    static CLAIMS: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let claim = CLAIMS.fetch_add(1, Ordering::Relaxed);
    let digest = Sha256::digest(format!(
        "{nanos}:{}:{claim}:{repo}:{pane_id}",
        std::process::id()
    ));
    let bits = digest[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("c-{id}")
}

fn info(tenure: StoredTenure) -> CoordinatorInfo {
    CoordinatorInfo {
        coordinator_id: tenure.id,
        repo: tenure.repo,
        item: tenure.item,
        started_ms: tenure.started_at,
        ended_ms: tenure.ended_at,
        end_reason: tenure.end_reason,
        epoch: tenure.epoch,
        pane_id: tenure.pane_id,
        session_id: tenure.session_id,
    }
}

fn store_error(error: rusqlite::Error) -> WorkerError {
    WorkerError::Io(std::io::Error::other(format!(
        "the worker store failed: {error}"
    )))
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// The title and body of the notification that tells the user about an
/// allowlist exception: the reason, then the command's first line.
pub(crate) fn override_notice(record: &CoordinatorOverride) -> (String, String) {
    let command = record.command.lines().next().unwrap_or_default();
    let command: String = command.chars().take(120).collect();
    (
        "Coordinator override".to_owned(),
        format!("{}: {command}", record.reason),
    )
}

impl WorkerSupervisor {
    fn tenure_store(&self) -> Result<&super::store::Store, WorkerError> {
        self.shared
            .store
            .as_ref()
            .map_err(|error| WorkerError::Io(std::io::Error::other(error.clone())))
    }

    /// Starts a tenure of `repo` bound to `pane_id`, refused with
    /// `coordinator_active` while the repository has another active tenure
    /// (named with its pane) or the pane coordinates another repository.
    /// The pane's own active tenure of `repo` is returned as it is.
    pub(crate) fn coordinator_start(
        &self,
        repo: &str,
        pane_id: &str,
        session_id: Option<&str>,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let store = self.tenure_store()?;
        let id = new_tenure_id(repo, pane_id);
        let outcome = store.transaction(|tx| {
            if let Some(active) = tx.active_coordinator(repo)? {
                return Ok(Err(active));
            }
            if let Some(bound) = tx.coordinator_of_pane(pane_id)? {
                return Ok(Err(bound));
            }
            let tenure = NewTenure {
                id: &id,
                repo,
                pane_id,
                session_id,
            };
            tx.coordinator_started(&tenure, now_ms()).map(Ok)
        });
        let refused = match outcome {
            Ok(Ok(tenure)) => {
                notify_clients();
                return Ok(info(tenure));
            }
            Ok(Err(other)) => other,
            // Another server's tenure committed between the check and the
            // insert: the index refused this one.
            Err(error) if is_unique_violation(&error) => {
                return match store.active_coordinators(Some(repo)) {
                    Ok(mut active) if !active.is_empty() => {
                        Err(Self::refusal(repo, pane_id, active.remove(0)))
                    }
                    _ => Err(WorkerError::CoordinatorActive(format!(
                        "repository {repo} already has an active coordinator"
                    ))),
                };
            }
            Err(error) => return Err(store_error(error)),
        };
        if refused.repo == repo && refused.pane_id.as_deref() == Some(pane_id) {
            return Ok(info(refused));
        }
        Err(Self::refusal(repo, pane_id, refused))
    }

    fn refusal(repo: &str, pane_id: &str, other: StoredTenure) -> WorkerError {
        let other_pane = other.pane_id.as_deref().unwrap_or("an unknown pane");
        WorkerError::CoordinatorActive(if other.repo == repo {
            format!(
                "repository {repo} already has coordinator {} in pane {other_pane}; it ends \
                 with `herdr coordinator end` from that pane, or when that pane closes or its \
                 agent exits",
                other.id
            )
        } else {
            format!(
                "pane {pane_id} already coordinates repository {} as {}",
                other.repo, other.id
            )
        })
    }

    /// Ends tenure `id` with `reason` (and `cause`, what herdr saw); a
    /// tenure that has ended already is returned as it is.
    pub(crate) fn coordinator_end(
        &self,
        id: &str,
        reason: &str,
        cause: Option<&str>,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 200 || reason.chars().any(char::is_control) {
            return Err(WorkerError::Invalid(
                "reason must be one nonempty line of at most 200 bytes".into(),
            ));
        }
        let ended = self
            .tenure_store()?
            .transaction(|tx| tx.coordinator_ended(id, reason, cause, now_ms()))
            .map_err(store_error)?;
        let tenure = ended.ok_or_else(|| {
            WorkerError::CoordinatorNotFound(format!("coordinator {id} not found"))
        })?;
        notify_clients();
        Ok(info(tenure))
    }

    /// The active tenures, of one repository when given.
    pub(crate) fn coordinator_status(
        &self,
        repo: Option<&str>,
    ) -> Result<Vec<CoordinatorInfo>, WorkerError> {
        Ok(self
            .tenure_store()?
            .active_coordinators(repo)
            .map_err(store_error)?
            .into_iter()
            .map(info)
            .collect())
    }

    /// The active tenure bound to `pane_id`; none when the store cannot say.
    pub(crate) fn coordinator_of_pane(&self, pane_id: &str) -> Option<CoordinatorInfo> {
        let store = self.shared.store.as_ref().ok()?;
        match store.coordinator_of_pane(pane_id) {
            Ok(tenure) => tenure.map(info),
            Err(error) => {
                warn!(%error, pane_id, "cannot read the pane's coordination tenure");
                None
            }
        }
    }

    /// The panes the active tenures are bound to; an error when the store
    /// cannot say, so a caller does not take that for "none".
    pub(crate) fn coordinator_panes(&self) -> Result<Vec<String>, WorkerError> {
        let mut panes: Vec<String> = self
            .coordinator_status(None)?
            .into_iter()
            .filter_map(|tenure| tenure.pane_id)
            .collect();
        panes.sort();
        panes.dedup();
        Ok(panes)
    }

    /// Records one exception to a coordinator tab's command allowlist, made
    /// in pane `pane_id` (its public id): with the pane's active tenure, its
    /// repository and item, else the repository of `params.cwd`. The reason
    /// is one line of at most 500 bytes; a longer command is cut to its first
    /// [`OVERRIDE_COMMAND_CHARS`] characters, so the record is never refused
    /// for its size.
    pub(crate) fn record_override(
        &self,
        pane_id: &str,
        params: &CoordinatorRecordOverrideParams,
    ) -> Result<CoordinatorOverride, WorkerError> {
        let reason = params.reason.trim();
        if reason.is_empty() || reason.len() > 500 || reason.chars().any(char::is_control) {
            return Err(WorkerError::Invalid(
                "reason must be one nonempty line of at most 500 bytes".into(),
            ));
        }
        let tool = params.tool.trim();
        if tool.is_empty() || tool.len() > 100 || tool.chars().any(char::is_control) {
            return Err(WorkerError::Invalid("tool must be a tool's name".into()));
        }
        if params.command.trim().is_empty() {
            return Err(WorkerError::Invalid("command is empty".into()));
        }
        let tenure = self.coordinator_of_pane(pane_id);
        let repo = match &tenure {
            Some(tenure) => Some(tenure.repo.clone()),
            None => params.cwd.as_deref().and_then(super::repository_of_dir),
        };
        let record = CoordinatorOverride {
            id: 0,
            ts_ms: now_ms(),
            pane_id: pane_id.to_owned(),
            tool: tool.to_owned(),
            command: params
                .command
                .chars()
                .take(OVERRIDE_COMMAND_CHARS)
                .collect(),
            reason: reason.to_owned(),
            repo,
            coordinator_id: tenure.as_ref().map(|tenure| tenure.coordinator_id.clone()),
            item: tenure.and_then(|tenure| tenure.item),
            session_id: params.session_id.clone(),
        };
        self.tenure_store()?
            .record_override(&record)
            .map_err(store_error)
    }

    /// The recorded allowlist exceptions, oldest first: of the repository of
    /// `repo` (a directory in it) when given.
    pub(crate) fn overrides(
        &self,
        repo: Option<&str>,
    ) -> Result<Vec<CoordinatorOverride>, WorkerError> {
        let repo = repo.map(|dir| super::repository_of_dir(dir).unwrap_or_else(|| dir.to_owned()));
        self.tenure_store()?
            .overrides(repo.as_deref())
            .map_err(store_error)
    }

    /// Ends the tenure bound to `pane_id` as orphaned: its pane closed or its
    /// agent exited (`cause`).
    pub(super) fn orphan_coordinator(&self, pane_id: &str, cause: &str) {
        let Some(tenure) = self.coordinator_of_pane(pane_id) else {
            return;
        };
        if let Err(error) = self.coordinator_end(&tenure.coordinator_id, ORPHANED, Some(cause)) {
            warn!(
                %error,
                pane_id,
                coordinator_id = %tenure.coordinator_id,
                "cannot end the orphaned coordination tenure"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workers::OwnerEvent;

    fn scratch(name: &str) -> (std::path::PathBuf, WorkerSupervisor) {
        let root = std::env::temp_dir().join(format!(
            "herdr-coordinators-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        (root, supervisor)
    }

    fn override_params(command: &str, reason: &str) -> CoordinatorRecordOverrideParams {
        CoordinatorRecordOverrideParams {
            pane_id: "p1".into(),
            tool: "Bash".into(),
            command: command.into(),
            reason: reason.into(),
            cwd: None,
            session_id: Some("s-1".into()),
        }
    }

    #[test]
    fn allowlist_overrides_are_recorded_with_the_panes_tenure_and_never_changed() {
        let (root, supervisor) = scratch("overrides");
        let tenure = supervisor
            .coordinator_start("/repo", "p1", Some("s-1"))
            .unwrap();
        let first = supervisor
            .record_override(
                "p1",
                &override_params("cargo build # herdr-override: x", " x "),
            )
            .unwrap();
        assert_eq!(first.reason, "x");
        assert_eq!(first.repo.as_deref(), Some("/repo"));
        assert_eq!(
            first.coordinator_id.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );
        assert_eq!(first.session_id.as_deref(), Some("s-1"));
        // A pane without a tenure, whose directory is in no repository.
        let long = "x".repeat(OVERRIDE_COMMAND_CHARS + 10);
        let mut params = override_params(&long, "the user asked");
        params.cwd = Some(root.display().to_string());
        let second = supervisor.record_override("p9", &params).unwrap();
        assert_eq!(second.coordinator_id, None);
        assert_eq!(second.command.chars().count(), OVERRIDE_COMMAND_CHARS);
        assert!(second.id > first.id);

        assert_eq!(
            supervisor.overrides(None).unwrap(),
            vec![first.clone(), second]
        );
        assert_eq!(supervisor.overrides(Some("/repo")).unwrap(), vec![first]);
        let (title, body) = override_notice(&supervisor.overrides(None).unwrap()[0]);
        assert_eq!(title, "Coordinator override");
        assert_eq!(body, "x: cargo build # herdr-override: x");

        for (command, reason) in [("ls", ""), ("ls", "two\nlines"), (" ", "why")] {
            let refused = supervisor
                .record_override("p1", &override_params(command, reason))
                .unwrap_err();
            assert_eq!(refused.code(), "invalid_request", "{command:?} {reason:?}");
        }
        let store = supervisor.tenure_store().unwrap();
        for change in [
            "UPDATE coordinator_overrides SET reason = 'y'",
            "DELETE FROM coordinator_overrides",
        ] {
            assert!(store.connection().execute(change, []).is_err(), "{change}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tenure_ids_are_c_and_eight_base32_characters() {
        let first = new_tenure_id("/repo", "p1");
        assert_eq!(first.len(), 10, "{first}");
        assert!(first.starts_with("c-"), "{first}");
        assert!(first[2..]
            .chars()
            .all(|c| c.is_ascii_lowercase() || ('2'..='7').contains(&c)));
        assert_ne!(first, new_tenure_id("/repo", "p1"));
    }

    #[test]
    fn a_repository_has_one_active_coordinator() {
        let (root, supervisor) = scratch("one");
        let started = supervisor
            .coordinator_start("/repo", "p1", Some("s-1"))
            .unwrap();
        assert_eq!(
            (started.epoch, started.pane_id.as_deref(), started.ended_ms),
            (1, Some("p1"), None)
        );
        // Its own pane asking again gets the same tenure.
        assert_eq!(
            supervisor.coordinator_start("/repo", "p1", None).unwrap(),
            started
        );
        // Another pane is refused, named with the active tenure and pane.
        let refused = supervisor
            .coordinator_start("/repo", "p2", None)
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active");
        let message = refused.to_string();
        assert!(
            message.contains(&started.coordinator_id) && message.contains("p1"),
            "{message}"
        );
        // A pane coordinates one repository at a time.
        let refused = supervisor
            .coordinator_start("/other", "p1", None)
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active");
        // Another repository has a coordinator of its own.
        supervisor.coordinator_start("/other", "p3", None).unwrap();
        assert_eq!(supervisor.coordinator_status(None).unwrap().len(), 2);

        let ended = supervisor
            .coordinator_end(&started.coordinator_id, ENDED, None)
            .unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ENDED));
        assert!(ended.ended_ms.is_some());
        // Ending it again changes nothing.
        assert_eq!(
            supervisor
                .coordinator_end(&started.coordinator_id, "other", None)
                .unwrap(),
            ended
        );
        assert_eq!(
            supervisor.coordinator_status(Some("/repo")).unwrap(),
            Vec::new()
        );
        // The next tenure of the repository has the next epoch.
        let next = supervisor.coordinator_start("/repo", "p2", None).unwrap();
        assert_eq!(next.epoch, 2);
        assert_ne!(next.coordinator_id, started.coordinator_id);
        assert_eq!(
            supervisor
                .coordinator_end("c-nosuchid", ENDED, None)
                .unwrap_err()
                .code(),
            "coordinator_not_found"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_events_and_their_projection_are_stored_together() {
        let (root, supervisor) = scratch("events");
        let started = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        supervisor
            .coordinator_end(&started.coordinator_id, ENDED, None)
            .unwrap();
        let store = supervisor.shared.store.as_ref().unwrap();
        let conn = store.connection();
        let types: Vec<String> = conn
            .prepare("SELECT type FROM events WHERE worker_id = ?1 ORDER BY seq")
            .unwrap()
            .query_map([&started.coordinator_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(types, ["coordinator_started", "coordinator_ended"]);
        let bound: (String, Option<i64>) = conn
            .query_row(
                "SELECT pane_id, to_at FROM coordinator_bindings WHERE coordinator_id = ?1",
                [&started.coordinator_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(bound.0, "p1");
        assert!(bound.1.is_some());
        // The index refuses a second active row however it is written.
        conn.execute(
            "INSERT INTO coordinators (id, repo, started_at, epoch) VALUES ('c-a', '/r', 1, 1)",
            [],
        )
        .unwrap();
        let refused = conn
            .execute(
                "INSERT INTO coordinators (id, repo, started_at, epoch) VALUES ('c-b', '/r', 2, 2)",
                [],
            )
            .unwrap_err();
        assert!(is_unique_violation(&refused), "{refused}");
        drop(conn);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_closed_pane_or_exited_agent_orphans_its_tenure() {
        let (root, supervisor) = scratch("orphaned");
        let first = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let second = supervisor.coordinator_start("/other", "p2", None).unwrap();
        assert_eq!(supervisor.owner_panes(), ["p1".to_owned(), "p2".to_owned()]);
        // Not the end of a tenure.
        supervisor.owner_event("p1", OwnerEvent::TurnEnded, "");
        supervisor.owner_event("p1", OwnerEvent::Blocked, "");
        assert!(supervisor.coordinator_of_pane("p1").is_some());

        supervisor.owner_event("p1", OwnerEvent::PaneClosed, "");
        supervisor.owner_event("p2", OwnerEvent::AgentExited, "");
        let store = supervisor.shared.store.as_ref().unwrap();
        for (tenure, cause) in [
            (&first, "the coordinator's pane closed"),
            (&second, "the coordinator's agent exited"),
        ] {
            let stored = store.coordinator(&tenure.coordinator_id).unwrap().unwrap();
            assert_eq!(stored.end_reason.as_deref(), Some(ORPHANED));
            let body: String = store
                .connection()
                .query_row(
                    "SELECT body FROM events WHERE worker_id = ?1 AND type = 'coordinator_ended'",
                    [&tenure.coordinator_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(body.contains(cause), "{body}");
        }
        assert!(supervisor.owner_panes().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_restarted_server_re_evaluates_the_active_tenures() {
        let (root, supervisor) = scratch("restart");
        let gone = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let alive = supervisor.coordinator_start("/other", "p2", None).unwrap();
        drop(supervisor);
        // The next server finds both in the store, and judges each pane by
        // what it shows now.
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(supervisor.coordinator_status(None).unwrap().len(), 2);
        supervisor.owners_at_start(|pane| match pane {
            "p1" => Some(OwnerEvent::PaneClosed),
            _ => Some(OwnerEvent::Working),
        });
        let active = supervisor.coordinator_status(None).unwrap();
        assert_eq!(
            active
                .iter()
                .map(|tenure| tenure.coordinator_id.as_str())
                .collect::<Vec<_>>(),
            [alive.coordinator_id.as_str()]
        );
        let ended = supervisor
            .tenure_store()
            .unwrap()
            .coordinator(&gone.coordinator_id)
            .unwrap()
            .unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        let _ = std::fs::remove_dir_all(root);
    }
}
