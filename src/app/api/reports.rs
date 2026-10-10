//! `report.record` and `report.close`. Reports live in the worker store
//! (`crate::workers::reports`); these run on the app because a report names
//! the reporter's pane, its agent session and directory, and because a
//! close is allowed only from the pane bound to the herdr repository's
//! coordination tenure, named by its canonical public id. The headless
//! server answers `report.record` itself ([`App::record_report`] then a
//! notification for its client shells); here a new report is an in-app
//! toast.

use super::responses::{encode_error, encode_success};
use crate::api::schema::{
    NotificationShowParams, NotificationShowSound, ReportCloseParams, ReportRecordParams,
    ResponseResult,
};
use crate::app::App;
use crate::workers::reports::{report_notice, RecordedReport, ReportNotice};
use crate::workers::WorkerError;

fn unavailable() -> WorkerError {
    WorkerError::Io(std::io::Error::other(
        "reports are unavailable on this server",
    ))
}

impl App {
    /// Records the occurrence with the reporter's pane (its canonical id),
    /// its agent session and directory, and returns it with the notice to
    /// send, if it notifies.
    pub(crate) fn record_report(
        &self,
        params: &ReportRecordParams,
    ) -> Result<(RecordedReport, Option<ReportNotice>), WorkerError> {
        let supervisor = crate::workers::coordinators().ok_or_else(unavailable)?;
        let pane = params
            .pane_id
            .as_deref()
            .and_then(|pane| self.parse_pane_id(pane))
            .and_then(|(ws_idx, pane_id)| self.pane_metadata(ws_idx, pane_id));
        let recorded = supervisor.report_record(
            params,
            pane.as_ref()
                .map(|pane| pane.pane_id.as_str())
                .or(params.pane_id.as_deref()),
            pane.as_ref()
                .and_then(|pane| pane.agent_session.as_ref())
                .map(|session| session.value.as_str()),
            pane.as_ref().and_then(|pane| pane.cwd.as_deref()),
        )?;
        let notice = report_notice(&recorded, supervisor.herdr_coordinator().as_ref());
        Ok((recorded, notice))
    }

    pub(super) fn handle_report_record(
        &mut self,
        id: String,
        params: ReportRecordParams,
    ) -> String {
        match self.record_report(&params) {
            Ok((recorded, notice)) => {
                let notified = notice.map(|notice| {
                    let _ = self.handle_notification_show(
                        id.clone(),
                        NotificationShowParams {
                            title: notice.title,
                            body: Some(notice.body),
                            position: None,
                            sound: NotificationShowSound::Request,
                        },
                    );
                    notice.notified
                });
                encode_success(
                    id,
                    ResponseResult::ReportRecorded {
                        report: recorded.report,
                        occurrence: recorded.occurrence,
                        notified,
                    },
                )
            }
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }

    pub(super) fn handle_report_close(&mut self, id: String, params: ReportCloseParams) -> String {
        let Some(supervisor) = crate::workers::coordinators() else {
            let error = unavailable();
            return encode_error(id, error.code(), error.to_string());
        };
        let pane = params
            .pane_id
            .as_deref()
            .map(|pane| self.canonical_pane_id(pane));
        match supervisor.report_close(
            &params.report_id,
            pane.as_deref(),
            params.fix.as_deref(),
            params.not_reproducible,
        ) {
            Ok(report) => encode_success(id, ResponseResult::Report { report }),
            Err(error) => encode_error(id, error.code(), error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::workers::WorkerSupervisor;
    use crate::workspace::Workspace;

    /// An app with one tab whose pane runs in this checkout (herdr's own
    /// repository), and a worker store of its own.
    fn app(name: &str) -> (App, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("herdr-app-reports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::workers::set_test_coordinators(WorkerSupervisor::open(
            root.join("workers"),
            "claude".into(),
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = Workspace::test_new("reports");
        let checkout = std::env::current_dir().unwrap();
        for tab in &workspace.tabs {
            for pane in tab.panes.values() {
                let terminal_id = pane.attached_terminal_id.clone();
                app.state.terminals.insert(
                    terminal_id.clone(),
                    crate::terminal::TerminalState::new(terminal_id, checkout.clone()),
                );
            }
        }
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        (app, root)
    }

    #[test]
    fn a_report_names_the_pane_and_its_repository_and_closes_from_the_coordinators_pane() {
        let (mut app, root) = app("record");
        let pane = app.tab_public_pane_ids(0, 0).remove(0);
        let response = app.handle_report_record(
            "req".into(),
            ReportRecordParams {
                kind: "todo-wait".into(),
                summary: "todo wait r-abcd2345 gave up".into(),
                pane_id: Some(pane.clone()),
                ..ReportRecordParams::default()
            },
        );
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let result = &value["result"];
        assert_eq!(result["type"], "report_recorded", "{response}");
        assert_eq!(result["occurrence"], 1);
        assert_eq!(result["notified"], "user", "no coordinator holds herdr");
        let occurrence = &result["report"]["occurrences"][0];
        assert_eq!(occurrence["pane_id"], pane.as_str());
        let repo = crate::workers::repository_of_dir(
            &std::env::current_dir().unwrap().display().to_string(),
        )
        .unwrap();
        assert_eq!(occurrence["repo"], repo.as_str());

        // Closed from a pane without the tenure: refused; with it: closed.
        let close = |app: &mut App| {
            app.handle_report_close(
                "req".into(),
                ReportCloseParams {
                    report_id: "r-1".into(),
                    fix: Some("t-abcd2345".into()),
                    not_reproducible: false,
                    pane_id: Some(pane.clone()),
                },
            )
        };
        let refused = close(&mut app);
        assert!(refused.contains("report_close_refused"), "{refused}");
        crate::workers::coordinators()
            .unwrap()
            .coordinator_start(&repo, &pane, None)
            .unwrap();
        let closed = close(&mut app);
        assert!(closed.contains("\"state\":\"closed\""), "{closed}");

        // A repeat after the close reopens it and notifies the coordinator.
        let response = app.handle_report_record(
            "req".into(),
            ReportRecordParams {
                kind: "todo-wait".into(),
                summary: "todo wait r-zzzz7777 gave up".into(),
                ..ReportRecordParams::default()
            },
        );
        assert!(
            response.contains("\"notified\":\"coordinator c-"),
            "{response}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
