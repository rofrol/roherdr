use serde::{Deserialize, Serialize};

/// Starts a headless Claude worker: `claude -p` over stream-json pipes, no
/// terminal. The server owns the process; clients only ask about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerStartParams {
    /// Working directory; file tools are allowed only inside its real path.
    pub cwd: String,
    /// The first user message.
    pub prompt: String,
    /// Passed to `claude --model`; the CLI's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The worker's task, as the sidebar names it; the prompt's first line
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The space the worker belongs to, which the sidebar lists it under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Runs the worker in the persistent git worktree
    /// `<repository parent>/herdr-worktrees/<folder_slot>` of `cwd`'s
    /// repository instead of in `cwd`, one worker at a time, so its build
    /// caches stay warm. Needs `branch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_slot: Option<String>,
    /// With `folder_slot`: the new branch checked out in the slot; it must
    /// not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// With `folder_slot`: what the branch starts from; `master` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// With `folder_slot`: removes the slot's `target/` and Zig cache first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_build: bool,
    /// The pane of the coordinator that starts the worker (the CLI sends its
    /// `HERDR_PANE_ID`): the worker's owner, whose `worker.obligations` list
    /// the worker's events it has not acknowledged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    /// The owner's agent session (the CLI sends Claude Code's
    /// `CLAUDE_CODE_SESSION_ID`), so the owner is the session, not only the
    /// pane that holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerTarget {
    pub worker_id: String,
}

/// The owner acknowledges having handled the worker's events up to `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerAckParams {
    pub worker_id: String,
    /// The `seq` handled: the one `worker.wait` with `until: attention` or
    /// `worker.obligations` returned. An ack at or below the acknowledged
    /// one changes nothing; one past the worker's latest event is refused.
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerObligationsParams {
    /// Only the workers this pane owns; every owned worker when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
}

/// The owner hands one of the worker's pending questions to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerEscalateParams {
    pub worker_id: String,
    /// The question to escalate; one that is no longer pending is refused
    /// with `worker_question_gone`.
    pub request_id: String,
}

/// A worker whose owner has an event of it to handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerObligation {
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// What it needs: `question` (an unanswered question asked after the
    /// acknowledged `seq`), `turn_end` (a turn ended after it, to review) or
    /// `gone` (the worker exited or was lost after it).
    pub reason: WorkerAttentionReason,
    /// The worker's latest event's `seq`; `worker.ack` with it settles
    /// everything listed here.
    pub seq: i64,
    /// With `question`: the pending questions asked after the acknowledged
    /// `seq`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<WorkerQuestion>,
    pub owner_pane_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
}

/// A worker and the client's id for a command on it (`worker.stop`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerCommandTarget {
    pub worker_id: String,
    /// The pane that sends the stop (the CLI sends its `HERDR_PANE_ID`).
    /// When it is the worker's owner, the exit it causes is acknowledged
    /// with it, so the owner owes nothing for an end it asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerPromptParams {
    pub worker_id: String,
    /// The next user message; accepted only between turns.
    pub text: String,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerWaitParams {
    pub worker_id: String,
    /// What to wait for; `turn_end` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<WorkerWaitUntil>,
    /// With `until: attention` only: the `seq` the previous attention reply
    /// carried. A pending question asked or a turn ended at or before it does
    /// not count again; only what later events bring does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerInterruptParams {
    pub worker_id: String,
    /// The turn to interrupt: its `turn_seq` (the `seq` of the user message
    /// that began it, as `worker.prompt` returns it). Refused with
    /// `worker_turn_ended` once that turn has ended, so a repeated interrupt
    /// never reaches the next turn. When absent, whatever runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<i64>,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerKillParams {
    pub worker_id: String,
    /// Needed to signal anything for a worker that is `exited` or `lost`:
    /// without it such a kill is refused with `worker_needs_force`, and the
    /// message lists what it would signal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// The pane that sends the kill (the CLI sends its `HERDR_PANE_ID`).
    /// When it is the worker's owner, the exit it causes is acknowledged
    /// with it, so the owner owes nothing for an end it asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

/// What `worker.kill` did with the worker's recorded tool sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerKillReport {
    /// Processes sent SIGKILL: the members of each recorded session whose
    /// leader still is the process recorded with it (same start time) that
    /// did not start before that leader.
    pub pids: Vec<u32>,
    /// Sessions recorded without their leader's start time (journals from
    /// before herdr recorded it): not verified, not killed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_sessions: Vec<u32>,
    /// Sessions whose leader is gone or is now another process with the
    /// same pid: skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_sessions: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerWaitUntil {
    /// A finished, failed or interrupted turn, or the process's end.
    TurnEnd,
    /// The process's end (`exited` or `lost`).
    Exit,
    /// The first of a pending question, a finished, failed or interrupted
    /// turn, or the process's end; answered with `worker_attention`.
    Attention,
}

