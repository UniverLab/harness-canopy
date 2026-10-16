use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::graph_live_state::GraphLiveState;
use crate::application::notification_service::NotificationService;
use crate::db::project::{RagInfoSummary, RagQueueItem};
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphDetails, GraphNodeRun, GraphSpec};
use crate::domain::models::{Agent, CorruptAgent, RunLog};
use crate::domain::project::Project;
use crate::domain::sync::{ActiveIntent, SyncMessage, WorkspaceStatus};
use crate::rag::vector_store::SearchResult;
use crate::tui::agent::InteractiveAgent;
use crate::tui::app::dialog::{
    GraphFormDialog, LaunchpadDialog, NewAgentDialog, SimplePromptDialog,
};
use crate::tui::app::terminal_search::TerminalSearch;

mod graph_editor;
mod sidebar;

pub(crate) use graph_editor::{GraphEditorDialog, GraphEditorMode, RouterField, RouterRouteDraft};
pub use sidebar::{AgentSectionFocus, AutomationKind, PanelFace, ProjectTab, SidebarLayer};
pub(crate) use sidebar::{
    GraphLiveFocus, GraphSidebarMeta, ProjectPreviewSummary, SidebarStepMemory,
};

/// Unified entry in the sidebar.
#[allow(clippy::large_enum_variant)]
pub enum AgentEntry {
    Agent(Agent),
    /// An agent row that failed to decode (e.g. malformed `trigger_config`
    /// written directly to SQLite by an external tool). Rendered as a
    /// degraded card instead of crashing the whole sidebar.
    Corrupt(CorruptAgent),
    Interactive(usize), // index into App::interactive_agents
    Terminal(usize),    // index into App::terminal_agents
    Orphaned(usize),    // index into App::orphaned_sessions
    Group(usize),       // index into App::split_groups
}

impl AgentEntry {
    pub fn id<'a>(&'a self, app: &'a App) -> &'a str {
        match self {
            Self::Agent(a) => &a.id,
            Self::Corrupt(c) => &c.id,
            Self::Interactive(idx) => app
                .interactive_agents
                .get(*idx)
                .map_or("?", |a| a.seed_name.as_deref().unwrap_or(&a.name)),
            Self::Terminal(idx) => app.terminal_agents.get(*idx).map_or("?", |a| &a.name),
            Self::Orphaned(idx) => app.orphaned_sessions.get(*idx).map_or("?", |s| &s.name),
            Self::Group(idx) => app.split_groups.get(*idx).map_or("?", |g| &g.id),
        }
    }
}

/// Which panel has focus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Home,
    Preview,
    NewAgentDialog,
    LaunchpadDialog,
    KnowledgeDialog,
    KnowledgeViewer,
    Agent,
    ContextTransfer,
    RagTransfer,
    PromptTemplateDialog,
    GraphEditorDialog,
    GraphFormDialog,
    ProjectRelationDialog,
}

/// Mouse text selection over the focused agent's PTY pane. Coordinates are
/// pane-relative `(row, col)` cells matching the rendered `ScreenSnapshot`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalSelection {
    /// Selected agent this selection belongs to: (is_terminal, index).
    pub agent: (bool, usize),
    pub start: (u16, u16),
    pub end: (u16, u16),
    pub dragging: bool,
}

impl TerminalSelection {
    /// Selection endpoints in linear (reading) order: start ≤ end.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        if self.end < self.start {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }
}

/// CT26: the copy-reflex Ctrl+C guard's state — the one-line sentence the
/// pane shows, and the confirm window that decides whether the next Ctrl+C is
/// forwarded on a platform that exits on the first one. One instance lives on
/// `App`.
#[derive(Clone)]
pub(crate) struct CtrlCState {
    /// The sentence to show, if any.
    pub(crate) notice: Option<String>,
    /// When `notice` was set; drives its TTL.
    pub(crate) notice_at: std::time::Instant,
    /// When the first swallowed Ctrl+C arrived on a `ctrl_c_exits` platform;
    /// `None` when no confirm window is open.
    pub(crate) armed_at: Option<std::time::Instant>,
}

impl CtrlCState {
    /// A fresh guard: nothing to show, nothing armed (`notice_at` is backdated
    /// so an expired `None` notice can never be drawn).
    pub fn initial() -> Self {
        Self {
            notice: None,
            notice_at: std::time::Instant::now() - std::time::Duration::from_secs(10),
            armed_at: None,
        }
    }

    pub fn set_notice(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_at = std::time::Instant::now();
    }

    /// The notice to draw, or `None` once its TTL has passed.
    pub fn notice(&self) -> Option<&str> {
        if self.notice_at.elapsed() <= crate::tui::event::ctrl_c::NOTICE_TTL {
            self.notice.as_deref()
        } else {
            None
        }
    }

    pub fn arm(&mut self) {
        self.armed_at = Some(std::time::Instant::now());
    }

    pub fn clear_arm(&mut self) {
        self.armed_at = None;
    }

    /// True only inside the confirm window, so a late second press re-arms
    /// instead of forwarding.
    pub fn armed(&self) -> bool {
        self.armed_at
            .is_some_and(|at| at.elapsed() <= crate::tui::event::ctrl_c::CTRL_C_CONFIRM_WINDOW)
    }
}

