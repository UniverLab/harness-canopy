//! The trust form: add a trusted GitHub profile or edit one's grant.
//!
//! `t` on the Remote tab opens a blank form; `Enter` on a trusted row opens
//! it prefilled for that login. The form edits a username plus a
//! comma-separated session list, with an all-sessions toggle: `Tab` (or
//! `↑`/`↓`) moves between the three stops, typing edits the text stops,
//! `a` or `Space` flips the toggle, `Enter` submits through the daemon,
//! `Esc` closes without saving.

use crate::remote::panel::TrustedView;

/// Which stop of the trust form has the cursor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TrustField {
    /// The GitHub username.
    #[default]
    Login,
    /// The comma-separated session names.
    Sessions,
    /// The all-sessions toggle.
    All,
}

/// The add/edit form state, closed until opened.
#[derive(Debug, Default)]
pub struct TrustForm {
    /// Whether the overlay is up.
    pub open: bool,
    /// The username being trusted.
    pub login: String,
    /// The granted names as typed, comma-separated.
    pub sessions: String,
    /// Whether the grant covers every session.
    pub all: bool,
    /// The stop under the cursor.
    pub focus: TrustField,
}

impl TrustForm {
    /// Open a blank form for a new profile.
    pub fn open_new(&mut self) {
        *self = Self {
            open: true,
            ..Self::default()
        };
    }

    /// Open the form prefilled with one profile's grant.
    pub fn open_edit(&mut self, profile: &TrustedView) {
        *self = Self {
            open: true,
            login: profile.login.clone(),
            sessions: profile.sessions.join(", "),
            all: profile.all_sessions,
            focus: TrustField::default(),
        };
    }

    /// Hide the overlay; the draft is dropped.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Flip the all-sessions toggle.
    pub fn toggle_all(&mut self) {
        self.all = !self.all;
    }

    /// Move the cursor between the three stops.
    pub fn cycle_focus(&mut self, forward: bool) {
        let order = [TrustField::Login, TrustField::Sessions, TrustField::All];
        let at = order
            .iter()
            .position(|field| *field == self.focus)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % order.len()
        } else {
            (at + order.len() - 1) % order.len()
        };
        self.focus = order[next];
    }

    /// The granted names: trimmed, empties dropped, order kept.
    pub fn sessions_list(&self) -> Vec<String> {
        self.sessions
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect()
    }

    /// Type one character into the focused text stop (a no-op on the toggle).
    pub fn push_char(&mut self, c: char) {
        match self.focus {
            TrustField::Login => self.login.push(c),
            TrustField::Sessions => self.sessions.push(c),
            TrustField::All => {}
        }
    }

    /// Delete one character from the focused text stop.
    pub fn pop_char(&mut self) {
        match self.focus {
            TrustField::Login => {
                self.login.pop();
            }
            TrustField::Sessions => {
                self.sessions.pop();
            }
            TrustField::All => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_new_resets_and_open_edit_prefills() {
        let mut form = TrustForm::default();
        form.open_new();
        assert!(form.open);
        assert!(form.login.is_empty());
        assert!(!form.all);

        form.login = "stale".to_string();
        form.open_edit(&TrustedView {
            login: "guest".to_string(),
            all_sessions: false,
            sessions: vec!["alpha".to_string(), "beta".to_string()],
        });
        assert!(form.open);
        assert_eq!(form.login, "guest");
        assert_eq!(form.sessions, "alpha, beta");
        assert!(!form.all);

        form.close();
        assert!(!form.open);
    }

    #[test]
    fn sessions_list_trims_and_drops_empties() {
        let form = TrustForm {
            sessions: " alpha,, beta , ".to_string(),
            ..TrustForm::default()
        };
        assert_eq!(form.sessions_list(), vec!["alpha", "beta"]);
    }

    #[test]
    fn focus_cycles_through_all_three_stops() {
        let mut form = TrustForm::default();
        assert_eq!(form.focus, TrustField::Login);
        form.cycle_focus(true);
        assert_eq!(form.focus, TrustField::Sessions);
        form.cycle_focus(true);
        assert_eq!(form.focus, TrustField::All);
        form.cycle_focus(true);
        assert_eq!(form.focus, TrustField::Login);
        form.cycle_focus(false);
        assert_eq!(form.focus, TrustField::All);
    }

    #[test]
    fn typing_edits_text_stops_and_never_the_toggle() {
        let mut form = TrustForm::default();
        form.push_char('g');
        assert_eq!(form.login, "g");
        form.cycle_focus(true);
        form.push_char('a');
        assert_eq!(form.sessions, "a");
        form.pop_char();
        assert!(form.sessions.is_empty());
        form.cycle_focus(true);
        assert_eq!(form.focus, TrustField::All);
        form.push_char('a');
        form.pop_char();
        assert!(form.login == "g" && form.sessions.is_empty());
        form.toggle_all();
        assert!(form.all);
    }
}
