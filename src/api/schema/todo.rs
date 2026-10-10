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
    /// The server reviews each `review` event itself with a bounded,
    /// stateless model call (`claude -p`, structured output, no tools) and
    /// applies its typed decision: `approve` (bound to the event's commit
    /// and base), `retry` (its review text) or `escalate` (a question for
    /// the user, a new `review` event). A call that fails or returns
    /// invalid output is retried once, then escalated. `todo.resume` still
    /// answers the event and wins when it comes first. The reply's
    /// `auto_review` says whether the server took it.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub auto_review: bool,
    /// The server answers each question the worker policy leaves (a
    /// `question` event) itself with a bounded, stateless model call that
    /// returns `allow`, `deny` (with a message), `answer` (an
    /// `AskUserQuestion`'s answers) or `escalate` (a question for the user,
    /// which also enters the user's `?` list). A request herdr's policy
    /// leaves to the user (a classifier escalation, a path outside the
    /// worker's folders, a tool it does not know) is only escalated. The
    /// reply's `auto_answer` says whether the server took it.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub auto_answer: bool,
}

/// `todo.run` whose worker's task the server drafts itself: a bounded,
/// stateless model call (`draft_task`) with the item's text, the
/// `DECISIONS.md` sections it names, the repository's code and test rules
/// (`AGENTS.md`), its registered checks, `git log -20` and the item's
/// history returns the task text, the exact commit subject, the path globs
/// and the check names, or a question for the user. The server checks the
/// draft (a lowercase conventional subject, relative globs inside the
/// repository, registered checks), records it and starts the run with it as
/// `todo.run` with those parameters would; an invalid draft or a failed call
/// is asked once more, then escalated. The run is reviewed and its worker's
/// questions answered by the server too (`auto_review`, `auto_answer`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoDraftRunParams {
    /// A directory in the repository (the CLI sends its working directory).
    pub cwd: String,
    /// The TODO item's id (`t-abcd2345`); it must be in the repository's
    /// `TODO.md`.
    pub item: String,
    /// The coordinator's pane, which owns the run (as `todo.run`'s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The caller's environment, which the checks run with (`HERDR_*`
    /// dropped); kept in the server's memory only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// Start even while the usage gate refuses, on the user's word.
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
    /// `note`. A close takes `next` or `stop_reason`, never neither: the
    /// command that closes an item also carries what comes after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close: Option<String>,
    /// With `close`: the item whose run the driver starts as soon as this
    /// run is `done`, in the same repository, owned by this run's owner.
    /// Recorded as intent before the start and result after it (a server
    /// restart in between starts it then); a refused start (its preflight,
    /// the usage gate, a run in progress) is a `next_refused` event on this
    /// run. Excludes `stop_reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<TodoNextRun>,
    /// With `close`: why no next item is started. Recorded in the item's
    /// history (`stopped`) when the run is `done`, and shown to the user as
    /// a notification. Excludes `next`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
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

/// The run a closing approval starts when its run is done: `todo.run`'s
/// parameters for the next item, in the closed run's repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoNextRun {
    /// The next item's id; it must be in `TODO.md` when the run starts.
    pub item: String,
    /// Its worker's task text.
    pub task: String,
    /// Its exact commit subject.
    pub message: String,
    /// Its git glob pathspecs.
    pub paths: Vec<String>,
    /// Its checks; the closed run's checks when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
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
    /// The server drafts the worker's task (`todo.draft_run`), then
    /// starts it.
    Draft,
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
    /// After `done`: the next item the close named did not start (`error`
    /// names the refusal: its preflight, the usage gate, a run in
    /// progress). The run stays done; start the item by hand.
    NextRefused,
    /// The server's draft of the worker's task needs an answer (`error`
    /// holds the question and its options, or why no valid draft came):
    /// `retry` with the answer as task text drafts again with it, or abort.
    /// The user can answer it from the `?` list too.
    Draft,
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
    /// The item the close named to start next (`todo.resume`'s `next`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_item: Option<String>,
    /// The run of `next_item` the driver started once this run was done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_id: Option<String>,
    /// Why `next_item` did not start (also a `next_refused` event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_refusal: Option<String>,
    /// Why the close started no next item (`todo.resume`'s `stop_reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// The server reviews the run's `review` events itself
    /// (`todo.run`'s `auto_review`).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub auto_review: bool,
    /// The server answers the questions of the run's worker itself
    /// (`todo.run`'s `auto_answer`).
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub auto_answer: bool,
    /// The server drafts the run's task, subject, paths and checks itself
    /// (`todo.draft_run`); they are empty until the draft is applied.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub drafted: bool,
    /// The repository's queue started the run (`todo.queue_set`): an
    /// approval of its own review closes the item.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub queued: bool,
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

