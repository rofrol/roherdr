//! The pure transition core of a todo run's landing.
//!
//! `step(state, input)` decides what a run does next from approve onwards:
//! the cherry-pick onto the target branch, the verify on that integrated
//! tree, the install, the TODO edit, the push, the cleanup, and the abort
//! paths. It does no I/O, no async work, no logging and no store access:
//! the shell runs the returned effects and feeds their acknowledgements back
//! as inputs, and persists the new state, the cursor and the pending effect
//! intent together.
//!
//! Every input carries the sender's fence (tenure epoch, pane generation).
//! An input from an older fence is rejected without effects, and a newer
//! fence must be adopted first, so two coordinators never both drive a run.
//!
//! `reconcile` decides, at startup or handoff, what to do with an effect
//! whose acknowledgement was not persisted, from the observed git state: it
//! acknowledges what is observed as done and runs again only what is
//! observed as not done (or safe to repeat), never blindly.
//!
//! Trace format v1 (`tests/fixtures/todo-traces/*.json`), replayed by
//! `tests::traces_replay`:
//!
//! ```json
//! {"version": 1,
//!  "initial": <CoreState>,
//!  "steps": [
//!    {"input": <Input>, "expect": {"rejection": "stale_fence"}},
//!    {"input": <Input>, "expect": {"state": {<some CoreState fields>},
//!                                  "effects": [<CoreEffect>...]}},
//!    {"observe": <Observed>, "expect": {"reconcile": <Reconciled>}}]}
//! ```
//!
//! A rejected input leaves the state unchanged. `expect.state` compares only
//! the fields it names; `expect.effects` compares the whole list.

use serde::{Deserialize, Serialize};

use crate::api::schema::{TodoEventKind, TodoRunStatus, TodoStep};

/// Who may drive the run: the coordinator tenure's epoch and the generation
/// of the pane that holds it. Ordered by epoch, then generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub tenure_epoch: u64,
    pub pane_generation: u64,
}

impl Fence {
    fn key(self) -> (u64, u64) {
        (self.tenure_epoch, self.pane_generation)
    }
}

/// An outcome the landing went on without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Degraded {
    /// `skip-install`: the commit landed without the registered install.
    InstallSkipped,
    /// `skip-todo`: the commit landed without the TODO edit.
    TodoSkipped,
}

/// Why the landing waits for the coordinator or stopped for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// The cherry-pick onto the target branch conflicted.
    CherryPickConflict,
    /// The verify on the integrated tree failed or could not run.
    VerifyFailed,
    InstallFailed,
    TodoFailed,
    /// The push failed without origin having diverged: retry it or abort.
    PushFailed,
    /// Origin's branch is not an ancestor of the target branch: only the
    /// user can resolve that, so the run stops (no wait, no retry-push).
    PushDiverged,
}

/// The effect whose acknowledgement the core waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingEffect {
    CherryPick,
    VerifyIntegrated,
    Install,
    TodoEdit,
    Push,
    RestoreBranch,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreState {
    pub step: TodoStep,
    pub status: TodoRunStatus,
    pub fence: Fence,
    /// The run holds the worker slot; released only after an acknowledged
    /// push or once the run ended aborted.
    pub slot_held: bool,
    #[serde(default)]
    pub pending: Option<PendingEffect>,
    #[serde(default)]
    pub degraded: Vec<Degraded>,
    #[serde(default)]
    pub attention: Option<Attention>,
    /// The commit the cherry-pick made on the target branch.
    #[serde(default)]
    pub picked: Option<String>,
    /// The commit origin acknowledged; once set, an abort leaves it
    /// published.
    #[serde(default)]
    pub pushed: Option<String>,
    /// The id of the last input applied.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub id: String,
    pub fence: Fence,
    pub event: CoreEvent,
}

