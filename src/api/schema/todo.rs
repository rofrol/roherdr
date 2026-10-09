use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::workers::{WorkerDecision, WorkerQuestion, WorkerVerification};

/// Drives one TODO item from preflight to a cherry-pick onto the
/// repository's `master` and on through the registered install, the TODO
/// update, a fast-forward push to `origin` and the cleanup of its merged
/// branches: a persisted, resumable run that starts a headless worker in the
/// repository's folder slot, turns what needs the coordinator (a question
/// outside the worker policy, the turn's end, a failed verify, install, TODO
/// edit or push) into run events, and verifies the worker's commit with a
/// registered check before picking it.
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
    /// The coordinator's pane, which owns the run's workers (their
    /// questions, obligations and coordination tenure) and resumes the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    /// The coordinator's workspace, which lists the run's workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The caller's environment, which the check runs with (`HERDR_*`
    /// dropped); kept in the server's memory only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Start even while the usage gate refuses (Claude's usage is high or
    /// unknown), on the user's word; recorded in the run's events.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub ignore_usage: bool,
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
    /// With `retry` (of a `review` or `verify_failed` event): the review of
    /// the attempt, which the next attempt's task appends to this one's.
    /// Refused after `retry_conflict`, which keeps the review.
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
    /// With `approve`: lines the run appends to the item in `TODO.md` after
    /// the install (`scripts/todo_edit.py append-to`), committed by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// With `approve`: the decision that closes the item. The run removes
    /// the item from `TODO.md` and adds the decision to `DECISIONS.md` as a
    /// section (its first line, when it starts with `#`, is the section's
    /// title; otherwise the item's title is), committed by path. Excludes
    /// `note`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<String>,
    /// The caller's pane. A run owned by another pane that is still there
    /// is refused (`run_owned_elsewhere`); once the owner's pane or agent
    /// is gone, the caller takes the run over: its pane, agent session and
    /// workspace then own the run and its later workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_workspace_id: Option<String>,
    /// With `retry`: start the next attempt even while the usage gate
    /// refuses, on the user's word; recorded in the run's events.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub ignore_usage: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoWaitParams {
    pub run_id: String,
    /// The `event_id` the previous wait returned: only a later event
    /// counts. A run that ended (`done`, `blocked`, `aborted`) returns its
    /// last event however old.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoRunsParams {
    /// Only the runs of this repository: a directory in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Only the run that landed this commit (a sha or a prefix of at least
    /// 7 characters, the landed commit or the worker's): found in the
    /// store's landings, else by the `Herdr-Run` trailer of the commit in
    /// `repo`'s history. Refused with `todo_run_not_found` when neither
    /// names a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Where a landing was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoLandingSource {
    /// The worker store's record, written when the run landed the commit.
    Store,
    /// The commit's `Herdr-Item` and `Herdr-Run` trailers (the store has no
    /// record of it: another machine's run, or a sha the upstream rebase
    /// changed).
    Trailers,
    #[serde(other)]
    Unknown,
}

/// A commit a run landed on `master`, with the run and attempt it came
/// from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoLanding {
    /// The commit on `master`, which carries the trailers.
    pub landed_sha: String,
    pub run_id: String,
    pub attempt: u32,
    pub item: String,
    /// The worker's commit it was picked from (subject only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_commit: Option<String>,
    /// The attempt's worker, whose journal is the run's transcript; absent
    /// when the store does not know the attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    /// Unix milliseconds of the landing; absent when found by trailers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_ms: Option<u64>,
    pub source: TodoLandingSource,
}

