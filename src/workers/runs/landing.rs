//! The shell of a todo run's landing: it feeds the pure transition core
//! ([`super::core`]) the coordinator's actions and the acknowledgements of
//! the effects it runs, persists the core's state (with its cursor, the
//! effect it waits for and that effect's intent) with the run's row and its
//! events in one store transaction, and runs the one effect the core waits
//! for. It decides nothing about the landing itself.
//!
//! The landing starts once an approved attempt's worker stopped
//! ([`WorkerSupervisor::start_landing`]): the approval is checked against
//! the branch (a moved branch is reviewed again); then the core takes
//! `approve`, or, in the repository rooted at the home directory,
//! `ask_landing_approval` and waits for its owner's `approve_landing` of
//! the commit it names (a branch that moved meanwhile is reviewed again).
//! From there: the verify of the integrated tree (the
//! approved commit picked onto the target branch's head as a commit of its
//! own, checked in the folder slot, the target branch untouched), the
//! landing of that verified tree on the target branch with the run's
//! trailers, only while the branch's head is still the verified parent (a
//! moved head is verified again), the install, the TODO edit, the push, the
//! cleanup; a head that keeps moving stops the run after
//! [`core::MAX_REVERIFIES`] verifies again. The TODO edit's commit is made
//! the same way, with the same trailers. An abort after the landing and
//! before an acknowledged push takes the run's commits (found by their
//! trailers and the shas the run recorded) off the branch again, by a
//! compare-and-swap on its ref, unless origin itself has them. A diverged
//! origin stops the run for the user, and so does an origin the abort
//! cannot read.
//!
//! An effect records its intent, and that it may have begun, right before
//! its side effect. A driver that takes a run whose effect began and was
//! never acknowledged (a server that ended, or let go in a live handoff)
//! reconciles it from what git shows ([`WorkerSupervisor::reconcile_landing`]):
//! it acknowledges what is done and runs again only what is not, never
//! blindly. That happens when a server starts or a coordinator's handoff
//! completes, never on a timer. A halted run keeps its effect: its abort
//! settles that effect from git first ([`WorkerSupervisor::settle_pending`]).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use super::core::{
    self, Attention, CoreEffect, CoreEvent, CoreState, Fence, Observed, Origin, PendingEffect,
    Reconciled,
};
use super::{
    announce, check_private_details, git, git_with, landing_trailers, new_event, run_trailer,
    Intent, Landing, Run, APPROVE, RUN_ENV,
};
use crate::api::schema::{
    TodoAction, TodoDegraded, TodoEventKind, TodoLanding, TodoLandingSource, TodoRunEvent,
    TodoRunStatus, TodoStep, WorkerVerdict, WorkerVerification,
};
use crate::workers::{lock, now_ms, WorkerError, WorkerSupervisor};

/// The trailer a revert of a run's landing carries, naming the run and
/// attempt as `Herdr-Run` does; the revert carries no `Herdr-Run` itself.
const REVERT_TRAILER: &str = "Herdr-Revert";

impl Run {
    /// The landing as the core holds it: the stored one, or for a run
    /// recorded before the core, derived from its step (the cherry-pick and
    /// the verify ran before the landing then, so a run at either has not
    /// started it). A running step derived so may have begun its effect: it
    /// is reconciled from git, never run again blindly. None before the
    /// landing starts, and for a stored landing that does not parse.
    pub(in crate::workers) fn landing_state(&self) -> Option<Landing> {
        if self.landing_invalid.is_some() {
            return None;
        }
        if let Some(landing) = &self.landing {
            return Some(landing.clone());
        }
        let pending = match self.info.step {
            TodoStep::Install => PendingEffect::Install,
            TodoStep::Todo => PendingEffect::TodoEdit,
            TodoStep::Push => PendingEffect::Push,
            TodoStep::Cleanup => PendingEffect::Cleanup,
            _ => return None,
        };
        let (status, pending, attention) = match self.info.status {
            TodoRunStatus::Running => (TodoRunStatus::Running, Some(pending), None),
            TodoRunStatus::Waiting => (
                TodoRunStatus::Waiting,
                None,
                Some(match pending {
                    PendingEffect::Install => Attention::InstallFailed,
                    PendingEffect::TodoEdit => Attention::TodoFailed,
                    _ => Attention::PushFailed,
                }),
            ),
            TodoRunStatus::Blocked => (TodoRunStatus::Blocked, None, Some(Attention::Halted)),
            _ => return None,
        };
        let cleanup = self.info.step == TodoStep::Cleanup;
        Some(Landing {
            core: CoreState {
                step: self.info.step,
                status,
                fence: Fence {
                    tenure_epoch: 0,
                    pane_generation: 0,
                },
                slot_held: !cleanup,
                pending,
                degraded: Vec::new(),
                attention,
                integrated: None,
                picked: self.info.picked.clone(),
                // At its cleanup the run pushed or had nowhere to push.
                pushed: self
                    .info
                    .pushed
                    .clone()
                    .or_else(|| cleanup.then(|| self.info.picked.clone()).flatten()),
                cursor: None,
                abort_requested: false,
                reverifies: 0,
                owner_approval: None,
                push_skipped: None,
            },
            started: pending.is_some(),
            intent: None,
        })
    }

    /// Whether the run is in its landing as the core holds it.
    pub(in crate::workers) fn in_landing(&self) -> bool {
        self.landing_state()
            .is_some_and(|landing| core::in_landing(landing.core.step))
    }

    /// The core's state written back into the run: its row's step, status
    /// and picked commit follow it. `kept` is the landing whose pending
    /// effect the state keeps (a halt, an abort that settles it first), with
    /// whether that effect began and its intent; otherwise the effect is a
    /// new one, not begun.
    fn take_core(&mut self, state: CoreState, kept: Option<Landing>) {
        self.info.step = state.step;
        self.info.status = state.status;
        self.info.picked = state.picked.clone();
        self.info.degraded = degraded_of(&state);
        self.info.push_skipped = state.push_skipped.clone();
        let (started, intent) = match kept {
            Some(kept) if kept.core.pending == state.pending => (kept.started, kept.intent),
            _ => (false, None),
        };
        self.landing = Some(Landing {
            core: state,
            started,
            intent,
        });
    }

    /// What the begun effect's intent recorded.
    fn intent(&self) -> Intent {
        self.landing
            .as_ref()
            .and_then(|landing| landing.intent.clone())
            .unwrap_or_default()
    }
}

/// The degraded outcomes as `todo.status` shows them.
pub(in crate::workers) fn degraded_of(state: &CoreState) -> Vec<TodoDegraded> {
    state
        .degraded
        .iter()
        .map(|degraded| match degraded {
            core::Degraded::InstallSkipped => TodoDegraded::InstallSkipped,
            core::Degraded::TodoSkipped => TodoDegraded::TodoSkipped,
        })
        .collect()
}

/// The id of the input after the core's cursor.
fn next_input_id(state: &CoreState) -> String {
    let last = state
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.strip_prefix('c'))
        .and_then(|number| number.parse::<u64>().ok())
        .unwrap_or(0);
    format!("c{}", last + 1)
}

/// The input the core takes for a coordinator's action in the landing.
pub(super) fn landing_action(action: TodoAction) -> Option<CoreEvent> {
    Some(match action {
        TodoAction::Verify => CoreEvent::Verify,
        TodoAction::RetryInstall => CoreEvent::RetryInstall,
        TodoAction::SkipInstall => CoreEvent::SkipInstall,
        TodoAction::RetryTodo => CoreEvent::RetryTodo,
        TodoAction::SkipTodo => CoreEvent::SkipTodo,
        TodoAction::RetryPush => CoreEvent::RetryPush,
        TodoAction::Abort => CoreEvent::Abort,
        _ => return None,
    })
}

/// Applies `event` to the run's landing core: the new state written into
/// the run, and the event a resulting `emit` effect raises, if any.
/// `ReleaseSlot` needs no work of its own: the run's claim on its
/// repository and slot ends with the run, which follows it. Every other
/// effect is the one the core now waits for, which the driver runs.
fn feed(run: &mut Run, event: CoreEvent) -> Result<Option<TodoEventKind>, String> {
    if let Some((_, error)) = &run.landing_invalid {
        return Err(format!(
            "the run's stored landing state does not parse ({error}); nothing of its landing runs"
        ));
    }
    let current = run
        .landing_state()
        .ok_or("the run has not started its landing")?;
    let input = core::Input {
        id: next_input_id(&current.core),
        fence: current.core.fence,
        event: event.clone(),
    };
    let (next, effects) = core::step(&current.core, &input).map_err(|rejection| {
        format!(
            "the landing refused {}: {}",
            serde_json::to_value(&event)
                .ok()
                .and_then(|value| value["kind"].as_str().map(str::to_owned))
                .unwrap_or_default(),
            serde_json::to_value(rejection)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        )
    })?;
    // A halt and an abort that settles the effect first keep the effect
    // and what its intent recorded.
    let keeps = matches!(event, CoreEvent::Halted | CoreEvent::Abort);
    run.take_core(next, keeps.then_some(current));
    Ok(effects.into_iter().find_map(|effect| match effect {
        CoreEffect::Emit { event } => Some(event),
        _ => None,
    }))
}

/// What a reconcile found of an effect a gone driver began.
pub(super) enum Reconciliation {
    /// Done or failed, as git shows: its acknowledgement.
    Ack(Box<Reconcile>),
    /// Not done: it runs again (or, under an abort, is cancelled).
    Rerun,
    /// Git cannot show whether it was done now (origin cannot be read):
    /// it stays pending, never cancelled, with why.
    Unobserved(String),
}