/// The coordinator's actions and the effects' acknowledgements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreEvent {
    Approve,
    Answer,
    Retry,
    Verify,
    RetryInstall,
    SkipInstall,
    RetryTodo,
    SkipTodo,
    RetryPush,
    Abort,
    /// A newer tenure or pane takes the run over: the input's fence becomes
    /// the run's.
    Adopt,
    Picked {
        sha: String,
    },
    PickFailed,
    Verified,
    VerifyFailed,
    Installed,
    InstallFailed,
    Edited,
    TodoFailed,
    PushAcked {
        sha: String,
    },
    PushRejected {
        diverged: bool,
    },
    BranchRestored,
    CleanedUp,
}

impl CoreEvent {
    /// The effect this event acknowledges, if it is an acknowledgement.
    fn acknowledges(&self) -> Option<PendingEffect> {
        Some(match self {
            Self::Picked { .. } | Self::PickFailed => PendingEffect::CherryPick,
            Self::Verified | Self::VerifyFailed => PendingEffect::VerifyIntegrated,
            Self::Installed | Self::InstallFailed => PendingEffect::Install,
            Self::Edited | Self::TodoFailed => PendingEffect::TodoEdit,
            Self::PushAcked { .. } | Self::PushRejected { .. } => PendingEffect::Push,
            Self::BranchRestored => PendingEffect::RestoreBranch,
            Self::CleanedUp => PendingEffect::Cleanup,
            _ => return None,
        })
    }
}

/// What the shell does; every effect but `release_slot` and `emit` is
/// acknowledged by an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreEffect {
    /// Cherry-pick the approved commit onto the target branch.
    CherryPick,
    /// Run the checks on the target branch with the picked commit.
    VerifyIntegrated,
    Install,
    TodoEdit,
    /// Push the target branch to origin, only as a fast-forward.
    Push,
    /// Reset the local target branch to where it was before the
    /// cherry-pick.
    RestoreBranch,
    /// Free the worker slot for the next run.
    ReleaseSlot,
    /// Delete the run's merged branches.
    Cleanup,
    /// Raise a run event for the coordinator.
    Emit {
        event: TodoEventKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    /// The input's fence is older than the run's.
    StaleFence,
    /// The input's fence is newer than the run's and was not adopted.
    FenceNotAdopted,
    /// The input id is the one last applied.
    Duplicate,
    /// The run is done or aborted.
    Terminal,
    /// The run is not in its landing.
    NotInLanding,
    /// An action arrived while an effect runs.
    EffectInFlight,
    /// An acknowledgement of an effect that is not pending.
    UnexpectedAck,
    /// The action does not apply where the run stands.
    NotAllowed,
}

/// Apply one input. A rejection changes nothing and has no effects.
pub fn step(s: &CoreState, i: &Input) -> Result<(CoreState, Vec<CoreEffect>), Rejection> {
    let adopt = matches!(i.event, CoreEvent::Adopt);
    match i.fence.key().cmp(&s.fence.key()) {
        std::cmp::Ordering::Less => return Err(Rejection::StaleFence),
        std::cmp::Ordering::Equal if adopt => return Err(Rejection::StaleFence),
        std::cmp::Ordering::Greater if !adopt => return Err(Rejection::FenceNotAdopted),
        _ => {}
    }
    if s.cursor.as_deref() == Some(i.id.as_str()) {
        return Err(Rejection::Duplicate);
    }
    let mut n = s.clone();
    n.cursor = Some(i.id.clone());
    if adopt {
        n.fence = i.fence;
        return Ok((n, Vec::new()));
    }
    if matches!(s.status, TodoRunStatus::Done | TodoRunStatus::Aborted) {
        return Err(Rejection::Terminal);
    }
    if let Some(acked) = i.event.acknowledges() {
        if s.pending != Some(acked) {
            return Err(Rejection::UnexpectedAck);
        }
        n.pending = None;
        return Ok(acknowledged(n, &i.event));
    }
    action(n, &i.event)
}

