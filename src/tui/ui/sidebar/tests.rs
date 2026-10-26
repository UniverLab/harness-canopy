//! `tests` tests (split from `src/tui/ui/sidebar.rs`, pure move).
use super::automation_section_floor_index;
use super::live_section_floor_index;
use super::*;
use std::sync::Arc;

fn needed_agents(count: u16) -> u16 {
    count * 4 + 2
}

/// Builds an App backed by a fresh temp DB with `project_count` registered
/// projects and one graph named "Probe Graph", then renders the sidebar into
/// a `width`x`height` TestBackend and returns the screen contents as a
/// flat string for substring assertions. The `Live` tab is active by
/// default (matching `App::new`) — use `render_sidebar_text_on_tab` to
/// assert on Automation's or Knowledge's body, since only the active
/// tab renders.
fn render_sidebar_text(project_count: usize, width: u16, height: u16) -> String {
    render_sidebar_text_themed(project_count, width, height, &Theme::classic())
}

fn render_sidebar_text_themed(
    project_count: usize,
    width: u16,
    height: u16,
    theme: &Theme,
) -> String {
    render_sidebar_text_on_tab(project_count, width, height, theme, SidebarLayer::Live)
}

fn render_sidebar_text_on_tab(
    project_count: usize,
    width: u16,
    height: u16,
    theme: &Theme,
    active_layer: SidebarLayer,
) -> String {
    render_sidebar_text_with(project_count, width, height, theme, active_layer, |_| {})
}

/// Like [`render_sidebar_text_on_tab`], but runs `prepare` on the App after
/// it is built and before the sidebar is drawn — for state the constructor
/// does not set up, such as an installed Brian's Brain.
fn render_sidebar_text_with(
    project_count: usize,
    width: u16,
    height: u16,
    theme: &Theme,
    active_layer: SidebarLayer,
    prepare: impl FnOnce(&mut App),
) -> String {
    use crate::db::Database;
    use crate::domain::graphs::{Graph, GraphStatus};
    use crate::domain::project::Project;
    use crate::tui::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    for i in 0..project_count {
        db.upsert_project(&Project {
            hash: format!("hash{i}"),
            path: format!("/tmp/project{i}"),
            name: format!("project{i}"),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 0,
        })
        .unwrap();
    }
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-probe".to_string(),
        name: "Probe Graph".to_string(),
        description: None,
        workdir: "/tmp/probe".to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();

    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.sidebar_layer = active_layer;
    assert!(
        !app.sidebar_graphs().is_empty(),
        "graph should be loaded from db"
    );
    prepare(&mut app);

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw_sidebar(frame, area, &mut app, theme);
        })
        .unwrap();

    buffer_to_text(terminal.backend().buffer())
}

/// Flattens a rendered `TestBackend` buffer into a plain string (row by
/// row, no trailing per-cell styling) for substring assertions.
fn buffer_to_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

/// A brain whose every cell is On, so its glyphs are deterministic instead
/// of depending on the automaton's random seed.
fn solid_brain(rows: usize, cols: usize) -> crate::tui::brians_brain::BriansBrain {
    use crate::tui::brians_brain::CellState;
    let mut brain = crate::tui::brians_brain::BriansBrain::new(rows, cols);
    for cell in brain.grid.iter_mut() {
        *cell = CellState::On;
    }
    brain
}

// ── Last-run sidebar meta (replaces the old done/total spec count) ──

/// Builds an App around a caller-supplied `Graph` plus whatever the `seed`
/// closure inserts into the same DB, renders the Automation tab into a
/// TestBackend, and returns the screen text. Unlike
/// `render_sidebar_text_on_tab` (a fixed `Running`, spec-less "Probe
/// Graph"), this lets each last-run test control the graph's status and
/// `graph_runs` history directly.
fn render_automation_text_for(
    lp: &crate::domain::graphs::Graph,
    seed: impl FnOnce(&crate::db::Database),
) -> String {
    use crate::db::Database;
    use crate::tui::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    db.insert_graph(lp).unwrap();
    seed(&db);

    let data_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.sidebar_layer = SidebarLayer::Automation;

    let backend = TestBackend::new(50, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw_sidebar(frame, area, &mut app, &Theme::classic());
        })
        .unwrap();

    buffer_to_text(terminal.backend().buffer())
}

fn bare_graph(id: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: format!("Graph {id}"),
        description: None,
        workdir: "/tmp/probe".to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    }
}

/// Records one node run against `graph_id` via a queue-driven spec — the
/// spec's own `graph_id` is `None` (never bound to this graph, so
/// `list_graph_specs(graph_id)` stays empty, exactly like a queue-driven
/// run), but `graph_runs.graph_id` is set, which is what the sidebar's
/// last-run query reads.
fn seed_queue_driven_run(
    db: &crate::db::Database,
    graph_id: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    output: Option<serde_json::Value>,
) {
    use crate::domain::graphs::{
        GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
    };

    let spec_id = format!("{graph_id}-spec");
    let node_id = format!("{graph_id}-node");
    db.insert_graph_spec(&GraphSpec {
        id: spec_id.clone(),
        graph_id: None,
        name: "queued spec".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Completed,
        started_at: Some(started_at),
        completed_at: Some(started_at),
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: node_id.clone(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "node".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 0,
        created_at: started_at,
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: format!("{graph_id}-run"),
        graph_id: graph_id.to_string(),
        spec_id,
        node_id,
        status: GraphRunStatus::Pass,
        input: None,
        output,
        started_at,
        completed_at: Some(started_at),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
}

/// Scans row `y` for the first cell matching one of `graph_status_icon`'s
/// glyphs, returning its `(symbol, fg color)` — used to check that every
/// status renders a visually distinct card without depending on the
/// inner panel's exact border offset.
fn status_icon_cell(buffer: &ratatui::buffer::Buffer, y: u16) -> (String, Color) {
    for x in 0..buffer.area.width {
        let cell = &buffer[(x, y)];
        if matches!(cell.symbol(), "▶" | "⏸" | "⛔" | "○" | "✓" | "✗") {
            return (cell.symbol().to_string(), cell.fg);
        }
    }
    panic!("no status icon glyph found in row {y}");
}

mod part01_tests;
mod part02_tests;
