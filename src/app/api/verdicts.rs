//! Assignments and their verdicts (`crate::assignments`): registered when herdr accepts a
//! prompt, recorded when the pane's agent reports a turn end, read with `agent.verdict`.

use std::path::PathBuf;

use crate::app::App;
use crate::terminal::prompt_turns::PromptTurnState;

use super::responses::{encode_error, encode_success};

impl App {
    /// The transcript of the pane's agent now: the path its integration reported, else the
    /// one its session names.
    fn pane_transcript(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> Option<PathBuf> {
        let pane = self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        crate::assignments::transcript_of(
            terminal.prompt_turns.transcript(),
            crate::app::creation::terminal_agent_session_info(terminal).as_ref(),
        )
    }

    /// Registers a prompt herdr accepted for the pane's agent, anchored at the end of the
    /// agent's transcript now.
    pub(super) fn register_assignment(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        request_id: &str,
        text: &str,
    ) {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return;
        };
        let terminal = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id));
        let agent = terminal.and_then(|terminal| terminal.effective_agent_label());
        let worker = terminal.and_then(|terminal| terminal.agent_name.as_deref());
        let transcript = self.pane_transcript(ws_idx, pane_id);
        crate::assignments::accepted(
            request_id,
            &public,
            agent,
            worker,
            text,
            transcript.as_deref(),
        );
    }

    /// The pane's agent reported a turn end: reads its transcript once for the pane's open
    /// assignments, records each verdict found (once), and marks a prompt the agent took in
    /// mid-turn as worked on by this turn, so its end ends it. A verdict that no
    /// `agent.wait_verdict` waits for asks for a supervision sweep. Returns whether a followed
    /// request changed.
    pub(super) fn record_turn_end_verdicts(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(public) = self.public_pane_id(ws_idx, pane_id) else {
            return false;
        };
        let open = crate::assignments::open_for_pane(&public);
        if open.is_empty() {
            return false;
        }
        let Some(path) = self.pane_transcript(ws_idx, pane_id).or_else(|| {
            open.iter()
                .rev()
                .find_map(|assignment| assignment.transcript.clone().map(PathBuf::from))
        }) else {
            return false;
        };
        let scan = match crate::assignments::scan_open(&path, &open, crate::assignments::now_ms()) {
            Ok(scan) => scan,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "cannot read the agent's transcript");
                return false;
            }
        };
        let mut changed = false;
        if let Some(terminal) = self.pane_terminal_mut(ws_idx, pane_id) {
            for (request_id, absorbed) in &scan.started {
                if *absorbed
                    && terminal.prompt_turns.state_of(request_id) == Some(PromptTurnState::Accepted)
                {
                    changed |= terminal.prompt_turns.absorbed(request_id);
                }
            }
        }
        for (request_id, offset) in &scan.start_offsets {
            if !scan.verdicts.contains_key(request_id) {
                crate::assignments::started_at(request_id, &path, *offset);
            }
        }
        for (request_id, verdict) in scan.verdicts {
            if crate::assignments::record_verdict(&request_id, verdict)
                && !crate::assignments::has_waiter(&request_id)
            {
                crate::workers::supervision::request_sweep(&format!(
                    "verdict of {request_id} in pane {public}"
                ));
            }
        }
        changed
    }

    pub(super) fn handle_agent_verdict(
        &mut self,
        id: String,
        params: crate::api::schema::AgentVerdictParams,
    ) -> String {
        match crate::assignments::get(&params.request_id) {
            Some(assignment) => encode_success(
                id,
                crate::api::schema::ResponseResult::AgentAssignment {
                    assignment: crate::assignments::info(&assignment),
                },
            ),
            None => encode_error(
                id,
                "prompt_request_not_found",
                format!(
                    "herdr keeps no assignment {}; it never typed that prompt, or the record \
                     is gone",
                    params.request_id
                ),
            ),
        }
    }
}