/// `todo.review`: one attempt of a run as the coordinator reviews it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoReviewParams {
    pub run_id: String,
    /// The attempt (1 for the first); the run's current one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Also the full diff (`git diff base commit`), not only its stat.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub diff: bool,
}

/// One attempt of a run in one view: the run's `attempts` row, the
/// worker's journal (its transcript) and `git diff base commit` in the
/// run's repository. Nothing of it is stored apart from those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoReview {
    pub run_id: String,
    pub item: String,
    pub repo: String,
    pub status: TodoRunStatus,
    pub attempt: u32,
    /// How many attempts the run has had so far.
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit the attempt is verified and reviewed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The attempt's commit: the one the coordinator's decision saw, else
    /// the verify's head, else the branch's tip when it is past the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The previous attempt and its commit this one's branch starts from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_commit: Option<String>,
    /// The task the worker got: its first prompt, the run's contract
    /// appended; the run's stored task when its journal is not there.
    pub task: String,
    /// The worker's last reply (its last turn's result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<String>,
    /// The questions the worker asked, oldest first, each with its answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<TodoReviewQuestion>,
    /// The tool calls that failed or were denied, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_failures: Vec<TodoReviewToolFailure>,
    /// `git diff --stat base commit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_stat: Option<String>,
    /// `git diff base commit`, when asked for (`diff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// Why the diff could not be read (no commit yet, git failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_error: Option<String>,
    /// Herdr's latest verify of the attempt: each check with its outcome
    /// and evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<WorkerVerification>,
    /// The coordinator's decision on the attempt: `approve`, `retry` or
    /// `abort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// With `retry`: the review the next attempt got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_text: Option<String>,
    /// With `approve`: the commit and base the approval is bound to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<TodoApproval>,
    /// The commit on `master` the attempt landed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_sha: Option<String>,
    /// The server's draft the run started with (`todo.draft_run`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<TodoDraft>,
}

/// A run's task as the server drafted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoDraft {
    /// The decision's id (`d-...`).
    pub decision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub task: String,
    pub message: String,
    pub paths: Vec<String>,
    pub checks: Vec<String>,
}

/// The `(commit, base)` an approval is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoApproval {
    pub commit: String,
    pub base: String,
}

/// A question the worker asked and how it was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoReviewQuestion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub tool_name: String,
    pub text: String,
    /// The answer (`allow`, `deny` or the choices); absent while it waits
    /// or when it was never answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// A tool call that failed or was denied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoReviewToolFailure {
    pub kind: TodoToolFailureKind,
    pub tool_name: String,
    /// The call's most telling input: the command, the path or the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// The error, or who denied it and why.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoToolFailureKind {
    /// The tool ran and returned an error.
    Failed,
    /// The policy, a pre-tool check, the user or the CLI's permission
    /// mode denied the call.
    Denied,
    #[serde(other)]
    Unknown,
}

