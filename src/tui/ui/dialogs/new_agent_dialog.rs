use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::{centered_rect, draw_dialog_left_wave, truncate_str};
use crate::tui::app::{
    dialog::new_agent::{BackgroundTrigger, NewAgentDialog, NewTaskMode, NewTaskType, SeedOption},
    types::App,
};
use crate::tui::ui::theme::Theme;

mod fields;
mod pickers;

#[cfg(test)]
use fields::prompt_visual_line_range;
use pickers::{
    append_background_sections, append_interactive_sections, append_terminal_sections,
    session_resume_label, SectionLayout,
};

const TYPE_FIELD: usize = 0;
const INTERACTIVE_MODE_FIELD: usize = 1;
const BACKGROUND_TRIGGER_FIELD: usize = 1;
const TERMINAL_DIR_FIELD: usize = 1;
const TERMINAL_SHELL_FIELD: usize = 2;

const CLI_PICKER_VISIBLE: usize = 6;
const MODEL_PICKER_VISIBLE: usize = 5;
const SESSION_PICKER_VISIBLE: usize = 6;
const DIR_BROWSER_VISIBLE: usize = 10;
/// Floor for the folder list when the frame is too short for the whole dialog:
/// the list shrinks first, and never below this many rows, before any other
/// row is cut (CT25 FR2).
const MIN_DIR_BROWSER_VISIBLE: usize = 3;
pub(crate) const SESSION_RESUME_PICKER_VISIBLE: usize = 6;

#[derive(Clone, Copy)]
struct FieldLayout {
    cli: usize,
    identity: usize,
    model: usize,
    prompt: usize,
    extra: usize,
    dir: usize,
    yolo: usize,
    sandbox: usize,
}

impl FieldLayout {
    fn for_task(task_type: NewTaskType) -> Self {
        match task_type {
            NewTaskType::Interactive => Self {
                cli: 2,
                identity: 5,
                model: 3,
                prompt: 4,
                extra: 5,
                dir: 6,
                yolo: 4,
                sandbox: 7,
            },
            NewTaskType::Terminal => Self {
                cli: 0,
                identity: 0,
                model: 3,
                prompt: 4,
                extra: 5,
                dir: TERMINAL_DIR_FIELD,
                yolo: 4,
                sandbox: 0,
            },
            NewTaskType::Background => Self {
                cli: 2,
                identity: 0,
                model: 3,
                prompt: 4,
                extra: 5,
                dir: 6,
                yolo: 4,
                sandbox: 0,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct PickerWindow {
    scroll: usize,
    has_above: bool,
    has_below: bool,
}

impl PickerWindow {
    fn new(selected: usize, total: usize, max_visible: usize) -> Self {
        let scroll = crate::tui::selection::clamp_scroll(selected, 0, total, max_visible);
        Self {
            scroll,
            has_above: scroll > 0,
            has_below: total > 0 && scroll + max_visible < total,
        }
    }
}

pub fn draw_new_agent_dialog(frame: &mut Frame, app: &App, theme: &Theme) {
    let Some(dialog) = &app.new_agent_dialog else {
        return;
    };

    let accent = theme.header_color;
    let filtered_clis = dialog.filtered_cli_indices();
    let field_width = prompt_field_width(frame.area());
    let dir_visible = visible_dir_rows(
        dialog,
        accent,
        &filtered_clis,
        field_width,
        theme,
        frame.area(),
    );
    let lines = build_dialog_lines(
        dialog,
        accent,
        &filtered_clis,
        field_width,
        theme,
        dir_visible,
    );
    let height = dialog_height(&lines, frame.area());

    let area = centered_rect(65, height, frame.area());
    frame.render_widget(Clear, area);
    draw_dialog_left_wave(frame, area, app.animation_tick.into());

    let block = Block::default()
        .title(dialog_title(dialog))
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

// ── Form state ───────────────────────────────────────────────────────────────

/// Total dialog height (content lines + 2 border rows), clamped to the frame.
fn dialog_height(lines: &[Line<'static>], frame_area: Rect) -> u16 {
    (lines.len() + 2).min(frame_area.height as usize) as u16
}

/// How many folder rows fit the frame: try the preferred window
/// (`DIR_BROWSER_VISIBLE`), and when the built dialog would overflow, shrink the
/// folder list by exactly the overflow — never below `MIN_DIR_BROWSER_VISIBLE` —
/// before any other row is cut. Returns 0 when the browser has nothing to list
/// (the `(no matches)` / absent browser states don't depend on this count).
pub(super) fn visible_dir_rows(
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    field_width: usize,
    theme: &Theme,
    frame_area: Rect,
) -> usize {
    let filtered = dialog.filtered_dir_entries();
    if filtered.is_empty() {
        return 0;
    }
    let preferred = filtered.len().min(DIR_BROWSER_VISIBLE);
    let inner = frame_area.height.saturating_sub(2) as usize;
    let full =
        build_dialog_lines(dialog, accent, filtered_clis, field_width, theme, preferred).len();
    if full <= inner {
        return preferred;
    }
    preferred
        .saturating_sub(full - inner)
        .max(MIN_DIR_BROWSER_VISIBLE)
}

fn dialog_title(dialog: &NewAgentDialog) -> &'static str {
    if !dialog.is_edit_mode() {
        return " New Agent ";
    }

    match dialog.task_type {
        NewTaskType::Background => " Edit Background ",
        NewTaskType::Interactive => " Edit Agent ",
        NewTaskType::Terminal => " Edit Terminal ",
    }
}

fn task_type_label(task_type: NewTaskType) -> &'static str {
    match task_type {
        NewTaskType::Interactive => "Interactive",
        NewTaskType::Terminal => "Terminal",
        NewTaskType::Background => "Background",
    }
}

fn interactive_mode_label(task_mode: NewTaskMode) -> &'static str {
    match task_mode {
        NewTaskMode::Interactive => "New",
        NewTaskMode::Resume => "Resume",
    }
}

fn background_trigger_label(trigger: BackgroundTrigger) -> &'static str {
    match trigger {
        BackgroundTrigger::Cron => "Cron",
        BackgroundTrigger::Watch => "Watch",
    }
}

fn help_text(dialog: &NewAgentDialog) -> &'static str {
    match dialog.task_type {
        NewTaskType::Interactive => {
            "  ↑↓: fields · Shift+↑↓: navigate  (in dirs: → enter  ← up) · Enter: launch · Esc: cancel"
        }
        NewTaskType::Background => {
            "  ↑↓: fields · Shift+↑↓: navigate  (in dirs: → enter  ← up) · Enter: create · Esc: cancel"
        }
        NewTaskType::Terminal => {
            "  ↑↓: fields · Shift+↑↓: navigate  (in dirs: → enter  ← up) · Enter: launch · Esc: cancel"
        }
    }
}

fn session_picker_label(dialog: &NewAgentDialog) -> String {
    let Some((_, title)) = &dialog.selected_session else {
        return "  ↵ pick session  (latest)".to_string();
    };

    format!("  ↵ pick  [{}]", truncate_with_ellipsis(title, 40))
}

pub(super) fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let shortened: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

// ── Rendering helpers ────────────────────────────────────────────────────────

fn build_dialog_lines(
    dialog: &NewAgentDialog,
    accent: Color,
    filtered_clis: &[usize],
    field_width: usize,
    theme: &Theme,
    dir_visible: usize,
) -> Vec<Line<'static>> {
    let layout = FieldLayout::for_task(dialog.task_type);
    let mut lines = dialog_header_lines(dialog, accent, theme);
    let section = SectionLayout {
        fields: layout,
        field_width,
        dir_visible,
    };

