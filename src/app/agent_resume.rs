use std::time::Instant;

use bytes::Bytes;
use ratatui::layout::Rect;

use super::App;

struct PendingAgentResumeCandidate {
    pane_id: crate::layout::PaneId,
    terminal_id: crate::terminal::TerminalId,
    cwd: std::path::PathBuf,
    plan: crate::agent_resume::AgentResumePlan,
    rows: u16,
    cols: u16,
}

impl App {
    pub(crate) fn has_pending_agent_resumes(&self) -> bool {
        self.state
            .terminals
            .values()
            .any(|terminal| terminal.pending_agent_resume_plan.is_some())
    }

    pub(crate) fn sync_pending_agent_resume_deadline(&mut self, now: Instant) {
        if !self.has_pending_agent_resumes() {
            self.pending_agent_resume_deadline = None;
            self.next_agent_resume_at = None;
            return;
        }
        if self.pending_agent_resume_candidates().is_empty() {
            self.pending_agent_resume_deadline = None;
            return;
        }
        if let Some(next) = self.next_agent_resume_at {
            self.pending_agent_resume_deadline = Some(next);
        } else {
            self.pending_agent_resume_deadline
                .get_or_insert(now + super::PENDING_AGENT_RESUME_THEME_WAIT);
        }
    }

    pub(crate) fn pending_agent_resume_due(&self, now: Instant) -> bool {
        self.pending_agent_resume_deadline
            .is_some_and(|deadline| now >= deadline)
    }

    pub(crate) fn start_pending_agent_resumes(
        &mut self,
        now: Instant,
        allow_empty_theme: bool,
    ) -> bool {
        // Geometry/theme events can also enter here; they must not bypass spacing.
        if self.next_agent_resume_at.is_some_and(|next| now < next) {
            return false;
        }
        let pending = self.pending_agent_resume_candidates();
        let mut changed = false;
        for PendingAgentResumeCandidate {
            pane_id,
            terminal_id,
            cwd,
            plan,
            rows,
            cols,
        } in pending
        {
            if self.terminal_runtimes.get(&terminal_id).is_some() {
                continue;
            }
            changed |= self.start_pending_agent_resume(
                pane_id,
                terminal_id,
                cwd,
                plan,
                rows,
                cols,
                allow_empty_theme,
            );
            if changed && !self.startup_per_agent_delay.is_zero() {
                self.next_agent_resume_at = Some(now + self.startup_per_agent_delay);
                self.pending_agent_resume_deadline = self.next_agent_resume_at;
                break;
            }
        }

        if changed {
            self.schedule_session_save();
        }
        if !self.has_pending_agent_resumes() || self.pending_agent_resume_candidates().is_empty() {
            self.pending_agent_resume_deadline = None;
        }
        if !self.has_pending_agent_resumes() {
            self.next_agent_resume_at = None;
        }
        changed
    }

