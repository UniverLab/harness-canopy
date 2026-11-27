//! Global non-blocking calendar reminder toast queue, renderer, and input handlers.

use std::collections::VecDeque;

use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::sessions::client::new_req;
use crate::sessions::protocol::{toast_dismiss_frame, toast_snooze_frame, ToastNotification};
use crate::tui::app::types::{App, Focus, SidebarLayer};
use crate::tui::ui::theme::Theme;

/// Maximum number of concurrent toasts rendered on screen at once.
pub(crate) const MAX_VISIBLE_TOASTS: usize = 3;

const TOAST_WIDTH: u16 = 44;
const TOAST_HEIGHT: u16 = 5;
const DISMISS_LABEL: &str = "[Dismiss]";
const SNOOZE_LABEL: &str = "[Snooze 5m]";

/// User action triggered from a toast button or shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToastAction {
    Dismiss(String),
    Snooze { event_id: String, minutes: u32 },
}

/// One queued toast and its last-rendered clickable button rectangles.
#[derive(Debug, Clone)]
pub(crate) struct ToastEntry {
    pub notification: ToastNotification,
    pub dismiss_rect: Option<Rect>,
    pub snooze_rect: Option<Rect>,
}

/// Queue of active global calendar reminder toasts (oldest at front).
#[derive(Debug, Default)]
pub(crate) struct ToastQueue {
    entries: VecDeque<ToastEntry>,
}

impl ToastQueue {
    /// Return `true` when no toasts are queued.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of queued toasts.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Push a toast notification, deduplicating by `event_id` and keeping oldest-first order.
    pub(crate) fn push(&mut self, notification: ToastNotification) {
        self.entries
            .retain(|entry| entry.notification.event_id != notification.event_id);
        self.entries.push_back(ToastEntry {
            notification,
            dismiss_rect: None,
            snooze_rect: None,
        });
    }

    /// Return the visible slice of up to [`MAX_VISIBLE_TOASTS`] oldest-first toasts.
    pub(crate) fn visible_mut(&mut self) -> impl Iterator<Item = &mut ToastEntry> {
        self.entries.iter_mut().take(MAX_VISIBLE_TOASTS)
    }

    /// Return the visible slice of up to [`MAX_VISIBLE_TOASTS`] oldest-first toasts.
    #[cfg(test)]
    pub(crate) fn visible(&self) -> impl Iterator<Item = &ToastEntry> {
        self.entries.iter().take(MAX_VISIBLE_TOASTS)
    }

    /// Remove and return the toast matching `event_id`.
    pub(crate) fn remove_by_event(&mut self, event_id: &str) -> Option<ToastNotification> {
        let pos = self
            .entries
            .iter()
            .position(|entry| entry.notification.event_id == event_id)?;
        self.entries.remove(pos).map(|e| e.notification)
    }

    /// Return the oldest queued toast's `(event_id, snooze_minutes)`.
    pub(crate) fn oldest_target(&self) -> Option<(&str, u32)> {
        self.entries.front().map(|e| {
            (
                e.notification.event_id.as_str(),
                e.notification.snooze_minutes,
            )
        })
    }

    /// Clear all cached button hit-rects before the next frame draws.
    pub(crate) fn clear_rects(&mut self) {
        for entry in &mut self.entries {
            entry.dismiss_rect = None;
            entry.snooze_rect = None;
        }
    }

    /// Hit-test a terminal coordinate against visible toast action buttons.
    pub(crate) fn hit_test(&self, col: u16, row: u16) -> Option<ToastAction> {
        let pos = Position::new(col, row);
        for entry in self.entries.iter().take(MAX_VISIBLE_TOASTS) {
            if entry.dismiss_rect.is_some_and(|r| r.contains(pos)) {
                return Some(ToastAction::Dismiss(entry.notification.event_id.clone()));
            }
            if entry.snooze_rect.is_some_and(|r| r.contains(pos)) {
                return Some(ToastAction::Snooze {
                    event_id: entry.notification.event_id.clone(),
                    minutes: entry.notification.snooze_minutes,
                });
            }
        }
        None
    }
}