/// An effect a gone driver began, acknowledged from what git shows: the
/// acknowledgement, why for a failure, the result the effect would have
/// recorded itself, and a pick's landing.
pub(super) struct Reconcile {
    event: CoreEvent,
    why: Option<String>,
    result: Vec<Value>,
    landed: Option<TodoLanding>,
}

/// What of the run is on the target branch.
enum Left {
    /// Nothing: none of its commits, or they were reverted.
    Nothing,
    /// A run recorded before herdr kept the branch's commit before its
    /// landing: its landed commit, with nothing telling where the branch
    /// was.
    Unrestorable { commit: String },
    /// Nothing tells how to take the run's commits off: the branch is gone,
    /// or its history was rewritten so it no longer contains the commit it
    /// was at before the landing. `commits` are the run's commits found by
    /// its trailers there, or else the ones it recorded.
    Unplannable { why: String, commits: Vec<String> },
    /// The run's commits (`ours`, oldest first) between `to`, where the
    /// branch was before the landing, and its head; `only_ours` when no
    /// other commit is among them.
    Commits {
        to: String,
        head: String,
        ours: Vec<String>,
        only_ours: bool,
    },
}

/// How the run's commits leave the target branch.
enum Restore {
    /// The branch has nothing of the run's left.
    Restored,
    /// A run recorded before herdr kept the branch's commit before its
    /// landing: nothing tells where to return the branch, so its commit
    /// stays.
    Kept { commit: String },
    /// No restore can be planned ([`Left::Unplannable`]): the branch is not
    /// moved, and the run's commits are left as they are.
    Left { why: String, commits: Vec<String> },
    /// Only the run's commits are on top of the commit the branch was at
    /// before the landing: back to that commit.
    Reset { to: String, head: String },
    /// Others committed on top of the run's commits: these are reverted in
    /// one commit on `head`.
    Revert { commits: Vec<String>, head: String },
    /// The push destination already has a commit of the run (its push was
    /// not acknowledged): it is published and stays.
    Published { commit: String, on: String },
}

impl WorkerSupervisor {
    /// Feeds `event` to the run's landing core and writes the new state with
    /// `notes` (what the effect recorded, each given the step it leaves the
    /// run at), the landing `landed` records and the event an `emit` raises,
    /// in one transaction. `why` is the failure that event names.
    pub(super) fn land(
        &self,
        run: &mut Run,
        event: CoreEvent,
        notes: Vec<Value>,
        why: Option<String>,
    ) -> Result<(), String> {
        self.land_with(run, event, notes, why, None)
    }