    match dialog.task_type {
        NewTaskType::Interactive => {
            append_interactive_sections(&mut lines, dialog, accent, filtered_clis, theme, section);
        }
        NewTaskType::Terminal => {
            append_terminal_sections(&mut lines, dialog, accent, theme, dir_visible);
        }
        NewTaskType::Background => {
            append_background_sections(&mut lines, dialog, accent, filtered_clis, theme, section);
        }
    }

    lines.push(Line::from(Span::styled(
        help_text(dialog),
        Style::default().fg(theme.dim_text),
    )));
    lines
}

fn dialog_header_lines(
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut type_row = vec![Span::styled(
        "  Type:  ",
        Style::default().fg(theme.dim_text),
    )];
    type_row.extend(selector_spans(
        task_type_label(dialog.task_type),
        TYPE_FIELD,
        dialog.is_edit_mode(),
        accent,
        dialog.field,
        theme,
    ));
    vec![Line::from(""), Line::from(type_row), Line::from("")]
}

fn selector_spans(
    value: &str,
    field: usize,
    locked: bool,
    accent: Color,
    current_field: usize,
    theme: &Theme,
) -> Vec<Span<'static>> {
    if locked {
        return vec![Span::styled(
            format!("  {value}  "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )];
    }

    // Discreet ‹ › lateral selectors, matching the prompt builder's send
    // control: arrows are dim when the field is unfocused and take the accent
    // color when it is focused; the value keeps the field's focus styling.
    let arrow_style = if current_field == field {
        Style::default().fg(accent)
    } else {
        Style::default().fg(theme.dim_text)
    };
    vec![
        Span::styled(" ‹ ", arrow_style),
        Span::styled(value.to_string(), focus_style(current_field, field, accent)),
        Span::styled(" › ", arrow_style),
    ]
}

fn focus_style(current_field: usize, field: usize, accent: Color) -> Style {
    if current_field == field {
        return Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD);
    }

    Style::default().fg(Color::White)
}

fn picker_item_style(accent: Color, selected: bool) -> Style {
    if selected {
        return Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD);
    }

    Style::default().fg(Color::White)
}

fn picker_detail_style(selected: bool, selected_style: Style, theme: &Theme) -> Style {
    if selected {
        selected_style
    } else {
        Style::default().fg(theme.dim_text)
    }
}