/// CT26: the platform slugs (`CliConfig::name`) whose registry entry sets
/// `ctrl_c_exits = true` — the set the Ctrl+C guard consults for the focused
/// interactive session's platform. Read from the persisted `[[clis]]` table
/// (the same table `merge_cli_fields` refreshes from the registry), never
/// inferred from a CLI's name.
pub(crate) fn ctrl_c_exits_clis(
    config: &crate::domain::canopy_config::CanopyConfig,
) -> std::collections::HashSet<String> {
    config
        .clis
        .iter()
        .filter(|cli| cli.ctrl_c_exits)
        .map(|cli| cli.name.clone())
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) enum ContextTransferSource {
    Interactive(usize),
    Terminal(usize),
}

#[derive(Clone)]
pub(crate) struct SyncPanelState {
    pub workdir: String,
    pub participant_count: usize,
    pub vibe: WorkspaceStatus,
    pub active_intents: Vec<ActiveIntent>,
    pub recent_messages: Vec<SyncMessage>,
}

#[derive(Clone)]
pub(crate) struct RagTransferModal {
    pub picker_selected: usize,
    pub query: String,
    pub context_payload: String,
}

// ── App struct ──────────────────────────────────────────────────

/// Main application state.
pub struct App {
    pub(crate) db: Arc<Database>,
    pub(crate) data_dir: PathBuf,

    // Data cache (refreshed every tick)
    pub(crate) agents: Vec<AgentEntry>,
    pub(crate) active_runs: HashMap<String, RunLog>,
    pub(crate) recent_runs: Vec<RunLog>,
    pub(crate) interactive_agents: Vec<InteractiveAgent>,
    /// Raw terminal sessions (no AI CLI).
    pub(crate) terminal_agents: Vec<InteractiveAgent>,
    /// Sessions orphaned during auto-resume (can be revived or dismissed).
    pub(crate) orphaned_sessions: Vec<crate::db::session::InteractiveSession>,
    /// Gate: pending scheduled sends are held (not delivered) until the
    /// startup restore runs — auto-resume reassigns each schedule onto its
    /// resumed session id and drops schedules whose session is gone. Without
    /// this, the first refresh (before sessions resume) would see zero live
    /// sessions and prematurely treat every due schedule as dead.
    pub(crate) scheduled_sends_restored: bool,

    // Split group state
    pub(crate) split_groups: Vec<crate::domain::models::SplitGroup>,
    /// ID of the split group currently being viewed (if any).
    pub(crate) active_split_id: Option<String>,
    /// True = right/bottom panel is focused in split view.
    pub(crate) split_right_focused: bool,
    /// Whether the split picker overlay is open.
    pub(crate) split_picker_open: bool,
    pub(crate) split_picker_idx: usize,
    pub(crate) split_picker_orientation: crate::domain::models::SplitOrientation,
    /// (name, type_label) for each available session in the picker.
    pub(crate) split_picker_sessions: Vec<(String, String)>,

    // Daemon info
    pub(crate) daemon_running: bool,
    pub(crate) daemon_pid: Option<u32>,
    pub(crate) daemon_version: String,
    /// Latest stable release found by the background notice check.
    pub(crate) update_available: Option<String>,

    // UI state
    pub(crate) selected: usize,
    pub(crate) focus: Focus,
    /// Which sidebar tab (Live / Automation / Knowledge) is active.
    pub(crate) sidebar_layer: SidebarLayer,
    /// Which of Automation's two sub-lists is active for navigation.
    pub(crate) automation_kind: AutomationKind,
    /// Remembered per-layer selection for `App::step_sidebar_tab`, so a step
    /// away and back doesn't reset the tab you return to.
    pub(crate) sidebar_step_memory: SidebarStepMemory,
    /// `Some(tab)` while a project is entered (Focus tab bar showing);
    /// `None` while only highlighted (Preview summary card showing).
    pub(crate) project_focus: Option<ProjectTab>,
    pub(crate) selected_project_history: usize,
    /// Persisted per-project History tab data, keyed by project hash and
    /// refreshed lazily on first show of the tab (functional requirement 4).
    pub(crate) project_history_cache: HashMap<String, Vec<crate::db::project::ProjectHistoryEntry>>,
    /// Cheap per-project Preview summary, keyed by project hash and
    /// recomputed on the normal refresh cadence — never per keystroke.
    pub(crate) project_preview_cache: HashMap<String, ProjectPreviewSummary>,
    /// Last-activity timestamp per project hash, in the same order
    /// `projects` is ranked by (`Database::list_projects_by_activity`).
    /// Rendered as the project row's first meta line.
    pub(crate) project_last_activity: HashMap<String, i64>,
    pub(crate) log_content: String,
    pub(crate) log_scroll: u16,
    pub(crate) running: bool,
    pub(crate) new_agent_dialog: Option<NewAgentDialog>,
    pub(crate) launchpad_dialog: Option<LaunchpadDialog>,
    pub(crate) knowledge_dialog: Option<crate::tui::app::dialog::KnowledgeDialog>,
    pub(crate) knowledge_viewer: Option<crate::tui::app::dialog::KnowledgeViewerDialog>,
    pub(crate) pending_launch_dialog: Option<NewAgentDialog>,
    pub(crate) quit_confirm: bool,
    pub(crate) delete_project_confirm: bool,
    pub(crate) archive_graph_confirm: bool,
    /// Permanent-delete confirmation, reachable only from the archived
    /// view on an already-archived graph — see [`App::permanent_delete_selected_archived_graph`].
    pub(crate) permanent_delete_graph_confirm: bool,
    /// Confirmation gate for `graph_reset` (mirrors `archive_graph_confirm`) —
    /// reset clears progress on every non-completed spec, so it asks first,
    /// with the same wording the CLI's own prompt uses (see
    /// `daemon::graph_cli::confirm_reset`) so the two surfaces never teach
    /// different levels of caution.
    pub(crate) graph_reset_confirm: bool,