/// Starts a fresh headless item coordinator for the top item of the
/// repository's `TODO.md` "Next, in order": a Claude worker in the
/// repository with herdr's coordinator prompt and allowlist, under a
/// headless coordination tenure of its own, owned by the caller's pane. It
/// drives the item with `todo.run` and ends; with `chain`, each one that
/// ends with its item done starts the next on its exit event, until "Next,
/// in order" is empty, an item escalates to the user or fails, or
/// `todo.stop`. Refused while the repository has a run in progress, an
/// active coordinator (`coordinator_active`), or the usage gate refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoNextParams {
    /// A directory in the repository (the CLI sends `--repo` or its working
    /// directory).
    pub cwd: String,
    /// Go on with the next item when this one is done.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub chain: bool,
    /// The pane that owns the item coordinators (their questions and
    /// obligations); the CLI sends its `HERDR_PANE_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The caller's environment (`HERDR_*` dropped), which the checks, the
    /// install and the push of the coordinators' runs run with instead of
    /// a coordinator's sandboxed one; kept in the server's memory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// Stops the repository's chain of item coordinators: the one that runs
/// finishes its item, and no next one starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoStopParams {
    /// A directory in the repository.
    pub cwd: String,
}

/// A repository's chain of item coordinators (`todo.next` with `chain`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoChainInfo {
    pub repo: String,
    /// Whether a coordinator that ends with its item done starts the next.
    pub active: bool,
    /// The pane that started it, which owns its coordinators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    /// Unix milliseconds.
    pub started_ms: u64,
    /// The latest coordinator's item and outcome (`done`, `escalated`,
    /// `blocked`, `failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_ms: Option<u64>,
    /// Why it stopped: `todo.stop`, an item that escalated or failed, an
    /// empty "Next, in order", or a next start herdr refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// A repository's queue mode: whether the server starts the top runnable
/// item of "Next, in order" by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoQueueMode {
    On,
    Paused,
    #[serde(other)]
    Unknown,
}

/// What a repository's queue does now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoQueueStatus {
    /// A run (or a headless item coordinator) of the repository is in
    /// progress.
    Running,
    /// The queue is paused, or the run in progress waits on an event only
    /// the coordinator or the user answers (a failed install or push).
    WaitingOnUser,
    /// The run in progress waits on an escalation in the user's `?` list.
    EscalationPending,
    /// The usage gate refused the next start.
    UsageGate,
    /// Every item left in "Next, in order" is blocked, or the next start
    /// was refused (preflight).
    Blocked,
    /// "Next, in order" has no open item.
    Empty,
    #[serde(other)]
    Unknown,
}

/// `todo.queue_set`: turns a repository's queue mode on or pauses it. On,
/// the server starts the top item of "Next, in order" that is not blocked
/// as `todo.draft_run` would whenever no run of the repository is active, no
/// escalation is pending and the usage gate admits; it looks again only on
/// its own events: a run ended (done, blocked or aborted), an escalation was
/// answered, the server started, the mode was set. Turning it on also clears
/// a pause of the circuit breaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoQueueSetParams {
    /// A directory in the repository.
    pub cwd: String,
    pub mode: TodoQueueMode,
    /// Why it pauses (`paused` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The pane that owns the runs the queue starts; the CLI sends its
    /// `HERDR_PANE_ID`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The caller's environment (`HERDR_*` dropped), which the checks, the
    /// install and the push of the queue's runs run with; kept in the
    /// server's memory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
}

/// `todo.queue_status`: a repository's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoQueueTarget {
    /// A directory in the repository.
    pub cwd: String,
}

/// An item the queue no longer starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoQueueBlockedItem {
    pub item: String,
    pub reason: String,
}

/// A repository's queue as `todo.queue_set` and `todo.queue_status` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TodoQueueInfo {
    pub repo: String,
    pub mode: TodoQueueMode,
    pub status: TodoQueueStatus,
    /// Why the queue is in that status.
    pub reason: String,
    /// The run in progress of the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The item of that run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// Why the mode is paused (the user's reason or the circuit breaker's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<String>,
    /// Consecutive queue runs that ended blocked or aborted; the circuit
    /// breaker pauses the queue at 3.
    pub failures: u32,
    /// Items of "Next, in order" the queue skips: their attempt cap was
    /// reached with their current text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_items: Vec<TodoQueueBlockedItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pane_id: Option<String>,
    /// When the mode or the last evaluation was recorded (Unix
    /// milliseconds); 0 for a repository that never had queue mode.
    pub updated_ms: u64,
}
