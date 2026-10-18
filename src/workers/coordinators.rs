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
//!
//! A tenure is the repository's lease. `coordinator.acquire` gives its
//! holder a token (only its sha256 is stored), which `worker.run` must
//! present while the tenure is active, and lets a process outside herdr's
//! panes (a supervisor started from a shell) hold the tenure itself: the
//! server records the process (its pid and start token) and ends the tenure
//! on the process's exit event ([`crate::platform::watch_process_exit`]),
//! and a server that starts, also after a live handoff, watches the
//! recorded holders again and ends the tenures of those that are gone. A
//! `todo.run` lands (cherry-pick, install, TODO edit, push) only while the
//! tenure it was claimed under is still the repository's active one
//! ([`WorkerSupervisor::begin_landing`]).
//!
//! A tenure check holds through the step it guards, so a tenure change can
//! never fall between a check and the work it admits. Two mechanisms, one
//! for each step:
//!
//! - A landing step is admitted in one transaction that checks the fence and
//!   records the step (`tenure_steps`), and the guard it returns holds its
//!   tenure from there until the step lands. While a step is in flight its
//!   tenure ends only through that table: `coordinator_end` records the end
//!   as pending and the step's release carries it out, so the step lands
//!   under its own tenure, never under a successor's, and no successor can
//!   claim the repository in between (the unique index still holds the
//!   tenure).
//! - `worker.run`'s token check runs in the transaction that records its
//!   usage admission ([`crate::workers::store::tenure_token_check`]), which
//!   also names the tenure it admitted, and the token is checked again in the
//!   transaction that records the start step just before the worker's process
//!   exists ([`WorkerSupervisor::begin_start_step`]). A tenure that ended, or
//!   a token a later acquire replaced, between the admission and the spawn
//!   starts nothing, and the step holds the tenure through the spawn itself.
//!
//! An end herdr saw itself (a holder process exited, a pane closed, an agent
//! exited) is persisted first (`tenure_pending_ends`) and carried out in the
//! same call when it can be; one a landing step holds back stays recorded and
//! is carried out by the next reader — a claim attempt, a server that starts,
//! `coordinator end`, or the step's release — never by a timer. A pending end
//! names its tenure's epoch, and it is applied only to that epoch, so a stale
//! end cannot end a later tenure of the same id. The same readers also check
//! a recorded holder's process again, so a tenure whose exit event herdr never
//! received (a watch it could not place, or one the kernel dropped) is
//! released there.
//!
//! A step whose release failed (a busy store, an unwind that did not run) is
//! retried by the next reader on this server, and a step whose admitting
//! server is gone is retired by a reader that matches the recorded pid and
//! start token, never the pid alone; the row records the retirer. A step a
//! live server genuinely abandoned is not retired by another reader: restart
//! herdr (a live handoff of an install does it), which gives the step a
//! server identity no live process has, and the next server retires it. The
//! Linux `poll` and macOS `kevent` error paths stop a watch without calling
//! its callback; the next reader's re-check of the holder's process ends that
//! tenure, and a platform-side fix is a separate change outside this module.
//!
//! `coordinator.acquire --pid` takes the pid the caller names: any process of
//! the same user can name another holder's pid and rotate its token. That is
//! the same-user trust the design already rests on (a same-user process can
//! also signal, read and write whatever the holder can), and the user accepted
//! it (2026-10-17, item t-bbwge2bc). Who rotated is not recorded: the requester
//! never reaches the store beyond the pid it names, so a fix costs the peer
//! identity a socket request does not carry.
//!
//! The workers and `todo.run`s a coordinator starts belong to its tenure,
//! not to its pane: when its agent session comes back in another pane
//! (`claude --resume`), the tenure's binding moves there with them
//! ([`WorkerSupervisor::coordinator_resume`]), and `coordinator.handoff`
//! ends the tenure and starts the next one, with the next epoch, in the
//! pane it names, moving them there in the same transaction.

use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tracing::warn;

use serde_json::{json, Value};

use super::store::{HolderProcess, NewTenure, RunOwner, StoredTenure, Tx};
use super::{
    lock, notify_clients, now_ms, Registry, StagedMoves, WorkerError, WorkerSupervisor, SUPERVISOR,
};
use crate::api::schema::{
    CoordinatorAllowlistMode, CoordinatorAllowlistRefusalParams, CoordinatorHolderKind,
    CoordinatorInfo, CoordinatorOverride, CoordinatorRecordOverrideParams, CoordinatorWouldDeny,
    CoordinatorWouldDenyShape,
};

/// `coordinator_ended`'s reason when its pane closed or its agent exited.
pub(crate) const ORPHANED: &str = "orphaned";
/// `coordinator_ended`'s reason when its tab's coordinator role was cleared.
pub(crate) const ROLE_CLEARED: &str = "role_cleared";
/// `coordinator.end`'s reason when none is given.
pub(crate) const ENDED: &str = "ended";
/// `coordinator_ended`'s reason when `coordinator.handoff` passed the
/// coordination on to the next tenure.
pub(crate) const HANDED_OFF: &str = "handed_off";
/// How many characters of an allowlist exception's command are kept.
const OVERRIDE_COMMAND_CHARS: usize = 4000;

/// How many characters of a would-deny's reason are kept.
const WOULD_DENY_REASON_CHARS: usize = 1000;

#[cfg(test)]
thread_local! {
    static TEST_ALLOWLIST_MODE: std::cell::Cell<CoordinatorAllowlistMode> =
        const { std::cell::Cell::new(CoordinatorAllowlistMode::Shadow) };
}

/// Sets the mode [`configured_allowlist_mode`] returns on this test thread.
#[cfg(test)]
pub(crate) fn set_test_allowlist_mode(mode: CoordinatorAllowlistMode) {
    TEST_ALLOWLIST_MODE.with(|cell| cell.set(mode));
}

/// `[coordinator] allowlist`, read from the config file at each call, so a
/// change applies to the next refused call. A config that cannot be read
/// gives the default, shadow, which never denies. A mode that differs from
/// the one last published is published again ([`publish_allowlist_mode`]).
pub(crate) fn configured_allowlist_mode() -> CoordinatorAllowlistMode {
    #[cfg(test)]
    {
        TEST_ALLOWLIST_MODE.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        use crate::config::CoordinatorAllowlistConfig;
        let mode = match crate::config::load_live_config() {
            Ok(loaded) => match loaded.config.coordinator.allowlist {
                CoordinatorAllowlistConfig::Shadow => CoordinatorAllowlistMode::Shadow,
                CoordinatorAllowlistConfig::Enforce => CoordinatorAllowlistMode::Enforce,
                CoordinatorAllowlistConfig::Off => CoordinatorAllowlistMode::Off,
            },
            Err(diagnostics) => {
                warn!(
                    "the config could not be read, so the coordinator allowlist is in shadow \
                     mode: {}",
                    diagnostics.join("; ")
                );
                CoordinatorAllowlistMode::Shadow
            }
        };
        #[cfg(unix)]
        publish_allowlist_mode(mode);
        mode
    }
}

/// The mode this server last wrote to its [`allowlist_mode_path`].
#[cfg(all(unix, not(test)))]
static PUBLISHED_ALLOWLIST_MODE: std::sync::Mutex<Option<CoordinatorAllowlistMode>> =
    std::sync::Mutex::new(None);

/// Publishes `[coordinator] allowlist` again, as the server starts and
/// after it reloads its config, so the Claude hook has the current mode
/// when herdr cannot answer it.
pub(crate) fn republish_allowlist_mode() {
    #[cfg(all(unix, not(test)))]
    {
        *lock(&PUBLISHED_ALLOWLIST_MODE) = None;
        configured_allowlist_mode();
    }
}

/// Writes `mode` to the [`allowlist_mode_path`] of this server's API socket
/// when it differs from the mode last written.
#[cfg(all(unix, not(test)))]
fn publish_allowlist_mode(mode: CoordinatorAllowlistMode) {
    let mut published = lock(&PUBLISHED_ALLOWLIST_MODE);
    if *published == Some(mode) {
        return;
    }
    let path = allowlist_mode_path(&crate::api::socket_path());
    match write_allowlist_mode(&path, mode) {
        Ok(()) => *published = Some(mode),
        Err(error) => warn!(
            "cannot publish the coordinator allowlist mode to {}: {error}",
            path.display()
        ),
    }
}

/// Where a server publishes `[coordinator] allowlist` for the Claude hook:
/// beside its API socket, so a hook finds it from `HERDR_SOCKET_PATH` and
/// knows the mode without a round trip when herdr cannot answer (shadow and
/// off never deny then).
#[cfg(unix)]
pub(crate) fn allowlist_mode_path(socket: &std::path::Path) -> std::path::PathBuf {
    let mut path = socket.as_os_str().to_owned();
    path.push(".allowlist-mode");
    path.into()
}

/// Writes `mode` (`shadow`, `enforce` or `off`) to `path` atomically: to a
/// temporary file of this process beside it, renamed over it.
#[cfg(unix)]
pub(crate) fn write_allowlist_mode(
    path: &std::path::Path,
    mode: CoordinatorAllowlistMode,
) -> std::io::Result<()> {
    let name = match mode {
        CoordinatorAllowlistMode::Shadow => "shadow",
        CoordinatorAllowlistMode::Enforce => "enforce",
        CoordinatorAllowlistMode::Off => "off",
        // Never configured; a hook reads an unknown mode as no file.
        CoordinatorAllowlistMode::Unknown => "unknown",
    };
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, format!("{name}\n"))
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// The shape a would-deny is counted under: the tool, and for Bash the
/// program of the first command that is not `cd` (leading `VAR=value`
/// words and `env` skipped, the path cut to its file name) with its first
/// argument that is not an option, such as `Bash: cargo build`.
pub(crate) fn command_shape(tool: &str, command: &str) -> String {
    if tool != "Bash" {
        return tool.to_owned();
    }
    let segments = command.split(['\n', ';', '|', '&']);
    for segment in segments {
        let mut words = segment
            .split_whitespace()
            .map(|word| word.trim_matches(|c| c == '\'' || c == '"'))
            .take_while(|word| !word.starts_with('#'))
            .skip_while(|word| {
                *word == "env"
                    || word.is_empty()
                    || (word.contains('=') && !word.starts_with('-') && !word.starts_with('='))
            });
        let Some(program) = words.next() else {
            continue;
        };
        let program = program.rsplit('/').next().unwrap_or(program);
        if program.is_empty() || program == "cd" {
            continue;
        }
        let program: String = program.chars().take(40).collect();
        return match words.find(|word| !word.starts_with('-') && !word.is_empty()) {
            Some(argument) => {
                let argument: String = argument.chars().take(40).collect();
                format!("Bash: {program} {argument}")
            }
            None => format!("Bash: {program}"),
        };
    }
    "Bash".to_owned()
}

