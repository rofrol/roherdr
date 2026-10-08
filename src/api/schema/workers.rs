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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerTarget {
    pub worker_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerPromptParams {
    pub worker_id: String,
    /// The next user message; accepted only between turns.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerWaitParams {
    pub worker_id: String,
    /// What to wait for; `turn_end` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<WorkerWaitUntil>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkerWaitUntil {
    /// A finished, failed or interrupted turn, or the process's end.
    TurnEnd,
    /// The process's end (`exited` or `lost`).
    Exit,
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
    /// The server that ran it ended before it did; its process is not ours
    /// any more.
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
    /// each Bash tool with `setsid`); `worker.kill` ends them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_sessions: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_signal: Option<i32>,
    /// Questions the worker waits on, oldest first; its state is
    /// `waiting_approval` while there are any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<WorkerQuestion>,
    /// Unix milliseconds when `worker.stop` sent SIGTERM, while the process
    /// has not exited yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_requested_ms: Option<u64>,
    /// Unix milliseconds when a takeover began (`worker.take_over`): the
    /// worker is being ended so its session can resume in a tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover_ms: Option<u64>,
    /// The JSONL journal of every event in and out.
    pub journal_path: String,
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
