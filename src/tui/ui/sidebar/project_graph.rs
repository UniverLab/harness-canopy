use super::*;

// ── Project Graph ────────────────────────────────────────────────

/// How many edge lines fit in the panel's inner area. `inner_height` is
/// already the post-border rect `render_titled_panel` hands to its
/// callback — budget it directly rather than subtracting border rows a
/// second time, which used to compute 0 at the panel's minimum height.
pub(super) fn graph_edge_row_budget(inner_height: u16, edge_count: usize) -> usize {
    edge_count.min(inner_height as usize)
}

pub(crate) fn draw_project_graph(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    offset: u16,
) {
    if app.project_graph_trees.is_empty() || app.project_graph_edges.is_empty() {
        let msg = if app.projects.len() <= 1 {
            "No relationships yet. Press Enter on a project to link."
        } else {
            "No relationships yet. Press Enter to link projects."
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(theme.muted_text),
            ))),
            area,
        );
        return;
    }

    let total = app.project_graph_edges.len();
    let visible = area.height as usize;
    let max_offset = total.saturating_sub(visible.max(1));
    let clamped_offset = (offset as usize).min(max_offset);
    let edge_count = graph_edge_row_budget(area.height, total.saturating_sub(clamped_offset));

    for (i, edge) in app
        .project_graph_edges
        .iter()
        .skip(clamped_offset)
        .take(edge_count)
        .enumerate()
    {
        let y = area.y + i as u16;
        if y + 1 > area.y + area.height {
            break;
        }
        let relation = match edge.relation.as_str() {
            "depends_on" => "(depends)",
            "complements" => "(complements)",
            "extends" => "(extends)",
            "publishes" => "(publishes)",
            "contains" => "(contains)",
            "relates_to" => "(related)",
            _ => "",
        };
        let label = format!(
            "{} → {} {}",
            truncate_str(&edge.from_name, 14),
            truncate_str(&edge.to_name, 14),
            relation,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_str(&label, area.width.saturating_sub(2) as usize),
                // THEME-EXEMPT: single-use graph-edge cyan — not a recurring role.
                Style::default().fg(Color::Cyan),
            ))),
            Rect::new(area.x, y, area.width, 1),
        );
    }
}

// ── Project Relation Dialog ──────────────────────────────────────

pub(super) fn draw_project_relation_dialog(
    frame: &mut Frame,
    _area: Rect,
    _app: &App,
    dialog: &crate::tui::app::types::ProjectRelationDialog,
    theme: &Theme,
) {
    // Center the dialog in the screen area
    let dialog_w = 50u16.min(frame.area().width.saturating_sub(4));
    let dialog_h = 14u16.min(frame.area().height.saturating_sub(2));
    let x = frame.area().x + (frame.area().width.saturating_sub(dialog_w)) / 2;
    let y = frame.area().y + (frame.area().height.saturating_sub(dialog_h)) / 2;
    let area = Rect::new(x, y, dialog_w, dialog_h);

    let block = Block::default()
        .title(format!(" Link: {} ", dialog.from_name))
        .borders(dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color));
    frame.render_widget(block, area);

    let inner = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );

    // Relation type selector
    let rel_line = format!(
        "Relation: ◀ {} ▶",
        dialog.relation_types[dialog.relation_idx]
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            rel_line,
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    if let Some(ref error) = dialog.error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_str(error, inner.width as usize),
                Style::default().fg(theme.error),
            ))),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }

    // Search/filter field
    let filter_label = if dialog.filter_buffer.is_empty() {
        "filter: _"
    } else {
        &dialog.filter_buffer
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}|", filter_label),
            Style::default().fg(theme.warning),
        ))),
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
    );

    // Project list
    let list_start_y = inner.y + 3;
    let max_items = inner
        .height
        .saturating_sub(4)
        .min(dialog.filtered.len() as u16);

    for i in 0..max_items {
        let idx = dialog.filtered[i as usize];
        let project = &dialog.available[idx];
        let name = format!(
            "{}  {}",
            if i as usize == dialog.selected_idx {
                "▶"
            } else {
                " "
            },
            truncate_str(&project.title, inner.width.saturating_sub(4) as usize),
        );
        let style = if i as usize == dialog.selected_idx {
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted_text)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(name, style))),
            Rect::new(inner.x, list_start_y + i, inner.width, 1),
        );
    }

    if dialog.filtered.is_empty() && dialog.filter_buffer.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No other projects indexed.",
                Style::default().fg(theme.muted_text),
            ))),
            Rect::new(inner.x, list_start_y, inner.width, 1),
        );
    }

    // Footer
    let footer_y = inner.y + inner.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Enter confirm  ·  ←→ relation  ·  Esc cancel  ·  type filter ",
            Style::default().fg(theme.dim_text),
        ))),
        Rect::new(inner.x, footer_y, inner.width, 1),
    );
}
