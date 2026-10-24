//! A worker's state, folded from its journal events ([`Status::apply`]),
//! with its questions. Folding touches no live process and no registry:
//! [`super::WorkerSupervisor::commit_locked`] compares [`Status::before`]
//! with the folded state and acts on what changed. Only
//! [`Status::process_gone`] asks the OS whether a process group is alive.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use super::{item_coordinators, pi, store, string_field, usage};
use crate::api::schema::{
    WorkerAttentionReason, WorkerHeldOutput, WorkerInfo, WorkerModelUsage, WorkerQuestion,
    WorkerQuestionState, WorkerRun, WorkerRunOutcome, WorkerSettledQuestion, WorkerState,
    WorkerToolCall, WorkerTurnResult, WorkerVerification,
};

/// What a journal line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    /// A line the CLI wrote to stdout.
    Out,
    /// A line herdr wrote to the CLI's stdin.
    In,
    /// Herdr's own record (start, permission decisions, sessions, exit).
    Herdr,
    /// A line the CLI wrote to stderr.
    Err,
}

impl Direction {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::In => "in",
            Self::Herdr => "herdr",
            Self::Err => "err",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "out" => Self::Out,
            "in" => Self::In,
            "herdr" => Self::Herdr,
            "err" => Self::Err,
            _ => return None,
        })
    }
}

/// A worker's state, folded from its journal events by [`Status::apply`].
#[derive(Debug, Clone)]
pub(super) struct Status {
    pub(super) worker_id: String,
    pub(super) state: WorkerState,
    pub(super) cwd: String,
    pub(super) name: String,
    pub(super) workspace_id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) pid: Option<u32>,
    pub(super) session_id: Option<String>,
    pub(super) turns: u32,
    pub(super) last_result: Option<WorkerTurnResult>,
    pub(super) rate_limit: Option<Value>,
    /// Each recorded tool session with its leader's start token
    /// ([`crate::platform::process_start_token`]); `None` for a session a
    /// journal recorded before herdr kept it, which `kill` does not trust.
    pub(super) tool_sessions: BTreeMap<u32, Option<u64>>,
    pub(super) exit_code: Option<i32>,
    pub(super) exit_signal: Option<i32>,
    /// Requests left to the user, oldest first.
    pub(super) questions: Vec<Pending>,
    /// The most recently settled questions, oldest first, with what ended
    /// each, so an answer to one of them says what happened to it.
    pub(super) resolved: VecDeque<(String, String)>,
    pub(super) stop_requested_ms: Option<u64>,
    /// Its owner sent a stop or kill (a `signal` with `by_owner`): the exit
    /// that follows is an end the owner asked for ([`Self::end_by_owner`]).
    /// Not stored: after a restart in between, the exit is an obligation,
    /// as one nobody asked for.
    pub(super) stop_by_owner: bool,
    /// It ended by the exit its owner asked for ([`Self::stop_by_owner`],
    /// set by [`Self::mark_seq`]): the owner owes nothing for that end,
    /// though it never acknowledged it, so `acked_seq` stays what the owner
    /// acknowledged.
    pub(super) end_by_owner: bool,
    /// The takeover claim: set by `takeover`, cleared by `takeover_failed`.
    /// Kept in every state, an exited worker's too.
    pub(super) takeover_ms: Option<u64>,
    /// The claim's unique id (`takeover`'s `takeover_id`), which its tab
    /// carries in `HERDR_TAKEOVER_ID` and its title, so a restart can find
    /// the tab. None for a claim recorded before the id.
    pub(super) takeover_id: Option<String>,
    /// The tab a takeover opened (`takeover_tab_opened`).
    pub(super) takeover_tab: Option<String>,
    /// Why the last takeover failed (`takeover_failed`).
    pub(super) takeover_error: Option<String>,
    /// A claim replayed from the journal without its tab: the server that
    /// made it ended before recording the tab.
    pub(super) takeover_unfinished: bool,
    /// The category of a model refusal in the running turn
    /// (`system/model_refusal_no_fallback`); it fails the turn.
    pub(super) refusal: Option<String>,
    /// The process's exit is recorded. The state is then `exited`, or
    /// `failed` when the exit failed a finished turn.
    pub(super) exited: bool,
    /// A later server recorded the worker `lost`: the server that ran it
    /// ended first. The state is then `lost`, or the last turn's end state
    /// when that turn had ended (with `end_note`). A later `exited` replaces
    /// it.
    pub(super) lost: bool,
    /// Why a worker that keeps its last turn's state is gone.
    pub(super) end_note: Option<String>,
    /// The folder slot it runs in, if any.
    pub(super) slot: Option<String>,
    /// The branch checked out in its folder slot (`started`'s
    /// `folder_slot.branch`). Not a stored column: a reopen reads it from
    /// the `started` event ([`store::Store::load`]).
    pub(super) branch: Option<String>,
    /// When the user message that began the current or last turn and the
    /// latest event were recorded, in Unix milliseconds. Not stored
    /// columns: a reopen reads them from the events `turn_seq` and
    /// `last_seq` name.
    pub(super) turn_started_ms: Option<u64>,
    pub(super) last_event_ms: Option<u64>,
    /// The `seq` of its latest event: the store's, or, after a store write
    /// failed, one counted on in this server's memory, so waits still see
    /// each event as new ([`WorkerSupervisor::commit_locked`]).
    pub(super) last_seq: i64,
    /// The `seq` of the user message that began the current or last turn.
    pub(super) turn_seq: Option<i64>,
    /// The `seq` of the event that last ended a turn (a `result`, the exit,
    /// `lost`); meaningful while [`Status::turn_ended`].
    pub(super) turn_end_seq: i64,
    /// The `seq` of the event that made the worker gone; meaningful while
    /// [`Status::is_gone`].
    pub(super) gone_seq: i64,
    /// Why the worker's record is incomplete: a store or journal write
    /// failed. Not an event: it is stored with the worker's row the next
    /// time the store takes a write, and a reopen finds a gap the store
    /// never learned of in the journal export ([`note_journal_gap`]).
    pub(super) degraded: Option<String>,
    /// The pane and agent session that started it (`started`'s `owner`).
    pub(super) owner_pane: Option<String>,
    pub(super) owner_session: Option<String>,
    /// The coordination tenure the owner pane was bound to then
    /// (`started`'s `owner.coordinator_id`).
    pub(super) owner_coordinator: Option<String>,
    /// The highest `seq` its owner acknowledged (`acked`); only grows.
    pub(super) acked_seq: i64,
    /// Why its owner is gone for good (`owner_gone`: its pane closed, its
    /// agent exited): every question, pending or later, goes to the user.
    pub(super) owner_gone: Option<String>,
    /// The TODO item it works on (`started`'s `item`).
    pub(super) item: Option<String>,
    /// That item's title in its repository's `TODO.md` when the worker
    /// started (`started`'s `item_title`): a finished item leaves the file,
    /// and its runs keep the title.
    pub(super) item_title: Option<String>,
    /// The repository of its directory (`started`'s `repo`): the parent of
    /// its git common directory.
    pub(super) repo: Option<String>,
    /// When its `started` and the event that made it gone were recorded,
    /// in Unix milliseconds ([`Self::mark_seq`]).
    pub(super) started_ms: Option<u64>,
    pub(super) ended_ms: Option<u64>,
    /// The shas its turns' results named on `WORKER-DONE <sha>` lines.
    pub(super) done_commits: Vec<String>,
    /// How many questions it asked.
    pub(super) questions_asked: u32,
    /// It exited or was lost while a turn ran.
    pub(super) ended_mid_turn: bool,
    /// The latest `worker.verify` of its work (`verification`).
    pub(super) verification: Option<WorkerVerification>,
    /// The broker that owns its pipes (`started`'s `broker`); none for a
    /// worker whose pipes the server owned.
    pub(super) broker: Option<BrokerRecord>,
    /// The broker seq of the last line of its output that is stored
    /// ([`WorkerSupervisor::record_spooled`]); 0 before any.
    pub(super) broker_seq: u64,
    /// Why a re-attach could not prove that the stored record holds every
    /// line the worker wrote (`continuity_gap`'s `reason`). Its state and
    /// session may then be stale, so it takes no prompt and no takeover.
    pub(super) continuity_gap: Option<String>,
    /// The headless tenure it runs as (`started`'s `coordinator`): an item
    /// coordinator ([`item_coordinators`]), whose every question goes to
    /// the user at once.
    pub(super) coordinates: Option<String>,
    /// The tool calls it started (an assistant `tool_use`) without a
    /// `tool_result` yet; a turn's end clears them.
    pub(super) open_tool_calls: Vec<WorkerToolCall>,
    /// The latest `tool_output_held` report in the current or last turn.
    pub(super) held_output: Option<WorkerHeldOutput>,
    /// The `seq` of the latest `tool_output_held` event.
    pub(super) held_output_seq: Option<i64>,
    /// The agent that runs it (`started`'s `agent`): `claude`, `pi`, or
    /// `pi-anthropic` for pi on the Anthropic API credits. None for a worker
    /// whose `started` named none.
    pub(super) agent: Option<String>,
    /// The model the agent reported at its start (`system/init`'s `model`).
    pub(super) ran_model: Option<String>,
    /// Its usage per model so far ([`usage`]); None while the agent has
    /// reported none.
    pub(super) usage: Option<Vec<WorkerModelUsage>>,
    /// Its cost so far in US dollars; None while unknown.
    pub(super) cost_usd: Option<f64>,
}