/// The would-denies counted per command shape: the most frequent first,
/// then the latest.
pub(crate) fn would_deny_shapes(
    records: &[CoordinatorWouldDeny],
) -> Vec<CoordinatorWouldDenyShape> {
    let mut shapes: Vec<CoordinatorWouldDenyShape> = Vec::new();
    for record in records {
        let shape = command_shape(&record.tool, &record.command);
        match shapes.iter_mut().find(|known| known.shape == shape) {
            Some(known) => {
                known.count += 1;
                if record.ts_ms >= known.last_ts_ms {
                    known.last_ts_ms = record.ts_ms;
                    known.reason = record.reason.clone();
                }
            }
            None => shapes.push(CoordinatorWouldDenyShape {
                shape,
                count: 1,
                last_ts_ms: record.ts_ms,
                reason: record.reason.clone(),
            }),
        }
    }
    shapes.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then(b.last_ts_ms.cmp(&a.last_ts_ms))
            .then(a.shape.cmp(&b.shape))
    });
    shapes
}

/// `text` as one line: control characters become spaces, cut to `chars`.
fn one_line(text: &str, chars: usize) -> String {
    text.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(chars)
        .collect()
}

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
pub(super) fn new_tenure_id(repo: &str, pane_id: &str) -> String {
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

/// A new tenure token: 64 hex characters from the system's secure random
/// source.
fn new_token() -> Result<String, WorkerError> {
    crate::platform::random_hex_token().map_err(|error| {
        WorkerError::Io(std::io::Error::other(format!(
            "cannot draw a tenure token: {error}"
        )))
    })
}

/// The sha256 of `token`, as the store keeps it.
pub(super) fn token_hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Who holds `tenure`, as refusals name it.
pub(super) fn holder_description(tenure: &StoredTenure) -> String {
    if let Some(holder) = tenure.holder {
        return format!("coordinator {} held by process {}", tenure.id, holder.pid);
    }
    if tenure.headless {
        return format!(
            "headless item coordinator {} (worker {})",
            tenure.id,
            tenure.worker_id.as_deref().unwrap_or("not started yet")
        );
    }
    format!(
        "coordinator {} in pane {}",
        tenure.id,
        tenure.pane_id.as_deref().unwrap_or("an unknown pane")
    )
}

pub(super) fn info(tenure: StoredTenure) -> CoordinatorInfo {
    let holder_kind = if tenure.holder.is_some() {
        CoordinatorHolderKind::Process
    } else if tenure.headless {
        CoordinatorHolderKind::Headless
    } else {
        CoordinatorHolderKind::Pane
    };
    CoordinatorInfo {
        holder_kind: Some(holder_kind),
        holder_pid: tenure.holder.map(|holder| holder.pid),
        coordinator_id: tenure.id,
        repo: tenure.repo,
        item: tenure.item,
        started_ms: tenure.started_at,
        ended_ms: tenure.ended_at,
        end_reason: tenure.end_reason,
        epoch: tenure.epoch,
        pane_id: tenure.pane_id,
        session_id: tenure.session_id,
        headless: tenure.headless,
        worker_id: tenure.worker_id,
        handoff: None,
        handoff_persist_error: None,
    }
}

pub(super) fn store_error(error: rusqlite::Error) -> WorkerError {
    WorkerError::Io(std::io::Error::other(format!(
        "the worker store failed: {error}"
    )))
}

pub(super) fn is_unique_violation(error: &rusqlite::Error) -> bool {
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

/// A landing step's id: `s-` and 8 lowercase base32 characters, from a hash
/// of the time, this process, a counter, the step and the run.
pub(super) fn new_step_id(kind: &str, run_id: &str) -> String {
    static STEPS: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let step = STEPS.fetch_add(1, Ordering::Relaxed);
    let digest = Sha256::digest(format!(
        "{nanos}:{}:{step}:{kind}:{run_id}",
        std::process::id()
    ));
    let bits = digest[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("s-{id}")
}

/// Why a claim of `repo` was refused as it read the tenures.
enum ClaimRefusal {
    /// Another tenure holds the repository.
    Active(Box<StoredTenure>),
    /// A run claimed under no tenure is landing a step here: the claim waits
    /// for it, so that step lands under the tenure it was admitted under and
    /// never under the claimant's.
    Landing(Vec<String>),
}

/// A landing or start step admitted against its tenure
/// ([`WorkerSupervisor::begin_landing`], [`WorkerSupervisor::begin_start_step`]):
/// the check and the record of the step are one transaction, and the step holds
/// its tenure from there until this guard is dropped. Dropping it releases the
/// step and carries out the end of its tenure if one was asked for meanwhile; a
/// release that fails is recorded here and retried by the next reader on this
/// server, and a server that starts after this one retires it
/// ([`WorkerSupervisor::holders_at_start`]).
pub(super) struct LandingStep<'a> {
    supervisor: &'a WorkerSupervisor,
    id: String,
}

impl std::fmt::Debug for LandingStep<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LandingStep").field("id", &self.id).finish()
    }
}

impl Drop for LandingStep<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.supervisor.release_landing(&self.id) {
            self.supervisor.note_failed_landing_release(&self.id);
            tracing::error!(
                %error,
                step = %self.id,
                "cannot release a landing step; the next reader retries it"
            );
        }
    }
}

