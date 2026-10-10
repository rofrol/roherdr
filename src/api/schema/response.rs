use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, AgentPromptRequest, AgentTurnEndReason};
use super::common::{ClientWindowTitleReason, NotificationShowReason};
use super::coordinators::{
    CoordinatorAllowlistMode, CoordinatorInfo, CoordinatorOverride, CoordinatorWouldDeny,
    CoordinatorWouldDenyShape,
};
use super::events::EventEnvelope;
use super::history::{HistoryItem, HistoryItemSummary, HistoryReconcile};
use super::integrations::{
    IntegrationInstallResult, IntegrationTarget, IntegrationUninstallResult,
};
use super::panes::{
    LayoutDescription, PaneEdgesResult, PaneFocusDirectionResult, PaneInfo, PaneLayoutSnapshot,
    PaneMoveResult, PaneNeighborResult, PaneProcessInfo, PaneReadResult, PaneResizeResult,
    PaneSwapResult, PaneTextPoint, PaneTextRange, PaneZoomResult,
};
use super::plugins::{
    InstalledPluginInfo, PluginActionInfo, PluginCommandLogInfo, PluginInvocationContext,
    PluginPaneInfo,
};
use super::reports::ReportInfo;
use super::server::{ServerCapabilities, SystemPtyUsageInfo};
use super::session::SessionSnapshot;
use super::tabs::TabInfo;
use super::todo::{
    TodoChainInfo, TodoLanding, TodoQueueInfo, TodoReview, TodoRunEvent, TodoRunInfo,
};
use super::workers::{
    WorkerAttentionReason, WorkerDrain, WorkerInfo, WorkerItemRuns, WorkerKillReport,
    WorkerObligation, WorkerQuestion, WorkerQuestionDetail, WorkerRun, WorkerVerification,
};
use super::workspaces::WorkspaceInfo;
use super::worktrees::{GitBranchInfo, WorktreeInfo, WorktreeSourceInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SuccessResponse {
    pub id: String,
    pub result: ResponseResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorResponse {
    pub id: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseResult {
    Pong {
        version: String,
        protocol: u32,
        #[serde(default)]
        capabilities: Option<ServerCapabilities>,
    },
    PtyUsage {
        /// Absent where the platform has no fixed pool (Windows) or the
        /// count cannot be read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<SystemPtyUsageInfo>,
    },
    SessionSnapshot {
        snapshot: Box<SessionSnapshot>,
    },
    WorkerInfo {
        worker: WorkerInfo,
    },
    WorkerList {
        workers: Vec<WorkerInfo>,
    },
    /// `worker.question`'s reply.
    WorkerQuestionDetail {
        detail: WorkerQuestionDetail,
    },
    /// `worker.wait`'s reply with `until: attention`: why it returned, the
    /// questions pending then, the worker, and `seq`, its latest event's,
    /// which the next wait passes as `after`.
    WorkerAttention {
        reason: WorkerAttentionReason,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        questions: Vec<WorkerQuestion>,
        seq: i64,
        worker: WorkerInfo,
    },
    /// `worker.obligations`' reply: the owned workers with an event their
    /// owner has not acknowledged, oldest worker first.
    WorkerObligations {
        obligations: Vec<WorkerObligation>,
    },
    /// `worker.drain`'s and `worker.wait_drained`'s reply.
    WorkerDrain {
        drain: WorkerDrain,
    },
    /// `worker.runs`' reply: each item's runs, the item whose first run
    /// started first first, then the runs started without an item.
    WorkerRuns {
        items: Vec<WorkerItemRuns>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unassigned: Vec<WorkerRun>,
    },
    /// `worker.verify`'s reply: the verdict herdr decided and its evidence.
    WorkerVerification {
        verification: WorkerVerification,
    },
    /// `todo.run`'s, `todo.resume`'s and `todo.status`' reply.
    TodoRun {
        run: TodoRunInfo,
    },
    /// `todo.runs`' reply, oldest first.
    TodoRuns {
        runs: Vec<TodoRunInfo>,
        /// With `commit`: the landing that named the run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        landing: Option<TodoLanding>,
    },
    /// `todo.review`'s reply: one attempt of a run in one view.
    TodoReview {
        review: Box<TodoReview>,
    },
    /// `todo.next`'s reply: the item, its coordinator's tenure and worker,
    /// and the chain when one was asked for.
    TodoNext {
        item: String,
        coordinator: CoordinatorInfo,
        worker: WorkerInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chain: Option<TodoChainInfo>,
    },
    /// `todo.stop`'s reply: the chain as it is now.
    TodoChain {
        chain: TodoChainInfo,
    },
    /// `todo.queue_set`'s and `todo.queue_status`' reply.
    TodoQueue {
        queue: TodoQueueInfo,
    },
    /// `history.list`'s reply: each recorded item with its latest record,
    /// the most recent first.
    HistoryList {
        items: Vec<HistoryItemSummary>,
    },
    /// `history.item`'s reply.
    HistoryItem {
        item: HistoryItem,
    },
    /// `history.reconcile`'s reply.
    HistoryReconcile {
        reconcile: HistoryReconcile,
    },
    /// `todo.wait`'s reply: the event and the run as it is now.
    TodoRunEvent {
        event: TodoRunEvent,
        run: TodoRunInfo,
    },
    /// `coordinator.start`'s, `coordinator.end`'s and
    /// `coordinator.handoff`'s reply.
    Coordinator {
        coordinator: CoordinatorInfo,
    },
    /// `coordinator.status`'s reply: the active tenures, oldest first.
    Coordinators {
        coordinators: Vec<CoordinatorInfo>,
    },
    /// `coordinator.record_override`'s reply: the stored record.
    CoordinatorOverride {
        record: CoordinatorOverride,
    },
    /// `history.overrides`' reply: the recorded allowlist exceptions, oldest
    /// first.
    HistoryOverrides {
        overrides: Vec<CoordinatorOverride>,
    },
    /// `coordinator.allowlist_refusal`'s reply: the configured mode, and in
    /// shadow mode the stored would-deny (absent when it could not be
    /// stored; the call runs anyway).
    CoordinatorAllowlistRefusal {
        mode: CoordinatorAllowlistMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record: Option<CoordinatorWouldDeny>,
    },
    /// `history.would_deny`'s reply: the would-denies, oldest first, and
    /// their count per command shape, the most frequent first.
    HistoryWouldDeny {
        would_deny: Vec<CoordinatorWouldDeny>,
        shapes: Vec<CoordinatorWouldDenyShape>,
    },
    /// `report.record`'s reply: the report with this occurrence's number,
    /// and whom it notified (`coordinator <id>` or `user`; absent when it
    /// was a repeat and notified no one).
    ReportRecorded {
        report: ReportInfo,
        occurrence: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notified: Option<String>,
    },
    /// `report.list`'s reply: the reports, the latest occurrence first.
    Reports {
        reports: Vec<ReportInfo>,
    },
    /// `report.close`'s reply.
    Report {
        report: ReportInfo,
    },
    /// `worker.kill`'s reply: the worker and what was signalled.
    WorkerKilled {
        worker: WorkerInfo,
        killed: WorkerKillReport,
    },
    WorkspaceInfo {
        workspace: WorkspaceInfo,
    },
    WorkspaceCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    WorkspaceList {
        workspaces: Vec<WorkspaceInfo>,
    },
    WorktreeList {
        source: WorktreeSourceInfo,
        worktrees: Vec<WorktreeInfo>,
    },
    GitBranchList {
        branches: Vec<GitBranchInfo>,
    },
    WorktreeCreated {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
    },
    WorktreeOpened {
        workspace: WorkspaceInfo,
        tab: TabInfo,
        root_pane: PaneInfo,
        worktree: WorktreeInfo,
        already_open: bool,
    },
    WorktreeRemoved {
        workspace_id: String,
        path: String,
        forced: bool,
    },
    TabInfo {
        tab: TabInfo,
    },
    TabCreated {
        tab: TabInfo,
        root_pane: PaneInfo,
    },
    TabList {
        tabs: Vec<TabInfo>,
    },
    AgentInfo {
        agent: AgentInfo,
    },
    AgentStarted {
        agent: AgentInfo,
        argv: Vec<String>,
    },
    /// Canonical ids of the interactive agents whose executable is on the
    /// server's `PATH`, in Herdr's agent order.
    AgentKindList {
        kinds: Vec<String>,
    },
    AgentPrompted {
        agent: AgentInfo,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_request: Option<AgentPromptRequest>,
    },
    AgentPromptStatus {
        pane_id: String,
        prompt_request: AgentPromptRequest,
    },
    /// `agent.wait_turn`: how the followed prompt's turn ended.
    AgentTurnEnded {
        request_id: String,
        reason: AgentTurnEndReason,
        /// The pane the request was typed into, when herdr still knew the request.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        /// The error a `failed` turn ended on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    AgentList {
        agents: Vec<AgentInfo>,
    },
    AgentView {
        active: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    PaneInfo {
        pane: PaneInfo,
    },
    PaneList {
        panes: Vec<PaneInfo>,
    },
    PaneCurrent {
        pane: PaneInfo,
    },
    PaneSwap {
        swap: PaneSwapResult,
    },
    PaneMove {
        move_result: PaneMoveResult,
    },
    PaneZoom {
        zoom: PaneZoomResult,
    },
    PaneLayout {
        layout: PaneLayoutSnapshot,
    },
    PaneProcessInfo {
        process_info: PaneProcessInfo,
    },
    LayoutExport {
        layout: LayoutDescription,
    },
    LayoutApply {
        layout: LayoutDescription,
    },
    LayoutSplitRatioSet {
        layout: LayoutDescription,
    },
    PaneNeighbor {
        neighbor: PaneNeighborResult,
    },
    PaneEdges {
        edges: PaneEdgesResult,
    },
    PaneFocusDirection {
        focus: PaneFocusDirectionResult,
    },
    PaneResize {
        resize: PaneResizeResult,
    },
    PaneRead {
        read: PaneReadResult,
    },
    PaneSelection {
        pane_id: String,
        text: String,
    },
    PaneCopyMotion {
        pane_id: String,
        cursor: PaneTextPoint,
        content_revision: u64,
    },
    PaneCopySearch {
        pane_id: String,
        content_revision: u64,
        matches: Vec<PaneTextRange>,
        total: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_global: Option<u64>,
    },
    AgentExplain {
        explain: serde_json::Value,
    },
    SubscriptionStarted {},
    WaitMatched {
        event: EventEnvelope,
    },
    OutputMatched {
        pane_id: String,
        revision: u64,
        matched_line: Option<String>,
        read: PaneReadResult,
    },
    NotificationShow {
        shown: bool,
        reason: NotificationShowReason,
    },
    ClientWindowTitle {
        changed: bool,
        reason: ClientWindowTitleReason,
    },
    IntegrationList {
        integrations: Vec<super::integrations::IntegrationInfo>,
    },
    UsageRead {
        usage: super::usage::UsageReport,
    },
    UsageSettings {
        settings: super::usage::UsageSettings,
    },
    NotificationList {
        notifications: Vec<super::NotificationRecord>,
    },
    IntegrationInstall {
        target: IntegrationTarget,
        details: IntegrationInstallResult,
    },
    IntegrationUninstall {
        target: IntegrationTarget,
        details: IntegrationUninstallResult,
    },
    AgentManifestReload {
        manifests: Vec<AgentManifestInfo>,
    },
    AgentManifestStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_check_unix: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_result: Option<String>,
        manifests: Vec<AgentManifestInfo>,
    },
    PluginLinked {
        plugin: InstalledPluginInfo,
    },
    PluginList {
        plugins: Vec<InstalledPluginInfo>,
    },
    PluginUnlinked {
        plugin_id: String,
        removed: bool,
    },
    PluginEnabled {
        plugin: InstalledPluginInfo,
    },
    PluginDisabled {
        plugin: InstalledPluginInfo,
    },
    PluginActionList {
        actions: Vec<PluginActionInfo>,
    },
    PluginActionInvoked {
        action: PluginActionInfo,
        context: PluginInvocationContext,
        log: PluginCommandLogInfo,
    },
    PaneLinkResolved {
        regions: Vec<super::panes::PaneLinkRegion>,
    },
    PaneLinkActivated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        handled: bool,
    },
    PluginLogList {
        logs: Vec<PluginCommandLogInfo>,
    },
    PluginPaneOpened {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneFocused {
        plugin_pane: PluginPaneInfo,
    },
    PluginPaneClosed {
        pane_id: String,
    },
    ConfigReload {
        status: crate::config::ConfigReloadStatus,
        diagnostics: Vec<String>,
    },
    /// Acknowledgement for the client-shell surface interest lease. This method is new on the
    /// endpoint protocol, so its revision-bearing result can establish an activation floor.
    ClientShellSurfaceSet {
        active: bool,
        projection_revision: u64,
    },
    Ok {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentManifestInfo {
    pub agent: String,
    pub source: String,
    pub source_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_remote_version: Option<String>,
    pub local_override_shadowing_remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_last_checked_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}