fn acknowledged(mut n: CoreState, event: &CoreEvent) -> (CoreState, Vec<CoreEffect>) {
    match event {
        CoreEvent::Picked { sha } => {
            n.picked = Some(sha.clone());
            go(n, TodoStep::Verify, PendingEffect::VerifyIntegrated)
        }
        CoreEvent::PickFailed => stop(n, Attention::CherryPickConflict),
        CoreEvent::Verified => go(n, TodoStep::Install, PendingEffect::Install),
        CoreEvent::VerifyFailed => wait(n, Attention::VerifyFailed, TodoEventKind::VerifyFailed),
        CoreEvent::Installed => go(n, TodoStep::Todo, PendingEffect::TodoEdit),
        CoreEvent::InstallFailed => wait(n, Attention::InstallFailed, TodoEventKind::InstallFailed),
        CoreEvent::Edited => go(n, TodoStep::Push, PendingEffect::Push),
        CoreEvent::TodoFailed => wait(n, Attention::TodoFailed, TodoEventKind::TodoFailed),
        CoreEvent::PushAcked { sha } => {
            n.pushed = Some(sha.clone());
            let (mut n, mut effects) = go(n, TodoStep::Cleanup, PendingEffect::Cleanup);
            n.slot_held = false;
            effects.insert(0, CoreEffect::ReleaseSlot);
            (n, effects)
        }
        CoreEvent::PushRejected { diverged: true } => stop(n, Attention::PushDiverged),
        CoreEvent::PushRejected { diverged: false } => {
            wait(n, Attention::PushFailed, TodoEventKind::PushFailed)
        }
        CoreEvent::BranchRestored => aborted(n),
        CoreEvent::CleanedUp => {
            n.step = TodoStep::Done;
            n.status = TodoRunStatus::Done;
            n.attention = None;
            (
                n,
                vec![CoreEffect::Emit {
                    event: TodoEventKind::Done,
                }],
            )
        }
        // `acknowledges` admits only the events above.
        _ => (n, Vec::new()),
    }
}

fn action(mut n: CoreState, event: &CoreEvent) -> Result<(CoreState, Vec<CoreEffect>), Rejection> {
    let landing = matches!(
        n.step,
        TodoStep::CherryPick
            | TodoStep::Verify
            | TodoStep::Install
            | TodoStep::Todo
            | TodoStep::Push
            | TodoStep::Cleanup
            | TodoStep::Abort
    );
    if n.step == TodoStep::Review {
        if n.status != TodoRunStatus::Waiting {
            return Err(Rejection::NotAllowed);
        }
        return match event {
            CoreEvent::Approve => Ok(go(n, TodoStep::CherryPick, PendingEffect::CherryPick)),
            // Nothing landed yet: the target branch was never touched.
            CoreEvent::Abort => Ok(aborted(n)),
            _ => Err(Rejection::NotAllowed),
        };
    }
    if !landing {
        return Err(Rejection::NotInLanding);
    }
    if matches!(event, CoreEvent::Abort) && n.pushed.is_some() {
        // The published commit stays; the slot is already free.
        n.pending = None;
        return Ok(aborted(n));
    }
    if n.pending.is_some() {
        return Err(Rejection::EffectInFlight);
    }
    let attention = n.attention;
    let blocked = n.status == TodoRunStatus::Blocked;
    match (event, n.step, attention) {
        (CoreEvent::Abort, _, _) => Ok(go(n, TodoStep::Abort, PendingEffect::RestoreBranch)),
        _ if blocked => Err(Rejection::NotAllowed),
        (CoreEvent::Verify, TodoStep::Verify, Some(Attention::VerifyFailed)) => {
            Ok(go(n, TodoStep::Verify, PendingEffect::VerifyIntegrated))
        }
        (CoreEvent::RetryInstall, TodoStep::Install, Some(Attention::InstallFailed)) => {
            Ok(go(n, TodoStep::Install, PendingEffect::Install))
        }
        (CoreEvent::SkipInstall, TodoStep::Install, Some(Attention::InstallFailed)) => {
            n.degraded.push(Degraded::InstallSkipped);
            Ok(go(n, TodoStep::Todo, PendingEffect::TodoEdit))
        }
        (CoreEvent::RetryTodo, TodoStep::Todo, Some(Attention::TodoFailed)) => {
            Ok(go(n, TodoStep::Todo, PendingEffect::TodoEdit))
        }
        (CoreEvent::SkipTodo, TodoStep::Todo, Some(Attention::TodoFailed)) => {
            n.degraded.push(Degraded::TodoSkipped);
            Ok(go(n, TodoStep::Push, PendingEffect::Push))
        }
        (CoreEvent::RetryPush, TodoStep::Push, Some(Attention::PushFailed)) => {
            Ok(go(n, TodoStep::Push, PendingEffect::Push))
        }
        _ => Err(Rejection::NotAllowed),
    }
}

