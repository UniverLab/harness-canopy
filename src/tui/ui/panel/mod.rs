//! Right panel rendering — PTY output, brain automaton, banner, background_agent/watcher details, log.

use chrono::{Local, TimeZone};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Color;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use super::theme::Theme;
use super::{truncate_str, truncate_str_keep_tail, STATUS_RUNNING};
use crate::tui::agent::ScreenSnapshot;
use crate::tui::app::types::{AgentEntry, App, Focus, ProjectTab, SidebarLayer};

mod log_panel;
use log_panel::*;
mod project_tabs;
use project_tabs::*;
mod rag_view;
use rag_view::*;
mod playground;
use playground::*;
mod crews;

pub mod background_agent;
pub mod details;
mod graph_live;
pub mod home;
pub mod log_fallback;
pub mod sync;
pub mod vt100;
pub mod warp;

pub(crate) use background_agent::draw_background_agent_panel;
pub use details::{draw_agent_details, draw_group_details};
use graph_live::draw_graph_live_view;
pub(crate) use home::draw_brians_brain;
pub use log_fallback::draw_log_text;
pub(crate) use sync::draw_panel_face;
use vt100::render_vt_screen;
#[allow(unused_imports)]
pub use warp::compact_cwd;
pub use warp::{draw_warp_input_box, render_command_chips};

use home::draw_canopy_banner_animation;
use vt100::render_indicators;

fn render_panel_block<'a>(
    frame: &mut Frame,
    area: Rect,
    border_color: Color,
    title: Option<Span<'a>>,
    theme: &Theme,
) -> Rect {
    let mut block = Block::default()
        .borders(super::borders_for(theme))
        .border_style(Style::default().fg(border_color));

    if let Some(title) = title {
        block = block.title(title);
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn render_wrapped_paragraph<'a>(frame: &mut Frame, area: Rect, lines: Vec<Line<'a>>) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn format_unix_timestamp(timestamp: i64) -> String {
    match Local.timestamp_opt(timestamp, 0).single() {
        Some(datetime) => datetime.format("%Y-%m-%d %H:%M").to_string(),
        None => timestamp.to_string(),
    }
}

fn project_metadata_matches(
    metadata: Option<&str>,
    project: &crate::domain::project::Project,
) -> bool {
    let Some(metadata) = metadata else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return false;
    };
    value.get("workdir").and_then(serde_json::Value::as_str) == Some(project.path.as_str())
}

fn recent_project_session_summaries(
    app: &App,
    project: &crate::domain::project::Project,
    limit: usize,
) -> Vec<(String, String)> {
    let Ok(nodes) = app
        .db
        .search_operational_sessions(&project.path, limit.saturating_mul(4))
    else {
        return Vec::new();
    };

    nodes
        .into_iter()
        .filter(|node| {
            node.project_hash.as_deref() == Some(project.hash.as_str())
                || project_metadata_matches(node.metadata.as_deref(), project)
        })
        .take(limit)
        .map(|node| {
            let summary = if node.body.trim().is_empty() {
                "No summary captured yet.".to_string()
            } else {
                truncate_str(node.body.trim(), 96)
            };
            (node.title, summary)
        })
        .collect()
}

fn set_cursor_from_snapshot(frame: &mut Frame, area: Rect, snap: &ScreenSnapshot) {
    if snap.scrolled || area.width == 0 || area.height == 0 {
        return;
    }

    let cx = area.x + snap.cursor_col.min(area.width.saturating_sub(1));
    let cy = area.y + snap.cursor_row.min(area.height.saturating_sub(1));
    frame.set_cursor_position((cx, cy));
}

fn render_snapshot(
    frame: &mut Frame,
    area: Rect,
    snap: &ScreenSnapshot,
    app: &App,
    _mask_cursor_line: bool,
    show_cursor: bool,
    selection: Option<vt100::PaneSelection>,
) {
    render_vt_screen(frame, area, snap, selection);
    if show_cursor {
        set_cursor_from_snapshot(frame, area, snap);
    }
    render_indicators(frame, area, snap, app);
}

/// The active mouse selection, if it belongs to the pane's agent.
fn pane_selection(app: &App, is_terminal: bool, idx: usize) -> Option<vt100::PaneSelection> {
    let sel = app.terminal_selection?;
    (sel.agent == (is_terminal, idx)).then(|| sel.normalized())
}

/// Splits the terminal-warp panel into the PTY output area and the input
/// box, with a 1-row gap between them. `input_text` is the current buffer
/// contents (used to size the input box for wrapped/multiline content).
fn split_warp_areas(area: Rect, input_text: &str) -> (Rect, Rect) {
    let input_height = warp::input_height(input_text, area.width);
    let chunks = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(input_height),
    ])
    .split(area);
    (chunks[0], chunks[2])
}

