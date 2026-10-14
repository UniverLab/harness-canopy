//! CT26: the copy-reflex Ctrl+C guard.
//!
//! Two different accidents close an interactive session with a Ctrl+C the user
//! never meant to send:
//!
//! 1. The copy reflex itself. The user selects text with Shift+drag (the
//!    terminal's own selection, which canopy cannot observe) and presses
//!    Ctrl+C. Because opencode's default keybinding exits the app on a single
//!    Ctrl+C (claude needs two), that reflex is fatal there and merely noise
//!    everywhere else.
//! 2. A copy canopy *did* make: a plain drag inside a session pane already
//!    copies on mouse-up (`event::mouse::handle_selection_mouse`), and the
//!    reflex that follows it arrives a second or two later.
//!
//! Both are decided here, in one place, before the byte reaches the PTY:
//! `handle_agent_key` → `forward_key_to_focused_agent` → this module.

use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::agent_focus::FocusedAgent;
use crate::tui::app::types::App;

/// How long after a first swallowed Ctrl+C a second one is forwarded on a
/// `ctrl_c_exits` platform (FR1).
pub(crate) const CTRL_C_CONFIRM_WINDOW: Duration = Duration::from_secs(1);

/// How long after a TUI selection copy a Ctrl+C is treated as the copy reflex
/// rather than a real interrupt (FR2). Every platform, `ctrl_c_exits` or not.
pub(crate) const COPY_GUARD_WINDOW: Duration = Duration::from_secs(3);

/// How long the guard's one-line pane notice stays up. Long enough to cover
/// the longest window it explains (the 3 s copy window).
pub(crate) const NOTICE_TTL: Duration = Duration::from_secs(3);

/// FR2's notice, byte for byte.
pub(crate) const ALREADY_COPIED_NOTICE: &str = "already copied";

pub(super) fn is_ctrl_c(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL)
}

/// Decide a Ctrl+C headed for the focused session. `true` = the key was
/// consumed and nothing was written to the PTY.
///
/// Scope, deliberately narrow (FR4): only interactive sessions are guarded. A
/// shell's Ctrl+C is never fatal and never closes the session, so swallowing
/// it — even right after a copy — would only break the muscle memory of
/// clearing a shell's input line. Terminal panes keep today's path untouched.
///
/// Order, when more than one rule could apply: the copy guard wins. "already
/// copied" is the more useful thing to say when a copy just happened, and that
/// branch never forwards, so the accidental kill stays impossible. The confirm
/// window is left alone by it and expires on its own.
pub(super) fn handle_ctrl_c(
    app: &mut App,
    target: FocusedAgent,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    let FocusedAgent::Interactive(idx) = target else {
        return false;
    };
    if !is_ctrl_c(code, modifiers) {
        return false;
    }

    if app.copied_at.elapsed() < COPY_GUARD_WINDOW {
        app.ctrl_c.set_notice(ALREADY_COPIED_NOTICE);
        return true;
    }

    let Some(agent) = app.interactive_agents.get(idx) else {
        return false;
    };
    let platform = agent.cli.as_str();
    if !app.ctrl_c_exits_clis.contains(platform) {
        return false;
    }

    if app.ctrl_c.armed() {
        app.ctrl_c.clear_arm();
        return false;
    }

    app.ctrl_c.arm();
    app.ctrl_c
        .set_notice(&format!("Ctrl+C again within 1 s to send it to {platform}"));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::types::CtrlCState;

    #[test]
    fn is_ctrl_c_only_for_control_c() {
        assert!(is_ctrl_c(KeyCode::Char('c'), KeyModifiers::CONTROL));
        // A terminal that reports extra modifiers alongside CONTROL still
        // resolves, matching `is_reserved_focus_key`'s contains-not-equality rule.
        assert!(is_ctrl_c(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        ));
        assert!(!is_ctrl_c(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!is_ctrl_c(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(!is_ctrl_c(KeyCode::Esc, KeyModifiers::CONTROL));
    }

    #[test]
    fn windows_match_the_spec() {
        assert_eq!(CTRL_C_CONFIRM_WINDOW, Duration::from_secs(1));
        assert_eq!(COPY_GUARD_WINDOW, Duration::from_secs(3));
        assert!(
            NOTICE_TTL >= COPY_GUARD_WINDOW,
            "the notice must outlive the window it explains"
        );
        assert_eq!(ALREADY_COPIED_NOTICE, "already copied");
    }

    #[test]
    fn notice_is_some_only_while_its_ttl_holds() {
        let mut state = CtrlCState::initial();
        assert_eq!(state.notice(), None);

        state.set_notice(ALREADY_COPIED_NOTICE);
        assert_eq!(state.notice(), Some("already copied"));

        state.notice_at = std::time::Instant::now() - NOTICE_TTL - Duration::from_millis(1);
        assert_eq!(state.notice(), None, "an expired notice must not be drawn");
    }

    #[test]
    fn armed_is_true_only_inside_the_confirm_window() {
        let mut state = CtrlCState::initial();
        assert!(!state.armed());

        state.arm();
        assert!(state.armed());

        state.armed_at =
            Some(std::time::Instant::now() - CTRL_C_CONFIRM_WINDOW - Duration::from_millis(1));
        assert!(!state.armed(), "an expired window must not forward");

        state.arm();
        state.clear_arm();
        assert!(!state.armed());
    }
}