/// Why `worker.wait` with `until: attention` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAttentionReason {
    /// A question waits for an answer.
    Question,
    /// The turn finished, failed or was interrupted.
    TurnEnd,
    /// The process ended (`exited` or `lost`); nothing more will happen.
    Gone,
    /// A reason this client does not know.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// Spawned; no `system/init` yet.
    Starting,
    /// A turn is running.
    Working,
    /// The CLI asked for a tool permission or a question that is not
    /// answered yet; `questions` lists those left to the user.
    WaitingApproval,
    /// The last turn ended with `result/success`.
    Finished,
    /// The last turn ended with an error `result` other than an interrupt,
    /// or with a model refusal (`last_result.failure` says which).
    Failed,
    /// The last turn was interrupted (`terminal_reason` `aborted_*`).
    Interrupted,
    /// The process ended; `exit_code` or `exit_signal` says how.
    Exited,
    /// The server that ran it ended before it did, during a turn; its
    /// process is not ours any more. One between turns keeps its last
    /// turn's state with `end_note` instead.
    Lost,
    /// A state this client does not know.
    #[serde(other)]
    Unknown,
}

/// The `result` message that ended the last turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerTurnResult {
    pub subtype: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_error_status: Option<i64>,
    /// The reply text, when the CLI sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Why herdr counts the turn as failed even when its `result` reads as
    /// success: a model refusal in the turn, or a non-zero exit code right
    /// after a finished turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// The `result`'s `permission_denials`: the tool uses denied in the
    /// turn, as the CLI sent them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permission_denials: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerInfo {
    pub worker_id: String,
    pub state: WorkerState,
    pub cwd: String,
    /// The worker's task: the `name` given at start, else the prompt's
    /// first line.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The space given at start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The CLI's process id, which also leads the worker's process group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Claude Code's session id from `system/init`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Turns that ended with a `result`.
    pub turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<WorkerTurnResult>,
    /// `rate_limit_info` of the last `rate_limit_event`, as the CLI sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<serde_json::Value>,
    /// Sessions of the worker's tool processes seen so far (Claude Code runs
    /// each Bash tool with `setsid`); `worker.kill` ends those whose leader
    /// is still the process recorded with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_sessions: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
    /// Questions the worker waits on, oldest first: `pending` ones and
    /// those whose answer is being sent (`answering`); its state is
    /// `waiting_approval` while there are any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<WorkerQuestion>,
    /// The most recently settled questions, oldest first, with how each
    /// ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settled_questions: Vec<WorkerSettledQuestion>,
    /// Unix milliseconds when `worker.stop` sent SIGTERM, while the process
    /// has not exited yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_requested_ms: Option<u64>,
    /// Unix milliseconds when a takeover began (`worker.take_over`): the
    /// worker is being ended so its session can resume in a tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover_ms: Option<u64>,
    /// The tab the takeover opened to resume the worker's session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover_tab_id: Option<String>,
    /// Why the last takeover failed; its claim was released, so it can be
    /// tried again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover_error: Option<String>,
    /// A takeover claimed by an earlier server that did not record opening
    /// its tab: a tab may or may not resume the session. A new takeover is
    /// accepted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub takeover_unfinished: bool,
    /// How a worker ended that keeps its last turn's state: one that had
    /// finished its turn when its server ended shows that turn's state with
    /// "ended by a server restart", not `lost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_note: Option<String>,
    /// Why the worker's record is incomplete: writing one of its events to
    /// the worker store or its journal failed, with the error. The status
    /// still follows the worker; a restart may not show what was lost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    /// The JSONL journal of every event in and out, exported after each
    /// event is stored; `herdr worker log` reads it.
    pub journal_path: String,
    /// The `seq` of the worker's latest event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// The `seq` of the user message that began the current or last turn;
    /// `worker.interrupt` takes it as `turn`. In `worker.prompt`'s reply it
    /// is the message that prompt appended; in `worker.interrupt`'s, the
    /// turn it interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_seq: Option<i64>,
    /// The pane that started the worker, its owner (`worker.start`'s
    /// `owner_pane_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    /// The highest `seq` its owner acknowledged (`worker.ack`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_seq: Option<i64>,
}

