use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{truncate_str, PromptGeometry};
use crate::tui::app::dialog::{PromptTab, SimplePromptDialog};

// Old function removed - using simple prompt dialog instead
pub(super) fn generate_top_border(title: &str, width: u16, style: Style) -> Line<'static> {
    if width < 2 {
        return Line::from(vec![Span::styled(String::new(), style)]);
    }

    let max_title_chars = width.saturating_sub(4) as usize;
    let title_with_spaces = if max_title_chars == 0 {
        String::new()
    } else {
        format!(" {} ", truncate_str(title, max_title_chars))
    };
    let title_width = title_with_spaces.chars().count() as u16;
    let available_width = width.saturating_sub(title_width + 2);
    let left_dashes = available_width / 2;
    let right_dashes = available_width - left_dashes;

    let border = format!(
        "┌{}{}{}┐",
        "─".repeat(left_dashes as usize),
        title_with_spaces,
        "─".repeat(right_dashes as usize)
    );
    Line::from(vec![Span::styled(border, style)])
}

/// Generate a bottom border line dynamically based on width
pub(super) fn generate_bottom_border(width: u16, style: Style) -> Line<'static> {
    if width < 2 {
        return Line::from(vec![Span::styled(String::new(), style)]);
    }
    let border = format!("└{}┘", "─".repeat((width - 2) as usize));
    Line::from(vec![Span::styled(border, style)])
}

pub(super) fn centered_rect_fixed(
    width: u16,
    height: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let clamped_w = width.clamp(1, area.width.max(1));
    let clamped_h = height.clamp(1, area.height.max(1));
    let x = area.x + area.width.saturating_sub(clamped_w) / 2;
    let y = area.y + area.height.saturating_sub(clamped_h) / 2;
    ratatui::layout::Rect::new(x, y, clamped_w, clamped_h)
}

/// Compute the dialog width, per-section heights and the scheduled-list view.
///
/// `pending_len` drives the list panel height: a label/top-border row plus one
/// row per visible entry, present only when the selected session actually has
/// pending sends. A selection is only honored (highlighted, scrolled into view)
/// while there is a list to browse.
pub(super) fn compute_prompt_geometry(
    frame_area: Rect,
    dialog: &SimplePromptDialog,
    list_panel_height: u16,
) -> PromptGeometry {
    // Use 65% of terminal width (responsive, not edge-to-edge)
    let percent_x = 65u16;
    let max_dialog_w = frame_area.width.saturating_sub(2).max(1);
    let preferred_dialog_w = frame_area.width.saturating_mul(percent_x) / 100;
    let min_dialog_w = 40u16.min(max_dialog_w);
    let dialog_width = preferred_dialog_w.clamp(min_dialog_w, max_dialog_w);
    let inner_width = dialog_width.saturating_sub(2);
    let field_width = inner_width.saturating_sub(2).max(10) as usize;

    // Pre-compute render height for each section (label + content + border + gap = content_h + 3).
    // focused_section 0 = send_at, sections start at index 1.
    let section_focus_offset = 1;
    let section_heights: Vec<u16> = dialog
        .enabled_sections
        .iter()
        .enumerate()
        .map(|(i, section_name)| {
            let is_focused = dialog.focused_section == i + section_focus_offset;
            let content_h = if is_focused {
                let content = dialog.section_content_for_build(section_name).unwrap_or("");
                let vis = SimplePromptDialog::visual_line_count(content, field_width);
                let max_h = SimplePromptDialog::max_visible_lines(section_name);
                (vis as u16).clamp(1, max_h as u16)
            } else {
                1u16
            };
            content_h + 3 // label(1) + content + bottom_border(1) + gap(1)
        })
        .collect();

    let total_sections_height: u16 = section_heights.iter().sum();

    // Raw tab: a single content block — the raw field, or the composed-prompt
    // preview when the buffer is empty — instead of the section stack.
    let raw_content_height: u16 = if dialog.active_tab == PromptTab::Raw {
        let text = if dialog.raw_is_empty() {
            dialog.raw_preview.as_deref().unwrap_or("")
        } else {
            dialog.raw_text()
        };
        let vis = SimplePromptDialog::visual_line_count(text, field_width);
        (vis as u16).clamp(3, 18)
    } else {
        0
    };
    let content_height = match dialog.active_tab {
        PromptTab::Normal => total_sections_height,
        PromptTab::Raw => raw_content_height,
    };
    // borders(2) + tab bar(1) + hint(1) + gap(1) + content + gap(1) +
    // scheduled-list panel + send(1). The panel is 0-height when there are no
    // pending sends, so the layout is byte-for-byte unchanged in that case.
    let total_height = 2 + 1 + 1 + 1 + content_height + 1 + list_panel_height + 1;

    // Cap dialog height — leave at least 4 rows margin, minimum 10 rows.
    let max_dialog_h = frame_area.height.saturating_sub(2).max(1);
    let min_dialog_h = 10u16.min(max_dialog_h);
    let height = total_height.min(max_dialog_h).max(min_dialog_h);

    PromptGeometry {
        dialog_width,
        height,
        field_width,
        section_heights,
    }
}
