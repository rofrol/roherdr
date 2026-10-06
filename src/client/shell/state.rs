use super::*;

pub(super) const MIN_TAB_WIDTH: u16 = 8;
pub(super) const NEW_TAB_WIDTH: u16 = 3;
pub(super) const WORKSPACE_HEADER_ROWS: u16 = 1;
const ENDPOINT_ERROR_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientShellKeybindingSource {
    Local,
    RemoteLocal,
    Endpoint,
}

pub(crate) struct ClientShellConfig {
    pub(super) sidebar_width: u16,
    pub(super) sidebar_min_width: u16,
    pub(super) sidebar_max_width: u16,
    pub(super) sidebar_start_collapsed: bool,
    pub(super) sidebar_collapsed_mode: SidebarCollapsedModeConfig,
    pub(super) mobile_width_threshold: u16,
    pub(super) tab_bar_position: TabBarPositionConfig,
    pub(super) tab_label: crate::config::TabLabelConfig,
    pub(super) new_tab_position: crate::config::NewTabPositionConfig,
    pub(super) hide_tab_bar_when_single_tab: bool,
    pub(super) spaces: SpacesSidebarConfig,
    pub(super) show_agents_panel: bool,
    pub(super) agents: crate::config::AgentsSidebarConfig,
    pub(super) agent_panel_sort: crate::config::AgentPanelSortConfig,
    pub(super) status_indicators: crate::config::StatusIndicatorStyle,
    pub(super) animations: bool,
    pub(super) sound_enabled: bool,
    pub(super) toast_delivery: crate::config::ToastDelivery,
    pub(super) toast_delay_seconds: u64,
    pub(super) toast_alert_on_finished: bool,
    pub(super) toast_position: crate::config::ToastHerdrPosition,
    pub(super) toast_bottom_margin: u16,
    pub(super) copy_on_select: bool,
    pub(super) clipboard_toast_enabled: bool,
    pub(super) clipboard_toast_position: crate::config::ToastClipboardPosition,
    pub(super) theme_name: String,
    pub(super) theme_runtime: crate::app::state::ThemeRuntimeConfig,
    pub(super) palette: Palette,
    pub(super) keybinds: LiveKeybindConfig,
    pub(super) local_keys: crate::config::KeysConfig,
    pub(super) keybinding_source: ClientShellKeybindingSource,
    pub(super) prompt_new_tab_name: bool,
    pub(super) prompt_new_workspace_name: bool,
    pub(super) confirm_close: bool,
    pub(super) confirm_close_running: bool,
    pub(super) mouse_capture: bool,
    pub(super) mouse_scroll_lines: usize,
    pub(super) right_click_passthrough_modifiers: Option<crossterm::event::KeyModifiers>,
    pub(super) redraw_on_focus_gained: bool,
    pub(super) switch_ascii_input_source_in_prefix: bool,
    pub(super) local_config_path: std::path::PathBuf,
    pub(super) preferences_path: Option<std::path::PathBuf>,
    pub(super) preferences: preferences::ClientChromePreferences,
    pub(super) startup_config_diagnostic: Option<String>,
    pub(super) startup_onboarding: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClientShellLayout {
    pub sidebar: Rect,
    pub tab_bar: Rect,
    /// Children of the active tab, next to the tab bar on the pane side.
    pub child_tab_bar: Rect,
    pub mobile_header: Rect,
    pub pane_surface: Rect,
    pub job_footer: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClientMobileTarget {
    Machine(ClientEndpointId),
    NewWorkspace,
    Workspace {
        endpoint_id: ClientEndpointId,
        workspace_id: String,
    },
    NewTab,
    Tab {
        endpoint_id: ClientEndpointId,
        tab_id: String,
    },
    Agent {
        endpoint_id: ClientEndpointId,
        pane_id: String,
    },
    Menu(usize),
}

#[derive(Default)]
pub(super) struct ShellHitMap {
    pub(super) machines: Vec<MachineHit>,
    pub(super) workspaces: Vec<WorkspaceHit>,
    /// Every space of the local sidebar in screen rows, see
    /// [`WorkspaceLayout`]; empty for the multi-machine sidebar.
    pub(super) workspace_layout: Vec<WorkspaceLayout>,
    pub(super) workspace_body: Rect,
    pub(super) workspace_scrollbar: Rect,
    pub(super) workspace_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) workspace_max_scroll: usize,
    pub(super) tabs: Vec<(Rect, String)>,
    pub(super) child_tabs: Vec<(Rect, String)>,
    pub(super) panes: Vec<PaneHit>,
    pub(super) popup: Option<PaneHit>,
    pub(super) job_footer: Rect,
    pub(super) pane_splits: Vec<PaneSplitHit>,
    pub(super) agents: Vec<(Rect, String)>,
    /// Tab lines under a space (`ui.sidebar.spaces.tabs`), with their tab ids.
    pub(super) space_tabs: Vec<(Rect, String)>,
    /// The `+` at the end of a space's name line, with the space it adds a
    /// tab to.
    pub(super) space_new_tab: Vec<(Rect, String)>,
    /// A space's push status chip, which opens its branch menu.
    pub(super) space_push_status: Vec<(Rect, String)>,
    /// A space's launch button, left of its `+`.
    pub(super) space_launch_agent: Vec<(Rect, String)>,
    /// The `⤒` on a hovered space's name line: that space to the top.
    pub(super) space_to_top: Vec<(Rect, String)>,
    /// Drawn targets whose text is cut, for tooltips.
    pub(super) tooltips: Vec<super::tooltip::TooltipTarget>,
    /// Disclosure triangles and counts at the end of tab lines, with the
    /// tab whose squares they fold.
    pub(super) space_tab_folds: Vec<(Rect, String)>,
    /// Squares of nested tabs under an unfolded tab line, with their tab.
    pub(super) space_tab_squares: Vec<(Rect, String)>,
    /// Blank slots of job tabs that closed while the pointer was over the
    /// sidebar.
    pub(super) space_tab_gone: Vec<Rect>,
    /// The spaces' root order drawn, taken into `held_space_order`.
    pub(super) space_order: Vec<String>,
    /// The square order drawn, taken into `held_squares` after each frame.
    pub(super) space_tab_square_order: super::space_tabs::HeldSquares,
    pub(super) endpoint_agents: Vec<(Rect, ClientEndpointId, String)>,
    pub(super) agent_body: Rect,
    pub(super) agent_scrollbar: Rect,
    pub(super) agent_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) agent_max_scroll: usize,
    pub(super) agent_sort_toggle: Rect,
    /// The `cust`, `name` and `prio` buttons in the spaces header.
    pub(super) space_sort_buttons: Vec<(Rect, super::space_sort::SpaceSortKey)>,
    pub(super) sidebar_divider: Rect,
    pub(super) sidebar_section_divider: Rect,
    /// Rows the spaces and detail sections split between them; dragging the
    /// section divider maps a row to a split ratio within it.
    pub(super) sidebar_sections: Rect,
    pub(super) sidebar_toggle: Rect,
    pub(super) new_workspace: Rect,
    pub(super) new_tab: Rect,
    pub(super) tab_scroll_left: Rect,
    pub(super) tab_scroll_right: Rect,
    /// The whole main tab row and child tab row: the wheel steps through
    /// their tabs anywhere on them, gaps and status included.
    pub(super) tab_bar: Rect,
    pub(super) child_tab_bar: Rect,
    pub(super) mobile_switch: Rect,
    pub(super) mobile_close: Rect,
    pub(super) mobile_targets: Vec<(Rect, ClientMobileTarget)>,
    pub(super) mobile_max_scroll: usize,
    pub(super) global_launcher: Rect,
    pub(super) usage_footer: Rect,
    pub(super) notification_toast: Rect,
    pub(super) global_menu_rows: Vec<(Rect, usize)>,
    /// The notification history button at the right of the spaces header.
    pub(super) notification_log_button: Rect,
    /// The header indicators of agents working and agents asking.
    pub(super) working_list_button: Rect,
    pub(super) asking_list_button: Rect,
    /// The `★` button that lists the bookmarked tabs.
    pub(super) bookmarks_list_button: Rect,
    /// The items of a header list row's menu, open over the list.
    pub(super) list_menu_rows: Vec<(Rect, usize)>,
    /// The rows of the image list.
    pub(super) image_picker_rows: Vec<(Rect, usize)>,
    /// Where the image list shows the highlighted image.
    pub(super) image_preview: Rect,
    /// The `/ filter` button in the sidebar's bottom row that opens the filter bar.
    pub(super) space_filter_button: Rect,
    /// The header button that puts away the quiet tabs of every space.
    pub(super) quiet_fold_button: Rect,
    /// The header's back and forward buttons over focus jumps.
    pub(super) focus_back_button: Rect,
    pub(super) focus_forward_button: Rect,
    /// The filter bar, and the `×` at its right end that closes it.
    pub(super) space_filter_bar: Rect,
    pub(super) space_filter_close: Rect,
    pub(super) notification_log_rows: Vec<(Rect, usize)>,
    pub(super) context_menu_rows: Vec<(Rect, usize)>,
    pub(super) overlay_primary: Rect,
    pub(super) overlay_clear: Rect,
    pub(super) overlay_cancel: Rect,
    pub(super) navigator_popup: Rect,
    pub(super) navigator_search: Rect,
    pub(super) navigator_rows: Vec<(Rect, ClientNavigatorTarget)>,
    pub(super) navigator_scrollbar: Rect,
    pub(super) navigator_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) worktree_search: Rect,
    pub(super) worktree_rows: Vec<(Rect, usize)>,
    pub(super) help_popup: Rect,
    pub(super) help_scrollbar: Rect,
    pub(super) help_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) help_max_scroll: usize,
    pub(super) settings_popup: Rect,
    pub(super) settings_tabs: Vec<(Rect, ClientSettingsSection)>,
    pub(super) settings_choices: Vec<(Rect, usize)>,
    pub(super) product_announcement_scrollbar: Rect,
    pub(super) product_announcement_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) product_announcement_max_scroll: usize,
    pub(super) release_notes_scrollbar: Rect,
    pub(super) release_notes_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(super) release_notes_max_scroll: usize,
}

#[derive(Clone)]
pub(super) struct PaneHit {
    pub(super) rect: Rect,
    pub(super) inner_rect: Rect,
    pub(super) scrollbar_rect: Option<Rect>,
    pub(super) scroll: Option<crate::pane::ScrollMetrics>,
    pub(super) pane_id: String,
    pub(super) popup: bool,
    pub(super) mouse_reporting: bool,
    pub(super) sgr_pixel_mouse: bool,
    pub(super) pixel_width: u32,
    pub(super) pixel_height: u32,
}

#[derive(Clone)]
pub(super) struct PaneSplitHit {
    pub(super) direction: crate::protocol::PaneSurfaceSplitDirection,
    pub(super) pos: u16,
    pub(super) area: Rect,
    pub(super) hit_rect: Rect,
    pub(super) path: Vec<bool>,
    pub(super) topology_signature: u64,
}

pub(super) struct ClientPaneMouseGesture {
    pub(super) hit: PaneHit,
    pub(super) button: crossterm::event::MouseButton,
    pub(super) stripped_modifiers: crossterm::event::KeyModifiers,
    pub(super) last_event: crossterm::event::MouseEvent,
    pub(super) last_position: crate::protocol::ClientMousePosition,
}