    // Brian's Brain automaton (sidebar decoration)
    pub(crate) sidebar_brain: Option<crate::tui::brians_brain::BriansBrain>,
    // Brian's Brain for home banner background
    pub(crate) home_brain: Option<crate::tui::brians_brain::BriansBrain>,
    /// Rect where the home brain was last drawn; pointer seeding is scoped to
    /// it so a mouse move elsewhere never plants cells.
    pub(crate) home_brain_rect: Option<ratatui::layout::Rect>,
    /// Rect where the sidebar brain was last drawn.
    pub(crate) sidebar_brain_rect: Option<ratatui::layout::Rect>,

    // System monitoring (updated asynchronously to avoid UI freezes)
    pub(crate) system_info: crate::system::SystemInfo,
    pub(crate) system_info_target: crate::system::SystemInfo,
    pub(crate) system_info_rx: std::sync::mpsc::Receiver<crate::system::SystemInfo>,
    /// Controls system monitor activity: true = poll, false = pause polling.
    pub(crate) system_monitor_active: Arc<AtomicBool>,
    pub(crate) last_system_update: std::time::Instant,
    pub(crate) last_system_frame_at: std::time::Instant,
    pub(crate) process_start_time: std::time::Instant,

    // Layout state
    pub(crate) sidebar_click_map: Vec<(usize, u16, u16)>,
    /// Agent index under the mouse cursor in the sidebar (hover highlight).
    pub(crate) hovered_row: Option<usize>,
    /// Manual mouse-wheel scroll adjustment applied on top of the
    /// selection-follow scroll in the agent sidebar sections.
    pub(crate) sidebar_scroll_offset: usize,
    /// Total visible agent rows across the rendered sidebar sections on the
    /// last frame; used to clamp mouse-wheel scrolling.
    pub(crate) sidebar_visible_capacity: usize,
    pub(crate) projects: Vec<Project>,
    pub(crate) selected_project: usize,
    pub(crate) agent_section_focus: AgentSectionFocus,
    /// Mouse hit-test rows for the Automation layer's graph cards, populated
    /// during draw: `(graph id, row_start, row_end)`.
    pub(crate) automation_graph_click_map: Vec<(String, u16, u16)>,
    /// Mouse hit-test rows for the Knowledge layer's project list,
    /// populated during draw: `(project index, row_start, row_end)`.
    pub(crate) project_click_map: Vec<(usize, u16, u16)>,
    /// Mouse hit-test columns for a Focus tab bar, populated during draw:
    /// `(tab, col_start, col_end)`.
    pub(crate) project_tab_click_map: Vec<(ProjectTab, u16, u16)>,
    /// Mouse hit-test rows for the active tab's list, populated during draw.
    pub(crate) project_tab_row_click_map: Vec<(usize, u16, u16)>,
    /// Mouse hit-test cells for the sidebar's tab strip, populated during
    /// draw: `(tab, row, col_start, col_end)` — clicking switches the active
    /// tab.
    pub(crate) sidebar_tab_click_map: Vec<(SidebarLayer, u16, u16, u16)>,
    pub(crate) graphs: Vec<Graph>,
    /// Archived graphs (excluded from `graphs`), populated only while
    /// `graph_view_archived` is true — see [`App::refresh_graphs`].
    pub(crate) archived_graphs: Vec<Graph>,
    /// Count of archived graphs, kept up to date on every refresh so it's
    /// visible from the main view at all times regardless of
    /// `graph_view_archived`.
    pub(crate) archived_graph_count: usize,
    /// Whether the Graphs sidebar section is currently showing the archive
    /// (`true`) instead of the main list (`false`) — a toggle on the
    /// existing Graphs section rather than a separate sidebar layer, so
    /// archived graphs stay in the same mental place as active ones.
    pub(crate) graph_view_archived: bool,
    pub(crate) selected_graph_id: Option<String>,
    pub(crate) graph_details: Option<GraphDetails>,
    pub(crate) graph_runs: Vec<GraphNodeRun>,
    pub(crate) graph_selected_spec: usize,
    pub(crate) graph_selected_node: usize,
    pub(crate) graph_editor_dialog: Option<GraphEditorDialog>,
    pub(crate) graph_form_dialog: Option<GraphFormDialog>,
    /// Per-graph spec progress ("done/total") and blocked status for the
    /// sidebar's `Graphs` section, keyed by graph id. Refreshed alongside
    /// `graphs` in `App::refresh_graphs`.
    pub(crate) graph_sidebar_meta: HashMap<String, GraphSidebarMeta>,
    /// Live snapshot of the currently-selected graph's runtime state,
    /// refreshed every tick. `None` when no graph is selected.
    pub(crate) graph_live_state: Option<GraphLiveState>,
    /// Whether the live graph view's graph highlight auto-follows the
    /// engine's current node (`true`, the default) or sits on a node the
    /// user manually navigated to (`false`, see `graph_live_selected_node`).
    /// Reset to `true` whenever the selected graph changes. CT23: this is
    /// also the graph's entered/not-entered state — it only becomes `false`
    /// via `App::graph_live_enter` (`Enter`), and plain arrow keys drive the
    /// graph's internal navigation only while it is `false`; while `true`,
    /// plain arrows fall through to ordinary sidebar navigation instead.
    pub(crate) graph_live_follow: bool,
    /// The node id manually highlighted in the live graph view's graph.
    /// Only meaningful while `graph_live_follow` is `false`.
    pub(crate) graph_live_selected_node: Option<String>,
    /// CT8: the node id the live graph last re-centred the scroll onto while
    /// auto-following. Auto-follow re-centres ONLY when the engine's current
    /// node changes away from this anchor; every other redraw (same node,
    /// changed status/elapsed, the user scrolling) leaves the scroll alone.
    /// `None` forces exactly one re-centre on the next render — set when a
    /// graph is selected, when `Esc` restores follow, and when a vanished
    /// manual selection falls back to follow.
    pub(crate) graph_live_follow_anchor: Option<String>,
    /// Which sub-region of the live graph view plain arrow keys drive.
    /// Reset to `Graph` whenever the selected graph changes.
    pub(crate) graph_live_focus: GraphLiveFocus,
    /// The spec id manually selected in the live graph view's marker strip
    /// (independent of `current_spec_id` / the graph's own follow state —
    /// selecting a spec here never touches `graph_live_follow`). `None`
    /// means the strip shows the running/next-pending spec by default.
    pub(crate) graph_spec_strip_selected: Option<String>,
    /// First visible index into `GraphLiveState::spec_queue` for the marker
    /// strip, when there are more specs than fit in the panel's width.
    pub(crate) graph_spec_strip_scroll: usize,
    /// How many marker chips fit in the panel's width on the last render —
    /// used to keep keyboard navigation's scroll offset in sync with what's
    /// actually drawn. Populated in `draw_graph_live_view`.
    pub(crate) graph_spec_strip_capacity: usize,
    /// Mouse hit-test cells for the marker strip, populated during draw:
    /// `(spec id, row, col_start, col_end)` — mirrors `sidebar_tab_click_map`.
    pub(crate) graph_spec_strip_click_map: Vec<(String, u16, u16, u16)>,
    /// Vertical scroll offset (in text lines) for the live graph view's main
    /// content area. Clamped to `[0, total_lines - panel_height]` on every
    /// render. Reset to 0 whenever the selected graph changes.
    pub(crate) graph_live_view_scroll: u16,
    /// Total number of content lines rendered on the last frame — used to
    /// clamp `graph_live_view_scroll`. Populated by `render_graph_live_view`.
    pub(crate) graph_live_view_total_lines: u16,
    /// Open autorun-scheduling input for the graph currently focused in the
    /// live view — `None` when not open. See
    /// [`crate::tui::app::dialog::GraphAutorunDialog`].
    pub(crate) graph_autorun_dialog: Option<crate::tui::app::dialog::GraphAutorunDialog>,
    /// CT3 live-tail viewer for a running check node. Diagnostic-only: all
    /// reads, so dismissing it can never affect execution. `Some` while
    /// open (including after the node finishes, showing the final banner).
    pub(crate) node_tail_dialog: Option<crate::tui::app::dialog::NodeTailDialog>,
    /// CT28: usable body height (rows) of the tail pane on the last frame —
    /// the layout sets it so key handlers can page by half-height.
    pub(crate) node_tail_pane_body_height: usize,
    /// True while a graph-control dispatch (`graph_run`/`graph_pause`/
    /// `graph_continue`/`graph_reset`/`graph_schedule_autorun`) is in flight on
    /// `graph_action_rx` — guards against a second dispatch racing the first.
    pub(crate) graph_action_pending: bool,
    /// Receiver for the background thread running the current graph-control
    /// dispatch (see `App::dispatch_graph_action`), polled non-blockingly by
    /// `App::poll_graph_action` every tick so the UI thread never waits on the
    /// daemon's HTTP round-trip.
    pub(crate) graph_action_rx:
        Option<std::sync::mpsc::Receiver<crate::tui::app::dialog::GraphActionOutcome>>,
    /// The daemon's verbatim response to the last graph-control action, shown
    /// until dismissed or superseded by the next dispatch — success or
    /// error, per the graph controls' "always shown, never swallowed" rule.
    pub(crate) graph_action_message: Option<crate::tui::app::dialog::GraphActionMessage>,
    /// When the current `graph_action_message` was set — drives its
    /// auto-dismiss (mirrors `copied_at`/`dismiss_copied`).
    pub(crate) graph_action_message_at: std::time::Instant,
    /// Standalone/backlog specs (no graph yet), filtered to the selected
    /// project's workdir tag when a project is selected. Refreshed alongside
    /// `projects` in `App::refresh_projects`.
    pub(crate) backlog_specs: Vec<GraphSpec>,
    pub(crate) selected_backlog: usize,
    pub(crate) global_rag_queue: Vec<RagQueueItem>,
    pub(crate) selected_rag_queue: usize,
    pub(crate) rag_info: RagInfoSummary,
    /// Per-file RAG status loaded from `rag_file_events` table.
    pub(crate) rag_file_status: Vec<crate::db::project::RagPerFileStatus>,
    /// Knowledge nodes (facts/patterns) for the selected project.
    pub(crate) project_knowledge: Vec<crate::db::intelligence::IntelligenceNodeRecord>,
    pub(crate) selected_knowledge: usize,
    /// Wheel-scroll offset for the right panel's knowledge list (Knowledge
    /// face). Independent of `selected_knowledge`, which the main Knowledge
    /// tab (`ProjectTab::Knowledge`) owns — this only affects what the right
    /// panel shows.
    pub(crate) knowledge_list_scroll: u16,
    /// Wheel-scroll offset (in edges) for the right panel's project-relations
    /// graph (Knowledge face). Fed into `draw_project_graph`'s new `offset`
    /// parameter; the sidebar's own project-graph card always passes 0.
    pub(crate) knowledge_graph_scroll: u16,
    pub(crate) knowledge_filter: String,
    pub(crate) knowledge_filter_mode: bool,
    /// Type-ahead over the Knowledge layer's project list (sidebar layer
    /// Knowledge, no project entered). Deliberately separate from
    /// `knowledge_filter`, which filters entries *inside* a project.
    pub(crate) project_filter: String,
    pub(crate) sidebar_visible: bool,
    pub(crate) hidden_activity_workdirs: HashSet<String>,
    pub(crate) forced_activity_workdirs: HashSet<String>,
    pub(crate) term_width: u16,
    /// CT31: whether the F1 Canopy hub overlay is open.
    pub(crate) hub_open: bool,
    /// Cursor of the hub's `Missions & stats` tab.
    pub(crate) legend_selected: usize,
    pub(crate) show_copied: bool,
    pub(crate) copied_at: std::time::Instant,
    /// CT26: the copy-reflex Ctrl+C guard's state.
    pub(crate) ctrl_c: CtrlCState,
    /// CT26: platform slugs whose registry entry sets `ctrl_c_exits = true`.
    /// Derived from `CanopyConfig` at startup and re-derived on every settings
    /// revision, so a registry refresh needs no restart. Empty for every
    /// platform that keeps today's forwarding.
    pub(crate) ctrl_c_exits_clis: std::collections::HashSet<String>,
    pub(crate) last_scroll_at: std::time::Instant,
    pub(crate) last_panel_inner: (u16, u16),
    pub(crate) last_panel_x: u16,
    pub(crate) last_panel_y: u16,
    /// Active mouse text selection over the focused agent's PTY pane.
    pub(crate) terminal_selection: Option<TerminalSelection>,
    pub(crate) whimsg: crate::tui::whimsg::Whimsg,
    /// Hash of the last log chunk scanned for whimsg triggers — avoids re-firing
    /// on the same content every tick.
    pub(crate) whimsg_last_log_hash: u64,
    /// Previous per-graph snapshot for the header's graph events. `None` until
    /// the first refresh primes it silently (no events on the first sighting).
    pub(crate) whimsg_graphs: Option<HashMap<String, crate::tui::whimsg::events::GraphSnapshot>>,
    /// Background run ids already announced by the header, so each fires once.
    pub(crate) whimsg_announced_runs: HashSet<String>,
    pub(crate) context_transfer_modal: Option<crate::tui::context_transfer::ContextTransferModal>,
    pub(crate) rag_transfer_modal: Option<RagTransferModal>,
    pub(crate) context_transfer_config: crate::tui::context_transfer::ContextTransferConfig,
    /// Prompt templates loaded from registry
    #[allow(dead_code)]
    pub(crate) prompt_templates: crate::tui::prompt_templates::PromptTemplates,
    /// Current simple prompt dialog state
    pub(crate) simple_prompt_dialog: Option<SimplePromptDialog>,
    /// Persisted prompt-builder sessions per agent/session (cleared on send).
    pub(crate) prompt_builder_sessions:
        HashMap<String, crate::tui::app::dialog::PromptBuilderSession>,
    /// Tab-bar origin `(x, y)` of the prompt builder from the last frame, used
    /// for mouse hit-testing the clickable Normal/Raw tabs.
    pub(crate) prompt_tab_origin: Option<(u16, u16)>,
    /// Raw tab content region `Rect` from the last frame, used for mouse
    /// wheel hit-testing the scrollable content area.
    pub(crate) prompt_raw_content_rect: Option<ratatui::layout::Rect>,
    /// Whether to send OS-level desktop notifications (agent done/failed).
    pub(crate) notifications_enabled: bool,
    /// CM52: the `[tui]` settings this app renders with (date/time formats,
    /// panel dwell, scroll steps, brian's-brain and atmosphere toggles). A
    /// snapshot, reloaded when the daemon publishes a newer settings
    /// revision — the TUI redraws every frame and cannot reload
    /// `config.toml` per draw.
    pub(crate) tui_settings: crate::domain::canopy_config::TuiConfig,
    /// Notification service for sending cross-platform notifications.
    pub(crate) notification_service: Arc<dyn NotificationService>,
    /// IDs of runs that were active on the previous refresh tick.
    pub(crate) prev_active_run_ids: std::collections::HashSet<String>,
    /// Tick counter for animation (increments every refresh)
    pub(crate) animation_tick: u32,
    /// Preferred unit for sysinfo temperature labels.
    pub(crate) temperature_unit: crate::domain::canopy_config::TemperatureUnit,
    /// Resolved TUI color theme (T6), read from config once at startup.
    /// No live switching yet — changing it requires a restart.
    pub(crate) theme: crate::tui::ui::theme::Theme,
    /// Terminal autocomplete suggestion picker (shown on Tab).
    pub(crate) suggestion_picker: Option<crate::tui::terminal_history::SuggestionPicker>,
    /// Per-session terminal histories (loaded on demand, cached in memory).
    pub(crate) terminal_histories: HashMap<String, crate::tui::terminal_history::SessionHistory>,
    /// Terminal scrollback search state (Ctrl+F).
    pub(crate) terminal_search: Option<TerminalSearch>,
    /// CLI launch usage counters (persisted to disk).
    pub(crate) cli_usage: crate::domain::usage_stats::CliUsage,