impl WorkerSupervisor {
    pub(super) fn tenure_store(&self) -> Result<&super::store::Store, WorkerError> {
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
        // An agent session that coordinated before keeps its tenure.
        if let Some(session_id) = session_id {
            if let Err(error) = self.coordinator_resume(pane_id, session_id, None) {
                warn!(%error, pane_id, session_id, "cannot resume the session's coordination tenure");
            }
        }
        // A reader of the tenures: it retries a step release this server could
        // not finish and retires the steps of servers that are gone, before it
        // asks whether the repository is free.
        self.read_tenures();
        let store = self.tenure_store()?;
        let id = new_tenure_id(repo, pane_id);
        let at = now_ms();
        let mut settled: Vec<StoredTenure> = Vec::new();
        let outcome = store.transaction(|tx| {
            settled.extend(self.settle_before_claim(tx, repo, at)?);
            if let Some(active) = tx.active_coordinator(repo)? {
                return Ok(Err(ClaimRefusal::Active(Box::new(active))));
            }
            let landing = tx.unowned_landing_steps(repo)?;
            if !landing.is_empty() {
                return Ok(Err(ClaimRefusal::Landing(landing)));
            }
            if let Some(bound) = tx.coordinator_of_pane(pane_id)? {
                return Ok(Err(ClaimRefusal::Active(Box::new(bound))));
            }
            let tenure = NewTenure {
                id: &id,
                repo,
                pane_id: Some(pane_id),
                session_id,
                holder: None,
            };
            tx.coordinator_started(&tenure, at).map(Ok)
        });
        for tenure in &settled {
            self.announce_tenure_end(&tenure.id);
        }
        let refused = match outcome {
            Ok(Ok(tenure)) => {
                notify_clients();
                return Ok(info(tenure));
            }
            Ok(Err(ClaimRefusal::Landing(kinds))) => {
                return Err(Self::landing_refusal(repo, &kinds))
            }
            Ok(Err(ClaimRefusal::Active(other))) => *other,
            // Another server's tenure committed between the check and the
            // insert: the index refused this one.
            Err(error) if is_unique_violation(&error) => {
                return match store.active_coordinators(Some(repo)) {
                    Ok(mut active) if !active.is_empty() => {
                        Err(self.refusal(repo, pane_id, &active.remove(0)))
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
        Err(self.refusal(repo, pane_id, &refused))
    }

    /// `coordinator.acquire` by a pane: a new token for the active tenure
    /// bound to `pane_id` (claimed with `coordinator.start`), which replaces
    /// its earlier token, so a token handed out before stops being accepted.
    /// Refused with `coordinator_not_found` while the pane holds none.
    pub(crate) fn coordinator_token_for_pane(
        &self,
        pane_id: &str,
    ) -> Result<(CoordinatorInfo, String), WorkerError> {
        let token = new_token()?;
        let hash = token_hash(&token);
        let issued = self
            .tenure_store()?
            .transaction(|tx| {
                let Some(tenure) = tx.coordinator_of_pane(pane_id)? else {
                    return Ok(None);
                };
                tx.coordinator_token(&tenure.id, &hash, now_ms()).map(Some)
            })
            .map_err(store_error)?;
        let tenure = issued.ok_or_else(|| {
            WorkerError::CoordinatorNotFound(format!(
                "pane {pane_id} holds no active coordinator tenure; claim the repository with \
                 `herdr coordinator start` first"
            ))
        })?;
        Ok((info(tenure), token))
    }

    /// `coordinator.acquire` by a process outside herdr's panes (a
    /// supervisor started from a shell): starts a tenure of `repo` held by
    /// process `pid`, recorded with its start token, and returns it with a
    /// new token. The same process asking again gets the same tenure with a
    /// new token, which replaces the earlier one. Refused with
    /// `coordinator_active`, naming the holder, while another pane or
    /// process holds the repository; with `invalid_request` when `pid` is
    /// not running; and with `worker_unsupported` where herdr cannot watch
    /// a process's exit, since only that exit event releases the tenure.
    pub(crate) fn coordinator_acquire_process(
        &self,
        repo: &str,
        pid: u32,
    ) -> Result<(CoordinatorInfo, String), WorkerError> {
        let start = crate::platform::process_start_token(pid).ok_or_else(|| {
            WorkerError::Invalid(format!(
                "process {pid} is not running, or herdr cannot read when it started"
            ))
        })?;
        // A reader of the tenures, as `coordinator.start` is: a failed step
        // release is retried and a dead server's step retired before the claim
        // asks whether the repository is free.
        self.read_tenures();
        let holder = HolderProcess { pid, start };
        let token = new_token()?;
        let hash = token_hash(&token);
        let store = self.tenure_store()?;
        let id = new_tenure_id(repo, &format!("pid:{pid}"));
        let at = now_ms();
        let mut settled: Vec<StoredTenure> = Vec::new();
        let outcome = store.transaction(|tx| {
            settled.extend(self.settle_before_claim(tx, repo, at)?);
            if let Some(active) = tx.active_coordinator(repo)? {
                if active.holder != Some(holder) {
                    return Ok(Err(ClaimRefusal::Active(Box::new(active))));
                }
                return tx.coordinator_token(&active.id, &hash, at).map(Ok);
            }
            let landing = tx.unowned_landing_steps(repo)?;
            if !landing.is_empty() {
                return Ok(Err(ClaimRefusal::Landing(landing)));
            }
            let tenure = NewTenure {
                id: &id,
                repo,
                pane_id: None,
                session_id: None,
                holder: Some(holder),
            };
            let started = tx.coordinator_started(&tenure, at)?;
            tx.coordinator_token(&started.id, &hash, at).map(Ok)
        });
        let tenure = match outcome {
            Ok(Ok(tenure)) => tenure,
            Ok(Err(ClaimRefusal::Landing(kinds))) => {
                return Err(Self::landing_refusal(repo, &kinds))
            }
            Ok(Err(ClaimRefusal::Active(other))) => return Err(self.refusal(repo, "none", &other)),
            Err(error) if is_unique_violation(&error) => {
                return match store.active_coordinators(Some(repo)) {
                    Ok(mut active) if !active.is_empty() => {
                        Err(self.refusal(repo, "none", &active.remove(0)))
                    }
                    _ => Err(WorkerError::CoordinatorActive(format!(
                        "repository {repo} already has an active coordinator"
                    ))),
                };
            }
            Err(error) => return Err(store_error(error)),
        };
        for tenure in &settled {
            self.announce_tenure_end(&tenure.id);
        }
        notify_clients();
        if let Err(error) = self.watch_holder(holder) {
            let cause = format!("herdr cannot watch holder process {pid}'s exit: {error}");
            if let Err(end_error) = self.coordinator_end(&tenure.id, ENDED, Some(&cause)) {
                warn!(%end_error, coordinator_id = %tenure.id, "cannot end the unwatched tenure");
            }
            return Err(WorkerError::Unsupported(format!(
                "{cause}; without its exit event nothing would release the tenure, so it was \
                 not kept"
            )));
        }
        // The process may have exited before its watch was in place, which
        // ended the tenure at once.
        let tenure = store
            .coordinator(&tenure.id)
            .map_err(store_error)?
            .ok_or_else(|| {
                WorkerError::CoordinatorNotFound(format!("coordinator {} not found", tenure.id))
            })?;
        if tenure.ended_at.is_some() {
            return Err(WorkerError::Invalid(format!(
                "process {pid} exited before herdr watched it; its tenure {} ended",
                tenure.id
            )));
        }
        Ok((info(tenure), token))
    }

    /// Watches `holder`'s exit, once per process for this server: its exit
    /// event ends the tenures it holds ([`Self::holder_exited`]), at once
    /// when it is gone already.
    fn watch_holder(&self, holder: HolderProcess) -> std::io::Result<()> {
        if !lock(&self.shared.holder_watches).insert(holder) {
            return Ok(());
        }
        let shared = std::sync::Arc::clone(&self.shared);
        let watched = crate::platform::watch_process_exit(
            holder.pid,
            holder.start,
            Box::new(move || {
                let supervisor = WorkerSupervisor { shared };
                // The watch goes only once the exit is recorded: an end that
                // could not be carried out is left for the next reader, not
                // forgotten with the watch ([`Self::holder_exited`]).
                supervisor.holder_exited(holder);
                lock(&supervisor.shared.holder_watches).remove(&holder);
            }),
        );
        if watched.is_err() {
            lock(&self.shared.holder_watches).remove(&holder);
        }
        watched
    }

    /// Process `holder` exited (its exit event, or found gone when this
    /// server started): every active tenure it holds ends `orphaned`. Each
    /// end is recorded before it is carried out, so one a landing step holds
    /// back stays recorded and the next reader carries it out even when this
    /// call could not finish; a store that cannot be read or written here
    /// leaves both the end and the tenure for that reader, which also
    /// re-checks the holder's process (`settle_before_claim`, a server that
    /// starts).
    fn holder_exited(&self, holder: HolderProcess) {
        let held = match self
            .tenure_store()
            .and_then(|store| store.active_coordinators(None).map_err(store_error))
        {
            Ok(active) => active,
            Err(error) => {
                warn!(%error, pid = holder.pid, "cannot read the tenures of an exited holder");
                return;
            }
        };
        let cause = format!("its holder process {} exited", holder.pid);
        for tenure in held
            .into_iter()
            .filter(|tenure| tenure.holder == Some(holder))
        {
            if let Err(error) = self.coordinator_end(&tenure.id, ORPHANED, Some(&cause)) {
                warn!(
                    %error,
                    coordinator_id = %tenure.id,
                    "cannot end the exited holder's tenure; the next reader ends it"
                );
            }
        }
    }

    /// What a claim attempt reads before it asks for a repository: the ends
    /// herdr saw and has not carried out yet are carried out now, and a
    /// tenure whose recorded holder process is gone ends even when no exit
    /// event reached herdr (a watch it could not place, a kernel event it
    /// lost, or an end whose store write failed). Returns the tenures that
    /// ended, which the caller announces after its commit.
    fn settle_before_claim(
        &self,
        tx: &Tx<'_>,
        repo: &str,
        at_ms: u64,
    ) -> super::store::StoreResult<Vec<StoredTenure>> {
        let active = tx.active_coordinator(repo)?;
        if let Some(holder) = active.as_ref().and_then(|active| active.holder) {
            // The start token tells the holder's process from a later one that
            // reused its pid; a process herdr cannot inspect counts as gone,
            // as it does when a watch cannot be placed.
            if crate::platform::process_start_token(holder.pid) != Some(holder.start) {
                if let Some(active) = active {
                    tx.request_tenure_end(&super::store::PendingEnd {
                        coordinator_id: active.id,
                        epoch: active.epoch,
                        reason: ORPHANED.to_owned(),
                        cause: Some(format!("its holder process {} exited", holder.pid)),
                        requested_ms: at_ms,
                    })?;
                }
            }
        }
        tx.settle_pending_ends(at_ms)
    }

    /// A reader of the tenures, called before a claim (or `coordinator end`)
    /// asks for a repository: it retries a step release this server could not
    /// finish, and retires the steps of servers that are gone (with the end
    /// each held back). Neither runs on a timer.
    fn read_tenures(&self) {
        self.retry_failed_landing_releases();
        self.retire_dead_tenure_steps();
    }

    /// Whether the server that admitted a step is gone. Only its pid and start
    /// token identify it: one recorded without a start token can never be
    /// proven gone, so a pid alone never retires a step.
    fn step_server_gone(pid: u32, start: Option<u64>) -> bool {
        let Some(recorded) = start else {
            return false;
        };
        crate::platform::process_start_token(pid) != Some(recorded)
    }

    /// Retires the landing steps of servers that are gone, recording that this
    /// server retired them, and carries out the ends they held back. A step of
    /// a live server is left to its release, or to the next server when that
    /// release was lost with the process.
    fn retire_dead_tenure_steps(&self) {
        let store = match self.tenure_store() {
            Ok(store) => store,
            Err(error) => {
                warn!(%error, "cannot read the tenures' landing steps");
                return;
            }
        };
        let gone: Vec<String> = match store.tenure_step_servers() {
            Ok(steps) => steps
                .into_iter()
                .filter(|(_, pid, start)| Self::step_server_gone(*pid, *start))
                .map(|(id, _, _)| id)
                .collect(),
            Err(error) => {
                warn!(%error, "cannot read the tenures' landing steps");
                return;
            }
        };
        if gone.is_empty() {
            return;
        }
        let pid = std::process::id();
        let retired_by = (pid, crate::platform::process_start_token(pid));
        match store.retire_tenure_steps(&gone, now_ms(), retired_by) {
            Ok(ended) => {
                for tenure in &ended {
                    self.announce_tenure_end(&tenure.id);
                }
            }
            Err(error) => {
                warn!(%error, "cannot retire the landing steps of the servers that are gone")
            }
        }
    }

    /// Records a step whose release failed, so the next reader on this server
    /// retries it. The step stays open meanwhile, so the tenure it holds does
    /// not end under the work the step guards.
    fn note_failed_landing_release(&self, id: &str) {
        lock(&self.shared.failed_step_releases).insert(id.to_owned());
    }

    /// Retries the releases of steps recorded by [`Self::note_failed_landing_release`].
    fn retry_failed_landing_releases(&self) {
        let ids: Vec<String> = lock(&self.shared.failed_step_releases)
            .iter()
            .cloned()
            .collect();
        if ids.is_empty() {
            return;
        }
        let store = match self.tenure_store() {
            Ok(store) => store,
            Err(error) => {
                tracing::error!(%error, "cannot retry the landing steps' releases");
                return;
            }
        };
        for id in ids {
            match store.finish_tenure_step(&id, now_ms()) {
                Ok(ended) => {
                    lock(&self.shared.failed_step_releases).remove(&id);
                    for tenure in &ended {
                        self.announce_tenure_end(&tenure.id);
                    }
                }
                Err(error) => tracing::error!(
                    %error,
                    step = %id,
                    "cannot release a landing step; the next reader retries it"
                ),
            }
        }
    }

    /// Reconciles the tenures' landing steps and pending ends when this
    /// server starts (also after a live handoff, whose new server starts
    /// here): a step of a server that is gone is retired and the end it held
    /// back is carried out. Then the process holders: a recorded holder that
    /// is gone, or whose pid names a later process, releases its tenure now;
    /// a live one is watched for its exit event. A holder whose exit cannot
    /// be watched releases its tenure too.
    pub(super) fn holders_at_start(&self) {
        self.read_tenures();
        let store = match self.tenure_store() {
            Ok(store) => store,
            Err(error) => {
                warn!(%error, "cannot read the tenures' holder processes");
                return;
            }
        };
        let active = match store.active_coordinators(None) {
            Ok(active) => active,
            Err(error) => {
                warn!(%error, "cannot read the tenures' holder processes");
                return;
            }
        };
        for tenure in active {
            let Some(holder) = tenure.holder else {
                continue;
            };
            if let Err(error) = self.watch_holder(holder) {
                let cause = format!(
                    "herdr cannot watch holder process {}'s exit (found when herdr started): \
                     {error}",
                    holder.pid
                );
                if let Err(error) = self.coordinator_end(&tenure.id, ORPHANED, Some(&cause)) {
                    warn!(%error, coordinator_id = %tenure.id, "cannot end the unwatched tenure");
                }
            }
        }
    }

    /// Refuses a start in `repo` (`worker.run`) with `tenure_token_refused`
    /// while the repository's tenure is active and `token` is not its
    /// current one: none given, another tenure's, or one its holder was
    /// given before acquiring again. With no active tenure nothing is
    /// checked.
    ///
    /// This is the precheck, before the start reads the provider's usage: the
    /// authoritative check runs in the transaction that records the start's
    /// usage admission ([`super::store::tenure_token_check`]), so a tenure
    /// change cannot fall between the check and the start it admits.
    pub(crate) fn check_tenure_token(
        &self,
        repo: &str,
        token: Option<&str>,
    ) -> Result<(), WorkerError> {
        match self
            .tenure_store()?
            .tenure_token_check(repo, token)
            .map_err(store_error)?
        {
            Ok(_) => Ok(()),
            Err(why) => Err(WorkerError::TenureToken(why)),
        }
    }

    /// Admits the landing step `kind` of run `run_id` in `repo`, claimed under
    /// tenure `owner` (none for a run claimed outside any tenure), and holds
    /// the tenure until the returned guard is dropped: one transaction checks
    /// the fence ([`super::store::tenure_fence_check`]) and records the step,
    /// so a tenure change cannot fall between the check and the step. While
    /// the step is in flight its tenure does not end — `coordinator_end`
    /// records the end and carries it out once the step has landed, and a
    /// successor can claim the repository only then. The refusal names the
    /// current holder; a store that cannot say refuses too.
    pub(super) fn begin_landing(
        &self,
        repo: &str,
        owner: Option<&str>,
        kind: &str,
        run_id: &str,
    ) -> Result<LandingStep<'_>, String> {
        let unreadable =
            |error: WorkerError| format!("cannot read repository {repo}'s tenure: {error}");
        let store = self.tenure_store().map_err(unreadable)?;
        let id = new_step_id(kind, run_id);
        let at = now_ms();
        let pid = std::process::id();
        let admitted = store
            .transaction(|tx| {
                match tx.tenure_fence(repo, owner)? {
                    Ok(()) => {}
                    Err(why) => return Ok(Err(why)),
                }
                let coordinator = match owner {
                    Some(owner) => tx.coordinator(owner)?.map(|tenure| (owner, tenure.epoch)),
                    None => None,
                };
                tx.tenure_step(&super::store::NewTenureStep {
                    id: &id,
                    repo,
                    coordinator,
                    kind,
                    run_id,
                    server: (pid, crate::platform::process_start_token(pid)),
                    at_ms: at,
                })?;
                Ok(Ok(()))
            })
            .map_err(|error| unreadable(store_error(error)))?;
        admitted?;
        Ok(LandingStep {
            supervisor: self,
            id,
        })
    }

    /// Admits the start of the `worker.run` whose usage admission is `holder`'s
    /// and holds its tenure through the spawn: one transaction re-checks the
    /// admission's tenure and its token ([`super::store::Tx::admission_tenure`],
    /// [`super::store::Tx::tenure_token_check`]) and records the step, so an
    /// end, a successor's claim or a token rotated after the admission starts
    /// nothing, and one that comes after this transaction waits for the guard.
    /// The caller releases the admission on a refusal.
    pub(super) fn begin_start_step(
        &self,
        repo: &str,
        holder: &str,
        token: Option<&str>,
    ) -> Result<LandingStep<'_>, WorkerError> {
        let store = self.tenure_store()?;
        let id = new_step_id("start", holder);
        let at = now_ms();
        let pid = std::process::id();
        let admitted = store
            .transaction(|tx| {
                let Some(recorded) = tx.admission_tenure(holder)? else {
                    return Ok(Err(
                        "its usage admission ended before its worker started; nothing started"
                            .to_owned(),
                    ));
                };
                let current = match tx.tenure_token_check(repo, token)? {
                    Ok(current) => current,
                    Err(why) => return Ok(Err(why)),
                };
                if current != recorded {
                    return Ok(Err(format!(
                    "repository {repo}'s coordination tenure changed between the run's admission \
                     and its worker's start; nothing started"
                )));
                }
                tx.tenure_step(&super::store::NewTenureStep {
                    id: &id,
                    repo,
                    coordinator: recorded
                        .as_ref()
                        .map(|(coordinator_id, epoch)| (coordinator_id.as_str(), *epoch)),
                    kind: "start",
                    run_id: holder,
                    server: (pid, crate::platform::process_start_token(pid)),
                    at_ms: at,
                })?;
                Ok(Ok(()))
            })
            .map_err(store_error)?;
        admitted.map_err(WorkerError::TenureToken)?;
        Ok(LandingStep {
            supervisor: self,
            id,
        })
    }

    /// Releases landing step `id` and carries out the end it held back
    /// ([`super::store::Store::finish_tenure_step`]).
    fn release_landing(&self, id: &str) -> Result<(), WorkerError> {
        #[cfg(test)]
        if std::mem::take(&mut *lock(&self.shared.fail_landing_release)) {
            return Err(WorkerError::Io(std::io::Error::other(
                "the step release failed (test hook)",
            )));
        }
        let ended = self
            .tenure_store()?
            .finish_tenure_step(id, now_ms())
            .map_err(store_error)?;
        for tenure in &ended {
            self.announce_tenure_end(&tenure.id);
        }
        Ok(())
    }

    /// Makes the next landing-step release fail without writing it, so a test
    /// can check that the next reader retries it.
    #[cfg(test)]
    pub(in crate::workers) fn fail_next_landing_release_for_test(&self) {
        *lock(&self.shared.fail_landing_release) = true;
    }

    /// Wakes what waits on tenure `id`'s end: a `worker.events` wait on the
    /// tenure, which answers once it has ended, through the registry lock it
    /// holds from its check to its block, so the wake cannot fall in between;
    /// and the clients. A tenure that ends has nothing left to hand over.
    fn announce_tenure_end(&self, id: &str) {
        lock(&self.shared.registry).handoffs.remove(id);
        drop(lock(&self.shared.registry));
        self.shared.changed.notify_all();
        notify_clients();
    }

    /// The refusal for a repository another holder coordinates, naming it (and
    /// the end herdr already saw, when a landing step still holds it back).
    pub(super) fn refusal(&self, repo: &str, pane_id: &str, other: &StoredTenure) -> WorkerError {
        let refusal = Self::refusal_message(repo, pane_id, other);
        let Some(note) = self.pending_end_note(&other.id, other.epoch) else {
            return refusal;
        };
        match refusal {
            WorkerError::CoordinatorActive(message) => {
                WorkerError::CoordinatorActive(format!("{message} ({note})"))
            }
            refusal => refusal,
        }
    }

    /// The refusal for a repository whose run, claimed under no tenure, is
    /// landing a step: a claim waits for it to land, so the step stays under
    /// the tenure it was admitted under (none) and never lands for a holder
    /// that claimed the repository meanwhile.
    fn landing_refusal(repo: &str, kinds: &[String]) -> WorkerError {
        WorkerError::Busy(format!(
            "repository {repo} has a run landing its {} step, claimed under no coordinator \
             tenure; a claim waits for it to land, so run it again then",
            kinds.join(", ")
        ))
    }

    /// Why a tenure herdr saw end has not ended yet, when a landing step of
    /// its run still holds it back: the refusal of a successor's claim says it,
    /// so a waiting holder is not mistaken for a stranded one.
    fn pending_end_note(&self, id: &str, epoch: i64) -> Option<String> {
        let end = self
            .tenure_store()
            .ok()
            .and_then(|store| store.pending_end(id, epoch).ok().flatten())?;
        Some(format!(
            "herdr saw it end ({}), and the end waits for its landing step to land",
            end.cause.unwrap_or(end.reason)
        ))
    }

    pub(super) fn refusal_message(repo: &str, pane_id: &str, other: &StoredTenure) -> WorkerError {
        if other.holder.is_some() {
            return WorkerError::CoordinatorActive(format!(
                "repository {} already has {}; it ends when that process exits, or with \
                 `herdr coordinator end --id {}`",
                other.repo,
                holder_description(other),
                other.id
            ));
        }
        if other.headless {
            return WorkerError::CoordinatorActive(format!(
                "repository {} already has headless item coordinator {} (worker {}) on item {}; \
                 it ends with its item, or stop its worker",
                other.repo,
                other.id,
                other.worker_id.as_deref().unwrap_or("not started yet"),
                other.item.as_deref().unwrap_or("none")
            ));
        }
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

    /// Ends tenure `id` with `reason` and `cause` (what herdr saw); a tenure
    /// that has ended already is returned as it is.
    ///
    /// The end is recorded (`tenure_pending_ends`) and carried out in the
    /// same transaction, unless a landing step of the tenure is in flight:
    /// the step holds its tenure until it lands, so the end waits for it and
    /// the returned tenure is not ended yet (the CLI says so). Nothing is
    /// lost when the store cannot write: the record goes first, and the next
    /// reader carries it out.
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
        let at = now_ms();
        let store = self.tenure_store()?;
        let Some(tenure) = store.coordinator(id).map_err(store_error)? else {
            return Err(WorkerError::CoordinatorNotFound(format!(
                "coordinator {id} not found"
            )));
        };
        // A reader of the tenures: a failed step release is retried and a dead
        // server's step retired before this end asks to be recorded.
        self.read_tenures();
        let request = super::store::PendingEnd {
            coordinator_id: id.to_owned(),
            epoch: tenure.epoch,
            reason: reason.to_owned(),
            cause: cause.map(str::to_owned),
            requested_ms: at,
        };
        // Two transactions, in this order: the end is durable before it is
        // attempted, so a failure here is left for the next reader instead of
        // vanishing with the call.
        store
            .transaction(|tx| tx.request_tenure_end(&request))
            .map_err(store_error)?;
        let ended = store.settle_pending_ends(at).map_err(store_error)?;
        for tenure in &ended {
            self.announce_tenure_end(&tenure.id);
        }
        let tenure = store.coordinator(id).map_err(store_error)?.ok_or_else(|| {
            WorkerError::CoordinatorNotFound(format!("coordinator {id} not found"))
        })?;
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
            .map(|tenure| self.with_handoff(info(tenure)))
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

    /// Binds the tenure agent session `session_id` coordinated to `pane_id`,
    /// where that session runs now (resumed there, or found there by
    /// `coordinator.start`): the tenure stays, a new binding starts, an
    /// orphaned tenure is active again, and the workers and runs it owns
    /// move to the pane in the same transaction (`owner_moved`,
    /// `run_owner_moved`). Nothing when the session coordinated nothing
    /// that can come back, when the tenure is bound to that pane already,
    /// when the pane is bound to another tenure, or when an orphaned
    /// tenure's repository has another coordinator now. `workspace` is the
    /// pane's, which lists the runs' workers. Returns the moved tenure.
    pub(crate) fn coordinator_resume(
        &self,
        pane_id: &str,
        session_id: &str,
        workspace: Option<&str>,
    ) -> Result<Option<CoordinatorInfo>, WorkerError> {
        let store = self.tenure_store()?;
        let mut registry = lock(&self.shared.registry);
        let at = now_ms();
        let outcome = store
            .transaction(|tx| {
                let Some(tenure) = tx.resumable_coordinator(session_id)? else {
                    return Ok(None);
                };
                let active = tenure.ended_at.is_none();
                if active && tenure.pane_id.as_deref() == Some(pane_id) {
                    return Ok(None);
                }
                if tx
                    .coordinator_of_pane(pane_id)?
                    .is_some_and(|bound| bound.id != tenure.id)
                {
                    return Ok(None);
                }
                if !active && tx.active_coordinator(&tenure.repo)?.is_some() {
                    return Ok(None);
                }
                let resumed = tx.coordinator_resumed(&tenure.id, pane_id, Some(session_id), at)?;
                let cause = format!(
                    "its coordinator {} resumed in pane {pane_id} (agent session {session_id})",
                    tenure.id
                );
                let owner = RunOwner {
                    pane_id: Some(pane_id),
                    session_id: Some(session_id),
                    workspace,
                    coordinator_id: Some(&tenure.id),
                };
                let (moved, staged) =
                    Self::move_owned(&registry, tx, &tenure.id, &owner, &cause, at)?;
                Ok(Some((resumed, moved, staged)))
            })
            .map_err(store_error)?;
        let Some((resumed, moved, staged)) = outcome else {
            return Ok(None);
        };
        Self::settle_moves(&mut registry, staged, &moved, at);
        drop(registry);
        self.shared.changed.notify_all();
        notify_clients();
        Ok(Some(info(resumed)))
    }

    /// The agent in pane `pane_id` cleared its session (`/clear`): from
    /// `from_session`, the one the pane had, into `to_session`. The active
    /// tenure bound to the pane with `from_session` (or with no session yet)
    /// moves its binding to `to_session` in one transaction, so a later
    /// resume of `to_session` finds it; the workers and runs it owns stay
    /// its own and follow the new session (`owner_moved`,
    /// `run_owner_moved`). Nothing when the pane is bound to no tenure or
    /// the binding has another session. Returns the moved tenure.
    pub(crate) fn coordinator_session_cleared(
        &self,
        pane_id: &str,
        from_session: Option<&str>,
        to_session: &str,
        workspace: Option<&str>,
    ) -> Result<Option<CoordinatorInfo>, WorkerError> {
        let store = self.tenure_store()?;
        let mut registry = lock(&self.shared.registry);
        let at = now_ms();
        let outcome = store
            .transaction(|tx| {
                let Some(tenure) = tx.coordinator_of_pane(pane_id)? else {
                    return Ok(None);
                };
                let bound = tenure.session_id.as_deref();
                if bound == Some(to_session) || bound.is_some_and(|bound| Some(bound) != from_session)
                {
                    return Ok(None);
                }
                let cleared =
                    tx.coordinator_session_cleared(&tenure.id, pane_id, bound, to_session, at)?;
                let cause = format!(
                    "its coordinator {} cleared its agent session into {to_session} in pane {pane_id}",
                    tenure.id
                );
                let owner = RunOwner {
                    pane_id: Some(pane_id),
                    session_id: Some(to_session),
                    workspace,
                    coordinator_id: Some(&tenure.id),
                };
                let (moved, staged) =
                    Self::move_owned(&registry, tx, &tenure.id, &owner, &cause, at)?;
                Ok(Some((cleared, moved, staged)))
            })
            .map_err(store_error)?;
        let Some((cleared, moved, staged)) = outcome else {
            return Ok(None);
        };
        Self::settle_moves(&mut registry, staged, &moved, at);
        drop(registry);
        self.shared.changed.notify_all();
        notify_clients();
        Ok(Some(info(cleared)))
    }

    /// Hands the coordination over (`coordinator.handoff`): active tenure
    /// `coordinator_id`, else the one bound to `from_pane`, ends
    /// `handed_off`, and a new tenure of its repository starts bound to
    /// `to_pane` and its agent session `to_session`, with the next epoch and
    /// the old tenure's current item; the workers and runs the old tenure
    /// owns move to it. All in one transaction, recorded as `handoff`.
    /// Only this explicit call hands off, never a timer. Refused with
    /// `coordinator_not_found` without an active tenure, with
    /// `invalid_request` for the tenure's own pane and with
    /// `coordinator_active` when `to_pane` coordinates already.
    pub(crate) fn coordinator_handoff(
        &self,
        coordinator_id: Option<&str>,
        from_pane: Option<&str>,
        to_pane: &str,
        to_session: Option<&str>,
        to_workspace: Option<&str>,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let mut registry = lock(&self.shared.registry);
        let started = self.coordinator_handoff_locked(
            &mut registry,
            coordinator_id,
            from_pane,
            to_pane,
            to_session,
            to_workspace,
        );
        drop(registry);
        self.shared.changed.notify_all();
        notify_clients();
        started
    }

    /// [`Self::coordinator_handoff`] under the registry lock the caller
    /// holds, so a caller that must change more in the same step (the ack of
    /// a self-service handoff) leaves no window between them. The caller
    /// wakes waiters and clients once it let go of the lock.
    pub(super) fn coordinator_handoff_locked(
        &self,
        registry: &mut Registry,
        coordinator_id: Option<&str>,
        from_pane: Option<&str>,
        to_pane: &str,
        to_session: Option<&str>,
        to_workspace: Option<&str>,
    ) -> Result<CoordinatorInfo, WorkerError> {
        let store = self.tenure_store()?;
        let at = now_ms();
        let outcome = store
            .transaction(|tx| {
                let from = match (coordinator_id, from_pane) {
                    (Some(id), _) => tx.coordinator(id)?,
                    (None, Some(pane)) => tx.coordinator_of_pane(pane)?,
                    (None, None) => None,
                };
                let Some(from) = from.filter(|tenure| tenure.ended_at.is_none()) else {
                    let named = coordinator_id
                        .map(|id| format!("coordinator {id} is not active"))
                        .or_else(|| {
                            from_pane.map(|pane| {
                                format!("pane {pane} is bound to no active coordinator")
                            })
                        })
                        .unwrap_or_else(|| {
                            "coordinator.handoff needs coordinator_id or pane_id".into()
                        });
                    return Ok(Err(WorkerError::CoordinatorNotFound(named)));
                };
                if from.pane_id.as_deref() == Some(to_pane) {
                    return Ok(Err(WorkerError::Invalid(format!(
                        "coordinator {} is bound to pane {to_pane} already",
                        from.id
                    ))));
                }
                if let Some(other) = tx.coordinator_of_pane(to_pane)? {
                    return Ok(Err(Self::refusal_message(&from.repo, to_pane, &other)));
                }
                // The run a landing step carries must land under this tenure,
                // not under its successor's: the handoff waits for it.
                let in_flight = tx.open_tenure_steps(&from.id, from.epoch)?;
                if !in_flight.is_empty() {
                    return Ok(Err(WorkerError::Busy(format!(
                        "coordinator {} has a landing step in flight ({}); the handoff waits for it \
                         to land, so run it again then",
                        from.id,
                        in_flight.join(", ")
                    ))));
                }
                let id = new_tenure_id(&from.repo, to_pane);
                let next = NewTenure {
                    id: &id,
                    repo: &from.repo,
                    pane_id: Some(to_pane),
                    session_id: to_session,
                    holder: None,
                };
                let (_, started) = tx.coordinator_handoff(&from, &next, at)?;
                let cause = format!(
                    "its coordinator {} handed off to {} in pane {to_pane}",
                    from.id, started.id
                );
                let owner = RunOwner {
                    pane_id: Some(to_pane),
                    session_id: to_session,
                    workspace: to_workspace,
                    coordinator_id: Some(&started.id),
                };
                let (moved, staged) = Self::move_owned(registry, tx, &from.id, &owner, &cause, at)?;
                Ok(Ok((started, moved, staged)))
            })
            .map_err(store_error)?;
        let (started, moved, staged) = outcome?;
        Self::settle_moves(registry, staged, &moved, at);
        Ok(info(started))
    }

    /// Moves what tenure `from` owns to `owner` inside `tx`: its open runs,
    /// and its workers on copies of their statuses, returned with the
    /// `owner_moved` event for [`Self::settle_moves`].
    fn move_owned(
        registry: &Registry,
        tx: &Tx<'_>,
        from: &str,
        owner: &RunOwner<'_>,
        cause: &str,
        at: u64,
    ) -> rusqlite::Result<(Value, StagedMoves)> {
        let moved = json!({
            "type": "owner_moved",
            "pane_id": owner.pane_id,
            "session_id": owner.session_id,
            "coordinator_id": owner.coordinator_id,
            "from_coordinator_id": from,
            "cause": cause,
        });
        let staged = Self::stage_owner_moves(registry, tx, from, &moved, at)?;
        for run in tx.open_runs_of_coordinator(from)? {
            tx.move_run_owner(&run.info.run_id, owner, cause, at)?;
        }
        Ok((moved, staged))
    }

    /// What to do with one call the allowlist refused in pane `pane_id`
    /// (its public id), in `mode`: in shadow mode the call is stored as a
    /// would-deny, with the pane's active tenure, its repository and item,
    /// else the repository of `params.cwd`. Shadow never refuses: the input
    /// is cut to fit, and a store that fails gives no record (logged).
    pub(crate) fn allowlist_refusal(
        &self,
        pane_id: &str,
        params: &CoordinatorAllowlistRefusalParams,
        mode: CoordinatorAllowlistMode,
    ) -> Option<CoordinatorWouldDeny> {
        if mode != CoordinatorAllowlistMode::Shadow {
            return None;
        }
        let tool = match one_line(&params.tool, 100) {
            tool if tool.is_empty() => "unknown".to_owned(),
            tool => tool,
        };
        let tenure = self.coordinator_of_pane(pane_id);
        let repo = match &tenure {
            Some(tenure) => Some(tenure.repo.clone()),
            None => params.cwd.as_deref().and_then(super::repository_of_dir),
        };
        let record = CoordinatorWouldDeny {
            id: 0,
            ts_ms: now_ms(),
            pane_id: pane_id.to_owned(),
            tool,
            command: params
                .command
                .chars()
                .take(OVERRIDE_COMMAND_CHARS)
                .collect(),
            reason: one_line(&params.reason, WOULD_DENY_REASON_CHARS),
            repo,
            coordinator_id: tenure.as_ref().map(|tenure| tenure.coordinator_id.clone()),
            item: tenure.and_then(|tenure| tenure.item),
            session_id: params.session_id.clone(),
        };
        let stored = self
            .tenure_store()
            .and_then(|store| store.record_would_deny(&record).map_err(store_error));
        match stored {
            Ok(record) => Some(record),
            Err(error) => {
                warn!(%error, pane = pane_id, "a coordinator would-deny was not recorded");
                None
            }
        }
    }

    /// The calls the allowlist would have denied, oldest first, and their
    /// count per command shape: of the repository of `repo` (a directory
    /// in it) when given.
    pub(crate) fn would_deny(
        &self,
        repo: Option<&str>,
    ) -> Result<(Vec<CoordinatorWouldDeny>, Vec<CoordinatorWouldDenyShape>), WorkerError> {
        let repo = repo.map(|dir| super::repository_of_dir(dir).unwrap_or_else(|| dir.to_owned()));
        let records = self
            .tenure_store()?
            .would_deny(repo.as_deref())
            .map_err(store_error)?;
        let shapes = would_deny_shapes(&records);
        Ok((records, shapes))
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

    #[cfg(unix)]
    #[test]
    fn the_allowlist_mode_is_published_beside_the_socket_and_replaced_whole() {
        let root = std::env::temp_dir().join(format!(
            "herdr-allowlist-mode-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("herdr.sock");
        let path = allowlist_mode_path(&socket);
        assert_eq!(path, root.join("herdr.sock.allowlist-mode"));
        for (mode, text) in [
            (CoordinatorAllowlistMode::Enforce, "enforce\n"),
            (CoordinatorAllowlistMode::Shadow, "shadow\n"),
            (CoordinatorAllowlistMode::Off, "off\n"),
        ] {
            write_allowlist_mode(&path, mode).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        // No temporary file is left beside it.
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            names,
            [std::ffi::OsString::from("herdr.sock.allowlist-mode")]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

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

    fn refusal_params(
        tool: &str,
        command: &str,
        reason: &str,
    ) -> CoordinatorAllowlistRefusalParams {
        CoordinatorAllowlistRefusalParams {
            pane_id: "p1".into(),
            tool: tool.into(),
            command: command.into(),
            reason: reason.into(),
            cwd: None,
            session_id: Some("s-1".into()),
        }
    }

    #[test]
    fn only_shadow_mode_records_a_would_deny_with_the_panes_tenure() {
        let (root, supervisor) = scratch("would-deny");
        let tenure = supervisor
            .coordinator_start("/repo", "p1", Some("s-1"))
            .unwrap();
        for mode in [
            CoordinatorAllowlistMode::Enforce,
            CoordinatorAllowlistMode::Off,
        ] {
            let params = refusal_params("Bash", "cargo build", "`cargo` is not allowed");
            assert_eq!(supervisor.allowlist_refusal("p1", &params, mode), None);
        }
        assert_eq!(
            supervisor.would_deny(None).unwrap(),
            (Vec::new(), Vec::new())
        );

        let shadow = CoordinatorAllowlistMode::Shadow;
        let first = supervisor
            .allowlist_refusal(
                "p1",
                &refusal_params("Bash", "cargo build --release", " `cargo`\nis not allowed "),
                shadow,
            )
            .unwrap();
        assert_eq!(first.reason, "`cargo` is not allowed");
        assert_eq!(first.repo.as_deref(), Some("/repo"));
        assert_eq!(
            first.coordinator_id.as_deref(),
            Some(tenure.coordinator_id.as_str())
        );
        assert_eq!(first.session_id.as_deref(), Some("s-1"));
        // Shadow never refuses: an empty tool or reason and a long command
        // are stored as they fit.
        let long = format!("cargo test {}", "x".repeat(OVERRIDE_COMMAND_CHARS));
        let mut params = refusal_params("", &long, "");
        params.cwd = Some(root.display().to_string());
        let second = supervisor.allowlist_refusal("p9", &params, shadow).unwrap();
        assert_eq!(
            (second.tool.as_str(), second.reason.as_str()),
            ("unknown", "")
        );
        assert_eq!(second.coordinator_id, None);
        assert_eq!(second.command.chars().count(), OVERRIDE_COMMAND_CHARS);
        let third = supervisor
            .allowlist_refusal(
                "p1",
                &refusal_params("Bash", "cd /repo && cargo build", "later"),
                shadow,
            )
            .unwrap();

        let (records, shapes) = supervisor.would_deny(None).unwrap();
        assert_eq!(records, vec![first.clone(), second.clone(), third.clone()]);
        let counted: Vec<(&str, u64, &str)> = shapes
            .iter()
            .map(|shape| (shape.shape.as_str(), shape.count, shape.reason.as_str()))
            .collect();
        assert_eq!(
            counted,
            vec![("Bash: cargo build", 2, "later"), ("unknown", 1, "")]
        );
        assert_eq!(
            supervisor.would_deny(Some("/repo")).unwrap().0,
            vec![first, third]
        );
        // The overrides are a list of their own.
        assert!(supervisor.overrides(None).unwrap().is_empty());
        let store = supervisor.tenure_store().unwrap();
        for change in [
            "UPDATE coordinator_would_deny SET reason = 'y'",
            "DELETE FROM coordinator_would_deny",
        ] {
            assert!(store.connection().execute(change, []).is_err(), "{change}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_would_deny_is_counted_under_its_tool_program_and_first_argument() {
        for (tool, command, shape) in [
            ("Bash", "cargo build --release", "Bash: cargo build"),
            ("Bash", "cargo  --locked build", "Bash: cargo build"),
            ("Bash", "cd /repo && just check 2>&1", "Bash: just check"),
            (
                "Bash",
                "FOO=1 env BAR=2 /usr/bin/git push origin",
                "Bash: git push",
            ),
            ("Bash", "rm -rf target", "Bash: rm target"),
            ("Bash", "make # herdr-override: why", "Bash: make"),
            ("Bash", "\"scripts/x.sh\" 'a b'", "Bash: x.sh a"),
            ("Bash", "cd /repo", "Bash"),
            ("Bash", "", "Bash"),
            ("Write", "/repo/src/main.rs", "Write"),
            ("Agent", "{}", "Agent"),
        ] {
            assert_eq!(command_shape(tool, command), shape, "{tool} {command:?}");
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

    /// A process that runs until its input closes.
    #[cfg(unix)]
    fn holder_process() -> std::process::Child {
        std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Tenure `id` once it has ended: woken by `coordinator_end`'s notify,
    /// which comes after its commit, so the check under the registry lock
    /// cannot miss it.
    #[cfg(unix)]
    fn wait_ended(supervisor: &WorkerSupervisor, id: &str) -> StoredTenure {
        let store = supervisor.tenure_store().unwrap();
        let mut registry = lock(&supervisor.shared.registry);
        loop {
            let tenure = store.coordinator(id).unwrap().unwrap();
            if tenure.ended_at.is_some() {
                return tenure;
            }
            registry = supervisor
                .shared
                .changed
                .wait(registry)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    fn ended_cause(supervisor: &WorkerSupervisor, id: &str) -> String {
        supervisor
            .tenure_store()
            .unwrap()
            .connection()
            .query_row(
                "SELECT body FROM events WHERE worker_id = ?1 AND type = 'coordinator_ended'",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_process_holds_the_repository_until_its_exit_event() {
        let (root, supervisor) = scratch("process-holder");
        let mut holder = holder_process();
        let pid = holder.id();
        let (tenure, token) = supervisor
            .coordinator_acquire_process("/repo", pid)
            .unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(
            (
                tenure.holder_kind,
                tenure.holder_pid,
                tenure.pane_id.as_deref()
            ),
            (Some(CoordinatorHolderKind::Process), Some(pid), None)
        );
        assert!(!tenure.headless);
        // The store keeps the token's hash, never the token.
        let stored = supervisor
            .tenure_store()
            .unwrap()
            .coordinator(&tenure.coordinator_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.token_sha256.as_deref(),
            Some(token_hash(&token).as_str())
        );
        let events: Vec<String> = supervisor
            .tenure_store()
            .unwrap()
            .connection()
            .prepare("SELECT body FROM events WHERE worker_id = ?1")
            .unwrap()
            .query_map([&tenure.coordinator_id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            events.iter().all(|body| !body.contains(&token)
                && !body.contains(stored.token_sha256.as_deref().unwrap())),
            "{events:?}"
        );
        // Status names the holder process, never the token.
        let status = supervisor.coordinator_status(Some("/repo")).unwrap();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].holder_pid, Some(pid));
        assert!(!serde_json::to_string(&status).unwrap().contains(&token));

        // The same process acquiring again keeps the tenure, with a new token.
        let (again, replaced) = supervisor
            .coordinator_acquire_process("/repo", pid)
            .unwrap();
        assert_eq!(again.coordinator_id, tenure.coordinator_id);
        assert_ne!(replaced, token);
        // Another process, and a pane, are refused, named with the holder.
        let mut other = holder_process();
        let refused = supervisor
            .coordinator_acquire_process("/repo", other.id())
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active");
        let message = refused.to_string();
        assert!(
            message.contains(&tenure.coordinator_id) && message.contains(&format!("process {pid}")),
            "{message}"
        );
        let refused = supervisor
            .coordinator_start("/repo", "p1", None)
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active");
        assert!(
            refused.to_string().contains(&format!("process {pid}")),
            "{refused}"
        );
        // A pane that holds nothing gets no token.
        assert_eq!(
            supervisor
                .coordinator_token_for_pane("p1")
                .unwrap_err()
                .code(),
            "coordinator_not_found"
        );
        // A process that is not running holds nothing.
        drop(other.stdin.take());
        let other_pid = other.id();
        other.wait().unwrap();
        assert_eq!(
            supervisor
                .coordinator_acquire_process("/elsewhere", other_pid)
                .unwrap_err()
                .code(),
            "invalid_request"
        );

        // Its exit event releases the tenure.
        drop(holder.stdin.take());
        let ended = wait_ended(&supervisor, &tenure.coordinator_id);
        holder.wait().unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        let cause = ended_cause(&supervisor, &tenure.coordinator_id);
        assert!(
            cause.contains(&format!("holder process {pid} exited")),
            "{cause}"
        );
        // The repository is free again.
        supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_starting_server_releases_a_gone_holder_and_watches_a_live_one() {
        let (root, supervisor) = scratch("holder-restart");
        let mut gone = holder_process();
        let gone_holder = HolderProcess {
            pid: gone.id(),
            start: crate::platform::process_start_token(gone.id()).unwrap(),
        };
        let mut alive = holder_process();
        let alive_holder = HolderProcess {
            pid: alive.id(),
            start: crate::platform::process_start_token(alive.id()).unwrap(),
        };
        // Recorded by a previous server, which watched them until it ended.
        let store = supervisor.tenure_store().unwrap();
        for (id, repo, holder) in [
            ("c-gonehold", "/repo", gone_holder),
            ("c-livehold", "/other", alive_holder),
        ] {
            store
                .transaction(|tx| {
                    let tenure = NewTenure {
                        id,
                        repo,
                        pane_id: None,
                        session_id: None,
                        holder: Some(holder),
                    };
                    tx.coordinator_started(&tenure, now_ms())
                })
                .unwrap();
        }
        drop(supervisor);
        // One holder exits while no server runs.
        drop(gone.stdin.take());
        gone.wait().unwrap();

        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        assert_eq!(supervisor.coordinator_status(None).unwrap().len(), 2);
        supervisor.owners_at_start(|_| None);
        let store = supervisor.tenure_store().unwrap();
        let released = store.coordinator("c-gonehold").unwrap().unwrap();
        assert_eq!(released.end_reason.as_deref(), Some(ORPHANED));
        let cause = ended_cause(&supervisor, "c-gonehold");
        assert!(
            cause.contains(&format!("holder process {}", gone_holder.pid)),
            "{cause}"
        );
        assert!(store
            .coordinator("c-livehold")
            .unwrap()
            .unwrap()
            .ended_at
            .is_none());
        // The live one is watched from now on: its exit releases it.
        drop(alive.stdin.take());
        let ended = wait_ended(&supervisor, "c-livehold");
        alive.wait().unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        let _ = std::fs::remove_dir_all(root);
    }

    /// A landing step admitted for `owner`, or its refusal.
    fn land<'a>(
        supervisor: &'a WorkerSupervisor,
        repo: &str,
        owner: Option<&str>,
        kind: &str,
    ) -> Result<LandingStep<'a>, String> {
        supervisor.begin_landing(repo, owner, kind, "r-test")
    }

    #[test]
    fn a_run_lands_only_under_the_tenure_it_was_claimed_under() {
        let (root, supervisor) = scratch("fence");
        // No tenure, then and now: it lands.
        drop(land(&supervisor, "/repo", None, "push").unwrap());
        let first = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        drop(land(&supervisor, "/repo", Some(&first.coordinator_id), "push").unwrap());
        // Claimed under no tenure while a holder holds the repository.
        let refused = land(&supervisor, "/repo", None, "push").unwrap_err();
        assert!(
            refused.contains("p1") && refused.contains(&first.coordinator_id),
            "{refused}"
        );
        // Its tenure ended: nobody holds it, and still it does not land.
        supervisor
            .coordinator_end(&first.coordinator_id, ENDED, None)
            .unwrap();
        let refused = land(&supervisor, "/repo", Some(&first.coordinator_id), "push").unwrap_err();
        assert!(refused.contains("no coordinator holds it now"), "{refused}");
        // Another holder now: named.
        let second = supervisor.coordinator_start("/repo", "p2", None).unwrap();
        let refused = land(&supervisor, "/repo", Some(&first.coordinator_id), "push").unwrap_err();
        assert!(
            refused.contains(&second.coordinator_id) && refused.contains("p2"),
            "{refused}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A run claimed while no tenure was active lands its step under no
    /// tenure: a claim made meanwhile waits for it, so the step never lands
    /// for a holder that claimed the repository in between.
    #[test]
    fn a_claim_waits_for_a_step_a_run_claimed_outside_any_tenure_is_landing() {
        let (root, supervisor) = scratch("fence-unowned");
        let step = land(&supervisor, "/repo", None, "push").unwrap();
        let refused = supervisor
            .coordinator_start("/repo", "p1", None)
            .unwrap_err();
        assert_eq!(refused.code(), "worker_busy", "{refused}");
        assert!(refused.to_string().contains("push"), "{refused}");
        // The step lands, and the repository is free for the claim.
        drop(step);
        let claimed = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(claimed.pane_id.as_deref(), Some("p1"));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The interleaving a check-then-act fence cannot survive: the tenure
    /// changes while the step it admitted is in flight (a latch: the test
    /// holds the step open across the tenure change, no timing).
    #[test]
    fn a_holder_that_ends_during_a_landing_ends_once_the_step_has_landed() {
        let (root, supervisor) = scratch("fence-latch");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let store = supervisor.tenure_store().unwrap();
        let step = land(&supervisor, "/repo", Some(&tenure.coordinator_id), "push").unwrap();
        // The holder asks to end inside the step: the end is recorded here
        // and carried out once the step lands, so the step lands under its
        // own tenure and the repository has no successor meanwhile.
        let ending = supervisor
            .coordinator_end(&tenure.coordinator_id, ENDED, None)
            .unwrap();
        assert_eq!(ending.ended_ms, None, "ended inside its push step");
        assert_eq!(
            store
                .pending_end(&tenure.coordinator_id, tenure.epoch)
                .unwrap()
                .map(|end| end.reason),
            Some(ENDED.to_owned())
        );
        let refused = supervisor
            .coordinator_start("/repo", "p2", None)
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active", "{refused}");
        // The step lands, and only then does the tenure end: the successor
        // gets the repository afterwards, and the landed run can land
        // nothing more.
        drop(step);
        let ended = store.coordinator(&tenure.coordinator_id).unwrap().unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ENDED));
        assert!(
            store
                .pending_end(&tenure.coordinator_id, tenure.epoch)
                .unwrap()
                .is_none(),
            "a carried-out end is no longer pending"
        );
        let refused = land(
            &supervisor,
            "/repo",
            Some(&tenure.coordinator_id),
            "install",
        )
        .unwrap_err();
        assert!(
            refused.contains("lost the tenure") || refused.contains("no longer holds"),
            "{refused}"
        );
        let second = supervisor.coordinator_start("/repo", "p2", None).unwrap();
        assert_eq!(second.epoch, tenure.epoch + 1);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The same interleaving for an end herdr saw itself (a pane closed, a
    /// holder process exited): it cannot be refused, so it is recorded and
    /// carried out by the next reader.
    #[test]
    fn an_end_herdr_saw_is_recorded_before_it_is_carried_out() {
        let (root, supervisor) = scratch("fence-observed");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let store = supervisor.tenure_store().unwrap();
        let step = land(
            &supervisor,
            "/repo",
            Some(&tenure.coordinator_id),
            "install",
        )
        .unwrap();
        supervisor.orphan_coordinator("p1", "the coordinator's pane closed");
        assert_eq!(
            store
                .pending_end(&tenure.coordinator_id, tenure.epoch)
                .unwrap()
                .map(|end| end.reason),
            Some(ORPHANED.to_owned())
        );
        // A claim attempt reads the tenures: it carries out what no step
        // holds back, and this one is held back — and the refusal says the
        // end is coming, so a waiting holder is not taken for a stranded one.
        let refused = supervisor
            .coordinator_start("/repo", "p2", None)
            .unwrap_err();
        assert_eq!(refused.code(), "coordinator_active", "{refused}");
        assert!(
            refused
                .to_string()
                .contains("the end waits for its landing step to land"),
            "{refused}"
        );
        assert!(store
            .coordinator(&tenure.coordinator_id)
            .unwrap()
            .unwrap()
            .ended_at
            .is_none());
        // The step releases the end, with the cause herdr saw.
        drop(step);
        let ended = store.coordinator(&tenure.coordinator_id).unwrap().unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        assert!(
            ended_cause(&supervisor, &tenure.coordinator_id).contains("pane closed"),
            "{}",
            ended_cause(&supervisor, &tenure.coordinator_id)
        );
        supervisor.coordinator_start("/repo", "p2", None).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A tenure whose exit event never reached herdr (a watch it could not
    /// place, a kernel event it lost, an end whose store write failed) is
    /// released by the next claim attempt, which re-checks the recorded
    /// holder's process instead of waiting for a server to start.
    #[cfg(unix)]
    #[test]
    fn a_claim_attempt_releases_a_tenure_whose_holder_is_gone() {
        let (root, supervisor) = scratch("claim-gone-holder");
        let mut holder = holder_process();
        let holder_pid = holder.id();
        let start = crate::platform::process_start_token(holder_pid).unwrap();
        // Recorded with no watch in place: this server never saw its exit.
        supervisor
            .tenure_store()
            .unwrap()
            .transaction(|tx| {
                tx.coordinator_started(
                    &NewTenure {
                        id: "c-gonewatch",
                        repo: "/repo",
                        pane_id: None,
                        session_id: None,
                        holder: Some(HolderProcess {
                            pid: holder_pid,
                            start,
                        }),
                    },
                    now_ms(),
                )
            })
            .unwrap();
        drop(holder.stdin.take());
        holder.wait().unwrap();
        let claimed = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(claimed.pane_id.as_deref(), Some("p1"));
        let ended = supervisor
            .tenure_store()
            .unwrap()
            .coordinator("c-gonewatch")
            .unwrap()
            .unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        assert!(
            ended_cause(&supervisor, "c-gonewatch")
                .contains(&format!("holder process {holder_pid} exited")),
            "{}",
            ended_cause(&supervisor, "c-gonewatch")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A landing step whose server is gone (a restart, a live handoff) holds
    /// no tenure: a server that starts retires it and carries out the end it
    /// held back. The end is recorded first, so a server that ends between
    /// the record and the end loses nothing.
    #[cfg(unix)]
    #[test]
    fn a_server_that_starts_retires_a_dead_servers_landing_step() {
        let (root, supervisor) = scratch("fence-dead-server");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        // The server that admitted the step is gone: its process exited
        // before the step could land.
        let mut server = holder_process();
        let server_identity = (
            server.id(),
            Some(crate::platform::process_start_token(server.id()).unwrap()),
        );
        drop(server.stdin.take());
        server.wait().unwrap();
        {
            let store = supervisor.tenure_store().unwrap();
            store
                .transaction(|tx| {
                    tx.tenure_step(&super::super::store::NewTenureStep {
                        id: "s-deadstep",
                        repo: "/repo",
                        coordinator: Some((&tenure.coordinator_id, tenure.epoch)),
                        kind: "push",
                        run_id: "r-dead",
                        server: server_identity,
                        at_ms: now_ms(),
                    })
                })
                .unwrap();
            store
                .transaction(|tx| {
                    tx.request_tenure_end(&super::super::store::PendingEnd {
                        coordinator_id: tenure.coordinator_id.clone(),
                        epoch: tenure.epoch,
                        reason: ORPHANED.to_owned(),
                        cause: Some("its pane closed".to_owned()),
                        requested_ms: now_ms(),
                    })
                })
                .unwrap();
        }
        drop(supervisor);
        let supervisor = WorkerSupervisor::open(root.join("workers"), "claude".into());
        supervisor.owners_at_start(|_| None);
        let store = supervisor.tenure_store().unwrap();
        let ended = store.coordinator(&tenure.coordinator_id).unwrap().unwrap();
        assert_eq!(ended.end_reason.as_deref(), Some(ORPHANED));
        // The retired step says who retired it: this server, by pid and start
        // token, never by the pid the dead server recorded.
        let retired: (Option<i64>, Option<i64>, Option<i64>) = store
            .connection()
            .query_row(
                "SELECT retired_ms, retired_by_pid, retired_by_start FROM tenure_steps
                 WHERE id = 's-deadstep'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let pid = std::process::id();
        assert!(retired.0.is_some(), "the step records when it was retired");
        assert_eq!(retired.1, Some(i64::from(pid)));
        assert_eq!(
            retired.2,
            crate::platform::process_start_token(pid).map(|start| start as i64)
        );
        supervisor.coordinator_start("/repo", "p2", None).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    /// An end recorded for an earlier epoch of a tenure's id is stale: it is
    /// dropped without ending the tenure that is active now.
    #[test]
    fn a_stale_pending_end_for_an_old_epoch_ends_nothing() {
        let (root, supervisor) = scratch("stale-pending-end");
        let tenure = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        let store = supervisor.tenure_store().unwrap();
        let stale_epoch = tenure.epoch - 1;
        store
            .transaction(|tx| {
                tx.request_tenure_end(&super::super::store::PendingEnd {
                    coordinator_id: tenure.coordinator_id.clone(),
                    epoch: stale_epoch,
                    reason: ORPHANED.to_owned(),
                    cause: Some("an earlier holder exited".to_owned()),
                    requested_ms: now_ms(),
                })
            })
            .unwrap();
        assert!(store
            .pending_end(&tenure.coordinator_id, stale_epoch)
            .unwrap()
            .is_some());
        assert!(store.settle_pending_ends(now_ms()).unwrap().is_empty());
        assert!(
            store
                .coordinator(&tenure.coordinator_id)
                .unwrap()
                .unwrap()
                .ended_at
                .is_none(),
            "a stale end leaves the active tenure alone"
        );
        assert!(store
            .pending_end(&tenure.coordinator_id, stale_epoch)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A step whose release failed stays open (it still holds its tenure) and
    /// is retried by the next reader, so the repository is not stranded while
    /// this server lives.
    #[test]
    fn a_failed_step_release_is_retried_by_the_next_claim() {
        let (root, supervisor) = scratch("release-retry");
        let step = land(&supervisor, "/repo", None, "push").unwrap();
        supervisor.fail_next_landing_release_for_test();
        drop(step);
        let store = supervisor.tenure_store().unwrap();
        let open_steps = || -> i64 {
            store
                .connection()
                .query_row(
                    "SELECT count(*) FROM tenure_steps WHERE retired_ms IS NULL",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(open_steps(), 1, "the failed release left the step open");
        let claimed = supervisor.coordinator_start("/repo", "p1", None).unwrap();
        assert_eq!(claimed.pane_id.as_deref(), Some("p1"));
        assert_eq!(open_steps(), 0, "the next claim retried the release");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A step recorded without a start token cannot be proven gone by its pid
    /// alone, so no reader retires it.
    #[test]
    fn a_step_without_a_start_token_is_never_retired_by_pid_alone() {
        let (root, supervisor) = scratch("step-no-start-token");
        supervisor
            .tenure_store()
            .unwrap()
            .transaction(|tx| {
                tx.tenure_step(&super::super::store::NewTenureStep {
                    id: "s-no-start",
                    repo: "/repo",
                    coordinator: None,
                    kind: "push",
                    run_id: "r-no-start",
                    server: (std::process::id(), None),
                    at_ms: now_ms(),
                })
            })
            .unwrap();
        let refused = supervisor
            .coordinator_start("/repo", "p1", None)
            .unwrap_err();
        assert_eq!(refused.code(), "worker_busy", "{refused}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// `coordinator.acquire`, like `coordinator.start`, waits for a run claimed
    /// outside any tenure to land its step.
    #[cfg(unix)]
    #[test]
    fn coordinator_acquire_waits_for_an_untenured_landing_step() {
        let (root, supervisor) = scratch("acquire-unowned-step");
        let mut holder = holder_process();
        let pid = holder.id();
        let step = land(&supervisor, "/repo", None, "push").unwrap();
        let refused = supervisor
            .coordinator_acquire_process("/repo", pid)
            .unwrap_err();
        assert_eq!(refused.code(), "worker_busy", "{refused}");
        assert!(refused.to_string().contains("push"), "{refused}");
        drop(step);
        let (tenure, _) = supervisor
            .coordinator_acquire_process("/repo", pid)
            .unwrap();
        assert_eq!(tenure.holder_pid, Some(pid));
        drop(holder.stdin.take());
        holder.wait().unwrap();
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