pub(super) struct ClientWorkspacePress {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
    /// Set once the pointer moved but the space cannot be dragged; the
    /// sidebar header says why.
    pub(super) refused: Option<WorkspaceDragRefusal>,
}

/// Why a space cannot be dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceDragRefusal {
    /// Only the custom order can be rearranged.
    Sort,
    /// A linked worktree moves with its parent space.
    LinkedWorktree,
    /// Spaces of another endpoint are not reordered from here.
    Remote,
    /// A filtered list hides spaces, so a drop slot would be ambiguous.
    Filtered,
}

impl WorkspaceDragRefusal {
    pub(super) fn hint(self) -> &'static str {
        match self {
            Self::Sort => "use manual to reorder",
            Self::LinkedWorktree => "moves with its parent",
            Self::Remote => "can't reorder here",
            Self::Filtered => "clear the filter to reorder",
        }
    }
}

pub(super) struct ClientTabPress {
    pub(super) tab_id: String,
    pub(super) workspace_id: String,
    /// Pressed in the main row, not the second row (where a parent also has
    /// its own entry).
    pub(super) main_row: bool,
    /// Pressed on a tab line in the sidebar's spaces list, not in the tab
    /// bar: the click opens the tab on release, a drag reorders it there.
    pub(super) sidebar_line: bool,
    pub(super) start_column: u16,
    pub(super) start_row: u16,
}

/// Where a space's tab lines were drawn: (index among its top-level tabs, first
/// row) per drawn line in order, and the row below the space's block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TabLineGeometry {
    pub(super) lines: Vec<(usize, i32)>,
    pub(super) bottom: i32,
    /// The space's top-level tab ids at the start: a drag cancels when they
    /// change under it (a tab closed or added elsewhere), since the rows no
    /// longer match the tabs.
    pub(super) top_level: Vec<String>,
}

pub(super) enum ClientChromeDrag {
    SidebarWidth,
    SidebarSection,
    WorkspaceScrollbar {
        grab_row_offset: u16,
    },
    AgentScrollbar {
        grab_row_offset: u16,
    },
    HelpScrollbar {
        grab_row_offset: u16,
    },
    NavigatorScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
        insert_index: Option<usize>,
    },
    /// A tab line dragged in the sidebar's spaces list, within its space.
    TabLine {
        tab_id: String,
        workspace_id: String,
        /// Where the tab would land among its space's top-level tabs (counting
        /// the dragged one), or none while the pointer is outside them.
        insert_index: Option<usize>,
        /// The lines' rows when the drag started. The list shows the tab at
        /// its landing slot, so the drop is measured against these, never the
        /// reordered frame.
        geometry: TabLineGeometry,
    },
    Workspace {
        source_workspace_id: String,
        /// Where the space would land: before this space, or `None` for the end.
        target: Option<Option<String>>,
        /// Rows between the dragged block's top and the row it was grabbed at.
        grab_offset: u16,
    },
    PaneSplit {
        hit: PaneSplitHit,
        tab_id: String,
        grab_offset: i32,
        last_sent_ratio: Option<f32>,
        last_sent_at: Option<std::time::Instant>,
    },
    PaneScrollbar {
        hit: PaneHit,
        grab_row_offset: u16,
        last_sent_offset: Option<usize>,
        last_sent_at: Option<std::time::Instant>,
    },
}

#[derive(Clone)]
pub(super) struct WorkspaceHit {
    pub(super) rect: Rect,
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) workspace_id: String,
    pub(super) indented: bool,
    pub(super) group_toggle: Option<(Rect, String)>,
}

/// The space and its row at the top of the local spaces list, with the
/// scroll offset that showed it: the next frame keeps that row at the top
/// when rows above it come or go, as long as the offset was not changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScrollAnchor {
    pub(super) workspace_id: String,
    pub(super) row: usize,
    pub(super) scroll: usize,
}

/// Where a space of the local sidebar is, in screen rows, whether it is
/// drawn or scrolled out of the list: `top` may be above the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspaceLayout {
    pub(super) workspace_id: String,
    pub(super) indented: bool,
    pub(super) top: i32,
    pub(super) bottom: i32,
}

/// Moves a hit rect drawn at row 0 of a space block's scratch buffer to the
/// screen (`dy` rows down) and clips it to the block's visible part.
fn shift_rect(rect: Rect, dy: i32, visible: Rect) -> Option<Rect> {
    let y = i32::from(rect.y) + dy;
    let top = y.max(i32::from(visible.y));
    let bottom = (y + i32::from(rect.height)).min(i32::from(visible.bottom()));
    (bottom > top).then(|| Rect::new(rect.x, top as u16, rect.width, (bottom - top) as u16))
}

impl ShellHitMap {
    /// Moves the hits of a space block drawn off screen to where its visible
    /// rows were copied, dropping those that are not shown.
    pub(super) fn shift_space_block(&mut self, dy: i32, visible: Rect) {
        let shift_all = |hits: &mut Vec<(Rect, String)>| {
            *hits = std::mem::take(hits)
                .into_iter()
                .filter_map(|(rect, id)| Some((shift_rect(rect, dy, visible)?, id)))
                .collect();
        };
        shift_all(&mut self.space_tabs);
        shift_all(&mut self.space_new_tab);
        shift_all(&mut self.space_push_status);
        shift_all(&mut self.space_launch_agent);
        shift_all(&mut self.space_to_top);
        self.tooltips = std::mem::take(&mut self.tooltips)
            .into_iter()
            .filter_map(|mut target| {
                target.rect = shift_rect(target.rect, dy, visible)?;
                Some(target)
            })
            .collect();
        shift_all(&mut self.space_tab_folds);
        shift_all(&mut self.space_tab_squares);
        self.space_tab_gone = std::mem::take(&mut self.space_tab_gone)
            .into_iter()
            .filter_map(|rect| shift_rect(rect, dy, visible))
            .collect();
        for hit in &mut self.workspaces {
            hit.rect = shift_rect(hit.rect, dy, visible).unwrap_or_default();
            hit.group_toggle = hit
                .group_toggle
                .take()
                .and_then(|(rect, key)| Some((shift_rect(rect, dy, visible)?, key)));
        }
    }

    /// A copy of a space block's positional hits, for one of the pieces the
    /// block is drawn in when worktree spaces are nested inside it. The
    /// square order is not positional; the caller merges it once.
    pub(super) fn space_block_piece(&self) -> ShellHitMap {
        ShellHitMap {
            workspaces: self.workspaces.clone(),
            space_tabs: self.space_tabs.clone(),
            space_new_tab: self.space_new_tab.clone(),
            space_push_status: self.space_push_status.clone(),
            space_launch_agent: self.space_launch_agent.clone(),
            space_to_top: self.space_to_top.clone(),
            tooltips: self.tooltips.clone(),
            space_tab_folds: self.space_tab_folds.clone(),
            space_tab_squares: self.space_tab_squares.clone(),
            space_tab_gone: self.space_tab_gone.clone(),
            ..ShellHitMap::default()
        }
    }

    /// Adds a space block's hits.
    pub(super) fn merge_space_block(&mut self, block: ShellHitMap) {
        self.workspaces.extend(block.workspaces);
        self.space_tabs.extend(block.space_tabs);
        self.space_new_tab.extend(block.space_new_tab);
        self.space_push_status.extend(block.space_push_status);
        self.space_launch_agent.extend(block.space_launch_agent);
        self.space_to_top.extend(block.space_to_top);
        self.tooltips.extend(block.tooltips);
        self.space_tab_folds.extend(block.space_tab_folds);
        self.space_tab_squares.extend(block.space_tab_squares);
        self.space_tab_gone.extend(block.space_tab_gone);
        self.space_tab_square_order
            .extend(block.space_tab_square_order);
    }
}

#[derive(Debug)]
pub(crate) enum ClientShellAction {
    Endpoint {
        endpoint_id: ClientEndpointId,
        boot_id: String,
        request: Box<crate::api::schema::Request>,
    },
    ClipboardWrite(Vec<u8>),
    OpenSafeWebUrl(String),
    /// Image files from this machine to paste into a pane, staged by the
    /// server like a clipboard image.
    AttachImages(super::image_picker::AttachImages),
    /// An image file from this machine to show in Quick Look, or to close
    /// when it is already shown.
    PreviewImage(std::path::PathBuf),
    ActivateEndpoint {
        endpoint_id: ClientEndpointId,
        target: Option<ClientEndpointFocusTarget>,
    },
    ReplayMouse(Vec<crossterm::event::MouseEvent>),
    Keybind(crate::input::KeybindAction),
}

