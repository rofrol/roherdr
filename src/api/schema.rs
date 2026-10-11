use serde::{Deserialize, Serialize};

pub mod agents;
pub mod commands;
pub mod common;
pub mod coordinators;
pub mod decisions;
pub mod events;
pub mod history;
pub mod integrations;
pub mod panes;
pub mod plugins;
pub mod reports;
pub mod response;
pub mod server;
pub mod session;
pub mod tabs;
pub mod todo;
pub mod usage;
pub mod workers;
pub mod workspaces;
pub mod worktrees;

pub use agents::*;
pub use commands::*;
pub use common::*;
pub use coordinators::*;
pub use decisions::*;
pub use events::*;
pub use history::*;
pub use integrations::*;
pub use panes::*;
pub use plugins::*;
pub use reports::*;
pub use response::*;
pub use server::*;
pub use session::*;
pub use tabs::*;
pub use todo::*;
pub use usage::*;
pub use workers::*;
pub use workspaces::*;
pub use worktrees::*;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Request {
    pub id: String,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "method", content = "params")]
// Request enums are short-lived wire values; keeping variants direct preserves
// the simple serde shape and avoids boxing churn across every caller.
#[allow(clippy::large_enum_variant)]
pub enum Method {
    #[serde(rename = "ping")]
    Ping(PingParams),
    #[serde(rename = "server.stop")]
    ServerStop(EmptyParams),
    #[serde(rename = "server.live_handoff")]
    ServerLiveHandoff(ServerLiveHandoffParams),
    #[serde(rename = "server.reload_config")]
    ServerReloadConfig(EmptyParams),
    #[serde(rename = "server.ssh_agent.register")]
    ServerSshAgentRegister(ServerSshAgentRegisterParams),
    #[serde(rename = "server.agent_manifests")]
    ServerAgentManifests(EmptyParams),
    #[serde(rename = "server.reload_agent_manifests")]
    ServerReloadAgentManifests(EmptyParams),
    #[serde(rename = "server.pty_usage")]
    ServerPtyUsage(EmptyParams),
    #[serde(rename = "worker.start")]
    WorkerStart(WorkerStartParams),
    #[serde(rename = "worker.status")]
    WorkerStatus(WorkerTarget),
    #[serde(rename = "worker.list")]
    WorkerList(EmptyParams),
    #[serde(rename = "worker.wait")]
    WorkerWait(WorkerWaitParams),
    /// One coordinator's inbox over all of its workers: their events after a
    /// cursor, in order, as a bounded batch; blocks while empty with `wait`.
    #[serde(rename = "worker.events")]
    WorkerEvents(WorkerEventsParams),
    #[serde(rename = "worker.prompt")]
    WorkerPrompt(WorkerPromptParams),
    #[serde(rename = "worker.interrupt")]
    WorkerInterrupt(WorkerInterruptParams),
    #[serde(rename = "worker.stop")]
    WorkerStop(WorkerCommandTarget),
    #[serde(rename = "worker.kill")]
    WorkerKill(WorkerKillParams),
    #[serde(rename = "worker.answer")]
    WorkerAnswer(WorkerAnswerParams),
    /// `worker.answer` from a coordination tenure, refused with
    /// `ownership_transferred` once the worker is another tenure's.
    #[serde(rename = "worker.answer_as")]
    WorkerAnswerAs(WorkerAnswerAsParams),
    /// The owner acknowledges the worker's events up to a `seq`.
    #[serde(rename = "worker.ack")]
    WorkerAck(WorkerAckParams),
    /// The owned workers with an event their owner has not acknowledged.
    #[serde(rename = "worker.obligations")]
    WorkerObligations(WorkerObligationsParams),
    /// Stops admitting new worker turns before an install, reports the
    /// workers still in a turn, or admits them again.
    #[serde(rename = "worker.drain")]
    WorkerDrain(WorkerDrainParams),
    /// Blocks until the workers in a turn differ from the caller's list, or
    /// none is in a turn.
    #[serde(rename = "worker.wait_drained")]
    WorkerWaitDrained(WorkerWaitDrainedParams),
    /// The workers' runs, grouped by the TODO item given at start.
    #[serde(rename = "worker.runs")]
    WorkerRuns(WorkerRunsParams),
    /// Hands a question its owner will not answer to the user: it becomes a
    /// loud `?` question.
    #[serde(rename = "worker.escalate")]
    WorkerEscalate(WorkerEscalateParams),
    /// Herdr checks the worker's commit (its message, paths, worktree and
    /// processes, generated files, a command) and decides its verdict.
    #[serde(rename = "worker.verify")]
    WorkerVerify(WorkerVerifyParams),
    /// Opens the worker's journal, read-only, in a popup that follows it.
    #[serde(rename = "worker.open_log")]
    WorkerOpenLog(WorkerTarget),
    /// Ends the worker (interrupt, its `result`, stop, its exit), then opens
    /// a tab running `claude --resume <session>` in its directory.
    #[serde(rename = "worker.take_over")]
    WorkerTakeOver(WorkerTarget),
    /// `worker.take_over` that also retries a takeover an earlier server
    /// left unfinished when no tab or process resuming the session was
    /// found: a tab it opened unseen would be a second writer of the
    /// session. A method of its own, since `worker.take_over`'s shape is
    /// frozen for clients.
    #[serde(rename = "worker.force_take_over")]
    WorkerForceTakeOver(WorkerTarget),
    /// The worker's transcript as structured events, from a journal line
    /// on: what its read-only tab shows.
    #[serde(rename = "worker.transcript")]
    WorkerTranscript(WorkerTranscriptParams),
    /// Blocks until the worker's transcript has lines after `after`, or the
    /// worker is gone, then answers as `worker.transcript`: a live
    /// subscription by repeated calls.
    #[serde(rename = "worker.transcript_wait")]
    WorkerTranscriptWait(WorkerTranscriptParams),
    /// One pending question with the tool's whole input, for a client's
    /// answer dialog, which asks when the user opens it.
    #[serde(rename = "worker.question")]
    WorkerQuestion(WorkerQuestionTarget),
    /// Denies a pending question, then stops the worker.
    #[serde(rename = "worker.deny_and_stop")]
    WorkerDenyAndStop(WorkerDenyAndStopParams),
    /// Drives a TODO item from preflight to a cherry-pick onto `master`.
    #[serde(rename = "todo.run")]
    TodoRun(TodoRunParams),
    /// `todo.run` whose task, subject, paths and checks the server drafts
    /// itself with a typed model call.
    #[serde(rename = "todo.draft_run")]
    TodoDraftRun(TodoDraftRunParams),
    /// Answers a run's pending event: approve, retry or answer.
    #[serde(rename = "todo.resume")]
    TodoResume(TodoResumeParams),
    /// Blocks until the run has an event after `after` that needs the
    /// coordinator, or it ended.
    #[serde(rename = "todo.wait")]
    TodoWait(TodoWaitParams),
    #[serde(rename = "todo.status")]
    TodoStatus(TodoRunTarget),
    #[serde(rename = "todo.runs")]
    TodoRuns(TodoRunsParams),
    /// One attempt of a run as the coordinator reviews it: its task, the
    /// worker's last reply, questions and failed tool calls, the diff, the
    /// verify and the approval.
    #[serde(rename = "todo.review")]
    TodoReview(TodoReviewParams),
    /// Starts a fresh headless item coordinator for the top item of
    /// "Next, in order"; with `chain`, the next starts on its exit.
    #[serde(rename = "todo.next")]
    TodoNext(TodoNextParams),
    /// Stops the repository's chain of item coordinators.
    #[serde(rename = "todo.stop")]
    TodoStop(TodoStopParams),
    /// Turns a repository's queue mode on or pauses it: on, the server
    /// starts the top runnable item of "Next, in order" itself.
    #[serde(rename = "todo.queue_set")]
    TodoQueueSet(TodoQueueSetParams),
    /// A repository's queue mode and what its queue does now.
    #[serde(rename = "todo.queue_status")]
    TodoQueueStatus(TodoQueueTarget),
    /// Whether a repository's TODO waits on its coordinator: no run of it
    /// is active, its queue mode is not on and not paused, and "Next, in
    /// order" on `master` has items the driver can run.
    #[serde(rename = "todo.runnable_state")]
    TodoRunnableState(TodoQueueTarget),
    /// The user's grant of a repository's operation (`prepare`) for the
    /// exact definition and hash `master` declares.
    #[serde(rename = "todo.grant")]
    TodoGrant(TodoGrantParams),
    /// The stored capability grants.
    #[serde(rename = "todo.grants")]
    TodoGrants(TodoGrantsParams),
    /// Removes stored grants of a repository's operation, recorded.
    #[serde(rename = "todo.revoke")]
    TodoRevoke(TodoRevokeParams),
    /// Records a question for the user, or a decision of theirs, in the
    /// decision ledger.
    #[serde(rename = "decision.add")]
    DecisionAdd(DecisionAddParams),
    /// Records the user's answer to an open question of the ledger.
    #[serde(rename = "decision.decide")]
    DecisionDecide(DecisionDecideParams),
    /// The ledger's records, oldest first.
    #[serde(rename = "decision.list")]
    DecisionList(DecisionListParams),
    /// One record of the ledger.
    #[serde(rename = "decision.get")]
    DecisionGet(DecisionGetParams),
    /// The TODO items herdr recorded, each with its latest record.
    #[serde(rename = "history.list")]
    HistoryList(HistoryListParams),
    /// One TODO item's timeline: its records and the runs they name.
    #[serde(rename = "history.item")]
    HistoryItem(HistoryItemParams),
    /// Compares a repository's `TODO.md` with the records: claims without
    /// an end, items deleted without a `closed` record. Reports only.
    #[serde(rename = "history.reconcile")]
    HistoryReconcile(HistoryReconcileParams),
    /// Claims a repository's coordination for a pane: a tenure, refused
    /// while another one of the repository is active.
    #[serde(rename = "coordinator.start")]
    CoordinatorStart(CoordinatorStartParams),
    /// Ends a coordination tenure with a reason.
    #[serde(rename = "coordinator.end")]
    CoordinatorEnd(CoordinatorEndParams),
    /// The active coordination tenures.
    #[serde(rename = "coordinator.status")]
    CoordinatorStatus(CoordinatorStatusParams),
    /// Hands a repository's coordination to another pane: the next tenure,
    /// which takes over the workers and runs.
    #[serde(rename = "coordinator.handoff")]
    CoordinatorHandoff(CoordinatorHandoffParams),
    /// Records one exception to a coordinator tab's command allowlist and
    /// notifies the user.
    #[serde(rename = "coordinator.record_override")]
    CoordinatorRecordOverride(CoordinatorRecordOverrideParams),
    /// The recorded allowlist exceptions, oldest first.
    #[serde(rename = "history.overrides")]
    HistoryOverrides(HistoryOverridesParams),
    /// What to do with one call the coordinator allowlist refused: the
    /// configured mode, and in shadow mode the stored would-deny.
    #[serde(rename = "coordinator.allowlist_refusal")]
    CoordinatorAllowlistRefusal(CoordinatorAllowlistRefusalParams),
    /// The calls the coordinator allowlist would have denied in shadow mode,
    /// oldest first, and their count per command shape.
    #[serde(rename = "history.would_deny")]
    HistoryWouldDeny(HistoryWouldDenyParams),
    /// Records one occurrence of a protocol problem; the first of its
    /// fingerprint notifies the herdr coordinator (or the user).
    #[serde(rename = "report.record")]
    ReportRecord(ReportRecordParams),
    /// The reports, open ones or all.
    #[serde(rename = "report.list")]
    ReportList(ReportListParams),
    /// Closes a report with its fix or as not reproducible; only from the
    /// herdr repository's coordinator pane.
    #[serde(rename = "report.close")]
    ReportClose(ReportCloseParams),
    #[serde(rename = "notification.show")]
    NotificationShow(NotificationShowParams),
    #[serde(rename = "notification.show_for_pane")]
    NotificationShowForPane(NotificationShowForPaneParams),
    #[serde(rename = "product_announcement.dismiss")]
    ProductAnnouncementDismiss(ProductAnnouncementDismissParams),
    #[serde(rename = "release_notes.dismiss")]
    ReleaseNotesDismiss(ReleaseNotesDismissParams),
    #[serde(rename = "command.invoke")]
    CommandInvoke(CommandInvokeParams),
    #[serde(rename = "client.window_title.set")]
    ClientWindowTitleSet(ClientWindowTitleSetParams),
    #[serde(rename = "client.window_title.clear")]
    ClientWindowTitleClear(EmptyParams),
    #[serde(rename = "client_shell.surface.set")]
    ClientShellSurfaceSet(ClientShellSurfaceSetParams),
    /// The worker whose transcript this client shell's worker tab shows;
    /// the server pushes it as `endpoint.worker-transcript.v1`.
    #[serde(rename = "client_shell.worker_transcript.set")]
    ClientShellWorkerTranscriptSet(ClientShellWorkerTranscriptSetParams),
    #[serde(rename = "session.snapshot")]
    SessionSnapshot(EmptyParams),
    #[serde(rename = "workspace.create")]
    WorkspaceCreate(WorkspaceCreateParams),
    #[serde(rename = "workspace.create_after")]
    WorkspaceCreateAfter(WorkspaceCreateAfterParams),
    #[serde(rename = "workspace.list")]
    WorkspaceList(EmptyParams),
    #[serde(rename = "workspace.get")]
    WorkspaceGet(WorkspaceTarget),
    #[serde(rename = "workspace.focus")]
    WorkspaceFocus(WorkspaceTarget),
    #[serde(rename = "workspace.rename")]
    WorkspaceRename(WorkspaceRenameParams),
    #[serde(rename = "workspace.move")]
    WorkspaceMove(WorkspaceMoveParams),
    #[serde(rename = "workspace.move_block")]
    WorkspaceMoveBlock(WorkspaceMoveBlockParams),
    #[serde(rename = "workspace.report_metadata")]
    WorkspaceReportMetadata(WorkspaceReportMetadataParams),
    #[serde(rename = "workspace.close")]
    WorkspaceClose(WorkspaceCloseParams),
    #[serde(rename = "workspace.bookmark")]
    WorkspaceBookmark(WorkspaceBookmarkParams),
    #[serde(rename = "worktree.list")]
    WorktreeList(WorktreeListParams),
    #[serde(rename = "worktree.create")]
    WorktreeCreate(WorktreeCreateParams),
    #[serde(rename = "worktree.create_from_pane")]
    WorktreeCreateFromPane(WorktreeCreateFromPaneParams),
    #[serde(rename = "worktree.open")]
    WorktreeOpen(WorktreeOpenParams),
    #[serde(rename = "worktree.remove")]
    WorktreeRemove(WorktreeRemoveParams),
    #[serde(rename = "git.branch_list")]
    GitBranchList(GitBranchListParams),
    #[serde(rename = "tab.create")]
    TabCreate(TabCreateParams),
    #[serde(rename = "tab.create_child")]
    TabCreateChild(TabCreateChildParams),
    #[serde(rename = "tab.create_after")]
    TabCreateAfter(TabCreateAfterParams),
    #[serde(rename = "tab.reopen_closed")]
    TabReopenClosed(TabReopenClosedParams),
    #[serde(rename = "tab.create_agent")]
    TabCreateAgent(TabCreateAgentParams),
    #[serde(rename = "tab.list")]
    TabList(TabListParams),
    #[serde(rename = "tab.get")]
    TabGet(TabTarget),
    #[serde(rename = "tab.focus")]
    TabFocus(TabTarget),
    #[serde(rename = "tab.rename")]
    TabRename(TabRenameParams),
    #[serde(rename = "tab.move")]
    TabMove(TabMoveParams),
    #[serde(rename = "tab.close")]
    TabClose(TabTarget),
    #[serde(rename = "tab.set_parent")]
    TabSetParent(TabSetParentParams),
    #[serde(rename = "tab.set_status")]
    TabSetStatus(TabSetStatusParams),
    #[serde(rename = "tab.bookmark")]
    TabBookmark(TabBookmarkParams),
    #[serde(rename = "tab.set_role")]
    TabSetRole(TabSetRoleParams),
    #[serde(rename = "tab.set_job_metadata")]
    TabSetJobMetadata(TabSetJobMetadataParams),
    #[serde(rename = "agent.list")]
    AgentList(EmptyParams),
    #[serde(rename = "agent.get")]
    AgentGet(AgentTarget),
    #[serde(rename = "agent.read")]
    AgentRead(AgentReadParams),
    #[serde(rename = "agent.explain")]
    AgentExplain(AgentTarget),
    #[serde(rename = "agent.send_keys")]
    AgentSendKeys(AgentSendKeysParams),
    #[serde(rename = "agent.rename")]
    AgentRename(AgentRenameParams),
    #[serde(rename = "agent.view.set")]
    AgentViewSet(AgentViewSetParams),
    #[serde(rename = "agent.view.clear")]
    AgentViewClear(AgentViewClearParams),
    #[serde(rename = "agent.focus")]
    AgentFocus(AgentTarget),
    #[serde(rename = "agent.kind_list")]
    AgentKindList(EmptyParams),
    #[serde(rename = "agent.start")]
    AgentStart(AgentStartParams),
    #[serde(rename = "agent.handoff")]
    AgentHandoff(AgentHandoffParams),
    #[serde(rename = "agent.prompt")]
    AgentPrompt(AgentPromptParams),
    #[serde(rename = "agent.prompt_turn")]
    AgentPromptTurn(AgentPromptTurnParams),
    #[serde(rename = "agent.prompt_status")]
    AgentPromptStatus(AgentPromptStatusParams),
    #[serde(rename = "agent.prompt_tracked")]
    AgentPromptTracked(AgentPromptTrackedParams),
    #[serde(rename = "agent.prompt_confirmed")]
    AgentPromptConfirmed(AgentPromptConfirmedParams),
    #[serde(rename = "agent.wait_turn")]
    AgentWaitTurn(AgentWaitTurnParams),
    #[serde(rename = "agent.wait")]
    AgentWait(AgentWaitParams),
    #[serde(rename = "agent.wait_change")]
    AgentWaitChange(AgentWaitChangeParams),
    #[serde(rename = "pane.split")]
    PaneSplit(PaneSplitParams),
    #[serde(rename = "pane.swap")]
    PaneSwap(PaneSwapParams),
    #[serde(rename = "pane.move")]
    PaneMove(PaneMoveParams),
    #[serde(rename = "pane.zoom")]
    PaneZoom(PaneZoomParams),
    #[serde(rename = "pane.layout")]
    PaneLayout(PaneLayoutParams),
    #[serde(rename = "pane.process_info")]
    PaneProcessInfo(PaneProcessInfoParams),
    #[serde(rename = "layout.export")]
    LayoutExport(LayoutExportParams),
    #[serde(rename = "layout.apply")]
    LayoutApply(LayoutApplyParams),
    #[serde(rename = "layout.set_split_ratio")]
    LayoutSetSplitRatio(LayoutSetSplitRatioParams),
    #[serde(rename = "pane.neighbor")]
    PaneNeighbor(PaneNeighborParams),
    #[serde(rename = "pane.edges")]
    PaneEdges(PaneEdgesParams),
    #[serde(rename = "pane.focus_direction")]
    PaneFocusDirection(PaneFocusDirectionParams),
    #[serde(rename = "pane.resize")]
    PaneResize(PaneResizeParams),
    #[serde(rename = "pane.scroll")]
    PaneScroll(PaneScrollParams),
    #[serde(rename = "pane.clear")]
    PaneClear(PaneTarget),
    #[serde(rename = "pane.edit_scrollback")]
    PaneEditScrollback(PaneTarget),
    #[serde(rename = "pane.selection.read")]
    PaneSelectionRead(PaneSelectionReadParams),
    #[serde(rename = "pane.copy_motion")]
    PaneCopyMotion(PaneCopyMotionParams),
    #[serde(rename = "pane.copy_search")]
    PaneCopySearch(PaneCopySearchParams),
    #[serde(rename = "pane.list")]
    PaneList(PaneListParams),
    #[serde(rename = "pane.current")]
    PaneCurrent(PaneCurrentParams),
    #[serde(rename = "pane.get")]
    PaneGet(PaneTarget),
    #[serde(rename = "pane.focus")]
    PaneFocus(PaneTarget),
    #[serde(rename = "pane.input.set")]
    PaneInputSet(PaneInputSetParams),
    #[serde(rename = "pane.link.activate")]
    PaneLinkActivate(PaneLinkActivateParams),
    #[serde(rename = "pane.link.resolve")]
    PaneLinkResolve(PaneLinkActivateParams),
    #[serde(rename = "pane.rename")]
    PaneRename(PaneRenameParams),
    #[serde(rename = "pane.send_text")]
    PaneSendText(PaneSendTextParams),
    #[serde(rename = "pane.send_keys")]
    PaneSendKeys(PaneSendKeysParams),
    #[serde(rename = "pane.send_input")]
    PaneSendInput(PaneSendInputParams),
    #[serde(rename = "pane.read")]
    PaneRead(PaneReadParams),
    #[serde(rename = "pane.report_agent")]
    PaneReportAgent(PaneReportAgentParams),
    #[serde(rename = "pane.report_agent_session")]
    PaneReportAgentSession(PaneReportAgentSessionParams),
    #[serde(rename = "pane.report_turn")]
    PaneReportTurn(PaneReportTurnParams),
    #[serde(rename = "pane.report_awaiting_reply")]
    PaneReportAwaitingReply(PaneReportAwaitingReplyParams),
    #[serde(rename = "pane.clear_awaiting_reply")]
    PaneClearAwaitingReply(PaneClearAwaitingReplyParams),
    #[serde(rename = "pane.report_task")]
    PaneReportTask(PaneReportTaskParams),
    #[serde(rename = "pane.report_limit")]
    PaneReportLimit(PaneReportLimitParams),
    #[serde(rename = "pane.report_metadata")]
    PaneReportMetadata(PaneReportMetadataParams),
    #[serde(rename = "pane.clear_agent_authority")]
    PaneClearAgentAuthority(PaneClearAgentAuthorityParams),
    #[serde(rename = "pane.release_agent")]
    PaneReleaseAgent(PaneReleaseAgentParams),
    #[serde(rename = "pane.forget_agent_session")]
    PaneForgetAgentSession(PaneForgetAgentSessionParams),
    #[serde(rename = "pane.report_agent_stopped")]
    PaneReportAgentStopped(PaneReportAgentStoppedParams),
    #[serde(rename = "pane.close")]
    PaneClose(PaneTarget),
    #[serde(rename = "popup.close")]
    PopupClose(EmptyParams),
    #[serde(rename = "events.subscribe")]
    EventsSubscribe(EventsSubscribeParams),
    #[serde(rename = "events.wait")]
    EventsWait(EventsWaitParams),
    #[serde(rename = "pane.wait_for_output")]
    PaneWaitForOutput(PaneWaitForOutputParams),
    #[serde(rename = "integration.list")]
    IntegrationList(EmptyParams),
    #[serde(rename = "usage.read")]
    UsageRead(UsageReadParams),
    #[serde(rename = "usage.settings")]
    UsageSettings(EmptyParams),
    #[serde(rename = "usage.set_enabled")]
    UsageSetEnabled(UsageSetEnabledParams),
    #[serde(rename = "usage.set_provider")]
    UsageSetProvider(UsageSetProviderParams),
    #[serde(rename = "notification.list")]
    NotificationList(EmptyParams),
    #[serde(rename = "integration.install")]
    IntegrationInstall(IntegrationInstallParams),
    #[serde(rename = "integration.uninstall")]
    IntegrationUninstall(IntegrationUninstallParams),
    #[serde(rename = "plugin.link")]
    PluginLink(PluginLinkParams),
    #[serde(rename = "plugin.list")]
    PluginList(PluginListParams),
    #[serde(rename = "plugin.unlink")]
    PluginUnlink(PluginUnlinkParams),
    #[serde(rename = "plugin.enable")]
    PluginEnable(PluginSetEnabledParams),
    #[serde(rename = "plugin.disable")]
    PluginDisable(PluginSetEnabledParams),
    #[serde(rename = "plugin.action.list")]
    PluginActionList(PluginActionListParams),
    #[serde(rename = "plugin.action.invoke")]
    PluginActionInvoke(PluginActionInvokeParams),
    #[serde(rename = "plugin.log.list")]
    PluginLogList(PluginLogListParams),
    #[serde(rename = "plugin.pane.open")]
    PluginPaneOpen(PluginPaneOpenParams),
    #[serde(rename = "plugin.pane.focus")]
    PluginPaneFocus(PluginPaneFocusParams),
    #[serde(rename = "plugin.pane.close")]
    PluginPaneClose(PluginPaneCloseParams),
}

#[cfg(test)]
mod tests;
