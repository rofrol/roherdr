//! The worker types the rest of the server reads: what a wait for
//! attention returns, notices and pending questions for the clients, owner
//! events, the sidebar's summaries and item counts, and what a takeover
//! hands over or recovery finds.

use crate::api::schema::{WorkerAttentionReason, WorkerInfo, WorkerQuestion, WorkerState};

/// What `worker.wait` with `until: attention` returns
/// ([`WorkerSupervisor::wait_attention`]).
#[derive(Debug, Clone)]
pub(crate) struct Attention {
    pub(crate) reason: WorkerAttentionReason,
    pub(crate) questions: Vec<WorkerQuestion>,
    pub(crate) seq: i64,
    pub(crate) worker: WorkerInfo,
}

/// A notification a driver raised for the user (a todo run's stop reason),
/// which the server sends to its client shells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserNotice {
    pub(crate) title: String,
    pub(crate) body: String,
    /// The pane a click on it goes to (a supervision finding's to the herdr
    /// coordinator's pane), by its public id; none for the user's own.
    pub(crate) pane_id: Option<String>,
}

/// A question some worker waits on, with the worker it belongs to.
#[derive(Debug, Clone)]
pub(crate) struct PendingWorkerQuestion {
    pub(crate) worker_id: String,
    pub(crate) cwd: String,
    pub(crate) question: WorkerQuestion,
    /// It waits for the worker's owner, not for the user.
    pub(crate) quiet: bool,
    /// When the owner last showed an event herdr saw; display only.
    pub(crate) owner_seen_ms: Option<u64>,
}

/// What a worker owner's pane or agent did, as herdr's own events report
/// it ([`WorkerSupervisor::owner_event`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnerEvent {
    /// The pane closed: the owner is gone for good.
    PaneClosed,
    /// The agent in it exited: the owner is gone for good.
    AgentExited,
    /// Its turn ended on a usage or credit limit (`pane.report_limit`).
    Limited,
    /// It waits on its own question to the user.
    Blocked,
    /// It went idle or done: a question still unanswered then was not
    /// answered in its turn (acknowledging is not answering).
    TurnEnded,
    /// It works again: it can handle new questions.
    Working,
}

impl OwnerEvent {
    /// What an owner pane's agent status change means: its agent left the
    /// pane (`released`), works again, is blocked, or went idle or done from
    /// a state that was neither (a turn's end; idle and done only differ in
    /// whether the user has seen it).
    pub(crate) fn from_agent_status(
        previous: crate::api::schema::AgentStatus,
        now: crate::api::schema::AgentStatus,
        released: bool,
    ) -> Option<Self> {
        use crate::api::schema::AgentStatus;
        let resting = |status| matches!(status, AgentStatus::Idle | AgentStatus::Done);
        if released {
            Some(Self::AgentExited)
        } else if previous == now {
            None
        } else if now == AgentStatus::Working {
            Some(Self::Working)
        } else if now == AgentStatus::Blocked {
            Some(Self::Blocked)
        } else if resting(now) && !resting(previous) {
            Some(Self::TurnEnded)
        } else {
            None
        }
    }

    /// Why it hands its workers' quiet questions to the user, if it does.
    pub(super) fn cause(self) -> Option<&'static str> {
        Some(match self {
            Self::PaneClosed => "the coordinator's pane closed",
            Self::AgentExited => "the coordinator's agent exited",
            Self::Limited => "the coordinator hit a usage or credit limit",
            Self::Blocked => "the coordinator is blocked on its own question to the user",
            Self::TurnEnded => "the coordinator ended its turn without answering",
            Self::Working => return None,
        })
    }
}