fn warp_input_text(app: &App, idx: usize) -> String {
    app.terminal_agents
        .get(idx)
        .map(|agent| {
            if agent.is_sensitive_input_active() {
                String::new()
            } else {
                agent
                    .input_buffer
                    .lock()
                    .map(|b| b.clone())
                    .unwrap_or_default()
            }
        })
        .unwrap_or_default()
}

fn labeled_value_line<'a>(label: &'static str, value: Span<'a>, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(theme.dim_text)),
        value,
    ])
}

fn selected_row_style(selected: bool, theme: &Theme) -> (Style, &'static str) {
    if selected {
        (Style::default().bg(theme.selected_bg), "›")
    } else {
        (Style::default(), " ")
    }
}

fn selected_agent_accent(app: &App, theme: &Theme) -> Option<Color> {
    let selected = app.selected_agent()?;
    match selected {
        AgentEntry::Interactive(idx) => app
            .interactive_agents
            .get(*idx)
            .map(|agent| agent.accent_color),
        AgentEntry::Terminal(idx) => app
            .terminal_agents
            .get(*idx)
            .map(|agent| agent.accent_color),
        _ => Some(theme.header_color),
    }
}

/// Resolves (border color, label color) for the log panel given the current
/// focus and the selected agent's accent (if any). Pure so it's testable
/// without constructing a full `App`.
fn panel_focus_colors(focus: Focus, agent_accent: Option<Color>, theme: &Theme) -> (Color, Color) {
    let accent = agent_accent.unwrap_or(theme.border_color);
    match focus {
        // Full-border accent for real focus.
        Focus::Agent => (accent, accent),
        // Preview is quieter: normal border, accent only on the label.
        Focus::Preview => (theme.border_color, accent),
        _ => (theme.border_color, theme.border_color),
    }
}

fn log_panel_border_color(app: &App, theme: &Theme) -> Color {
    panel_focus_colors(app.focus, selected_agent_accent(app, theme), theme).0
}

fn panel_mode_label_color(app: &App, theme: &Theme) -> Color {
    panel_focus_colors(app.focus, selected_agent_accent(app, theme), theme).1
}

fn panel_mode_label(app: &App) -> Option<&'static str> {
    match app.focus {
        Focus::Preview => Some(" Preview "),
        Focus::Agent => Some(" Focus "),
        _ => None,
    }
}

fn graph_live_mode_label(app: &App) -> Option<&'static str> {
    if app.sidebar_layer == SidebarLayer::Automation
        && app.automation_kind == crate::tui::app::AutomationKind::Graph
        && app.graph_live_state.is_some()
    {
        if app.graph_live_follow {
            Some(" Auto-follow ")
        } else {
            Some(" Manual ")
        }
    } else {
        None
    }
}

fn render_focus_indicator(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    app: &App,
    theme: &Theme,
    label: Option<&str>,
) {
    if theme.show_borders || area.width == 0 || area.height == 0 {
        return;
    }

    let rail_color = match app.focus {
        Focus::Agent => theme.header_color,
        Focus::Preview => theme.dim_text,
        _ => return,
    };
    let rail = Rect::new(area.x, area.y, 1, area.height);
    frame.render_widget(
        Paragraph::new("█".repeat(rail.height as usize))
            .style(Style::default().fg(rail_color).bg(rail_color)),
        rail,
    );

    if let Some(label) = label.filter(|_| inner.height > 0 && inner.width > 4) {
        let text = label.trim();
        let width = text.chars().count() as u16;
        if width + 2 <= inner.width {
            let pill_area = Rect::new(inner.x + 1, inner.y, width + 2, 1);
            let pill = Span::styled(
                format!(" {text} "),
                Style::default()
                    .fg(theme.accent_fg)
                    .bg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            );
            frame.render_widget(Paragraph::new(Line::from(pill)), pill_area);
        }
    }
}

fn show_home_fallback(app: &App) -> bool {
    app.agents.is_empty()
        && app.projects.is_empty()
        && !matches!(
            app.focus,
            Focus::NewAgentDialog
                | Focus::LaunchpadDialog
                | Focus::ContextTransfer
                | Focus::RagTransfer
                | Focus::PromptTemplateDialog
                | Focus::GraphEditorDialog
                | Focus::GraphFormDialog
                | Focus::ProjectRelationDialog
        )
}

fn draw_home_panel(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    app.home_brain_rect = Some(area);
    if let Some(brain) = app.home_brain.as_ref() {
        draw_brians_brain(frame, area, brain, theme.panel_bg);
    }
    draw_canopy_banner_animation(frame, area, app);
}

