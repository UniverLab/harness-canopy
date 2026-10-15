use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::tui::ui::theme::Theme;

/// Which send shortcut is currently active, for the footer/hint line.
/// Shift+Enter requires the terminal's Kitty keyboard enhancement protocol
/// to disambiguate it from plain Enter; where that isn't supported, Ctrl+S
/// remains the fallback (see `run_tui`'s `supports_keyboard_enhancement`
/// probe at startup).
pub(super) fn active_send_shortcut_label(
    keyboard_enhancement_active: bool,
) -> (&'static str, &'static str) {
    if keyboard_enhancement_active {
        ("Shift+Enter ", "send")
    } else {
        ("Ctrl+S ", "send")
    }
}

/// One entry in the prompt builder's shortcut/help bar.
pub(super) struct ShortcutHint {
    /// Dim key label, includes its trailing space (e.g. `"Ctrl+A "`).
    pub(super) key: String,
    /// White description, includes trailing padding (e.g. `"add section  "`).
    pub(super) desc: String,
    /// Keep-priority: lower is more important. On a narrow window the
    /// highest-priority-number entries are dropped first. `Ctrl+S send` (1)
    /// and `Esc hide` (2) are never dropped.
    pub(super) priority: u8,
}

impl ShortcutHint {
    pub(super) fn width(&self) -> usize {
        self.key.chars().count() + self.desc.chars().count()
    }
}

/// All shortcut hints in left-to-right DISPLAY order. Priority (keep→drop):
/// Ctrl+S send, Esc hide, Shift+↑↓ navigate, Ctrl+L recall, Ctrl+P scheduled,
/// @ file, Ctrl+A add section, Ctrl+X remove. The `Ctrl+P scheduled` hint only
/// appears when the selected session actually has pending scheduled sends.
pub(super) fn all_shortcut_hints(
    send_label: &str,
    send_hint: &str,
    has_scheduled: bool,
) -> Vec<ShortcutHint> {
    let mut hints = vec![
        ShortcutHint {
            key: "Shift+↑↓ ".to_string(),
            desc: "navigate  ".to_string(),
            priority: 3,
        },
        ShortcutHint {
            key: "@ ".to_string(),
            desc: "file  ".to_string(),
            priority: 5,
        },
        ShortcutHint {
            key: "Ctrl+A ".to_string(),
            desc: "add section  ".to_string(),
            priority: 6,
        },
        ShortcutHint {
            key: "Ctrl+X ".to_string(),
            desc: "remove  ".to_string(),
            priority: 7,
        },
        ShortcutHint {
            // Ctrl+E still works as an unadvertised alias.
            key: "Shift+←→ ".to_string(),
            desc: "tabs  ".to_string(),
            priority: 8,
        },
        ShortcutHint {
            key: "Ctrl+L ".to_string(),
            desc: "recall  ".to_string(),
            priority: 4,
        },
        ShortcutHint {
            key: send_label.to_string(),
            desc: format!("{send_hint}  "),
            priority: 1,
        },
        ShortcutHint {
            key: "Esc  ".to_string(),
            desc: "hide".to_string(),
            priority: 2,
        },
    ];
    // Advertise the scheduled-list entry point only when it does something —
    // inserted just before the send/hide pair so it reads next to recall.
    if has_scheduled {
        let insert_at = hints.len().saturating_sub(2);
        hints.insert(
            insert_at,
            ShortcutHint {
                key: "Ctrl+P ".to_string(),
                desc: "scheduled  ".to_string(),
                priority: 4,
            },
        );
    }
    hints
}

/// Pure function: pick which shortcut hints fit into `width` columns, in
/// display order, dropping the least important first. `Ctrl+S send` and
/// `Esc hide` (priority ≤ 2) are always kept even when they overflow.
pub(super) fn select_shortcut_hints(
    width: usize,
    send_label: &str,
    send_hint: &str,
    has_scheduled: bool,
) -> Vec<ShortcutHint> {
    let mut hints = all_shortcut_hints(send_label, send_hint, has_scheduled);
    loop {
        let total: usize = hints.iter().map(ShortcutHint::width).sum();
        if total <= width {
            break;
        }
        // Drop the least important (highest priority number) droppable entry.
        let victim = hints
            .iter()
            .enumerate()
            .filter(|(_, hint)| hint.priority > 2)
            .max_by_key(|(_, hint)| hint.priority)
            .map(|(idx, _)| idx);
        match victim {
            Some(idx) => {
                hints.remove(idx);
            }
            None => break, // only the two mandatory hints remain
        }
    }
    hints
}

/// Build the hint line — rendered by priority so the most important shortcuts
/// (Ctrl+S send, Esc hide) survive on narrow windows instead of scrolling off
/// the end.
pub(super) fn prompt_hint_line(
    width: u16,
    keyboard_enhancement_active: bool,
    has_pending: bool,
    theme: &Theme,
) -> Line<'static> {
    let (send_label, send_hint) = active_send_shortcut_label(keyboard_enhancement_active);
    let hint_spans: Vec<Span> =
        select_shortcut_hints(width as usize, send_label, send_hint, has_pending)
            .into_iter()
            .flat_map(|hint| {
                [
                    Span::styled(hint.key, Style::default().fg(theme.dim_text)),
                    Span::styled(hint.desc, Style::default().fg(theme.text_primary)),
                ]
            })
            .collect();
    Line::from(hint_spans)
}