/// What the sidebar shows of a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSummary {
    pub(crate) worker_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) state: WorkerState,
    pub(crate) session_id: Option<String>,
    pub(crate) takeover: bool,
    /// The TODO item it works on and that item's title when it started.
    pub(crate) item: Option<String>,
    pub(crate) item_title: Option<String>,
    /// Its folder slot and the branch checked out there.
    pub(crate) slot: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) turns: u32,
    /// When it started, Unix milliseconds.
    pub(crate) started_ms: Option<u64>,
    /// When its running turn began; none between turns and once it ended.
    pub(crate) turn_started_ms: Option<u64>,
    /// When its latest event was recorded.
    pub(crate) last_event_ms: Option<u64>,
    /// Whether the sidebar lists it ([`Status::in_sidebar`]): while it
    /// works or asks, and after its turn or its end until its owner
    /// acknowledges it.
    pub(crate) listed: bool,
    /// Its process is gone (exited, or lost with a server).
    pub(crate) gone: bool,
    /// What ended it beyond its state, e.g. `ended by a server restart`
    /// for one lost between turns, which keeps its last turn's state.
    pub(crate) end_note: Option<String>,
    /// Nothing waits on anyone: its turn or process ended, no question is
    /// open and its owner, if any, acknowledged every event
    /// ([`Status::settled`]).
    pub(crate) quiet: bool,
    /// The pane of the coordinator that owns it.
    pub(crate) owner_pane: Option<String>,
}

/// How many TODO items of one repository have workers on them, for the
/// coordinator's Items button: items with a running worker, and items with
/// an ended run whose owner has not acknowledged its end (a result or a
/// failure to review).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ItemCounts {
    pub(crate) in_progress: u32,
    pub(crate) attention: u32,
}

/// What a takeover hands to the tab that resumes the worker's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Takeover {
    pub(crate) worker_id: String,
    pub(crate) workspace_id: Option<String>,
    pub(crate) name: String,
    pub(crate) cwd: String,
    pub(crate) session_id: String,
    /// The claim's id, which the tab carries in [`TAKEOVER_ID_ENV`] and in
    /// its title.
    pub(crate) takeover_id: String,
}

/// The environment variable a takeover tab is created with: its claim's id.
pub(crate) const TAKEOVER_ID_ENV: &str = "HERDR_TAKEOVER_ID";

/// A takeover whose claim an earlier server recorded without its tab: what
/// recovery looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnfinishedTakeover {
    pub(crate) worker_id: String,
    /// None for a claim recorded before takeovers had ids: then only a
    /// process resuming the session can be found.
    pub(crate) takeover_id: Option<String>,
    pub(crate) session_id: Option<String>,
}

/// A tab recovery may adopt: its id, its title and the pids of its panes'
/// shells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TakeoverTabCandidate {
    pub(crate) tab_id: String,
    pub(crate) title: String,
    pub(crate) shell_pids: Vec<u32>,
}

/// What recovery found of an unfinished takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TakeoverFound {
    /// The tab that carries its id, or whose pane runs a process resuming
    /// its session.
    Tab(String),
    /// A process resuming its session outside herdr's tabs.
    Process(u32),
    Nothing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_status_changes_map_to_owner_events() {
        use crate::api::schema::AgentStatus::{Blocked, Done, Idle, Unknown, Working};
        let map = OwnerEvent::from_agent_status;
        assert_eq!(map(Working, Idle, false), Some(OwnerEvent::TurnEnded));
        assert_eq!(map(Working, Done, false), Some(OwnerEvent::TurnEnded));
        assert_eq!(map(Blocked, Idle, false), Some(OwnerEvent::TurnEnded));
        assert_eq!(map(Unknown, Idle, false), Some(OwnerEvent::TurnEnded));
        // Seen or not is no turn's end.
        assert_eq!(map(Done, Idle, false), None);
        assert_eq!(map(Idle, Done, false), None);
        assert_eq!(map(Idle, Idle, false), None);
        assert_eq!(map(Idle, Working, false), Some(OwnerEvent::Working));
        assert_eq!(map(Working, Blocked, false), Some(OwnerEvent::Blocked));
        assert_eq!(map(Working, Unknown, false), None);
        assert_eq!(map(Working, Working, true), Some(OwnerEvent::AgentExited));
    }
}