/// Where a worker's broker serves it, and what a server that re-attaches
/// needs to answer its permission requests again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub(super) struct BrokerRecord {
    pub(super) pid: u32,
    pub(super) socket: PathBuf,
    /// The worker's directory as asked for, before resolving links (its
    /// resolved one is [`Status::cwd`]).
    pub(super) cwd: PathBuf,
    /// The pre-tool checks its hook runs ([`pre_tool_checks`]), as they
    /// were when it started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) pre_tool_checks: Vec<Vec<String>>,
}

/// How many settled questions a worker remembers for `worker_question_gone`.
pub(super) const RESOLVED_QUESTIONS_KEPT: usize = 32;

/// An open question with the tool input its answer is built from.
#[derive(Debug, Clone)]
pub(super) struct Pending {
    pub(super) question: WorkerQuestion,
    pub(super) input: Value,
    /// The `seq` of the event that asked it.
    pub(super) asked_seq: i64,
    /// Its answer is stored and being written (`answer_intent`): it is no
    /// longer answerable, and only the answer's outcome (`answer_sent`,
    /// `answer_failed`, `answer_expired`) settles it, never a cleanup.
    pub(super) answering: bool,
    /// What ended its turn or its worker while its answer was in flight: a
    /// failed write then settles it so instead of making it pending again.
    pub(super) cleared: Option<String>,
    /// Why it was handed to the user (`escalated`, `owner_gone`); once set it
    /// stays, whatever its owner does later.
    pub(super) escalated: Option<String>,
}

/// What an event's `seq` marks, taken before the event is folded in
/// ([`Status::before`], [`Status::mark_seq`]).
pub(super) struct Before {
    /// The open questions, each with whether its answer was in flight.
    pub(super) pending: Vec<(String, bool)>,
    pub(super) turn_ended: bool,
    pub(super) gone: (bool, bool),
    /// The tenure that owned it ([`inbox`]).
    pub(super) owner: Option<String>,
    pub(super) listed: bool,
}