/// Render up to [`MAX_VISIBLE_TOASTS`] stacked banners at the top-right without stealing focus.
pub(crate) fn draw_toasts(frame: &mut Frame, app: &mut App, theme: &Theme) {
    if app.toast.is_empty() {
        return;
    }
    let area = frame.area();
    if area.width < 24 || area.height < TOAST_HEIGHT + 2 {
        return;
    }
    let width = TOAST_WIDTH.min(area.width.saturating_sub(2));
    let x = area.width.saturating_sub(width + 1);

    for (idx, entry) in app.toast.visible_mut().enumerate() {
        let y = 1 + (idx as u16) * TOAST_HEIGHT;
        if y + TOAST_HEIGHT > area.height.saturating_sub(1) {
            break;
        }
        let rect = Rect::new(x, y, width, TOAST_HEIGHT);
        frame.render_widget(Clear, rect);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.header_color))
            .style(Style::default().bg(theme.dialog_bg).fg(theme.text_primary));
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let max_chars = inner.width as usize;
        let title_line = Line::from(Span::styled(
            truncate_chars(&entry.notification.title, max_chars),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ));
        let body_line = Line::from(Span::styled(
            truncate_chars(&entry.notification.body, max_chars),
            Style::default().fg(theme.dim_text),
        ));
        let action_line = Line::from(vec![
            Span::styled(
                DISMISS_LABEL,
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                SNOOZE_LABEL,
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        let para = Paragraph::new(vec![title_line, body_line, action_line]);
        frame.render_widget(para, inner);

        if inner.height >= 3 {
            let btn_y = inner.y + 2;
            let dismiss_w = (DISMISS_LABEL.len() as u16).min(inner.width);
            entry.dismiss_rect = Some(Rect::new(inner.x, btn_y, dismiss_w, 1));
            let snooze_x = inner.x.saturating_add(dismiss_w + 2);
            if snooze_x < inner.x + inner.width {
                let avail = (inner.x + inner.width).saturating_sub(snooze_x);
                let snooze_w = (SNOOZE_LABEL.len() as u16).min(avail);
                entry.snooze_rect = Some(Rect::new(snooze_x, btn_y, snooze_w, 1));
            }
        }
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let prefix: String = text.chars().take(keep).collect();
    format!("{prefix}...")
}

/// Handle mouse clicks on visible toast `[Dismiss]` / `[Snooze 5m]` buttons without stealing focus.
pub(crate) fn handle_toast_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    let Some(action) = app.toast.hit_test(mouse.column, mouse.row) else {
        return false;
    };
    dispatch_toast_action(app, action);
    true
}

/// Handle global `Alt+D` (dismiss) and `Alt+S` (snooze 5m) over any active view.
pub(crate) fn handle_toast_global_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    let Some((event_id, snooze_minutes)) = oldest_target_owned(app) else {
        return false;
    };
    let is_alt_d = modifiers.contains(KeyModifiers::ALT)
        && matches!(code, KeyCode::Char('d') | KeyCode::Char('D'));
    let is_alt_s = modifiers.contains(KeyModifiers::ALT)
        && matches!(code, KeyCode::Char('s') | KeyCode::Char('S'));
    trigger_toast_shortcut(app, event_id, snooze_minutes, is_alt_d, is_alt_s)
}

/// Handle plain `d` / `s` when a toast is visible and no PTY, modal, or text filter owns the key.
pub(crate) fn handle_toast_plain_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    if modifiers != KeyModifiers::NONE || !plain_toast_shortcut_allowed(app) {
        return false;
    }
    let Some((event_id, snooze_minutes)) = oldest_target_owned(app) else {
        return false;
    };
    trigger_toast_shortcut(
        app,
        event_id,
        snooze_minutes,
        code == KeyCode::Char('d'),
        code == KeyCode::Char('s'),
    )
}

fn oldest_target_owned(app: &App) -> Option<(String, u32)> {
    app.toast
        .oldest_target()
        .map(|(id, mins)| (id.to_string(), mins))
}

fn plain_toast_shortcut_allowed(app: &App) -> bool {
    if !matches!(app.focus, Focus::Home | Focus::Preview) {
        return false;
    }
    if app.hub_open
        || app.quit_confirm
        || app.terminal_search.is_some()
        || app.quick_menu.is_some()
        || app.session_rename_input.is_some()
        || app.delete_project_confirm
        || app.archive_graph_confirm
        || app.permanent_delete_graph_confirm
        || app.graph_reset_confirm
        || app.graph_autorun_dialog.is_some()
        || app.playground_active
    {
        return false;
    }
    let project_typeahead_active = app.focus == Focus::Preview
        && app.sidebar_layer == SidebarLayer::Knowledge
        && app.project_focus.is_none()
        && !app.agents_rag_focused;
    !project_typeahead_active
}

fn trigger_toast_shortcut(
    app: &mut App,
    event_id: String,
    snooze_minutes: u32,
    dismiss: bool,
    snooze: bool,
) -> bool {
    if dismiss {
        dispatch_toast_action(app, ToastAction::Dismiss(event_id));
        return true;
    }
    if snooze {
        dispatch_toast_action(
            app,
            ToastAction::Snooze {
                event_id,
                minutes: snooze_minutes,
            },
        );
        return true;
    }
    false
}

