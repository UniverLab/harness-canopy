//! Sidebar rendering — RAG (pinned top) → tab bar (Live / Automation /
//! Knowledge, exactly one visible at a time, full remaining height) →
//! sysinfo (pinned bottom).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use super::{
    borders_for, dialog_borders_for, last_two_segments, truncate_str, BG_HOVER, INTERACTIVE_COLOR,
};
use super::{
    STATUS_DISABLED, STATUS_FAIL, STATUS_OK, STATUS_RUNNING, STATUS_RUNNING_BRIGHT,
    STATUS_RUNNING_DIM, STATUS_WAIT_OFF, STATUS_WAIT_ON,
};
use crate::domain::graphs::{Graph, GraphStatus};
use crate::tui::agent::AgentStatus;
use crate::tui::app::types::{
    AgentEntry, AgentSectionFocus, App, AutomationKind, Focus, GraphSidebarMeta, SidebarLayer,
};
use ratatui::style::Color;

mod agent_cards;
mod knowledge_tab;
mod live_tab;
mod navigation;
mod project_graph;
mod rag;
mod shared_layout;

use agent_cards::*;
use knowledge_tab::*;
use live_tab::*;
use navigation::*;
use project_graph::*;
use rag::*;
use shared_layout::*;

pub(crate) use project_graph::draw_project_graph;
pub(crate) use shared_layout::draw_scroll_indicators;

pub(super) fn draw_sidebar(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    app.sidebar_click_map.clear();
    app.automation_graph_click_map.clear();
    app.project_click_map.clear();
    app.sidebar_tab_click_map.clear();
    app.sidebar_visible_capacity = 0;

    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(theme.sidebar_bg)),
        area,
    );

    let areas = split_sidebar_content(area, app, theme);

    let show_rag = app.rag_info.has_rag_activity() && areas.content.height >= 6;
    let (rag_area, content_below) = split_top_panel(areas.content, show_rag, 6);

    if let Some(rag_area) = rag_area.filter(|area| area.height >= 3) {
        render_titled_panel(
            frame,
            rag_area,
            rag_info_title(app),
            Style::default().fg(if is_rag_focused(app) {
                theme.header_color
            } else {
                theme.dim_text
            }),
            rag_border_style(app, theme),
            theme,
            |frame, inner| draw_rag_info(frame, inner, app, theme),
        );
    }

    let brain_area = draw_sidebar_tabs(frame, content_below, app, theme);
    let is_knowledge_tab = app.sidebar_layer == SidebarLayer::Knowledge;
    render_brain_or_graph(frame, brain_area, app, theme, is_knowledge_tab);

    render_dashboard_if_present(frame, areas.dashboard, app, theme);

    if let Some(dialog) = app.project_relation_dialog.as_ref() {
        draw_project_relation_dialog(frame, areas.content, app, dialog, theme);
    }
}