#[derive(Default)]
pub(crate) struct ClientShellInput {
    pub detach: bool,
    pub repaint: bool,
    pub resize: bool,
    pub query_host_appearance: bool,
    pub query_host_theme: bool,
    pub requests: Vec<ClientMessage>,
    pub actions: Vec<ClientShellAction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellMode {
    Terminal,
    Prefix,
    Navigate,
    Resize,
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientShellOverlayKind {
    Onboarding,
    ProductAnnouncement,
    ReleaseNotes,
    Rename,
    ConfirmClose,
    Help,
    Navigator,
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove,
    ContextMenu,
    GlobalMenu,
    NotificationLog,
    Settings,
    Usage,
    ImagePicker,
}

#[derive(Debug)]
pub(super) enum ClientRenameTarget {
    NewWorkspace {
        source_workspace_id: Option<String>,
        cwd: Option<String>,
        suggested_name: String,
    },
    Workspace {
        workspace_id: String,
    },
    NewTab {
        workspace_id: String,
        default_name: String,
    },
    Tab {
        tab_id: String,
        auto_name: bool,
        original_name: String,
    },
    Pane {
        pane_id: String,
    },
}

#[derive(Debug)]
pub(super) struct ClientRenameOverlay {
    pub(super) title: &'static str,
    pub(super) input: TextEditor,
    pub(super) target: ClientRenameTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientNavigatorFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientNavigatorTarget {
    Machine {
        endpoint_id: ClientEndpointId,
    },
    Workspace {
        endpoint_id: ClientEndpointId,
        workspace_id: String,
    },
    Pane {
        endpoint_id: ClientEndpointId,
        pane_id: String,
    },
}

#[derive(Clone, Debug)]
pub(super) struct ClientNavigatorRow {
    pub(super) depth: u8,
    pub(super) label: String,
    pub(super) meta: String,
    pub(super) detail: String,
    pub(super) agent: Option<String>,
    pub(super) status: Option<crate::api::schema::AgentStatus>,
    pub(super) stale: bool,
    pub(super) current: bool,
    pub(super) target: ClientNavigatorTarget,
}

#[derive(Debug)]
pub(super) struct ClientNavigatorOverlay {
    pub(super) query: TextEditor,
    pub(super) search_focused: bool,
    pub(super) selected: Option<ClientNavigatorTarget>,
    pub(super) scroll: usize,
    pub(super) filter: Option<ClientNavigatorFilter>,
}

#[derive(Debug)]
pub(super) struct ClientHelpOverlay {
    /// Shows the status legend instead of the keybinds; it has no search.
    pub(super) legend: bool,
    pub(super) query: TextEditor,
    pub(super) search_focused: bool,
    pub(super) scroll: usize,
}

#[derive(Debug)]
pub(super) struct ClientGlobalMenuOverlay {
    pub(super) highlighted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientSettingsSection {
    Theme,
    Indicators,
    Sound,
    Toast,
    Integrations,
    Usage,
}

impl ClientSettingsSection {
    pub(super) const ALL: &[Self] = &[
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::Toast,
        Self::Integrations,
        Self::Usage,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::Toast => "toasts",
            Self::Integrations => "integrations",
            Self::Usage => "usage",
        }
    }
}

#[derive(Debug)]
pub(super) struct ClientSettingsOverlay {
    pub(super) section: ClientSettingsSection,
    pub(super) selected: usize,
    pub(super) original_theme_name: String,
    pub(super) original_palette: Palette,
    pub(super) integrations: Vec<crate::api::schema::IntegrationInfo>,
    pub(super) integration_messages: Vec<String>,
    pub(super) loading_integrations: bool,
    pub(super) installing_integrations: bool,
    pub(super) usage: ClientUsageSettings,
}

/// The usage tab: the active machine's `[usage]` choices, read and written
/// through its server, which may be another machine than this client's.
#[derive(Debug, Default)]
pub(super) struct ClientUsageSettings {
    pub(super) settings: Option<crate::api::schema::UsageSettings>,
    /// The machine `settings` came from; a change of machine drops them.
    pub(super) endpoint_id: Option<ClientEndpointId>,
    pub(super) loading: bool,
    /// The server predates `usage.settings`.
    pub(super) unsupported: bool,
    /// A provider whose warning was shown; the next apply turns it on.
    pub(super) confirming: Option<String>,
    pub(super) error: Option<String>,
}

#[derive(Debug)]
pub(super) struct ClientWorktreeCreateOverlay {
    pub(super) source_workspace_id: String,
    pub(super) repo_name: String,
    pub(super) branch: TextEditor,
    pub(super) checkout_path: String,
    pub(super) error: Option<String>,
    pub(super) creating: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ClientWorktreeOpenEntry {
    pub(super) path: String,
    pub(super) branch: Option<String>,
    pub(super) is_linked_worktree: bool,
    pub(super) is_detached: bool,
    pub(super) open_workspace_id: Option<String>,
    pub(super) label: String,
}

impl ClientWorktreeOpenEntry {
    pub(super) fn status_label(&self) -> &'static str {
        if self.open_workspace_id.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_detached && self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    pub(super) fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || format!(
                "{} {} {} {}",
                self.label,
                self.branch.as_deref().unwrap_or_default(),
                self.path,
                self.status_label()
            )
            .to_lowercase()
            .contains(&query)
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeOpenOverlay {
    pub(super) source_workspace_id: String,
    pub(super) entries: Vec<ClientWorktreeOpenEntry>,
    pub(super) selected: usize,
    pub(super) query: TextEditor,
    pub(super) search_focused: bool,
    pub(super) error: Option<String>,
    pub(super) opening: bool,
}

impl ClientWorktreeOpenOverlay {
    pub(super) fn filtered_indices(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.matches_query(&self.query).then_some(index))
            .collect()
    }

    pub(super) fn selected_entry_index(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        filtered
            .contains(&self.selected)
            .then_some(self.selected)
            .or_else(|| filtered.first().copied())
    }
}

#[derive(Debug)]
pub(super) struct ClientWorktreeRemoveOverlay {
    pub(super) workspace_id: String,
    pub(super) path: String,
    pub(super) error: Option<String>,
    pub(super) removing: bool,
    pub(super) force_confirmation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientContextMenuAction {
    Rename,
    Close,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    ToggleGroup,
    NewTab,
    RenamePane,
    ClearPaneName,
    SwapWithFocusedPane,
    SplitRight,
    SplitDown,
    Zoom,
    ToggleRightClickPassthrough,
    ClosePane,
    /// Closes the tab's nested job tabs that succeeded.
    CloseSucceededJobs,
    /// Closes the tab's nested job tabs that failed.
    CloseFailedJobs,
    /// Stops the tab's running jobs by closing their tabs, after asking.
    StopRunningJobs,
    /// Adds the tab to the bookmarks, or removes it.
    ToggleBookmark,
    /// Sorts the spaces list by this key (a repeat flips the direction).
    SortSpaces(super::space_sort::SpaceSortKey),
    /// A row that only informs, such as a branch: closes the menu.
    Dismiss,
    /// Launches the agent picker's row at this index in a new tab.
    LaunchAgent(usize),
    /// Dismisses the questions of the tab's (or the pane's) agents that
    /// await a reply, without typing into their panes.
    DismissQuestions,
    /// Opens the image list to attach image files to the pane.
    AttachImage,
}

/// The branches the server listed for the branch menu, or why it could not.
pub(super) type BranchListing = Result<Vec<crate::api::schema::GitBranchInfo>, String>;

/// The buttons at the end of a space's name line that light up on hover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NameLineButton {
    PushStatus,
    Launch,
    NewTab,
    ToTop,
}

#[derive(Debug)]
pub(super) enum ClientContextMenuTarget {
    Workspace {
        workspace_id: String,
        is_git: bool,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        close_group: bool,
        collapsed: bool,
        /// The space's bookmark when the menu opened, or none when the
        /// server cannot bookmark spaces (no menu item then).
        bookmarked: Option<bool>,
    },
    Tab {
        tab_id: String,
        workspace_id: String,
        /// Nested job tabs that run, succeeded and failed, when the menu
        /// opened.
        running_jobs: usize,
        succeeded_jobs: usize,
        failed_jobs: usize,
        /// The tab's bookmark when the menu opened, or none when the server
        /// cannot bookmark (no menu item then).
        bookmarked: Option<bool>,
        /// Opened on a row of a header list, over the list: no `New tab`,
        /// which would not act on the row.
        in_list: bool,
        /// Agents of the tab that await a reply, when the server can dismiss
        /// their questions.
        awaiting_panes: usize,
    },
    /// The spaces list's sort choice, opened from the header button.
    SortSpaces(super::space_sort::SpaceSort),
    /// The installed agents, the launch button's own first, opened by a
    /// right click on the button (or a left click with nothing to repeat).
    AgentPicker {
        workspace_id: String,
        current: Option<String>,
        /// `None` while the server lists them.
        kinds: Option<Result<Vec<String>, String>>,
    },
    /// A space's other branches with their push state, opened from its push
    /// status chip, which the menu's title repeats.
    Branches {
        workspace_id: String,
        branch: Option<String>,
        ahead_behind: Option<(usize, usize)>,
        /// `None` while the server lists them.
        branches: Option<BranchListing>,
    },
    Pane {
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        has_manual_label: bool,
        right_click_passthrough: bool,
        /// The pane's agent awaits a reply and the server can dismiss it.
        awaiting_reply: bool,
    },
}

#[derive(Debug)]
pub(super) struct ClientContextMenuOverlay {
    pub(super) target: ClientContextMenuTarget,
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) highlighted: usize,
}

pub(super) struct ClientContextMenuItem {
    pub(super) label: String,
    pub(super) action: ClientContextMenuAction,
}

#[derive(Debug)]
pub(super) struct ClientTabCloseConfirmation {
    pub(super) tab_id: String,
    pub(super) workspace: WorkspaceNavigationTarget,
    /// Child tabs closed before the tab; the server refuses to close a parent.
    pub(super) children: Vec<String>,
    /// Close only `children`, keeping the tab: stopping its running jobs.
    pub(super) children_only: bool,
}

#[derive(Debug)]
pub(super) struct ClientConfirmCloseOverlay {
    pub(super) workspace_id: String,
    pub(super) close_group: bool,
    pub(super) tab_target: Option<ClientTabCloseConfirmation>,
    /// A single pane to close instead of the tab or workspace.
    pub(super) pane_target: Option<String>,
    pub(super) title: String,
    pub(super) detail: String,
    /// The running work the close would stop, e.g. `build marked running`.
    pub(super) running: Option<String>,
}

#[derive(Debug)]
pub(super) enum ClientShellOverlay {
    Onboarding,
    ProductAnnouncement(crate::app::state::ProductAnnouncementState),
    ReleaseNotes(crate::app::state::ReleaseNotesState),
    Rename(ClientRenameOverlay),
    ConfirmClose(ClientConfirmCloseOverlay),
    Help(ClientHelpOverlay),
    Navigator(ClientNavigatorOverlay),
    WorktreeCreate(ClientWorktreeCreateOverlay),
    WorktreeOpen(ClientWorktreeOpenOverlay),
    WorktreeRemove(ClientWorktreeRemoveOverlay),
    ContextMenu(ClientContextMenuOverlay),
    GlobalMenu(ClientGlobalMenuOverlay),
    NotificationLog(super::notification_log::ClientNotificationLogOverlay),
    Settings(ClientSettingsOverlay),
    Usage(super::usage::ClientUsageOverlay),
    ImagePicker(super::image_picker::ImagePickerOverlay),
}

impl ClientShellOverlay {
    pub(super) fn kind(&self) -> ClientShellOverlayKind {
        match self {
            Self::Onboarding => ClientShellOverlayKind::Onboarding,
            Self::ProductAnnouncement(_) => ClientShellOverlayKind::ProductAnnouncement,
            Self::ReleaseNotes(_) => ClientShellOverlayKind::ReleaseNotes,
            Self::Rename(_) => ClientShellOverlayKind::Rename,
            Self::ConfirmClose(_) => ClientShellOverlayKind::ConfirmClose,
            Self::Help(_) => ClientShellOverlayKind::Help,
            Self::Navigator(_) => ClientShellOverlayKind::Navigator,
            Self::WorktreeCreate(_) => ClientShellOverlayKind::WorktreeCreate,
            Self::WorktreeOpen(_) => ClientShellOverlayKind::WorktreeOpen,
            Self::WorktreeRemove(_) => ClientShellOverlayKind::WorktreeRemove,
            Self::ContextMenu(_) => ClientShellOverlayKind::ContextMenu,
            Self::GlobalMenu(_) => ClientShellOverlayKind::GlobalMenu,
            Self::NotificationLog(_) => ClientShellOverlayKind::NotificationLog,
            Self::Settings(_) => ClientShellOverlayKind::Settings,
            Self::Usage(_) => ClientShellOverlayKind::Usage,
            Self::ImagePicker(_) => ClientShellOverlayKind::ImagePicker,
        }
    }
}

#[derive(Debug)]
pub(super) enum PendingEndpointKind {
    Generic,
    /// A back or forward step over focus jumps; a refusal puts the history
    /// back where the focus is.
    FocusStep {
        endpoint_id: ClientEndpointId,
    },
    /// A tab close; a close the server accepts is remembered for reopening.
    TabClose {
        closed: Option<Box<super::closed_tabs::ClosedTab>>,
    },
    /// The tab create of a reopened tab; its place is restored afterwards.
    ReopenTab {
        closed: Box<super::closed_tabs::ClosedTab>,
    },
    ProductAnnouncementDismiss {
        version: String,
        id: String,
    },
    ReleaseNotesDismiss,
    PopupCommand,
    ReloadConfig,
    UsageRead {
        endpoint_id: ClientEndpointId,
    },
    NotificationList {
        endpoint_id: ClientEndpointId,
    },
    GitBranchList {
        workspace_id: String,
    },
    AgentKindList {
        workspace_id: String,
    },
    IntegrationList,
    IntegrationInstall,
    UsageSettings {
        endpoint_id: ClientEndpointId,
    },
    PrepareWorktreeCreate {
        workspace_id: String,
    },
    PrepareWorktreeOpen {
        workspace_id: String,
    },
    PrepareWorktreeRemove {
        workspace_id: String,
    },
    WorktreeCreate,
    WorktreeOpen,
    WorktreeRemove {
        forced: bool,
    },
    SelectionCopy,
    PaneScroll {
        pane_id: String,
        serial: u64,
    },
    WordSelection {
        pane_id: String,
        absolute_row: u32,
        generation: u64,
    },
    PaneLinkResolve {
        target: super::link_hover::LinkHoverTarget,
    },
    PaneLinkActivate {
        pane_id: String,
        inner_rect: Rect,
        fallback_events: Vec<crossterm::event::MouseEvent>,
    },
    CopyMotion {
        pane_id: String,
        origin: crate::api::schema::PaneTextPoint,
        session_generation: u64,
    },
    CopySearch {
        pane_id: String,
        origin: crate::api::schema::PaneTextPoint,
        query: String,
        direction: crate::api::schema::PaneCopySearchDirection,
        repeat: bool,
        generation: u64,
        session_generation: u64,
    },
}

pub(super) struct PendingEndpointRequest {
    pub(super) boot_id: String,
    pub(super) method_name: String,
    pub(super) confirmation_workspace_id: Option<String>,
    pub(super) kind: PendingEndpointKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ClientEndpointNoticeKind {
    Unsupported,
    Rejected,
    Timeout,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ClientEndpointNoticeKey {
    pub(super) boot_id: String,
    pub(super) kind: ClientEndpointNoticeKind,
    pub(super) code: String,
}

pub(super) struct ClientVisibleEndpointNotice {
    pub(super) key: ClientEndpointNoticeKey,
    pub(super) title: String,
    pub(super) body: String,
    pub(super) deadline: std::time::Instant,
}

pub(crate) struct ClientShellEndpointError {
    pub code: Option<String>,
    pub message: String,
}

pub(crate) enum ClientShellNotificationEffect {
    Sound {
        sound: crate::sound::Sound,
        agent: Option<String>,
    },
    Terminal {
        title: String,
        body: Option<String>,
    },
    System {
        title: String,
        subtitle: Option<String>,
        body: Option<String>,
        /// Pane to focus when the notification is clicked; only set for the
        /// local endpoint, whose API socket this client process can reach.
        click_target: Option<ClientNotificationClickTarget>,
        #[cfg(windows)]
        target: Option<ClientSystemNotificationTarget>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientNotificationClickTarget {
    pub(crate) pane_id: String,
    pub(crate) tab_id: Option<String>,
    /// Last resort once the pane and its tab are gone (e.g. a finished job tab).
    pub(crate) workspace_id: Option<String>,
}

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientSystemNotificationTarget {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) boot_id: String,
    pub(crate) pane_id: String,
}

pub(super) struct ClientPendingNotification {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
    pub(super) expires_at: std::time::Instant,
    pub(super) validate_state: bool,
}

pub(super) struct ClientVisibleNotification {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) event: SemanticNotification,
    pub(super) deadline: std::time::Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientInputTarget {
    Pane(String),
    Popup(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientInputContext {
    pub(super) mode: ClientShellMode,
    pub(super) overlay: Option<ClientShellOverlayKind>,
    pub(super) popup_terminal_id: Option<String>,
    pub(super) popup_pending: bool,
    pub(super) retained_selection: bool,
}

type ClientInputLeases = crate::input::InputLeaseTable<u8, ClientInputContext, ClientInputTarget>;

#[derive(Clone, Debug)]
pub(super) struct ClientPaneClick {
    pub(super) pane_id: String,
    pub(super) viewport_row: u16,
    pub(super) col: u16,
    pub(super) at: std::time::Instant,
}

impl ClientPaneClick {
    pub(super) fn is_double_click_for(&self, next: &Self) -> bool {
        self.pane_id == next.pane_id
            && next.at.duration_since(self.at) <= std::time::Duration::from_millis(350)
            && self.viewport_row.abs_diff(next.viewport_row) <= 1
            && self.col.abs_diff(next.col) <= 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientSelectionAutoscrollDirection {
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub(super) struct ClientSelectionAutoscroll {
    pub(super) pane_id: String,
    pub(super) direction: ClientSelectionAutoscrollDirection,
    pub(super) last_mouse_column: u16,
    pub(super) last_mouse_row: u16,
    pub(super) inner_rect: Rect,
    pub(super) offset_from_bottom: usize,
    pub(super) max_offset_from_bottom: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClientCopySelection {
    Character {
        anchor: crate::api::schema::PaneTextPoint,
    },
    Linewise {
        anchor_row: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientCopySearchPrompt {
    pub(super) direction: crate::api::schema::PaneCopySearchDirection,
    pub(super) query: TextEditor,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientCopyOperation {
    Motion(crate::api::schema::PaneCopyMotion),
    Search {
        query: String,
        direction: crate::api::schema::PaneCopySearchDirection,
        repeat: bool,
    },
}

pub(super) struct ClientCopySearchResult {
    pub(super) content_revision: u64,
    pub(super) matches: Vec<crate::api::schema::PaneTextRange>,
    pub(super) total: u64,
    pub(super) current: Option<usize>,
    pub(super) current_global: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClientCopyModeState {
    pub(super) pane_id: String,
    pub(super) content_revision: u64,
    pub(super) geometry: (u16, u16),
    pub(super) alternate_screen_active: bool,
    pub(super) cursor: crate::api::schema::PaneTextPoint,
    pub(super) offset_from_bottom: usize,
    pub(super) max_offset_from_bottom: usize,
    pub(super) entry_offset_from_bottom: usize,
    pub(super) selection: Option<ClientCopySelection>,
    pub(super) search_prompt: Option<ClientCopySearchPrompt>,
    pub(super) search_query: String,
    pub(super) search_direction: Option<crate::api::schema::PaneCopySearchDirection>,
    pub(super) search_matches: Vec<crate::api::schema::PaneTextRange>,
    pub(super) search_total: u64,
    pub(super) search_current: Option<usize>,
    pub(super) search_current_global: Option<u64>,
    pub(super) search_generation: u64,
    pub(super) copy_after_search: bool,
}

pub(crate) struct ClientShellState {
    pub(super) machine_diagnostics: super::machine_diagnostics::MachineDiagnostics,
    pub(super) config: ClientShellConfig,
    pub(super) snapshot: Option<Box<ClientShellSnapshot>>,
    pub(super) active_snapshot_generation: Option<u64>,
    pub(super) pane_surface_generation: Option<u64>,
    pub(super) pane_surface: Option<PaneSurfaceFrame>,
    /// A future projection surface waits here until its matching snapshot arrives. The visible
    /// pane surface always remains an exact snapshot pair.
    pub(super) pending_pane_surface: Option<PaneSurfaceFrame>,
    pub(super) graphics: crate::kitty_graphics::surface::ClientState,
    pub(super) graphics_cell_size: crate::kitty_graphics::HostCellSize,
    pub(super) popup_terminal_id: Option<String>,
    pub(super) sidebar_collapsed: bool,
    pub(super) sidebar_collapsed_manual: bool,
    pub(super) sidebar_width: u16,
    pub(super) sidebar_width_manual: bool,
    pub(super) sidebar_section_split: f32,
    pub(super) sidebar_section_split_manual: bool,
    pub(super) agent_panel_sort_manual: bool,
    /// How the spaces list is sorted; a client preference.
    pub(super) space_sort: super::space_sort::SpaceSort,
    pub(super) last_sidebar_divider_click: Option<std::time::Instant>,
    pub(super) chrome_drag: Option<ClientChromeDrag>,
    pub(super) workspace_press: Option<ClientWorkspacePress>,
    /// Space of the active endpoint under the pointer that can be dragged,
    /// so its name line shows a grip.
    pub(super) hovered_workspace_id: Option<String>,
    /// Nested tab whose square under a tab line is under the pointer.
    pub(super) hovered_square: Option<String>,
    /// The name-line button the pointer is over, and its space.
    pub(super) hovered_name_button: Option<(String, NameLineButton)>,
    /// The tab line whose job summary (its fold toggle) the pointer is over.
    pub(super) hovered_fold: Option<String>,
    pub(super) tooltip: Option<super::tooltip::Tooltip>,
    pub(super) tab_press: Option<ClientTabPress>,
    /// The spaces filter bar (client-only).
    pub(super) space_filter: super::space_filter::SpaceFilter,
    /// Tabs this client closed, newest last (see `closed_tabs`).
    pub(super) closed_tabs: std::collections::VecDeque<super::closed_tabs::ClosedTab>,
    /// When the animated glyphs started together (see `ui::motion`).
    pub(super) motion_epoch: std::time::Instant,
    /// The frames the glyphs are drawn with.
    pub(super) motion: crate::ui::motion::Motion,
    /// Whether the last frame had a glyph that turns; the timer runs only then.
    pub(super) motion_active: bool,
    /// Last focused tab of each tab group, by endpoint and the group's
    /// top-level tab. Kept by this client, so one client's navigation never
    /// moves another's.
    pub(super) last_group_tabs: HashMap<(ClientEndpointId, String), String>,
    pub(super) collapsed_groups: HashSet<String>,
    pub(super) remote_collapsed_groups: HashMap<ClientEndpointId, HashSet<String>>,
    /// Tabs whose nested tabs are unfolded as squares under their tab line,
    /// by endpoint; folded by default and not saved.
    pub(super) unfolded_squares: HashMap<ClientEndpointId, HashSet<String>>,
    /// The squares' order as last drawn; held while the pointer is over the
    /// spaces list, so a job tab that closes leaves a blank slot instead of
    /// moving the others.
    pub(super) held_squares: super::space_tabs::HeldSquares,
    /// The agent each space's launch button last started, by workspace id;
    /// client memory only (after a restart the newest tab's agent decides).
    pub(super) launched_agents: HashMap<String, String>,
    /// Each parent tab's last focused job per endpoint, kept under its folded
    /// line after the focus returns to the parent. Client memory only.
    pub(super) kept_jobs: HashMap<ClientEndpointId, super::space_tabs::KeptJobs>,
    /// The focused tab the last snapshot had, by endpoint: a job is pinned
    /// only when the focus moves to it, so unfolding unpins it for good.
    pub(super) last_focused_tab: HashMap<ClientEndpointId, String>,
    /// The tabs the header's fold button put away, by endpoint: the quiet
    /// ones at the press. A tab leaves for good once it is no longer quiet,
    /// so nothing folds by itself. Client memory only.
    pub(super) quiet_folds: HashMap<ClientEndpointId, HashSet<String>>,
    /// Back and forward over this client's focus jumps, by endpoint.
    pub(super) focus_history: HashMap<ClientEndpointId, super::focus_history::FocusHistory>,
    pub(super) pointer_over_spaces: bool,
    /// The sorted spaces' order as last drawn, held while the pointer is
    /// over the list so a re-sort cannot move a space under it.
    pub(super) held_space_order: Vec<String>,
    /// What the last `⤒` click moved, to put it back.
    pub(super) space_to_top_undo: Option<super::space_sort::SpaceToTopUndo>,
    /// When the last image attached to each pane was taken.
    pub(super) image_attach_times: super::image_picker::ImageAttachTimes,
    /// The image list may show the highlighted image as a kitty image.
    pub(super) image_previews: bool,
    /// The file sent to the host terminal as the image list's preview.
    pub(super) image_preview_sent: Option<std::path::PathBuf>,
    pub(super) workspace_scroll: usize,
    pub(super) workspace_scroll_anchor: Option<ScrollAnchor>,
    pub(super) agent_scroll: usize,
    pub(super) pending_agent_reveal: Option<(ClientEndpointId, String)>,
    pub(super) tab_scroll: usize,
    pub(super) mobile_switcher_scroll: usize,
    pub(super) reveal_focused_workspace: bool,
    /// A tab line that was just unfolded: the spaces list scrolls to show its
    /// squares.
    pub(super) reveal_unfolded_tab: Option<String>,
    pub(super) reveal_mobile_workspace: bool,
    pub(super) mobile_switcher_suspended: bool,
    pub(super) reveal_focused_tab: bool,
    pub(super) last_tab_bar_width: Option<u16>,
    pub(super) last_composed_size: Option<(u16, u16)>,
    pub(super) last_composed_at: Option<std::time::Instant>,
    pub(super) selection_repaint_deadline: Option<std::time::Instant>,
    pub(super) hits: ShellHitMap,
    pub(super) endpoints: Vec<ClientShellEndpoint>,
    pub(super) active_endpoint_id: ClientEndpointId,
    pub(super) collapsed_endpoints: HashSet<ClientEndpointId>,
    pub(super) mode: ClientShellMode,
    pub(super) navigate_workspace_id: Option<WorkspaceNavigationTarget>,
    pub(super) pending_workspace_highlight: Option<PendingWorkspaceHighlight>,
    pub(super) reveal_navigation_workspace: bool,
    pub(super) overlay: Option<ClientShellOverlay>,
    pub(super) previous_pane_id: Option<String>,
    pub(super) pane_mouse_gesture: Option<ClientPaneMouseGesture>,
    pub(super) link_hover: Option<super::link_hover::LinkHover>,
    pub(super) url_click_consumes_until_up: bool,
    /// The host terminal reports key releases (Kitty event types), so text
    /// presses can be tracked until their release arrives.
    pub(super) host_reports_key_releases: bool,
    /// The host tty's erase character is `^H`: a raw 0x08 is Backspace, not
    /// Ctrl+H (MobaXterm, PuTTY-style terminals; tmux reads VERASE the same way).
    pub(super) host_erase_is_ctrl_h: bool,
    /// The focused pane asks for every key as an escape code, so Herdr pushed
    /// report-all to the host; plain text input then reaches it as text.
    pub(super) host_reports_all_keys: bool,
    pub(super) replaying_url_click: bool,
    pub(super) selection: Option<crate::selection::Selection<String>>,
    pub(super) last_pane_click: Option<ClientPaneClick>,
    pub(super) selection_autoscroll: Option<ClientSelectionAutoscroll>,
    pub(super) selection_autoscroll_deadline: Option<std::time::Instant>,
    /// A dragged space at the list's edge: direction (-1 up, 1 down), the
    /// pointer, and when the list scrolls next.
    pub(super) space_drag_autoscroll: Option<(i8, (u16, u16), std::time::Instant)>,
    pub(super) selection_highlight_clear_deadline: Option<std::time::Instant>,
    pub(super) word_selection_gesture: Option<ClientWordSelection>,
    pub(super) word_selection_generation: u64,
    pub(super) copy_mode: Option<ClientCopyModeState>,
    pub(super) copy_session_generation: u64,
    pub(super) copy_operation_in_flight: bool,
    pub(super) copy_operation_queue: VecDeque<ClientCopyOperation>,
    pub(super) copy_input_queue: VecDeque<crate::input::TerminalKey>,
    pub(super) next_scroll_serial: u64,
    pub(super) pane_scroll_in_flight: HashMap<String, u64>,
    pub(super) pane_scroll_queued: HashMap<String, usize>,
    pub(super) pane_scroll_targets: HashMap<String, usize>,
    pub(super) copy_feedback: Option<crate::app::state::CopyFeedback>,
    pub(super) copy_feedback_deadline: Option<std::time::Instant>,
    pub(super) usage: super::usage::ClientUsageState,
    pub(super) notification_log: super::notification_log::NotificationLog,
    pub(super) host_mouse_pixels: Option<crate::input::mouse::HostPixels>,
    pub(super) input_leases: ClientInputLeases,
    pub(super) popup_pending: bool,
    pub(super) popup_pending_deadline: Option<std::time::Instant>,
    /// The pending popup is consult stats: a read-only view a click outside may close.
    pub(super) popup_pending_dismissable: bool,
    /// Terminal id of the open popup when a click outside closes it.
    pub(super) dismissable_popup_id: Option<String>,
    pub(super) next_request_id: u64,
    pub(super) pending_requests: HashMap<String, PendingEndpointRequest>,
    pub(super) pending_integration_installs: usize,
    pub(super) pending_notifications: Vec<ClientPendingNotification>,
    pub(super) visible_notification: Option<ClientVisibleNotification>,
    pub(super) queued_notifications: VecDeque<ClientVisibleNotification>,
    pub(super) endpoint_notice_seen: HashSet<ClientEndpointNoticeKey>,
    pub(super) visible_endpoint_notice: Option<ClientVisibleEndpointNotice>,
    pub(super) outer_focused: Option<bool>,
    pub(super) ascii_input_source_active: bool,
    pub(super) pending_input_source_changes: Vec<bool>,
    pub(super) host_appearance: Option<crate::terminal_theme::HostAppearance>,
    pub(super) host_appearance_explicit: bool,
    pub(super) host_background: Option<crate::terminal_theme::RgbColor>,
    pub(super) local_config_diagnostic: Option<String>,
    pub(super) config_diagnostic: Option<String>,
    pub(super) endpoint_error: Option<String>,
    pub(super) endpoint_error_deadline: Option<std::time::Instant>,
    pub(super) dismissed_product_announcement: Option<(String, String)>,
}

pub(super) fn product_announcement_state(
    announcement: &crate::protocol::ClientShellProductAnnouncement,
) -> crate::app::state::ProductAnnouncementState {
    crate::app::state::ProductAnnouncementState {
        version: announcement.version.clone(),
        id: announcement.id.clone(),
        title: announcement.title.clone(),
        body: announcement.body.clone(),
        scroll: 0,
        preview: announcement.preview,
    }
}

pub(super) fn release_notes_state(
    notes: &crate::protocol::ClientShellReleaseNotes,
) -> crate::app::state::ReleaseNotesState {
    crate::app::state::ReleaseNotesState {
        version: notes.version.clone(),
        body: notes.body.clone(),
        scroll: 0,
        preview: notes.preview,
    }
}

#[derive(Clone, Copy)]
pub(super) struct WorkspaceEntry {
    pub(super) index: usize,
    pub(super) indented: bool,
    pub(super) last_child: bool,
}

impl ClientShellState {
    pub(crate) fn new(mut config: ClientShellConfig) -> Self {
        let preferences = config.preferences.clone();
        let local_config_diagnostic = config.startup_config_diagnostic.take();
        let overlay = config
            .startup_onboarding
            .then_some(ClientShellOverlay::Onboarding);
        let sidebar_collapsed = preferences
            .sidebar_collapsed
            .unwrap_or(config.sidebar_start_collapsed);
        let (min_width, max_width) = crate::config::validated_sidebar_bounds(
            config.sidebar_min_width,
            config.sidebar_max_width,
        )
        .unwrap_or((18, 36));
        let sidebar_width = preferences
            .sidebar_width
            .unwrap_or(config.sidebar_width)
            .clamp(min_width, max_width);
        let sidebar_section_split = preferences
            .sidebar_section_split
            .filter(|split| split.is_finite())
            .map(|split| split.clamp(0.1, 0.9))
            .unwrap_or(0.5);
        if let Some(sort) = preferences.agent_panel_sort {
            config.agent_panel_sort = sort;
        }
        let mut remote_collapsed_groups = HashMap::<ClientEndpointId, HashSet<String>>::new();
        for saved in preferences.remote_collapsed_groups {
            let Ok(profile_id) = crate::client::endpoint::ProfileId::parse(saved.profile_id) else {
                continue;
            };
            remote_collapsed_groups
                .entry(ClientEndpointId::Ssh(profile_id))
                .or_default()
                .extend(saved.collapsed_groups);
        }
        Self {
            machine_diagnostics: Default::default(),
            config,
            snapshot: None,
            active_snapshot_generation: None,
            pane_surface_generation: None,
            pane_surface: None,
            pending_pane_surface: None,
            graphics: crate::kitty_graphics::surface::ClientState::new(),
            graphics_cell_size: crate::kitty_graphics::HostCellSize {
                width_px: 1,
                height_px: 1,
            },
            popup_terminal_id: None,
            sidebar_collapsed,
            sidebar_collapsed_manual: preferences.sidebar_collapsed.is_some(),
            sidebar_width,
            sidebar_width_manual: preferences.sidebar_width.is_some(),
            sidebar_section_split,
            sidebar_section_split_manual: preferences.sidebar_section_split.is_some(),
            agent_panel_sort_manual: preferences.agent_panel_sort.is_some(),
            space_sort: preferences.space_sort.unwrap_or_default(),
            last_sidebar_divider_click: None,
            chrome_drag: None,
            workspace_press: None,
            hovered_workspace_id: None,
            hovered_square: None,
            hovered_name_button: None,
            hovered_fold: None,
            tooltip: None,
            tab_press: None,
            space_filter: Default::default(),
            closed_tabs: Default::default(),
            motion_epoch: std::time::Instant::now(),
            motion: crate::ui::motion::Motion::at(std::time::Duration::ZERO),
            motion_active: false,
            last_group_tabs: HashMap::new(),
            collapsed_groups: preferences.collapsed_groups.into_iter().collect(),
            remote_collapsed_groups,
            unfolded_squares: [(
                ClientEndpointId::Local,
                preferences.unfolded_job_tabs.into_iter().collect(),
            )]
            .into_iter()
            .collect(),
            held_squares: HashMap::new(),
            launched_agents: HashMap::new(),
            kept_jobs: [(
                ClientEndpointId::Local,
                preferences
                    .kept_jobs
                    .into_iter()
                    .map(|kept| (kept.parent_tab_id, kept.job_tab_id))
                    .collect(),
            )]
            .into_iter()
            .collect(),
            last_focused_tab: HashMap::new(),
            quiet_folds: HashMap::new(),
            focus_history: HashMap::new(),
            pointer_over_spaces: false,
            held_space_order: Vec::new(),
            workspace_scroll: 0,
            workspace_scroll_anchor: None,
            space_to_top_undo: None,
            image_attach_times: Default::default(),
            image_previews: false,
            image_preview_sent: None,
            agent_scroll: 0,
            pending_agent_reveal: None,
            tab_scroll: 0,
            mobile_switcher_scroll: 0,
            reveal_focused_workspace: true,
            reveal_unfolded_tab: None,
            reveal_mobile_workspace: false,
            mobile_switcher_suspended: false,
            reveal_focused_tab: true,
            last_tab_bar_width: None,
            last_composed_size: None,
            last_composed_at: None,
            selection_repaint_deadline: None,
            hits: ShellHitMap::default(),
            endpoints: vec![local_endpoint()],
            active_endpoint_id: ClientEndpointId::Local,
            collapsed_endpoints: HashSet::new(),
            mode: ClientShellMode::Terminal,
            navigate_workspace_id: None,
            pending_workspace_highlight: None,
            reveal_navigation_workspace: false,
            overlay,
            previous_pane_id: None,
            pane_mouse_gesture: None,
            link_hover: None,
            url_click_consumes_until_up: false,
            host_reports_key_releases: false,
            host_erase_is_ctrl_h: false,
            host_reports_all_keys: false,
            replaying_url_click: false,
            selection: None,
            last_pane_click: None,
            selection_autoscroll: None,
            selection_autoscroll_deadline: None,
            space_drag_autoscroll: None,
            selection_highlight_clear_deadline: None,
            word_selection_gesture: None,
            word_selection_generation: 0,
            copy_mode: None,
            copy_session_generation: 0,
            copy_operation_in_flight: false,
            copy_operation_queue: VecDeque::new(),
            copy_input_queue: VecDeque::new(),
            next_scroll_serial: 0,
            pane_scroll_in_flight: HashMap::new(),
            pane_scroll_queued: HashMap::new(),
            pane_scroll_targets: HashMap::new(),
            copy_feedback: None,
            copy_feedback_deadline: None,
            usage: super::usage::ClientUsageState::default(),
            notification_log: Default::default(),
            host_mouse_pixels: None,
            input_leases: ClientInputLeases::default(),
            popup_pending: false,
            popup_pending_deadline: None,
            popup_pending_dismissable: false,
            dismissable_popup_id: None,
            next_request_id: 1,
            pending_requests: HashMap::new(),
            pending_integration_installs: 0,
            pending_notifications: Vec::new(),
            visible_notification: None,
            queued_notifications: VecDeque::new(),
            endpoint_notice_seen: HashSet::new(),
            visible_endpoint_notice: None,
            outer_focused: None,
            ascii_input_source_active: false,
            pending_input_source_changes: Vec::new(),
            host_appearance: None,
            host_appearance_explicit: false,
            host_background: None,
            config_diagnostic: local_config_diagnostic.clone(),
            local_config_diagnostic,
            endpoint_error: None,
            endpoint_error_deadline: None,
            dismissed_product_announcement: None,
        }
    }

    pub(super) fn resume_mobile_switcher_if_ready(&mut self) -> bool {
        if !self.mobile_switcher_suspended || self.overlay.is_some() {
            return false;
        }
        self.mobile_switcher_suspended = false;
        if self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.focused_workspace_id.as_ref())
            .is_some()
        {
            self.mode = self.copy_or_terminal_mode();
            self.navigate_workspace_id = None;
        } else {
            self.mode = ClientShellMode::Navigate;
        }
        true
    }

    pub(super) fn mobile_layout_active(&self) -> bool {
        self.last_composed_size
            .is_some_and(|(cols, rows)| !self.layout(cols, rows).mobile_header.is_empty())
    }

    pub(super) fn collapsed_groups_for_endpoint(
        &self,
        endpoint_id: &ClientEndpointId,
    ) -> Option<&HashSet<String>> {
        if endpoint_id.is_local() {
            Some(&self.collapsed_groups)
        } else {
            self.remote_collapsed_groups.get(endpoint_id)
        }
    }

    pub(super) fn group_is_collapsed(&self, endpoint_id: &ClientEndpointId, key: &str) -> bool {
        self.collapsed_groups_for_endpoint(endpoint_id)
            .is_some_and(|groups| groups.contains(key))
    }

    /// Opens a collapsed group or space; whether it was collapsed.
    pub(super) fn expand_collapsed_group(
        &mut self,
        endpoint_id: &ClientEndpointId,
        key: &str,
    ) -> bool {
        let groups = if endpoint_id.is_local() {
            &mut self.collapsed_groups
        } else {
            match self.remote_collapsed_groups.get_mut(endpoint_id) {
                Some(groups) => groups,
                None => return false,
            }
        };
        groups.remove(key)
    }

    pub(super) fn toggle_collapsed_group(&mut self, endpoint_id: &ClientEndpointId, key: String) {
        let groups = if endpoint_id.is_local() {
            &mut self.collapsed_groups
        } else {
            self.remote_collapsed_groups
                .entry(endpoint_id.clone())
                .or_default()
        };
        if !groups.remove(&key) {
            groups.insert(key);
        }
    }

    pub(super) fn navigation_workspace_entries(
        &self,
        snapshot: &ClientShellSnapshot,
    ) -> Vec<WorkspaceEntry> {
        let empty_collapsed_groups = HashSet::new();
        if self.mobile_layout_active() {
            render::workspace_entries(snapshot, &empty_collapsed_groups)
        } else {
            let collapsed_groups = self
                .collapsed_groups_for_endpoint(&self.active_endpoint_id)
                .unwrap_or(&empty_collapsed_groups);
            self.sorted_for_sidebar(
                snapshot,
                render::workspace_entries(snapshot, collapsed_groups),
                collapsed_groups,
            )
        }
    }

    /// `entries` in the order the sidebar shows them: the sort applies to the
    /// single-machine sidebar; the multi-machine one keeps the manual order.
    pub(super) fn sorted_for_sidebar(
        &self,
        snapshot: &ClientShellSnapshot,
        entries: Vec<WorkspaceEntry>,
        collapsed_groups: &HashSet<String>,
    ) -> Vec<WorkspaceEntry> {
        if self.endpoints.len() > 1 {
            return entries;
        }
        let sorted =
            super::space_sort::sorted_entries(snapshot, entries, collapsed_groups, self.space_sort);
        match self.held_space_order() {
            Some(held) => super::space_sort::held_entries(snapshot, sorted, held),
            None => sorted,
        }
    }

    /// The order a sorted list holds while the pointer is over it.
    pub(super) fn held_space_order(&self) -> Option<&[String]> {
        (self.pointer_over_spaces
            && !self.space_sort.allows_drag()
            && !self.held_space_order.is_empty())
        .then_some(self.held_space_order.as_slice())
    }

    pub(super) fn reveal_workspace(&mut self, workspace_id: &str) {
        if self
            .hits
            .workspaces
            .iter()
            .any(|hit| hit.workspace_id == workspace_id)
        {
            return;
        }
        let target = self.snapshot.as_deref().and_then(|snapshot| {
            self.navigation_workspace_entries(snapshot)
                .iter()
                .position(|entry| snapshot.workspaces[entry.index].workspace_id == workspace_id)
        });
        // The local sidebar scrolls by rows: bring the space's name row in.
        if let Some(layout) = self
            .hits
            .workspace_layout
            .iter()
            .find(|layout| layout.workspace_id == workspace_id)
        {
            let body = self.hits.workspace_body;
            let row = (layout.top - i32::from(body.y) + self.workspace_scroll as i32).max(0);
            self.workspace_scroll = super::scroll::rows_start_to_reveal(
                self.workspace_scroll,
                usize::from(body.height),
                row as usize,
                row as usize,
            )
            .min(self.hits.workspace_max_scroll);
            return;
        }
        // The multi-machine sidebar knows no rows here: it reveals the space
        // once the focus arrives.
        if target.is_some() {
            self.reveal_focused_workspace = true;
        }
    }

    pub(super) fn layout(&self, cols: u16, rows: u16) -> ClientShellLayout {
        let mut layout = self.config.layout(
            cols,
            rows,
            self.sidebar_collapsed,
            self.focused_tab_count(),
            self.sidebar_width,
            self.snapshot
                .as_deref()
                .is_some_and(super::tab_groups::workspace_has_child_tabs),
        );
        if self.active_job_metadata().is_some()
            && layout.pane_surface.height >= 2
            && layout.pane_surface.width >= 6
        {
            layout.pane_surface.height -= 1;
            layout.job_footer = Rect::new(
                layout.pane_surface.x,
                layout.pane_surface.bottom(),
                layout.pane_surface.width,
                1,
            );
        }
        layout
    }

    pub(crate) fn surface_size(&self, cols: u16, rows: u16) -> ClientSurfaceSize {
        let surface = self.layout(cols, rows).pane_surface;
        ClientSurfaceSize {
            cols: surface.width.max(1),
            rows: surface.height.max(1),
        }
    }

    pub(super) fn reset_endpoint_projection(&mut self) {
        self.hits = ShellHitMap::default();
        self.pane_surface = None;
        self.pending_pane_surface = None;
        self.input_leases = ClientInputLeases::default();
        self.popup_terminal_id = None;
        self.chrome_drag = None;
        self.workspace_press = None;
        self.hovered_workspace_id = None;
        self.hovered_square = None;
        self.hovered_name_button = None;
        self.hovered_fold = None;
        self.tab_press = None;
        self.workspace_scroll = 0;
        self.agent_scroll = 0;
        self.tab_scroll = 0;
        self.mobile_switcher_scroll = 0;
        self.reveal_focused_workspace = true;
        self.reveal_mobile_workspace = false;
        self.mobile_switcher_suspended = false;
        self.reveal_focused_tab = true;
        self.last_tab_bar_width = None;
        self.last_composed_size = None;
        self.last_composed_at = None;
        self.selection_repaint_deadline = None;
        self.pending_requests.clear();
        self.pane_scroll_in_flight.clear();
        self.pane_scroll_queued.clear();
        self.pane_scroll_targets.clear();
        self.popup_pending = false;
        self.popup_pending_deadline = None;
        self.popup_pending_dismissable = false;
        self.dismissable_popup_id = None;
        self.pending_integration_installs = 0;
        self.endpoint_notice_seen.clear();
        self.visible_endpoint_notice = None;
        self.endpoint_error = None;
        self.endpoint_error_deadline = None;
        self.navigate_workspace_id = None;
        self.pending_workspace_highlight = None;
        self.overlay = self
            .config
            .startup_onboarding
            .then_some(ClientShellOverlay::Onboarding);
        self.previous_pane_id = None;
        self.pane_mouse_gesture = None;
        self.link_hover = None;
        self.url_click_consumes_until_up = false;
        self.replaying_url_click = false;
        self.selection = None;
        self.last_pane_click = None;
        self.selection_autoscroll = None;
        self.selection_autoscroll_deadline = None;
        self.selection_highlight_clear_deadline = None;
        self.word_selection_gesture = None;
        self.copy_mode = None;
        if self.mode == ClientShellMode::Copy {
            self.mode = ClientShellMode::Terminal;
        }
        self.reset_copy_pipeline();
        self.copy_feedback = None;
        self.copy_feedback_deadline = None;
        self.host_mouse_pixels = None;
        self.dismissed_product_announcement = None;
    }

    pub(super) fn apply_active_snapshot(
        &mut self,
        mut snapshot: Box<ClientShellSnapshot>,
        generation: Option<u64>,
    ) {
        snapshot
            .commands
            .retain(|command| command.action != crate::protocol::ClientShellCommandAction::Unknown);
        let graphics_scope = match &self.active_endpoint_id {
            // Local direct uploads use image IDs authored by the server from its boot ID.
            ClientEndpointId::Local => snapshot.boot_id.clone(),
            endpoint_id => format!("{}:{}", endpoint_id.storage_key(), snapshot.boot_id),
        };
        let endpoint_boot_changed =
            self.snapshot.is_some() && self.graphics.scope() != graphics_scope;
        let generation_changed = self.active_snapshot_generation != generation;
        if !endpoint_boot_changed
            && !generation_changed
            && self.snapshot.as_ref().is_some_and(|current| {
                current.boot_id == snapshot.boot_id && snapshot.revision < current.revision
            })
        {
            return;
        }
        // Screen revisions restart per connection. Keep the displayed surface for selection
        // content comparisons, but retire speculative frames from the old connection.
        if generation_changed {
            self.pending_pane_surface = None;
        }
        self.active_snapshot_generation = generation;
        self.graphics.set_scope(&graphics_scope);
        let command_bindings_changed = self.snapshot.as_ref().is_none_or(|current| {
            current.commands.len() != snapshot.commands.len()
                || current
                    .commands
                    .iter()
                    .zip(&snapshot.commands)
                    .any(|(left, right)| {
                        left.binding_labels != right.binding_labels || left.action != right.action
                    })
        });
        let endpoint_profile_changed = self.snapshot.as_ref().is_none_or(|current| {
            current.server_keybindings_toml != snapshot.server_keybindings_toml
        });
        let snapshot_keybindings_changed = match self.config.keybinding_source {
            ClientShellKeybindingSource::Local => self
                .snapshot
                .as_ref()
                .is_none_or(|current| current.commands != snapshot.commands),
            ClientShellKeybindingSource::Endpoint => {
                endpoint_profile_changed
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|current| current.commands != snapshot.commands)
            }
            ClientShellKeybindingSource::RemoteLocal => false,
        };
        let active_keymap_changed = match self.config.keybinding_source {
            ClientShellKeybindingSource::Local => command_bindings_changed,
            ClientShellKeybindingSource::Endpoint => {
                endpoint_profile_changed || command_bindings_changed
            }
            ClientShellKeybindingSource::RemoteLocal => false,
        };
        self.config_diagnostic = super::config::merged_config_diagnostic(
            self.local_config_diagnostic.as_deref(),
            snapshot.config_diagnostic.as_deref(),
        );
        let boot_changed = endpoint_boot_changed
            || self
                .snapshot
                .as_ref()
                .is_some_and(|current| current.boot_id != snapshot.boot_id);
        if boot_changed
            || self
                .pane_surface
                .as_ref()
                .is_none_or(|surface| surface.projection_revision != snapshot.revision)
        {
            self.hits = ShellHitMap::default();
        }
        if boot_changed {
            // A reboot must not turn Enter on a stale preview into focus on a reused ID.
            let preview = (self.mode == ClientShellMode::Navigate)
                .then(|| self.navigate_workspace_id.take())
                .flatten();
            self.reset_endpoint_projection();
            self.navigate_workspace_id = preview;
        } else if let Some(previous) = self
            .snapshot
            .as_deref()
            .and_then(|current| current.focused_pane_id.as_ref())
            .filter(|previous| Some(previous.as_str()) != snapshot.focused_pane_id.as_deref())
        {
            self.previous_pane_id = Some(previous.clone());
        }
        if snapshot_keybindings_changed {
            if let Err(err) = self.config.apply_snapshot_keybindings(
                snapshot.server_keybindings_toml.as_deref(),
                &snapshot.commands,
            ) {
                self.set_endpoint_error(err);
            } else if active_keymap_changed
                && matches!(
                    self.mode,
                    ClientShellMode::Prefix | ClientShellMode::Navigate | ClientShellMode::Resize
                )
            {
                self.mode = ClientShellMode::Terminal;
            }
        }
        let tab_layout_changed = self.snapshot.as_deref().is_none_or(|current| {
            current.tabs.len() != snapshot.tabs.len()
                || current
                    .tabs
                    .iter()
                    .zip(&snapshot.tabs)
                    .any(|(left, right)| {
                        left.tab_id != right.tab_id
                            || left.workspace_id != right.workspace_id
                            || left.label != right.label
                            || left.zoomed != right.zoomed
                            || left.parent_tab_id != right.parent_tab_id
                            || left.status != right.status
                    })
                || render::tab_bar_status_width(current) != render::tab_bar_status_width(&snapshot)
        });
        // A new focused space, or a new focused tab in it (a tab just
        // created far down a tall space), scrolls the list to it.
        if self.snapshot.as_deref().is_some_and(|current| {
            current.focused_workspace_id != snapshot.focused_workspace_id
                || current.focused_tab_id != snapshot.focused_tab_id
        }) || self.snapshot.is_none()
        {
            self.reveal_focused_workspace = true;
        }
        if tab_layout_changed
            || self
                .snapshot
                .as_deref()
                .and_then(|current| current.focused_tab_id.as_deref())
                != snapshot.focused_tab_id.as_deref()
        {
            self.reveal_focused_tab = true;
        }
        let selection_focus_lost = if let Some(gesture) = self.word_selection_gesture.as_mut() {
            let focused_pane = snapshot.focused_pane_id.as_deref();
            // Remember confirmed focus across intermediate snapshots with no
            // focused pane, without rejecting the gesture's in-flight focus request.
            gesture.focus_confirmed |= focused_pane == Some(gesture.pane_id.as_str());
            !snapshot
                .panes
                .iter()
                .any(|pane| pane.pane_id == gesture.pane_id)
                || (gesture.focus_confirmed
                    && focused_pane.is_some_and(|pane_id| pane_id != gesture.pane_id))
        } else {
            self.selection.as_ref().is_some_and(|selection| {
                snapshot.focused_pane_id.as_deref() != Some(selection.pane_id.as_str())
                    || !snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == selection.pane_id)
            })
        };
        if selection_focus_lost {
            self.selection = None;
            self.selection_autoscroll = None;
            self.selection_autoscroll_deadline = None;
            self.selection_highlight_clear_deadline = None;
            self.word_selection_gesture = None;
            self.last_pane_click = None;
        }
        if let Some(copy_pane_id) = self
            .copy_mode
            .as_ref()
            .map(|copy_mode| copy_mode.pane_id.clone())
        {
            let pane_exists = snapshot
                .panes
                .iter()
                .any(|pane| pane.pane_id == copy_pane_id);
            let pane_focused = snapshot.focused_pane_id.as_deref() == Some(copy_pane_id.as_str());
            if !pane_exists {
                self.copy_mode = None;
                self.reset_copy_pipeline();
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.pane_id == copy_pane_id)
                {
                    self.selection = None;
                    self.stop_selection_autoscroll();
                    self.selection_highlight_clear_deadline = None;
                }
                if self.mode == ClientShellMode::Copy {
                    self.mode = ClientShellMode::Terminal;
                }
            } else if pane_focused {
                if self.mode == ClientShellMode::Terminal {
                    self.mode = ClientShellMode::Copy;
                }
                if self.selection.is_none() {
                    self.sync_copy_selection();
                }
            } else {
                if self
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.pane_id == copy_pane_id)
                {
                    self.selection = None;
                    self.stop_selection_autoscroll();
                    self.selection_highlight_clear_deadline = None;
                }
                if self.mode == ClientShellMode::Copy {
                    self.mode = ClientShellMode::Terminal;
                }
            }
        }
        if self.mode == ClientShellMode::Navigate && self.navigate_workspace_id.is_none() {
            self.navigate_workspace_id = snapshot
                .focused_workspace_id
                .as_deref()
                .and_then(|id| self.navigation_target(&self.active_endpoint_id, id));
            self.reveal_mobile_workspace = self.mobile_layout_active();
        }
        let pane_exists =
            |pane_id: &String| snapshot.panes.iter().any(|pane| &pane.pane_id == pane_id);
        self.pane_scroll_in_flight
            .retain(|pane_id, _| pane_exists(pane_id));
        self.pane_scroll_queued
            .retain(|pane_id, _| pane_exists(pane_id));
        self.pane_scroll_targets
            .retain(|pane_id, _| pane_exists(pane_id));

        if !self.config.startup_onboarding {
            match snapshot.product_announcement.as_ref() {
                Some(announcement) => {
                    let key = (announcement.version.clone(), announcement.id.clone());
                    let already_open = matches!(
                        self.overlay.as_ref(),
                        Some(ClientShellOverlay::ProductAnnouncement(current))
                            if current.version == announcement.version && current.id == announcement.id
                    );
                    let may_open = self.overlay.is_none()
                        || matches!(
                            self.overlay.as_ref(),
                            Some(ClientShellOverlay::ProductAnnouncement(_))
                        );
                    if self.dismissed_product_announcement.as_ref() != Some(&key)
                        && may_open
                        && !already_open
                    {
                        self.overlay = Some(ClientShellOverlay::ProductAnnouncement(
                            product_announcement_state(announcement),
                        ));
                    }
                }
                None if matches!(
                    self.overlay.as_ref(),
                    Some(ClientShellOverlay::ProductAnnouncement(_))
                ) =>
                {
                    self.overlay = None;
                    self.chrome_drag = None;
                    self.dismissed_product_announcement = None;
                }
                None => {
                    self.dismissed_product_announcement = None;
                }
            }
        }
        if let Some(ClientShellOverlay::ReleaseNotes(current)) = self.overlay.as_ref() {
            match snapshot.release_notes.as_ref() {
                Some(notes)
                    if current.version != notes.version
                        || current.body != notes.body
                        || current.preview != notes.preview =>
                {
                    self.overlay =
                        Some(ClientShellOverlay::ReleaseNotes(release_notes_state(notes)));
                    self.chrome_drag = None;
                }
                None => {
                    self.overlay = None;
                    self.chrome_drag = None;
                }
                Some(_) => {}
            }
        }
        let job_folds = self.saved_job_folds();
        self.snapshot = Some(snapshot);
        self.forget_closed_kept_jobs();
        self.remember_focused_group_tab();
        self.release_unquiet_folds();
        self.observe_focus_history();
        if self.saved_job_folds() != job_folds {
            self.persist_chrome_preferences(&mut ClientShellInput::default());
        }
        self.mark_focused_tab_notifications_read();
        self.reconcile_pending_workspace_highlight();
        let pending_surface = self.pending_pane_surface.take();
        if let Some(surface) = pending_surface {
            let matching = self.snapshot.as_ref().is_some_and(|snapshot| {
                surface.boot_id == snapshot.boot_id
                    && surface.projection_revision == snapshot.revision
            });
            if matching {
                self.install_pane_surface(surface, false);
            } else if self.snapshot.as_ref().is_some_and(|snapshot| {
                surface.boot_id == snapshot.boot_id
                    && surface.projection_revision > snapshot.revision
            }) {
                self.pending_pane_surface = Some(surface);
            }
        }
        self.resume_mobile_switcher_if_ready();
        self.reconcile_input_source();
    }

    pub(crate) fn has_presented_surface(&self) -> bool {
        self.pane_surface.is_some()
    }

    pub(crate) fn set_pane_surface(&mut self, surface: PaneSurfaceFrame) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        if surface.boot_id != snapshot.boot_id || surface.projection_revision < snapshot.revision {
            return;
        }
        if self.pane_surface.as_ref().is_some_and(|current| {
            self.pane_surface_generation == self.active_snapshot_generation
                && current.boot_id == surface.boot_id
                && (surface.projection_revision < current.projection_revision
                    || (surface.projection_revision == current.projection_revision
                        && surface.surface_revision < current.surface_revision))
        }) {
            return;
        }
        if surface.projection_revision == snapshot.revision.saturating_add(1) {
            // The next expected surface waits separately for its exact snapshot. Keeping the
            // current pair avoids treating this speculative successor as presentation evidence.
            self.pending_pane_surface = Some(surface);
            self.hits = ShellHitMap::default();
            return;
        }
        // A surface that skips one or more revisions supersedes any retained pair, but is still
        // not rendered until its matching snapshot arrives. Retain it monotonically so delayed
        // intermediate surfaces cannot replace it.
        self.install_pane_surface(surface, true);
    }

    fn install_pane_surface(&mut self, mut surface: PaneSurfaceFrame, retain_future: bool) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        if surface.boot_id != snapshot.boot_id
            || surface.projection_revision < snapshot.revision
            || (!retain_future && surface.projection_revision != snapshot.revision)
            || self.pane_surface.as_ref().is_some_and(|current| {
                self.pane_surface_generation == self.active_snapshot_generation
                    && current.boot_id == surface.boot_id
                    && (surface.projection_revision < current.projection_revision
                        || (surface.projection_revision == current.projection_revision
                            && surface.surface_revision < current.surface_revision))
            })
        {
            return;
        }
        // A retained future surface is not presentable yet. Clear hit targets immediately; the
        // exact-pair compose guard prevents it from replacing the visible frame.
        if surface.projection_revision != snapshot.revision {
            self.hits = ShellHitMap::default();
        }
        self.acknowledge_active_surface_agents(&surface);
        let previous_popup = self.popup_terminal_id.clone();
        let next_popup = surface
            .popup
            .as_deref()
            .map(|popup| popup.terminal_id.clone());
        if previous_popup != next_popup {
            if next_popup.is_some() && matches!(self.overlay, Some(ClientShellOverlay::Settings(_)))
            {
                self.cancel_settings_overlay();
            }
            if let Some(terminal_id) = previous_popup.as_ref() {
                self.input_leases
                    .remove_target(&ClientInputTarget::Popup(terminal_id.clone()));
            }
            self.mode = ClientShellMode::Terminal;
            self.navigate_workspace_id = None;
            if !matches!(
                self.overlay.as_ref(),
                Some(ClientShellOverlay::Onboarding | ClientShellOverlay::ProductAnnouncement(_))
            ) {
                self.overlay = self
                    .config
                    .startup_onboarding
                    .then_some(ClientShellOverlay::Onboarding);
            }
            self.selection = None;
            self.last_pane_click = None;
            self.selection_autoscroll = None;
            self.selection_autoscroll_deadline = None;
            self.selection_highlight_clear_deadline = None;
            self.word_selection_gesture = None;
            self.copy_mode = None;
            self.reset_copy_pipeline();
            self.chrome_drag = None;
            self.workspace_press = None;
            self.tab_press = None;
            if self.pane_mouse_gesture.as_ref().is_some_and(|gesture| {
                gesture.hit.popup && previous_popup.as_deref() == Some(gesture.hit.pane_id.as_str())
            }) {
                self.pane_mouse_gesture = None;
            }
            self.hits.popup = None;
            self.endpoint_error = None;
            self.endpoint_error_deadline = None;
        }
        if previous_popup != next_popup {
            self.dismissable_popup_id = None;
        }
        if next_popup.is_some() {
            if self.popup_pending && self.popup_pending_dismissable {
                self.dismissable_popup_id = next_popup.clone();
            }
            self.popup_pending = false;
            self.popup_pending_deadline = None;
            self.popup_pending_dismissable = false;
        }
        let selection_pane = match &self.word_selection_gesture {
            Some(gesture) => Some(&gesture.pane_id),
            None => self.selection.as_ref().map(|selection| &selection.pane_id),
        };
        let selection_invalidated = selection_pane.is_some_and(|pane_id| {
            let Some(previous_surface) = self.pane_surface.as_ref() else {
                return false;
            };
            let previous = previous_surface
                .panes
                .iter()
                .find(|pane| &pane.pane_id == pane_id);
            let next = surface.panes.iter().find(|pane| &pane.pane_id == pane_id);
            let (Some(previous), Some(next)) = (previous, next) else {
                return false;
            };
            previous.inner_rect.width != next.inner_rect.width
                || previous.inner_rect.height != next.inner_rect.height
                || previous.alternate_screen_active != next.alternate_screen_active
                // Ordinary selections are live buffer ranges. Only word gestures
                // cache content-dependent boundaries that output can invalidate.
                || (self.word_selection_gesture.is_some()
                    && previous.content_revision != next.content_revision)
        });
        if selection_invalidated {
            self.word_selection_gesture = None;
            self.selection = None;
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
        }
        for pane in &surface.panes {
            let Some(target) = self.pane_scroll_targets.get(&pane.pane_id).copied() else {
                continue;
            };
            let Some(scroll) = pane.scroll else {
                continue;
            };
            let target =
                target.min(usize::try_from(scroll.max_offset_from_bottom).unwrap_or(usize::MAX));
            if usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX) == target {
                self.pane_scroll_targets.remove(&pane.pane_id);
            }
        }
        let mut invalidated_copy_pane = None;
        if let Some(copy_mode) = self.copy_mode.as_mut() {
            if let Some(pane) = surface
                .panes
                .iter()
                .find(|pane| pane.pane_id == copy_mode.pane_id)
            {
                let geometry = (pane.inner_rect.width, pane.inner_rect.height);
                let coordinates_changed = copy_mode.geometry != geometry
                    || copy_mode.alternate_screen_active != pane.alternate_screen_active;
                if copy_mode.content_revision != pane.content_revision || coordinates_changed {
                    copy_mode.content_revision = pane.content_revision;
                    copy_mode.geometry = geometry;
                    copy_mode.alternate_screen_active = pane.alternate_screen_active;
                    if coordinates_changed {
                        copy_mode.selection = None;
                        invalidated_copy_pane = Some(copy_mode.pane_id.clone());
                    }
                    copy_mode.search_matches.clear();
                    copy_mode.search_total = 0;
                    copy_mode.search_current = None;
                    copy_mode.search_current_global = None;
                    copy_mode.search_generation = copy_mode.search_generation.saturating_add(1);
                    copy_mode.copy_after_search = false;
                }
                if let Some(scroll) = pane.scroll {
                    let actual_offset =
                        usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX);
                    if !self.pane_scroll_targets.contains_key(&pane.pane_id) {
                        copy_mode.offset_from_bottom = actual_offset;
                    }
                    copy_mode.max_offset_from_bottom =
                        usize::try_from(scroll.max_offset_from_bottom).unwrap_or(usize::MAX);
                }
            }
        }
        if invalidated_copy_pane.as_ref().is_some_and(|pane_id| {
            self.selection
                .as_ref()
                .is_some_and(|selection| &selection.pane_id == pane_id)
        }) {
            self.selection = None;
            self.stop_selection_autoscroll();
            self.selection_highlight_clear_deadline = None;
        }
        self.popup_terminal_id = next_popup;
        self.graphics
            .set_scene(std::mem::take(&mut surface.graphics));
        self.pane_surface = Some(surface);
        self.pane_surface_generation = self.active_snapshot_generation;
        self.invalidate_link_hover();
        self.resume_mobile_switcher_if_ready();
        self.reconcile_input_source();
    }

    pub(crate) fn tick_popup_pending(&mut self, now: std::time::Instant) {
        if self
            .popup_pending_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.popup_pending = false;
            self.popup_pending_deadline = None;
            self.popup_pending_dismissable = false;
        }
    }

    pub(crate) fn show_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        if !self.config.clipboard_toast_enabled {
            return false;
        }
        self.copy_feedback = Some(crate::app::state::CopyFeedback {
            message: "copied to clipboard".to_owned(),
        });
        self.copy_feedback_deadline = Some(now + std::time::Duration::from_secs(2));
        true
    }

    pub(crate) fn tick_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        let mut repaint = false;
        if self
            .copy_feedback_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.copy_feedback = None;
            self.copy_feedback_deadline = None;
            repaint = true;
        }
        if self
            .selection_highlight_clear_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.selection = None;
            self.selection_highlight_clear_deadline = None;
            repaint = true;
        }
        repaint
    }

    /// Show a transient client-side action error, restarting its lifetime.
    ///
    /// Every assignment must go through this setter so a repeated identical
    /// message gets a fresh deadline instead of inheriting the previous one.
    pub(super) fn set_endpoint_error(&mut self, message: impl Into<String>) {
        self.endpoint_error = Some(message.into());
        self.endpoint_error_deadline = Some(
            std::time::Instant::now() + std::time::Duration::from_secs(ENDPOINT_ERROR_TIMEOUT_SECS),
        );
    }

    pub(crate) fn tick_endpoint_error(&mut self, now: std::time::Instant) -> bool {
        if self.endpoint_error.is_none() {
            self.endpoint_error_deadline = None;
            return false;
        }
        if self
            .endpoint_error_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.endpoint_error = None;
            self.endpoint_error_deadline = None;
            return true;
        }
        false
    }

    /// Advances the turning glyphs to the frame for `now`; whether the frame
    /// changed, so a repaint is due. Nothing happens while none is drawn.
    pub(crate) fn tick_motion(&mut self, now: std::time::Instant) -> bool {
        if !self.motion_active {
            return false;
        }
        let next = crate::ui::motion::Motion::at(now.saturating_duration_since(self.motion_epoch));
        let changed = next != self.motion;
        self.motion = next;
        changed
    }

    pub(crate) fn timer_delay(&self, now: std::time::Instant) -> std::time::Duration {
        let default = std::time::Duration::from_millis(100);
        let next_frame = self.motion_active.then(|| {
            now + crate::ui::motion::Motion::until_next_frame(
                now.saturating_duration_since(self.motion_epoch),
            )
        });
        self.selection_autoscroll_deadline
            .into_iter()
            .chain(next_frame)
            .chain(self.selection_repaint_deadline)
            .chain(self.space_drag_autoscroll.map(|(_, _, deadline)| deadline))
            .chain(self.tooltip_deadline())
            .min()
            .map(|deadline| deadline.saturating_duration_since(now).min(default))
            .unwrap_or(default)
    }

    pub(crate) fn invalidate_pane_surface(&mut self) {
        self.pane_surface = None;
        self.pending_pane_surface = None;
        self.hits = ShellHitMap::default();
        self.host_mouse_pixels = None;
    }
}
