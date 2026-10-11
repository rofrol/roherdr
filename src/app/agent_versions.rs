//! "Restart pending" after an agent CLI updates on disk (user, 2026-10-06):
//! herdr only marks the agents that still run the old version; the user
//! restarts them (the "Restart agents" menu). Never an automatic restart.
//!
//! The version an agent runs is its launcher's `--version` (the `claude` or
//! `pi` found on `PATH`), checked when the agent starts in a pane. The
//! version installed is the same check run again when the launcher changes
//! on disk (a watch on the launcher, its symlink target and their
//! directories) or when another pane starts the same agent. The launcher's
//! file identity (symlink target, mtime, size) is only a hint that a check
//! is worth running; the version strings decide. No polling timer.
//!
//! [`AgentVersions`] is pure state in [`super::state::AppState`]; [`App`]
//! runs the checks and owns the watch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::detect::Agent;
use crate::events::AppEvent;
use crate::terminal::TerminalId;

use super::App;

/// delay: hang guard for an external `--version` process that never exits;
/// a check that times out counts as failed and changes nothing.
const VERSION_CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// The agents whose updates herdr tracks: those the "Restart agents" menu
/// can restart and resume.
pub(crate) fn tracks_version(agent: Agent) -> bool {
    matches!(agent, Agent::Claude | Agent::Pi)
}

/// What a launcher looks like on disk: when it is unchanged, so is the
/// version, and no check runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LauncherStamp {
    /// The file the launcher resolves to through its symlinks.
    pub(crate) target: PathBuf,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) len: u64,
}