    // Activity panel scroll
    pub(crate) sync_scroll_offset: u16,
    /// Last rendered area of the activity panel (used for mouse hit-testing).
    pub(crate) last_sync_area: Option<ratatui::layout::Rect>,
    /// Last-rendered rect of the activity section — the full Activity face,
    /// or the activity sub-area inside the Knowledge face. Populated during
    /// draw, cleared to `None` for a frame where it isn't drawn.
    pub(crate) last_activity_rect: Option<ratatui::layout::Rect>,
    /// Last-rendered rect of the Knowledge face's project-relations graph.
    pub(crate) last_knowledge_graph_rect: Option<ratatui::layout::Rect>,
    /// Last-rendered rect of the Knowledge face's knowledge/backlog list.
    pub(crate) last_knowledge_list_rect: Option<ratatui::layout::Rect>,
    /// Last-rendered rect of the Graph face's content (the whole face — it
    /// isn't sub-divided).
    pub(crate) last_graph_face_rect: Option<ratatui::layout::Rect>,
    /// Wheel-scroll offset (lines) for the right panel's Graph face. Own
    /// field, NOT shared with the main preview pane's
    /// `graph_live_view_scroll` — see design note in canopy-design.md: the
    /// right panel's Graph face can be visible independently of the main
    /// pane's live graph view, so sharing bookkeeping would clamp against
    /// stale/unrelated state.
    pub(crate) graph_face_scroll: u16,
    /// Total rendered line count of the Graph face's last draw, written by
    /// `draw_graph_face`. Used to clamp `graph_face_scroll`.
    pub(crate) graph_face_total_lines: u16,