    /// As [`Self::land`]; the transition is applied to a copy of the run,
    /// which becomes the run only once the store committed it, so a failed
    /// write leaves the run as it was.
    fn land_with(
        &self,
        run: &mut Run,
        event: CoreEvent,
        mut notes: Vec<Value>,
        why: Option<String>,
        landed: Option<&TodoLanding>,
    ) -> Result<(), String> {
        let mut next = run.clone();
        let emit = feed(&mut next, event.clone())?;
        let emitted = emit.map(|kind| self.emitted(&mut next, kind, why.clone()));
        for note in &mut notes {
            note["step"] = json!(next.info.step);
        }
        if notes.is_empty() && emitted.is_none() {
            notes.push(json!({"type": "run_landing", "input": event, "step": next.info.step}));
        }
        let store = self.run_store().map_err(|error| error.to_string())?;
        let at = now_ms();
        store
            .transaction(|tx| {
                for note in &notes {
                    tx.run_event(&mut next, note, false, at)?;
                }
                if let Some(landing) = landed {
                    tx.landing(landing)?;
                }
                if let Some(event) = &emitted {
                    let pending = next.info.status == TodoRunStatus::Waiting;
                    let body = event_body(&next, event);
                    tx.run_event(&mut next, &body, pending, at)?;
                }
                Ok(())
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        *run = next;
        for note in &notes {
            super::activity::observe(&run.info, Some(note), run.info.updated_ms);
        }
        super::pending_usage::observe(run);
        announce();
        match emit {
            Some(TodoEventKind::Blocked) => {
                lock(&RUN_ENV).remove(&run.info.run_id);
                if run
                    .landing
                    .as_ref()
                    .and_then(|landing| landing.core.attention)
                    == Some(Attention::PushDiverged)
                {
                    crate::workers::notify_user(crate::workers::UserNotice {
                        pane_id: None,
                        title: format!(
                            "todo run {}: {} stopped before its push",
                            run.info.run_id, run.info.item
                        ),
                        body: why.unwrap_or_default(),
                    });
                }
            }
            Some(TodoEventKind::Done) => {
                let env = lock(&RUN_ENV).remove(&run.info.run_id);
                self.follow_up(&run.info.run_id, env);
            }
            _ => {}
        }
        Ok(())
    }

    /// The event an `emit` effect raises, with what the coordinator decides
    /// on; a blocked or aborted run's error is the event's.
    pub(super) fn emitted(
        &self,
        run: &mut Run,
        kind: TodoEventKind,
        why: Option<String>,
    ) -> TodoRunEvent {
        let mut event = new_event(kind);
        event.error = why;
        match kind {
            TodoEventKind::VerifyFailed => {
                event.verification = run
                    .current
                    .verification
                    .as_deref()
                    .and_then(|text| serde_json::from_str::<WorkerVerification>(text).ok());
                if let Some(worker) = run
                    .info
                    .worker_id
                    .as_deref()
                    .and_then(|id| self.status(id).ok())
                {
                    (event.diff_stat, event.commits) = Self::evidence(run, &worker);
                }
            }
            TodoEventKind::Blocked => run.info.error = event.error.clone(),
            TodoEventKind::Aborted => {
                let (why, commits) = self.abort_summary(run);
                run.info.error = Some(why.clone());
                event.error = Some(why);
                event.commits = commits;
            }
            TodoEventKind::LandingApproval => {
                let commit = run
                    .landing
                    .as_ref()
                    .and_then(|landing| landing.core.owner_approval.clone())
                    .unwrap_or_default();
                match self.landing_approval_event(run, &commit) {
                    Ok(asked) => event = asked,
                    Err(error) => {
                        event.commits = vec![commit.clone()];
                        event.error = Some(format!(
                            "the patch of commit {commit} cannot be shown ({error}): read it \
                             yourself before approve-landing, or abort the run"
                        ));
                    }
                }
            }
            // The run's branch moved while its owner was asked: the commit
            // that would land is reviewed again.
            TodoEventKind::Review => {
                let asked = run
                    .landing
                    .as_ref()
                    .and_then(|landing| landing.core.owner_approval.clone())
                    .unwrap_or_default();
                if let (Some(branch), Some(base)) = (run.info.branch.clone(), run.info.base.clone())
                {
                    let repo = PathBuf::from(&run.info.repo);
                    (event.diff_stat, event.commits) =
                        super::branch_evidence(&repo, &base, &format!("refs/heads/{branch}"));
                    let tip = event.commits.last().cloned().unwrap_or_default();
                    event.error = Some(format!(
                        "the owner's landing approval was asked for commit {asked}, but branch \
                         {branch} is at {tip} now; nothing was verified or landed: review it \
                         again"
                    ));
                }
            }
            TodoEventKind::Done => {
                event.commits = run
                    .info
                    .picked
                    .iter()
                    .chain(run.info.todo_commit.iter())
                    .cloned()
                    .collect();
            }
            _ => {}
        }
        event
    }

    /// The landing the run's approval starts, once its worker stopped: the
    /// branch must still be at the approved commit on the approved base (or
    /// the run raises a new `review`), and a landing in the repository
    /// rooted at the home directory waits for its owner's approval first.
    /// Then the core takes `approve`.
    pub(super) fn start_landing(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
        let branch = run.info.branch.clone().ok_or("the run has no branch")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let commit = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )?
        .trim()
        .to_owned();
        if run.current.review_decision.as_deref() == Some(APPROVE)
            && (run.current.commit.as_deref() != Some(commit.as_str())
                || run.current.base != run.info.base)
        {
            return self.review_again(run, &repo, &branch, &base, &commit);
        }
        let landing_dir = run.landing_dir();
        // A run that waited on its owner's approval before the core decided
        // it carries that approval here.
        let ask = super::super::home_repo::is_home_repository(&run.info.repo)
            && run.finish.landing_approved.as_deref() != Some(commit.as_str())
            && !applied(&landing_dir, &commit);
        // The run's fence is the one every input carries until the
        // coordinator tenure's epoch and pane generation are wired in.
        run.landing = Some(Landing {
            core: CoreState {
                step: TodoStep::Review,
                status: TodoRunStatus::Waiting,
                fence: Fence {
                    tenure_epoch: 0,
                    pane_generation: 0,
                },
                slot_held: true,
                pending: None,
                degraded: Vec::new(),
                attention: None,
                integrated: None,
                picked: None,
                pushed: None,
                cursor: None,
                abort_requested: false,
                reverifies: 0,
                owner_approval: None,
                push_skipped: None,
            },
            started: false,
            intent: None,
        });
        let started = json!({"type": "run_landing_started", "commit": commit, "base": base});
        if ask {
            // Landing in the home directory changes the user's live files:
            // the landing waits on a `landing_approval` event with the
            // whole patch and the backup and rollback note until its owner
            // approves that commit; nothing is verified or landed yet.
            return self.land(
                run,
                CoreEvent::AskLandingApproval { commit },
                vec![started],
                None,
            );
        }
        self.land(run, CoreEvent::Approve, vec![started], None)
    }

    /// The commit the landing verifies and lands: the one its owner
    /// approved in the repository rooted at the home directory, otherwise
    /// the one the run's branch is at.
    fn landing_commit(&self, run: &Run, branch: &str) -> Result<String, String> {
        if let Some(approved) = run
            .landing
            .as_ref()
            .and_then(|landing| landing.core.owner_approval.clone())
        {
            return Ok(approved);
        }
        Ok(git(
            Path::new(&run.info.repo),
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )?
        .trim()
        .to_owned())
    }

    /// Runs the effect the run's landing core waits for. One a driver began
    /// before it went away is reconciled from git first; one an abort waits
    /// to settle is settled, never run.
    pub(super) fn step_landing(&self, run: &mut Run) -> Result<(), String> {
        let landing = run
            .landing_state()
            .ok_or("the run has not started its landing")?;
        let pending = landing
            .core
            .pending
            .ok_or("the run's landing waits for no effect")?;
        run.landing = Some(landing.clone());
        if landing.core.abort_requested {
            return self.settle_pending(run, pending, landing.started);
        }
        if landing.started {
            // An effect git cannot settle now runs again: each reads what
            // it acts on itself first (the push reads origin).
            if let Reconciliation::Ack(reconciled) = self.reconcile_landing(run, pending)? {
                return self.land_reconciled(run, pending, reconciled);
            }
        }
        match pending {
            PendingEffect::VerifyIntegrated => self.effect_verify(run),
            PendingEffect::CherryPick => self.effect_land(run),
            PendingEffect::Install => self.step_install(run),
            PendingEffect::TodoEdit => self.step_todo(run),
            PendingEffect::Push => self.step_push(run),
            PendingEffect::RestoreBranch => self.effect_restore(run),
            PendingEffect::Cleanup => self.step_cleanup(run),
        }
    }

    fn land_reconciled(
        &self,
        run: &mut Run,
        pending: PendingEffect,
        reconciled: Box<Reconcile>,
    ) -> Result<(), String> {
        let mut notes = vec![json!({
            "type": "run_reconciled",
            "effect": pending,
            "ack": reconciled.event,
        })];
        notes.extend(reconciled.result);
        self.land_with(
            run,
            reconciled.event,
            notes,
            reconciled.why,
            reconciled.landed.as_ref(),
        )
    }

    /// Before an abort goes on: the effect a halted run left pending is
    /// acknowledged from what git shows when it began, otherwise (or when
    /// git shows it not done) cancelled. It is never run. One git cannot
    /// show done or not now (a push whose origin cannot be read) is never
    /// cancelled: it stays pending and the run stops again with why, for
    /// an abort once git can show it.
    fn settle_pending(
        &self,
        run: &mut Run,
        pending: PendingEffect,
        started: bool,
    ) -> Result<(), String> {
        if started {
            match self.reconcile_landing(run, pending)? {
                Reconciliation::Ack(reconciled) => {
                    return self.land_reconciled(run, pending, reconciled)
                }
                Reconciliation::Unobserved(why) => {
                    self.halt(run, why);
                    return Ok(());
                }
                Reconciliation::Rerun => {}
            }
        }
        let note = json!({"type": "run_cancelled", "effect": pending, "began": started});
        self.land(run, CoreEvent::Cancelled, vec![note], None)
    }

    /// Records the intent of the effect about to run, what its reconcile
    /// needs beside git, and that it may have begun from now on, in one
    /// write.
    pub(super) fn effect_intent(
        &self,
        run: &mut Run,
        event: Value,
        intent: Intent,
    ) -> Result<(), String> {
        if let Some(landing) = &mut run.landing {
            landing.started = true;
            landing.intent = Some(intent);
        }
        self.run_step(run, event)?;
        Ok(())
    }

    /// Stops the run where it stands with why: the landing core stops it
    /// when it is in its landing, keeping the effect it waits for (it may
    /// have begun); otherwise the run is blocked as before. The landing's
    /// halt starts from the run as the store has it, so nothing a failed
    /// write left in memory is written with it.
    pub(super) fn halt(&self, run: &mut Run, why: String) {
        if !run.in_landing() {
            return self.block_run(run, why);
        }
        let mut stored = self
            .load_run(&run.info.run_id)
            .unwrap_or_else(|_| run.clone());
        if !stored.in_landing() {
            *run = stored;
            return self.block_run(run, why);
        }
        tracing::warn!(run_id = %run.info.run_id, why, "todo run halted in its landing");
        match self.land(
            &mut stored,
            CoreEvent::Halted,
            Vec::new(),
            Some(why.clone()),
        ) {
            Ok(()) => *run = stored,
            Err(error) => {
                tracing::warn!(run_id = %run.info.run_id, error, "the landing did not take the halt");
                *run = stored;
                self.block_run(run, why);
            }
        }
    }

    /// What to do with the effect a driver began before it went away, from
    /// what git shows: the acknowledgement to feed (with why, for a
    /// failure), a rerun (or, under an abort, a cancellation), or that git
    /// cannot show it now. Recorded either way. A verify's folder slot goes
    /// back to where it was first, which is recorded before anything else;
    /// a TODO edit's files an older server wrote and did not commit are put
    /// back as `HEAD` has them (an error when they cannot be).
    pub(super) fn reconcile_landing(
        &self,
        run: &mut Run,
        pending: PendingEffect,
    ) -> Result<Reconciliation, String> {
        let state = run
            .landing_state()
            .ok_or("the run has not started its landing")?
            .core;
        let intent = run.intent();
        let env = lock(&RUN_ENV).get(&run.info.run_id).cloned();
        let mut observed = Observed {
            picked: None,
            installed: false,
            todo_committed: false,
            origin: Origin::Behind,
            restored: false,
        };
        let mut unobserved = None;
        let mut installed_build = None;
        let mut todo_commit = None;
        let mut put_back = Vec::new();
        match pending {
            PendingEffect::CherryPick => observed.picked = self.observed_pick(run)?,
            PendingEffect::VerifyIntegrated => {
                if let (Some(slot), Some(back)) = (&intent.slot, &intent.back) {
                    return_slot(Path::new(slot), back, intent.back_is_branch).map_err(|error| {
                        format!(
                            "the folder slot {slot} did not return to {back} after an \
                             interrupted verify: {error}; the run stops here"
                        )
                    })?;
                    // Recorded at once: a driver that ends from here on does
                    // not switch the slot again.
                    if let Some(intent) = run
                        .landing
                        .as_mut()
                        .and_then(|landing| landing.intent.as_mut())
                    {
                        intent.slot = None;
                        intent.back = None;
                    }
                    self.run_step(
                        run,
                        json!({"type": "run_slot_returned", "slot": slot, "back": back}),
                    )?;
                    #[cfg(test)]
                    super::crashes_at(&run.info.repo, "slot_returned")?;
                }
            }
            PendingEffect::Install => {
                if let (Some(env), Some(install)) = (&env, run.finish.install.clone()) {
                    if !install.build_id.is_empty() {
                        let target = run.target_commit()?;
                        installed_build = super::finish::installed_build(
                            &run.landing_dir(),
                            &install.build_id,
                            env,
                        )
                        .ok()
                        .filter(|build| super::finish::build_is(build, &target));
                    }
                }
                observed.installed = installed_build.is_some();
            }
            PendingEffect::TodoEdit => {
                todo_commit = self.todo_commit_of(run)?;
                observed.todo_committed = todo_commit.is_some();
                if todo_commit.is_none() {
                    put_back = put_back_todo_files(&run.landing_dir(), &intent.todo_files)?;
                }
            }
            PendingEffect::Push => {
                let sends = match intent.sends.clone() {
                    Some(sends) => sends,
                    None => run.target_commit()?,
                };
                match self.observed_origin(run, env.as_ref(), &sends) {
                    Ok(origin) => observed.origin = origin,
                    Err(why) => unobserved = Some(why),
                }
            }
            PendingEffect::RestoreBranch => {
                if let (Some(from), Some(to)) = (&intent.restore_from, &intent.restore_to) {
                    finish_restore_switch(&run.landing_dir(), run.target_branch(), from, to)?;
                }
                observed.restored = matches!(self.left_on_branch(run)?, Left::Nothing)
            }
            PendingEffect::Cleanup => {}
        }
        let decided = match unobserved {
            Some(_) => None,
            None => Some(core::reconcile(&state, &observed)),
        };
        let (outcome, ack) = match &decided {
            Some(Reconciled::Ack { event }) => ("ack", Some(event.clone())),
            Some(Reconciled::Rerun { .. }) => ("rerun", None),
            Some(Reconciled::Nothing) => ("nothing", None),
            None => ("unobserved", None),
        };
        if ack.is_none() {
            self.run_note(
                &run.info.run_id,
                json!({
                    "type": "run_reconciled",
                    "effect": pending,
                    "outcome": outcome,
                    "unobserved": unobserved,
                }),
            );
        }
        // What the acknowledged effect did, recorded as its own result would
        // have recorded it, a landing with its record.
        let mut result = Vec::new();
        let mut landed = None;
        match &ack {
            Some(CoreEvent::Picked { sha }) => {
                let worker_commit = run.info.branch.as_deref().and_then(|branch| {
                    git(
                        Path::new(&run.info.repo),
                        &[
                            "rev-parse",
                            "--verify",
                            &format!("refs/heads/{branch}^{{commit}}"),
                        ],
                    )
                    .ok()
                    .map(|sha| sha.trim().to_owned())
                });
                result.push(json!({
                    "type": "run_picked",
                    "commit": sha,
                    "worker_commit": worker_commit,
                    "trailers": true,
                    "already": true,
                }));
                landed = Some(self.landing_record(run, sha, worker_commit));
            }
            Some(CoreEvent::Installed) => {
                run.info.installed_build = installed_build.clone();
                result.push(json!({
                    "type": "run_installed",
                    "build": installed_build,
                    "already": true,
                }));
            }
            Some(CoreEvent::Edited) => {
                if let (Some(commit), Some((message, _))) =
                    (todo_commit.clone(), self.todo_message(run))
                {
                    result.push(self.todo_committed_note(run, commit, &message, true));
                }
            }
            Some(CoreEvent::PushAcked { sha }) => {
                run.info.pushed = Some(sha.clone());
                result.push(json!({
                    "type": "run_pushed",
                    "target": sha,
                    "origin_before": null,
                    "already": true,
                }));
            }
            Some(CoreEvent::BranchRestored) => {
                result.push(json!({"type": "run_restored", "already": true}));
            }
            _ => {}
        }
        let why = match &ack {
            Some(CoreEvent::InstallFailed) => Some(
                "the install began before the driver that ran it went away, and the installed \
                 build is not the target branch's (or no build_id names it): it is not run \
                 again by itself; retry-install installs again, skip-install goes on without it"
                    .to_owned(),
            ),
            Some(CoreEvent::TodoFailed) => Some(format!(
                "the TODO edit began before the driver that ran it went away, and its commit is \
                 not on the target branch{}: retry-todo edits again, skip-todo goes on without it",
                if put_back.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the files it wrote ({}) were put back as HEAD has them",
                        put_back.join(", ")
                    )
                }
            )),
            Some(CoreEvent::PushRejected { diverged: true }) => {
                Some(self.diverged_why(run, "the push destination's branch", None))
            }
            _ => None,
        };
        if let Some(why) = unobserved {
            return Ok(Reconciliation::Unobserved(format!(
                "the push began before the driver that ran it went away, and herdr cannot tell \
                 whether origin has the commit it sent ({why}): the push stays pending and \
                 nothing is taken off {}; abort again once origin can be read",
                run.target_branch()
            )));
        }
        Ok(match ack {
            Some(event) => Reconciliation::Ack(Box::new(Reconcile {
                event,
                why,
                result,
                landed,
            })),
            None => Reconciliation::Rerun,
        })
    }

    /// The landing of `landed` as the store records it.
    fn landing_record(
        &self,
        run: &Run,
        landed: &str,
        worker_commit: Option<String>,
    ) -> TodoLanding {
        TodoLanding {
            landed_sha: landed.to_owned(),
            run_id: run.info.run_id.clone(),
            attempt: run.info.attempt,
            item: run.info.item.clone(),
            worker_commit,
            worker_id: run.info.worker_id.clone(),
            ts_ms: Some(now_ms()),
            source: TodoLandingSource::Store,
        }
    }

    /// The commit on the target branch since the landing's verified parent
    /// (or the base) that carries this run's trailers.
    fn observed_pick(&self, run: &Run) -> Result<Option<String>, String> {
        let since = run
            .finish
            .restore_to
            .clone()
            .or_else(|| run.info.base.clone())
            .ok_or("the run has no base")?;
        let repo = run.landing_dir();
        let commits = git(
            &repo,
            &[
                "rev-list",
                &format!("{since}..refs/heads/{}", run.target_branch()),
            ],
        )?;
        let trailer = run_trailer(&run.info);
        Ok(commits.lines().map(str::trim).find_map(|sha| {
            landing_trailers(&repo, sha)
                .filter(|(_, run_id, attempt)| format!("{run_id}/{attempt}") == trailer)
                .map(|_| sha.to_owned())
        }))
    }

    /// Origin's target branch against `sends`, the commit the push sends,
    /// read with the caller's environment; an error when it cannot be read.
    fn observed_origin(
        &self,
        run: &Run,
        env: Option<&std::collections::HashMap<String, String>>,
        sends: &str,
    ) -> Result<Origin, String> {
        let env = env.ok_or("this server has not got the caller's environment")?;
        let repo = run.landing_dir();
        let theirs =
            origin_head(run, &repo, env)?.ok_or("the push destination's branch cannot be found")?;
        if theirs == sends {
            return Ok(Origin::Contains {
                sha: sends.to_owned(),
            });
        }
        if is_ancestor(&repo, sends, &theirs) {
            return Ok(Origin::Contains {
                sha: sends.to_owned(),
            });
        }
        if is_ancestor(&repo, &theirs, sends) {
            return Ok(Origin::Behind);
        }
        Ok(Origin::Diverged)
    }

    /// Why a push was refused for a diverged origin: `theirs` names the
    /// remote branch, `at` its commit when known.
    pub(super) fn diverged_why(&self, run: &Run, theirs: &str, at: Option<&str>) -> String {
        let branch = run.target_branch();
        let at = at.map(|sha| format!(" {sha}")).unwrap_or_default();
        format!(
            "{theirs}{at} is not an ancestor of {branch}: the push would not be a fast-forward, \
             and herdr never forces or rebases it. The run stops here for you: bring {branch} \
             and {theirs} together by hand, or abort the run, which takes its commit off \
             {branch}"
        )
    }

    /// Verifies the approved commit on the integrated tree without touching
    /// the target branch: the commit is picked onto the target branch's
    /// head as a commit of its own (`git merge-tree`, then `git
    /// commit-tree`; the integration commit, with the run's subject alone),
    /// the attempt's worker directory (the folder slot, its build warm) is
    /// switched to it, and the run's checks run there as its argv; then the
    /// directory goes back to where it was, recorded with the intent. A
    /// verdict the automatic review's verify already reached on the same
    /// tree with every check the run has is taken instead of running the
    /// checks again. It fails closed: a slot that does not switch, or is
    /// not exactly at the integration commit afterwards, is a failed verify
    /// with the reason, never a verify of something else; a slot that does
    /// not go back halts the run. A commit that does not pick onto the head
    /// stops the run.
    fn effect_verify(&self, run: &mut Run) -> Result<(), String> {
        let worker_id = run
            .info
            .worker_id
            .clone()
            .ok_or("the run has no worker to verify")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let branch = run.info.branch.clone().ok_or("the run has no branch")?;
        for (check, reason) in self.path_checks_due(run, &worker_id, &base)? {
            run.info.checks.push(check.name.clone());
            run.checks.push(check.clone());
            self.run_step(
                run,
                json!({
                    "type": "run_check_added",
                    "check": check,
                    "reason": reason,
                    "checks": run.info.checks,
                }),
            )?;
        }
        let worker_commit = self.landing_commit(run, &branch)?;
        let repo = run.landing_dir();
        let target = run.target_branch().to_owned();
        let onto = run.target_commit()?;
        let integrated = match super::carry_commit(&repo, &onto, &worker_commit)? {
            Ok(integrated) => integrated,
            Err(conflict) => {
                let why = format!(
                    "the approved commit {worker_commit} does not pick onto {target}'s head \
                     {onto} ({conflict}); nothing was verified or landed"
                );
                let note = json!({
                    "type": "run_integration_conflict",
                    "commit": worker_commit,
                    "onto": onto,
                });
                return self.land(run, CoreEvent::IntegrationConflict, vec![note], Some(why));
            }
        };
        let tree = git(
            &repo,
            &["rev-parse", "--verify", &format!("{integrated}^{{tree}}")],
        )?
        .trim()
        .to_owned();
        if let Some(verified) = self.verified_already(run, &worker_id, &tree) {
            let note = json!({
                "type": "run_verified",
                "head": verified.head,
                "commits": verified.commits,
                "integrated": integrated,
                "onto": onto,
                "reused": true,
                "verified_ms": verified.verified_ms,
            });
            return self.land(run, CoreEvent::Verified { integrated }, vec![note], None);
        }
        let worker = self.status(&worker_id).map_err(|error| error.to_string())?;
        let dir = PathBuf::from(&worker.cwd);
        let (back, back_is_branch) =
            match git(&dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
                Ok(name) => (name.trim().to_owned(), true),
                Err(_) => (git(&dir, &["rev-parse", "HEAD"])?.trim().to_owned(), false),
            };
        self.effect_intent(
            run,
            json!({
                "type": "run_verify_intent",
                "worker_id": worker_id,
                "checks": run.info.checks,
                "commit": worker_commit,
                "onto": onto,
                "integrated": integrated,
                "slot": dir.display().to_string(),
                "back": back,
            }),
            Intent {
                slot: Some(dir.display().to_string()),
                back: Some(back.clone()),
                back_is_branch,
                ..Intent::default()
            },
        )?;
        let switched = git(&dir, &["switch", "--quiet", "--detach", &integrated]);
        #[cfg(test)]
        super::crashes_at(&run.info.repo, "verify_switched")?;
        let at = git(&dir, &["rev-parse", "HEAD"]).map(|head| head.trim().to_owned());
        let refused = match (&switched, &at) {
            (Err(error), _) => Some(format!(
                "the folder slot {} did not switch to the integration commit {integrated}: \
                 {error}",
                dir.display()
            )),
            (Ok(_), Err(error)) => Some(format!(
                "the folder slot {}'s HEAD cannot be read after the switch: {error}",
                dir.display()
            )),
            (Ok(_), Ok(head)) if *head != integrated => Some(format!(
                "the folder slot {} is at {head} after the switch, not at the integration \
                 commit {integrated}",
                dir.display()
            )),
            _ => None,
        };
        if let Some(why) = refused {
            return_slot(&dir, &back, back_is_branch).map_err(|error| {
                format!("{why}; it did not return to {back} either: {error}; the run stops here")
            })?;
            run.current.verification = None;
            let note = json!({
                "type": "run_verify_failed",
                "integrated": integrated,
                "onto": onto,
                "switched": false,
                "error": why,
            });
            return self.land(
                run,
                CoreEvent::VerifyFailed,
                vec![note],
                Some(format!("{why}; nothing was verified, nothing landed")),
            );
        }
        let verification = self.verify_attempt(run, &worker_id, &onto);
        return_slot(&dir, &back, back_is_branch).map_err(|error| {
            format!(
                "the folder slot {} did not return to {back} after the verify: {error}; the \
                 run stops here",
                dir.display()
            )
        })?;
        let verification = verification?;
        run.current.verification = serde_json::to_string(&verification).ok();
        let verified = verification.verdict == WorkerVerdict::Verified
            && verification.head.as_deref() == Some(integrated.as_str());
        let note = json!({
            "type": if verified { "run_verified" } else { "run_verify_failed" },
            "head": verification.head,
            "commits": verification.commits,
            "verdict": verification.verdict,
            "integrated": integrated,
            "onto": onto,
            "switched": true,
        });
        let event = if verified {
            CoreEvent::Verified { integrated }
        } else {
            CoreEvent::VerifyFailed
        };
        self.land(run, event, vec![note], None)
    }

    /// The automatic review's verdict when it still holds for the landing:
    /// `verified`, against the run's base, of a commit with the integrated
    /// tree `tree`, with every check the run has now.
    fn verified_already(
        &self,
        run: &Run,
        worker_id: &str,
        tree: &str,
    ) -> Option<WorkerVerification> {
        if run.current.review_decision.as_deref() != Some(APPROVE) {
            return None;
        }
        let verification: WorkerVerification =
            serde_json::from_str(run.current.verification.as_deref()?).ok()?;
        let head = verification.head.as_deref()?;
        if verification.verdict != WorkerVerdict::Verified
            || run.current.commit.as_deref() != Some(head)
            || Some(verification.base.as_str()) != run.info.base.as_deref()
        {
            return None;
        }
        let checked = |name: &str| {
            verification
                .checks
                .iter()
                .any(|check| check.name.as_deref() == Some(name))
        };
        if !run.checks.iter().all(|check| checked(&check.name)) {
            return None;
        }
        let worker = self.status(worker_id).ok()?;
        let verified_tree = git(
            Path::new(&worker.cwd),
            &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
        )
        .ok()?;
        (verified_tree.trim() == tree).then_some(verification)
    }

    /// Lands the verified integration commit's tree on the target branch
    /// ([`Run::target_branch`]) in the worktree the run recorded
    /// ([`Run::landing_dir`]), as a commit carrying the `Herdr-Item` and
    /// `Herdr-Run` trailers from the start, and records the landing. It
    /// lands only while the branch's head is the verified parent: checked
    /// right before a fast-forward of the worktree (`git merge --ff-only`,
    /// which refuses to touch the worktree's local changes); a head that
    /// moved lands nothing and is verified again. Refused (`pick_failed`,
    /// the run stops) when that worktree is not on the branch, has
    /// uncommitted changes or a cherry-pick in progress, the branch no
    /// longer contains the base, or the private details guard refuses the
    /// commit. A commit with the run's trailers the branch already has (a
    /// landing a crash did not record) is not landed again. A commit
    /// without them that already carries the approved change (a person
    /// picked it) is not the run's: the run stops for the user, naming it,
    /// and never records, resets or reverts it. The verified parent is
    /// recorded with the intent: the restore returns the branch there.
    fn effect_land(&self, run: &mut Run) -> Result<(), String> {
        let branch = run.info.branch.clone().ok_or("the run has no branch")?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let integrated = run
            .landing
            .as_ref()
            .and_then(|landing| landing.core.integrated.clone())
            .ok_or("the landing has no verified commit")?;
        let worker_commit = self.landing_commit(run, &branch)?;
        let target = run.target_branch().to_owned();
        run.target_commit()?;
        let repo = run.landing_dir();
        if let Some(landed) = self.observed_pick(run)? {
            let note = json!({
                "type": "run_picked",
                "commit": landed,
                "worker_commit": worker_commit,
                "trailers": true,
                "already": true,
            });
            let record = self.landing_record(run, &landed, Some(worker_commit));
            return self.land_with(
                run,
                CoreEvent::Picked { sha: landed },
                vec![note],
                None,
                Some(&record),
            );
        }
        // The change is on the branch by other hands (a person picked it):
        // that commit is not the run's, so it is neither landed again nor
        // taken for the run's (an abort would take it off); the user decides.
        if let Some(theirs) = patch_equivalent(&repo, &target, &base, &worker_commit) {
            let why = format!(
                "{target} already has the approved change of {worker_commit} as commit {theirs}, \
                 which is not the run's: nothing was landed, and that commit is left as it is. \
                 The run stops here for you: abort it, which leaves {theirs} on {target}"
            );
            let note = json!({
                "type": "run_change_on_branch",
                "commit": theirs,
                "worker_commit": worker_commit,
            });
            return self.land(run, CoreEvent::PickFailed, vec![note], Some(why));
        }
        // The recorded worktree, rechecked right before the landing: still
        // on the target branch, clean, and the branch still has the base.
        if let Err(why) = super::super::target_branch::check_landing(&repo, &target, &base) {
            let why = format!("{why}; commit {worker_commit} was not landed");
            return self.land(run, CoreEvent::PickFailed, Vec::new(), Some(why));
        }
        let onto = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{integrated}^{{commit}}^"),
            ],
        )?
        .trim()
        .to_owned();
        let head = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        if head != onto {
            return self.head_moved(run, &onto, &head);
        }
        if let Err(why) = check_private_details(&repo, &onto, &integrated, None) {
            return self.land(run, CoreEvent::PickFailed, Vec::new(), Some(why));
        }
        let message = git(&repo, &["log", "-1", "--format=%B", &integrated])?;
        let message = format!(
            "{}\n\nHerdr-Item: {}\nHerdr-Run: {}\n",
            message.trim_end(),
            run.info.item,
            run_trailer(&run.info)
        );
        let tree = format!("{integrated}^{{tree}}");
        let landed = commit_like(&repo, &integrated, &tree, &onto, &message)?;
        run.finish.restore_to = Some(onto.clone());
        self.effect_intent(
            run,
            json!({
                "type": "run_cherry_pick_intent",
                "commit": worker_commit,
                "integrated": integrated,
                "landed": landed,
                "target": onto,
            }),
            Intent::default(),
        )?;
        #[cfg(test)]
        super::run_hook(&run.info.repo, "before_land");
        if let Err(moved) = fast_forward(&repo, &target, &onto, &landed, &run.info.repo)? {
            return self.head_moved(run, &onto, &moved);
        }
        #[cfg(test)]
        super::crashes_at(&run.info.repo, "landed")?;
        let record = self.landing_record(run, &landed, Some(worker_commit.clone()));
        let note = json!({
            "type": "run_picked",
            "commit": landed,
            "worker_commit": worker_commit,
            "integrated": integrated,
            "trailers": true,
        });
        self.land_with(
            run,
            CoreEvent::Picked { sha: landed },
            vec![note],
            None,
            Some(&record),
        )
    }

    /// The target branch's head is no longer the verified parent: nothing
    /// landed, so there is nothing to return the branch to, and the commit
    /// is verified again on the new head, at most
    /// [`core::MAX_REVERIFIES`] times for one landing; then the run stops
    /// for the user with why.
    fn head_moved(&self, run: &mut Run, onto: &str, head: &str) -> Result<(), String> {
        run.finish.restore_to = None;
        let note = json!({
            "type": "run_head_moved",
            "verified_on": onto,
            "head": head,
        });
        let why = format!(
            "{} kept moving: its head moved before the landing after each of {} verifies \
             again (now at {head}); nothing was landed. The run stops here for you: abort it, \
             or approve the item again in a new run once the branch is quiet",
            run.target_branch(),
            core::MAX_REVERIFIES
        );
        self.land(run, CoreEvent::HeadMoved, vec![note], Some(why))
    }

    /// What of the run is on the target branch now. The run's commits are
    /// those with its trailers and the ones it recorded (its landed and
    /// TODO commits), never a commit matched by its subject or its change.
    fn left_on_branch(&self, run: &Run) -> Result<Left, String> {
        let picked = run
            .landing
            .as_ref()
            .and_then(|landing| landing.core.picked.clone())
            .or_else(|| run.info.picked.clone());
        let Some(to) = run.finish.restore_to.clone() else {
            return Ok(match picked {
                None => Left::Nothing,
                Some(commit) => Left::Unrestorable { commit },
            });
        };
        let repo = run.landing_dir();
        let branch = run.target_branch();
        let recorded = || -> Vec<String> {
            picked
                .iter()
                .chain(run.info.todo_commit.iter())
                .cloned()
                .collect()
        };
        let head = match run.target_commit() {
            Ok(head) => head,
            Err(why) => {
                return Ok(Left::Unplannable {
                    why,
                    commits: recorded(),
                })
            }
        };
        if !is_ancestor(&repo, &to, &head) {
            let found: Vec<String> =
                git(&repo, &["rev-list", "--reverse", &format!("{to}..{head}")])
                    .unwrap_or_default()
                    .lines()
                    .map(str::trim)
                    .filter(|sha| {
                        landing_trailers(&repo, sha)
                            .is_some_and(|(_, run_id, _)| run_id == run.info.run_id)
                    })
                    .map(str::to_owned)
                    .collect();
            return Ok(Left::Unplannable {
                why: format!(
                    "{branch} no longer contains {to}, where it was before the run's landing: \
                     its history was rewritten"
                ),
                commits: if found.is_empty() { recorded() } else { found },
            });
        }
        let listed = git(&repo, &["rev-list", "--reverse", &format!("{to}..{head}")])?;
        let commits: Vec<String> = listed.lines().map(|sha| sha.trim().to_owned()).collect();
        let trailer = run_trailer(&run.info);
        if commits
            .iter()
            .any(|sha| trailer_value(&repo, sha, REVERT_TRAILER).as_deref() == Some(&trailer))
        {
            return Ok(Left::Nothing);
        }
        let ours: Vec<String> = commits
            .iter()
            .filter(|sha| {
                Some(sha.as_str()) == picked.as_deref()
                    || Some(sha.as_str()) == run.info.todo_commit.as_deref()
                    || landing_trailers(&repo, sha).is_some_and(|(_, run_id, attempt)| {
                        format!("{run_id}/{attempt}") == trailer
                    })
            })
            .cloned()
            .collect();
        if ours.is_empty() {
            return Ok(Left::Nothing);
        }
        Ok(Left::Commits {
            to,
            head,
            only_ours: ours.len() == commits.len(),
            ours,
        })
    }

    /// How the run's commits leave the target branch now. Before taking a
    /// commit off, origin itself is read (`git ls-remote`, its commit
    /// fetched when unknown here), not the remote-tracking branch: a commit
    /// origin has is published (a push whose acknowledgement was lost) and
    /// stays. Origin that cannot be read is an error: nothing is taken off
    /// on a guess.
    fn restore_plan(&self, run: &Run) -> Result<Restore, String> {
        let (to, head, ours, only_ours) = match self.left_on_branch(run)? {
            Left::Nothing => return Ok(Restore::Restored),
            Left::Unrestorable { commit } => return Ok(Restore::Kept { commit }),
            Left::Unplannable { why, commits } => return Ok(Restore::Left { why, commits }),
            Left::Commits {
                to,
                head,
                ours,
                only_ours,
            } => (to, head, ours, only_ours),
        };
        let repo = run.landing_dir();
        let plan = Self::push_plan(run, &repo);
        if let (Some(remote), Some(dst)) = (plan.remote, plan.dst_ref) {
            let unreadable = |why: String| {
                format!(
                    "herdr cannot tell whether {remote}'s {dst} has the run's commits ({why}): \
                     nothing was taken off {}; abort again once origin can be read",
                    run.target_branch()
                )
            };
            let env = lock(&RUN_ENV)
                .get(&run.info.run_id)
                .cloned()
                .ok_or_else(|| {
                    unreadable("this server has not got the caller's environment".into())
                })?;
            if let Some(theirs) = origin_head(run, &repo, &env).map_err(unreadable)? {
                if let Some(published) = ours.iter().find(|sha| is_ancestor(&repo, sha, &theirs)) {
                    return Ok(Restore::Published {
                        commit: published.clone(),
                        on: format!("{remote}'s {dst}"),
                    });
                }
            }
        }
        if only_ours {
            return Ok(Restore::Reset { to, head });
        }
        Ok(Restore::Revert {
            commits: ours,
            head,
        })
    }

    /// Takes the run's commits off the target branch in the landing
    /// worktree: back to where the branch was before the landing when only
    /// the run's commits are on top of it, otherwise one commit that
    /// reverts them on the branch's head (with a `Herdr-Revert` trailer
    /// naming the run), fast-forwarded onto it, so nobody else's commit is
    /// lost. The reset is a compare-and-swap on the branch's ref (`git
    /// update-ref <branch> <to> <head>`, refused unless the branch is at
    /// the head the plan read), then the worktree and index follow it (`git
    /// read-tree -m -u`, which keeps the worktree's other changes and
    /// refuses to touch one of them, checked before the ref moves). A
    /// refused swap is planned again (a commit that arrived is reverted
    /// around, never reset over). A commit the push destination already has
    /// stays (its push went through unacknowledged), and so does the commit
    /// of a run recorded before herdr kept where the branch was. A refusal
    /// halts the run; the next abort tries again.
    fn effect_restore(&self, run: &mut Run) -> Result<(), String> {
        let repo = run.landing_dir();
        let branch = run.target_branch().to_owned();
        // The head moves at most once between a plan and its swap here in
        // practice; a branch that keeps moving halts the run instead.
        for _ in 0..3 {
            let note = match self.restore_plan(run)? {
                Restore::Restored => json!({"type": "run_restored", "already": true}),
                Restore::Kept { commit } => json!({
                    "type": "run_restored",
                    "kept": commit,
                    "how": "kept",
                    "why": "the run was recorded before herdr kept the branch's commit before \
                            its landing",
                }),
                Restore::Left { why, commits } => json!({
                    "type": "run_restored",
                    "how": "left",
                    "why": why,
                    "left": commits,
                }),
                Restore::Published { commit, on } => {
                    run.info.pushed = Some(commit.clone());
                    json!({"type": "run_restored", "kept": commit, "on": on, "how": "published"})
                }
                Restore::Reset { to, head } => {
                    on_branch(&repo, &branch)?;
                    self.effect_intent(
                        run,
                        json!({"type": "run_restore_intent", "to": to, "head": head}),
                        Intent {
                            restore_from: Some(head.clone()),
                            restore_to: Some(to.clone()),
                            ..Intent::default()
                        },
                    )?;
                    #[cfg(test)]
                    super::run_hook(&run.info.repo, "restore_planned");
                    if move_branch(&repo, &branch, &head, &to, &run.info.repo)?.is_err() {
                        continue;
                    }
                    json!({"type": "run_restored", "to": to, "from": head, "how": "reset"})
                }
                Restore::Revert { commits, head } => {
                    on_branch(&repo, &branch)?;
                    self.effect_intent(
                        run,
                        json!({"type": "run_restore_intent", "revert": commits, "head": head}),
                        Intent::default(),
                    )?;
                    #[cfg(test)]
                    super::run_hook(&run.info.repo, "restore_planned");
                    let revert = revert_commit(&repo, &head, &commits, run)?;
                    if fast_forward(&repo, &branch, &head, &revert, &run.info.repo)?.is_err() {
                        continue;
                    }
                    json!({
                        "type": "run_restored",
                        "reverted": commits,
                        "revert": revert,
                        "how": "revert",
                    })
                }
            };
            return self.land(run, CoreEvent::BranchRestored, vec![note], None);
        }
        Err(format!(
            "{branch} kept moving while the run's commits were taken off it; nothing was reset \
             over another commit, and the next abort tries again"
        ))
    }

    /// Why the run aborted and the commits it leaves: the coordinator's
    /// reason, its attempts' branches as they are, and what became of a
    /// commit it landed.
    pub(super) fn abort_summary(&self, run: &Run) -> (String, Vec<String>) {
        let repo = PathBuf::from(&run.info.repo);
        let mut branches: Vec<String> = (1..=run.info.attempt)
            .map(|attempt| super::branch_of(&run.info.item, &run.info.run_id, attempt))
            .chain(run.info.branch.clone())
            .collect();
        branches.dedup();
        let mut left = Vec::new();
        let mut commits = Vec::new();
        for branch in branches {
            let Ok(head) = git(
                &repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}^{{commit}}"),
                ],
            ) else {
                continue;
            };
            let since_base = run.info.base.as_deref().and_then(|base| {
                git(
                    &repo,
                    &["rev-list", "--reverse", &format!("{base}..{branch}")],
                )
                .ok()
            });
            let own: Vec<String> = since_base
                .as_deref()
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            left.push(format!(
                "{branch} at {} ({} commit{} since the base)",
                &head.trim()[..head.trim().len().min(12)],
                own.len(),
                if own.len() == 1 { "" } else { "s" }
            ));
            for commit in own {
                if !commits.contains(&commit) {
                    commits.push(commit);
                }
            }
        }
        let mut why = run
            .info
            .error
            .clone()
            .unwrap_or_else(|| "the coordinator aborted the run".to_owned());
        if left.is_empty() {
            why.push_str("; the run has no branch left");
        } else {
            why.push_str(&format!("; branches left as they are: {}", left.join(", ")));
        }
        let branch = run.target_branch();
        // A landing state that does not parse took no landing effect on
        // the abort: the run's commits (by its trailers) stay where they are.
        if run.landing_invalid.is_some() {
            let repo = run.landing_dir();
            let ours: Vec<String> = run
                .info
                .base
                .as_deref()
                .and_then(|base| {
                    git(
                        &repo,
                        &[
                            "rev-list",
                            "--reverse",
                            &format!("{base}..refs/heads/{branch}"),
                        ],
                    )
                    .ok()
                })
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|sha| {
                    landing_trailers(&repo, sha)
                        .is_some_and(|(_, run_id, _)| run_id == run.info.run_id)
                })
                .map(str::to_owned)
                .collect();
            if ours.is_empty() {
                why.push_str(&format!(
                    "; its landing state does not parse, so nothing was taken off {branch}, \
                     which has no commit with its trailers"
                ));
            } else {
                why.push_str(&format!(
                    "; its landing state does not parse, so nothing was taken off {branch}: its \
                     commits there were left as they are: {}",
                    ours.join(", ")
                ));
            }
            return (why, commits);
        }
        let landing = run.landing.as_ref().map(|landing| &landing.core);
        let published = landing
            .and_then(|core| core.pushed.as_deref())
            .or(run.info.pushed.as_deref());
        // A restore that could not be planned moved nothing: the run's
        // commits are left as they are.
        if landing.is_some() && published.is_none() && run.finish.restore_to.is_some() {
            if let Ok(Left::Unplannable {
                why: cause,
                commits: ours,
            }) = self.left_on_branch(run)
            {
                why.push_str(&format!("; {cause}, so nothing was taken off {branch}: "));
                if ours.is_empty() {
                    why.push_str("the run has no commit there");
                } else {
                    why.push_str(&format!(
                        "its commits were left as they are: {}",
                        ours.join(", ")
                    ));
                }
                return (why, commits);
            }
        }
        match (landing, &run.info.picked) {
            (Some(_), _) if published.is_some() => why.push_str(&format!(
                "; commit {} stays on {branch} and was pushed",
                published.unwrap_or_default(),
            )),
            (Some(_), Some(picked)) if run.finish.restore_to.is_none() => why.push_str(&format!(
                "; commit {picked} stays on {branch}: the run was recorded before herdr kept \
                 where {branch} was before its landing, so it was not taken off"
            )),
            (Some(_), Some(picked)) => {
                why.push_str(&format!("; commit {picked} was taken off {branch}"))
            }
            (Some(_), None) => {}
            (None, Some(picked)) => why.push_str(&format!("; commit {picked} stays on {branch}")),
            (None, None) => {}
        }
        (why, commits)
    }

    /// Asks the coordinator's runs whose effect began under a driver that
    /// is gone to be driven again, reconciled from git: after a
    /// coordinator handoff, whose successor takes them over. A run another
    /// driver of this process holds is looked at again by that driver.
    pub(in crate::workers) fn reconcile_runs_of(&self, coordinators: &[&str]) {
        let runs = match self.run_store().map(|store| store.runs_in_progress()) {
            Ok(Ok(runs)) => runs,
            Ok(Err(error)) => {
                tracing::warn!(%error, "cannot read the todo runs to reconcile");
                return;
            }
            Err(_) => return,
        };
        for run in runs {
            let owned = run
                .owner_coordinator
                .as_deref()
                .is_some_and(|owner| coordinators.contains(&owner));
            let began = run.landing_state().is_some_and(|landing| landing.started);
            if owned && began && run.info.status == TodoRunStatus::Running {
                self.spawn_driver(&run.info.run_id);
            }
        }
    }

    /// The run's TODO commit on the target branch: the one after its landed
    /// commit that carries its `Herdr-Run` trailer, never one matched by
    /// its subject. None before the landing.
    pub(super) fn todo_commit_of(&self, run: &Run) -> Result<Option<String>, String> {
        let picked = run
            .landing
            .as_ref()
            .and_then(|landing| landing.core.picked.clone())
            .or_else(|| run.info.picked.clone());
        let Some(picked) = picked else {
            return Ok(None);
        };
        let repo = run.landing_dir();
        let commits = git(
            &repo,
            &[
                "rev-list",
                &format!("{picked}..refs/heads/{}", run.target_branch()),
            ],
        )?;
        let trailer = run_trailer(&run.info);
        Ok(commits.lines().map(str::trim).find_map(|sha| {
            landing_trailers(&repo, sha)
                .filter(|(_, run_id, attempt)| format!("{run_id}/{attempt}") == trailer)
                .map(|_| sha.to_owned())
        }))
    }

    /// The TODO step's commit message and files: a note's or a close's;
    /// none without either.
    pub(super) fn todo_message(&self, run: &Run) -> Option<(String, Vec<&'static str>)> {
        let item = &run.info.item;
        match (&run.finish.note, &run.finish.close) {
            (None, None) => None,
            (Some(_), _) => Some((
                format!("docs(todo): note on {item}"),
                vec![super::TODO_FILE],
            )),
            (None, Some(_)) => Some((
                format!("docs(todo): close {item}"),
                vec![super::TODO_FILE, super::DECISIONS_FILE],
            )),
        }
    }
}