/// Renders a corrupt agent row's detail panel: id and parse error, no
/// attempt to interpret the malformed data.
fn draw_corrupt_agent_panel(
    frame: &mut Frame,
    area: Rect,
    corrupt: &crate::domain::models::CorruptAgent,
) {
    let text = format!(
        "{} [corrupt config]\n\nThis agent's trigger_config failed to parse and has been \
         quarantined (disabled). It was not modified or reinterpreted.\n\nError: {}\n\nPress 'd' \
         to delete this row.",
        corrupt.id, corrupt.error
    );
    let paragraph = ratatui::widgets::Paragraph::new(text)
        .style(ratatui::style::Style::default().fg(ratatui::style::Color::Red))
        .wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

pub(super) fn draw_log_panel(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let border_color = log_panel_border_color(app, theme);
    let label_color = panel_mode_label_color(app, theme);
    let graph_title = graph_live_mode_label(app);
    let indicator_label = graph_title.or_else(|| panel_mode_label(app));
    let title = if theme.show_borders {
        match (graph_title, panel_mode_label(app)) {
            (Some(lt), _) => Some(Span::styled(
                lt,
                Style::default()
                    .fg(label_color)
                    .add_modifier(Modifier::BOLD),
            )),
            (None, Some(bt)) => Some(Span::styled(
                bt,
                Style::default()
                    .fg(label_color)
                    .add_modifier(Modifier::BOLD),
            )),
            (None, None) => None,
        }
    } else {
        None
    };
    let inner = render_panel_block(frame, area, border_color, title, theme);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    app.last_panel_inner = (inner.width, inner.height);
    app.last_panel_x = inner.x;
    app.last_panel_y = inner.y;

    app.home_brain_rect = None;
    if show_home_fallback(app) {
        draw_home_panel(frame, inner, app, theme);
        render_focus_indicator(frame, area, inner, app, theme, indicator_label);
        return;
    }

    if draw_log_panel_focus(frame, inner, app, theme) {
        render_focus_indicator(frame, area, inner, app, theme, indicator_label);
        return;
    }

    draw_log_text(frame, area, inner, app);
    render_focus_indicator(frame, area, inner, app, theme, indicator_label);
}

// ── Split panel ─────────────────────────────────────────────────

/// Render one half of a split view — finds the session by name and draws its PTY.
pub(super) fn draw_split_panel(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    session_name: &str,
    focused: bool,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let found = find_session_by_name(app, session_name);
    let border_color = if focused {
        found.map_or(theme.border_color, |session| session.accent(app))
    } else {
        theme.border_color
    };
    let title = Span::styled(
        if focused {
            format!(" ● {session_name} ")
        } else {
            format!("   {session_name} ")
        },
        Style::default()
            .fg(border_color)
            .add_modifier(Modifier::BOLD),
    );

    let inner = render_panel_block(frame, area, border_color, Some(title), theme);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if focused {
        app.last_panel_inner = (inner.width, inner.height);
        app.last_panel_x = inner.x;
        app.last_panel_y = inner.y;
    }

    let Some(session) = found else {
        render_missing_session(frame, inner, session_name, theme);
        return;
    };

    if let Some(terminal_idx) = session.warp_terminal_idx(app) {
        let snapshot = session.snapshot(app);
        draw_split_warp_panel(frame, inner, app, terminal_idx, snapshot.as_ref(), focused);
        return;
    }

    let Some(snap) = session.snapshot(app) else {
        return;
    };

    render_snapshot(
        frame,
        inner,
        &snap,
        app,
        false,
        focused && matches!(app.focus, Focus::Agent),
        None,
    );
}

fn draw_split_warp_panel(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    terminal_idx: usize,
    snap: Option<&ScreenSnapshot>,
    focused: bool,
) {
    let (pty_area, input_area) = split_warp_areas(area, &warp_input_text(app, terminal_idx));

    if let Some(snap) = snap {
        render_snapshot(frame, pty_area, snap, app, false, false, None);
    }

    if focused && matches!(app.focus, Focus::Agent) {
        draw_warp_input_box(frame, input_area, app, terminal_idx);
    }

    if focused {
        app.last_panel_inner = (pty_area.width, pty_area.height);
        app.last_panel_x = pty_area.x;
        app.last_panel_y = pty_area.y;
    }
}

fn render_missing_session(frame: &mut Frame, area: Rect, session_name: &str, theme: &Theme) {
    let message = Paragraph::new(format!("  Session '{session_name}' not found"))
        .style(Style::default().fg(theme.dim_text));
    frame.render_widget(message, area);
}

#[derive(Clone, Copy)]
enum SessionRef {
    Interactive(usize),
    Terminal(usize),
}

impl SessionRef {
    fn accent(self, app: &App) -> Color {
        match self {
            SessionRef::Interactive(idx) => app.interactive_agents[idx].accent_color,
            SessionRef::Terminal(idx) => app.terminal_agents[idx].accent_color,
        }
    }

    fn snapshot(self, app: &App) -> Option<ScreenSnapshot> {
        match self {
            SessionRef::Interactive(idx) => app.interactive_agents[idx].screen_snapshot(),
            SessionRef::Terminal(idx) => app.terminal_agents[idx].screen_snapshot(),
        }
    }

    fn warp_terminal_idx(self, app: &App) -> Option<usize> {
        match self {
            SessionRef::Terminal(idx)
                if app.terminal_agents[idx].warp_mode
                    && !app.terminal_agents[idx].should_bypass_warp_input() =>
            {
                Some(idx)
            }
            _ => None,
        }
    }
}

fn find_session_by_name(app: &App, name: &str) -> Option<SessionRef> {
    if let Some(idx) = app
        .interactive_agents
        .iter()
        .position(|agent| agent.name == name)
    {
        return Some(SessionRef::Interactive(idx));
    }
    if let Some(idx) = app
        .terminal_agents
        .iter()
        .position(|agent| agent.name == name)
    {
        return Some(SessionRef::Terminal(idx));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::theme::Theme;
    use super::adjusted_interactive_cursor_col;
    use super::draw_backlog_overview;
    use super::panel_focus_colors;
    use super::split_warp_areas;
    use super::warp;
    use super::*;
    use crate::tui::agent::screen::VtCell;
    use crate::tui::agent::ScreenSnapshot;
    use crate::tui::app::types::{App, Focus};
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn render_to_text(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut ratatui::Frame, Rect),
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn backlog_overview_previews_selected_spec_name_and_description_read_only() {
        use crate::db::Database;
        use crate::domain::graphs::{GraphSpec, GraphSpecStatus};
        use std::sync::Arc;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        db.insert_graph_spec(&GraphSpec {
            id: "spec-1".to_string(),
            graph_id: None,
            name: "Add retry backoff".to_string(),
            description: Some("## Objective\nRetry requests with backoff.".to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
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

        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        assert_eq!(app.backlog_specs.len(), 1, "backlog spec should be loaded");

        let text = render_to_text(50, 10, |frame, area| {
            draw_backlog_overview(frame, area, &app, &Theme::classic());
        });

        assert!(
            text.contains("Add retry backoff"),
            "expected spec name in preview, got:\n{text}"
        );
        assert!(
            text.contains("Retry requests with backoff."),
            "expected spec description in preview, got:\n{text}"
        );
    }

    #[test]
    fn copilot_cursor_no_longer_shifted_without_inverse() {
        let snap = ScreenSnapshot {
            cells: vec![(0..8).map(|_| None).collect()],
            cursor_row: 0,
            cursor_col: 5,
            scrolled: false,
        };
        assert_eq!(adjusted_interactive_cursor_col("copilot", &snap), 5);
        let snap_zero = ScreenSnapshot {
            cursor_col: 0,
            ..snap
        };
        assert_eq!(adjusted_interactive_cursor_col("copilot", &snap_zero), 0);
    }

    #[test]
    fn copilot_cursor_prefers_inverse_cell_when_present() {
        let mut row: Vec<Option<VtCell>> = (0..8).map(|_| None).collect();
        row[3] = Some(VtCell {
            ch: "x".to_string(),
            fg: Color::White,
            bg: Color::Black,
            bold: false,
            underline: false,
            inverse: true,
            wide_continuation: false,
        });
        let snap = ScreenSnapshot {
            cells: vec![row],
            cursor_row: 0,
            cursor_col: 7,
            scrolled: false,
        };
        assert_eq!(adjusted_interactive_cursor_col("copilot-cli", &snap), 3);
    }

    #[test]
    fn other_clients_keep_their_cursor_position() {
        let snap = ScreenSnapshot {
            cells: vec![(0..8).map(|_| None).collect()],
            cursor_row: 0,
            cursor_col: 5,
            scrolled: false,
        };
        assert_eq!(adjusted_interactive_cursor_col("opencode", &snap), 5);
    }

    #[test]
    fn split_warp_areas_reserves_four_rows_for_empty_input() {
        let area = Rect::new(0, 0, 80, 20);
        let (pty_area, input_area) = split_warp_areas(area, "");
        assert_eq!(input_area.height, 4);
        // 1 row gap + 4 row input box.
        assert_eq!(pty_area.height, 15);
    }

    #[test]
    fn split_warp_areas_leaves_one_row_gap_above_input() {
        let area = Rect::new(0, 0, 80, 20);
        let (pty_area, input_area) = split_warp_areas(area, "hello");
        assert_eq!(input_area.y, pty_area.y + pty_area.height + 1);
    }

    #[test]
    fn warp_input_height_short_text_stays_at_base() {
        // 100 chars at 40 cols wraps to 3 lines, which fits within the
        // base box without growing it.
        let text = "a".repeat(100);
        assert_eq!(warp::input_height(&text, 40), 4);
    }

    #[test]
    fn warp_input_height_long_text_grows() {
        // 200 chars at 40 cols wraps to 5 lines: 2 lines beyond the
        // 3-line base capacity, so the box grows from 4 to 6 rows.
        let text = "a".repeat(200);
        let height = warp::input_height(&text, 40);
        assert!((5..=6).contains(&height), "height was {height}");
    }

    #[test]
    fn warp_input_height_explicit_newlines_grow_and_cap_at_max() {
        let text = "a\nb\nc\nd\ne\nf\ng"; // 7 lines
        assert_eq!(warp::input_height(text, 40), 8);
    }

    #[test]
    fn warp_input_height_empty_is_base() {
        assert_eq!(warp::input_height("", 40), 4);
    }

    #[test]
    fn focus_agent_draws_full_accent_border() {
        let theme = Theme::classic();
        let accent = Color::Rgb(200, 50, 50);
        let (border, label) = panel_focus_colors(Focus::Agent, Some(accent), &theme);
        assert_eq!(border, accent);
        assert_eq!(label, accent);
    }

    #[test]
    fn focus_preview_keeps_normal_border_and_accents_only_label() {
        let theme = Theme::classic();
        let accent = Color::Rgb(50, 200, 50);
        let (border, label) = panel_focus_colors(Focus::Preview, Some(accent), &theme);
        assert_eq!(border, theme.border_color);
        assert_eq!(label, accent);
    }

    #[test]
    fn other_focus_states_use_normal_border_and_label() {
        let theme = Theme::classic();
        let accent = Color::Rgb(50, 50, 200);
        let (border, label) = panel_focus_colors(Focus::Home, Some(accent), &theme);
        assert_eq!(border, theme.border_color);
        assert_eq!(label, theme.border_color);
    }

    #[test]
    fn missing_accent_falls_back_to_border_color_everywhere() {
        let theme = Theme::classic();
        assert_eq!(
            panel_focus_colors(Focus::Agent, None, &theme),
            (theme.border_color, theme.border_color)
        );
        assert_eq!(
            panel_focus_colors(Focus::Preview, None, &theme),
            (theme.border_color, theme.border_color)
        );
    }

    #[test]
    fn format_unix_timestamp_valid() {
        let ts = 1_700_000_000; // 2023-11-14 22:13:20 UTC
        let result = format_unix_timestamp(ts);
        assert!(result.contains("2023"), "Should contain year: {result}");
    }

    #[test]
    fn format_unix_timestamp_zero() {
        let result = format_unix_timestamp(0);
        // Epoch 0 is 1970-01-01 UTC, displayed as local time
        assert!(!result.is_empty(), "Should produce a string: {result}");
    }

    #[test]
    fn render_wrapped_paragraph_zero_area() {
        let backend = TestBackend::new(10, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 0, 0);
                render_wrapped_paragraph(frame, area, vec![]);
            })
            .unwrap();
    }

    #[test]
    fn render_panel_block_with_title() {
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let title = Span::styled(" Test ", Style::default().fg(Color::White));
                let inner = render_panel_block(frame, area, Color::Cyan, Some(title), &theme);
                assert!(inner.width > 0);
                assert!(inner.height > 0);
            })
            .unwrap();
    }

    #[test]
    fn render_panel_block_without_title() {
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let inner = render_panel_block(frame, area, Color::Cyan, None, &theme);
                assert!(inner.width > 0);
                assert!(inner.height > 0);
            })
            .unwrap();
    }

    #[test]
    fn render_panel_block_modern_theme_no_borders() {
        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::modern();
        terminal
            .draw(|frame| {
                let area = frame.area();
                let inner = render_panel_block(frame, area, Color::Cyan, None, &theme);
                // Modern theme: no borders, so inner == area
                assert_eq!(inner, area);
            })
            .unwrap();
    }

    #[test]
    fn set_cursor_from_snapshot_scrolled_no_cursor() {
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let snap = ScreenSnapshot {
            cells: vec![],
            cursor_row: 5,
            cursor_col: 5,
            scrolled: true,
        };
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 20, 10);
                set_cursor_from_snapshot(frame, area, &snap);
            })
            .unwrap();
        // Scrolled: no cursor set
    }

    #[test]
    fn set_cursor_from_snapshot_zero_area() {
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let snap = ScreenSnapshot {
            cells: vec![],
            cursor_row: 0,
            cursor_col: 0,
            scrolled: false,
        };
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 0, 0);
                set_cursor_from_snapshot(frame, area, &snap);
            })
            .unwrap();
    }

    #[test]
    fn labeled_value_line_renders() {
        let theme = Theme::classic();
        let line = labeled_value_line("Key: ", Span::raw("value"), &theme);
        assert_eq!(line.width(), 10); // "Key: " + "value"
    }

    #[test]
    fn selected_row_style_selected() {
        let theme = Theme::classic();
        let (style, marker) = selected_row_style(true, &theme);
        assert_eq!(style.bg, Some(theme.selected_bg));
        assert_eq!(marker, "›");
    }

    #[test]
    fn selected_row_style_not_selected() {
        let theme = Theme::classic();
        let (style, marker) = selected_row_style(false, &theme);
        assert_eq!(style.bg, None);
        assert_eq!(marker, " ");
    }

    #[test]
    fn panel_mode_label_preview() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Preview;
        assert_eq!(panel_mode_label(&app), Some(" Preview "));
    }

    #[test]
    fn panel_mode_label_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Agent;
        assert_eq!(panel_mode_label(&app), Some(" Focus "));
    }

    #[test]
    fn panel_mode_label_home() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Home;
        assert_eq!(panel_mode_label(&app), None);
    }

    #[test]
    fn focus_preview_is_evident_in_modern_buffer_without_border_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let theme = Theme::modern();
        let area = Rect::new(0, 0, 30, 8);

        for (focus, expected_color, expected_label) in [
            (Focus::Agent, theme.header_color, "Focus"),
            (Focus::Preview, theme.dim_text, "Preview"),
        ] {
            app.focus = focus;
            let backend = TestBackend::new(area.width, area.height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_focus_indicator(frame, area, area, &app, &theme, panel_mode_label(&app));
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].symbol(), "█");
            assert_eq!(buffer[(0, 0)].fg, expected_color);
            let text: String = (0..area.height)
                .flat_map(|y| (0..area.width).map(move |x| buffer[(x, y)].symbol()))
                .collect();
            assert!(
                text.contains(expected_label),
                "missing {expected_label}: {text}"
            );
        }
    }

    #[test]
    fn draw_log_panel_modern_renders_rail_and_hides_border_title() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Agent;
        // Prevent the home fallback (empty workspace) from taking over the
        // panel and hiding the rail/title distinction we want to assert.
        app.projects
            .push(crate::domain::project::Project::new("/tmp/proj"));
        // Also seed a minimal agent so the Agent-focused branch doesn't fall
        // through to an empty log (keeps the exercised path stable).
        {
            let mut agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
                "cat",
                "/tmp",
                80,
                24,
                Some("focus-agent"),
                &[],
                ratatui::style::Color::White,
            )
            .expect("spawn");
            agent.status = crate::tui::agent::AgentStatus::Running;
            app.interactive_agents.push(agent);
            app.agents
                .push(crate::tui::app::types::AgentEntry::Interactive(0));
        }
        let modern = Theme::modern();
        let area = Rect::new(0, 0, 40, 12);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_log_panel(frame, area, &mut app, &modern))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        // Modern must show the 1-col rail at the panel's left edge.
        assert_eq!(buffer[(area.x, area.y)].symbol(), "█");
        assert_eq!(buffer[(area.x, area.y)].fg, modern.header_color);
        // The border title for Focus must NOT be drawn as a top border (it's the
        // pill instead), so the top row should be the rail, not box-drawing.
        let top_row: String = (0..area.width)
            .map(|x| buffer[(x, area.y)].symbol())
            .collect();
        assert!(
            !top_row.contains('┌') && !top_row.contains('─'),
            "modern must not render a border title, top row was {top_row:?}"
        );

        // Classic still uses the border title (and no rail).
        let classic = Theme::classic();
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_log_panel(frame, area, &mut app, &classic))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        let mut text = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(
            text.contains("Focus"),
            "classic title must contain Focus, got {text:?}"
        );
    }

    #[test]
    fn show_home_fallback_empty_agents_and_projects() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Home;
        assert!(show_home_fallback(&app));
    }

    #[test]
    fn show_home_fallback_not_when_dialog_open() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::NewAgentDialog;
        assert!(!show_home_fallback(&app));
    }

    #[test]
    fn draw_log_panel_zero_area_no_panic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 0, 0);
                draw_log_panel(frame, area, &mut app, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_split_panel_zero_area_no_panic() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 0, 0);
                draw_split_panel(frame, area, &mut app, "test-session", true, &theme);
            })
            .unwrap();
    }

    #[test]
    fn draw_corrupt_agent_panel_renders() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let corrupt = crate::domain::models::CorruptAgent {
            id: "bad-agent".to_string(),
            enabled: false,
            error: "failed to parse JSON".to_string(),
        };
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_corrupt_agent_panel(frame, area, &corrupt);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("bad-agent"), "Should show agent id: {text}");
        assert!(
            text.contains("corrupt config"),
            "Should show corrupt config: {text}"
        );
    }

    #[test]
    fn draw_home_panel_renders() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let theme = Theme::classic();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_home_panel(frame, area, &mut app, &theme);
            })
            .unwrap();
    }

    #[test]
    fn rag_file_icon_and_color_indexed() {
        let theme = Theme::classic();
        let (icon, color) = rag_file_icon_and_color("indexed", &theme);
        assert_eq!(icon, "✓");
        assert_eq!(color, theme.success);
    }

    #[test]
    fn rag_file_icon_and_color_deleted() {
        let theme = Theme::classic();
        let (icon, color) = rag_file_icon_and_color("deleted", &theme);
        assert_eq!(icon, "○");
        assert_eq!(color, theme.dim_text);
    }

    #[test]
    fn rag_file_icon_and_color_error() {
        let theme = Theme::classic();
        let (icon, color) = rag_file_icon_and_color("error", &theme);
        assert_eq!(icon, "✗");
        assert_eq!(color, Color::Red);
    }

    #[test]
    fn rag_file_detail_error_with_message() {
        use crate::db::project::RagPerFileStatus;
        let file = RagPerFileStatus {
            file_path: "test.rs".to_string(),
            last_event_type: "error".to_string(),
            last_detail: Some("parse failed".to_string()),
            times_indexed: 0,
            last_at: 0,
        };
        let detail = rag_file_detail(&file, 50);
        assert!(detail.starts_with("error:"));
    }

    #[test]
    fn rag_file_detail_error_no_message() {
        use crate::db::project::RagPerFileStatus;
        let file = RagPerFileStatus {
            file_path: "test.rs".to_string(),
            last_event_type: "error".to_string(),
            last_detail: None,
            times_indexed: 0,
            last_at: 0,
        };
        let detail = rag_file_detail(&file, 50);
        assert_eq!(detail, "error");
    }

    #[test]
    fn rag_file_detail_deleted() {
        use crate::db::project::RagPerFileStatus;
        let file = RagPerFileStatus {
            file_path: "test.rs".to_string(),
            last_event_type: "deleted".to_string(),
            last_detail: None,
            times_indexed: 0,
            last_at: 0,
        };
        let detail = rag_file_detail(&file, 50);
        assert_eq!(detail, "deleted");
    }

    #[test]
    fn rag_file_detail_indexed() {
        use crate::db::project::RagPerFileStatus;
        let file = RagPerFileStatus {
            file_path: "test.rs".to_string(),
            last_event_type: "indexed".to_string(),
            last_detail: None,
            times_indexed: 3,
            last_at: 0,
        };
        let detail = rag_file_detail(&file, 50);
        assert_eq!(detail, "indexed ×3");
    }

    #[test]
    fn visible_playground_window_basic() {
        let area = Rect::new(0, 0, 80, 20);
        let (max_visible, scroll_start) = visible_playground_window(area, 0, 20);
        assert!(max_visible > 0);
        assert_eq!(scroll_start, 0);
    }

    #[test]
    fn visible_playground_window_scrolled() {
        let area = Rect::new(0, 0, 80, 20);
        let (max_visible, scroll_start) = visible_playground_window(area, 10, 20);
        assert!(scroll_start > 0 || max_visible >= 10);
    }

    #[test]
    fn playground_scope_label_global() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        assert_eq!(playground_scope_label(&app), "Global");
    }

    #[test]
    fn detail_progress_line_all_visible() {
        let theme = Theme::classic();
        let result = detail_progress_line(5, 10, 0, &theme);
        assert!(result.is_none());
    }

    #[test]
    fn detail_progress_line_partial() {
        let theme = Theme::classic();
        let result = detail_progress_line(100, 10, 50, &theme);
        assert!(result.is_some());
    }

    #[test]
    fn rag_status_lines_empty_queue() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_info.queued_items = 0;
        let theme = Theme::classic();
        let lines = rag_summary_lines(&app, "● ready", theme.header_color, String::new(), &theme);
        assert!(!lines.is_empty());
    }

    #[test]
    fn rag_status_lines_with_queue() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        let theme = Theme::classic();
        let lines = rag_summary_lines(
            &app,
            "● ready",
            theme.header_color,
            "3 queued".to_string(),
            &theme,
        );
        assert!(!lines.is_empty());
    }

    #[test]
    #[cfg(not(feature = "local-embeddings"))]
    fn rag_status_reports_unavailable_for_local_model_without_feature() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        // Even a "loaded" daemon state must not mask the capability gap.
        app.rag_embeddings_model = "baai/bge-small-en-v1.5".to_string();
        app.rag_model_loaded = true;
        let theme = Theme::classic();
        let (text, color) = rag_status(&app, &theme);
        assert_eq!(text, "✗ unavailable");
        assert_eq!(color, Color::Red);
    }

    #[test]
    fn rag_status_reports_downloading_and_not_ready() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_embeddings_model = "text-embedding-3-small".to_string();
        // Even a "loaded" daemon state must not hide an in-progress download.
        app.rag_model_loaded = true;
        app.rag_acquisition_state =
            Some(crate::rag::status::AcquisitionState::Downloading { started_at: 0 });
        let theme = Theme::classic();
        let (text, color) = rag_status(&app, &theme);
        assert_eq!(text, "⬇ downloading");
        assert_eq!(color, Color::Yellow);
    }

    #[test]
    fn rag_status_reports_preparing() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_embeddings_model = "text-embedding-3-small".to_string();
        app.rag_acquisition_state =
            Some(crate::rag::status::AcquisitionState::Preparing { started_at: 0 });
        let theme = Theme::classic();
        let (text, color) = rag_status(&app, &theme);
        assert_eq!(text, "⚙ preparing");
        assert_eq!(color, Color::Yellow);
    }

    #[test]
    fn rag_status_reports_download_failed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.rag_embeddings_model = "text-embedding-3-small".to_string();
        app.rag_acquisition_state = Some(crate::rag::status::AcquisitionState::Failed {
            reason: "connection reset".to_string(),
        });
        let theme = Theme::classic();
        let (text, color) = rag_status(&app, &theme);
        assert_eq!(text, "✗ download failed");
        assert_eq!(color, Color::Red);
    }

    fn graph_live_state() -> crate::tui::app::graph_live_state::GraphLiveState {
        crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "test".to_string(),
            graph_status: crate::domain::graphs::GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![],
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![],
            effective_edges: vec![],
            ensembles: vec![],
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        }
    }

    fn graph_app(follow: bool) -> App {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path: std::path::PathBuf = db_file.path().to_path_buf();
        std::mem::forget(db_file);
        let mut app = App::new(
            Arc::new(crate::db::Database::new(&db_path).unwrap()),
            tempfile::tempdir().unwrap().path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.graph_live_state = Some(graph_live_state());
        app.graph_live_follow = follow;
        app
    }

    #[test]
    fn graph_mode_appears_in_border_title_auto_follow() {
        let mut app = graph_app(true);
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, area| {
            draw_log_panel(frame, area, &mut app, &theme);
        });
        assert!(
            text.contains("Auto-follow"),
            "border title should show Auto-follow in graph view:\n{text}"
        );
        assert!(
            !text.contains("AUTO-FOLLOW"),
            "strip must not appear in the live view:\n{text}"
        );
    }

    #[test]
    fn graph_mode_appears_in_border_title_manual() {
        let mut app = graph_app(false);
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, area| {
            draw_log_panel(frame, area, &mut app, &theme);
        });
        assert!(
            text.contains("Manual"),
            "border title should show Manual in manual mode:\n{text}"
        );
        assert!(
            !text.contains("MANUAL"),
            "all-caps strip text must not appear:\n{text}"
        );
        assert!(
            !text.contains("Esc"),
            "instruction must not leak into border title:\n{text}"
        );
    }

    #[test]
    fn narrow_pane_keeps_mode_label() {
        // At narrow width the border title must still show the mode label.
        // (FR4: mode survives when pane is too narrow for both title and label.)
        let app = graph_app(false);
        assert_eq!(graph_live_mode_label(&app), Some(" Manual "));
        let title = graph_live_mode_label(&app).map(|label| {
            Span::styled(
                label,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        });
        let backend = TestBackend::new(10, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_panel_block(frame, area, Color::DarkGray, title, &Theme::classic());
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("Manual"),
            "mode label must survive at narrow width:\n{text}"
        );
    }

    #[test]
    fn graph_mode_label_changes_with_follow() {
        let mut app = graph_app(true);
        assert_eq!(graph_live_mode_label(&app), Some(" Auto-follow "));
        app.graph_live_follow = false;
        assert_eq!(graph_live_mode_label(&app), Some(" Manual "));
    }

    #[test]
    fn graph_mode_not_shown_when_not_graph_view() {
        // Outside the graph live view the graph mode must not appear; the
        // panel falls back to the interactive session label (or none).
        let mut app = graph_app(true);
        app.sidebar_layer = SidebarLayer::Live;
        assert_eq!(graph_live_mode_label(&app), None);

        let mut app = graph_app(true);
        app.automation_kind = crate::tui::app::AutomationKind::Agent;
        assert_eq!(graph_live_mode_label(&app), None);

        let mut app = graph_app(true);
        app.graph_live_state = None;
        assert_eq!(graph_live_mode_label(&app), None);
    }
}