    // CT1 multi-face right panel: the switching rule lives in
    // `crate::tui::app::panel_face` — these are its inputs and outputs.
    /// Currently visible face. Only ever changed by the panel-face tick or
    /// by an explicit pin/pick — never as a side effect of rendering — so
    /// switching faces can't flicker or force a full-screen redraw.
    pub(crate) panel_face: PanelFace,
    /// The face pinned from the picker (`None` = automatic mode).
    pub(crate) panel_pinned: Option<PanelFace>,
    /// Event-driven face with its 10-second dwell deadline. A newer event
    /// replaces it and restarts the dwell; events never queue.
    pub(crate) panel_dwell_face: Option<PanelFace>,
    pub(crate) panel_dwell_until: Option<std::time::Instant>,
    /// Human-readable reason for the current dwell (`"new knowledge"`,
    /// `"backlog changed"`). Shown in the title while the dwell holds.
    pub(crate) panel_dwell_reason: Option<String>,
    /// Reason for the last automatic switch (`"graph running"`, `"new
    /// knowledge"`, …). Rendered as a badge so a face never appears
    /// unexplained. `None` after a manual pin/pick.
    pub(crate) panel_last_reason: Option<String>,
    /// Whether the face picker overlay is open, and its cursor.
    pub(crate) panel_picker_open: bool,
    pub(crate) panel_picker_idx: usize,
    /// CM51 (D4): the last `settings_revision` this session has applied. When
    /// the daemon publishes a newer one (a `config_set`, or a hand edit seen
    /// by the file watcher), the TUI reloads `CanopyConfig` so the values it
    /// renders — theme, temperature unit, the pinned face — are current
    /// without a restart.
    pub(crate) settings_seen: String,
    /// True while the mouse is pressed/dragging inside the panel or the
    /// panel was clicked into (keyboard focus claimed by the panel).
    /// While set, any pending automatic switch is dropped, not deferred.
    pub(crate) panel_focused: bool,
    /// CT30: state of the Crews panel face (pairs + crews sections). Keys
    /// are routed here only while `panel_focused` and the Crews face is
    /// visible; data arrives through the daemon client only (never SQLite).
    pub(crate) crews: crate::tui::app::crews_face::CrewsFaceState,
    /// CT31: the F1 Canopy hub's session-only state (last-used tab, the
    /// Settings tab's data and editors, the Keys tab's scroll offset).
    pub(crate) hub: crate::tui::app::hub::HubState,
    /// Set by a mouse-wheel scroll over the panel; consumed (cleared) by
    /// the next panel-face tick, which drops the pending switch with it.
    pub(crate) panel_interacting: bool,
    /// Baselines for event detection, independent of the capped display lists.
    pub(crate) panel_last_knowledge_updated: Option<i64>,
    pub(crate) panel_last_backlog_updated: Option<i64>,
    /// Graph-running state at the last tick (the STATE input).
    pub(crate) panel_last_graph_running: bool,
    /// False until the first tick has seeded the baselines above, so the
    /// initial data load never fires a spurious event.
    pub(crate) panel_baselines_init: bool,