fn filter_row(
    prefix: &str,
    filter: &str,
    focused: bool,
    accent: Color,
    theme: &Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            prefix.to_string(),
            if focused {
                Style::default().fg(accent)
            } else {
                Style::default().fg(theme.dim_text)
            },
        ),
        Span::styled(
            filter_display(filter),
            if filter.is_empty() {
                Style::default().fg(theme.dim_text)
            } else {
                Style::default().fg(Color::White)
            },
        ),
    ])
}

fn filter_display(filter: &str) -> String {
    if filter.is_empty() {
        "type to filter".to_string()
    } else {
        filter.to_string()
    }
}

fn push_spaced_row(lines: &mut Vec<Line<'static>>, row: Line<'static>) {
    lines.push(row);
    lines.push(Line::from(""));
}

// ── Section builders ─────────────────────────────────────────────────────────

pub(super) fn interactive_mode_row(
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "  Session:  ",
        Style::default().fg(theme.dim_text),
    )];
    spans.extend(selector_spans(
        interactive_mode_label(dialog.task_mode),
        INTERACTIVE_MODE_FIELD,
        false,
        accent,
        dialog.field,
        theme,
    ));

    if dialog.resume_unconfigured() && !dialog.has_session_picker() {
        spans.push(Span::styled(
            "  (not configured — falls back to new)",
            Style::default().fg(Color::Yellow),
        ));
    }

    if matches!(dialog.task_mode, NewTaskMode::Resume) && dialog.has_session_picker() {
        spans.push(Span::styled(
            session_picker_label(dialog),
            Style::default().fg(Color::Cyan),
        ));
    }

    if matches!(dialog.task_mode, NewTaskMode::Resume) {
        spans.push(Span::styled(
            session_resume_label(dialog),
            Style::default().fg(if dialog.resume_sessions_empty {
                Color::Yellow
            } else {
                Color::Cyan
            }),
        ));
    }

    Line::from(spans)
}

pub(super) fn model_value(dialog: &NewAgentDialog) -> String {
    if dialog.model.is_empty() {
        "(optional — Space to browse)".to_string()
    } else {
        format!("{}▏", dialog.model)
    }
}

