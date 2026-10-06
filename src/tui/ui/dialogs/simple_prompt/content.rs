use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Apply styling to collapsed paste blocks, making them stand out with accent color.
pub(super) fn style_collapsed_paste_blocks(
    render_text: &str,
    accent: Color,
    _section_bg: Color,
) -> Vec<(String, Option<Color>)> {
    let mut result = Vec::new();
    let mut current_pos = 0;

    // Find all collapsed paste blocks: [Pasted ~N lines]
    while let Some(start) = render_text[current_pos..].find("[Pasted ~") {
        let start_abs = current_pos + start;

        // Add text before the block (uncolored)
        if start > 0 {
            result.push((render_text[current_pos..start_abs].to_string(), None));
        }

        // Find the end of the block
        if let Some(end_rel) = render_text[start_abs..].find(']') {
            let end_abs = start_abs + end_rel + 1;
            let block_text = &render_text[start_abs..end_abs];

            // Add the collapsed block with accent color
            result.push((block_text.to_string(), Some(accent)));
            current_pos = end_abs;
        } else {
            // No closing bracket, treat rest as normal
            result.push((render_text[start_abs..].to_string(), None));
            break;
        }
    }

    // Add remaining text
    if current_pos < render_text.len() {
        result.push((render_text[current_pos..].to_string(), None));
    }

    result
}

/// Wrap styled content into visual lines with the exact char-based math of
/// `SimplePromptDialog::visual_line_count`, so rendered text, box height, and
/// cursor/scroll positions always agree. Hard newlines break lines (ratatui
/// drops `\n` inside a `Line` as a control char, which visually glued words
/// together), overflow wraps at `field_width`, and tabs expand to 4-col stops.
/// The char at `cursor_idx` is drawn as a block cursor.
pub(super) fn wrap_styled_content(
    styled_content: Vec<(String, Option<Color>)>,
    cursor_idx: Option<usize>,
    field_width: usize,
    section_bg: Color,
    text_color: Color,
) -> Vec<Line<'static>> {
    fn flush_run(spans: &mut Vec<Span<'static>>, run: &mut String, style: Style) {
        if !run.is_empty() {
            spans.push(Span::styled(std::mem::take(run), style));
        }
    }

    let field_width = field_width.max(1);
    let cursor_style = Style::default().fg(section_bg).bg(text_color);

    let mut lines: Vec<Line> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_style = Style::default().fg(text_color).bg(section_bg);
    let mut col = 0usize;
    let mut char_pos = 0usize;

    for (text, color) in styled_content {
        let base_style = Style::default()
            .fg(color.unwrap_or(text_color))
            .bg(section_bg);
        for ch in text.chars() {
            let style = if cursor_idx == Some(char_pos) {
                cursor_style
            } else {
                base_style
            };
            match ch {
                '\n' => {
                    flush_run(&mut spans, &mut run, run_style);
                    if style == cursor_style {
                        spans.push(Span::styled(" ", cursor_style));
                    }
                    lines.push(Line::from(std::mem::take(&mut spans)));
                    col = 0;
                }
                '\t' => {
                    let tab = 4 - (col % 4);
                    if col + tab > field_width {
                        flush_run(&mut spans, &mut run, run_style);
                        lines.push(Line::from(std::mem::take(&mut spans)));
                        col = tab;
                    } else {
                        col += tab;
                    }
                    if style != run_style {
                        flush_run(&mut spans, &mut run, run_style);
                        run_style = style;
                    }
                    run.push_str(&" ".repeat(tab));
                }
                _ => {
                    if col + 1 > field_width {
                        flush_run(&mut spans, &mut run, run_style);
                        lines.push(Line::from(std::mem::take(&mut spans)));
                        col = 1;
                    } else {
                        col += 1;
                    }
                    if style != run_style {
                        flush_run(&mut spans, &mut run, run_style);
                        run_style = style;
                    }
                    run.push(ch);
                }
            }
            char_pos += 1;
        }
    }

    flush_run(&mut spans, &mut run, run_style);
    // Cursor past the end of content: draw it as a highlighted blank cell.
    if cursor_idx == Some(char_pos) {
        spans.push(Span::styled(" ", cursor_style));
    }
    lines.push(Line::from(spans));
    lines
}
