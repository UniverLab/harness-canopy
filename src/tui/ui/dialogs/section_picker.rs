use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::centered_rect;
use super::ERROR_COLOR;
use crate::tui::app::dialog::{ProjectPickerEntry, SimplePromptDialog};
use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

// Old function removed - using simple prompt dialog instead
pub(crate) fn draw_section_picker_modal(
    frame: &mut Frame,
    app: &App,
    accent: Color,
    mode: &crate::tui::app::dialog::SectionPickerMode,
    theme: &Theme,
) {
    use crate::tui::app::dialog::SectionPickerMode;

    let Some(dialog) = &app.simple_prompt_dialog else {
        return;
    };

    match mode {
        SectionPickerMode::AddSection { selected } => {
            draw_add_section(frame, dialog, accent, *selected, theme);
        }
        SectionPickerMode::AddCustom { input } => {
            draw_add_custom(frame, input, accent, theme);
        }
        SectionPickerMode::RemoveSection { selected, scroll } => {
            draw_remove_section(frame, dialog, accent, *selected, *scroll, theme);
        }
        SectionPickerMode::SkillsPicker {
            selected,
            scroll,
            entries,
            ..
        } => {
            draw_skills_picker(frame, accent, *selected, *scroll, entries, theme);
        }
        SectionPickerMode::ProjectPicker {
            selected,
            entries,
            filter,
            scroll,
        } => {
            draw_project_picker(frame, accent, *selected, entries, filter, *scroll, theme);
        }
        SectionPickerMode::PresetPicker {
            selected,
            entries,
            filter,
        } => {
            draw_preset_picker(frame, accent, *selected, entries, filter, theme);
        }
        SectionPickerMode::None => {}
    }
}

/// Selection state shared by the project list body.
#[derive(Clone, Copy)]
struct PickerView {
    selected: usize,
    scroll: usize,
    visible_rows: usize,
}

/// Render the `🔍 <filter>│` line at the top of a picker body.
fn draw_filter_line(frame: &mut Frame, inner: Rect, filter: &str, theme: &Theme) {
    let mut filter_display = filter.to_string();
    filter_display.push('│');
    let filter_line = Line::from(vec![
        Span::styled("  🔍 ", Style::default().fg(theme.dim_text)),
        Span::styled(filter_display, Style::default().fg(theme.header_color)),
    ]);
    frame.render_widget(
        Paragraph::new(filter_line),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );
}

/// Render a single hint line on the last row of a picker body.
fn draw_hint_line(frame: &mut Frame, inner: Rect, hint: Line) {
    frame.render_widget(
        Paragraph::new(hint),
        Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );
}

/// Render a single dim gray message line at `y`.
fn draw_picker_message(frame: &mut Frame, inner: Rect, y: u16, msg: &str) {
    let line = Line::from(vec![Span::styled(
        msg,
        Style::default().fg(Color::DarkGray),
    )]);
    frame.render_widget(
        Paragraph::new(line),
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
    );
}