    fn pending_agent_resume_candidates(&self) -> Vec<PendingAgentResumeCandidate> {
        let terminal_area = self.state.view.terminal_area;
        if terminal_area.width == 0 || terminal_area.height == 0 {
            return Vec::new();
        };

        let mut pending = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in ws.tabs.iter().enumerate() {
                for info in
                    self.pending_agent_resume_pane_infos(ws_idx, tab_idx, tab, terminal_area)
                {
                    let Some(pane) = tab.panes.get(&info.id) else {
                        continue;
                    };
                    if self
                        .terminal_runtimes
                        .get(&pane.attached_terminal_id)
                        .is_some()
                    {
                        continue;
                    }
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    let Some(plan) = terminal.pending_agent_resume_plan.clone() else {
                        continue;
                    };
                    pending.push(PendingAgentResumeCandidate {
                        pane_id: info.id,
                        terminal_id: pane.attached_terminal_id.clone(),
                        cwd: terminal.cwd.clone(),
                        plan,
                        rows: info.inner_rect.height,
                        cols: info.inner_rect.width,
                    });
                }
            }
        }
        pending
    }

    fn pending_agent_resume_pane_infos(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        tab: &crate::workspace::Tab,
        terminal_area: Rect,
    ) -> Vec<crate::layout::PaneInfo> {
        let mut pane_infos = derived_pending_agent_resume_pane_infos(
            tab,
            terminal_area,
            self.state.pane_borders,
            self.state.pane_gaps,
            self.state.pane_outer_borders,
        );

        if self.state.active == Some(ws_idx)
            && self
                .state
                .workspaces
                .get(ws_idx)
                .is_some_and(|ws| tab_idx == ws.active_tab_index())
        {
            for visible_info in &self.state.view.pane_infos {
                if let Some(info) = pane_infos
                    .iter_mut()
                    .find(|info| info.id == visible_info.id)
                {
                    *info = visible_info.clone();
                } else {
                    pane_infos.push(visible_info.clone());
                }
            }
        }

        pane_infos
    }

    pub(crate) fn start_pending_agent_resume_for_terminal(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        rows: u16,
        cols: u16,
        allow_empty_theme: bool,
    ) -> bool {
        if self.terminal_runtimes.get(terminal_id).is_some() {
            return false;
        }
        let Some((pane_id, cwd, plan)) = self.state.workspaces.iter().find_map(|ws| {
            ws.tabs.iter().find_map(|tab| {
                tab.layout.pane_ids().into_iter().find_map(|pane_id| {
                    let pane = tab.panes.get(&pane_id)?;
                    if &pane.attached_terminal_id != terminal_id {
                        return None;
                    }
                    let terminal = self.state.terminals.get(terminal_id)?;
                    Some((
                        pane_id,
                        terminal.cwd.clone(),
                        terminal.pending_agent_resume_plan.clone()?,
                    ))
                })
            })
        }) else {
            return false;
        };

        let changed = self.start_pending_agent_resume(
            pane_id,
            terminal_id.clone(),
            cwd,
            plan,
            rows,
            cols,
            allow_empty_theme,
        );
        if changed {
            self.schedule_session_save();
        }
        if !self.has_pending_agent_resumes() {
            self.pending_agent_resume_deadline = None;
        }
        changed
    }

    fn start_pending_agent_resume(
        &mut self,
        pane_id: crate::layout::PaneId,
        terminal_id: crate::terminal::TerminalId,
        cwd: std::path::PathBuf,
        plan: crate::agent_resume::AgentResumePlan,
        rows: u16,
        cols: u16,
        allow_empty_theme: bool,
    ) -> bool {
        let host_terminal_theme = self.state.host_terminal_theme;
        if host_terminal_theme.is_empty() && !allow_empty_theme {
            return false;
        }

        let Some(resume_command) = shell_command_from_argv(&plan.argv) else {
            tracing::warn!(
                pane = pane_id.raw(),
                terminal = %terminal_id,
                agent = %plan.agent,
                "failed to start deferred agent resume with empty argv"
            );
            return false;
        };
        let Some(launch_env) = self
            .find_pane(pane_id)
            .and_then(|(ws_idx, _)| self.pane_launch_env(ws_idx, pane_id, Vec::new()))
        else {
            return false;
        };

        if !cwd.is_dir() {
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.pending_agent_resume_plan = None;
                terminal.restore_error = Some("Saved directory is unavailable. Restore the directory and restart this session.".into());
                terminal.revision = terminal.revision.saturating_add(1);
            }
            return true;
        }

        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    agent = %plan.agent,
                    err = %err,
                    "failed to start shell for deferred agent resume"
                );
                if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                    terminal.pending_agent_resume_plan = None;
                    terminal.restore_error = Some(format!("Could not start the saved shell: {err}. Fix the shell configuration and restart this session."));
                    terminal.revision = terminal.revision.saturating_add(1);
                }
                return true;
            }
        };

        let mut input = resume_command;
        input.push('\r');
        if let Err(err) = runtime.try_send_bytes(Bytes::from(input)) {
            tracing::warn!(
                pane = pane_id.raw(),
                terminal = %terminal_id,
                agent = %plan.agent,
                err = %err,
                "failed to send deferred agent resume command to shell"
            );
            runtime.shutdown();
            return false;
        }

        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.pending_agent_resume_plan = None;
            terminal.respawn_shell_on_exit = false;
        }
        true
    }
}

/// How often a queued resume checks whether its pane's shell idles again.
/// Polls process state, which sends no event when the shell finishes.
const AUTO_RESUME_SHELL_RECHECK: std::time::Duration = std::time::Duration::from_millis(200);
/// How long a queued resume waits for a busy shell before it gives up with a
/// notice: long enough for a prompt's `git` status, short enough that a
/// command the user started meanwhile is not typed into later.
const AUTO_RESUME_SHELL_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoResumeStep {
    /// Nothing to resume any more.
    Dropped,
    /// The pane's shell is busy; try again later.
    WaitingForShell,
    /// Resumed, or refused with a notice.
    Done,
}

impl App {
    /// When the next agent stopped by the OS resumes, while any waits.
    pub(crate) fn auto_resume_deadline(&self) -> Option<Instant> {
        if self.state.auto_resume_queue.is_empty() {
            return None;
        }
        Some(self.next_auto_resume_at.unwrap_or_else(Instant::now))
    }