/// A run's stored event for a coordinator-facing event: what
/// [`WorkerSupervisor::record_run_event`] and the landing write.
pub(super) fn event_body(run: &Run, event: &TodoRunEvent) -> Value {
    json!({
        "type": "run_event",
        "kind": event.kind,
        "step": run.info.step,
        "status": run.info.status,
        "attempt": run.info.attempt,
        "event": event,
    })
}

/// Whether the target branch checked out in `repo` already has `commit`
/// itself (a pick made in the same second reproduces its sha), or its
/// change: `git cherry` marks that `-`.
fn applied(repo: &Path, commit: &str) -> bool {
    let parent = format!("{commit}^");
    git(repo, &["merge-base", "--is-ancestor", commit, "HEAD"]).is_ok()
        || git(repo, &["cherry", "HEAD", commit, &parent])
            .is_ok_and(|out| out.lines().any(|line| line.starts_with("- ")))
}

/// The commit on `branch` since `base` that is `commit` itself or carries
/// its change (`git cherry` marks it `-`), the newest such: named, never
/// taken for the run's.
fn patch_equivalent(repo: &Path, branch: &str, base: &str, commit: &str) -> Option<String> {
    let tip = format!("refs/heads/{branch}");
    if is_ancestor(repo, commit, &tip) {
        return Some(commit.to_owned());
    }
    git(repo, &["cherry", commit, &tip, base])
        .ok()?
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("- "))
        .map(|sha| sha.trim().to_owned())
}