fn effect_of(pending: PendingEffect) -> CoreEffect {
    match pending {
        PendingEffect::CherryPick => CoreEffect::CherryPick,
        PendingEffect::VerifyIntegrated => CoreEffect::VerifyIntegrated,
        PendingEffect::Install => CoreEffect::Install,
        PendingEffect::TodoEdit => CoreEffect::TodoEdit,
        PendingEffect::Push => CoreEffect::Push,
        PendingEffect::RestoreBranch => CoreEffect::RestoreBranch,
        PendingEffect::Cleanup => CoreEffect::Cleanup,
    }
}

/// Move to `step` and run `pending`'s effect.
fn go(mut n: CoreState, step: TodoStep, pending: PendingEffect) -> (CoreState, Vec<CoreEffect>) {
    n.step = step;
    n.status = TodoRunStatus::Running;
    n.attention = None;
    n.pending = Some(pending);
    (n, vec![effect_of(pending)])
}

/// Wait for the coordinator, keeping the failed stage.
fn wait(
    mut n: CoreState,
    attention: Attention,
    event: TodoEventKind,
) -> (CoreState, Vec<CoreEffect>) {
    n.status = TodoRunStatus::Waiting;
    n.attention = Some(attention);
    (n, vec![CoreEffect::Emit { event }])
}

/// Stop for the user: only abort goes on from here.
fn stop(mut n: CoreState, attention: Attention) -> (CoreState, Vec<CoreEffect>) {
    n.status = TodoRunStatus::Blocked;
    n.attention = Some(attention);
    (
        n,
        vec![CoreEffect::Emit {
            event: TodoEventKind::Blocked,
        }],
    )
}

fn aborted(mut n: CoreState) -> (CoreState, Vec<CoreEffect>) {
    n.step = TodoStep::Abort;
    n.status = TodoRunStatus::Aborted;
    n.attention = None;
    let mut effects = Vec::new();
    if n.slot_held {
        n.slot_held = false;
        effects.push(CoreEffect::ReleaseSlot);
    }
    effects.push(CoreEffect::Emit {
        event: TodoEventKind::Aborted,
    });
    (n, effects)
}

/// Origin's target branch as observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    /// Origin's branch is the local target branch's tip `sha`.
    Contains { sha: String },
    /// Origin's branch is an ancestor of the local one, not at its tip.
    Behind,
    /// Origin's branch is not an ancestor of the local one.
    Diverged,
}

/// What startup or a handoff observes in the landing worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observed {
    /// A commit on the target branch with this run's trailers.
    #[serde(default)]
    pub picked: Option<String>,
    /// The installed build is the one the install would report.
    #[serde(default)]
    pub installed: bool,
    /// The TODO edit's commit is on the target branch.
    #[serde(default)]
    pub todo_committed: bool,
    pub origin: Origin,
    /// The target branch is back where it was before the cherry-pick.
    #[serde(default)]
    pub restored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reconciled {
    /// No effect was in flight.
    Nothing,
    /// Feed this acknowledgement, observed from git.
    Ack { event: CoreEvent },
    /// Run the effect again: it is observed as not done, or repeating it
    /// changes nothing more.
    Rerun { effect: CoreEffect },
}