/// `Add Section` picker — the sections that can still be added.
fn draw_add_section(
    frame: &mut Frame,
    dialog: &SimplePromptDialog,
    accent: Color,
    selected: usize,
    theme: &Theme,
) {
    let addable = dialog.get_addable_sections();
    let height = (addable.len() as u16 + 4).min(15);
    let area = centered_rect(50, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Add Section ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    for (y_pos, (i, (_, label))) in (inner.y..).zip(addable.iter().enumerate()) {
        if y_pos >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        let is_selected = i == selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let line = Line::from(vec![Span::styled(format!("  {} ", label), style)]);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y: y_pos,
                width: inner.width,
                height: 1,
            },
        );
    }

    let hint = Line::from(vec![
        Span::styled("↑↓ ", Style::default().fg(theme.dim_text)),
        Span::styled("select  ", Style::default().fg(Color::White)),
        Span::styled("c ", Style::default().fg(theme.dim_text)),
        Span::styled("custom  ", Style::default().fg(Color::White)),
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("add  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    draw_hint_line(frame, inner, hint);
}

/// `Custom Section` prompt — a single free-text name field.
fn draw_add_custom(frame: &mut Frame, input: &str, accent: Color, theme: &Theme) {
    let area = centered_rect(50, 6, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Custom Section ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label_line = Line::from(vec![Span::styled("Name: ", Style::default().fg(accent))]);
    frame.render_widget(
        Paragraph::new(label_line),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        },
    );

    let mut display = input.to_string();
    display.push('│');
    let input_line = Line::from(vec![Span::styled(
        display,
        Style::default()
            .fg(theme.header_color)
            .bg(Color::Rgb(20, 35, 20)),
    )]);
    frame.render_widget(
        Paragraph::new(input_line),
        Rect {
            x: inner.x + 1,
            y: inner.y + 1,
            width: inner.width.saturating_sub(2),
            height: 1,
        },
    );

    let hint = Line::from(vec![
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("add  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    frame.render_widget(
        Paragraph::new(hint),
        Rect {
            x: inner.x,
            y: inner.y + 3,
            width: inner.width,
            height: 1,
        },
    );
}

/// `Remove Section` picker — the sections already present.
fn draw_remove_section(
    frame: &mut Frame,
    dialog: &SimplePromptDialog,
    accent: Color,
    selected: usize,
    scroll: usize,
    theme: &Theme,
) {
    let removable = dialog.get_removable_sections();
    let height = SimplePromptDialog::remove_section_box_height(removable.len());
    let visible_rows = SimplePromptDialog::remove_section_visible_rows(removable.len());
    let area = centered_rect(50, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Remove Section ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible = removable.iter().enumerate().skip(scroll).take(visible_rows);
    for (y_pos, (i, (_, display_label))) in (inner.y..).zip(visible) {
        if y_pos >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        let is_selected = i == selected;

        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(ERROR_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let line = Line::from(vec![Span::styled(format!("  {} ", display_label), style)]);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: inner.x,
                y: y_pos,
                width: inner.width,
                height: 1,
            },
        );
    }

    let hint = Line::from(vec![
        Span::styled("↑↓ ", Style::default().fg(theme.dim_text)),
        Span::styled("select  ", Style::default().fg(Color::White)),
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("remove  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    draw_hint_line(frame, inner, hint);
}

/// `Tools — Pick a Skill` picker — shows `[prefix]:name` per entry.
fn draw_skills_picker(
    frame: &mut Frame,
    accent: Color,
    selected: usize,
    scroll: usize,
    entries: &[(String, String, String)],
    theme: &Theme,
) {
    let height = SimplePromptDialog::skills_picker_box_height(entries.len());
    let visible_rows = SimplePromptDialog::skills_picker_visible_rows(entries.len());
    let area = centered_rect(55, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Tools — Pick a Skill ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(Color::Rgb(10, 20, 30)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if entries.is_empty() {
        draw_picker_message(frame, inner, inner.y, "  No skills found");
    } else {
        let visible = entries.iter().enumerate().skip(scroll).take(visible_rows);
        for (y_pos, (i, (_label, raw_name, prefix))) in (inner.y..).zip(visible) {
            if y_pos >= inner.y + inner.height.saturating_sub(1) {
                break;
            }
            let is_selected = i == selected;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            // Picker shows [prefix]:name for clarity (skill vs global)
            let display = format!("  [{prefix}]:{raw_name} ");
            frame.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(display, style)])),
                Rect {
                    x: inner.x,
                    y: y_pos,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }

    let hint = Line::from(vec![
        Span::styled("↑↓ ", Style::default().fg(theme.dim_text)),
        Span::styled("select  ", Style::default().fg(Color::White)),
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("add  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    draw_hint_line(frame, inner, hint);
}

/// `Project Context` picker — registered projects filtered by a substring.
fn draw_project_picker(
    frame: &mut Frame,
    accent: Color,
    selected: usize,
    entries: &[ProjectPickerEntry],
    filter: &str,
    scroll: usize,
    theme: &Theme,
) {
    let filtered = SimplePromptDialog::filtered_project_indices(entries, filter);
    let visible_rows = SimplePromptDialog::project_picker_visible_rows(filtered.len());
    let height = SimplePromptDialog::project_picker_box_height(filtered.len());
    let area = centered_rect(60, height, frame.area());
    frame.render_widget(Clear, area);

    let title = if filtered.len() > visible_rows {
        format!(
            " Project Context ({}/{}) ",
            selected.saturating_add(1),
            filtered.len()
        )
    } else {
        " Project Context ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(Color::Rgb(10, 20, 30)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    draw_filter_line(frame, inner, filter, theme);

    let list_y = inner.y + 1;
    if entries.is_empty() {
        draw_picker_message(frame, inner, list_y, "  No registered projects found");
    } else if filtered.is_empty() {
        draw_picker_message(frame, inner, list_y, "  no projects match");
    } else {
        let view = PickerView {
            selected,
            scroll,
            visible_rows,
        };
        draw_project_list(frame, inner, entries, &filtered, list_y, view, accent);
    }

    let hint = Line::from(vec![
        Span::styled("↑↓ ", Style::default().fg(theme.dim_text)),
        Span::styled("select  ", Style::default().fg(Color::White)),
        Span::styled("type ", Style::default().fg(theme.dim_text)),
        Span::styled("filter  ", Style::default().fg(Color::White)),
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("add  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    draw_hint_line(frame, inner, hint);
}

/// Scrolled body of the project list.
fn draw_project_list(
    frame: &mut Frame,
    inner: Rect,
    entries: &[ProjectPickerEntry],
    filtered: &[usize],
    list_y: u16,
    view: PickerView,
    accent: Color,
) {
    let visible = filtered
        .iter()
        .enumerate()
        .skip(view.scroll)
        .take(view.visible_rows);
    for (y_pos, (i, &idx)) in (list_y..).zip(visible) {
        if y_pos >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        let Some(entry) = entries.get(idx) else {
            continue;
        };
        let is_selected = i == view.selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let display = format!("  {} [{}] ", entry.name, entry.hash);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(display, style)])),
            Rect {
                x: inner.x,
                y: y_pos,
                width: inner.width,
                height: 1,
            },
        );
    }
}

/// `Preset` picker — saved prompt presets filtered by a substring.
fn draw_preset_picker(
    frame: &mut Frame,
    accent: Color,
    selected: usize,
    entries: &[(String, String, String)],
    filter: &str,
    theme: &Theme,
) {
    let filtered = SimplePromptDialog::filtered_preset_indices(entries, filter);
    let height = (filtered.len() as u16 + 6).min(17);
    let area = centered_rect(60, height.max(7), frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Preset ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(Color::Rgb(10, 20, 30)));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    draw_filter_line(frame, inner, filter, theme);

    let list_y = inner.y + 1;
    if entries.is_empty() {
        draw_picker_message(frame, inner, list_y, "  no presets in ~/.canopy/prompts");
    } else if filtered.is_empty() {
        draw_picker_message(frame, inner, list_y, "  no presets match");
    } else {
        draw_preset_list(frame, inner, list_y, entries, &filtered, selected, accent);
    }

    let hint = Line::from(vec![
        Span::styled("↑↓ ", Style::default().fg(theme.dim_text)),
        Span::styled("select  ", Style::default().fg(Color::White)),
        Span::styled("type ", Style::default().fg(theme.dim_text)),
        Span::styled("filter  ", Style::default().fg(Color::White)),
        Span::styled("Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("insert  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("cancel", Style::default().fg(Color::White)),
    ]);
    draw_hint_line(frame, inner, hint);
}

/// Body of the preset list — one row per filtered preset.
fn draw_preset_list(
    frame: &mut Frame,
    inner: Rect,
    list_y: u16,
    entries: &[(String, String, String)],
    filtered: &[usize],
    selected: usize,
    accent: Color,
) {
    for (y_pos, (i, &idx)) in (list_y..).zip(filtered.iter().enumerate()) {
        if y_pos >= inner.y + inner.height.saturating_sub(1) {
            break;
        }
        let (name, preview, _) = &entries[idx];
        let is_selected = i == selected;
        let style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let display = if preview.is_empty() {
            format!("  {name} ")
        } else {
            format!("  {name} — {preview} ")
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(display, style)])),
            Rect {
                x: inner.x,
                y: y_pos,
                width: inner.width,
                height: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::dialog::{SectionPickerMode, SimplePromptDialog};
    use crate::tui::app::types::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::Arc;

    fn make_app_with_prompt_dialog() -> App {
        use crate::db::Database;
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
        app.simple_prompt_dialog = Some(SimplePromptDialog::new());
        app
    }

    fn render_to_text(
        width: u16,
        height: u16,
        draw: impl FnOnce(&mut ratatui::Frame, ratatui::layout::Rect),
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
    fn draw_section_picker_none_does_nothing() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(frame, &app, Color::Cyan, &SectionPickerMode::None, &theme);
        });
        // None mode should render nothing from the modal
        assert!(!text.contains("Add Section"));
    }

    #[test]
    fn draw_section_picker_no_dialog_returns_early() {
        let mut app = make_app_with_prompt_dialog();
        app.simple_prompt_dialog = None;
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::AddSection { selected: 0 },
                &theme,
            );
        });
        // Without dialog, nothing renders
        assert!(!text.contains("Add Section"));
    }

    #[test]
    fn draw_add_section_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::AddSection { selected: 0 },
                &theme,
            );
        });
        assert!(
            text.contains("Add Section"),
            "Should render Add Section title: {text}"
        );
        assert!(text.contains("select"), "Should show hint: {text}");
    }

    #[test]
    fn draw_add_custom_section_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::AddCustom {
                    input: "my_section".to_string(),
                },
                &theme,
            );
        });
        assert!(
            text.contains("Custom Section"),
            "Should render Custom Section title: {text}"
        );
        assert!(text.contains("my_section"), "Should show input: {text}");
    }

    #[test]
    fn draw_add_custom_section_picker_empty_input() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::AddCustom {
                    input: String::new(),
                },
                &theme,
            );
        });
        assert!(
            text.contains("Custom Section"),
            "Should render Custom Section title: {text}"
        );
        // Empty input still shows the cursor
        assert!(text.contains('│'), "Should show cursor: {text}");
    }

    #[test]
    fn draw_remove_section_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::RemoveSection {
                    selected: 0,
                    scroll: 0,
                },
                &theme,
            );
        });
        assert!(
            text.contains("Remove Section"),
            "Should render Remove Section title: {text}"
        );
        assert!(text.contains("cancel"), "Should show cancel hint: {text}");
    }

    #[test]
    fn draw_skills_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![
            (
                "Skill A".to_string(),
                "skill_a".to_string(),
                "skill".to_string(),
            ),
            (
                "Skill B".to_string(),
                "skill_b".to_string(),
                "global".to_string(),
            ),
        ];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::SkillsPicker {
                    selected: 0,
                    scroll: 0,
                    entries,
                    replace_id: None,
                },
                &theme,
            );
        });
        assert!(text.contains("Tools"), "Should render Tools title: {text}");
        assert!(text.contains("skill_a"), "Should show skill name: {text}");
    }

    #[test]
    fn draw_skills_picker_empty() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::SkillsPicker {
                    selected: 0,
                    scroll: 0,
                    entries: vec![],
                    replace_id: None,
                },
                &theme,
            );
        });
        assert!(
            text.contains("No skills found"),
            "Should show empty message: {text}"
        );
    }

    /// Regression test for the reported bug: a scrolled-down selection must
    /// stay on screen instead of walking off the bottom of the tools menu.
    #[test]
    fn draw_skills_picker_scrolled_selection_stays_visible() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries: Vec<_> = (0..20)
            .map(|i| {
                (
                    format!("Skill {i}"),
                    format!("skill_{i}"),
                    "skill".to_string(),
                )
            })
            .collect();
        let visible_rows =
            crate::tui::app::dialog::SimplePromptDialog::skills_picker_visible_rows(20);
        let scroll = 20 - visible_rows;
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::SkillsPicker {
                    selected: 19,
                    scroll,
                    entries: entries.clone(),
                    replace_id: None,
                },
                &theme,
            );
        });
        assert!(
            text.contains("skill_19"),
            "Last entry must stay visible when scrolled: {text}"
        );
        assert!(
            !text.contains("skill_0 "),
            "First entry should have scrolled out of view: {text}"
        );
    }

    #[test]
    fn draw_project_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![crate::tui::app::dialog::ProjectPickerEntry {
            name: "My Project".to_string(),
            hash: "abc123".to_string(),
            path: "/tmp/myproject".to_string(),
        }];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::ProjectPicker {
                    selected: 0,
                    entries,
                    filter: String::new(),
                    scroll: 0,
                },
                &theme,
            );
        });
        assert!(
            text.contains("Project Context"),
            "Should render Project Context title: {text}"
        );
        assert!(
            text.contains("My Project"),
            "Should show project name: {text}"
        );
    }

    #[test]
    fn draw_project_picker_empty() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::ProjectPicker {
                    selected: 0,
                    entries: vec![],
                    filter: String::new(),
                    scroll: 0,
                },
                &theme,
            );
        });
        assert!(
            text.contains("No registered projects"),
            "Should show empty message: {text}"
        );
    }

    #[test]
    fn draw_project_picker_shows_filter_text() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![crate::tui::app::dialog::ProjectPickerEntry {
            name: "My Project".to_string(),
            hash: "abc123".to_string(),
            path: "/tmp/myproject".to_string(),
        }];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::ProjectPicker {
                    selected: 0,
                    entries,
                    filter: "myp".to_string(),
                    scroll: 0,
                },
                &theme,
            );
        });
        assert!(text.contains("myp"), "Should show filter text: {text}");
    }

    #[test]
    fn draw_project_picker_filter_matches_nothing() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![crate::tui::app::dialog::ProjectPickerEntry {
            name: "My Project".to_string(),
            hash: "abc123".to_string(),
            path: "/tmp/myproject".to_string(),
        }];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::ProjectPicker {
                    selected: 0,
                    entries,
                    filter: "zzz".to_string(),
                    scroll: 0,
                },
                &theme,
            );
        });
        assert!(
            text.contains("no projects match"),
            "Should show no-match message: {text}"
        );
    }

    #[test]
    fn draw_project_picker_scrolled_selection_stays_visible() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries: Vec<_> = (0..20)
            .map(|i| crate::tui::app::dialog::ProjectPickerEntry {
                name: format!("proj{i}"),
                hash: format!("h{i}"),
                path: format!("/proj{i}"),
            })
            .collect();
        let visible_rows =
            crate::tui::app::dialog::SimplePromptDialog::project_picker_visible_rows(20);
        let scroll = 20 - visible_rows;
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::ProjectPicker {
                    selected: 19,
                    entries: entries.clone(),
                    filter: String::new(),
                    scroll,
                },
                &theme,
            );
        });
        assert!(
            text.contains("proj19"),
            "Last entry must stay visible when scrolled: {text}"
        );
        assert!(
            !text.contains("proj0 "),
            "First entry should have scrolled out of view: {text}"
        );
        assert!(
            text.contains("(20/20)"),
            "Should show a scroll position indicator: {text}"
        );
    }

    #[test]
    fn draw_preset_picker() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![
            (
                "preset_a".to_string(),
                "Preview A".to_string(),
                String::new(),
            ),
            (
                "preset_b".to_string(),
                "Preview B".to_string(),
                String::new(),
            ),
        ];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::PresetPicker {
                    selected: 0,
                    entries,
                    filter: String::new(),
                },
                &theme,
            );
        });
        assert!(
            text.contains("Preset"),
            "Should render Preset title: {text}"
        );
        assert!(text.contains("preset_a"), "Should show preset name: {text}");
    }

    #[test]
    fn draw_preset_picker_empty() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::PresetPicker {
                    selected: 0,
                    entries: vec![],
                    filter: String::new(),
                },
                &theme,
            );
        });
        assert!(
            text.contains("no presets"),
            "Should show empty message: {text}"
        );
    }

    #[test]
    fn draw_preset_picker_with_filter() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![
            (
                "preset_a".to_string(),
                "Preview A".to_string(),
                String::new(),
            ),
            (
                "preset_b".to_string(),
                "Preview B".to_string(),
                String::new(),
            ),
        ];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::PresetPicker {
                    selected: 0,
                    entries,
                    filter: "aaa".to_string(),
                },
                &theme,
            );
        });
        assert!(text.contains("aaa"), "Should show filter text: {text}");
    }

    #[test]
    fn draw_preset_picker_filter_matches_nothing() {
        let app = make_app_with_prompt_dialog();
        let theme = Theme::classic();
        let entries = vec![(
            "preset_a".to_string(),
            "Preview A".to_string(),
            String::new(),
        )];
        let text = render_to_text(80, 24, |frame, _area| {
            draw_section_picker_modal(
                frame,
                &app,
                Color::Cyan,
                &SectionPickerMode::PresetPicker {
                    selected: 0,
                    entries,
                    filter: "zzz".to_string(),
                },
                &theme,
            );
        });
        assert!(
            text.contains("no presets match"),
            "Should show no match message: {text}"
        );
    }
}
