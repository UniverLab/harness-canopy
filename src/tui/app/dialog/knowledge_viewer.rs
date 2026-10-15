//! `KnowledgeViewerDialog` — read-only viewer state for a knowledge node.

use crate::tui::app::types::Focus;

/// Read-only scrollable viewer for a project knowledge entry (fact or
/// pattern). `e` inside the viewer hands off to `KnowledgeDialog` in edit mode.
pub struct KnowledgeViewerDialog {
    pub title: String,
    pub kind: String,
    pub body: String,
    /// First wrapped line shown at the top of the body area.
    pub scroll: u16,
    /// Highest useful `scroll`, refreshed on every draw from the body's
    /// *wrapped* line count against the drawn height (see
    /// `ui::dialogs::knowledge_viewer::draw_knowledge_viewer`) — the body is a
    /// free-text blob agents write as one paragraph, so the number of `\n`
    /// lines says nothing about how many rows it renders to.
    pub max_scroll: u16,
    pub prev_focus: Option<Focus>,
}

impl KnowledgeViewerDialog {
    pub fn new(title: String, kind: String, body: String) -> Self {
        Self {
            title,
            kind,
            body,
            scroll: 0,
            max_scroll: 0,
            prev_focus: None,
        }
    }

    /// Move the body window by `delta` wrapped lines, saturating at both ends:
    /// scrolling must reach the last line of a long entry and never scroll a
    /// short one past its end.
    pub fn scroll_by(&mut self, delta: i32) {
        let step = delta.unsigned_abs() as u16;
        self.scroll = if delta < 0 {
            self.scroll.saturating_sub(step)
        } else {
            self.scroll.saturating_add(step).min(self.max_scroll)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> KnowledgeViewerDialog {
        KnowledgeViewerDialog::new("t".to_string(), "fact".to_string(), "body".to_string())
    }

    #[test]
    fn scroll_by_reaches_the_end_and_stops_there() {
        let mut dialog = dialog();
        dialog.max_scroll = 12;

        dialog.scroll_by(10);
        assert_eq!(dialog.scroll, 10);
        dialog.scroll_by(10);
        assert_eq!(dialog.scroll, 12, "PgDn must stop on the last wrapped line");
        dialog.scroll_by(1);
        assert_eq!(dialog.scroll, 12, "Down must not scroll past the end");
    }

    #[test]
    fn scroll_by_never_moves_a_body_that_fits() {
        let mut dialog = dialog();
        dialog.max_scroll = 0;

        dialog.scroll_by(10);
        dialog.scroll_by(1);
        assert_eq!(dialog.scroll, 0);
    }

    #[test]
    fn scroll_by_up_saturates_at_the_first_line() {
        let mut dialog = dialog();
        dialog.max_scroll = 4;
        dialog.scroll = 1;

        dialog.scroll_by(-10);
        assert_eq!(dialog.scroll, 0);
        dialog.scroll_by(-1);
        assert_eq!(dialog.scroll, 0);
    }
}