    /// Resumes the next agent the OS stopped by typing its resume command into
    /// the pane's shell, one per `startup_per_agent_delay`, so a mass stop does
    /// not start every agent at once. A pane whose shell is still busy (a
    /// prompt that runs `git` right after the agent exits) goes to the back of
    /// the queue until its shell idles. Returns whether anything changed.
    pub(crate) fn run_due_auto_resumes(&mut self, now: Instant) -> bool {
        if self.state.auto_resume_queue.is_empty() {
            self.next_auto_resume_at = None;
            return false;
        }
        if self.next_auto_resume_at.is_some_and(|next| now < next) {
            return false;
        }
        let terminal_id = self.state.auto_resume_queue.remove(0);
        let step = self.auto_resume_terminal(&terminal_id, now);
        if step == AutoResumeStep::WaitingForShell {
            self.state.auto_resume_queue.push(terminal_id);
        } else {
            self.auto_resume_shell_wait_since.remove(&terminal_id);
        }
        if self.state.auto_resume_queue.is_empty() {
            self.next_auto_resume_at = None;
            self.auto_resumed_in_batch = 0;
        } else {
            let pause = if step == AutoResumeStep::WaitingForShell {
                self.startup_per_agent_delay.max(AUTO_RESUME_SHELL_RECHECK)
            } else {
                self.startup_per_agent_delay
            };
            self.next_auto_resume_at = Some(now + pause);
        }
        step != AutoResumeStep::Dropped
    }

    fn auto_resume_terminal(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        now: Instant,
    ) -> AutoResumeStep {
        let Some(session) = self
            .state
            .terminals
            .get(terminal_id)
            .and_then(|terminal| terminal.auto_resume_session())
            .cloned()
        else {
            return AutoResumeStep::Dropped;
        };
        let refusal = self.auto_resume_refusal(terminal_id, &session);
        if refusal.is_none() && !self.pane_shell_idle(terminal_id) {
            let since = *self
                .auto_resume_shell_wait_since
                .entry(terminal_id.clone())
                .or_insert(now);
            if now.saturating_duration_since(since) < AUTO_RESUME_SHELL_WAIT {
                return AutoResumeStep::WaitingForShell;
            }
        }
        if let Some(terminal) = self.state.terminals.get_mut(terminal_id) {
            terminal.take_auto_resume_session();
        }
        if !self.resume_agents_on_restore {
            return AutoResumeStep::Dropped;
        }
        let Some((ws_idx, pane_id)) = self.pane_of_terminal(terminal_id) else {
            return AutoResumeStep::Dropped;
        };
        let Some(command) =
            crate::agent_resume::plan(&session.source, &session.agent, &session.session_ref)
                .and_then(|plan| shell_command_from_argv(&plan.argv))
        else {
            return AutoResumeStep::Dropped;
        };
        let refusal = refusal
            .or_else(|| (!self.pane_shell_idle(terminal_id)).then_some("the shell stayed busy"));
        if let Some(reason) = refusal {
            let title = format!("{} stopped, not resumed: {reason}", session.agent);
            self.show_auto_resume_toast(
                ws_idx,
                pane_id,
                crate::app::state::ToastKind::NeedsAttention,
                title,
            );
            return AutoResumeStep::Done;
        }
        let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
            return AutoResumeStep::Dropped;
        };
        // Ctrl-U first empties the shell's command line, so text typed there
        // since the agent stopped cannot run with the command.
        let mut input = String::from("\u{15}");
        input.push_str(&command);
        input.push('\r');
        if let Err(err) = runtime.try_send_bytes(Bytes::from(input)) {
            tracing::warn!(
                pane = pane_id.raw(),
                terminal = %terminal_id,
                agent = %session.agent,
                err = %err,
                "failed to type the resume command of an agent stopped by the OS"
            );
            let title = format!(
                "{} stopped, not resumed: the pane did not take input",
                session.agent
            );
            self.show_auto_resume_toast(
                ws_idx,
                pane_id,
                crate::app::state::ToastKind::NeedsAttention,
                title,
            );
            return AutoResumeStep::Done;
        }
        tracing::info!(
            pane = pane_id.raw(),
            terminal = %terminal_id,
            agent = %session.agent,
            "resuming an agent stopped by the OS"
        );
        self.state
            .auto_resumed_sessions
            .insert(session.session_ref.clone());
        self.auto_resumed_in_batch += 1;
        let count = self.auto_resumed_in_batch;
        let title = if count == 1 {
            format!("Resumed {} stopped by the system", session.agent)
        } else {
            format!("Resumed {count} agents stopped by the system")
        };
        self.show_auto_resume_toast(
            ws_idx,
            pane_id,
            crate::app::state::ToastKind::Finished,
            title,
        );
        AutoResumeStep::Done
    }

    /// Why the stopped session must not resume in its pane, if it must not.
    /// A busy shell is not a reason: the queue waits for it.
    fn auto_resume_refusal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
        session: &crate::agent_resume::PersistedAgentSession,
    ) -> Option<&'static str> {
        if self
            .state
            .auto_resumed_sessions
            .contains(&session.session_ref)
        {
            return Some("it stopped again after a resume");
        }
        let runs_elsewhere = self.state.terminals.iter().any(|(id, terminal)| {
            id != terminal_id
                && terminal
                    .persisted_agent_session
                    .as_ref()
                    .is_some_and(|other| {
                        other.source == session.source && other.session_ref == session.session_ref
                    })
        });
        runs_elsewhere.then_some("it runs in another pane")
    }

    /// Whether the pane's shell is at its prompt with nothing running, so a
    /// typed command reaches the shell and not a program.
    fn pane_shell_idle(&self, terminal_id: &crate::terminal::TerminalId) -> bool {
        #[cfg(test)]
        if let Some(idle) = self.test_pane_shell_idle {
            return idle && self.terminal_runtimes.get(terminal_id).is_some();
        }
        self.terminal_runtimes
            .get(terminal_id)
            .and_then(|runtime| runtime.child_pid())
            .is_some_and(crate::detect::pane_shell_is_idle)
    }

    fn pane_of_terminal(
        &self,
        terminal_id: &crate::terminal::TerminalId,
    ) -> Option<(usize, crate::layout::PaneId)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| {
                ws.tabs.iter().find_map(|tab| {
                    tab.panes
                        .iter()
                        .find(|(_, pane)| &pane.attached_terminal_id == terminal_id)
                        .map(|(pane_id, _)| (ws_idx, *pane_id))
                })
            })
    }

    /// Shows a resume notice the way the user's toast setting asks: as
    /// Herdr's own toast, and queued for the server to send as a system
    /// notification.
    fn show_auto_resume_toast(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        kind: crate::app::state::ToastKind,
        title: String,
    ) {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let workspace_label = ws.display_name_from_terminals(&self.state.terminals);
        let context =
            crate::app::actions::notification_context(ws, &workspace_label, ws_idx, pane_id);
        let toast = crate::app::state::ToastNotification {
            kind,
            title,
            context,
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id: ws.id.clone(),
                pane_id,
            }),
        };
        self.auto_resume_notices.push(toast.clone());
        if matches!(
            self.state.toast_config.delivery,
            crate::config::ToastDelivery::Herdr
        ) {
            let previous = self.state.toast.replace(toast);
            self.sync_toast_deadline(previous);
        }
    }
}