#[cfg(test)]
mod tests {
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
        let mut brain = crate::tui::brians_brain::BriansBrain::new(rows, cols, 60);
        for row in brain.grid.iter_mut() {
            for cell in row.iter_mut() {
                *cell = CellState::On;
            }
        }
        for row in brain.green_grid.iter_mut() {
            for green in row.iter_mut() {
                *green = 255;
            }
        }
        brain
    }

    /// The active tab's sub-sections are each capped at their own demand, so a
    /// tall sidebar holding one graph and no sessions leaves rows unclaimed.
    /// Those rows belong to the brain. `draw_sidebar_tabs` used to hand back a
    /// hardcoded zero-height rect instead, which made the brain unreachable no
    /// matter how `split_brain_or_graph` later divided it.
    #[test]
    fn unclaimed_tab_rows_are_handed_to_the_brain() {
        let text = render_sidebar_text_with(
            0,
            30,
            60,
            &Theme::classic(),
            SidebarLayer::Automation,
            |app| app.sidebar_brain = Some(solid_brain(40, 28)),
        );

        assert!(
            text.contains('\u{2588}') || text.contains('\u{28ff}'),
            "expected Brian's Brain glyphs in the sidebar's unclaimed rows, got:\n{text}"
        );
    }

    #[test]
    fn tab_bar_renders_every_layer_label() {
        // Wide enough that no tab cell needs to shorten its label to fit
        // (see `tab_cell_text`'s own narrow-width tests below).
        let text = render_sidebar_text(2, 45, 40);
        assert!(text.contains("Live"), "expected Live tab label");
        assert!(text.contains("Automation"), "expected Automation tab label");
        assert!(text.contains("Knowledge"), "expected Knowledge tab label");
        assert!(
            !text.contains("Knowledge (2)"),
            "tabs carry the label alone, no item count: {text}"
        );
    }

    #[test]
    fn sidebar_has_tab_to_list_gap() {
        use crate::db::Database;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let theme = Theme::modern();
        let backend = TestBackend::new(45, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar_tabs(frame, area, &mut app, &theme);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!((0..buffer.area.width)
            .all(|x| { buffer[(x, 1)].symbol() == " " && buffer[(x, 1)].bg == theme.sidebar_bg }));
    }

    #[test]
    fn sidebar_has_gaps_between_all_sections() {
        use crate::db::Database;
        use crate::tui::agent::InteractiveAgent;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        // Seed Live with one interactive and one terminal so both sub-panels allocate.
        let mut interactive = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("interactive-1"),
            &[],
            Color::White,
        )
        .expect("spawn interactive");
        interactive.status = crate::tui::agent::AgentStatus::Running;
        app.interactive_agents.push(interactive);
        let mut terminal = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("terminal-1"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        terminal.status = crate::tui::agent::AgentStatus::Running;
        app.terminal_agents.push(terminal);
        app.agents.push(AgentEntry::Interactive(0));
        app.agents.push(AgentEntry::Terminal(0));
        app.sidebar_layer = SidebarLayer::Live;

        let theme = Theme::modern();
        let backend = TestBackend::new(45, 40);
        let mut terminal_backend = Terminal::new(backend).unwrap();
        terminal_backend
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar_tabs(frame, area, &mut app, &theme);
            })
            .unwrap();
        let buffer = terminal_backend.backend().buffer();
        // Tab bar is y=0, gap at y=1 must be blank sidebar_bg.
        assert!((0..buffer.area.width)
            .all(|x| { buffer[(x, 1)].symbol() == " " && buffer[(x, 1)].bg == theme.sidebar_bg }));
        // After the first titled panel (interactive) there must be another blank gap
        // before the terminal panel, and likewise between terminal and groups.
        // Count blank rows inside the Live body to ensure the rule is applied between
        // every section, not just tabs->list.
        let blank_gap_rows = (0..buffer.area.height)
            .filter(|&y| {
                (0..buffer.area.width).all(|x| {
                    buffer[(x, y)].symbol() == " " && buffer[(x, y)].bg == theme.sidebar_bg
                })
            })
            .count();
        assert!(
            blank_gap_rows >= 3,
            "expected at least 3 breathing gaps (tab->interactive, interactive->terminal, terminal->groups), found {blank_gap_rows}"
        );
    }

    #[test]
    fn every_tab_label_fits_uncut_at_the_real_sidebar_width() {
        // The reason the count is gone: 33 columns / 3 tabs leaves 11 per
        // cell, which fits "Automation" and "Knowledge" but not either of
        // them followed by a count.
        let cell = super::super::SIDEBAR_WIDTH as usize / SIDEBAR_TABS.len();
        for layer in SIDEBAR_TABS {
            let label = layer_label(layer);
            assert!(
                !tab_cell_text(label, cell).contains('…'),
                "{label} must not be truncated at {cell} columns"
            );
        }
    }

    #[test]
    fn automation_layer_shows_running_graph() {
        let text =
            render_sidebar_text_on_tab(1, 45, 40, &Theme::classic(), SidebarLayer::Automation);
        assert!(text.contains("Probe Graph"), "expected graph name visible");
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

    #[test]
    fn queue_driven_graph_shows_last_run_time_not_zero_zero() {
        let started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let text = render_automation_text_for(&bare_graph("q1", GraphStatus::Draft), |db| {
            seed_queue_driven_run(db, "q1", started_at, None);
        });
        assert!(
            !text.contains("0/0"),
            "queue-driven graph must not fall back to the old done/total count: {text}"
        );
        assert!(
            text.contains("2m"),
            "expected the last run's relative time (2m) in the sidebar: {text}"
        );
    }

    #[test]
    fn never_run_graph_shows_plainly_not_blank_or_zero() {
        let text = render_automation_text_for(&bare_graph("never1", GraphStatus::Draft), |_db| {});
        assert!(
            !text.contains("0/0"),
            "a never-run graph must not show a misleading zero: {text}"
        );
        assert!(
            text.contains("never"),
            "a never-run graph must say so plainly: {text}"
        );
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

    #[test]
    fn sidebar_lists_every_status_ordered_by_recency_and_visually_distinguishable() {
        use crate::db::Database;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());

        // Staggered `created_at`, oldest first, so recency ordering is
        // unambiguous once sorted (none of these graphs have run, so
        // `last_activity` falls back to `created_at`).
        let base = chrono::Utc::now() - chrono::Duration::hours(10);
        let mut draft = bare_graph("s-draft", GraphStatus::Draft);
        draft.created_at = base;
        db.insert_graph(&draft).unwrap();

        let mut failed = bare_graph("s-failed", GraphStatus::Failed);
        failed.created_at = base + chrono::Duration::minutes(10);
        failed.autorun_at = Some(chrono::Utc::now() + chrono::Duration::minutes(20));
        db.insert_graph(&failed).unwrap();

        let mut completed = bare_graph("s-completed", GraphStatus::Completed);
        completed.created_at = base + chrono::Duration::minutes(20);
        db.insert_graph(&completed).unwrap();

        let mut paused = bare_graph("s-paused", GraphStatus::Paused);
        paused.created_at = base + chrono::Duration::minutes(30);
        db.insert_graph(&paused).unwrap();

        let mut running = bare_graph("s-running", GraphStatus::Running);
        running.created_at = base + chrono::Duration::minutes(40);
        db.insert_graph(&running).unwrap();

        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = AutomationKind::Graph;

        let theme = Theme::classic();
        let backend = TestBackend::new(50, 60);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar(frame, area, &mut app, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = buffer_to_text(&buffer);

        for name in [
            "Graph s-draft",
            "Graph s-failed",
            "Graph s-completed",
            "Graph s-paused",
            "Graph s-running",
        ] {
            assert!(text.contains(name), "expected {name} listed: {text}");
        }

        let click_map = app.automation_graph_click_map.clone();
        let ids: Vec<&str> = click_map.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "s-running",
                "s-paused",
                "s-completed",
                "s-failed",
                "s-draft",
            ],
            "every status must be listed, ordered by last activity most recent first"
        );

        let icons: Vec<(String, Color)> = click_map
            .iter()
            .map(|(_, y_start, _)| status_icon_cell(&buffer, *y_start))
            .collect();
        let unique: std::collections::HashSet<_> = icons.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            icons.len(),
            "each of the five statuses must render a distinct icon/color pair: {icons:?}"
        );

        assert!(
            text.contains("resumes"),
            "a pending autorun must be visible on its graph's sidebar entry: {text}"
        );
    }

    #[test]
    fn running_graph_shows_running_not_a_relative_time() {
        let started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
        let text = render_automation_text_for(&bare_graph("run1", GraphStatus::Running), |db| {
            seed_queue_driven_run(db, "run1", started_at, None);
        });
        assert!(
            text.contains("running"),
            "an actively-running graph must say 'running', not its stale last-run time: {text}"
        );
    }

    #[test]
    fn blocked_indicator_still_renders_for_paused_graph_with_blocker() {
        let started_at = chrono::Utc::now() - chrono::Duration::minutes(5);
        let text = render_automation_text_for(&bare_graph("blocked1", GraphStatus::Paused), |db| {
            seed_queue_driven_run(
                db,
                "blocked1",
                started_at,
                Some(serde_json::json!({"blocker": "waiting on human input"})),
            );
        });
        assert!(
            text.contains('⛔'),
            "a paused graph with a reported blocker must still show the blocked icon: {text}"
        );
    }

    #[test]
    fn inactive_tabs_bodies_are_not_rendered() {
        // The whole point of tabs over stacked layers: exactly one body is
        // visible at a time. A project ("only-in-knowledge") and the
        // always-present "Probe Graph" (Automation) must not leak into the
        // Live tab's render, and vice versa.
        let live_text =
            render_sidebar_text_on_tab(1, 45, 40, &Theme::classic(), SidebarLayer::Live);
        assert!(
            !live_text.contains("Probe Graph"),
            "Automation's body must not render while Live is active"
        );
        assert!(
            !live_text.contains("project0"),
            "Knowledge's body must not render while Live is active"
        );

        let knowledge_text =
            render_sidebar_text_on_tab(1, 45, 40, &Theme::classic(), SidebarLayer::Knowledge);
        assert!(
            knowledge_text.contains("project0"),
            "Knowledge's body must render while Knowledge is active"
        );
        assert!(
            !knowledge_text.contains("Probe Graph"),
            "Automation's body must not render while Knowledge is active"
        );
    }

    #[test]
    fn modern_theme_drops_border_glyphs_where_classic_shows_them() {
        // T5: a modern-rendered sidebar must have no box-drawing border glyphs,
        // while the classic-rendered sidebar must still show them — the
        // background contrast alone is expected to separate panels.
        let border_glyphs = ['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'];
        let assert_no_borders = |label: &str, text: &str| {
            for glyph in border_glyphs {
                assert!(
                    !text.contains(glyph),
                    "{label} render unexpectedly contains border glyph {glyph:?}\n--- text ---\n{text}"
                );
            }
        };

        let classic_text = render_sidebar_text_themed(1, 45, 40, &Theme::classic());
        assert!(
            border_glyphs.iter().any(|g| classic_text.contains(*g)),
            "classic render should still draw box-drawing borders\n--- text ---\n{classic_text}"
        );

        let modern_text = render_sidebar_text_themed(1, 45, 40, &Theme::modern());
        assert_no_borders("modern", &modern_text);
        // And the labels that the modern sidebar still owes the user must
        // survive (sanity: we didn't accidentally turn the whole area blank).
        assert!(
            modern_text.contains("Live"),
            "modern must keep the Live label"
        );
        assert!(
            modern_text.contains("Automation"),
            "modern must keep the Automation label"
        );
    }

    #[test]
    fn old_top_level_backlog_knowledge_history_sections_are_gone() {
        // T-regression: these used to be top-level sidebar sections; they now
        // only exist inside a project's Focus tab bar.
        let text = render_sidebar_text(1, 34, 40);
        assert!(
            !text.contains(" backlog "),
            "backlog must not be a top-level section"
        );
        assert!(
            !text.contains(" history "),
            "history must not be a top-level section"
        );
    }

    #[test]
    fn drawing_the_knowledge_layer_populates_project_click_map() {
        // T-regression: `draw_projects_list`/`draw_knowledge_body` used to
        // take `&App`, so nothing ever pushed into `project_click_map` and a
        // mouse click on a sidebar project row silently did nothing —
        // functional requirement 4 requires project rows to be clickable.
        use crate::db::Database;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        for i in 0..2 {
            db.upsert_project(&crate::domain::project::Project {
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

        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        assert_eq!(app.projects.len(), 2, "projects should be loaded from db");
        // Only the active tab's body renders now — Knowledge must be active
        // for its project rows to draw at all.
        app.sidebar_layer = SidebarLayer::Knowledge;

        let backend = TestBackend::new(34, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar(frame, area, &mut app, &Theme::classic());
            })
            .unwrap();

        assert_eq!(
            app.project_click_map.len(),
            2,
            "each rendered project row must register a click region: {:?}",
            app.project_click_map
        );
        let indices: Vec<usize> = app
            .project_click_map
            .iter()
            .map(|&(idx, _, _)| idx)
            .collect();
        assert!(indices.contains(&0));
        assert!(indices.contains(&1));
    }

    #[test]
    fn fair_section_heights_caps_small_sections_at_their_demand() {
        // background: 1 agent (6), interactive: 6 agents (26), terminal: 1 (6).
        let demands = [needed_agents(1), needed_agents(6), needed_agents(1)];
        let total: u16 = demands.iter().sum();
        assert_eq!(total, 38);

        let alloc = fair_section_heights(&demands, 30, None, 0);

        // No section is ever allocated more than it needs.
        for (got, want) in alloc.iter().zip(demands.iter()) {
            assert!(got <= want, "section {got} exceeded its demand {want}");
        }
        // The two small sections get exactly what they need (no empty gap).
        assert_eq!(alloc[0], demands[0]);
        assert_eq!(alloc[2], demands[2]);
        // Interactive absorbs all the surplus and scrolls internally.
        assert_eq!(alloc[1], 30 - demands[0] - demands[2]);
        // Demand exceeds the budget here, so every row is spoken for and
        // nothing is left over for the brain.
        assert_eq!(alloc.iter().sum::<u16>(), 30);
    }

    #[test]
    fn fair_section_heights_never_exceeds_demand_with_four_sections() {
        let demands = [needed_agents(1), needed_agents(8), needed_agents(1), 4];
        let budget = 24;
        assert!(demands.iter().sum::<u16>() > budget);

        let alloc = fair_section_heights(&demands, budget, None, 0);

        for (got, want) in alloc.iter().zip(demands.iter()) {
            assert!(got <= want, "section {got} exceeded its demand {want}");
        }
        assert_eq!(alloc.iter().sum::<u16>(), budget);
    }

    #[test]
    fn fair_section_heights_fits_everyone_when_budget_is_ample() {
        let demands = [needed_agents(1), needed_agents(2)];
        let alloc = fair_section_heights(&demands, 100, None, 0);
        assert_eq!(alloc[0], demands[0]);
        assert_eq!(alloc[1], demands[1]);
    }

    #[test]
    fn running_session_with_recent_activity_is_working_green() {
        assert_eq!(
            session_status_color(&AgentStatus::Running, true, 0),
            pulse_active(0)
        );
        // Neither pulse phase may be blank — the indicator must never
        // disappear (B21).
        assert_ne!(pulse_active(0), super::super::STATUS_WAIT_OFF);
        assert_ne!(pulse_active(10), super::super::STATUS_WAIT_OFF);
        assert_ne!(pulse_active(0), pulse_active(10));
    }

    #[test]
    fn running_session_gone_quiet_is_healthy_idle_green() {
        assert_eq!(
            session_status_color(&AgentStatus::Running, false, 0),
            STATUS_RUNNING
        );
    }

    #[test]
    fn exit_error_wins_over_activity() {
        assert_eq!(
            session_status_color(&AgentStatus::Exited(1), true, 0),
            STATUS_FAIL
        );
        assert_eq!(
            session_status_color(&AgentStatus::Exited(1), false, 0),
            STATUS_FAIL
        );
    }

    #[test]
    fn exit_clean_is_success_blue_regardless_of_activity() {
        assert_eq!(
            session_status_color(&AgentStatus::Exited(0), true, 0),
            STATUS_OK
        );
        assert_eq!(
            session_status_color(&AgentStatus::Exited(0), false, 0),
            STATUS_OK
        );
    }

    #[test]
    fn exit_nonzero_is_failure_red_regardless_of_activity() {
        assert_eq!(
            session_status_color(&AgentStatus::Exited(7), true, 0),
            STATUS_FAIL
        );
        assert_eq!(
            session_status_color(&AgentStatus::Exited(7), false, 0),
            STATUS_FAIL
        );
    }

    #[test]
    fn exited_0_and_exited_1_sidebar_cards_render_different_colors() {
        // The point of the fix is contrast: a clean exit and a failed exit
        // sitting side by side must be visually distinguishable, not just
        // individually "correct" in isolation.
        use crate::db::Database;
        use crate::tui::agent::InteractiveAgent;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();

        let mut ok_agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("ok-exit"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn ok-exit agent");
        ok_agent.status = AgentStatus::Exited(0);
        app.interactive_agents.push(ok_agent);

        let mut fail_agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("fail-exit"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn fail-exit agent");
        fail_agent.status = AgentStatus::Exited(1);
        app.interactive_agents.push(fail_agent);

        app.agents.push(AgentEntry::Interactive(0));
        app.agents.push(AgentEntry::Interactive(1));

        let theme = Theme::classic();
        let backend = TestBackend::new(50, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar(frame, area, &mut app, &theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let text = buffer_to_text(&buffer);

        // Scan the row containing each agent's name for the "▌" status
        // gutter glyph, wherever the card's left border happens to sit.
        let gutter_color_for = |name: &str| -> Color {
            let y = text
                .lines()
                .position(|line| line.contains(name))
                .unwrap_or_else(|| panic!("expected {name} listed: {text}"));
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y as u16)];
                if cell.symbol() == "▌" {
                    return cell.fg;
                }
            }
            panic!("no status gutter glyph found on {name}'s row");
        };

        let ok_color = gutter_color_for("ok-exit");
        let fail_color = gutter_color_for("fail-exit");
        assert_eq!(ok_color, STATUS_OK, "clean exit must render success blue");
        assert_eq!(fail_color, STATUS_FAIL, "failed exit must render fail red");
        assert_ne!(
            ok_color, fail_color,
            "clean and failed exits must be visually distinguishable"
        );
    }

    #[test]
    fn terminal_running_a_command_pulses_never_blank() {
        // A foreground command is executing → pulse through both phases,
        // ignoring output activity entirely.
        let bright = pty_session_status_color(true, &AgentStatus::Running, false, true, 0);
        let dim = pty_session_status_color(true, &AgentStatus::Running, false, true, 10);
        assert_eq!(bright, super::super::STATUS_RUNNING_BRIGHT);
        assert_eq!(dim, super::super::STATUS_RUNNING_DIM);
        // Never blank across the cycle (B21).
        assert_ne!(bright, super::super::STATUS_WAIT_OFF);
        assert_ne!(dim, super::super::STATUS_WAIT_OFF);
        assert_ne!(bright, dim);
    }

    #[test]
    fn terminal_idle_at_prompt_is_solid_even_right_after_output() {
        // No foreground command, but output just scrolled by (recent_activity
        // true): a terminal must NOT keep pulsing — it's solid green.
        assert_eq!(
            pty_session_status_color(true, &AgentStatus::Running, true, false, 0),
            STATUS_RUNNING
        );
        assert_eq!(
            pty_session_status_color(true, &AgentStatus::Running, true, false, 10),
            STATUS_RUNNING
        );
    }

    #[test]
    fn terminal_ignores_output_activity_command_execution_wins() {
        // Command executing but no recent output (e.g. a blocking `sleep`):
        // still pulsing. Output present but command finished: solid.
        assert_eq!(
            pty_session_status_color(true, &AgentStatus::Running, false, true, 0),
            pulse_active(0)
        );
        assert_eq!(
            pty_session_status_color(true, &AgentStatus::Running, true, false, 0),
            STATUS_RUNNING
        );
    }

    #[test]
    fn interactive_agent_still_pulses_on_output_activity() {
        // Interactive kind keeps the activity-based behavior unchanged: the
        // command_executing signal is ignored for it.
        assert_eq!(
            pty_session_status_color(false, &AgentStatus::Running, true, false, 0),
            pulse_active(0)
        );
        assert_eq!(
            pty_session_status_color(false, &AgentStatus::Running, false, true, 0),
            STATUS_RUNNING
        );
        // Exited status ignores the pulsing signal either way, but now
        // depends on the exit code: clean exit is blue, not red.
        assert_eq!(
            pty_session_status_color(false, &AgentStatus::Exited(0), true, false, 0),
            STATUS_OK
        );
        assert_eq!(
            pty_session_status_color(false, &AgentStatus::Exited(1), true, false, 0),
            STATUS_FAIL
        );
    }

    #[test]
    fn layer_label_all_variants() {
        assert_eq!(layer_label(SidebarLayer::Live), "Live");
        assert_eq!(layer_label(SidebarLayer::Automation), "Automation");
        assert_eq!(layer_label(SidebarLayer::Knowledge), "Knowledge");
    }

    #[test]
    fn tab_cell_text_centers_the_label_when_there_is_room() {
        // 20 columns, 10-char label → 5 columns of padding on each side.
        assert_eq!(tab_cell_text("Automation", 20), "     Automation     ");
    }

    #[test]
    fn tab_cell_text_odd_slack_gives_the_extra_column_to_the_right() {
        // 11 columns, 4-char label ("Live") → 7 slack columns, split 3/4.
        assert_eq!(tab_cell_text("Live", 11), "   Live    ");
    }

    #[test]
    fn tab_cell_text_truncates_a_label_too_wide_for_its_cell() {
        let text = tab_cell_text("Automation", 6);
        assert_eq!(text, "Autom…");
    }

    #[test]
    fn tab_cell_text_extreme_narrow_width_still_fits_exactly() {
        let text = tab_cell_text("Knowledge", 3);
        assert_eq!(text.chars().count(), 3);
    }

    #[test]
    fn draw_sidebar_tab_bar_registers_three_equal_click_cells_spanning_the_full_width() {
        use crate::db::Database;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();

        let backend = TestBackend::new(33, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar(frame, area, &mut app, &Theme::classic());
            })
            .unwrap();

        assert_eq!(app.sidebar_tab_click_map.len(), 3);
        let layers: Vec<SidebarLayer> = app
            .sidebar_tab_click_map
            .iter()
            .map(|&(layer, _, _, _)| layer)
            .collect();
        assert_eq!(
            layers,
            vec![
                SidebarLayer::Live,
                SidebarLayer::Automation,
                SidebarLayer::Knowledge
            ]
        );
        // The three cells must tile the row with no gap and no overlap.
        let first_col = app.sidebar_tab_click_map[0].2;
        let last_col_end = app.sidebar_tab_click_map[2].3;
        assert_eq!(last_col_end - first_col, 33);
        for pair in app.sidebar_tab_click_map.windows(2) {
            assert_eq!(pair[0].3, pair[1].2, "cells must be contiguous");
        }
    }

    #[test]
    fn draw_sidebar_tab_bar_marks_the_active_tab_with_accent_bold_and_no_fill() {
        use crate::db::Database;
        use crate::tui::app::App;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.sidebar_layer = SidebarLayer::Live;

        let theme = Theme::classic();
        let backend = TestBackend::new(33, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_sidebar(frame, area, &mut app, &theme);
            })
            .unwrap();

        let (_, row, start, end) = app.sidebar_tab_click_map[0];
        let buffer = terminal.backend().buffer().clone();
        let mut saw_accent_glyph = false;
        for x in start..end {
            let cell = &buffer[(x, row)];
            assert_eq!(
                cell.bg,
                Color::Reset,
                "active tab cell must not fill its background"
            );
            if cell.symbol() != " " {
                assert_eq!(cell.fg, theme.header_color);
                assert!(cell.modifier.contains(Modifier::BOLD));
                saw_accent_glyph = true;
            }
        }
        assert!(saw_accent_glyph, "expected the active label to be drawn");

        let (_, row, start, end) = app.sidebar_tab_click_map[1];
        for x in start..end {
            let cell = &buffer[(x, row)];
            assert_eq!(cell.bg, Color::Reset);
            if cell.symbol() != " " {
                assert_eq!(cell.fg, theme.dim_text);
                assert!(!cell.modifier.contains(Modifier::BOLD));
            }
        }
    }

    #[test]
    fn fair_section_heights_empty() {
        assert_eq!(fair_section_heights(&[], 20, None, 0), Vec::<u16>::new());
    }

    #[test]
    fn fair_section_heights_single_section() {
        assert_eq!(fair_section_heights(&[10], 20, None, 0), vec![10]);
    }

    #[test]
    fn fair_section_heights_budget_exactly_matches_demand() {
        assert_eq!(fair_section_heights(&[5, 5, 5], 15, None, 0), vec![5, 5, 5]);
    }

    #[test]
    fn fair_section_heights_budget_exceeds_demand() {
        assert_eq!(fair_section_heights(&[3, 3], 20, None, 0), vec![3, 3]);
    }

    #[test]
    fn fair_section_heights_budget_falls_short() {
        let result = fair_section_heights(&[10, 10], 10, None, 0);
        assert_eq!(result.iter().sum::<u16>(), 10);
    }

    #[test]
    fn fair_section_heights_one_small_one_large() {
        let result = fair_section_heights(&[2, 20], 12, None, 0);
        assert_eq!(result[0], 2);
        assert_eq!(result[1], 10);
        assert_eq!(result.iter().sum::<u16>(), 12);
    }

    #[test]
    fn fair_section_heights_budget_zero() {
        assert_eq!(fair_section_heights(&[10, 10], 0, None, 0), vec![0, 0]);
    }

    #[test]
    fn fair_section_heights_many_sections() {
        let result = fair_section_heights(&[5, 5, 5, 5, 5], 10, None, 0);
        assert_eq!(result.iter().sum::<u16>(), 10);
        for &v in &result {
            assert!(v <= 5);
        }
    }

    #[test]
    fn fair_section_heights_focused_section_gets_floor_on_short_screen() {
        // Short screen: three sections, each wanting far more than a fair
        // share of the budget. Plain fair distribution splits it roughly
        // evenly and leaves every section below the floor needed to show
        // the cursor -- "I can navigate them but I can't see them".
        let demands = [needed_agents(5), needed_agents(5), needed_agents(5)];
        let budget = 15;

        // Precondition: without a focused section the section the user is
        // looking at is starved below the floor. If this ever stops being
        // true the test below no longer proves the floor does anything.
        let fair = fair_section_heights(&demands, budget, None, 0);
        assert!(
            fair[0] < 6,
            "precondition: fair split should starve the section, got {}",
            fair[0]
        );

        // Focusing that section guarantees it the floor before fair
        // distribution runs for the rest.
        let alloc = fair_section_heights(&demands, budget, Some(0), 6);
        assert!(
            alloc[0] >= 6,
            "focused section got {} rows, expected >= 6",
            alloc[0]
        );

        // Still within budget, still nobody over their demand.
        assert!(alloc.iter().sum::<u16>() <= budget);
        for (got, want) in alloc.iter().zip(demands.iter()) {
            assert!(got <= want, "section got {got} exceeded demand {want}");
        }
    }

    #[test]
    fn fair_section_heights_floor_respects_demand() {
        let demands = [4, 20, 20];
        let alloc = fair_section_heights(&demands, 30, Some(0), 6);
        assert_eq!(alloc[0], 4);
        assert!(alloc.iter().sum::<u16>() <= 30);
    }

    #[test]
    fn fair_section_heights_floor_respects_budget() {
        let demands = [20, 20];
        let alloc = fair_section_heights(&demands, 3, Some(0), 6);
        assert!(alloc[0] <= 3);
        assert!(alloc.iter().sum::<u16>() <= 3);
    }

    #[test]
    fn fair_section_heights_no_focus_unchanged() {
        // With no focused section and a zero floor the function must
        // behave exactly as it did before the floor was added: the small
        // sections cap at their demand, the large one takes the rest.
        let demands = [needed_agents(6), needed_agents(1), groups_list_demand(1)];
        assert_eq!(fair_section_heights(&demands, 20, None, 0), vec![10, 6, 4]);
    }

    #[test]
    fn fair_section_heights_focus_no_effect_when_budget_is_ample() {
        // Tall screen: everything fits. The focused-section floor must not
        // change what any section gets -- no visible difference from the
        // pre-floor behaviour on screens with sufficient height.
        let demands = [needed_agents(3), needed_agents(2), groups_list_demand(2)];
        let ample = demands.iter().sum::<u16>() + 10;
        assert_eq!(
            fair_section_heights(&demands, ample, Some(0), 6),
            fair_section_heights(&demands, ample, None, 0),
        );
        assert_eq!(
            fair_section_heights(&demands, ample, Some(0), 6),
            demands.to_vec(),
        );
    }

    #[test]
    fn card_list_demand_empty() {
        assert_eq!(card_list_demand(0), 0);
    }

    #[test]
    fn card_list_demand_one() {
        assert_eq!(card_list_demand(1), 6);
    }

    #[test]
    fn card_list_demand_many() {
        assert_eq!(card_list_demand(5), 22);
    }

    #[test]
    fn groups_list_demand_empty() {
        assert_eq!(groups_list_demand(0), 0);
    }

    #[test]
    fn groups_list_demand_one() {
        assert_eq!(groups_list_demand(1), 4);
    }

    #[test]
    fn groups_list_demand_many() {
        assert_eq!(groups_list_demand(3), 8);
    }

    #[test]
    fn scroll_state_no_items() {
        let s = scroll_state(0, None, 5);
        assert_eq!(s.start, 0);
        assert!(!s.has_up);
        assert!(!s.has_down);
    }

    #[test]
    fn scroll_state_fewer_items_than_visible() {
        let s = scroll_state(3, None, 10);
        assert_eq!(s.start, 0);
        assert!(!s.has_up);
        assert!(!s.has_down);
    }

    #[test]
    fn scroll_state_selected_below_visible() {
        let s = scroll_state(20, Some(15), 5);
        assert_eq!(s.start, 11);
        assert!(s.has_up);
        assert!(s.has_down);
    }

    #[test]
    fn scroll_state_selected_at_top() {
        let s = scroll_state(20, Some(0), 5);
        assert_eq!(s.start, 0);
        assert!(!s.has_up);
        assert!(s.has_down);
    }

    #[test]
    fn scroll_state_selected_at_end() {
        let s = scroll_state(20, Some(19), 5);
        assert_eq!(s.start, 15);
        assert!(s.has_up);
        assert!(!s.has_down);
    }

    #[test]
    fn scroll_state_with_offset_shifts_start() {
        let s = scroll_state_with_offset(20, Some(0), 5, 3);
        assert_eq!(s.start, 3);
        assert!(s.has_up);
    }

    #[test]
    fn scroll_state_with_offset_clamped_to_max() {
        let s = scroll_state_with_offset(20, Some(19), 5, 100);
        assert_eq!(s.start, 15);
    }

    #[test]
    fn render_sidebar_card_line_short_area() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(20, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 20, 1);
                render_sidebar_card_line(
                    frame,
                    area,
                    0,
                    Color::Reset,
                    STATUS_OK,
                    vec![Span::raw("test")],
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, 0)].symbol());
        }
        assert!(text.contains("test"));
    }

    #[test]
    fn render_sidebar_card_line_beyond_height() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(20, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 20, 2);
                // line_offset=2 is beyond height, should not panic
                render_sidebar_card_line(
                    frame,
                    area,
                    2,
                    Color::Reset,
                    STATUS_OK,
                    vec![Span::raw("test")],
                );
            })
            .unwrap();
    }

    #[test]
    fn group_row_style_selected_and_active() {
        let theme = Theme::classic();
        let style = group_row_style(true, true, &theme);
        assert_eq!(style.bg, theme.selected_bg);
        assert_eq!(style.fg, theme.header_color);
        assert!(style.modifier.contains(Modifier::BOLD));
        assert_eq!(style.prefix_color, Color::Green);
        assert_eq!(style.active_tag, " ●");
    }

    #[test]
    fn group_row_style_not_selected_not_active() {
        let theme = Theme::classic();
        let style = group_row_style(false, false, &theme);
        assert_eq!(style.bg, Color::Reset);
        assert_eq!(style.fg, Color::White);
        assert!(style.modifier.is_empty());
        assert_eq!(style.prefix_color, theme.dim_text);
        assert_eq!(style.active_tag, "");
    }

    #[test]
    fn group_row_style_selected_not_active() {
        let theme = Theme::classic();
        let style = group_row_style(true, false, &theme);
        assert_eq!(style.bg, theme.selected_bg);
        assert_eq!(style.fg, theme.header_color);
        assert!(style.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn group_row_style_active_not_selected() {
        let theme = Theme::classic();
        let style = group_row_style(false, true, &theme);
        assert_eq!(style.bg, Color::Reset);
        assert_eq!(style.fg, Color::Green);
        assert!(style.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn project_title_style_selected_and_focused() {
        let theme = Theme::classic();
        let style = project_title_style(true, true, &theme);
        assert_eq!(style.bg, Some(theme.header_color));
        assert_eq!(style.fg, Some(Color::Black));
    }

    #[test]
    fn project_title_style_selected_not_focused() {
        let theme = Theme::classic();
        let style = project_title_style(true, false, &theme);
        assert_eq!(style.bg, Some(theme.selected_bg));
        assert_eq!(style.fg, Some(Color::White));
    }

    #[test]
    fn project_title_style_not_selected() {
        let theme = Theme::classic();
        let style = project_title_style(false, false, &theme);
        assert_eq!(style.bg, None);
        assert_eq!(style.fg, Some(theme.header_color));
    }

    #[test]
    fn project_meta_style_selected() {
        let theme = Theme::classic();
        let style = project_meta_style(true, &theme);
        assert_eq!(style.fg, Some(Color::White));
        assert_eq!(style.bg, Some(theme.selected_bg));
    }

    #[test]
    fn project_meta_style_not_selected() {
        let theme = Theme::classic();
        let style = project_meta_style(false, &theme);
        assert_eq!(style.fg, Some(theme.dim_text));
    }

    #[test]
    fn rag_info_title_when_paused() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_paused = true;
        assert_eq!(rag_info_title(&app), " ragInfo ⏸ ");
    }

    #[test]
    fn rag_info_title_when_not_paused() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_paused = false;
        assert_eq!(rag_info_title(&app), " ragInfo ");
    }

    #[test]
    fn rag_queue_text_with_items() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_info.queued_items = 5;
        assert_eq!(rag_queue_text(&app), "5 queued");
    }

    #[test]
    fn rag_queue_text_zero_items() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_info.queued_items = 0;
        assert_eq!(rag_queue_text(&app), "");
    }

    #[test]
    fn pulse_active_cycles_through_phases() {
        let a = pulse_active(0);
        let b = pulse_active(10);
        assert_ne!(a, b);
        assert_ne!(a, super::super::STATUS_WAIT_OFF);
        assert_ne!(b, super::super::STATUS_WAIT_OFF);
    }

    #[test]
    fn pulse_cycles_through_phases() {
        let a = pulse(Color::Red, 0);
        let b = pulse(Color::Red, 10);
        assert_ne!(a, b);
    }

    #[test]
    fn draw_sidebar_renders_without_panic() {
        use crate::db::Database;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let backend = TestBackend::new(33, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_sidebar(frame, frame.area(), &mut app, &Theme::classic());
            })
            .unwrap();
    }

    #[test]
    fn draw_sidebar_modern_theme_no_panic() {
        use crate::db::Database;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let backend = TestBackend::new(33, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_sidebar(frame, frame.area(), &mut app, &Theme::modern());
            })
            .unwrap();
    }

    #[test]
    fn rag_status_line_shows_downloading_not_ready_even_when_loaded_flag_is_stale() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_embeddings_model = "text-embedding-3-small".to_string();
        app.rag_model_loaded = true;
        app.rag_acquisition_state =
            Some(crate::rag::status::AcquisitionState::Downloading { started_at: 0 });

        let theme = Theme::classic();
        let line = rag_status_line(&app, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("downloading"), "got: {text:?}");
    }

    #[test]
    fn rag_status_line_shows_download_failed() {
        use crate::db::Database;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_embeddings_model = "text-embedding-3-small".to_string();
        app.rag_acquisition_state = Some(crate::rag::status::AcquisitionState::Failed {
            reason: "boom".to_string(),
        });

        let theme = Theme::classic();
        let line = rag_status_line(&app, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("download failed"), "got: {text:?}");
    }

    #[test]
    fn split_shares_space_when_both_fit() {
        let area = Rect::new(0, 0, 30, 20);
        match split_brain_or_graph(area, true) {
            BrainOrGraphLayout::Both { graph, brain } => {
                assert!(graph.height >= GRAPH_MIN_HEIGHT, "graph below its minimum");
                assert!(brain.height >= 3, "brain below its minimum");
                assert_eq!(
                    graph.height + brain.height,
                    area.height,
                    "the two sub-areas should cover the whole leftover area"
                );
                assert_eq!(
                    brain.y,
                    graph.y + graph.height,
                    "brain should sit below the graph"
                );
            }
            other => panic!("expected Both, got {other:?}"),
        }
    }

    #[test]
    fn split_gives_graph_alone_when_only_it_fits() {
        // Tall enough for the graph (>=4) but not for graph + brain (>=7).
        let area = Rect::new(0, 0, 30, 5);
        match split_brain_or_graph(area, true) {
            BrainOrGraphLayout::GraphOnly(graph) => assert_eq!(graph, area),
            other => panic!("expected GraphOnly, got {other:?}"),
        }
    }

    #[test]
    fn split_gives_brain_alone_when_graph_is_empty() {
        // Below the graph's own minimum, but that's moot since it has nothing to show.
        let area = Rect::new(0, 0, 30, 3);
        match split_brain_or_graph(area, false) {
            BrainOrGraphLayout::BrainOnly(brain) => assert_eq!(brain, area),
            other => panic!("expected BrainOnly, got {other:?}"),
        }
    }

    #[test]
    fn split_yields_neither_below_both_minimums() {
        let area = Rect::new(0, 0, 30, 2);
        assert_eq!(
            split_brain_or_graph(area, true),
            BrainOrGraphLayout::Neither
        );
    }

    #[test]
    fn split_yields_neither_on_zero_height() {
        let area = Rect::new(0, 0, 30, 0);
        assert_eq!(
            split_brain_or_graph(area, true),
            BrainOrGraphLayout::Neither
        );
        assert_eq!(
            split_brain_or_graph(area, false),
            BrainOrGraphLayout::Neither
        );
    }

    // ── C26: graph_has_content / graph_edge_row_budget ────────────────

    #[test]
    fn graph_has_content_regression_defect_1_trees_but_no_edges() {
        // 39 singleton trees, 0 edges: `project_graph_trees` is never empty
        // (defect 1's false-positive signal), but with no edges there's
        // nothing to draw, so content must read false and the split must
        // hand the whole area to the brain.
        assert!(!graph_has_content(0, true));

        let area = Rect::new(0, 0, 30, 20);
        match split_brain_or_graph(area, graph_has_content(0, true)) {
            BrainOrGraphLayout::BrainOnly(brain) => assert_eq!(brain, area),
            other => panic!("expected BrainOnly, got {other:?}"),
        }
    }

    #[test]
    fn graph_has_content_true_with_at_least_one_edge_on_knowledge_tab() {
        assert!(graph_has_content(1, true));
    }

    #[test]
    fn graph_has_content_gated_to_knowledge_layer() {
        // Same edge count, same area — only the tab differs.
        let edge_count = 3;
        assert!(
            graph_has_content(edge_count, true),
            "Knowledge should show it"
        );
        assert!(
            !graph_has_content(edge_count, false),
            "Live/Automation should not show it"
        );
    }

    #[test]
    fn graph_edge_row_budget_regression_defect_2() {
        // GRAPH_MIN_HEIGHT is the *outer* height the split hands the panel;
        // render_titled_panel strips Borders::ALL's 2 border rows before
        // draw_project_graph ever sees the area. The old code subtracted
        // another 2 from that already-inner height, computing 0 for a
        // panel that has exactly one edge to show.
        let inner_height = GRAPH_MIN_HEIGHT - 2;
        assert_eq!(graph_edge_row_budget(inner_height, 1), 1);
    }

    #[test]
    fn graph_edge_row_budget_truncates_to_available_rows() {
        assert_eq!(graph_edge_row_budget(2, 5), 2);
    }

    #[test]
    fn graph_edge_row_budget_draws_all_edges_when_rows_have_slack() {
        assert_eq!(graph_edge_row_budget(10, 3), 3);
    }

    #[test]
    fn knowledge_tab_does_not_draw_graph_in_sidebar() {
        let text = render_sidebar_text_with(
            2,
            34,
            40,
            &Theme::classic(),
            SidebarLayer::Knowledge,
            |app| {
                app.project_graph_edges
                    .push(crate::tui::app::types::ProjectGraphEdge {
                        from_name: "project0".to_string(),
                        to_name: "project1".to_string(),
                        from_hash: "hash0".to_string(),
                        to_hash: "hash1".to_string(),
                        relation: "depends_on".to_string(),
                    });
            },
        );
        assert!(
            !text.contains("project graph"),
            "project graph belongs to the right-panel Knowledge face: {text}"
        );
    }

    #[test]
    fn live_tab_never_shows_the_graph_panel_even_with_edges() {
        let text =
            render_sidebar_text_with(2, 34, 40, &Theme::classic(), SidebarLayer::Live, |app| {
                app.project_graph_edges
                    .push(crate::tui::app::types::ProjectGraphEdge {
                        from_name: "project0".to_string(),
                        to_name: "project1".to_string(),
                        from_hash: "hash0".to_string(),
                        to_hash: "hash1".to_string(),
                        relation: "depends_on".to_string(),
                    });
            });
        assert!(
            !text.contains("project graph"),
            "graph panel must not render on Live: {text}"
        );
    }

    #[test]
    fn registered_projects_with_zero_relations_do_not_claim_graph_height() {
        // Spec CB17: registered projects with zero relations make
        // `refresh_project_graph` fill `trees` with one singleton per project
        // while leaving `edges` empty. The old `!trees.is_empty()` height-claim
        // check fired on that, stealing GRAPH_MIN_HEIGHT rows to show only
        // "No relationships yet". The fixed criterion is the same one the
        // drawer uses — it requires an edge — so with none the panel claims
        // nothing and the strip goes to Brian's Brain.
        //
        // Guard: `knowledge_tab_with_an_edge_draws_the_graph_panel` renders
        // with this exact (project_count, width, height, tab) and asserts the
        // panel *does* appear once an edge is present. The only difference
        // here is the relation count, so a pass proves the criterion — not a
        // lack of room — is what withholds the rows.
        let text = render_sidebar_text_with(
            2,
            34,
            40,
            &Theme::classic(),
            SidebarLayer::Knowledge,
            |app| {
                assert!(
                    !app.project_graph_trees.is_empty(),
                    "precondition: singleton trees should be present"
                );
                assert!(
                    app.project_graph_edges.is_empty(),
                    "precondition: no relations were registered"
                );
            },
        );
        assert!(
            !text.contains("project graph"),
            "graph panel must not claim height with zero relations: {text}"
        );
        assert!(
            !text.contains("No relationships yet"),
            "empty message must not be drawn when panel has no content: {text}"
        );
    }

    #[test]
    fn ct14_fair_section_floor_tracks_cursor_not_stored_focus() {
        // CT14 render invariant (C32 follow-on): the guaranteed floor follows
        // the section holding the cursor, not the stored focus alone — so a
        // zero-row focused section cannot steal the floor on short screens.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = crate::tui::app::App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();

        // Cursor sits on a terminal row while stored focus claims Interactive.
        app.agents = vec![
            AgentEntry::Interactive(0),
            AgentEntry::Interactive(1),
            AgentEntry::Terminal(0),
        ];
        app.selected = 2;
        app.agent_section_focus = AgentSectionFocus::Interactive;
        assert_eq!(
            super::live_section_floor_index(&app, &[0, 1], &[2]),
            Some(1),
            "floor must track the terminal cursor, not the stored Interactive focus"
        );

        // Automation mirror: cursor on a background agent while the stored
        // kind claims Graphs (whose list is empty here).
        app.agents = vec![AgentEntry::Agent(crate::domain::models::Agent {
            id: "bg-1".to_string(),
            prompt: String::new(),
            trigger: None,
            cli: crate::domain::models::Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/bg-1.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
        })];
        app.selected = 0;
        app.selected_graph_id = None;
        app.automation_kind = AutomationKind::Graph;
        assert_eq!(
            super::automation_section_floor_index(&app, &[0]),
            Some(0),
            "floor must track the agent cursor, not the stored Graph kind"
        );
    }
}