fn is_ancestor(repo: &Path, ancestor: &str, of: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", ancestor, of]).is_ok()
}

/// The commit the run's push destination's branch is at, read from origin
/// itself with the caller's environment (`git ls-remote`), its commit
/// fetched when the repository lacks it: none when the run names no push
/// destination or origin has no such branch, an error when origin cannot
/// be read or its commit cannot be fetched.
fn origin_head(
    run: &Run,
    repo: &Path,
    env: &std::collections::HashMap<String, String>,
) -> Result<Option<String>, String> {
    let plan = WorkerSupervisor::push_plan(run, repo);
    let (Some(remote), Some(dst_ref)) = (plan.remote, plan.dst_ref) else {
        return Ok(None);
    };
    let theirs = git_with(repo, &["ls-remote", "--", &remote, &dst_ref], Some(env))
        .map_err(|why| format!("reading {remote}'s {dst_ref}: {why}"))?
        .lines()
        .find_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            (name.trim() == dst_ref).then(|| sha.trim().to_owned())
        });
    let Some(theirs) = theirs else {
        return Ok(None);
    };
    let known = |sha: &str| git(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok();
    if !known(&theirs) {
        git_with(
            repo,
            &["fetch", "--quiet", "--", &remote, &dst_ref],
            Some(env),
        )
        .map_err(|why| format!("fetching {remote}'s {dst_ref}: {why}"))?;
        if !known(&theirs) {
            return Err(format!(
                "{remote}'s {dst_ref} is at {theirs}, which a fetch did not bring"
            ));
        }
    }
    Ok(Some(theirs))
}