fn derived_pending_agent_resume_pane_infos(
    tab: &crate::workspace::Tab,
    terminal_area: Rect,
    pane_borders: crate::config::PaneBordersConfig,
    pane_gaps: bool,
    pane_outer_borders: bool,
) -> Vec<crate::layout::PaneInfo> {
    crate::ui::apply_pane_chrome(
        tab.layout.panes(terminal_area),
        pane_borders,
        pane_gaps,
        pane_outer_borders,
    )
    .into_iter()
    .map(|mut info| {
        let pane_inner = crate::ui::pane_inner_rect(info.rect, info.borders);
        info.inner_rect = stable_terminal_inner_rect(pane_inner);
        info
    })
    .collect()
}

fn stable_terminal_inner_rect(pane_inner: Rect) -> Rect {
    if pane_inner.width <= 4 {
        return pane_inner;
    }

    Rect::new(
        pane_inner.x,
        pane_inner.y,
        pane_inner.width.saturating_sub(1),
        pane_inner.height,
    )
}

fn shell_command_from_argv(argv: &[String]) -> Option<String> {
    let mut parts = argv.iter();
    let first = shell_quote(parts.next()?);
    let mut command = first;
    for part in parts {
        command.push(' ');
        command.push_str(&shell_quote(part));
    }
    Some(command)
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b'='
            )
    }) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod auto_resume {
        use super::*;
        use crate::detect::{Agent, AgentState};
        use crate::events::AppEvent;

        struct Pane {
            pane_id: crate::layout::PaneId,
            terminal_id: crate::terminal::TerminalId,
            target: String,
            typed: tokio::sync::mpsc::Receiver<Bytes>,
        }

        fn app_with_claude_panes(sessions: &[&str]) -> (App, Vec<Pane>) {
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                crate::api::EventHub::default(),
            );
            app.state.workspaces = sessions
                .iter()
                .map(|_| crate::workspace::Workspace::test_new("auto-resume"))
                .collect();
            app.state.ensure_test_terminals();
            app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
            app.test_pane_shell_idle = Some(true);
            let mut panes = Vec::new();
            for (ws_idx, session) in sessions.iter().enumerate() {
                let pane_id = app.state.workspaces[ws_idx].tabs[0].root_pane;
                let terminal_id = app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                    .attached_terminal_id
                    .clone();
                app.state
                    .terminals
                    .get_mut(&terminal_id)
                    .unwrap()
                    .set_detected_state(Some(Agent::Claude), AgentState::Idle);
                let (runtime, typed) =
                    crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, 8);
                app.terminal_runtimes.insert(terminal_id.clone(), runtime);
                let target = app.public_pane_id(ws_idx, pane_id).unwrap();
                let pane = Pane {
                    pane_id,
                    terminal_id,
                    target,
                    typed,
                };
                report(
                    &mut app,
                    "pane.report_agent_session",
                    &pane,
                    session,
                    10,
                    true,
                );
                panes.push(pane);
            }
            (app, panes)
        }

        fn report(app: &mut App, method: &str, pane: &Pane, session: &str, seq: u64, start: bool) {
            let mut params = serde_json::json!({
                "pane_id": pane.target,
                "source": "herdr:claude",
                "agent": "claude",
                "seq": seq,
                "agent_session_id": session,
            });
            if start {
                params["session_start_source"] = "startup".into();
            }
            let request = serde_json::from_value::<crate::api::schema::Request>(
                serde_json::json!({"id": method, "method": method, "params": params}),
            )
            .unwrap();
            let response: serde_json::Value =
                serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert!(response.get("error").is_none(), "{response}");
        }

        fn exit_claude(app: &mut App, pane: &Pane) {
            app.handle_internal_event(AppEvent::StateChanged {
                pane_id: pane.pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                process_exited: true,
                observed_at: Instant::now(),
            });
        }

        fn typed(pane: &mut Pane) -> Option<String> {
            pane.typed
                .try_recv()
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        }

        /// Runs every queued resume, ignoring the spacing between them.
        fn run_all(app: &mut App) {
            while app.auto_resume_deadline().is_some() {
                app.next_auto_resume_at = None;
                app.run_due_auto_resumes(Instant::now());
            }
        }

        #[tokio::test]
        async fn agents_the_os_stopped_resume_in_their_panes_with_their_question() {
            let (mut app, mut panes) = app_with_claude_panes(&["first", "second"]);
            app.state
                .terminals
                .get_mut(&panes[0].terminal_id)
                .unwrap()
                .report_awaiting_reply(Some("Install now?".into()), 1_000);

            // One pane reports before its exit is seen, the other after.
            report(
                &mut app,
                "pane.report_agent_stopped",
                &panes[0],
                "first",
                20,
                false,
            );
            exit_claude(&mut app, &panes[0]);
            exit_claude(&mut app, &panes[1]);
            report(
                &mut app,
                "pane.report_agent_stopped",
                &panes[1],
                "second",
                20,
                false,
            );
            run_all(&mut app);

            assert_eq!(
                typed(&mut panes[0]).as_deref(),
                Some("\u{15}claude --resume first\r")
            );
            assert_eq!(
                typed(&mut panes[1]).as_deref(),
                Some("\u{15}claude --resume second\r")
            );
            assert_eq!(
                app.state.toast.as_ref().map(|toast| toast.title.as_str()),
                Some("Resumed 2 agents stopped by the system")
            );

            report(
                &mut app,
                "pane.report_agent_session",
                &panes[0],
                "first",
                30,
                false,
            );
            let terminal = app.state.terminals.get_mut(&panes[0].terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
            assert_eq!(terminal.awaiting_reply_question(), Some("Install now?"));
        }

        #[tokio::test]
        async fn an_exit_without_a_stop_report_or_after_a_forget_does_not_resume() {
            let (mut app, mut panes) = app_with_claude_panes(&["plain", "ended"]);

            exit_claude(&mut app, &panes[0]);
            report(
                &mut app,
                "pane.forget_agent_session",
                &panes[1],
                "ended",
                20,
                false,
            );
            exit_claude(&mut app, &panes[1]);
            run_all(&mut app);

            assert_eq!(typed(&mut panes[0]), None);
            assert_eq!(typed(&mut panes[1]), None);
            assert!(app.state.toast.is_none());
        }

        #[tokio::test]
        async fn a_session_that_stops_again_after_a_resume_is_left_alone() {
            let (mut app, mut panes) = app_with_claude_panes(&["flaky"]);
            report(
                &mut app,
                "pane.report_agent_stopped",
                &panes[0],
                "flaky",
                20,
                false,
            );
            exit_claude(&mut app, &panes[0]);
            run_all(&mut app);
            assert!(typed(&mut panes[0]).is_some());

            report(
                &mut app,
                "pane.report_agent_session",
                &panes[0],
                "flaky",
                30,
                false,
            );
            app.state
                .terminals
                .get_mut(&panes[0].terminal_id)
                .unwrap()
                .set_detected_state(Some(Agent::Claude), AgentState::Idle);
            // A stale forget from an earlier run does not lift the limit.
            report(
                &mut app,
                "pane.forget_agent_session",
                &panes[0],
                "flaky",
                5,
                false,
            );
            report(
                &mut app,
                "pane.report_agent_stopped",
                &panes[0],
                "flaky",
                40,
                false,
            );
            exit_claude(&mut app, &panes[0]);
            run_all(&mut app);

            assert_eq!(typed(&mut panes[0]), None);
            assert_eq!(
                app.state.toast.as_ref().map(|toast| toast.title.as_str()),
                Some("claude stopped, not resumed: it stopped again after a resume")
            );
        }

        #[tokio::test]
        async fn a_session_running_in_another_pane_is_not_resumed() {
            // The other pane already runs the same session again.
            let (mut app, mut panes) = app_with_claude_panes(&["shared", "shared"]);
            report(
                &mut app,
                "pane.report_agent_stopped",
                &panes[0],
                "shared",
                20,
                false,
            );
            exit_claude(&mut app, &panes[0]);
            run_all(&mut app);

            assert_eq!(typed(&mut panes[0]), None);
            assert_eq!(
                app.state.toast.as_ref().map(|toast| toast.title.as_str()),
                Some("claude stopped, not resumed: it runs in another pane")
            );
        }

        #[tokio::test]
        async fn a_resume_waits_for_a_busy_shell_and_gives_up_after_a_while() {
            let (mut app, mut panes) = app_with_claude_panes(&["waits", "gives-up"]);
            app.test_pane_shell_idle = Some(false);
            for (pane, session) in panes.iter().zip(["waits", "gives-up"]) {
                report(
                    &mut app,
                    "pane.report_agent_stopped",
                    pane,
                    session,
                    20,
                    false,
                );
                exit_claude(&mut app, pane);
            }
            let start = Instant::now();
            for _ in 0..4 {
                app.next_auto_resume_at = None;
                app.run_due_auto_resumes(start);
            }
            assert_eq!(typed(&mut panes[0]), None);
            assert!(app.state.toast.is_none());
            assert!(app.auto_resume_deadline().is_some());

            // The first shell idles; the second stays busy past the wait.
            app.test_pane_shell_idle = Some(true);
            app.next_auto_resume_at = None;
            app.run_due_auto_resumes(start);
            assert_eq!(
                typed(&mut panes[0]).as_deref(),
                Some("\u{15}claude --resume waits\r")
            );
            app.test_pane_shell_idle = Some(false);
            app.next_auto_resume_at = None;
            app.run_due_auto_resumes(start + AUTO_RESUME_SHELL_WAIT);

            assert_eq!(typed(&mut panes[1]), None);
            assert_eq!(
                app.state.toast.as_ref().map(|toast| toast.title.as_str()),
                Some("claude stopped, not resumed: the shell stayed busy")
            );
            assert!(app.auto_resume_deadline().is_none());
        }
    }

    #[cfg(unix)]
    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[tokio::test]
    async fn pending_agent_resume_spacing_survives_events_and_failed_restores() {
        for delay_ms in [100, 250, 0] {
            let config: crate::config::Config = toml::from_str(&format!(
                "[session]\nstartup_per_agent_delay_ms = {delay_ms}"
            ))
            .unwrap();
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(
                &config,
                crate::app::AppPolicy::TEST,
                None,
                api_rx,
                crate::api::EventHub::default(),
            );
            app.state.workspaces = (0..4)
                .map(|_| crate::workspace::Workspace::test_new("restore"))
                .collect();
            app.state.active = Some(0);
            app.state.view.terminal_area = Rect::new(0, 0, 100, 30);
            app.state.ensure_test_terminals();
            let missing = std::env::current_dir()
                .unwrap()
                .join("__missing_resume_cwd__");
            assert!(!missing.exists());
            for terminal in app.state.terminals.values_mut() {
                terminal.cwd = missing.clone();
                terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
                    agent: "codex".into(),
                    argv: vec!["codex".into()],
                    dedupe_key: terminal.id.to_string(),
                });
            }
            let now = Instant::now();
            app.sync_pending_agent_resume_deadline(now);
            assert!(!app.start_pending_agent_resumes(now, false));
            assert!(app.start_pending_agent_resumes(now, true));
            if delay_ms != 0 {
                let next = now + std::time::Duration::from_millis(delay_ms);
                assert_eq!(
                    app.state
                        .terminals
                        .values()
                        .filter(|t| t.restore_error.is_some())
                        .count(),
                    1
                );
                // Geometry changes clear the wakeup, but must preserve the launch gap.
                app.pending_agent_resume_deadline = None;
                app.sync_pending_agent_resume_deadline(now);
                assert_eq!(app.pending_agent_resume_deadline, Some(next));
                assert!(!app
                    .start_pending_agent_resumes(next - std::time::Duration::from_millis(1), true));
                // A late wakeup must not release every overdue agent in a burst.
                for processed in 2..=4 {
                    let late = now + std::time::Duration::from_secs(processed * 10);
                    assert!(app.start_pending_agent_resumes(late, true));
                    assert_eq!(
                        app.state
                            .terminals
                            .values()
                            .filter(|t| t.restore_error.is_some())
                            .count(),
                        processed as usize
                    );
                }
            }
            assert!(!app.has_pending_agent_resumes());
            assert!(app.pending_agent_resume_deadline.is_none());
            assert!(app.next_agent_resume_at.is_none());
            assert_eq!(
                app.state
                    .terminals
                    .values()
                    .filter(|t| t.restore_error.is_some())
                    .count(),
                4
            );
        }
    }

    #[cfg(unix)]
    fn long_running_test_argv() -> Vec<String> {
        vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()]
    }

    #[cfg(unix)]
    fn marker_resume_test_argv() -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s' 'restored agent: shell quoted | marker'; sleep 5".into(),
        ]
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_deferred_restore_keeps_session_reference_without_retrying_elsewhere() {
        for missing_shell in [false, true] {
            let mut app = test_app();
            let workspace = crate::workspace::Workspace::test_new("unavailable");
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.terminal_id(pane_id).unwrap().clone();
            app.state.workspaces = vec![workspace];
            app.state.active = Some(0);
            app.state.ensure_test_terminals();
            if missing_shell {
                app.state.default_shell = "__herdr_missing_resume_shell__".into();
            }
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            if !missing_shell {
                terminal.cwd = std::env::current_dir()
                    .unwrap()
                    .join("__herdr_missing_resume_cwd__");
                assert!(!terminal.cwd.exists());
            }
            let session = crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("resume-test").unwrap(),
            };
            terminal.persisted_agent_session = Some(session.clone());
            terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
                agent: "codex".into(),
                argv: long_running_test_argv(),
                dedupe_key: "resume-test".into(),
            });
            app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true);
            assert!(app.terminal_runtimes.get(&terminal_id).is_none());
            let terminal = &app.state.terminals[&terminal_id];
            assert!(terminal.pending_agent_resume_plan.is_none());
            assert_eq!(terminal.persisted_agent_session.as_ref(), Some(&session));
            assert!(terminal.restore_error.is_some());
            assert!(!app.has_pending_agent_resumes());
            assert!(!app.start_pending_agent_resume_for_terminal(&terminal_id, 24, 80, true));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_waits_for_host_theme_before_launch() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        let pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.view.pane_infos = pane_infos;
        let terminal = app
            .state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist");
        terminal.pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: marker_resume_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(!app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.terminal_runtimes.get(&terminal_id).is_none());

        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };

        assert!(app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());
        let terminal = app
            .state
            .terminals
            .get(&terminal_id)
            .expect("terminal should survive launch");
        assert!(terminal.pending_agent_resume_plan.is_none());
        assert!(!terminal.respawn_shell_on_exit);

        let runtime = app
            .terminal_runtimes
            .get(&terminal_id)
            .expect("pending resume should leave a shell runtime");
        let marker = "restored agent: shell quoted | marker";
        for _ in 0..20 {
            if runtime
                .snapshot_history()
                .is_some_and(|text| text.contains(marker))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            runtime
                .snapshot_history()
                .expect("runtime should expose terminal history")
                .contains(marker),
            "deferred restore should inject the resume argv into the restored shell"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_can_launch_after_theme_wait_expires() {
        let mut app = test_app();
        let workspace = crate::workspace::Workspace::test_new("restored");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        app.sync_pending_agent_resume_deadline(std::time::Instant::now());
        assert!(!app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.start_pending_agent_resumes(Instant::now(), true));
        assert!(app.terminal_runtimes.get(&terminal_id).is_some());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_hidden_panes_with_current_terminal_area() {
        let mut app = test_app();
        let active_workspace = crate::workspace::Workspace::test_new("active");
        let active_pane = active_workspace.tabs[0].root_pane;
        let active_terminal = active_workspace.terminal_id(active_pane).cloned().unwrap();
        let hidden_workspace = crate::workspace::Workspace::test_new("hidden");
        let hidden_pane = hidden_workspace.tabs[0].root_pane;
        let hidden_terminal = hidden_workspace.terminal_id(hidden_pane).cloned().unwrap();
        app.state.view.pane_infos = active_workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![active_workspace, hidden_workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        for terminal_id in [&active_terminal, &hidden_terminal] {
            app.state
                .terminals
                .get_mut(terminal_id)
                .expect("test terminal should exist")
                .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
                agent: "codex".into(),
                argv: long_running_test_argv(),
                dedupe_key: format!("herdr:codex\0codex\0Id\0{terminal_id}"),
            });
        }
        app.pending_agent_resume_deadline =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(1));

        let now = Instant::now();
        assert!(app.start_pending_agent_resumes(now, false));
        assert!(app.terminal_runtimes.get(&active_terminal).is_some());
        assert!(app.terminal_runtimes.get(&hidden_terminal).is_none());
        assert!(!app.start_pending_agent_resumes(now, true));
        assert!(
            app.start_pending_agent_resumes(now + std::time::Duration::from_millis(100), false,)
        );
        assert!(app.terminal_runtimes.get(&hidden_terminal).is_some());
        assert!(
            app.pending_agent_resume_deadline.is_none(),
            "launched pending resumes should clear the wakeup deadline"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_inactive_tab_panes_with_current_terminal_area() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("tabs");
        let active_pane = workspace.tabs[0].root_pane;
        let inactive_tab = workspace.test_add_tab(Some("agents"));
        let inactive_pane = workspace.tabs[inactive_tab].root_pane;
        let inactive_terminal = workspace.tabs[inactive_tab]
            .terminal_id(inactive_pane)
            .cloned()
            .unwrap();
        app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        assert!(app
            .state
            .workspaces
            .first()
            .and_then(|ws| ws.tabs[0].terminal_id(active_pane))
            .is_some());
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&inactive_terminal)
            .expect("inactive tab terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0inactive-tab-session".into(),
        });

        assert!(app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.terminal_runtimes.get(&inactive_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&inactive_terminal)
                .expect("inactive tab terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "inactive tab restored panes should not wait for tab focus"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_launches_zoom_hidden_active_tab_panes() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("zoomed");
        let hidden_pane = workspace.tabs[0].root_pane;
        let visible_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].zoomed = true;
        let hidden_terminal = workspace.terminal_id(hidden_pane).cloned().unwrap();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: visible_pane,
            rect: ratatui::layout::Rect::new(0, 0, 100, 30),
            inner_rect: ratatui::layout::Rect::new(1, 1, 98, 28),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        }];
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&hidden_terminal)
            .expect("hidden zoom pane terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0zoom-hidden-session".into(),
        });

        assert!(app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.terminal_runtimes.get(&hidden_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&hidden_terminal)
                .expect("hidden zoom pane terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "zoom-hidden restored panes should not wait for pane focus"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn pending_agent_resume_uses_current_terminal_area_for_background_panes() {
        let mut app = test_app();
        let previous_workspace = crate::workspace::Workspace::test_new("previous");
        let previous_pane = previous_workspace.tabs[0].root_pane;
        let previous_terminal = previous_workspace
            .terminal_id(previous_pane)
            .cloned()
            .unwrap();
        let current_workspace = crate::workspace::Workspace::test_new("current");
        app.state.view.pane_infos = previous_workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 80, 24);
        app.state.workspaces = vec![previous_workspace, current_workspace];
        app.state.active = Some(1);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&previous_terminal)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        app.sync_pending_agent_resume_deadline(std::time::Instant::now());
        assert!(app.pending_agent_resume_deadline.is_some());
        assert!(app.start_pending_agent_resumes(Instant::now(), false));
        assert!(app.terminal_runtimes.get(&previous_terminal).is_some());
        assert!(
            app.state
                .terminals
                .get(&previous_terminal)
                .expect("previous terminal should still exist")
                .pending_agent_resume_plan
                .is_none(),
            "background restored panes should not wait for focus once terminal area is known"
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_agent_resume_launches_with_inner_rect_size() {
        let mut app = test_app();
        let mut workspace = crate::workspace::Workspace::test_new("split");
        let pane_id = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = vec![crate::layout::PaneInfo {
            id: pane_id,
            rect: ratatui::layout::Rect::new(0, 0, 100, 30),
            inner_rect: ratatui::layout::Rect::new(1, 1, 98, 28),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        }];
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.host_terminal_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 220,
                g: 220,
                b: 220,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 20,
                g: 20,
                b: 20,
            }),
            ..Default::default()
        };
        app.state
            .terminals
            .get_mut(&terminal_id)
            .expect("test terminal should exist")
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: long_running_test_argv(),
            dedupe_key: "herdr:codex\0codex\0Id\0codex-session".into(),
        });

        assert!(app.start_pending_agent_resumes(Instant::now(), false));
        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .expect("pending resume should launch")
                .current_size(),
            (28, 98)
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn shell_command_from_argv_quotes_resume_arguments() {
        let argv = vec![
            "claude".to_string(),
            "--resume".to_string(),
            "session with ' quote".to_string(),
        ];

        assert_eq!(
            shell_command_from_argv(&argv).as_deref(),
            Some("claude --resume 'session with '\\'' quote'")
        );
        assert_eq!(shell_command_from_argv(&[]), None);
    }
}