/// Answers a worker's pending question: a tool approval or an
/// `AskUserQuestion`. The worker waits for it without a time limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerAnswerParams {
    pub worker_id: String,
    /// The question to answer. When absent, the only pending question; an
    /// answer without it is refused while several are pending. A question
    /// that is no longer pending is refused with `worker_question_gone`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `allow` or `deny` for an approval. For an `AskUserQuestion`, `deny`
    /// declines to answer and `allow` (or absent) sends `answers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<WorkerDecision>,
    /// `AskUserQuestion` only: one answer per question, in order. Each is an
    /// option's label, its 1-based number, or free text; a multi-select
    /// question takes several labels or numbers separated by commas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// The message the model gets with a denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A client's id for this command, unique per command it means
    /// (`worker_command_conflict` when reused with other parameters). A
    /// repeated id returns the stored outcome without doing it again: the
    /// same reply, or the same refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerQuestionKind {
    /// A tool the policy does not decide; answered with allow or deny.
    Approval,
    /// An `AskUserQuestion`; answered with the chosen options.
    Choice,
    /// A kind this client does not know.
    #[serde(other)]
    Unknown,
}

/// A question a worker waits on: a `can_use_tool` request the policy left to
/// the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerQuestion {
    /// The CLI's control request id; `worker.answer` names it.
    pub request_id: String,
    pub kind: WorkerQuestionKind,
    pub tool_name: String,
    /// One line for the user: the Bash command, the question, or the tool's
    /// input.
    pub text: String,
    /// Why the policy did not decide it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `AskUserQuestion`'s questions, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<WorkerChoiceQuestion>,
    /// Unix milliseconds since when the worker waits.
    pub since_ms: u64,
    /// Where its answer is: `pending` (only such a question is answered),
    /// or `answering`.
    #[serde(default)]
    pub state: WorkerQuestionState,
    /// Why the question was handed to the user, for a worker with an owner:
    /// the owner escalated it, its pane closed, its agent exited, hit a
    /// limit, ended its turn or is blocked on its own question, or herdr
    /// restarted without it. Until then the user's `?` list shows it quietly
    /// as awaiting the coordinator. A worker without an owner asks the user
    /// at once and never sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated: Option<String>,
}

/// Where a question's answer is. `answered` means the answer was written to
/// the worker's input; the CLI sends no acknowledgement of it, and herdr
/// does not track which client was shown the question, so neither
/// "acknowledged" nor "delivered" is a state of its own.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkerQuestionState {
    /// Waits for an answer.
    #[default]
    Pending,
    /// An answer is stored and being written to the worker; a failed write
    /// makes it `pending` again.
    Answering,
    /// The answer was written to the worker's input.
    Answered,
    /// The CLI withdrew the question.
    Cancelled,
    /// Ended unanswered: its turn ended, the worker exited or was lost, or
    /// a server restart found its answer not confirmed written.
    Expired,
    /// A state this client does not know.
    #[serde(other)]
    Unknown,
}

/// A question that is no longer open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerSettledQuestion {
    pub request_id: String,
    pub state: WorkerQuestionState,
    /// What ended it, as `worker_question_gone` says it.
    pub how: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerChoiceQuestion {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The options' labels.
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}