impl LauncherStamp {
    pub(crate) fn read(launcher: &Path) -> Option<Self> {
        let target = std::fs::canonicalize(launcher).ok()?;
        let metadata = std::fs::metadata(&target).ok()?;
        Some(Self {
            target,
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

/// One `--version` run of a launcher. Its id orders it against the agents
/// that started while it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VersionCheck {
    pub(crate) launcher: PathBuf,
    pub(crate) id: u64,
}

#[derive(Debug, Default)]
struct LauncherState {
    /// What its last successful check printed.
    installed: Option<String>,
    /// What the launcher looked like before that check.
    stamp: Option<LauncherStamp>,
    next_id: u64,
    running: Option<u64>,
    /// Asked for again while a check ran: run one more when it ends.
    rerun: bool,
}

#[derive(Debug)]
struct RunningAgent {
    agent: Agent,
    /// Its process, when detection knew it.
    pid: Option<u32>,
    launcher: PathBuf,
    /// The first check that started after the agent did: its result is the
    /// version the agent runs.
    first_check: u64,
    version: Option<String>,
}

/// The versions agents run and their launchers have installed.
#[derive(Debug, Default)]
pub struct AgentVersions {
    launchers: HashMap<PathBuf, LauncherState>,
    agents: HashMap<TerminalId, RunningAgent>,
}

impl AgentVersions {
    /// An agent started in `terminal` from `launcher`; returns the check to
    /// run unless one already runs (then one more runs after it). The same
    /// process detected again (`pid`) keeps the version it was found to run.
    pub(crate) fn agent_started(
        &mut self,
        terminal: TerminalId,
        agent: Agent,
        pid: Option<u32>,
        launcher: PathBuf,
    ) -> Option<VersionCheck> {
        if pid.is_some()
            && self.agents.get(&terminal).is_some_and(|running| {
                running.agent == agent && running.pid == pid && running.launcher == launcher
            })
        {
            return None;
        }
        let first_check = self.launchers.entry(launcher.clone()).or_default().next_id;
        self.agents.insert(
            terminal,
            RunningAgent {
                agent,
                pid,
                launcher: launcher.clone(),
                first_check,
                version: None,
            },
        );
        self.request(launcher)
    }

    /// The agent in `terminal` is gone or is one herdr does not track.
    pub(crate) fn forget(&mut self, terminal: &TerminalId) {
        self.agents.remove(terminal);
    }

    /// Drops the agents of terminals that no longer exist.
    pub(crate) fn retain_terminals(&mut self, exists: impl Fn(&TerminalId) -> bool) {
        self.agents.retain(|terminal, _| exists(terminal));
    }

    /// Something changed in a directory the launcher lives in; returns the
    /// check to run when the launcher no longer looks like it did at its
    /// last check.
    pub(crate) fn launcher_touched(
        &mut self,
        launcher: &Path,
        stamp: Option<&LauncherStamp>,
    ) -> Option<VersionCheck> {
        let state = self.launchers.get(launcher)?;
        if stamp.is_some() && state.stamp.as_ref() == stamp && state.installed.is_some() {
            return None;
        }
        self.request(launcher.to_path_buf())
    }

    fn request(&mut self, launcher: PathBuf) -> Option<VersionCheck> {
        let state = self.launchers.entry(launcher.clone()).or_default();
        if state.running.is_some() {
            state.rerun = true;
            return None;
        }
        let id = state.next_id;
        state.next_id += 1;
        state.running = Some(id);
        Some(VersionCheck { launcher, id })
    }

    /// A check ended: `version` is what it printed (`None` when it failed)
    /// and `stamp` what the launcher looked like before it ran. Returns
    /// whether a restart-pending mark may have changed, and the next check
    /// to run when it was asked for again meanwhile.
    pub(crate) fn check_finished(
        &mut self,
        check: &VersionCheck,
        version: Option<String>,
        stamp: Option<LauncherStamp>,
    ) -> (bool, Option<VersionCheck>) {
        let Some(state) = self.launchers.get_mut(&check.launcher) else {
            return (false, None);
        };
        if state.running != Some(check.id) {
            return (false, None);
        }
        state.running = None;
        let changed = if let Some(version) = version {
            state.installed = Some(version.clone());
            state.stamp = stamp;
            for agent in self.agents.values_mut() {
                if agent.launcher == check.launcher
                    && agent.version.is_none()
                    && agent.first_check <= check.id
                {
                    agent.version = Some(version.clone());
                }
            }
            true
        } else {
            false
        };
        let rerun = std::mem::take(&mut state.rerun);
        let next = rerun
            .then(|| self.request(check.launcher.clone()))
            .flatten();
        (changed, next)
    }

    /// The launchers with a running agent, which the watch covers.
    pub(crate) fn launchers(&self) -> impl Iterator<Item = &Path> {
        self.launchers.keys().map(PathBuf::as_path)
    }

    /// The agent in `terminal` runs an older version than its launcher has
    /// installed. `agent` is the one the terminal runs now: a record of
    /// another agent says nothing about it.
    pub(crate) fn restart_pending(
        &self,
        terminal: &TerminalId,
        agent: Option<Agent>,
    ) -> Option<crate::api::schema::AgentRestartPending> {
        let running = self.agents.get(terminal)?;
        if Some(running.agent) != agent {
            return None;
        }
        let version = running.version.as_ref()?;
        let installed = self.launchers.get(&running.launcher)?.installed.as_ref()?;
        (version != installed).then(|| crate::api::schema::AgentRestartPending {
            running_version: version.clone(),
            installed_version: installed.clone(),
        })
    }
}

/// The launcher a pane runs for `agent`: its command name on the server's
/// `PATH`, as a shell would find it.
fn resolve_launcher(agent: Agent) -> Option<PathBuf> {
    let name = crate::detect::agent_label(agent);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| {
            ["", ".exe", ".cmd"]
                .into_iter()
                .map(move |extension| dir.join(format!("{name}{extension}")))
        })
        .find(|candidate| candidate.is_file())
}

/// The version a `--version` run printed: its first non-empty line.
pub(crate) fn parse_version_output(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Runs `launcher --version` and reports what it printed.
fn spawn_version_check(event_tx: tokio::sync::mpsc::Sender<AppEvent>, check: VersionCheck) {
    tokio::spawn(async move {
        let stamp = LauncherStamp::read(&check.launcher);
        let mut command = std::process::Command::new(&check.launcher);
        command
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        crate::platform::configure_background_command(&mut command);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let version = match tokio::time::timeout(VERSION_CHECK_TIMEOUT, command.output()).await {
            Ok(Ok(output)) if output.status.success() => parse_version_output(&output.stdout),
            Ok(Ok(output)) => {
                tracing::debug!(launcher = %check.launcher.display(), status = %output.status, "agent --version failed");
                None
            }
            Ok(Err(error)) => {
                tracing::debug!(launcher = %check.launcher.display(), %error, "agent --version did not start");
                None
            }
            Err(_) => {
                tracing::warn!(launcher = %check.launcher.display(), "agent --version did not exit");
                None
            }
        };
        let _ = event_tx
            .send(AppEvent::AgentVersionChecked {
                check,
                version,
                stamp,
            })
            .await;
    });
}

/// Watches agent launchers, their symlink targets and the directories they
/// live in.
pub(crate) struct LauncherWatch {
    watcher: crate::platform::PathWatcher,
}

/// The paths whose change may mean `launcher` changed.
fn launcher_paths(launcher: &Path) -> Vec<PathBuf> {
    let target = std::fs::canonicalize(launcher).ok();
    let mut paths = Vec::new();
    for path in [Some(launcher), target.as_deref()].into_iter().flatten() {
        for path in [Some(path), path.parent()].into_iter().flatten() {
            if !paths.iter().any(|known| known == path) {
                paths.push(path.to_path_buf());
            }
        }
    }
    paths
}

impl LauncherWatch {
    fn start(event_tx: tokio::sync::mpsc::Sender<AppEvent>) -> Option<Self> {
        let watcher = crate::platform::PathWatcher::start(Box::new(move |path| {
            if let Err(error) = event_tx.blocking_send(AppEvent::AgentLauncherChanged { path }) {
                tracing::debug!(%error, "agent launcher change not delivered");
            }
        }))
        .map_err(|error| tracing::debug!(%error, "agent launcher watch unavailable"))
        .ok()?;
        Some(Self { watcher })
    }

    /// Watches the launcher's paths; one already watched stays as it is, and
    /// one replaced since is watched again.
    fn watch_launcher(&self, launcher: &Path) {
        for path in launcher_paths(launcher) {
            if let Err(error) = self.watcher.watch(&path) {
                tracing::debug!(path = %path.display(), %error, "cannot watch agent launcher");
            }
        }
    }
}

impl App {
    /// Version checks run outside tests: they start the user's agent CLIs.
    fn agent_version_checks_enabled(&self) -> bool {
        self.policy.background_updates
    }

    /// An agent process was detected in `pane_id`: records the version it
    /// runs, or forgets the old record for an agent herdr does not track.
    pub(super) fn agent_process_started(
        &mut self,
        pane_id: crate::layout::PaneId,
        agent: Agent,
        pid: Option<u32>,
    ) {
        if !self.agent_version_checks_enabled() {
            return;
        }
        let Some(terminal_id) = self
            .find_pane(pane_id)
            .map(|(_, pane)| pane.attached_terminal_id.clone())
        else {
            return;
        };
        let terminals = &self.state.terminals;
        self.state
            .agent_versions
            .retain_terminals(|terminal| terminals.contains_key(terminal));
        let launcher = tracks_version(agent)
            .then(|| resolve_launcher(agent))
            .flatten();
        let Some(launcher) = launcher else {
            self.state.agent_versions.forget(&terminal_id);
            return;
        };
        if self.launcher_watch.is_none() {
            self.launcher_watch = LauncherWatch::start(self.event_tx.clone());
        }
        if let Some(watch) = self.launcher_watch.as_ref() {
            watch.watch_launcher(&launcher);
        }
        if let Some(check) =
            self.state
                .agent_versions
                .agent_started(terminal_id, agent, pid, launcher)
        {
            spawn_version_check(self.event_tx.clone(), check);
        }
    }

    /// A watched path changed: checks the launchers it may belong to that
    /// no longer look like they did.
    pub(super) fn agent_launcher_changed(&mut self, path: &Path) {
        let touched = self
            .state
            .agent_versions
            .launchers()
            .filter(|launcher| launcher_paths(launcher).iter().any(|known| known == path))
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        for launcher in touched {
            let stamp = LauncherStamp::read(&launcher);
            if let Some(watch) = self.launcher_watch.as_ref() {
                watch.watch_launcher(&launcher);
            }
            if let Some(check) = self
                .state
                .agent_versions
                .launcher_touched(&launcher, stamp.as_ref())
            {
                spawn_version_check(self.event_tx.clone(), check);
            }
        }
    }

    /// A check ended; returns whether a restart-pending mark may have
    /// changed.
    pub(super) fn agent_version_checked(
        &mut self,
        check: VersionCheck,
        version: Option<String>,
        stamp: Option<LauncherStamp>,
    ) -> bool {
        let (changed, next) = self
            .state
            .agent_versions
            .check_finished(&check, version, stamp);
        if let Some(next) = next {
            spawn_version_check(self.event_tx.clone(), next);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(id: &str) -> TerminalId {
        serde_json::from_value(serde_json::Value::String(id.to_owned())).unwrap()
    }

    fn launcher() -> PathBuf {
        PathBuf::from("/bin/claude")
    }

    fn stamp(len: u64) -> LauncherStamp {
        LauncherStamp {
            target: PathBuf::from("/versions/claude"),
            modified: None,
            len,
        }
    }

    #[test]
    fn an_agent_on_the_installed_version_is_not_pending() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        let (changed, next) = versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        assert!(changed);
        assert_eq!(next, None);
        assert_eq!(
            versions.restart_pending(&terminal("t1"), Some(Agent::Claude)),
            None
        );
    }

    #[test]
    fn an_update_on_disk_marks_the_running_agent_pending() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));

        let check = versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .unwrap();
        versions.check_finished(&check, Some("2.1.1".into()), Some(stamp(2)));

        let pending = versions
            .restart_pending(&terminal("t1"), Some(Agent::Claude))
            .unwrap();
        assert_eq!(pending.running_version, "2.1.0");
        assert_eq!(pending.installed_version, "2.1.1");
        // Another agent in the same terminal is not the one that is stale.
        assert_eq!(
            versions.restart_pending(&terminal("t1"), Some(Agent::Pi)),
            None
        );
    }

    #[test]
    fn an_unchanged_launcher_is_not_checked_again() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        assert_eq!(
            versions.launcher_touched(&launcher(), Some(&stamp(1))),
            None
        );
        assert_eq!(
            versions.launcher_touched(Path::new("/bin/other"), Some(&stamp(1))),
            None
        );
    }

