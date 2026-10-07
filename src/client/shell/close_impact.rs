//! What a close would stop: tabs marked running (e.g. herdr-job), agents that
//! are working, waiting for approval or a reply, or have background tasks, and
//! programs a pane's shell started, e.g. a build or `lazygit`, as Ghostty
//! asks. The TUI asks before a close that stops any of them. An agent
//! session in a normal tab counts in every state, also idle; the leftover
//! agent of a finished job's tab counts only while it works or has
//! background tasks.

use crate::api::schema::{AgentStatus, TabStatus};
use crate::protocol::{ClientShellPane, ClientShellSnapshot, ClientShellTab};

/// How many entries the confirmation names before `+N more`.
const NAMED: usize = 3;

/// Running work in the given tabs, one entry per tab or pane, in tab order.
pub(super) fn tabs_running_work(snapshot: &ClientShellSnapshot, tab_ids: &[&str]) -> Vec<String> {
    snapshot
        .tabs
        .iter()
        .filter(|tab| tab_ids.contains(&tab.tab_id.as_str()))
        .flat_map(|tab| tab_running_work(snapshot, tab, None))
        .collect()
}

/// Running work in all tabs of the given workspaces.
pub(super) fn workspaces_running_work(
    snapshot: &ClientShellSnapshot,
    workspace_ids: &[&str],
) -> Vec<String> {
    let tab_ids = snapshot
        .tabs
        .iter()
        .filter(|tab| workspace_ids.contains(&tab.workspace_id.as_str()))
        .map(|tab| tab.tab_id.as_str())
        .collect::<Vec<_>>();
    tabs_running_work(snapshot, &tab_ids)
}

/// Running work in one pane. Closing a tab's last pane closes the tab, so a
/// tab marked running counts then too.
pub(super) fn pane_running_work(snapshot: &ClientShellSnapshot, pane_id: &str) -> Vec<String> {
    let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
        return Vec::new();
    };
    let Some(tab) = snapshot.tabs.iter().find(|tab| tab.tab_id == pane.tab_id) else {
        return Vec::new();
    };
    let last_pane = !snapshot
        .panes
        .iter()
        .any(|other| other.tab_id == tab.tab_id && other.pane_id != pane_id);
    if last_pane {
        tab_running_work(snapshot, tab, None)
    } else {
        tab_running_work(snapshot, tab, Some(pane_id))
    }
}

/// `a, b, c +2 more`, for the confirmation's running line.
pub(super) fn summary(entries: &[String]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let named = entries
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    Some(match entries.len().saturating_sub(NAMED) {
        0 => named,
        more => format!("{named} +{more} more"),
    })
}

fn tab_running_work(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
    only_pane: Option<&str>,
) -> Vec<String> {
    // The job's own status says more than the process running it.
    if only_pane.is_none() && tab.status == Some(TabStatus::Running) {
        return vec![format!("{} marked running", tab.label)];
    }
    // A finished job's tab still runs its wrapper for a few seconds before it
    // closes itself; that process is not work anyone would lose.
    let finished = matches!(
        tab.status,
        Some(TabStatus::Succeeded) | Some(TabStatus::Failed)
    );
    snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab.tab_id)
        .filter(|pane| only_pane.is_none_or(|only| pane.pane_id == only))
        .filter_map(|pane| pane_work(snapshot, tab, pane, finished))
        .collect()
}