impl Status {
    pub(super) fn new(worker_id: String) -> Self {
        Self {
            worker_id,
            state: WorkerState::Starting,
            cwd: String::new(),
            name: String::new(),
            workspace_id: None,
            model: None,
            pid: None,
            session_id: None,
            turns: 0,
            last_result: None,
            rate_limit: None,
            tool_sessions: BTreeMap::new(),
            exit_code: None,
            exit_signal: None,
            questions: Vec::new(),
            resolved: VecDeque::new(),
            stop_requested_ms: None,
            stop_by_owner: false,
            end_by_owner: false,
            takeover_ms: None,
            takeover_id: None,
            takeover_tab: None,
            takeover_error: None,
            takeover_unfinished: false,
            refusal: None,
            exited: false,
            lost: false,
            end_note: None,
            slot: None,
            branch: None,
            turn_started_ms: None,
            last_event_ms: None,
            last_seq: 0,
            turn_seq: None,
            turn_end_seq: 0,
            gone_seq: 0,
            degraded: None,
            owner_pane: None,
            owner_session: None,
            owner_coordinator: None,
            acked_seq: 0,
            owner_gone: None,
            item: None,
            item_title: None,
            repo: None,
            started_ms: None,
            ended_ms: None,
            done_commits: Vec::new(),
            questions_asked: 0,
            ended_mid_turn: false,
            verification: None,
            broker: None,
            broker_seq: 0,
            continuity_gap: None,
            coordinates: None,
            open_tool_calls: Vec::new(),
            held_output: None,
            held_output_seq: None,
            agent: None,
            ran_model: None,
            usage: None,
            cost_usd: None,
        }
    }

    /// Drops an answered or cancelled question, recording `how` it ended;
    /// the turn goes on once none is left.
    pub(super) fn settle_question(&mut self, request_id: Option<&str>, how: &str) {
        if let Some(request_id) = request_id {
            let before = self.questions.len();
            self.questions
                .retain(|pending| pending.question.request_id != request_id);
            if self.questions.len() < before {
                self.remember_resolved(request_id.to_owned(), how);
            }
        }
        if self.questions.is_empty() && self.state == WorkerState::WaitingApproval {
            self.state = WorkerState::Working;
        }
    }

    /// Drops every pending question, recording `how` they ended. One whose
    /// answer is in flight stays until the answer's outcome settles it
    /// (the cleanup guard): the answer may already be on its way.
    pub(super) fn clear_questions(&mut self, how: &str) {
        let mut in_flight = Vec::new();
        for mut pending in std::mem::take(&mut self.questions) {
            if pending.answering {
                pending.cleared.get_or_insert_with(|| how.to_owned());
                in_flight.push(pending);
            } else {
                self.remember_resolved(pending.question.request_id, how);
            }
        }
        self.questions = in_flight;
    }

    /// Writing an answer failed: the question is pending again, or, when
    /// its turn or worker ended meanwhile, settled as that cleanup said.
    pub(super) fn answer_failed(&mut self, request_id: Option<&str>) {
        let Some(pending) = self
            .questions
            .iter_mut()
            .find(|pending| Some(pending.question.request_id.as_str()) == request_id)
        else {
            return;
        };
        match pending.cleared.clone() {
            Some(how) => self.settle_question(request_id, &how),
            None => pending.answering = false,
        }
    }

    pub(super) fn is_answering(&self, request_id: Option<&str>) -> bool {
        self.questions.iter().any(|pending| {
            pending.answering && Some(pending.question.request_id.as_str()) == request_id
        })
    }

    pub(super) fn remember_resolved(&mut self, request_id: String, how: &str) {
        self.resolved.retain(|(id, _)| *id != request_id);
        if self.resolved.len() == RESOLVED_QUESTIONS_KEPT {
            self.resolved.pop_front();
        }
        self.resolved.push_back((request_id, how.to_owned()));
    }

    /// What ended a question that is no longer pending, if it is recent.
    pub(super) fn resolution(&self, request_id: &str) -> Option<&str> {
        self.resolved
            .iter()
            .find(|(id, _)| id == request_id)
            .map(|(_, how)| how.as_str())
    }

    /// The ids of the open questions, each with whether its answer is in
    /// flight, to tell what an event changed.
    pub(super) fn open_questions(&self) -> Vec<(String, bool)> {
        self.questions
            .iter()
            .map(|pending| (pending.question.request_id.clone(), pending.answering))
            .collect()
    }

    pub(super) fn before(&self) -> Before {
        Before {
            pending: self.open_questions(),
            turn_ended: self.turn_ended(),
            gone: (self.exited, self.lost),
            owner: self.owner_coordinator.clone(),
            listed: self.listed(),
        }
    }

    /// Marks what the event just folded in, which got `seq` and was
    /// recorded at `ts_ms`, changed: the questions it asked, the start, the
    /// turn it began, ended or the end it brought.
    pub(super) fn mark_seq(
        &mut self,
        seq: i64,
        ts_ms: u64,
        before: &Before,
        direction: Direction,
        record: &store::Recorded<'_>,
    ) {
        self.last_seq = seq;
        self.last_event_ms = Some(ts_ms);
        for pending in &mut self.questions {
            if !before
                .pending
                .iter()
                .any(|(id, _)| *id == pending.question.request_id)
            {
                pending.asked_seq = seq;
            }
        }
        let kind = match record {
            store::Recorded::Event(event) => event.get("type").and_then(Value::as_str),
            store::Recorded::Raw(_) => None,
        };
        if (direction, kind) == (Direction::Herdr, Some("started")) {
            self.started_ms.get_or_insert(ts_ms);
        }
        if (direction, kind) == (Direction::In, Some("user")) && !self.is_gone() {
            self.turn_seq = Some(seq);
            self.turn_started_ms = Some(ts_ms);
        }
        if (direction, kind) == (Direction::Herdr, Some("tool_output_held")) {
            self.held_output_seq = Some(seq);
        }
        let result = (direction, kind) == (Direction::Out, Some("result"));
        if self.turn_ended() && (!before.turn_ended || result) {
            self.turn_end_seq = seq;
        }
        if self.is_gone() && (self.exited, self.lost) != before.gone {
            self.gone_seq = seq;
            self.ended_ms = Some(ts_ms);
            // The exit its owner asked for needs no ack; what came before
            // it stays as acknowledged as it was.
            self.end_by_owner = self.exited && self.stop_by_owner;
        }
    }