/// The last value of trailer `key` in `commit`'s message.
fn trailer_value(repo: &Path, commit: &str, key: &str) -> Option<String> {
    git(
        repo,
        &["log", "-1", "--format=%(trailers:only,unfold)", commit],
    )
    .ok()?
    .lines()
    .rev()
    .find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(key)
            .then(|| value.trim().to_owned())
    })
}

/// Refuses a worktree that is not on `branch`.
fn on_branch(repo: &Path, branch: &str) -> Result<(), String> {
    let on = git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .map(|head| head.trim().to_owned())
        .unwrap_or_default();
    if on == branch {
        return Ok(());
    }
    Err(format!(
        "the landing worktree {} is not on {branch}; nothing was changed on it",
        repo.display()
    ))
}

/// Moves `branch`, checked out in `repo`, from `expected` to its descendant
/// `new`, worktree and index with it, only while its head is `expected`
/// (compare-and-swap): the head is checked right before `git merge
/// --ff-only`, which refuses anything but a fast-forward and to touch the
/// worktree's local changes. `Ok(Err(head))` when the head is no longer
/// `expected`: nothing moved.
pub(super) fn fast_forward(
    repo: &Path,
    branch: &str,
    expected: &str,
    new: &str,
    hook: &str,
) -> Result<Result<(), String>, String> {
    on_branch(repo, branch)?;
    let head = || git(repo, &["rev-parse", "HEAD"]).map(|head| head.trim().to_owned());
    let before = head()?;
    if before != expected {
        return Ok(Err(before));
    }
    if let Err(error) = git(repo, &["merge", "--ff-only", "--quiet", new]) {
        let now = head()?;
        if now != expected {
            return Ok(Err(now));
        }
        return Err(format!("fast-forwarding {branch} to {new}: {error}"));
    }
    #[cfg(test)]
    super::run_hook(hook, "fast_forwarded");
    #[cfg(not(test))]
    let _ = hook;
    // Another session may commit on top right after the merge.
    let after = head()?;
    if after != new && !is_ancestor(repo, new, &after) {
        return Err(format!(
            "{branch} is at {after} after the fast-forward to {new}, which it does not contain"
        ));
    }
    Ok(Ok(()))
}