    // RAG pause state (synced from daemon_state table)
    pub(crate) rag_paused: bool,
    /// Whether the embedding model is currently loaded in the daemon's
    /// memory (synced from daemon_state table — see `rag::status`).
    pub(crate) rag_model_loaded: bool,
    /// Configured embeddings model id, snapshotted from config.toml at
    /// startup — lets the status widgets show "unavailable" when the
    /// configured provider is one this binary cannot serve (see
    /// `rag::status::compute_rag_status`), instead of only failing at
    /// query time.
    pub(crate) rag_embeddings_model: String,
    /// Current acquisition (download+prepare) state for the configured
    /// local embedding model, read from daemon_state per-model keys.
    /// `None` when the model is already fully available or was never
    /// tracked — the normal ready/sleeping logic applies.
    pub(crate) rag_acquisition_state: Option<crate::rag::status::AcquisitionState>,
    /// Whether the RagInfo panel has focus in Agents sidebar mode.
    pub(crate) agents_rag_focused: bool,

    // RAG Playground state
    pub(crate) playground_active: bool,
    pub(crate) playground_query: String,
    pub(crate) playground_results: Vec<SearchResult>,
    pub(crate) playground_selected: usize,
    pub(crate) playground_last_search: std::time::Instant,
    pub(crate) playground_search_pending: bool,
    /// In-flight background playground search (B23): receiving end of the
    /// worker thread running embed+search off the UI thread, tagged with the
    /// query it executed. `Some` while a search is executing — the TUI keeps
    /// rendering and polling instead of blocking on the model load.
    pub(crate) playground_search_rx:
        Option<std::sync::mpsc::Receiver<(String, anyhow::Result<Vec<SearchResult>>)>>,
    pub(crate) playground_last_executed_query: String,
    /// Whether the playground is showing a single chunk in detail mode.
    pub(crate) playground_detail_mode: bool,
    /// Scroll offset for the detail view content.
    pub(crate) playground_scroll: u16,
    /// Optional project hash to filter search results. None = Global.
    pub(crate) playground_project_hash: Option<String>,
    /// Tracks whether the session-start protocol block has been sent per
    /// session. Keyed by `App::current_prompt_session_key` (agent/session
    /// ID when one exists, workdir as a fallback) rather than by workdir
    /// alone, so two different agent sessions sharing a workdir don't share
    /// delivery state.
    pub(crate) session_protocol_state: HashMap<String, SessionProtocolState>,