/// What the coordinator may answer an event with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoAction {
    /// The worker's turn is accepted: stop it, verify, cherry-pick. The
    /// approval is bound to the commit and base the event showed: a branch
    /// that moved since is reviewed again instead of landed.
    Approve,
    /// Start the next attempt, its task text being the review of this one
    /// (at most 3 attempts): the next attempt's branch starts from this
    /// attempt's commit, and its task is this attempt's with the review
    /// appended. After `retry_conflict`: start that attempt from the base
    /// instead, without the previous commit (no task text).
    Retry,
    /// Answer the worker's pending question.
    Answer,
    /// Run the verify again with the environment this resume sends (after
    /// a check was `unavailable`).
    Verify,
    /// SIGKILL the worker that is still alive after its stop.
    ForceStop,
    /// Run the registered install again (after `install_failed`).
    RetryInstall,
    /// Go on without the install (after `install_failed`).
    SkipInstall,
    /// Edit and commit the TODO again (after `todo_failed`).
    RetryTodo,
    /// Go on without the TODO edit (after `todo_failed`).
    SkipTodo,
    /// Push again (after `push_failed`), still only as a fast-forward.
    RetryPush,
    /// End the run as `aborted` where it stands, from any event it waits
    /// on or from `blocked`: its worker is stopped when it still runs, and
    /// its branches and commits (also one already on `master`) stay as
    /// they are.
    Abort,
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
    /// Runs the repository's registered install command.
    Install,
    /// Appends the coordinator's note to the item, or closes it into
    /// `DECISIONS.md`, and commits that by path.
    Todo,
    /// Pushes `master` to `origin`, only as a fast-forward.
    Push,
    /// Deletes the run's branches that are fully merged.
    Cleanup,
    Done,
    /// The coordinator aborted the run: its worker is stopped, then the
    /// run ends `aborted`.
    Abort,
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
    /// attempts used up. Nothing more happens unless the coordinator
    /// aborts it.
    Blocked,
    /// The commit is on `master`, installed, recorded and pushed (each as
    /// far as registered and approved), and the merged branches are gone.
    Done,
    /// The coordinator aborted it; its worker exited, its branches and
    /// commits are left as they were.
    Aborted,
    #[serde(other)]
    Unknown,
}

/// A run's event that needs the coordinator or ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoEventKind {
    /// The worker asked a question the worker policy does not decide.
    Question,
    /// The worker's turn ended (or the worker did): review its work. Also
    /// raised before the cherry-pick when the branch's tip is not the
    /// commit the approval named (`error` says so): review it again.
    Review,
    /// The verify failed or could not run: give the next attempt's task,
    /// or verify again.
    VerifyFailed,
    /// Raised by `todo.status` while the worker has not exited since its
    /// stop (it ignores SIGTERM): force-stop it. The run goes on by itself
    /// when the worker exits first.
    StillAlive,
    /// The registered install failed or could not run: retry it, skip it
    /// or abort.
    InstallFailed,
    /// The TODO edit or its commit failed: retry it, skip it or abort.
    TodoFailed,
    /// `master` could not be pushed as a fast-forward (or the push failed):
    /// retry it or abort. Never forced.
    PushFailed,
    /// The run is blocked; it takes `abort`.
    Blocked,
    Done,
    /// The run was aborted: why, its branches and its commits.
    Aborted,
    /// The previous attempt's commit does not cherry-pick onto the next
    /// attempt's branch (`error` names the conflict): retry starts that
    /// attempt from the base without it, or abort.
    RetryConflict,
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
    /// The current attempt's task text: the first attempt's is the run's,
    /// each later one is the previous attempt's with its review appended.
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
    /// Why it is blocked or was aborted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The commit the cherry-pick made on `master`, with the `Herdr-Item`
    /// and `Herdr-Run` trailers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picked: Option<String>,
    /// The build the install reported (the registered `build_id` command's
    /// first line), or `installed` when none is registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_build: Option<String>,
    /// The commit of the TODO update on `master`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_commit: Option<String>,
    /// The `master` commit `origin` has after the push.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed: Option<String>,
    /// Merged branches of the run the cleanup kept because a worktree has
    /// them checked out (the folder slot); a later run's cleanup deletes
    /// them once the slot moved on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_branches: Vec<String>,
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
    /// With `blocked` and `aborted`: why (`aborted` also names the run's
    /// branches); with `still_alive`: what is still running; with a
    /// `review` raised before the cherry-pick: the approved commit and the
    /// branch's tip; with `retry_conflict`: the commit and the conflict;
    /// with `install_failed`, `todo_failed` and `push_failed`: what failed,
    /// with the output's last lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub ts_ms: u64,
}
