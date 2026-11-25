use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::terminal_history::SuggestionPicker;
use crate::tui::ui::theme::Theme;

pub fn draw_suggestion_picker(
    frame: &mut Frame,
    app: &App,
    panel_area: ratatui::layout::Rect,
    theme: &Theme,
) {
    let Some(picker) = &app.suggestion_picker else {
        return;
    };

    let picker_area = panel_area;

    if picker.items.is_empty() {
        // Show "no matches" indicator
        draw_no_matches_picker(frame, picker_area, theme);
        return;
    }

    let visible = picker.visible_count().min(10) as u16;
    let w = 60u16.min(picker_area.width.saturating_sub(2));
    let h = visible + 2; // items + border

    let title = suggestion_picker_title(picker);

    // Anchor above the warp input box (3 rows from bottom)
    let area = ratatui::layout::Rect::new(
        picker_area.x + 1,
        picker_area.y + picker_area.height.saturating_sub(h + 4),
        w,
        h,
    );
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(title)
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let scroll_offset = picker.scroll_offset;
    let items: Vec<ListItem> = picker
        .visible_items()
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let abs_index = scroll_offset + i;
            let selected = abs_index == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(theme.header_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let prefix = if selected { "> " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&item.label, style),
            ]))
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner);
}

/// Draw the "No matches" indicator in place of the picker list.
fn draw_no_matches_picker(frame: &mut Frame, picker_area: Rect, theme: &Theme) {
    let w = 30u16.min(picker_area.width.saturating_sub(2));
    let area = Rect::new(
        picker_area.x + 1,
        picker_area.y + picker_area.height.saturating_sub(3),
        w,
        2,
    );
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.border_color))
        .style(Style::default().bg(Color::Rgb(15, 15, 25)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let msg = Paragraph::new("  No matches").style(Style::default().fg(theme.dim_text));
    frame.render_widget(msg, inner);
}

/// Mode-dependent title, with the position indicator when the list overflows.
fn suggestion_picker_title(picker: &SuggestionPicker) -> String {
    let total = picker.items.len();
    match picker.mode {
        crate::tui::terminal_history::PickerMode::CommandHistory => {
            let filter_hint = if picker.input.is_empty() {
                String::new()
            } else {
                format!(" | {}", picker.input)
            };
            if total > picker.visible_count() {
                format!(
                    " History [{}/{}]{} ",
                    picker.selected + 1,
                    total,
                    filter_hint
                )
            } else {
                format!(" History{} ", filter_hint)
            }
        }
        crate::tui::terminal_history::PickerMode::CdDirectory => {
            format!(" {} ←→ ", picker.input)
        }
    }
}
