//! First-run hint markers: a blinking `[?]` that opens a tip popover.
//!
//! Hints are data — adding one is a row in [`HINTS`], no new UI code.
//! At most one hint is active at a time, only when its anchor is on
//! screen, and every tip offers the same two persisted actions.
//! Rendering lives in `tui::ui::hint_overlay`.

use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::tui::app::hub::HubTab;
use crate::tui::app::types::{AgentEntry, App, Focus, SidebarLayer};
use crate::tui::ui::theme::Theme;

/// Where a hint's `[?]` marker draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintAnchor {
    HomeBanner,
    AgentPromptBar,
    HubHeader,
}

/// One first-run hint: static data, so new hints need no new UI code.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HintDef {
    pub id: &'static str,
    pub anchor: HintAnchor,
    pub text: &'static str,
    pub key: &'static str,
}

/// The hints in order of appearance: at most one is active at a time.
pub(crate) const HINTS: &[HintDef] = &[
    HintDef {
        id: "start_session",
        anchor: HintAnchor::HomeBanner,
        text: "Press Ctrl+N to start a new agent or terminal session.",
        key: "Ctrl+N",
    },
    HintDef {
        id: "prompt_menu",
        anchor: HintAnchor::AgentPromptBar,
        text: "Press F5 for ready prompts —\ntry onboarding as your first message.",
        key: "F5",
    },
    HintDef {
        id: "hub_help",
        anchor: HintAnchor::HubHeader,
        text: "Open the Help tab for shortcuts,\ngraphs, and resetting hints.",
        key: "?",
    },
];

/// The on-screen hint, if any: hidden/dismissed/narrow yield none, then
/// the first hint (in [`HINTS`] order) whose anchor is on screen wins.
pub(crate) fn active_hint(app: &App) -> Option<&'static HintDef> {
    if app.cli_usage.hints_hidden {
        return None;
    }
    if app.term_width < 80 || app.term_height < 20 {
        return None;
    }
    HINTS
        .iter()
        .find(|hint| !is_dismissed(app, hint.id) && anchor_visible(app, hint.anchor))
}

/// Whether the anchor for `anchor` is currently on screen. Home and
/// agent anchors hide under the hub overlay; the hub anchor needs it.
fn anchor_visible(app: &App, anchor: HintAnchor) -> bool {
    match anchor {
        HintAnchor::HomeBanner => {
            !app.hub_open
                && app.agents.is_empty()
                && app.projects.is_empty()
                && app.focus == Focus::Home
        }
        HintAnchor::AgentPromptBar => {
            // The prompt strip only draws behind the focused agent panel:
            // on Home/Preview (or the Knowledge layer) the anchor is off
            // screen, so the hint stays inactive there too.
            !app.hub_open
                && app.focus == Focus::Agent
                && app.sidebar_layer != SidebarLayer::Knowledge
                && matches!(
                    app.selected_agent(),
                    Some(AgentEntry::Interactive(_) | AgentEntry::Terminal(_))
                )
        }
        HintAnchor::HubHeader => app.hub_open,
    }
}

fn is_dismissed(app: &App, id: &str) -> bool {
    app.cli_usage.hints_dismissed.iter().any(|done| done == id)
}

/// `F1` context rule: with the hub open it closes the hub (and any tip,
/// whose anchor leaves the screen with it); with a tip open it closes
/// the tip; with a hint on screen it opens that hint's tip; otherwise
/// the hub opens as before. Never writes to the agent PTY.
pub(crate) fn handle_f1(app: &mut App) -> bool {
    if app.hub_open {
        app.close_hub();
        app.hint_tip_open = None;
        return true;
    }
    if app.hint_tip_open.is_some() {
        app.hint_tip_open = None;
        return true;
    }
    if let Some(hint) = active_hint(app) {
        app.hint_tip_open = Some(hint.id);
    } else {
        app.toggle_hub();
    }
    true
}

/// While a tip is open it owns `y`/Enter ("Got it"), `n` ("Hide all
/// hints"), Esc and F1 (close) — the `simple_modals.rs` y/Enter-n/Esc
/// convention. Every action persists via [`CliUsage`]; nothing here
/// touches the agent PTY. True when the key was consumed.
pub(crate) fn handle_tip_key(app: &mut App, code: KeyCode, _modifiers: KeyModifiers) -> bool {
    let Some(id) = app.hint_tip_open else {
        return false;
    };
    match code {
        KeyCode::Char('y') | KeyCode::Enter => {
            let dir = app.data_dir.clone();
            app.cli_usage.dismiss_hint(&dir, id);
            app.hint_tip_open = None;
            true
        }
        KeyCode::Char('n') => {
            let dir = app.data_dir.clone();
            app.cli_usage.hide_all_hints(&dir);
            app.hint_tip_open = None;
            true
        }
        KeyCode::Esc | KeyCode::F(1) => {
            app.hint_tip_open = None;
            true
        }
        _ => false,
    }
}

/// The Help tab's keys: `r` brings dismissed/hidden hints back.
pub(crate) fn handle_help_key(app: &mut App, code: KeyCode) {
    if code == KeyCode::Char('r') {
        let dir = app.data_dir.clone();
        app.cli_usage.reset_hints(&dir);
    }
}

/// Click routing for the marker and the open tip's two actions.
/// Consumes only clicks on those rects; everything else falls through
/// so the marker never steals clicks from the UI underneath.
pub(crate) fn handle_hint_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    // The Help tab's reset stays clickable even with a tip open: the tip
    // is centered, the reset row is behind it, and both persist hints.
    if app.hub_open && app.hub.tab == HubTab::Help && contains(app.hub.help_reset_rect, mouse) {
        let dir = app.data_dir.clone();
        app.cli_usage.reset_hints(&dir);
        return true;
    }
    if let Some(id) = app.hint_tip_open {
        if contains(app.hint_tip_got_it, mouse) {
            let dir = app.data_dir.clone();
            app.cli_usage.dismiss_hint(&dir, id);
            app.hint_tip_open = None;
            return true;
        }
        if contains(app.hint_tip_hide_all, mouse) {
            let dir = app.data_dir.clone();
            app.cli_usage.hide_all_hints(&dir);
            app.hint_tip_open = None;
            return true;
        }
        if contains(app.hint_marker, mouse) {
            app.hint_tip_open = None;
            return true;
        }
        return false;
    }
    if contains(app.hint_marker, mouse) {
        if let Some(hint) = active_hint(app) {
            app.hint_tip_open = Some(hint.id);
            return true;
        }
    }
    false
}

fn contains(rect: Option<Rect>, mouse: &MouseEvent) -> bool {
    rect.is_some_and(|rect| {
        mouse.column >= rect.x
            && mouse.column < rect.x + rect.width
            && mouse.row >= rect.y
            && mouse.row < rect.y + rect.height
    })
}

/// The marker's pulse: bright accent and dim alternate every ~500 ms
/// (the fast tick is 50 ms); static dim when animations are off.
pub(crate) fn tick_style(app: &App, theme: &Theme) -> Style {
    if !crate::tui::ui::animations_enabled(app) {
        return Style::default().fg(theme.dim_text);
    }
    if (crate::tui::ui::effective_animation_tick(app) / 10).is_multiple_of(2) {
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim_text)
    }
}