/// Decide what to do with the pending effect after a crash or handoff.
/// Install and the TODO edit are not observed as done become failures for
/// the coordinator to retry or skip, never repeated by themselves.
pub fn reconcile(s: &CoreState, o: &Observed) -> Reconciled {
    let ack = |event| Reconciled::Ack { event };
    let rerun = |pending| Reconciled::Rerun {
        effect: effect_of(pending),
    };
    let Some(pending) = s.pending else {
        return Reconciled::Nothing;
    };
    match pending {
        PendingEffect::CherryPick => match &o.picked {
            Some(sha) => ack(CoreEvent::Picked { sha: sha.clone() }),
            None => rerun(pending),
        },
        PendingEffect::VerifyIntegrated | PendingEffect::Cleanup => rerun(pending),
        PendingEffect::Install if o.installed => ack(CoreEvent::Installed),
        PendingEffect::Install => ack(CoreEvent::InstallFailed),
        PendingEffect::TodoEdit if o.todo_committed => ack(CoreEvent::Edited),
        PendingEffect::TodoEdit => ack(CoreEvent::TodoFailed),
        PendingEffect::Push => match &o.origin {
            Origin::Contains { sha } => ack(CoreEvent::PushAcked { sha: sha.clone() }),
            Origin::Diverged => ack(CoreEvent::PushRejected { diverged: true }),
            Origin::Behind => rerun(pending),
        },
        PendingEffect::RestoreBranch if o.restored => ack(CoreEvent::BranchRestored),
        PendingEffect::RestoreBranch => rerun(pending),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::*;

    const FENCE: Fence = Fence {
        tenure_epoch: 4,
        pane_generation: 2,
    };

    fn state(step: TodoStep, status: TodoRunStatus, pending: Option<PendingEffect>) -> CoreState {
        CoreState {
            step,
            status,
            fence: FENCE,
            slot_held: true,
            pending,
            degraded: Vec::new(),
            attention: None,
            picked: None,
            pushed: None,
            cursor: None,
        }
    }

    fn input(id: &str, event: CoreEvent) -> Input {
        Input {
            id: id.to_owned(),
            fence: FENCE,
            event,
        }
    }

    fn apply(s: &CoreState, id: &str, event: CoreEvent) -> (CoreState, Vec<CoreEffect>) {
        match step(s, &input(id, event.clone())) {
            Ok(next) => next,
            Err(rejection) => panic!("{event:?} at {s:?} rejected: {rejection:?}"),
        }
    }

    fn emit(event: TodoEventKind) -> CoreEffect {
        CoreEffect::Emit { event }
    }

    #[test]
    fn happy_landing_verifies_the_integrated_tree_and_frees_the_slot_after_the_push() {
        let s = state(TodoStep::Review, TodoRunStatus::Waiting, None);
        let (s, e) = apply(&s, "e1", CoreEvent::Approve);
        assert_eq!(e, vec![CoreEffect::CherryPick]);
        let (s, e) = apply(&s, "e2", CoreEvent::Picked { sha: "p1".into() });
        assert_eq!(e, vec![CoreEffect::VerifyIntegrated]);
        assert_eq!(s.step, TodoStep::Verify);
        let (s, e) = apply(&s, "e3", CoreEvent::Verified);
        assert_eq!(e, vec![CoreEffect::Install]);
        let (s, e) = apply(&s, "e4", CoreEvent::Installed);
        assert_eq!(e, vec![CoreEffect::TodoEdit]);
        let (s, e) = apply(&s, "e5", CoreEvent::Edited);
        assert_eq!(e, vec![CoreEffect::Push]);
        assert!(
            s.slot_held,
            "the slot stays held until the push is acknowledged"
        );
        let (s, e) = apply(&s, "e6", CoreEvent::PushAcked { sha: "t1".into() });
        assert_eq!(e, vec![CoreEffect::ReleaseSlot, CoreEffect::Cleanup]);
        assert!(!s.slot_held);
        let (s, e) = apply(&s, "e7", CoreEvent::CleanedUp);
        assert_eq!(e, vec![emit(TodoEventKind::Done)]);
        assert_eq!((s.step, s.status), (TodoStep::Done, TodoRunStatus::Done));
        assert!(s.degraded.is_empty());
    }

    #[test]
    fn fence_guards_every_input() {
        let s = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        let older = Input {
            fence: Fence {
                tenure_epoch: 3,
                pane_generation: 9,
            },
            ..input("e1", CoreEvent::PushAcked { sha: "t".into() })
        };
        assert_eq!(step(&s, &older), Err(Rejection::StaleFence));
        let older_pane = Input {
            fence: Fence {
                tenure_epoch: 4,
                pane_generation: 1,
            },
            ..input("e1", CoreEvent::Abort)
        };
        assert_eq!(step(&s, &older_pane), Err(Rejection::StaleFence));
        let newer = Fence {
            tenure_epoch: 5,
            pane_generation: 0,
        };
        let unadopted = Input {
            fence: newer,
            ..input("e2", CoreEvent::PushAcked { sha: "t".into() })
        };
        assert_eq!(step(&s, &unadopted), Err(Rejection::FenceNotAdopted));
        let adopt = Input {
            fence: newer,
            ..input("e3", CoreEvent::Adopt)
        };
        let (s2, e) = step(&s, &adopt).expect("adopt");
        assert!(e.is_empty());
        assert_eq!(s2.fence, newer);
        // The old tenure is now stale; the adopted one drives.
        assert_eq!(
            step(&s2, &input("e4", CoreEvent::PushAcked { sha: "t".into() })),
            Err(Rejection::StaleFence)
        );
        assert!(step(&s2, &unadopted).is_ok());
        // Adopting the fence the run already has is not newer.
        assert_eq!(step(&s2, &adopt), Err(Rejection::StaleFence));
    }

    #[test]
    fn acknowledgements_must_match_the_pending_effect() {
        let s = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        assert_eq!(
            step(&s, &input("e1", CoreEvent::Installed)),
            Err(Rejection::UnexpectedAck)
        );
        assert_eq!(
            step(&s, &input("e1", CoreEvent::RetryPush)),
            Err(Rejection::EffectInFlight)
        );
        assert_eq!(
            step(&s, &input("e1", CoreEvent::Abort)),
            Err(Rejection::EffectInFlight)
        );
        let (s, _) = apply(&s, "e1", CoreEvent::PushAcked { sha: "t".into() });
        assert_eq!(
            step(&s, &input("e1", CoreEvent::CleanedUp)),
            Err(Rejection::Duplicate)
        );
    }

    #[test]
    fn diverged_origin_stops_for_the_user_and_takes_only_abort() {
        let s = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        let (s, e) = apply(&s, "e1", CoreEvent::PushRejected { diverged: true });
        assert_eq!(e, vec![emit(TodoEventKind::Blocked)]);
        assert_eq!(s.status, TodoRunStatus::Blocked);
        assert_eq!(s.attention, Some(Attention::PushDiverged));
        assert!(s.slot_held);
        assert_eq!(
            step(&s, &input("e2", CoreEvent::RetryPush)),
            Err(Rejection::NotAllowed)
        );
        let (s, e) = apply(&s, "e3", CoreEvent::Abort);
        assert_eq!(e, vec![CoreEffect::RestoreBranch]);
        let (s, e) = apply(&s, "e4", CoreEvent::BranchRestored);
        assert_eq!(
            e,
            vec![CoreEffect::ReleaseSlot, emit(TodoEventKind::Aborted)]
        );
        assert_eq!(s.status, TodoRunStatus::Aborted);
    }

    #[test]
    fn a_failed_push_without_divergence_waits_for_retry_push() {
        let s = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        let (s, e) = apply(&s, "e1", CoreEvent::PushRejected { diverged: false });
        assert_eq!(e, vec![emit(TodoEventKind::PushFailed)]);
        let (_, e) = apply(&s, "e2", CoreEvent::RetryPush);
        assert_eq!(e, vec![CoreEffect::Push]);
    }

    #[test]
    fn abort_after_the_cherry_pick_restores_the_branch() {
        let s = state(
            TodoStep::CherryPick,
            TodoRunStatus::Running,
            Some(PendingEffect::CherryPick),
        );
        let (s, _) = apply(&s, "e1", CoreEvent::Picked { sha: "p".into() });
        let (s, _) = apply(&s, "e2", CoreEvent::VerifyFailed);
        assert_eq!(s.status, TodoRunStatus::Waiting);
        assert_eq!(s.step, TodoStep::Verify);
        let (s, e) = apply(&s, "e3", CoreEvent::Abort);
        assert_eq!(e, vec![CoreEffect::RestoreBranch]);
        assert_eq!(s.status, TodoRunStatus::Running);
        let (s, e) = apply(&s, "e4", CoreEvent::BranchRestored);
        assert_eq!(
            e,
            vec![CoreEffect::ReleaseSlot, emit(TodoEventKind::Aborted)]
        );
        assert_eq!(
            (s.step, s.status),
            (TodoStep::Abort, TodoRunStatus::Aborted)
        );
        assert_eq!(
            step(&s, &input("e5", CoreEvent::Approve)),
            Err(Rejection::Terminal)
        );
    }

    #[test]
    fn abort_after_an_acknowledged_push_keeps_the_published_commit() {
        let s = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        let (s, _) = apply(&s, "e1", CoreEvent::PushAcked { sha: "t".into() });
        let (s, e) = apply(&s, "e2", CoreEvent::Abort);
        assert_eq!(e, vec![emit(TodoEventKind::Aborted)]);
        assert_eq!(s.status, TodoRunStatus::Aborted);
        assert_eq!(s.pushed.as_deref(), Some("t"));
        assert!(!e.contains(&CoreEffect::RestoreBranch));
    }

    #[test]
    fn skips_record_degraded_outcomes() {
        let s = state(
            TodoStep::Install,
            TodoRunStatus::Running,
            Some(PendingEffect::Install),
        );
        let (s, _) = apply(&s, "e1", CoreEvent::InstallFailed);
        let (s, e) = apply(&s, "e2", CoreEvent::SkipInstall);
        assert_eq!(e, vec![CoreEffect::TodoEdit]);
        let (s, _) = apply(&s, "e3", CoreEvent::TodoFailed);
        assert_eq!(
            step(&s, &input("e4", CoreEvent::SkipInstall)),
            Err(Rejection::NotAllowed)
        );
        let (s, e) = apply(&s, "e4", CoreEvent::SkipTodo);
        assert_eq!(e, vec![CoreEffect::Push]);
        assert_eq!(
            s.degraded,
            vec![Degraded::InstallSkipped, Degraded::TodoSkipped]
        );
    }

    #[test]
    fn worker_actions_do_not_apply_in_the_landing() {
        let s = state(TodoStep::Verify, TodoRunStatus::Waiting, None);
        let s = CoreState {
            attention: Some(Attention::VerifyFailed),
            ..s
        };
        for event in [CoreEvent::Retry, CoreEvent::Answer, CoreEvent::Approve] {
            assert_eq!(step(&s, &input("e1", event)), Err(Rejection::NotAllowed));
        }
        let start = state(TodoStep::Start, TodoRunStatus::Running, None);
        assert_eq!(
            step(&start, &input("e1", CoreEvent::Approve)),
            Err(Rejection::NotInLanding)
        );
    }

    #[test]
    fn reconcile_acts_on_observed_git_state_only() {
        let behind = Observed {
            picked: None,
            installed: false,
            todo_committed: false,
            origin: Origin::Behind,
            restored: false,
        };
        let push = state(
            TodoStep::Push,
            TodoRunStatus::Running,
            Some(PendingEffect::Push),
        );
        assert_eq!(
            reconcile(&push, &behind),
            Reconciled::Rerun {
                effect: CoreEffect::Push
            }
        );
        let pushed = Observed {
            origin: Origin::Contains { sha: "t".into() },
            ..behind.clone()
        };
        assert_eq!(
            reconcile(&push, &pushed),
            Reconciled::Ack {
                event: CoreEvent::PushAcked { sha: "t".into() }
            }
        );
        let diverged = Observed {
            origin: Origin::Diverged,
            ..behind.clone()
        };
        assert_eq!(
            reconcile(&push, &diverged),
            Reconciled::Ack {
                event: CoreEvent::PushRejected { diverged: true }
            }
        );
        let install = state(
            TodoStep::Install,
            TodoRunStatus::Running,
            Some(PendingEffect::Install),
        );
        assert_eq!(
            reconcile(&install, &behind),
            Reconciled::Ack {
                event: CoreEvent::InstallFailed
            }
        );
        let pick = state(
            TodoStep::CherryPick,
            TodoRunStatus::Running,
            Some(PendingEffect::CherryPick),
        );
        let picked = Observed {
            picked: Some("p".into()),
            ..behind.clone()
        };
        assert_eq!(
            reconcile(&pick, &picked),
            Reconciled::Ack {
                event: CoreEvent::Picked { sha: "p".into() }
            }
        );
        let idle = state(TodoStep::Review, TodoRunStatus::Waiting, None);
        assert_eq!(reconcile(&idle, &behind), Reconciled::Nothing);
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Trace {
        version: u32,
        initial: CoreState,
        steps: Vec<TraceStep>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TraceStep {
        #[serde(default)]
        input: Option<Input>,
        #[serde(default)]
        observe: Option<Observed>,
        expect: Expect,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Expect {
        #[serde(default)]
        rejection: Option<Rejection>,
        #[serde(default)]
        state: Option<Value>,
        #[serde(default)]
        effects: Option<Vec<CoreEffect>>,
        #[serde(default)]
        reconcile: Option<Reconciled>,
    }

    fn traces_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/todo-traces")
    }

    fn replay(name: &str, trace: Trace) {
        assert_eq!(trace.version, 1, "{name}: unknown trace version");
        let mut s = trace.initial;
        for (n, t) in trace.steps.into_iter().enumerate() {
            let at = format!("{name} step {n}");
            match (t.input, t.observe) {
                (Some(i), None) => match step(&s, &i) {
                    Err(rejection) => {
                        assert_eq!(t.expect.rejection, Some(rejection), "{at}");
                        assert!(
                            t.expect.effects.as_deref().unwrap_or_default().is_empty(),
                            "{at}: a rejection has no effects"
                        );
                    }
                    Ok((next, effects)) => {
                        assert_eq!(t.expect.rejection, None, "{at}: applied, not rejected");
                        let expected = t
                            .expect
                            .effects
                            .unwrap_or_else(|| panic!("{at}: an applied input names its effects"));
                        assert_eq!(effects, expected, "{at}: effects");
                        s = next;
                    }
                },
                (None, Some(o)) => {
                    let expected = t
                        .expect
                        .reconcile
                        .unwrap_or_else(|| panic!("{at}: an observation names its reconcile"));
                    assert_eq!(reconcile(&s, &o), expected, "{at}: reconcile");
                }
                _ => panic!("{at}: a step has exactly one of input and observe"),
            }
            if let Some(Value::Object(fields)) = t.expect.state {
                let actual = serde_json::to_value(&s).expect("state serializes");
                for (key, value) in fields {
                    assert_eq!(actual.get(&key), Some(&value), "{at}: state.{key}");
                }
            }
        }
    }

    #[test]
    fn traces_replay() {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(traces_dir())
            .expect("traces directory")
            .map(|entry| entry.expect("trace entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        assert!(paths.len() >= 7, "expected the v1 traces, found {paths:?}");
        for path in paths {
            let name = path.display().to_string();
            let text = std::fs::read_to_string(&path).expect("trace readable");
            let trace: Trace =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            replay(&name, trace);
        }
    }
}
