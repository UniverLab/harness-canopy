//! CT28: live-tail pane renderer — renders into a sub-area of the graph view.
//!
//! Diagnostic viewer that follows the highlighted node. ANSI escape sequences
//! are stripped at render time; the stored bytes are unchanged.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::domain::graphs::GraphRunStatus;
use crate::tui::app::dialog::NodeTailDialog;
use crate::tui::ui::theme::Theme;

/// Strip CSI/OSC ANSI escape sequences from a line (render-time only).
/// Walks chars, not bytes, so multi-byte UTF-8 text survives untouched —
/// agent CLI output regularly carries box-drawing/emoji/accented characters.
pub(crate) fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: skip until a final byte in 0x40..=0x7e ('@'..='~').
            Some('[') => {
                for final_byte in chars.by_ref() {
                    if ('@'..='~').contains(&final_byte) {
                        break;
                    }
                }
            }
            // OSC: skip until BEL (0x07) or ST (ESC \).
            Some(']') => loop {
                match chars.next() {
                    Some('\u{7}') => break,
                    // ESC \ ends the sequence; a bare ESC mid-OSC is dropped
                    // with the char that follows it, same as the byte walk.
                    Some('\u{1b}') => {
                        if chars.next() == Some('\\') {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            },
            // ESC + single char (or a trailing ESC): drop both.
            Some(_) => {}
            None => {}
        }
    }
    out
}

/// Title line: `<node name> · <platform>/<model> · <status> · <elapsed>`,
/// with `· member <i>/<n>` inserted right after the node name when
/// `member_pos` is Some. Missing parts omitted.
fn pane_title(dialog: &NodeTailDialog, now: chrono::DateTime<chrono::Utc>) -> String {
    let mut parts: Vec<String> = vec![dialog.node_name.clone()];
    if let Some((i, n)) = dialog.member_pos {
        parts.push(format!("member {i}/{n}"));
    }
    match (&dialog.platform, &dialog.model) {
        (Some(p), Some(m)) => parts.push(format!("{p}/{m}")),
        (Some(p), None) => parts.push(p.clone()),
        (None, Some(m)) => parts.push(m.clone()),
        (None, None) => {}
    }
    if let Some(status) = dialog.status {
        parts.push(status.as_str().to_string());
    }
    if let Some(started_at) = dialog.started_at {
        let elapsed = now.signed_duration_since(started_at).num_seconds().max(0);
        parts.push(format!("{elapsed}s"));
    } else {
        parts.push("–".to_string());
    }
    parts.join(" · ")
}

/// Final banner for a finished run (appended to the body).
fn tail_final_banner(dialog: &NodeTailDialog) -> Option<&'static str> {
    let status = dialog.status?;
    match status {
        GraphRunStatus::Pass => Some("■ passed"),
        GraphRunStatus::Fail if dialog.timed_out => Some("■ timed out"),
        GraphRunStatus::Fail => Some("■ failed"),
        GraphRunStatus::Error => Some("■ error (no verdict)"),
        GraphRunStatus::Interrupted => Some("■ interrupted"),
        GraphRunStatus::Running => None,
    }
}

/// Body lines for the pane, capped at `max_body` content rows. Applies
/// `dialog.scroll` and always keeps the tail.
pub(crate) fn tail_visible_lines(dialog: &NodeTailDialog, max_body: usize) -> Vec<String> {
    if max_body == 0 {
        return Vec::new();
    }
    let mut all: Vec<String> = Vec::new();
    if !dialog.stdout_lines.is_empty() {
        all.extend(dialog.stdout_lines.iter().map(|l| strip_ansi(l)));
    }
    if !dialog.stderr_lines.is_empty() {
        if !all.is_empty() {
            all.push("── stderr ──".to_string());
        }
        all.extend(dialog.stderr_lines.iter().map(|l| strip_ansi(l)));
    }
    if let Some(banner) = tail_final_banner(dialog) {
        if !all.is_empty() {
            all.push(String::new());
        }
        all.push(banner.to_string());
    }
    if all.is_empty() {
        return Vec::new();
    }
    let end = all
        .len()
        .saturating_sub(dialog.scroll.min(all.len().saturating_sub(1)));
    let start = end.saturating_sub(max_body);
    all[start..end].to_vec()
}

