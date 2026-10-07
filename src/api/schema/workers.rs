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
    /// The CLI asked for a tool permission that is not answered yet.
    WaitingApproval,
    /// The last turn ended with `result/success`.
    Finished,
    /// The last turn ended with an error `result` other than an interrupt.
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkerInfo {
    pub worker_id: String,
    pub state: WorkerState,
    pub cwd: String,
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
    /// The JSONL journal of every event in and out.
    pub journal_path: String,
}