/// Number of visual lines the prompt occupies at `field_width`. Empty prompts
/// count as 1 line so the cursor / placeholder always have a home.
pub(crate) fn prompt_visual_line_count(dialog: &NewAgentDialog, field_width: usize) -> usize {
    if dialog.prompt.is_empty() {
        return 1;
    }
    let field_width = field_width.max(1);
    let mut lines = 1usize;
    let mut col = 0usize;
    for ch in dialog.prompt.chars() {
        if ch == '\n' {
            lines += 1;
            col = 0;
            continue;
        }
        if col >= field_width {
            lines += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    lines
}

/// Mirrors the calculation in `event::prompt_template::prompt_field_width` so
/// the renderer's wrap math, the cursor's visual line index, and the input
/// handler's `Up`/`Down` row jumps all agree on narrow terminals.
pub(crate) fn prompt_field_width(frame_area: Rect) -> usize {
    let term_width = frame_area.width.max(1);
    let max_dialog_w = term_width.saturating_sub(2).max(1);
    let preferred_dialog_w = term_width.saturating_mul(65) / 100;
    let min_dialog_w = 40u16.min(max_dialog_w);
    let dialog_width = preferred_dialog_w.clamp(min_dialog_w, max_dialog_w);
    // dialog borders (2) + content indent (2) + 1 cell breathing room
    (dialog_width.saturating_sub(5) as usize).max(10)
}

pub(crate) const PROMPT_VISIBLE_ROWS: usize = 3;

pub(super) fn cron_value(dialog: &NewAgentDialog) -> String {
    if dialog.cron_expr.is_empty() {
        " * * * * *  (min hr dom mon dow · local time)".to_string()
    } else {
        format!(" {}▏ (local time)", dialog.cron_expr)
    }
}

pub(super) fn identity_label(dialog: &NewAgentDialog) -> String {
    match dialog.seed_options.get(dialog.seed_index) {
        Some(SeedOption::Seed { name, .. }) => truncate_with_ellipsis(name, 30),
        Some(SeedOption::PlantNewSeed) => "🌱 New".to_string(),
        _ => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::canopy_config::CanopyConfig;
    use crate::tui::app::dialog::new_agent::NewAgentDialog;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn dialog_with(prompt: &str) -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some("."));
        d.prompt = prompt.to_string();
        d.prompt_cursor = prompt.chars().count();
        d
    }

    #[test]
    fn empty_prompt_counts_as_one_visual_line() {
        let d = dialog_with("");
        assert_eq!(prompt_visual_line_count(&d, 30), 1);
    }

    #[test]
    fn single_short_line_counts_as_one_visual_line() {
        let d = dialog_with("hello world");
        assert_eq!(prompt_visual_line_count(&d, 30), 1);
    }

    #[test]
    fn hard_newline_breaks_visual_line() {
        let d = dialog_with("first\nsecond");
        assert_eq!(prompt_visual_line_count(&d, 30), 2);
    }

    #[test]
    fn overflow_wraps_at_field_width() {
        let d = dialog_with("abcdefghij"); // 10 chars
        assert_eq!(prompt_visual_line_count(&d, 4), 3); // 4 + 4 + 2
    }

    #[test]
    fn visual_line_range_respects_hard_newline_boundary() {
        let d = dialog_with("abc\ndef");
        assert_eq!(prompt_visual_line_range(&d, 0, 30), (0, 3));
        assert_eq!(prompt_visual_line_range(&d, 1, 30), (4, 7));
    }

    #[test]
    fn visual_line_range_respects_soft_wrap_boundary() {
        let d = dialog_with("abcdefghij");
        assert_eq!(prompt_visual_line_range(&d, 0, 4), (0, 4));
        assert_eq!(prompt_visual_line_range(&d, 1, 4), (4, 8));
        assert_eq!(prompt_visual_line_range(&d, 2, 4), (8, 10));
    }

    #[test]
    fn out_of_range_line_returns_past_end() {
        let d = dialog_with("abc");
        assert_eq!(prompt_visual_line_range(&d, 5, 30), (3, 3));
    }

    #[test]
    fn prompt_field_width_clamps_to_terminal_width() {
        use ratatui::layout::Rect;
        // Tiny terminal: 30 cols → preferred 19, min 30, clamp 30. minus 5 = 25.
        let w = prompt_field_width(Rect::new(0, 0, 30, 20));
        assert!(w <= 25);
        assert!(w >= 10);
    }

    #[test]
    fn prompt_field_width_grows_with_terminal() {
        use ratatui::layout::Rect;
        let w_small = prompt_field_width(Rect::new(0, 0, 80, 20));
        let w_large = prompt_field_width(Rect::new(0, 0, 200, 20));
        assert!(w_large > w_small);
    }

    #[test]
    fn background_dialog_height_includes_prompt_block() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Background;
        let theme = Theme::classic();
        let frame_area = Rect::new(0, 0, 200, 50);
        let visible = visible_dir_rows(&d, Color::White, &[], 40, &theme, frame_area);
        let lines = build_dialog_lines(&d, Color::White, &[], 40, &theme, visible);
        let h = dialog_height(&lines, frame_area);
        // 16 base rows + 1 label + 3 prompt rows + 2 borders = 22, well above the
        // old floor of 19; the point is the prompt block is no longer eaten.
        assert!(h >= 22, "expected >= 22, got {h}");
    }

    fn spans_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn selector_uses_discreet_angle_glyphs_not_solid_triangles() {
        // Focused and unfocused variants both use the ‹ › glyphs, never ◀ ▶.
        for current in [TYPE_FIELD, INTERACTIVE_MODE_FIELD] {
            let text = spans_text(&selector_spans(
                "Background",
                TYPE_FIELD,
                false,
                Color::Cyan,
                current,
                &Theme::classic(),
            ));
            assert!(text.contains('‹') && text.contains('›'), "got {text:?}");
            assert!(!text.contains('◀') && !text.contains('▶'), "got {text:?}");
            assert!(text.contains("Background"));
        }
    }

    #[test]
    fn locked_selector_has_no_lateral_arrows() {
        let text = spans_text(&selector_spans(
            "Interactive",
            TYPE_FIELD,
            true,
            Color::Cyan,
            TYPE_FIELD,
            &Theme::classic(),
        ));
        assert!(!text.contains('‹') && !text.contains('›'));
        assert!(!text.contains('◀') && !text.contains('▶'));
    }

    #[test]
    fn dialog_title_new_agent() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Interactive;
        assert_eq!(dialog_title(&d), " New Agent ");
    }

    #[test]
    fn dialog_title_edit_background() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Background;
        d.edit_id = Some("test-id".to_string());
        assert_eq!(dialog_title(&d), " Edit Background ");
    }

    #[test]
    fn dialog_title_edit_interactive() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Interactive;
        d.edit_id = Some("test-id".to_string());
        assert_eq!(dialog_title(&d), " Edit Agent ");
    }

    #[test]
    fn dialog_title_edit_terminal() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Terminal;
        d.edit_id = Some("test-id".to_string());
        assert_eq!(dialog_title(&d), " Edit Terminal ");
    }

    #[test]
    fn task_type_label_all_variants() {
        assert_eq!(task_type_label(NewTaskType::Interactive), "Interactive");
        assert_eq!(task_type_label(NewTaskType::Terminal), "Terminal");
        assert_eq!(task_type_label(NewTaskType::Background), "Background");
    }

    #[test]
    fn interactive_mode_label_all_variants() {
        assert_eq!(interactive_mode_label(NewTaskMode::Interactive), "New");
        assert_eq!(interactive_mode_label(NewTaskMode::Resume), "Resume");
    }

    #[test]
    fn background_trigger_label_all_variants() {
        assert_eq!(background_trigger_label(BackgroundTrigger::Cron), "Cron");
        assert_eq!(background_trigger_label(BackgroundTrigger::Watch), "Watch");
    }

    #[test]
    fn filter_display_empty() {
        assert_eq!(filter_display(""), "type to filter");
    }

    #[test]
    fn filter_display_non_empty() {
        assert_eq!(filter_display("claude"), "claude");
    }

    #[test]
    fn truncate_with_ellipsis_short() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn truncate_with_ellipsis_exact() {
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_with_ellipsis_long() {
        let result = truncate_with_ellipsis("hello world", 5);
        assert!(result.contains("…"));
        assert!(result.chars().count() <= 6); // 5 + ellipsis
    }

    #[test]
    fn cron_value_empty() {
        let mut d = NewAgentDialog::new(Some("."));
        d.cron_expr = String::new();
        let val = cron_value(&d);
        assert!(val.contains("* * * * *"));
    }

    #[test]
    fn cron_value_with_expr() {
        let mut d = NewAgentDialog::new(Some("."));
        d.cron_expr = "30 9 * * 1-5".to_string();
        let val = cron_value(&d);
        assert!(val.contains("30 9 * * 1-5"));
    }

    #[test]
    fn help_text_interactive() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Interactive;
        let text = help_text(&d);
        assert!(text.contains("launch"));
    }

    #[test]
    fn help_text_background() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Background;
        let text = help_text(&d);
        assert!(text.contains("create"));
    }

    #[test]
    fn help_text_terminal() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_type = NewTaskType::Terminal;
        let text = help_text(&d);
        assert!(text.contains("launch"));
    }

    #[test]
    fn model_value_empty() {
        let mut d = NewAgentDialog::new(Some("."));
        d.model = String::new();
        let val = model_value(&d);
        assert!(val.contains("optional"));
    }

    #[test]
    fn model_value_with_model() {
        let mut d = NewAgentDialog::new(Some("."));
        d.model = "claude-3-opus".to_string();
        let val = model_value(&d);
        assert!(val.contains("claude-3-opus"));
    }

    #[test]
    fn identity_label_with_seed() {
        use crate::tui::app::dialog::new_agent::SeedOption;
        let mut d = NewAgentDialog::new(Some("."));
        d.seed_options = vec![SeedOption::Seed {
            name: "My Seed".to_string(),
            id: "seed-1".to_string(),
        }];
        let label = identity_label(&d);
        assert_eq!(label, "My Seed");
    }

    #[test]
    fn identity_label_plant_new() {
        use crate::tui::app::dialog::new_agent::SeedOption;
        let mut d = NewAgentDialog::new(Some("."));
        d.seed_options = vec![SeedOption::PlantNewSeed];
        let label = identity_label(&d);
        assert!(label.contains("New"));
    }

    #[test]
    fn identity_label_none() {
        let mut d = NewAgentDialog::new(Some("."));
        d.seed_options = vec![];
        let label = identity_label(&d);
        assert_eq!(label, "None");
    }

    #[test]
    fn field_layout_for_interactive() {
        let layout = FieldLayout::for_task(NewTaskType::Interactive);
        assert_eq!(layout.cli, 2);
        assert_eq!(layout.identity, 5);
    }

    #[test]
    fn field_layout_for_terminal() {
        let layout = FieldLayout::for_task(NewTaskType::Terminal);
        assert_eq!(layout.cli, 0);
        assert_eq!(layout.identity, 0);
    }

    #[test]
    fn field_layout_for_background() {
        let layout = FieldLayout::for_task(NewTaskType::Background);
        assert_eq!(layout.cli, 2);
        assert_eq!(layout.identity, 0);
    }

    #[test]
    fn picker_window_basic() {
        let w = PickerWindow::new(5, 20, 6);
        assert_eq!(w.scroll, 0);
        assert!(!w.has_above);
        assert!(w.has_below);
    }

    #[test]
    fn picker_window_scrolled() {
        let w = PickerWindow::new(15, 20, 6);
        assert!(w.scroll > 0);
        assert!(w.has_above);
    }

    #[test]
    fn picker_window_at_end() {
        let w = PickerWindow::new(19, 20, 6);
        assert!(!w.has_below);
    }

    #[test]
    fn picker_window_empty() {
        let w = PickerWindow::new(0, 0, 6);
        assert!(!w.has_above);
        assert!(!w.has_below);
    }

    #[test]
    fn prompt_field_width_very_small_terminal() {
        let w = prompt_field_width(ratatui::layout::Rect::new(0, 0, 10, 10));
        assert!(w >= 10);
    }

    #[test]
    fn prompt_field_width_large_terminal() {
        let w = prompt_field_width(ratatui::layout::Rect::new(0, 0, 200, 40));
        assert!(w > 50);
    }

    #[test]
    fn prompt_visual_line_count_mixed_wrap_and_newlines() {
        let d = dialog_with("abcde\n12345678");
        // "abcde" = 1 line, "12345678" at width 4 = 2 lines (4+4)
        // But hard newline counts as a break point too
        let count = prompt_visual_line_count(&d, 4);
        assert!(count >= 3, "expected >= 3, got {count}");
    }

    #[test]
    fn prompt_visual_line_range_at_boundary() {
        let d = dialog_with("abcdefghij");
        // At width 4: line 0 = [0,4), line 1 = [4,8), line 2 = [8,10)
        assert_eq!(prompt_visual_line_range(&d, 2, 4), (8, 10));
    }

    #[test]
    fn session_picker_label_no_selection() {
        let mut d = NewAgentDialog::new(Some("."));
        d.selected_session = None;
        let label = session_picker_label(&d);
        assert!(label.contains("pick session"));
    }

    #[test]
    fn session_picker_label_with_selection() {
        let mut d = NewAgentDialog::new(Some("."));
        d.selected_session = Some(("id-1".to_string(), "My Session".to_string()));
        let label = session_picker_label(&d);
        assert!(label.contains("My Session"));
    }

    #[test]
    fn focus_style_focused() {
        let style = focus_style(0, 0, Color::Cyan);
        assert_eq!(style.bg, Some(Color::Cyan));
    }

    #[test]
    fn focus_style_unfocused() {
        let style = focus_style(1, 0, Color::Cyan);
        assert_eq!(style.fg, Some(Color::White));
    }

    #[test]
    fn picker_item_style_selected() {
        let style = picker_item_style(Color::Cyan, true);
        assert_eq!(style.bg, Some(Color::Cyan));
    }

    #[test]
    fn picker_item_style_not_selected() {
        let style = picker_item_style(Color::Cyan, false);
        assert_eq!(style.fg, Some(Color::White));
    }

    #[test]
    fn picker_detail_style_selected() {
        let selected_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        let style = picker_detail_style(true, selected_style, &Theme::classic());
        assert_eq!(style.bg, Some(Color::Cyan));
    }

    #[test]
    fn picker_detail_style_not_selected() {
        let selected_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        let theme = Theme::classic();
        let style = picker_detail_style(false, selected_style, &theme);
        assert_eq!(style.fg, Some(theme.dim_text));
    }

    #[test]
    fn session_picker_label_truncates_long_title() {
        let mut d = NewAgentDialog::new(Some("."));
        d.selected_session = Some(("id".to_string(), "A".repeat(100)));
        let label = session_picker_label(&d);
        assert!(label.chars().count() < 60);
    }

    #[test]
    fn interactive_mode_row_resume_unconfigured() {
        let mut d = NewAgentDialog::new(Some("."));
        d.task_mode = NewTaskMode::Resume;
        d.session_entries = vec![];
        d.cli_configs = vec![None];
        d.cli_index = 0;
        let line = interactive_mode_row(&d, Color::Cyan, &Theme::classic());
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("not configured"));
    }

    #[test]
    fn push_spaced_row_adds_empty_line() {
        let mut lines = Vec::new();
        push_spaced_row(&mut lines, Line::from("test"));
        assert_eq!(lines.len(), 2);
        assert!(lines[1].spans.is_empty());
    }

    /// task_type_label returns correct labels for all task types
    #[test]
    fn task_type_label_interactive() {
        assert_eq!(task_type_label(NewTaskType::Interactive), "Interactive");
    }

    #[test]
    fn task_type_label_terminal() {
        assert_eq!(task_type_label(NewTaskType::Terminal), "Terminal");
    }

    #[test]
    fn task_type_label_background() {
        assert_eq!(task_type_label(NewTaskType::Background), "Background");
    }

    /// interactive_mode_label returns correct labels for modes
    #[test]
    fn interactive_mode_label_new() {
        assert_eq!(interactive_mode_label(NewTaskMode::Interactive), "New");
    }

    #[test]
    fn interactive_mode_label_resume() {
        assert_eq!(interactive_mode_label(NewTaskMode::Resume), "Resume");
    }

    /// background_trigger_label returns correct labels for triggers
    #[test]
    fn background_trigger_label_cron() {
        assert_eq!(background_trigger_label(BackgroundTrigger::Cron), "Cron");
    }

    #[test]
    fn background_trigger_label_watch() {
        assert_eq!(background_trigger_label(BackgroundTrigger::Watch), "Watch");
    }

    /// truncate_with_ellipsis doesn't truncate short strings
    #[test]
    fn truncate_with_ellipsis_short_string() {
        let result = truncate_with_ellipsis("hello", 10);
        assert_eq!(result, "hello");
    }

    /// truncate_with_ellipsis adds ellipsis when truncating
    #[test]
    fn truncate_with_ellipsis_long_string() {
        let result = truncate_with_ellipsis("hello world test", 5);
        assert_eq!(result, "hello…");
    }

    /// truncate_with_ellipsis exact length has no ellipsis
    #[test]
    fn truncate_with_ellipsis_exact_length() {
        let result = truncate_with_ellipsis("hello", 5);
        assert_eq!(result, "hello");
    }

    /// truncate_with_ellipsis handles empty string
    #[test]
    fn truncate_with_ellipsis_empty_string() {
        let result = truncate_with_ellipsis("", 5);
        assert_eq!(result, "");
    }

    /// truncate_with_ellipsis handles zero max_chars
    #[test]
    fn truncate_with_ellipsis_zero_max_chars() {
        let result = truncate_with_ellipsis("hello", 0);
        assert_eq!(result, "…");
    }

    /// truncate_with_ellipsis handles unicode characters correctly by char count
    #[test]
    fn truncate_with_ellipsis_unicode() {
        // "こんにちは" is 5 chars; taking 3 gives "こんに" + ellipsis
        let result = truncate_with_ellipsis("こんにちは世界", 3);
        assert_eq!(result, "こんに…");
    }

    /// filter_display handles special characters
    #[test]
    fn filter_display_special_chars() {
        assert_eq!(filter_display("test@#$%"), "test@#$%");
    }

    #[test]
    fn dialog_renders_no_effort_field() {
        let mut d = NewAgentDialog::new(Some("."));
        // Interactive dialog
        d.task_type = NewTaskType::Interactive;
        let lines = build_dialog_lines(
            &d,
            Color::White,
            &[],
            40,
            &Theme::classic(),
            DIR_BROWSER_VISIBLE,
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !text.contains("Effort"),
            "interactive dialog must not show effort field"
        );

        // Background dialog
        d.task_type = NewTaskType::Background;
        let lines = build_dialog_lines(
            &d,
            Color::White,
            &[],
            40,
            &Theme::classic(),
            DIR_BROWSER_VISIBLE,
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !text.contains("Effort"),
            "background dialog must not show effort field"
        );
    }

    /// (CM32) The dialog renders the product to the person: both the
    /// selected-harness line and the picker rows show
    /// `provider · tool_name (slug)`, never the bare slug for a platform
    /// the registry describes. Storage stays the slug (`selected_cli`),
    /// proven in the state-level test.
    #[test]
    fn cli_section_renders_product_labels() {
        use crate::domain::cli_config::CliConfig;
        use crate::domain::models::Cli;
        let mut d = NewAgentDialog::new(Some("."));
        d.available_clis = vec![Cli::new("claude"), Cli::new("mistral")];
        d.cli_configs = vec![
            Some(CliConfig {
                name: "claude".to_string(),
                provider: Some("Anthropic".to_string()),
                tool_name: Some("Claude Code".to_string()),
                ..Default::default()
            }),
            Some(CliConfig {
                name: "mistral".to_string(),
                provider: Some("Mistral AI".to_string()),
                tool_name: Some("Vibe".to_string()),
                ..Default::default()
            }),
        ];
        d.cli_picker_open = true;
        let lines = build_dialog_lines(
            &d,
            Color::White,
            &[0, 1],
            40,
            &Theme::classic(),
            DIR_BROWSER_VISIBLE,
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains("Anthropic \u{b7} Claude Code (claude)"),
            "picker must name the product: {text}"
        );
        assert!(
            text.contains("Mistral AI \u{b7} Vibe (mistral)"),
            "picker must name the product: {text}"
        );
    }

    /// A rendered dialog row: its text, and the bg color of every cell.
    type RenderedRow = (String, Vec<Option<Color>>);

    fn render_dialog(width: u16, height: u16, app: &App) -> Vec<RenderedRow> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| draw_new_agent_dialog(frame, app, &theme))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut text = String::new();
                let mut bgs = Vec::new();
                for x in 0..buffer.area.width {
                    let cell = &buffer[(x, y)];
                    text.push_str(cell.symbol());
                    bgs.push(Some(cell.bg));
                }
                (text, bgs)
            })
            .collect()
    }

    fn join_rows(rows: &[RenderedRow]) -> String {
        rows.iter()
            .map(|(t, _)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn cell_grid(width: u16, height: u16, app: &App) -> Vec<Vec<(Color, Color)>> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| draw_new_agent_dialog(frame, app, &theme))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        (cell.fg, cell.bg)
                    })
                    .collect()
            })
            .collect()
    }

    fn app_with_dialog(dialog: NewAgentDialog) -> App {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let path = db_file.path().to_path_buf();
        std::mem::forget(db_file); // keep the sqlite file alive for the test
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempdir().unwrap();
        let mut app = App::new(db, data_dir.path(), &CanopyConfig::default()).unwrap();
        app.new_agent_dialog = Some(dialog);
        app
    }

    fn root_with_dirs(names: &[&str]) -> tempfile::TempDir {
        let root = tempdir().unwrap();
        for n in names {
            std::fs::create_dir(root.path().join(n)).unwrap();
        }
        root
    }

    #[test]
    fn all_six_folders_and_footer_are_drawn_in_a_tall_frame() {
        let names = ["Academic", "Projects", "bin", "go", "nltk_data", "temp"];
        let root = root_with_dirs(&names);
        let dialog = NewAgentDialog::new(Some(root.path().to_string_lossy().as_ref()));
        let app = app_with_dialog(dialog);
        let rows = render_dialog(200, 50, &app);
        let text = join_rows(&rows);
        for n in names {
            assert!(
                text.contains(&format!("{n}/")),
                "folder {n} must be drawn inside the dialog, screen was:\n{text}"
            );
        }
        assert!(
            text.contains("1/6"),
            "footer 1/6 missing, screen was:\n{text}"
        );
    }

    #[test]
    fn scrolled_selection_tenth_of_twelve_is_drawn_and_highlighted() {
        let names: Vec<String> = (0..12).map(|i| format!("dir{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let root = root_with_dirs(&refs);
        let mut dialog = NewAgentDialog::new(Some(root.path().to_string_lossy().as_ref()));
        // 9× ↓ from 0: the list handler ends in selection index 9.
        dialog.dir_selected = 9;
        let accent = Theme::classic().header_color;
        let app = app_with_dialog(dialog);
        let rows = render_dialog(200, 50, &app);
        let text = join_rows(&rows);
        assert!(
            text.contains("dir09/"),
            "10th folder must be drawn, screen was:\n{text}"
        );
        let row = rows
            .iter()
            .find(|(t, _)| t.contains("dir09/"))
            .expect("dir09 row present");
        assert!(
            row.1.contains(&Some(accent)),
            "dir09 row must carry the selected accent background"
        );
        assert!(
            text.contains("10/12"),
            "footer 10/12 missing, screen was:\n{text}"
        );
    }

    #[test]
    fn folder_list_shrinks_to_three_rows_in_a_short_frame() {
        let names: Vec<String> = (0..12).map(|i| format!("dir{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let root = root_with_dirs(&refs);
        let mut dialog = NewAgentDialog::new(Some(root.path().to_string_lossy().as_ref()));
        dialog.dir_selected = 5; // selection must stay on a drawn row even at the floor
        let app = app_with_dialog(dialog);
        let rows = render_dialog(200, 24, &app);
        let text = join_rows(&rows);
        // 200x24 fits exactly: 15 chrome lines + filter + 3 entries + footer + blank
        // + help = 22 content lines, +2 borders = 24 (visible_dir_rows shrinks 10→3).
        assert!(
            text.contains("6/12"),
            "footer must show selected/total: {text}"
        );
        let drawn = names
            .iter()
            .filter(|n| text.contains(&format!("{n}/")))
            .count();
        assert!(
            drawn >= 3,
            "at least 3 folder rows must be drawn, got {drawn}:\n{text}"
        );
        assert!(
            text.contains("dir05/"),
            "the selected entry must be among the drawn rows, screen was:\n{text}"
        );
    }

    #[test]
    fn no_folder_or_filter_glyphs_are_rendered() {
        let root = root_with_dirs(&["alpha", "bravo"]);
        let mut dialog = NewAgentDialog::new(Some(root.path().to_string_lossy().as_ref()));
        dialog.cli_picker_open = true; // the harness picker's filter row shares the glyph
        let app = app_with_dialog(dialog);
        let text = join_rows(&render_dialog(200, 50, &app));
        assert!(
            !text.contains('📁'),
            "no folder emoji may be rendered: {text}"
        );
        assert!(
            !text.contains('🔍'),
            "no magnifier glyph may be rendered: {text}"
        );
        assert!(
            text.contains("filter:"),
            "filter rows use the plain label: {text}"
        );
    }

    #[test]
    fn harness_accent_only_on_name() {
        use crate::domain::cli_config::CliConfig;
        use crate::domain::models::Cli;
        let mut dialog = NewAgentDialog::new(Some("."));
        dialog.available_clis = vec![Cli::new("claude")];
        dialog.cli_configs = vec![Some(CliConfig {
            name: "claude".to_string(),
            accent_color: Some([255, 0, 0]),
            ..Default::default()
        })];
        dialog.cli_index = 0;
        dialog.cli_picker_open = true;
        dialog.field = 2; // harness row focused, so the name carries its accent
        let label = dialog.cli_display_label(0);
        let app = app_with_dialog(dialog);
        let theme = Theme::classic();
        let header = theme.header_color;
        let harness = Color::Rgb(255, 0, 0);
        assert_ne!(harness, header);

        let rows = render_dialog(200, 50, &app);
        let grid = cell_grid(200, 50, &app);
        const BOX_GLYPHS: [char; 11] = ['┌', '┐', '└', '┘', '─', '│', '├', '┤', '┬', '┴', '┼'];

        let mut border_cells = 0;
        for (row, (text, _)) in rows.iter().enumerate() {
            for (x, ch) in text.chars().enumerate() {
                if BOX_GLYPHS.contains(&ch) && grid[row][x].0 == header {
                    border_cells += 1;
                }
            }
        }
        assert!(
            border_cells >= 1,
            "dialog border must use the canopy header color"
        );

        for (y, line) in grid.iter().enumerate() {
            for (x, &(_, bg)) in line.iter().enumerate() {
                assert_ne!(
                    bg, harness,
                    "cell ({x},{y}) must not use the harness accent as background"
                );
            }
        }

        let mut accent_rows = std::collections::BTreeSet::new();
        for (row, (text, _)) in rows.iter().enumerate() {
            for (x, _) in text.chars().enumerate() {
                if grid[row][x].0 == harness {
                    accent_rows.insert(row);
                    assert!(
                        text.contains(&label),
                        "harness accent fg is only for the harness name row, found on: {text:?}"
                    );
                }
            }
        }
        // Both the harness row's value and the picker row's own name carry the
        // accent, so at least two distinct rows must be painted with it.
        assert!(
            accent_rows.len() >= 2,
            "the harness name must carry its accent on the value row and on the \
             picker row (label {label:?}), found on rows {accent_rows:?}"
        );
    }
}
