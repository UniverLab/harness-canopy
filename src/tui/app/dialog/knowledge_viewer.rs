//! `KnowledgeViewerDialog` — read-only viewer state for a knowledge node.

use crate::tui::app::types::Focus;

/// Read-only scrollable viewer for a project knowledge entry (fact or
/// pattern). `e` inside the viewer hands off to `KnowledgeDialog` in edit mode.
pub struct KnowledgeViewerDialog {
    pub title: String,
    pub kind: String,
    pub body: String,
    pub scroll: u16,
    pub prev_focus: Option<Focus>,
}

impl KnowledgeViewerDialog {
    pub fn new(title: String, kind: String, body: String) -> Self {
        Self {
            title,
            kind,
            body,
            scroll: 0,
            prev_focus: None,
        }
    }
}
