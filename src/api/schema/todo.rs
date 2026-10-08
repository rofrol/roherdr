use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::workers::{WorkerDecision, WorkerQuestion, WorkerVerification};

/// Drives one TODO item from preflight to a cherry-pick onto the
/// repository's `master`: a persisted, resumable run that starts a headless
/// worker in the repository's folder slot, turns what needs the coordinator
/// (a question outside the worker policy, the turn's end, a failed verify)
/// into run events, and verifies the worker's commit with a registered
/// check before picking it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunParams {
    /// A directory in the repository (the CLI sends its working directory).
    pub cwd: String,
    /// The TODO item's id (`t-abcd2345`); it must be in the repository's
    /// `TODO.md`.
    pub item: String,
    /// The worker's task text, stored with the run as it is.
    pub task: String,
    /// The exact commit subject the worker must use: a lowercase
    /// conventional subject (`feat: ...`), one line.
    pub message: String,
    /// Git glob pathspecs (`src/**`, `AGENTS.md`) the commit may touch.
    pub paths: Vec<String>,
    /// The names of checks in the repository's `.herdr/checks.toml`, run
    /// in this order; every one must pass.
    pub checks: Vec<String>,
    /// The coordinator's pane, which owns the run's workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    /// The caller's environment, which the check runs with (`HERDR_*`
    /// dropped); kept in the server's memory only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunTarget {
    pub run_id: String,
}

/// The coordinator's answer to a run's event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoResumeParams {
    pub run_id: String,
    pub action: TodoAction,
    /// The event answered: the `event_id` `todo.wait` returned. Any other
    /// is refused as stale (`todo_event_stale`).
    pub event: i64,
    /// With `retry`: the next attempt's task text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// With `answer`: the question answered; the only pending one when
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// With `answer`: allow or deny for an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<WorkerDecision>,
    /// With `answer`: one choice per `AskUserQuestion` question.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// With `answer`: the message the model gets with a denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The caller's environment, sent again with every resume (`HERDR_*`
    /// dropped) and kept in the server's memory only, never stored. A
    /// resume without it leaves the server none: the run's next check is
    /// then `unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoWaitParams {
    pub run_id: String,
    /// The `event_id` the previous wait returned: only a later event
    /// counts. A run that ended (`done`, `blocked`) returns its last event
    /// however old.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunsParams {
    /// Only the runs of this repository: a directory in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// What the coordinator may answer an event with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoAction {
    /// The worker's turn is accepted: stop it, verify, cherry-pick.
    Approve,
    /// Start the next attempt with new task text (at most 3 attempts).
    Retry,
    /// Answer the worker's pending question.
    Answer,
    /// Run the verify again with the environment this resume sends (after
    /// a check was `unavailable`).
    Verify,
    /// SIGKILL the worker that is still alive after its stop.
    ForceStop,
    #[serde(other)]
    Unknown,
}

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStep {
    Preflight,
    Start,
    Attention,
    Review,
    Stop,
    Verify,
    /// Stops the attempt's worker before the next attempt starts.
    Restart,
    CherryPick,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoRunStatus {
    /// The driver works on its step.
    Running,
    /// It waits for the coordinator's `todo.resume` to its pending event.
    Waiting,
    /// It stopped and needs a person: a refused preflight, a conflict, the
    /// attempts used up. Nothing more happens.
    Blocked,
    /// The commit is on `master`.
    Done,
    #[serde(other)]
    Unknown,
}

/// A run's event that needs the coordinator or ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoEventKind {
    /// The worker asked a question the worker policy does not decide.
    Question,
    /// The worker's turn ended (or the worker did): review its work.
    Review,
    /// The verify failed or could not run: give the next attempt's task,
    /// or verify again.
    VerifyFailed,
    /// Raised by `todo.status` while the worker has not exited since its
    /// stop (it ignores SIGTERM): force-stop it. The run goes on by itself
    /// when the worker exits first.
    StillAlive,
    Blocked,
    Done,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunInfo {
    /// `r-` and 8 lowercase base32 characters.
    pub run_id: String,
    pub repo: String,
    pub item: String,
    pub step: TodoStep,
    pub status: TodoRunStatus,
    /// 1 for the first worker; each retry adds one, at most 3.
    pub attempt: u32,
    /// `master`'s commit when the run started, which each attempt's branch
    /// starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The current attempt's task text.
    pub task: String,
    pub message: String,
    pub paths: Vec<String>,
    /// The registered checks the verify runs, in order.
    pub checks: Vec<String>,
    /// The highest worker event the run has handled (acknowledged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_acked_seq: Option<i64>,
    /// The event waiting for `todo.resume`, while `waiting`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_event: Option<i64>,
    /// Why it is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The commit the cherry-pick made on `master`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunEvent {
    /// Named by `todo.resume --event` and `todo.wait --after`.
    pub event_id: i64,
    pub kind: TodoEventKind,
    /// What `todo.resume` takes for it; empty once the run ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<TodoAction>,
    /// With `question`: the worker's pending questions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<WorkerQuestion>,
    /// The branch's changes since the base (`git diff --stat`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    /// The commits since the base, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<String>,
    /// With `review`: the worker's last reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_text: Option<String>,
    /// With `verify_failed`: the verdict and each check's evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<WorkerVerification>,
    /// With `blocked`: why; with `still_alive`: what is still running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ts_ms: u64,
}