/// Moves `branch`, checked out in `repo`, from `expected` to `to` (any
/// commit, here an ancestor) as a compare-and-swap on its ref: `git
/// update-ref` refuses unless the ref is `expected`, so a commit that
/// arrived meanwhile is never moved over. The worktree and index follow
/// (`git read-tree -m -u <expected> <to>`, the two-tree switch, which keeps
/// local changes and refuses to overwrite one), checked by a dry run before
/// the ref moves; when they still refuse, the ref is swapped back.
/// `Ok(Err(head))` when the ref is no longer `expected`: nothing moved.
fn move_branch(
    repo: &Path,
    branch: &str,
    expected: &str,
    to: &str,
    hook: &str,
) -> Result<Result<(), String>, String> {
    on_branch(repo, branch)?;
    let reference = format!("refs/heads/{branch}");
    let head =
        || git(repo, &["rev-parse", "--verify", &reference]).map(|head| head.trim().to_owned());
    // Unchanged files whose stat data is stale are not local changes.
    let _ = git(repo, &["update-index", "-q", "--refresh"]);
    if let Err(error) = git(repo, &["read-tree", "-m", "-n", expected, to]) {
        let now = head()?;
        if now != expected {
            return Ok(Err(now));
        }
        return Err(format!(
            "moving {branch} back to {to} would touch the landing worktree's local changes: \
             {error}; nothing was changed"
        ));
    }
    #[cfg(test)]
    super::run_hook(hook, "restore_checked");
    #[cfg(not(test))]
    let _ = hook;
    let message = format!("herdr: take the run's commits off {branch}");
    if let Err(error) = git(
        repo,
        &["update-ref", "-m", &message, &reference, to, expected],
    ) {
        let now = head()?;
        if now != expected {
            return Ok(Err(now));
        }
        return Err(format!("moving {branch} back to {to}: {error}"));
    }
    #[cfg(test)]
    super::crashes_at(hook, "restore_moved")?;
    if let Err(error) = git(repo, &["read-tree", "-m", "-u", expected, to]) {
        let back = git(
            repo,
            &["update-ref", "-m", "herdr: undo", &reference, expected, to],
        );
        return Err(format!(
            "the landing worktree did not follow {branch} back to {to}: {error}; {}",
            match back {
                Ok(_) => format!("{branch} was put back at {expected}"),
                Err(why) => format!("{branch} could not be put back at {expected} either: {why}"),
            }
        ));
    }
    Ok(Ok(()))
}