    #[test]
    fn a_new_agent_on_a_newer_version_marks_the_older_ones() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Pi, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("0.70.0".into()), Some(stamp(1)));

        // The update changed nothing the watch sees; the next start shows it.
        let check = versions
            .agent_started(terminal("t2"), Agent::Pi, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("0.71.0".into()), Some(stamp(1)));

        assert!(versions
            .restart_pending(&terminal("t1"), Some(Agent::Pi))
            .is_some());
        assert_eq!(
            versions.restart_pending(&terminal("t2"), Some(Agent::Pi)),
            None
        );
    }

    #[test]
    fn an_agent_started_during_a_check_takes_the_next_one() {
        let mut versions = AgentVersions::default();
        let first = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        // Started while the first check runs, maybe after the update.
        assert_eq!(
            versions.agent_started(terminal("t2"), Agent::Claude, None, launcher()),
            None
        );
        let (_, second) = versions.check_finished(&first, Some("2.1.0".into()), Some(stamp(1)));
        let second = second.expect("the check asked for meanwhile runs");
        assert!(second.id > first.id);
        versions.check_finished(&second, Some("2.1.1".into()), Some(stamp(2)));

        assert!(versions
            .restart_pending(&terminal("t1"), Some(Agent::Claude))
            .is_some());
        assert_eq!(
            versions.restart_pending(&terminal("t2"), Some(Agent::Claude)),
            None
        );
    }

    #[test]
    fn the_same_process_detected_again_keeps_its_version() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, Some(40), launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        let check = versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .unwrap();
        versions.check_finished(&check, Some("2.1.1".into()), Some(stamp(2)));

        // Detection lost and found the old process again: still stale.
        assert_eq!(
            versions.agent_started(terminal("t1"), Agent::Claude, Some(40), launcher()),
            None
        );
        assert!(versions
            .restart_pending(&terminal("t1"), Some(Agent::Claude))
            .is_some());

        // A new process in the pane (the restart) runs what is installed.
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, Some(41), launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.1".into()), Some(stamp(2)));
        assert_eq!(
            versions.restart_pending(&terminal("t1"), Some(Agent::Claude)),
            None
        );
    }

    #[test]
    fn a_failed_check_changes_nothing() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        let check = versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .unwrap();
        let (changed, _) = versions.check_finished(&check, None, Some(stamp(2)));
        assert!(!changed);
        assert_eq!(
            versions.restart_pending(&terminal("t1"), Some(Agent::Claude)),
            None
        );
        // The stamp of a failed check is not kept: the next change checks.
        assert!(versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .is_some());
    }

    #[test]
    fn a_stale_check_result_is_ignored() {
        let mut versions = AgentVersions::default();
        let check = versions
            .agent_started(terminal("t1"), Agent::Claude, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        let (changed, _) = versions.check_finished(&check, Some("9.9.9".into()), Some(stamp(9)));
        assert!(!changed);
    }

    #[test]
    fn forgotten_and_closed_terminals_are_not_pending() {
        let mut versions = AgentVersions::default();
        for id in ["t1", "t2"] {
            let check = versions.agent_started(terminal(id), Agent::Claude, None, launcher());
            if let Some(check) = check {
                versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
            }
        }
        let check = versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .unwrap();
        versions.check_finished(&check, Some("2.1.1".into()), Some(stamp(2)));
        versions.forget(&terminal("t1"));
        versions.retain_terminals(|terminal| terminal.as_str() != "t2");
        assert_eq!(
            versions.restart_pending(&terminal("t1"), Some(Agent::Claude)),
            None
        );
        assert_eq!(
            versions.restart_pending(&terminal("t2"), Some(Agent::Claude)),
            None
        );
    }

    #[test]
    fn agent_info_reports_the_pending_restart_of_the_agent_it_runs() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Claude);

        let versions = &mut app.state.agent_versions;
        let check = versions
            .agent_started(terminal_id.clone(), Agent::Claude, None, launcher())
            .unwrap();
        versions.check_finished(&check, Some("2.1.0".into()), Some(stamp(1)));
        assert_eq!(app.agent_info(0, pane_id).unwrap().restart_pending, None);

        let versions = &mut app.state.agent_versions;
        let check = versions
            .launcher_touched(&launcher(), Some(&stamp(2)))
            .unwrap();
        versions.check_finished(&check, Some("2.1.1".into()), Some(stamp(2)));
        let info = app.agent_info(0, pane_id).unwrap();
        assert_eq!(
            info.restart_pending,
            Some(crate::api::schema::AgentRestartPending {
                running_version: "2.1.0".into(),
                installed_version: "2.1.1".into(),
            })
        );
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["restart_pending"]["installed_version"], "2.1.1");
    }

    /// A launcher script that prints `version`.
    #[cfg(unix)]
    fn fake_launcher(version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "herdr-agent-version-{}-{}",
            std::process::id(),
            version
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let launcher = dir.join("claude");
        std::fs::write(
            &launcher,
            format!("#!/bin/sh\n[ \"$1\" = --version ] && echo '{version} (Claude Code)'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        launcher
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_check_reports_what_the_launcher_prints_with_its_stamp() {
        let launcher = fake_launcher("2.1.7");
        let (event_tx, mut events) = tokio::sync::mpsc::channel(1);
        let check = VersionCheck {
            launcher: launcher.clone(),
            id: 3,
        };
        spawn_version_check(event_tx, check.clone());
        let Some(AppEvent::AgentVersionChecked {
            check: reported,
            version,
            stamp,
        }) = events.recv().await
        else {
            panic!("expected the check's result");
        };
        assert_eq!(reported, check);
        assert_eq!(version.as_deref(), Some("2.1.7 (Claude Code)"));
        assert_eq!(stamp, LauncherStamp::read(&launcher));
        let _ = std::fs::remove_dir_all(launcher.parent().unwrap());
    }

    #[test]
    fn version_output_is_its_first_line() {
        assert_eq!(
            parse_version_output(b"\n2.1.288 (Claude Code)\nmore\n").as_deref(),
            Some("2.1.288 (Claude Code)")
        );
        assert_eq!(parse_version_output(b"  \n"), None);
    }
}