    /// Why a waiter that has seen everything up to `after` should look at
    /// the worker now, if it should: a pending question asked after it, a
    /// turn ended after it, the worker's end, which counts however old,
    /// since nothing can follow it, or a tool call's output held open by
    /// its descendants reported after it.
    pub(super) fn attention(&self, after: Option<i64>) -> Option<WorkerAttentionReason> {
        let after = after.unwrap_or(i64::MIN);
        if self.is_gone() {
            Some(WorkerAttentionReason::Gone)
        } else if self
            .questions
            .iter()
            .any(|pending| pending.asked_seq > after)
        {
            Some(WorkerAttentionReason::Question)
        } else if self.turn_ended() && self.turn_end_seq > after {
            Some(WorkerAttentionReason::TurnEnd)
        } else if self.held_output_seq.is_some_and(|seq| seq > after) {
            Some(WorkerAttentionReason::HeldOutput)
        } else {
            None
        }
    }

    /// What the owner has to handle that it has not acknowledged: the
    /// worker's end after `acked_seq`, else the unanswered questions asked
    /// after it, else a turn that ended after it. Unlike [`Self::attention`]
    /// the end counts only once acknowledged, and a question whose answer
    /// is in flight is handled already.
    pub(super) fn obligation(&self) -> Option<(WorkerAttentionReason, Vec<WorkerQuestion>)> {
        let acked = self.acked_seq;
        if self.is_gone() {
            return self
                .end_owed()
                .then(|| (WorkerAttentionReason::Gone, Vec::new()));
        }
        let questions: Vec<WorkerQuestion> = self
            .questions
            .iter()
            .filter(|pending| !pending.answering && pending.asked_seq > acked)
            .map(Pending::shown)
            .collect();
        if !questions.is_empty() {
            Some((WorkerAttentionReason::Question, questions))
        } else if self.turn_ended() && self.turn_end_seq > acked {
            Some((WorkerAttentionReason::TurnEnd, Vec::new()))
        } else {
            None
        }
    }

    /// Whether a pending question waits quietly for the worker's owner: it
    /// has one, and nothing has handed the question to the user yet.
    pub(super) fn is_quiet(&self, pending: &Pending) -> bool {
        self.owner_pane.is_some() && pending.escalated.is_none()
    }

    /// The ids of the questions that still wait quietly for the owner, whose
    /// answer is not in flight.
    pub(super) fn quiet_questions(&self) -> Vec<String> {
        self.questions
            .iter()
            .filter(|pending| !pending.answering && self.is_quiet(pending))
            .map(|pending| pending.question.request_id.clone())
            .collect()
    }

    /// A takeover claimed by a server that is gone without recording its
    /// tab: whether it opened the tab is unknown.
    pub(super) fn mark_unfinished_takeover(&mut self) {
        self.takeover_unfinished = self.takeover_ms.is_some() && self.takeover_tab.is_none();
    }

    pub(super) fn is_gone(&self) -> bool {
        self.exited || self.lost
    }

    /// An ended worker whose owner has not acknowledged its end yet: its
    /// handoff is open, so its result still waits for review.
    pub(super) fn end_unacked(&self) -> bool {
        self.is_gone() && self.owner_pane.is_some() && self.end_owed()
    }

    /// Whether its end waits for its owner's ack: it came after what the
    /// owner acknowledged, and the owner did not ask for it.
    fn end_owed(&self) -> bool {
        self.gone_seq > self.acked_seq && !self.end_by_owner
    }

    /// Whether the sidebar lists the worker: while it runs, and after its
    /// end until its owner acknowledges that end. A worker without an owner
    /// leaves at its end: nobody would acknowledge it, and its run stays in
    /// its item's history (`worker.runs`).
    pub(super) fn listed(&self) -> bool {
        !self.is_gone() || self.end_unacked()
    }

    /// Whether its owner has nothing left to do for it: its turn or its
    /// process ended, no question is open and every event is acknowledged.
    /// A worker without an owner is settled once its turn ended with no
    /// question, as nobody acknowledges it.
    pub(super) fn settled(&self) -> bool {
        self.turn_ended()
            && self.questions.is_empty()
            && (self.owner_pane.is_none() || self.obligation().is_none())
    }

    /// Whether the sidebar shows its line: while [`Self::listed`], except
    /// an owned worker that finished its turn and is [`Self::settled`]
    /// (its acknowledgement stops it, see [`WorkerSupervisor::ack`]). Its
    /// log stays reachable (`worker.log`, `worker.list`, `worker.runs`).
    pub(super) fn in_sidebar(&self) -> bool {
        self.listed() && !(self.owner_pane.is_some() && self.settled())
    }

    /// Whether an acknowledgement just handled everything of a live owned
    /// worker between turns, so it is to be stopped: an ack means handled,
    /// and its process and folder slot are not kept for nothing. Not while
    /// a stop or a takeover is under way.
    pub(super) fn handled_between_turns(&self) -> bool {
        !self.is_gone()
            && self.owner_pane.is_some()
            && self.settled()
            && self.stop_requested_ms.is_none()
            && self.takeover_ms.is_none()
    }

    /// Whether the worker's process is known to be gone: its exit is
    /// recorded, or it was lost and its process group no longer exists.
    pub(super) fn process_gone(&self) -> bool {
        self.exited
            || (self.lost
                && self
                    .pid
                    .is_none_or(|pid| !crate::platform::process_group_alive(pid)))
    }

    /// Whether a turn runs: the first has not ended yet, or a prompt began
    /// another after the last one ended.
    pub(super) fn mid_turn(&self) -> bool {
        !matches!(
            self.state,
            WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
        )
    }

    pub(super) fn turn_ended(&self) -> bool {
        self.is_gone()
            || matches!(
                self.state,
                WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
            )
    }