fn dispatch_toast_action(app: &mut App, action: ToastAction) {
    let req = new_req();
    match action {
        ToastAction::Dismiss(event_id) => {
            app.session_hub
                .send_frame(toast_dismiss_frame(&req, &event_id));
            app.toast.remove_by_event(&event_id);
        }
        ToastAction::Snooze { event_id, minutes } => {
            app.session_hub
                .send_frame(toast_snooze_frame(&req, &event_id, Some(minutes)));
            app.toast.remove_by_event(&event_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_toast(id: &str, event_id: &str, title: &str) -> ToastNotification {
        ToastNotification {
            id: id.to_string(),
            event_id: event_id.to_string(),
            title: title.to_string(),
            body: "2026-11-27 10:00".to_string(),
            event_at: "2026-11-27T10:00:00Z".to_string(),
            snooze_minutes: 5,
        }
    }

    #[test]
    fn stack_cap_three_visible_and_oldest_first() {
        let mut queue = ToastQueue::default();
        queue.push(sample_toast("t1", "e1", "First"));
        queue.push(sample_toast("t2", "e2", "Second"));
        queue.push(sample_toast("t3", "e3", "Third"));
        queue.push(sample_toast("t4", "e4", "Fourth"));

        assert_eq!(queue.len(), 4);
        let visible: Vec<&str> = queue
            .visible()
            .map(|e| e.notification.event_id.as_str())
            .collect();
        assert_eq!(visible, vec!["e1", "e2", "e3"]);

        // Dismiss oldest (e1) -> e4 becomes visible.
        assert!(queue.remove_by_event("e1").is_some());
        let visible_after: Vec<&str> = queue
            .visible()
            .map(|e| e.notification.event_id.as_str())
            .collect();
        assert_eq!(visible_after, vec!["e2", "e3", "e4"]);
    }

    #[test]
    fn hit_test_matches_dismiss_and_snooze_without_focus_steal() {
        let mut queue = ToastQueue::default();
        queue.push(sample_toast("t1", "e1", "Standup"));
        if let Some(entry) = queue.visible_mut().next() {
            entry.dismiss_rect = Some(Rect::new(50, 4, 9, 1));
            entry.snooze_rect = Some(Rect::new(61, 4, 11, 1));
        }

        assert_eq!(
            queue.hit_test(52, 4),
            Some(ToastAction::Dismiss("e1".to_string()))
        );
        assert_eq!(
            queue.hit_test(65, 4),
            Some(ToastAction::Snooze {
                event_id: "e1".to_string(),
                minutes: 5,
            })
        );
        assert_eq!(queue.hit_test(10, 10), None);

        queue.clear_rects();
        assert_eq!(queue.hit_test(52, 4), None);
    }

    fn test_app() -> (App, tempfile::TempDir) {
        let path = crate::test_support::temp_db_path();
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        (app, data_dir)
    }

    #[test]
    fn key_shortcuts_respect_focus_and_overlays() {
        let (mut app, _dir) = test_app();
        app.toast.push(sample_toast("t1", "e1", "First"));
        app.toast.push(sample_toast("t2", "e2", "Second"));

        // Plain 'd' must NOT fire in Focus::Agent.
        app.focus = Focus::Agent;
        assert!(!handle_toast_plain_key(
            &mut app,
            KeyCode::Char('d'),
            KeyModifiers::NONE
        ));
        assert_eq!(app.toast.len(), 2);

        // Alt+D works in Focus::Agent without stealing focus, removing oldest ("e1").
        assert!(handle_toast_global_key(
            &mut app,
            KeyCode::Char('d'),
            KeyModifiers::ALT
        ));
        assert!(matches!(app.focus, Focus::Agent));
        assert_eq!(app.toast.oldest_target(), Some(("e2", 5)));

        // Alt+S works even when Hub is open.
        app.hub_open = true;
        assert!(handle_toast_global_key(
            &mut app,
            KeyCode::Char('s'),
            KeyModifiers::ALT
        ));
        assert!(app.toast.is_empty());

        // Plain 's' is blocked when Knowledge project type-ahead is active,
        // and allowed on Home when no modal is open.
        app.hub_open = false;
        app.toast.push(sample_toast("t3", "e3", "Third"));
        app.focus = Focus::Preview;
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.project_focus = None;
        app.agents_rag_focused = false;
        assert!(!handle_toast_plain_key(
            &mut app,
            KeyCode::Char('s'),
            KeyModifiers::NONE
        ));
        assert_eq!(app.toast.len(), 1);

        app.focus = Focus::Home;
        assert!(handle_toast_plain_key(
            &mut app,
            KeyCode::Char('s'),
            KeyModifiers::NONE
        ));
        assert!(app.toast.is_empty());
    }

    #[test]
    fn draw_and_mouse_click_dismisses_without_stealing_focus() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (mut app, _dir) = test_app();
        app.focus = Focus::Agent;
        app.toast.push(sample_toast("t1", "e1", "Standup"));

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::modern();
        terminal
            .draw(|frame| draw_toasts(frame, &mut app, &theme))
            .unwrap();

        let dismiss_rect = app.toast.visible().next().unwrap().dismiss_rect.unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: dismiss_rect.x,
            row: dismiss_rect.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(handle_toast_mouse(&mut app, &click));
        assert!(matches!(app.focus, Focus::Agent));
        assert!(app.toast.is_empty());
    }
}