fn pane_work(
    snapshot: &ClientShellSnapshot,
    tab: &ClientShellTab,
    pane: &ClientShellPane,
    finished: bool,
) -> Option<String> {
    if let Some(agent) = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane.pane_id)
    {
        let name = agent
            .display_agent
            .as_deref()
            .or(agent.agent.as_deref())
            .unwrap_or("agent");
        // The `$bg` token (`2 bg`) counts its background tasks, which die with it.
        let background = agent
            .tokens
            .iter()
            .find(|(key, value)| key == "bg" && !value.trim().is_empty())
            .map(|(_, value)| value.trim());
        // An interactive agent session in a normal tab is always asked about,
        // also while idle: closing ends the live process and loses its
        // unsent input, queued context and scrollback, and a resume brings
        // back only the saved conversation. (Once an idle agent was left
        // out, and a Claude session closed without a word.) The exception is
        // a finished job's tab, where the agent is the one-shot command's
        // leftover: only a live turn or background tasks count there.
        let live = matches!(
            agent.agent_status,
            AgentStatus::Working | AgentStatus::Blocked
        );
        let state = match agent.agent_status {
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "waiting",
            _ if agent.awaiting_reply => "waiting for a reply",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
            AgentStatus::Unknown => "open",
        };
        if finished && !(live || background.is_some()) {
            return None;
        }
        let background = background.map_or_else(String::new, |tasks| format!(" · {tasks}"));
        return Some(format!("{name} {state} in {}{background}", tab.label));
    }
    if finished {
        return None;
    }
    pane.running_program
        .as_deref()
        .map(|program| format!("{program} in {}", tab.label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientShellAgent;

    fn snapshot() -> ClientShellSnapshot {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.agents.clear();
        snapshot.tabs = vec![tab("t1", "build", None), tab("t2", "claude", None)];
        snapshot.panes = vec![pane("p1", "t1", None), pane("p2", "t2", None)];
        snapshot
    }

    fn tab(tab_id: &str, label: &str, status: Option<TabStatus>) -> ClientShellTab {
        ClientShellTab {
            activity: None,
            bookmarked: false,
            tab_id: tab_id.into(),
            workspace_id: "w1".into(),
            number: 1,
            label: label.into(),
            custom_label: true,
            zoomed: false,
            focused: false,
            agent_status: AgentStatus::Unknown,
            parent_tab_id: None,
            status,
            program: None,
            role: None,
        }
    }

    fn pane(pane_id: &str, tab_id: &str, running_program: Option<&str>) -> ClientShellPane {
        ClientShellPane {
            pane_id: pane_id.into(),
            workspace_id: "w1".into(),
            tab_id: tab_id.into(),
            label: None,
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
            running_program: running_program.map(str::to_owned),
        }
    }

    fn agent(pane_id: &str, tab_id: &str, status: AgentStatus) -> ClientShellAgent {
        ClientShellAgent {
            task: None,
            question: None,
            waiting_since_ms: None,
            limited: None,
            pane_id: pane_id.into(),
            workspace_id: "w1".into(),
            tab_id: tab_id.into(),
            name: None,
            display_agent: Some("claude".into()),
            agent: Some("claude".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 0,
            awaiting_reply: false,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    #[test]
    fn idle_shells_are_not_running_work() {
        assert!(tabs_running_work(&snapshot(), &["t1", "t2"]).is_empty());
    }

    #[test]
    fn an_idle_agent_session_in_a_normal_tab_is_still_asked_about() {
        let mut snapshot = snapshot();
        snapshot.agents.push(agent("p2", "t2", AgentStatus::Idle));
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude idle in claude".to_owned()]
        );
        snapshot.agents[0].agent_status = AgentStatus::Done;
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude done in claude".to_owned()]
        );
        snapshot.agents[0]
            .tokens
            .push(("bg".to_owned(), "2 bg".to_owned()));
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude done in claude · 2 bg".to_owned()]
        );
        snapshot.agents[0].tokens.clear();
        snapshot.agents[0].awaiting_reply = true;
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude waiting for a reply in claude".to_owned()]
        );
        snapshot.agents[0].agent_status = AgentStatus::Unknown;
        snapshot.agents[0].awaiting_reply = false;
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude open in claude".to_owned()]
        );
    }

    #[test]
    fn an_agent_left_in_a_finished_job_tab_is_not_work_unless_live() {
        let mut snapshot = snapshot();
        snapshot.tabs[0].status = Some(TabStatus::Succeeded);
        snapshot.agents.push(agent("p1", "t1", AgentStatus::Idle));
        assert!(tabs_running_work(&snapshot, &["t1"]).is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Unknown;
        assert!(tabs_running_work(&snapshot, &["t1"]).is_empty());
        snapshot.agents[0].agent_status = AgentStatus::Working;
        assert_eq!(
            tabs_running_work(&snapshot, &["t1"]),
            vec!["claude working in build".to_owned()]
        );
        snapshot.agents[0].agent_status = AgentStatus::Idle;
        snapshot.agents[0]
            .tokens
            .push(("bg".to_owned(), "1 bg".to_owned()));
        assert_eq!(
            tabs_running_work(&snapshot, &["t1"]),
            vec!["claude idle in build · 1 bg".to_owned()]
        );
    }

    #[test]
    fn running_tabs_and_busy_agents_are_running_work() {
        let mut snapshot = snapshot();
        snapshot.tabs[0].status = Some(TabStatus::Running);
        snapshot
            .agents
            .push(agent("p2", "t2", AgentStatus::Blocked));
        assert_eq!(
            workspaces_running_work(&snapshot, &["w1"]),
            vec![
                "build marked running".to_owned(),
                "claude waiting in claude".to_owned()
            ]
        );
        snapshot.agents[0].agent_status = AgentStatus::Working;
        assert_eq!(
            tabs_running_work(&snapshot, &["t2"]),
            vec!["claude working in claude".to_owned()]
        );
    }

    #[test]
    fn a_finished_job_tab_ignores_its_wrapper_process() {
        let mut snapshot = snapshot();
        snapshot.tabs[0].status = Some(TabStatus::Succeeded);
        snapshot.panes[0].running_program = Some("python3".into());
        assert!(tabs_running_work(&snapshot, &["t1"]).is_empty());
    }

    #[test]
    fn programs_the_shell_started_are_running_work() {
        let mut snapshot = snapshot();
        snapshot.panes[0].running_program = Some("lazygit".into());
        assert_eq!(
            tabs_running_work(&snapshot, &["t1"]),
            vec!["lazygit in build".to_owned()]
        );
        // A running tab names the job, not the process running it.
        snapshot.tabs[0].status = Some(TabStatus::Running);
        assert_eq!(
            tabs_running_work(&snapshot, &["t1"]),
            vec!["build marked running".to_owned()]
        );
    }

    #[test]
    fn closing_one_of_several_panes_counts_only_that_pane() {
        let mut snapshot = snapshot();
        snapshot.tabs[0].status = Some(TabStatus::Running);
        snapshot.panes.push(pane("p3", "t1", None));
        snapshot
            .agents
            .push(agent("p3", "t1", AgentStatus::Working));
        assert!(pane_running_work(&snapshot, "p1").is_empty());
        assert_eq!(
            pane_running_work(&snapshot, "p3"),
            vec!["claude working in build".to_owned()]
        );
        // The last pane takes the tab, and its running job, with it.
        snapshot.panes.retain(|pane| pane.pane_id != "p3");
        assert_eq!(
            pane_running_work(&snapshot, "p1"),
            vec!["build marked running".to_owned()]
        );
    }

    #[test]
    fn summary_names_three_then_counts_the_rest() {
        let entries = ["a", "b", "c", "d", "e"].map(str::to_owned);
        assert_eq!(summary(&entries).as_deref(), Some("a, b, c +2 more"));
        assert_eq!(summary(&[]), None);
    }
}