    pub(super) fn apply(&mut self, direction: Direction, event: &Value) {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        // A takeover goes on after the worker has exited (its tab opens
        // then), so its events count in every state.
        if direction == Direction::Herdr {
            match kind {
                "takeover" => {
                    self.takeover_ms = Some(event["at_ms"].as_u64().unwrap_or(0));
                    self.takeover_id = string_field(event, "takeover_id");
                    self.takeover_tab = None;
                    self.takeover_error = None;
                    self.takeover_unfinished = false;
                    return;
                }
                "takeover_failed" => {
                    self.takeover_ms = None;
                    self.takeover_id = None;
                    self.takeover_error = Some(
                        string_field(event, "error").unwrap_or_else(|| "unknown error".into()),
                    );
                    self.takeover_unfinished = false;
                    return;
                }
                "takeover_tab_opened" => {
                    self.takeover_tab = Some(string_field(event, "tab_id").unwrap_or_default());
                    self.takeover_unfinished = false;
                    return;
                }
                // An answer's outcome also comes after the turn or the
                // worker ended while it was in flight.
                "answer_sent" => {
                    self.settle_question(event["request_id"].as_str(), "answered");
                    return;
                }
                "answer_failed" => {
                    self.answer_failed(event["request_id"].as_str());
                    return;
                }
                "answer_expired" => {
                    let how = string_field(event, "how").unwrap_or_else(|| "expired".into());
                    self.settle_question(event["request_id"].as_str(), &how);
                    return;
                }
                // Verified after the worker ended, as it usually is.
                "verification" => {
                    self.verification = serde_json::from_value(event["verification"].clone()).ok();
                    return;
                }
                // Its coordinator resumed in another pane or handed off:
                // the new owner handles its events, a gone worker's end
                // too.
                "owner_moved" => {
                    self.owner_pane = string_field(event, "pane_id");
                    self.owner_session = string_field(event, "session_id");
                    self.owner_coordinator = string_field(event, "coordinator_id");
                    self.owner_gone = None;
                    return;
                }
                // A former owner's answer, refused and kept for the owner
                // ([`WorkerSupervisor::answer_as`]): it changes nothing.
                "late_answer" => return,
                // The owner handles a gone worker's end too.
                "acked" => {
                    let seq = event["seq"].as_i64().unwrap_or(0);
                    self.acked_seq = self.acked_seq.max(seq);
                    return;
                }
                _ => {}
            }
        }
        // An exit recorded after `lost` (by the server that still ran the
        // worker) replaces it.
        let exit_after_lost =
            self.lost && !self.exited && (direction, kind) == (Direction::Herdr, "exited");
        if self.is_gone() && !exit_after_lost {
            return;
        }
        match (direction, kind) {
            (Direction::Herdr, "started") => {
                self.cwd = string_field(event, "cwd").unwrap_or_default();
                self.name = string_field(event, "name").unwrap_or_default();
                self.workspace_id = string_field(event, "workspace_id");
                self.model = string_field(event, "model");
                self.slot = event["folder_slot"]["name"].as_str().map(str::to_owned);
                self.branch = event["folder_slot"]["branch"].as_str().map(str::to_owned);
                self.owner_pane = event["owner"]["pane_id"].as_str().map(str::to_owned);
                self.owner_session = event["owner"]["session_id"].as_str().map(str::to_owned);
                self.owner_coordinator =
                    event["owner"]["coordinator_id"].as_str().map(str::to_owned);
                self.agent = string_field(event, "agent").map(|agent| {
                    let credits = self
                        .model
                        .as_deref()
                        .is_some_and(|model| model.starts_with(pi::ANTHROPIC_PREFIX));
                    if agent == "pi" && credits {
                        "pi-anthropic".to_owned()
                    } else {
                        agent
                    }
                });
                self.item = string_field(event, "item");
                self.item_title = string_field(event, "item_title");
                self.repo = string_field(event, "repo");
                self.coordinates = event["coordinator"]["coordinator_id"]
                    .as_str()
                    .map(str::to_owned);
                self.pid = event
                    .get("pid")
                    .and_then(Value::as_u64)
                    .map(|pid| pid as u32);
                self.broker = event
                    .get("broker")
                    .and_then(|broker| serde_json::from_value(broker.clone()).ok());
            }
            (Direction::Herdr, "continuity_gap") => {
                let reason = string_field(event, "reason").unwrap_or_default();
                self.degraded = Some(format!("output continuity not proven: {reason}"));
                self.continuity_gap = Some(reason);
            }
            (Direction::Herdr, "tool_sessions") => {
                for recorded in event["sessions"].as_array().into_iter().flatten() {
                    // Older journals list bare session ids.
                    let (session, leader_start) = match recorded.as_u64() {
                        Some(session) => (session, None),
                        None => match recorded["session"].as_u64() {
                            Some(session) => (session, recorded["leader_start"].as_u64()),
                            None => continue,
                        },
                    };
                    let known = self.tool_sessions.entry(session as u32).or_default();
                    if known.is_none() {
                        *known = leader_start;
                    }
                }
            }
            (Direction::Herdr, "question") => {
                let question = event
                    .get("question")
                    .cloned()
                    .and_then(|question| serde_json::from_value(question).ok());
                if let Some(question) = question {
                    self.questions_asked += 1;
                    self.questions.push(Pending {
                        question,
                        input: event.get("input").cloned().unwrap_or_else(|| json!({})),
                        asked_seq: 0,
                        answering: false,
                        cleared: None,
                        escalated: self.owner_gone.clone().or_else(|| {
                            self.coordinates
                                .as_ref()
                                .map(|_| item_coordinators::QUESTION_TO_USER.to_owned())
                        }),
                    });
                }
            }
            // A journal from before the answer outbox settled the question
            // with its answer.
            (Direction::Herdr, "answer") => {
                self.settle_question(event["request_id"].as_str(), "answered");
            }
            (Direction::Herdr, "answer_intent") => {
                let request_id = event["request_id"].as_str();
                if let Some(pending) = self
                    .questions
                    .iter_mut()
                    .find(|pending| Some(pending.question.request_id.as_str()) == request_id)
                {
                    pending.answering = true;
                }
            }
            (Direction::Herdr, "escalated") => {
                let cause = string_field(event, "cause").unwrap_or_else(|| "escalated".into());
                let ids: Vec<&str> = event["request_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                for pending in &mut self.questions {
                    if ids.contains(&pending.question.request_id.as_str()) {
                        pending.escalated.get_or_insert_with(|| cause.clone());
                    }
                }
            }
            (Direction::Herdr, "owner_gone") => {
                let cause = string_field(event, "cause").unwrap_or_else(|| "owner gone".into());
                for pending in &mut self.questions {
                    pending.escalated.get_or_insert_with(|| cause.clone());
                }
                self.owner_gone.get_or_insert(cause);
            }
            (Direction::Herdr, "signal") => {
                if event["signal"].as_str() == Some("SIGTERM") {
                    self.stop_requested_ms = event["at_ms"].as_u64();
                }
                if event["by_owner"].as_bool() == Some(true) {
                    self.stop_by_owner = true;
                }
            }
            (Direction::Herdr, "exited") => {
                self.ended_mid_turn = self.mid_turn();
                let code = event.get("code").and_then(Value::as_i64);
                // A refused turn can end with `result/success` and then
                // exit code 1 (T3-1): that turn is `failed`, not `exited`.
                // Herdr's own stop and an interrupted last turn also exit
                // non-zero, but neither follows a `finished` turn.
                let failed_turn = match (
                    code.filter(|code| *code != 0),
                    self.state == WorkerState::Finished,
                    self.stop_requested_ms,
                ) {
                    (Some(code), true, None) => {
                        if let Some(result) = self.last_result.as_mut() {
                            result.failure.get_or_insert_with(|| {
                                format!("the CLI exited with code {code} after the turn")
                            });
                        }
                        true
                    }
                    _ => false,
                };
                self.clear_questions("the worker exited");
                self.stop_requested_ms = None;
                self.exited = true;
                self.lost = false;
                self.end_note = None;
                self.state = if failed_turn {
                    WorkerState::Failed
                } else {
                    WorkerState::Exited
                };
                self.exit_code = code.map(|v| v as i32);
                self.exit_signal = event
                    .get("signal")
                    .and_then(Value::as_i64)
                    .map(|v| v as i32);
                // The broker ended the worker rather than lose its output.
                if let Some(error) = event["spool_error"].as_str() {
                    self.degraded = Some(format!(
                        "the worker's broker could not write its output spool and ended the \
                         worker: {error}"
                    ));
                }
            }
            (Direction::Herdr, "lost") => {
                // A worker between turns lost nothing but its process: it
                // keeps its last turn's state. A stop request stays, so a
                // later `exited` is judged as the stop's.
                self.ended_mid_turn = self.mid_turn();
                self.clear_questions("the worker was lost");
                self.lost = true;
                if matches!(
                    self.state,
                    WorkerState::Finished | WorkerState::Failed | WorkerState::Interrupted
                ) {
                    self.end_note = Some("ended by a server restart".into());
                } else {
                    self.state = WorkerState::Lost;
                }
            }
            (Direction::Out, "system") => {
                if event.get("subtype").and_then(Value::as_str) == Some("model_refusal_no_fallback")
                {
                    self.refusal = Some(
                        string_field(event, "api_refusal_category")
                            .unwrap_or_else(|| "unknown".into()),
                    );
                }
                if event.get("subtype").and_then(Value::as_str) == Some("init") {
                    if let Some(session_id) = string_field(event, "session_id") {
                        self.session_id = Some(session_id);
                    }
                    if let Some(model) = string_field(event, "model") {
                        self.ran_model = Some(model);
                    }
                    if self.state == WorkerState::Starting {
                        self.state = WorkerState::Working;
                    }
                }
            }
            (Direction::Out, "control_request") => {
                if event["request"]["subtype"].as_str() == Some("can_use_tool") {
                    self.state = WorkerState::WaitingApproval;
                }
            }
            (Direction::In, "control_response") => {
                // An answer's own line: `answer_sent` settles it once written.
                let request_id = event["response"]["request_id"].as_str();
                if !self.is_answering(request_id) {
                    self.settle_question(request_id, "answered");
                }
            }
            (Direction::Out, "control_cancel_request") => {
                self.settle_question(event["request_id"].as_str(), "cancelled");
            }
            (Direction::In, "user") => {
                // A report belongs to its turn: the next one starts without.
                if !self.mid_turn() {
                    self.held_output = None;
                }
                // A refusal stays until the turn's `result` takes it: a
                // message sent during the turn does not clear it.
                if self.state != WorkerState::Starting {
                    self.state = WorkerState::Working;
                }
            }
            (Direction::Out, "assistant") => {
                self.add_reply_usage(event);
                self.open_tool_calls.extend(
                    event["message"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|block| block["type"].as_str() == Some("tool_use"))
                        .filter_map(|block| {
                            Some(WorkerToolCall {
                                id: string_field(block, "id")?,
                                tool: string_field(block, "name").unwrap_or_default(),
                                command: block["input"]["command"]
                                    .as_str()
                                    .map(|command| command.chars().take(200).collect()),
                            })
                        }),
                );
            }
            (Direction::Out, "user") => {
                for block in event["message"]["content"].as_array().into_iter().flatten() {
                    if let Some(id) = block["tool_use_id"].as_str() {
                        self.open_tool_calls.retain(|call| call.id != id);
                    }
                }
            }
            (Direction::Herdr, "tool_output_held") => {
                self.held_output = serde_json::from_value(event.clone()).ok();
            }
            (Direction::Out, "rate_limit_event") => {
                self.rate_limit = event.get("rate_limit_info").cloned();
            }
            (Direction::Out, "result") => {
                let result = WorkerTurnResult {
                    subtype: string_field(event, "subtype").unwrap_or_default(),
                    is_error: event
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    terminal_reason: string_field(event, "terminal_reason"),
                    api_error_status: event.get("api_error_status").and_then(Value::as_i64),
                    text: string_field(event, "result"),
                    failure: self
                        .refusal
                        .take()
                        .map(|category| format!("the model refused the turn ({category})")),
                    permission_denials: event["permission_denials"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                };
                self.state = turn_end_state(&result);
                self.take_result_usage(event);
                for sha in result
                    .text
                    .as_deref()
                    .map(done_commits)
                    .into_iter()
                    .flatten()
                {
                    if !self.done_commits.contains(&sha) {
                        self.done_commits.push(sha);
                    }
                }
                self.clear_questions("its turn ended");
                self.open_tool_calls.clear();
                self.turns += 1;
                self.last_result = Some(result);
            }
            _ => {}
        }
    }

    /// Adds one pi model reply's usage (its translated `assistant` event's
    /// `herdr_usage`). pi on the API credits prices its custom model at
    /// nothing, so its cost stays unknown there.
    fn add_reply_usage(&mut self, event: &Value) {
        let Some(mut reply) = event
            .get("herdr_usage")
            .and_then(|reply| serde_json::from_value::<WorkerModelUsage>(reply.clone()).ok())
        else {
            return;
        };
        if self.agent.as_deref() == Some("pi-anthropic") {
            reply.cost_usd = None;
        }
        let run = self.usage.get_or_insert_with(Vec::new);
        usage::add(run, &reply);
        self.cost_usd = usage::total_cost(run);
    }

    /// Takes Claude's cumulative usage from a turn's `result`: the session's
    /// so far, which replaces the run's. A `result` whose cost is the
    /// credit ledger's spend for a whole `todo.run` (`credit_accounting`)
    /// says nothing about this worker's.
    fn take_result_usage(&mut self, event: &Value) {
        if event.get("credit_accounting").is_some() {
            return;
        }
        if let Some(models) = usage::claude_result_usage(event) {
            self.usage = Some(models);
        }
        if let Some(cost) = event.get("total_cost_usd").and_then(Value::as_f64) {
            self.cost_usd = Some(cost);
        }
    }

    pub(super) fn info(&self, journal_path: &Path) -> WorkerInfo {
        WorkerInfo {
            worker_id: self.worker_id.clone(),
            state: self.state,
            cwd: self.cwd.clone(),
            name: self.name.clone(),
            workspace_id: self.workspace_id.clone(),
            model: self.model.clone(),
            pid: self.pid,
            session_id: self.session_id.clone(),
            turns: self.turns,
            last_result: self.last_result.clone(),
            rate_limit: self.rate_limit.clone(),
            tool_sessions: self.tool_sessions.keys().copied().collect(),
            exit_code: self.exit_code,
            exit_signal: self.exit_signal,
            questions: self.questions.iter().map(Pending::shown).collect(),
            settled_questions: self
                .resolved
                .iter()
                .map(|(request_id, how)| WorkerSettledQuestion {
                    request_id: request_id.clone(),
                    state: settled_state(how),
                    how: how.clone(),
                })
                .collect(),
            stop_requested_ms: self.stop_requested_ms,
            takeover_ms: self.takeover_ms,
            takeover_id: self.takeover_id.clone(),
            takeover_tab_id: self.takeover_tab.clone(),
            takeover_error: self.takeover_error.clone(),
            takeover_unfinished: self.takeover_unfinished,
            end_note: self.end_note.clone(),
            degraded: self.degraded.clone(),
            journal_path: journal_path.display().to_string(),
            seq: Some(self.last_seq),
            turn_seq: self.turn_seq,
            owner_pane_id: self.owner_pane.clone(),
            owner_session_id: self.owner_session.clone(),
            owner_coordinator_id: self.owner_coordinator.clone(),
            acked_seq: (self.acked_seq > 0).then_some(self.acked_seq),
            item: self.item.clone(),
            repo: self.repo.clone(),
            survives_handoff: self.broker.is_some() && !self.is_gone(),
            held_output: self.held_output.clone(),
        }
    }

    pub(super) fn run(&self, journal_path: &Path) -> WorkerRun {
        // Judged by how its last turn ended, unless it ended in a turn.
        let last_turn = self.last_result.as_ref().map(turn_end_state);
        let outcome = if self.degraded.is_some() {
            WorkerRunOutcome::Degraded
        } else if !self.is_gone() {
            WorkerRunOutcome::Running
        } else if self.ended_mid_turn {
            if self.lost {
                WorkerRunOutcome::Lost
            } else {
                WorkerRunOutcome::Exited
            }
        } else if self.state == WorkerState::Failed || last_turn == Some(WorkerState::Failed) {
            WorkerRunOutcome::Failed
        } else if last_turn == Some(WorkerState::Finished) {
            WorkerRunOutcome::Finished
        } else if self.lost {
            WorkerRunOutcome::Lost
        } else {
            WorkerRunOutcome::Exited
        };
        WorkerRun {
            worker_id: self.worker_id.clone(),
            name: self.name.clone(),
            repo: self.repo.clone(),
            started_ms: self.started_ms,
            ended_ms: self.ended_ms,
            outcome,
            turns: self.turns,
            commits: self.done_commits.clone(),
            questions: self.questions_asked,
            journal_path: journal_path.display().to_string(),
            verification: self.verification.clone(),
        }
    }
}

impl Pending {
    /// The question as the API shows it, with where its answer is.
    pub(super) fn shown(&self) -> WorkerQuestion {
        let mut question = self.question.clone();
        question.escalated = self.escalated.clone();
        question.state = if self.answering {
            WorkerQuestionState::Answering
        } else {
            WorkerQuestionState::Pending
        };
        question
    }
}

/// The state of a question that ended `how`.
fn settled_state(how: &str) -> WorkerQuestionState {
    match store::question_state(how) {
        "answered" => WorkerQuestionState::Answered,
        "cancelled" => WorkerQuestionState::Cancelled,
        _ => WorkerQuestionState::Expired,
    }
}

/// Judges a turn by its `result` and a model refusal in it (which can end
/// with `result/success`, T3-1); a non-zero exit code after a `finished`
/// turn is recorded in its `failure` when the process exits (it is also 1
/// after an interrupted last turn and 143 after SIGTERM).
fn turn_end_state(result: &WorkerTurnResult) -> WorkerState {
    if result.failure.is_some() {
        WorkerState::Failed
    } else if result
        .terminal_reason
        .as_deref()
        .is_some_and(|reason| reason.starts_with("aborted"))
    {
        WorkerState::Interrupted
    } else if result.subtype == "success" && !result.is_error {
        WorkerState::Finished
    } else {
        WorkerState::Failed
    }
}

/// The commit shas a turn's reply names on its `WORKER-DONE <sha> | ...`
/// lines (the worker's last line, by the coordinators' convention).
fn done_commits(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("WORKER-DONE "))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|sha| (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::user_message;
    use super::*;

    #[test]
    fn done_commits_are_read_from_worker_done_lines() {
        assert_eq!(
            done_commits("x\nWORKER-DONE 3ecc4be6 | summary\n  WORKER-DONE abcdef0123 | more"),
            ["3ecc4be6", "abcdef0123"]
        );
        assert!(
            done_commits("WORKER-DONE <sha> | x\nWORKER-BLOCKED no\nWORKER-DONE abc | x")
                .is_empty()
        );
    }

    /// Folds `events` into a new status as the store does, without one.
    fn folded(events: &[(Direction, Value)]) -> Status {
        let mut status = Status::new("w1".into());
        for (seq, (direction, event)) in (1..).zip(events) {
            let before = status.before();
            status.apply(*direction, event);
            status.mark_seq(seq, 0, &before, *direction, &store::Recorded::Event(event));
        }
        status
    }

    #[test]
    fn a_reattach_after_a_lost_init_waits_for_the_replayed_init() {
        let started = (
            Direction::Herdr,
            json!({"type": "started", "cwd": "/repo", "pid": 4242,
                   "broker": {"pid": 4241, "socket": "/state/w1.sock", "cwd": "/repo"}}),
        );
        let prompt = (Direction::In, user_message("block"));
        let reattached = (
            Direction::Herdr,
            json!({"type": "reattached", "broker_pid": 4241}),
        );
        let init = (
            Direction::Out,
            json!({"type": "system", "subtype": "init", "session_id": "s"}),
        );

        // The gone server took the CLI's `system/init` and stored nothing: the
        // re-attach itself changes nothing.
        let status = folded(&[started.clone(), prompt.clone(), reattached.clone()]);
        assert_eq!(status.state, WorkerState::Starting);
        assert_eq!(status.session_id, None);
        // The broker replays the init after the re-attach: the turn works and
        // the session is known, as a takeover needs.
        let status = folded(&[started.clone(), prompt.clone(), reattached.clone(), init]);
        assert_eq!(status.state, WorkerState::Working);
        assert_eq!(status.session_id.as_deref(), Some("s"));

        // Without the init, because continuity is not proven, nothing is made
        // up: the worker stays starting, with no session, degraded.
        let gap = (
            Direction::Herdr,
            json!({"type": "continuity_gap", "reason": "lines 2..=4 are missing"}),
        );
        let status = folded(&[started, prompt, reattached, gap]);
        assert_eq!(status.state, WorkerState::Starting);
        assert_eq!(status.session_id, None);
        assert_eq!(
            status.continuity_gap.as_deref(),
            Some("lines 2..=4 are missing")
        );
        assert_eq!(
            status.degraded.as_deref(),
            Some("output continuity not proven: lines 2..=4 are missing")
        );
    }

    /// A pi worker's replies add up, by model; on the API credits its cost
    /// stays unknown, since pi prices the proxy's model at nothing. Its
    /// `result` carries no usage, and the credit ledger's run-wide spend on
    /// it says nothing about this worker.
    #[test]
    fn a_pi_workers_replies_add_up_and_its_cost_on_the_credits_stays_unknown() {
        let reply = |output: u64| {
            json!({"type": "assistant", "message": {"content": []}, "herdr_usage": {
                "model": "openai-codex/gpt-6.1-sol", "input_tokens": 10,
                "output_tokens": output, "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 0, "cost_usd": 0.25}})
        };
        let mut status = Status::new("w1".into());
        status.apply(
            Direction::Herdr,
            &json!({"type": "started", "agent": "pi", "model": "openai-codex/gpt-6.1-sol"}),
        );
        assert_eq!(status.agent.as_deref(), Some("pi"));
        assert_eq!((status.usage.clone(), status.cost_usd), (None, None));
        status.apply(Direction::Out, &reply(5));
        status.apply(Direction::Out, &reply(7));
        status.apply(
            Direction::Out,
            &json!({"type": "result", "subtype": "success", "result": "ok"}),
        );
        let usage = status.usage.clone().unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(
            (
                usage[0].input_tokens,
                usage[0].output_tokens,
                usage[0].cost_usd
            ),
            (20, 12, Some(0.5))
        );
        assert_eq!(status.cost_usd, Some(0.5));

        let mut credits = Status::new("w2".into());
        credits.apply(
            Direction::Herdr,
            &json!({"type": "started", "agent": "pi",
                "model": "herdr-anthropic/claude-sonnet-5-5"}),
        );
        assert_eq!(credits.agent.as_deref(), Some("pi-anthropic"));
        credits.apply(Direction::Out, &reply(5));
        credits.apply(
            Direction::Out,
            &json!({"type": "result", "subtype": "success", "total_cost_usd": 9.0,
                "credit_accounting": {"spent_usd": 9.0, "worker": "r-1"}}),
        );
        assert_eq!(credits.usage.as_ref().unwrap()[0].output_tokens, 5);
        assert_eq!(credits.usage.as_ref().unwrap()[0].cost_usd, None);
        assert_eq!(credits.cost_usd, None);
    }
}