    pub(crate) active_sandbox: Option<crate::domain::sandbox::Sandbox>,

    // Project relation graph
    pub(crate) project_relation_dialog: Option<ProjectRelationDialog>,
    pub(crate) project_graph_edges: Vec<ProjectGraphEdge>,
    pub(crate) project_graph_trees: Vec<Vec<String>>,

    // Nursery — temporary path for seed creation graph
    pub(crate) nursery_path: Option<std::path::PathBuf>,

    /// Whether the terminal supports and has enabled the Kitty keyboard
    /// enhancement protocol (Shift+Enter disambiguation).
    pub(crate) keyboard_enhancement_active: bool,

    // Atmosphere engine
    pub(crate) atmosphere: crate::tui::atmosphere::SceneManager,
    pub(crate) atmosphere_ctx: crate::tui::atmosphere::AtmosphereCtx,
    /// Previous mouse position for delta calculation.
    pub(crate) atmosphere_last_mouse: (u16, u16),
    /// When true, particles are not rendered (suppressed by held click).
    pub(crate) atmosphere_hidden: bool,

    // Gamification
    pub(crate) mission_manager: crate::tui::gamification::MissionManager,
    pub(crate) mission_pending_events: Vec<crate::tui::gamification::MissionEvent>,
    pub(crate) max_cpu_frequency_seen: Option<u64>,
    /// Monotonic anchor for accumulating real Canopy uptime (persisted in state).
    pub(crate) uptime_anchor: Option<std::time::Instant>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct ProjectGraphEdge {
    pub from_name: String,
    pub to_name: String,
    pub from_hash: String,
    pub to_hash: String,
    pub relation: String,
}

#[derive(Clone)]
pub(crate) struct ProjectRelationDialog {
    pub from_hash: String,
    pub from_name: String,
    pub available: Vec<crate::db::intelligence::IntelligenceNodeRecord>,
    pub filtered: Vec<usize>,
    pub selected_idx: usize,
    pub relation_idx: usize,
    pub relation_types: Vec<String>,
    pub filter_buffer: String,
    pub error: Option<String>,
}

/// Tracks session-start protocol block delivery per session for idempotency.
/// The block is the static "[CANOPY]" contract, not the
/// per-turn workspace/intents context, which is sent every turn
/// regardless of this state.
#[derive(Clone, Default)]
pub(crate) struct SessionProtocolState {
    pub protocol_sent: bool,
}

#[cfg(test)]
mod router_routes_dialog_tests {
    use super::{GraphEditorDialog, RouterField, RouterRouteDraft};