/// Status line for the pane header.
fn pane_status_line(dialog: &NodeTailDialog) -> Option<String> {
    let status = dialog.status?;
    match status {
        GraphRunStatus::Running => {
            let total = dialog.stdout_lines.len() + dialog.stderr_lines.len();
            let platform = dialog.platform.as_deref().unwrap_or("the harness");
            if total == 0 {
                let when = dialog
                    .started_at
                    .map(|t| t.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "?".to_string());
                Some(format!(
                    "no output yet — {platform} has written nothing since {when} (some CLIs print only on exit)"
                ))
            } else {
                Some(format!("{total} lines (following live)"))
            }
        }
        GraphRunStatus::Pass => Some("passed".to_string()),
        GraphRunStatus::Fail if dialog.timed_out => Some("timed out".to_string()),
        GraphRunStatus::Fail => Some("failed".to_string()),
        GraphRunStatus::Error => Some("error (no verdict)".to_string()),
        GraphRunStatus::Interrupted => Some("interrupted by operator".to_string()),
    }
}

pub(crate) fn draw_node_tail_pane(
    frame: &mut Frame,
    area: Rect,
    dialog: &NodeTailDialog,
    theme: &Theme,
    now: chrono::DateTime<chrono::Utc>,
) {
    if area.width < 10 || area.height < 4 {
        return;
    }
    let title = pane_title(dialog, now);
    let block = Block::default()
        .title(title)
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Header (status) reserves 1 row; 1 more of headroom so the body never
    // paints over the block's bottom border while scrolled.
    let max_body = inner.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();

    if dialog.run_id.is_none() {
        // No run of this node in the current spec yet.
        let spec = dialog.no_run_spec_name.as_deref().unwrap_or("this spec");
        lines.push(Line::from(Span::styled(
            format!("no run of this node in {spec} yet"),
            Style::default().fg(theme.dim_text),
        )));
    } else if let Some(status_text) = pane_status_line(dialog) {
        let status_style = match dialog.status {
            Some(GraphRunStatus::Running) => Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
            Some(GraphRunStatus::Pass) => Style::default().fg(theme.header_color),
            Some(GraphRunStatus::Error) => Style::default()
                .fg(theme.status_interrupted)
                .add_modifier(Modifier::BOLD),
            Some(GraphRunStatus::Fail) | Some(GraphRunStatus::Interrupted) => Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
            None => Style::default().fg(theme.dim_text),
        };
        lines.push(Line::from(Span::styled(status_text, status_style)));
    }

    let body = tail_visible_lines(dialog, max_body);
    for line in body {
        lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(theme.text_primary),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn test_dialog(status: Option<GraphRunStatus>, stdout: usize, stderr: usize) -> NodeTailDialog {
        NodeTailDialog {
            run_id: Some("run1".to_string()),
            node_name: "Node node1".to_string(),
            stdout_lines: (0..stdout).map(|i| format!("out {i}")).collect(),
            stderr_lines: (0..stderr).map(|i| format!("err {i}")).collect(),
            status,
            timed_out: false,
            started_at: Some(Utc::now() - chrono::Duration::seconds(12)),
            scroll: 0,
            member_pos: None,
            platform: None,
            model: None,
            member_cursor: 0,
            member_count: 0,
            no_run_spec_name: None,
            last_resolved: None,
        }
    }

    #[test]
    fn strip_ansi_removes_csi_color() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strip_ansi_removes_osc() {
        assert_eq!(strip_ansi("before\x1b]0;title\x07after"), "beforeafter");
    }

    #[test]
    fn strip_ansi_plain_text_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn strip_ansi_preserves_multibyte_utf8() {
        // Regression: the stripper once walked bytes and pushed each one as a
        // char, mojibake-ing every multi-byte character. Real harness output
        // carries box-drawing, emoji and accented text.
        assert_eq!(strip_ansi("héllo — wörld ✓"), "héllo — wörld ✓");
        assert_eq!(strip_ansi("\x1b[32m⠋ working…\x1b[0m"), "⠋ working…");
    }

    #[test]
    fn strip_ansi_osc_ends_with_string_terminator() {
        assert_eq!(strip_ansi("a\x1b]0;title\x1b\\b"), "ab");
    }

    #[test]
    fn running_with_no_output_looks_hung_not_blank() {
        let dialog = test_dialog(Some(GraphRunStatus::Running), 0, 0);
        let body = tail_visible_lines(&dialog, 20);
        assert!(body.is_empty());
    }

    #[test]
    fn running_with_output_shows_live_following() {
        let dialog = test_dialog(Some(GraphRunStatus::Running), 3, 0);
        let status = pane_status_line(&dialog).unwrap();
        assert!(status.contains("following"), "status: {status}");
    }

    #[test]
    fn running_no_output_status_names_platform() {
        let mut dialog = test_dialog(Some(GraphRunStatus::Running), 0, 0);
        dialog.platform = Some("claude".to_string());
        let status = pane_status_line(&dialog).unwrap();
        assert!(status.contains("no output yet"), "status: {status}");
        assert!(status.contains("claude"), "status: {status}");
    }

    #[test]
    fn timed_out_banner_names_kill_point() {
        let mut dialog = test_dialog(Some(GraphRunStatus::Fail), 2, 0);
        dialog.timed_out = true;
        assert_eq!(tail_final_banner(&dialog), Some("■ timed out"));
    }

    #[test]
    fn failed_without_timeout_says_failed() {
        let dialog = test_dialog(Some(GraphRunStatus::Fail), 2, 0);
        assert_eq!(tail_final_banner(&dialog), Some("■ failed"));
    }

    #[test]
    fn passed_banner_says_passed() {
        let dialog = test_dialog(Some(GraphRunStatus::Pass), 2, 0);
        assert_eq!(tail_final_banner(&dialog), Some("■ passed"));
    }

    #[test]
    fn tail_renders_lines_capped() {
        let dialog = test_dialog(Some(GraphRunStatus::Running), 4000, 1000);
        let body = tail_visible_lines(&dialog, 20);
        assert_eq!(body.len(), 20);
    }

    #[test]
    fn tail_without_cap_returns_everything_in_order() {
        let dialog = test_dialog(Some(GraphRunStatus::Running), 2, 1);
        let body = tail_visible_lines(&dialog, 100);
        assert_eq!(body, vec!["out 0", "out 1", "── stderr ──", "err 0"]);
    }

    #[test]
    fn scroll_holds_back_from_live_edge() {
        let mut dialog = test_dialog(Some(GraphRunStatus::Running), 10, 0);
        dialog.scroll = 4;
        let body = tail_visible_lines(&dialog, 100);
        assert_eq!(body.len(), 6);
        assert_eq!(body[5], "out 5");
    }

    #[test]
    fn pane_title_includes_platform_model_status_elapsed() {
        let mut dialog = test_dialog(Some(GraphRunStatus::Running), 1, 0);
        dialog.platform = Some("claude".to_string());
        dialog.model = Some("opus".to_string());
        let title = pane_title(&dialog, Utc::now());
        assert!(title.contains("claude/opus"), "title: {title}");
        assert!(title.contains("running"), "title: {title}");
    }

    #[test]
    fn pane_title_with_member_pos() {
        let mut dialog = test_dialog(Some(GraphRunStatus::Running), 1, 0);
        dialog.member_pos = Some((2, 3));
        dialog.platform = Some("claude".to_string());
        dialog.model = Some("opus".to_string());
        let title = pane_title(&dialog, Utc::now());
        assert!(
            title.starts_with("Node node1 · member 2/3 · claude/opus · running"),
            "the member segment goes right after the node name: {title}"
        );
    }

    #[test]
    fn no_run_shows_waiting_line() {
        let mut dialog = NodeTailDialog::new();
        dialog.node_name = "Tail".to_string();
        dialog.no_run_spec_name = Some("My Spec".to_string());
        let area = Rect::new(0, 0, 80, 20);
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_node_tail_pane(frame, area, &dialog, &Theme::classic(), Utc::now());
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains("no run of this node in My Spec yet"),
            "got: {text}"
        );
    }
}