/// After a restore's reset of `branch` from `from` to `to` that a driver
/// left cut off: when the branch is at `to` but the index's tree is not
/// `to`'s, its worktree and index did not follow the ref, and the two-tree
/// switch (`git read-tree -m -u <from> <to>`) is made now, which keeps the
/// worktree's other changes and refuses to touch one of them. An error,
/// which stops the run, when it refuses: another session's next commit would
/// otherwise carry the change the reset took off.
fn finish_restore_switch(repo: &Path, branch: &str, from: &str, to: &str) -> Result<(), String> {
    let reference = format!("refs/heads/{branch}");
    let at = git(repo, &["rev-parse", "--verify", &reference])?
        .trim()
        .to_owned();
    if at != to {
        return Ok(());
    }
    let differs = git(repo, &["diff-index", "--cached", "--name-only", to, "--"])?;
    if differs.trim().is_empty() {
        return Ok(());
    }
    // Unchanged files whose stat data is stale are not local changes.
    let _ = git(repo, &["update-index", "-q", "--refresh"]);
    git(repo, &["read-tree", "-m", "-u", from, to]).map_err(|error| {
        format!(
            "{branch} was moved back to {to}, but the landing worktree {} still holds {from} \
             and does not follow without touching its local changes: {error}; the run stops \
             here: make the worktree match {to} (git read-tree -m -u {from} {to}), then abort \
             again",
            repo.display()
        )
    })?;
    Ok(())
}

/// Returns the folder slot `dir` to `back`: its branch, or the commit it was
/// detached at; an error unless it is exactly there afterwards.
fn return_slot(dir: &Path, back: &str, is_branch: bool) -> Result<(), String> {
    let args: Vec<&str> = if is_branch {
        vec!["switch", "--quiet", back]
    } else {
        vec!["switch", "--quiet", "--detach", back]
    };
    git(dir, &args)?;
    let at = if is_branch {
        git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    } else {
        git(dir, &["rev-parse", "HEAD"])
    }
    .map(|at| at.trim().to_owned())
    .unwrap_or_default();
    if at == back {
        Ok(())
    } else {
        Err(format!("it is at {at:?} after the switch"))
    }
}

/// The blob id git gives `bytes`.
pub(super) fn blob_id(repo: &Path, bytes: &[u8]) -> Result<String, String> {
    hash_object(repo, bytes, false)
}

/// `bytes` written to the repository as a blob: its id.
pub(super) fn write_blob(repo: &Path, bytes: &[u8]) -> Result<String, String> {
    hash_object(repo, bytes, true)
}

fn hash_object(repo: &Path, bytes: &[u8], write: bool) -> Result<String, String> {
    let mut args = vec!["hash-object", "--stdin"];
    if write {
        args.push("-w");
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(super::super::home_repo::git_dir_args(repo))
        .args(&args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(bytes)
            .map_err(|error| format!("cannot write to git hash-object: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("git hash-object: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git hash-object: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Puts each file a TODO edit of an older server wrote back as `HEAD` has
/// it, but only while its content is still exactly what the edit wrote
/// (`blob`): a file someone changed since is left alone. The files put
/// back; an error naming the file one that cannot be.
pub(super) fn put_back_todo_files(
    repo: &Path,
    files: &[(String, String)],
) -> Result<Vec<String>, String> {
    let mut put_back = Vec::new();
    for (file, blob) in files {
        let Ok(now) = std::fs::read(repo.join(file)) else {
            continue;
        };
        if blob_id(repo, &now).ok().as_deref() != Some(blob.as_str()) {
            continue;
        }
        let committed = git(repo, &["rev-parse", "--verify", &format!("HEAD:{file}")])
            .map(|id| id.trim() == blob.as_str())
            .unwrap_or(false);
        if committed {
            continue;
        }
        git(
            repo,
            &[
                "restore",
                "--source=HEAD",
                "--staged",
                "--worktree",
                "--",
                file,
            ],
        )
        .map_err(|error| {
            format!(
                "the TODO edit's {file} in {}, written and not committed by a driver that went \
                 away, could not be put back as HEAD has it ({error}); the run stops here so \
                 that no edit of herdr's stays in the checkout",
                repo.display()
            )
        })?;
        put_back.push(file.clone());
    }
    Ok(put_back)
}

/// The tree of `commit` with each of `files` (a path and its bytes) in
/// place of what it has there, built in the temporary index `index`, so
/// neither the worktree nor its index is touched.
pub(super) fn tree_with(
    repo: &Path,
    commit: &str,
    files: &[(String, Vec<u8>)],
    index: &Path,
) -> Result<String, String> {
    let _ = std::fs::remove_file(index);
    let indexed = |args: &[&str]| -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(super::super::home_repo::git_dir_args(repo))
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_INDEX_FILE", index)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("cannot run git: {error}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    };
    let built = (|| {
        indexed(&["read-tree", commit])?;
        for (file, bytes) in files {
            let blob = write_blob(repo, bytes)?;
            let mode = git(repo, &["ls-tree", commit, "--", file])?
                .split_whitespace()
                .next()
                .map(str::to_owned)
                .unwrap_or_else(|| "100644".to_owned());
            indexed(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("{mode},{blob},{file}"),
            ])?;
        }
        indexed(&["write-tree"])
    })();
    let _ = std::fs::remove_file(index);
    built
}

/// A commit with `tree` on `parent` and `message`, with the author,
/// committer and dates of `like`: the same inputs make the same sha.
fn commit_like(
    repo: &Path,
    like: &str,
    tree: &str,
    parent: &str,
    message: &str,
) -> Result<String, String> {
    let fields = git(
        repo,
        &[
            "log",
            "-1",
            "--date=raw",
            "--format=%an%x00%ae%x00%ad%x00%cn%x00%ce%x00%cd",
            like,
        ],
    )?;
    let fields: Vec<&str> = fields.trim_end_matches('\n').splitn(6, '\0').collect();
    let [author, author_email, author_date, committer, committer_email, committer_date] =
        fields[..]
    else {
        return Err(format!("cannot read commit {like}'s author"));
    };
    let created = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(super::super::home_repo::git_dir_args(repo))
        .args(["commit-tree", tree, "-p", parent, "-m", message])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", author)
        .env("GIT_AUTHOR_EMAIL", author_email)
        .env("GIT_AUTHOR_DATE", author_date)
        .env("GIT_COMMITTER_NAME", committer)
        .env("GIT_COMMITTER_EMAIL", committer_email)
        .env("GIT_COMMITTER_DATE", committer_date)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !created.status.success() {
        return Err(format!(
            "git commit-tree {tree}: {}",
            String::from_utf8_lossy(&created.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&created.stdout).trim().to_owned())
}

/// `git merge-tree --write-tree` of `ours` and `theirs` on `merge_base`:
/// the tree, or the conflict as the inner error.
fn merge_tree(
    repo: &Path,
    merge_base: &str,
    ours: &str,
    theirs: &str,
) -> Result<Result<String, String>, String> {
    let merged = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(super::super::home_repo::git_dir_args(repo))
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            &format!("--merge-base={merge_base}"),
            ours,
            theirs,
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    let out = String::from_utf8_lossy(&merged.stdout);
    match merged.status.code() {
        Some(0) => Ok(Ok(out.lines().next().unwrap_or_default().trim().to_owned())),
        Some(1) => Ok(Err(out
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("; "))),
        _ => Err(format!(
            "git merge-tree {ours} {theirs}: {}",
            String::from_utf8_lossy(&merged.stderr).trim()
        )),
    }
}

/// One commit on `head` that reverts `commits` (oldest first), made without
/// a worktree, its message naming each and carrying the run's
/// `Herdr-Item` and `Herdr-Revert` trailers.
fn revert_commit(repo: &Path, head: &str, commits: &[String], run: &Run) -> Result<String, String> {
    let mut current = head.to_owned();
    for commit in commits.iter().rev() {
        let parent = git(
            repo,
            &["rev-parse", "--verify", &format!("{commit}^{{commit}}^")],
        )?
        .trim()
        .to_owned();
        let tree = merge_tree(repo, commit, &current, &parent)?
            .map_err(|conflict| format!("reverting {commit} conflicts: {conflict}"))?;
        current = git(
            repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &current,
                "-m",
                "herdr revert step",
            ],
        )?
        .trim()
        .to_owned();
    }
    let reverts: Vec<String> = commits
        .iter()
        .map(|commit| format!("This reverts commit {commit}."))
        .collect();
    let message = format!(
        "Revert \"{}\"\n\n{}\n\nHerdr-Item: {}\n{REVERT_TRAILER}: {}\n",
        run.info.message,
        reverts.join("\n"),
        run.info.item,
        run_trailer(&run.info)
    );
    let tree = format!("{current}^{{tree}}");
    Ok(
        git(repo, &["commit-tree", &tree, "-p", head, "-m", &message])?
            .trim()
            .to_owned(),
    )
}

/// The landing's input for a coordinator's action applied in `todo.resume`'s
/// transaction: the run's new state, and the event an `emit` raises (an
/// abort after the push ends the run at once).
pub(super) fn resume_landing(
    current: &mut Run,
    event: CoreEvent,
) -> Result<Option<TodoEventKind>, WorkerError> {
    feed(current, event).map_err(WorkerError::Invalid)
}
