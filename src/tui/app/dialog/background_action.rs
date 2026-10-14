//! Delivery action for a background agent created from the TUI dialog (CM53).
//!
//! The dialog's background form can either run a CLI headlessly (the default)
//! or deliver its prompt into a live interactive session. The target list is
//! the same live-session list the scheduled-send prompt uses.

use super::new_agent::NewAgentDialog;

/// How a background agent fires. Mirrors `crate::domain::models::AgentAction`
/// for the dialog's purposes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackgroundAction {
    Headless,
    SendToSession,
}

impl BackgroundAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Headless => "headless",
            Self::SendToSession => "send to session",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Headless => Self::SendToSession,
            Self::SendToSession => Self::Headless,
        }
    }
}

impl NewAgentDialog {
    /// Whether this background agent delivers into a session.
    pub fn is_send_to_session(&self) -> bool {
        self.background_action == BackgroundAction::SendToSession
    }

    /// The non-empty session names to offer as targets, in order. Duplicates
    /// are kept: name resolution refuses an ambiguous target at fire time.
    pub fn send_target_names(sessions: &[String]) -> Vec<String> {
        sessions
            .iter()
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .collect()
    }

    /// The currently selected target session name, if any.
    pub fn selected_send_target(&self) -> Option<String> {
        self.send_target_names
            .get(self.send_target_idx)
            .filter(|name| !name.trim().is_empty())
            .cloned()
    }

    /// Move the target selection, wrapping.
    pub fn move_send_target(&mut self, forward: bool) {
        let count = self.send_target_names.len();
        if count == 0 {
            return;
        }
        self.send_target_idx = if forward {
            (self.send_target_idx + 1) % count
        } else {
            self.send_target_idx.checked_sub(1).unwrap_or(count - 1)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_mode_lists_names_and_skips_blanks() {
        let names = NewAgentDialog::send_target_names(&[
            "orch".to_string(),
            "  ".to_string(),
            "builder".to_string(),
        ]);
        assert_eq!(names, vec!["orch".to_string(), "builder".to_string()]);
    }

    #[test]
    fn action_toggles_labels() {
        assert_eq!(
            BackgroundAction::Headless.next(),
            BackgroundAction::SendToSession
        );
        assert_eq!(BackgroundAction::SendToSession.label(), "send to session");
    }
}
