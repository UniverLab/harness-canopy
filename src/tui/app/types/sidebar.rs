use chrono::{DateTime, Utc};

/// The sidebar's three thematic tabs, shown one at a time below the pinned
/// RAG summary (top) and above the sysinfo dashboard (bottom).
/// `App::sidebar_layer` tracks which one is currently active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarLayer {
    /// Interactive agents + terminals — the things with a PTY right now.
    Live,
    /// Background agents + graphs — live/recent runs, global across projects.
    Automation,
    /// The projects list.
    Knowledge,
}

/// Which of Automation's two sub-lists (background agents, graphs) arrow-key
/// navigation is currently cycling through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutomationKind {
    Agent,
    Graph,
}

/// The right panel's faces (CT1): one panel, four views. Activity is the
/// resting face when nothing else applies; Knowledge shows activity moved
/// under it plus the project-relations graph; Graph shows a read-only view
/// of the running graph; Crews is a management view: pin-only, never chosen
/// automatically.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelFace {
    #[default]
    Activity,
    Knowledge,
    Graph,
    Crews,
}

impl PanelFace {
    /// Every face, in panel order. The panel picker that consumed this is
    /// retired; the persistence round-trip test still walks it.
    #[allow(dead_code)]
    pub const ALL: [PanelFace; 4] = [
        PanelFace::Activity,
        PanelFace::Knowledge,
        PanelFace::Graph,
        PanelFace::Crews,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PanelFace::Activity => "activity",
            PanelFace::Knowledge => "knowledge",
            PanelFace::Graph => "graph",
            PanelFace::Crews => "crews",
        }
    }

    /// Parse the persisted config string. Unknown values map to `None` so a
    /// config written by a newer binary never breaks this one — the panel
    /// just stays in automatic mode.
    pub fn from_str(value: &str) -> Option<PanelFace> {
        match value {
            "activity" => Some(PanelFace::Activity),
            "knowledge" => Some(PanelFace::Knowledge),
            "graph" => Some(PanelFace::Graph),
            "crews" => Some(PanelFace::Crews),
            _ => None,
        }
    }
}

/// Tabs shown inside a project once it's entered (`Focus::Agent` while
/// `SidebarLayer::Knowledge` is active) — everything project-scoped lives
/// here instead of as top-level sidebar siblings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectTab {
    Overview,
    Backlog,
    Knowledge,
    History,
}

/// Per-layer selection remembered across a keyboard tab *step*
/// (`Shift+←/→`), so stepping away and back lands you where you left off —
/// unlike a direct jump (mouse click, F2), which deliberately always lands
/// on the tab's edge item. `Live` and `Automation`'s agent sub-list share
/// `App::selected` as their index space, so a step away from one clobbers
/// the other's value; this cache is what lets a step back restore it.
#[derive(Clone, Default)]
pub(crate) struct SidebarStepMemory {
    pub(crate) live_selected: Option<usize>,
    pub(crate) automation_kind: Option<AutomationKind>,
    pub(crate) automation_selected: Option<usize>,
    pub(crate) automation_graph_id: Option<String>,
    pub(crate) knowledge_selected: Option<usize>,
}

impl ProjectTab {
    pub const ALL: [ProjectTab; 4] = [
        ProjectTab::Overview,
        ProjectTab::Backlog,
        ProjectTab::Knowledge,
        ProjectTab::History,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProjectTab::Overview => "Overview",
            ProjectTab::Backlog => "Backlog",
            ProjectTab::Knowledge => "Knowledge",
            ProjectTab::History => "History",
        }
    }

    pub fn hotkey(self) -> char {
        match self {
            ProjectTab::Overview => 'o',
            ProjectTab::Backlog => 'b',
            ProjectTab::Knowledge => 'k',
            ProjectTab::History => 'h',
        }
    }
}

/// Cheap, cached summary shown on a project's Preview card (highlighted, not
/// entered) — recomputed on the normal `App::refresh` cadence, never on a
/// per-keystroke highlight move (functional requirement 3).
#[derive(Clone, Default)]
pub(crate) struct ProjectPreviewSummary {
    pub pending_backlog: usize,
    pub knowledge_entries: usize,
    pub last_activity: Option<i64>,
    pub graph_running: bool,
}

/// Per-graph rendering data for the sidebar's `Graphs` section: when the graph
/// last did something, and whether it is stuck on a reported blocker (a
/// `Paused` graph whose latest run recorded a `blocker`, see
/// `graph_report_blocker`). Computed once per refresh cycle
/// (`App::refresh_graphs`) rather than queried or formatted per frame — the
/// sidebar just prints `last_run_label` as-is.
///
/// Deliberately not a per-graph spec count: that reads `0/0` for a
/// queue-driven graph, whose specs live on the queue rather than on the graph
/// itself (the graph focus view's `state.done_count`/`total_count` is the
/// correct place for that number, and is unaffected by this struct).
#[derive(Clone)]
pub(crate) struct GraphSidebarMeta {
    /// Last recorded activity for this graph: the latest `graph_runs.started_at`
    /// across every node run belonging to it, or — if it has never run — the
    /// graph's own `created_at`. A single monotonic "last activity" key that
    /// is defined for every graph regardless of status — the sort key behind
    /// `App::sidebar_graphs`' most-recent-first ordering.
    pub last_activity: DateTime<Utc>,
    /// Precomputed display text for `last_activity`: a compact relative
    /// time (`2m`, `1h`, `3d`), `"running"` while the graph is actively
    /// executing, or `"never"` if it has never run.
    pub last_run_label: String,
    pub blocked: bool,
    /// `"resumes 5m"`-style label when the graph has a pending
    /// `graph_control`, `None` otherwise.
    pub autorun_label: Option<String>,
}

impl Default for GraphSidebarMeta {
    /// Only used as a placeholder before the first `refresh_graphs` populates
    /// the real map — every graph gets a real entry on every refresh, so this
    /// value is never actually shown.
    fn default() -> Self {
        Self {
            last_activity: DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is representable"),
            last_run_label: String::new(),
            blocked: false,
            autorun_label: None,
        }
    }
}

/// Border-focus sub-section within the `Live` layer (interactive/terminal
/// agents render as three stacked sub-panels sharing one collapsible layer).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[expect(dead_code)]
pub enum AgentSectionFocus {
    Interactive,
    Terminal,
    Groups,
    Brain,
}

/// Which sub-region of the live graph view owns plain arrow-key navigation:
/// the node graph (`graph_live_move_highlight`, the long-standing default)
/// or the spec marker strip at the top (`graph_spec_strip_move_selection`).
/// Toggled with Tab/BackTab while a graph is the active sidebar selection —
/// see `on_graph` in `crate::tui::event::home_preview`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum GraphLiveFocus {
    #[default]
    Graph,
    SpecStrip,
}