    fn dialog_with_routes(labels: &[&str]) -> GraphEditorDialog {
        let routes = labels
            .iter()
            .map(|label| RouterRouteDraft {
                label: label.to_string(),
                description: format!("{label} desc"),
                target_node_id: None,
            })
            .collect();
        let targets = vec![
            ("n1".to_string(), "Node One".to_string()),
            ("n2".to_string(), "Node Two".to_string()),
        ];
        GraphEditorDialog::new_router_routes(
            "router1".to_string(),
            "Classify".to_string(),
            "title".to_string(),
            "help".to_string(),
            routes,
            String::new(),
            targets,
        )
    }

    #[test]
    fn router_next_field_cycles_through_a_route_then_advances_to_the_next() {
        let mut dialog = dialog_with_routes(&["a", "b"]);
        assert_eq!(dialog.router_field, RouterField::Label);

        dialog.router_next_field();
        assert_eq!(dialog.router_field, RouterField::Description);
        assert_eq!(dialog.router_route_index, 0);

        dialog.router_next_field();
        assert_eq!(dialog.router_field, RouterField::Target);
        assert_eq!(dialog.router_route_index, 0);

        dialog.router_next_field();
        assert_eq!(dialog.router_field, RouterField::Label);
        assert_eq!(dialog.router_route_index, 1, "wraps to the next route");
    }

    #[test]
    fn router_prev_field_is_the_exact_inverse() {
        let mut dialog = dialog_with_routes(&["a", "b"]);
        dialog.router_route_index = 1;
        dialog.router_field = RouterField::Label;

        dialog.router_prev_field();
        assert_eq!(dialog.router_field, RouterField::Target);
        assert_eq!(
            dialog.router_route_index, 0,
            "wraps back to the previous route"
        );
    }

    #[test]
    fn router_push_and_pop_char_edit_the_focused_route_field() {
        let mut dialog = dialog_with_routes(&["", ""]);
        dialog.router_routes[0].description.clear();
        dialog.router_field = RouterField::Label;
        dialog.router_push_char('b');
        dialog.router_push_char('i');
        dialog.router_push_char('n');
        assert_eq!(dialog.router_routes[0].label, "bin");

        dialog.router_pop_char();
        assert_eq!(dialog.router_routes[0].label, "bi");

        dialog.router_next_field();
        dialog.router_push_char('x');
        assert_eq!(dialog.router_routes[0].description, "x");
        // The other route is untouched.
        assert_eq!(dialog.router_routes[1].label, "");
    }

    #[test]
    fn cycle_router_target_moves_through_none_and_every_candidate() {
        let mut dialog = dialog_with_routes(&["a"]);
        assert_eq!(dialog.router_routes[0].target_node_id, None);

        dialog.cycle_router_target(true);
        assert_eq!(
            dialog.router_routes[0].target_node_id.as_deref(),
            Some("n1")
        );

        dialog.cycle_router_target(true);
        assert_eq!(
            dialog.router_routes[0].target_node_id.as_deref(),
            Some("n2")
        );

        dialog.cycle_router_target(true);
        assert_eq!(
            dialog.router_routes[0].target_node_id, None,
            "wraps back to unwired after the last candidate"
        );

        dialog.cycle_router_target(false);
        assert_eq!(
            dialog.router_routes[0].target_node_id.as_deref(),
            Some("n2")
        );
    }

    #[test]
    fn router_clear_target_unwires_the_focused_route() {
        let mut dialog = dialog_with_routes(&["a"]);
        dialog.router_routes[0].target_node_id = Some("n1".to_string());
        dialog.router_clear_target();
        assert_eq!(dialog.router_routes[0].target_node_id, None);
    }

    #[test]
    fn router_add_route_appends_and_focuses_a_fresh_unwired_route() {
        let mut dialog = dialog_with_routes(&["a", "b"]);
        dialog.router_add_route();

        assert_eq!(dialog.router_routes.len(), 3);
        assert_eq!(dialog.router_route_index, 2);
        assert_eq!(dialog.router_field, RouterField::Label);
        assert_eq!(dialog.router_routes[2].label, "");
        assert_eq!(dialog.router_routes[2].target_node_id, None);
    }

    #[test]
    fn router_remove_route_drops_it_and_clears_a_dangling_fallback() {
        let mut dialog = dialog_with_routes(&["a", "b", "c"]);
        dialog.router_fallback = "b".to_string();
        dialog.router_route_index = 1;

        dialog.router_remove_route();

        assert_eq!(dialog.router_routes.len(), 2);
        assert!(dialog.router_routes.iter().all(|r| r.label != "b"));
        assert_eq!(
            dialog.router_fallback, "",
            "fallback naming the removed route is cleared, not left dangling"
        );
    }

    #[test]
    fn router_remove_route_keeps_an_unrelated_fallback() {
        let mut dialog = dialog_with_routes(&["a", "b", "c"]);
        dialog.router_fallback = "a".to_string();
        dialog.router_route_index = 1; // removes "b"

        dialog.router_remove_route();

        assert_eq!(dialog.router_fallback, "a");
    }

    #[test]
    fn router_set_fallback_names_the_focused_route() {
        let mut dialog = dialog_with_routes(&["a", "b"]);
        dialog.router_route_index = 1;
        dialog.router_set_fallback();
        assert_eq!(dialog.router_fallback, "b");
    }
}
