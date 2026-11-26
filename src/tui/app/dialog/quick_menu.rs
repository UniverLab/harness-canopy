//! Ready-prompt tabs + F5 menu for interactive sessions.
//!
//! The set comes from `domain::quick_prompts`: the prompt builder (a special
//! action item) followed by canopy's built-in prompts and the user's
//! `quick/*.md` files. The work window shows a passive single-row strip of
//! small numbered tabs at its top right; `F5` opens a menu dialog listing
//! every item by name, number (1–9) and one-line preview. Choosing a prompt
//! inserts its text into the focused agent's PTY input via bracketed paste
//! as-is — never submitting — while choosing the builder opens the prompt
//! builder dialog. The tab row never takes focus.

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::domain::quick_prompts::{self, QuickItem};
use crate::tui::app::types::{AgentEntry, App};
use crate::tui::ui::theme::Theme;

/// Open F5 menu state: the loaded set plus the highlighted row.
pub struct QuickMenu {
    pub items: Vec<QuickItem>,
    pub selected: usize,
}

/// One tab-strip hit box from the last frame: `index` is the set index, or
/// `None` for the `+N` overflow badge (which opens the F5 menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickTabHit {
    pub index: Option<usize>,
    pub row: u16,
    pub x0: u16,
    pub x1: u16,
}

/// Label of tab `index`: `[1 Builder]`, `[2 onboarding]`, …
pub fn tab_label(index: usize, item: &QuickItem) -> String {
    format!("[{} {}]", index + 1, item.name())
}

/// Right-aligned single-row layout of the tab strip, relative to the
/// strip's left edge. Tabs are taken from item 1 rightward while the next
/// tab fits; when any do not fit a trailing `+N` badge claims the rest.
#[derive(Debug, PartialEq, Eq)]
pub struct TabLayout {
    /// `(set index, relative x0, cell width)` for every shown tab.
    pub cells: Vec<(usize, u16, u16)>,
    /// `(relative x0, width, hidden count)` of the trailing `+N` badge.
    pub overflow: Option<(u16, u16, usize)>,
}

pub fn layout_tabs(items: &[QuickItem], available_width: u16) -> TabLayout {
    if available_width < 6 || items.is_empty() {
        return TabLayout {
            cells: Vec::new(),
            overflow: None,
        };
    }
    let widths: Vec<usize> = items
        .iter()
        .enumerate()
        .map(|(index, item)| tab_label(index, item).chars().count())
        .collect();
    let shown = (0..=items.len())
        .rev()
        .find(|&count| row_width(&widths, count) <= available_width as usize)
        .unwrap_or(0);
    let mut cells = Vec::new();
    let mut x = available_width as usize - row_width(&widths, shown);
    for (index, width) in widths.iter().enumerate().take(shown) {
        cells.push((index, x as u16, *width as u16));
        x += width + 1;
    }
    let overflow = if shown < items.len() {
        let hidden = items.len() - shown;
        let badge_width = format!("+{hidden}").chars().count();
        let x0 = if shown == 0 {
            available_width as usize - badge_width
        } else {
            x
        };
        Some((x0 as u16, badge_width as u16, hidden))
    } else {
        None
    };
    TabLayout { cells, overflow }
}

fn row_width(widths: &[usize], shown: usize) -> usize {
    if shown == 0 {
        return format!("+{}", widths.len()).chars().count();
    }
    let labels: usize = widths.iter().take(shown).sum();
    let gaps = if shown < widths.len() {
        shown
    } else {
        shown - 1
    };
    let badge = if shown < widths.len() {
        format!("+{}", widths.len() - shown).chars().count()
    } else {
        0
    };
    labels + gaps + badge
}

/// What choosing set index `index` does: insert text as-is, or open the
/// prompt builder. Pure so tests can assert the no-`\r` contract without a
/// live PTY: the text travels byte-identical into `paste_to_pty`.
#[derive(Debug, PartialEq, Eq)]
pub enum QuickChoice {
    InsertText(String),
    OpenBuilder,
}

pub fn choice_for(items: &[QuickItem], index: usize) -> Option<QuickChoice> {
    match items.get(index)? {
        QuickItem::Builder => Some(QuickChoice::OpenBuilder),
        item => Some(QuickChoice::InsertText(item.body()?.to_string())),
    }
}

/// Digit keys choose items 1–9 directly; anything else (including `0` and
/// items 10+) is left to arrow navigation.
pub fn digit_index(key: char, len: usize) -> Option<usize> {
    if !('1'..='9').contains(&key) {
        return None;
    }
    let index = (key as u8 - b'1') as usize;
    (index < len).then_some(index)
}

impl App {
    /// Load the set fresh from `data_dir` without opening anything.
    fn open_quick_menu_items(&mut self) {
        let items = quick_prompts::load(&self.data_dir);
        self.quick_menu = Some(QuickMenu { items, selected: 0 });
    }

    /// Open the F5 menu over the focused session, loading the set fresh so
    /// an external edit to `quick/*.md` shows up immediately.
    pub fn open_quick_menu(&mut self) {
        self.open_quick_menu_items();
        self.quick_menu_rect = None;
    }

    /// Apply set index `index`: insert the prompt text at the focused
    /// agent's PTY input cursor (as-is, no submit), or open the builder.
    /// The menu is closed either way. Loads the set on demand so a tab
    /// click works with no menu open.
    pub fn choose_quick_item(&mut self, index: usize) {
        if self.quick_menu.is_none() {
            self.open_quick_menu_items();
        }
        let choice = self
            .quick_menu
            .as_ref()
            .and_then(|menu| choice_for(&menu.items, index));
        self.quick_menu = None;
        self.quick_menu_rect = None;
        match choice {
            Some(QuickChoice::InsertText(text)) => self.insert_quick_text(&text),
            Some(QuickChoice::OpenBuilder) => self.open_simple_prompt_dialog(None),
            None => {}
        }
    }

    fn insert_quick_text(&mut self, text: &str) {
        let target = match self.selected_agent() {
            Some(AgentEntry::Interactive(index)) => Some((*index, false)),
            Some(AgentEntry::Terminal(index)) => Some((*index, true)),
            _ => None,
        };
        let Some((index, is_terminal)) = target else {
            return;
        };
        let agent = if is_terminal {
            self.terminal_agents.get(index)
        } else {
            self.interactive_agents.get(index)
        };
        if let Some(agent) = agent {
            let _ = agent.paste_to_pty(text);
        }
    }
}

/// Keys while the F5 menu is open: digits choose 1–9 directly, `Up`/`Down`
/// move (wrapping, so items 10+ are reachable) with `Enter` to confirm,
/// `Esc` closes. Nothing else is bound.
pub fn handle_quick_menu_key(app: &mut App, code: KeyCode) -> Result<()> {
    let Some(selected) = app.quick_menu.as_ref().map(|menu| menu.selected) else {
        return Ok(());
    };
    let len = app.quick_menu.as_ref().map_or(0, |menu| menu.items.len());
    match code {
        KeyCode::Esc => {
            app.quick_menu = None;
            app.quick_menu_rect = None;
        }
        KeyCode::Up => {
            if len > 0 {
                if let Some(menu) = app.quick_menu.as_mut() {
                    menu.selected = selected.checked_sub(1).unwrap_or(len - 1);
                }
            }
        }
        KeyCode::Down => {
            if len > 0 {
                if let Some(menu) = app.quick_menu.as_mut() {
                    menu.selected = (selected + 1) % len;
                }
            }
        }
        KeyCode::Enter => {
            app.choose_quick_item(selected);
        }
        KeyCode::Char(key) => {
            if let Some(index) = digit_index(key, len) {
                app.choose_quick_item(index);
            }
        }
        _ => {}
    }
    Ok(())
}

/// Mouse while the menu is open (modal: every click is swallowed) or on the
/// tab strip: a row click chooses that item, a `+N` click opens the menu.
/// Returns true when the event was consumed.
pub fn handle_quick_menu_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if app.quick_menu.is_some() {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(index) = menu_row_at(app, mouse.row) {
                app.choose_quick_item(index);
            }
        }
        return true;
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    let hit = app
        .quick_tab_click_map
        .iter()
        .find(|hit| mouse.row == hit.row && mouse.column >= hit.x0 && mouse.column < hit.x1)
        .copied();
    match hit {
        Some(QuickTabHit { index: Some(i), .. }) => {
            app.choose_quick_item(i);
            true
        }
        Some(QuickTabHit { index: None, .. }) => {
            app.open_quick_menu();
            true
        }
        None => false,
    }
}

fn menu_row_at(app: &App, row: u16) -> Option<usize> {
    let menu = app.quick_menu.as_ref()?;
    let rect = app.quick_menu_rect?;
    let index = row.checked_sub(rect.y)? as usize;
    (index < menu.items.len()).then_some(index)
}

/// Passive single-row strip of numbered tabs at the top right of the work
/// window. Never takes focus: it only renders and records hit boxes.
/// Hidden cleanly when the area is under 6 columns wide.
pub fn draw_quick_tabs(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let items = quick_prompts::load(&app.data_dir);
    let layout = layout_tabs(&items, area.width);
    if layout.cells.is_empty() && layout.overflow.is_none() {
        app.quick_tab_click_map.clear();
        return;
    }
    let row = area.y;
    let mut hits = Vec::new();
    for (index, rel_x0, width) in &layout.cells {
        let absolute = area.x + rel_x0;
        let label = tab_label(*index, &items[*index]);
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(theme.header_color).bg(Color::Black)),
            Rect::new(absolute, row, *width, 1),
        );
        hits.push(QuickTabHit {
            index: Some(*index),
            row,
            x0: absolute,
            x1: absolute + width,
        });
    }
    if let Some((rel_x0, width, _)) = layout.overflow {
        let absolute = area.x + rel_x0;
        let hidden = items.len() - layout.cells.len();
        frame.render_widget(
            Paragraph::new(format!("+{hidden}"))
                .style(Style::default().fg(Color::Black).bg(theme.header_color)),
            Rect::new(absolute, row, width, 1),
        );
        hits.push(QuickTabHit {
            index: None,
            row,
            x0: absolute,
            x1: absolute + width,
        });
    }
    app.quick_tab_click_map = hits;
}

/// F5 menu dialog: every set item by number (1–9), name and one-line
/// preview. Rendered centered over the session; the used rows are stored
/// for mouse hit-testing.
pub fn draw_quick_menu(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let Some((items, selected)) = app
        .quick_menu
        .as_ref()
        .map(|menu| (menu.items.clone(), menu.selected))
    else {
        return;
    };
    let box_height = (items.len() as u16 + 6).clamp(8, 22);
    let area = crate::tui::ui::centered_rect(60, box_height, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Quick prompts (F5) ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if rows.len() as u16 >= inner.height.saturating_sub(1) {
            break;
        }
        let number = if index < 9 {
            format!("{} ", index + 1)
        } else {
            "  ".to_string()
        };
        let max_preview = inner
            .width
            .saturating_sub(number.chars().count() as u16 + item.name().chars().count() as u16 + 6)
            as usize;
        let line = format!("  {number}{} — {} ", item.name(), item.preview(max_preview));
        let style = if index == selected {
            Style::default()
                .fg(Color::Black)
                .bg(theme.header_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        rows.push(Line::from(vec![Span::styled(line, style)]));
    }
    let shown = rows.len();
    let hint = Line::from(vec![
        Span::styled("1-9 ", Style::default().fg(theme.dim_text)),
        Span::styled("choose  ", Style::default().fg(Color::White)),
        Span::styled("↑↓+Enter ", Style::default().fg(theme.dim_text)),
        Span::styled("10+  ", Style::default().fg(Color::White)),
        Span::styled("Esc ", Style::default().fg(theme.dim_text)),
        Span::styled("close", Style::default().fg(Color::White)),
    ]);
    rows.push(hint);
    frame.render_widget(Paragraph::new(rows), inner);
    app.quick_menu_rect = Some(Rect::new(inner.x, inner.y, inner.width, shown as u16));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::{Arc, Mutex};

    fn test_app() -> (App, tempfile::TempDir) {
        let path = crate::test_support::temp_db_path();
        let db = Arc::new(crate::db::Database::new(&path).expect("create test db"));
        let data_dir = tempfile::tempdir().expect("create data dir");
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        (app, data_dir)
    }

    fn write_quick_files(data_dir: &std::path::Path, names: &[&str]) {
        let quick = crate::domain::prompts::quick_dir(data_dir);
        std::fs::create_dir_all(&quick).unwrap();
        for name in names {
            std::fs::write(
                quick.join(format!("{name}.md")),
                format!("body of {name}\n"),
            )
            .unwrap();
        }
    }

    fn attach_cat_session(app: &mut App) {
        let workdir = app.current_workdir().to_string_lossy().to_string();
        let agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
            "cat",
            &workdir,
            80,
            24,
            Some("session-under-test"),
            &[],
            ratatui::style::Color::Reset,
        )
        .expect("spawn cat as a stand-in harness session");
        app.interactive_agents.push(agent);
        app.agents.push(AgentEntry::Interactive(0));
        app.selected = 0;
        app.interactive_agents[0].test_sink = Some(Arc::new(Mutex::new(Vec::new())));
    }

    fn render_menu_text(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::classic();
        terminal
            .draw(|frame| draw_quick_menu(frame, app, &theme))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn layout_shows_everything_when_it_fits() {
        let items = vec![
            QuickItem::Builder,
            QuickItem::Prompt {
                name: "onboarding".to_string(),
                body: "b".to_string(),
            },
        ];
        let layout = layout_tabs(&items, 60);
        assert_eq!(layout.cells.len(), 2);
        assert!(layout.overflow.is_none());
        assert_eq!(tab_label(0, &items[0]), "[1 Builder]");
        assert_eq!(tab_label(1, &items[1]), "[2 onboarding]");
    }

    #[test]
    fn layout_truncates_from_the_end_with_a_plus_n_badge() {
        let items = vec![
            QuickItem::Builder,
            QuickItem::Prompt {
                name: "onboarding".to_string(),
                body: "b".to_string(),
            },
            QuickItem::Prompt {
                name: "protocol".to_string(),
                body: "b".to_string(),
            },
        ];
        // "[1 Builder]" is 12 wide; "[2 onboarding]" is 15; "[3 protocol]"
        // is 13. Width 31 fits the first two (12 + 1 + 15) plus the gap and
        // the "+1" badge (2 + 2); the third tab does not fit.
        let layout = layout_tabs(&items, 31);
        assert_eq!(layout.cells.len(), 2);
        assert_eq!(layout.overflow, Some((29, 2, 1)));
    }

    #[test]
    fn layout_hides_cleanly_below_six_columns() {
        let items = vec![QuickItem::Builder];
        assert_eq!(
            layout_tabs(&items, 5),
            TabLayout {
                cells: Vec::new(),
                overflow: None,
            }
        );
        assert_eq!(
            layout_tabs(&items, 0),
            TabLayout {
                cells: Vec::new(),
                overflow: None,
            }
        );
    }

    #[test]
    fn digits_choose_one_through_nine_directly() {
        assert_eq!(digit_index('1', 12), Some(0));
        assert_eq!(digit_index('9', 12), Some(8));
        assert_eq!(digit_index('9', 3), None);
        assert_eq!(digit_index('0', 12), None);
        assert_eq!(digit_index('a', 12), None);
    }

    #[test]
    fn arrows_wrap_past_item_nine_and_enter_confirms() {
        let (mut app, _data) = test_app();
        app.open_quick_menu();
        let len = app.quick_menu.as_ref().unwrap().items.len();
        assert!(len >= 6);
        handle_quick_menu_key(&mut app, KeyCode::Up).unwrap();
        assert_eq!(app.quick_menu.as_ref().unwrap().selected, len - 1);
        handle_quick_menu_key(&mut app, KeyCode::Down).unwrap();
        assert_eq!(app.quick_menu.as_ref().unwrap().selected, 0);
        // Enter on the highlighted row (the builder) opens the dialog.
        handle_quick_menu_key(&mut app, KeyCode::Enter).unwrap();
        assert!(app.quick_menu.is_none());
        assert!(app.simple_prompt_dialog.is_some());
    }

    #[test]
    fn tenth_item_is_reached_with_arrows_and_enter_not_digits() {
        let (mut app, data_dir) = test_app();
        write_quick_files(data_dir.path(), &["zeta", "alpha", "m1", "m2", "m3"]);
        attach_cat_session(&mut app);
        app.open_quick_menu();
        let items = app.quick_menu.as_ref().unwrap().items.clone();
        // Builder + 5 builtins + 5 user files, extras sorted after builtins.
        assert_eq!(items.len(), 11);
        assert_eq!(items[9].name(), "m3");
        // Digits stop at item 9: nothing selects the 10th item directly.
        assert_eq!(digit_index('9', items.len()), Some(8));
        for _ in 0..9 {
            handle_quick_menu_key(&mut app, KeyCode::Down).unwrap();
        }
        assert_eq!(app.quick_menu.as_ref().unwrap().selected, 9);
        handle_quick_menu_key(&mut app, KeyCode::Enter).unwrap();
        assert!(app.quick_menu.is_none());
        let sink = app.interactive_agents[0]
            .test_sink
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .clone();
        let body = items[9].body().unwrap().as_bytes().to_vec();
        assert_eq!(sink, body);
        assert!(
            !sink.contains(&b'\r'),
            "choosing a prompt must never submit: {sink:?}"
        );
    }

    #[test]
    fn menu_lists_names_numbers_and_previews() {
        let (mut app, data_dir) = test_app();
        write_quick_files(data_dir.path(), &["zeta"]);
        app.open_quick_menu();
        let text = render_menu_text(&mut app, 80, 24);
        assert!(
            text.contains("1 Builder"),
            "menu numbers the builder: {text}"
        );
        assert!(
            text.contains("Open the interactive prompt"),
            "menu shows the builder preview (truncated to fit): {text}"
        );
        assert!(
            text.contains("2 onboarding"),
            "menu numbers builtins: {text}"
        );
        assert!(text.contains("zeta"), "menu lists user files: {text}");
        assert!(
            text.contains("body of zeta"),
            "menu previews bodies: {text}"
        );
    }

    #[test]
    fn choosing_a_prompt_writes_its_text_without_submit() {
        let (mut app, _data) = test_app();
        attach_cat_session(&mut app);
        app.open_quick_menu();
        let body = app.quick_menu.as_ref().unwrap().items[1]
            .body()
            .unwrap()
            .to_string();
        assert!(!body.is_empty());
        app.choose_quick_item(1);

        let sink = app.interactive_agents[0]
            .test_sink
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .clone();
        assert_eq!(sink, body.as_bytes());
        assert!(
            !sink.contains(&b'\r'),
            "choosing a prompt must never submit: {sink:?}"
        );
        assert!(app.quick_menu.is_none());
    }

    #[test]
    fn choosing_the_builder_opens_the_builder_dialog() {
        let (mut app, _data) = test_app();
        attach_cat_session(&mut app);
        app.open_quick_menu();
        app.choose_quick_item(0);
        assert!(app.quick_menu.is_none());
        assert!(app.simple_prompt_dialog.is_some());
    }

    #[test]
    fn choice_for_maps_builder_and_prompts() {
        let items = vec![
            QuickItem::Builder,
            QuickItem::Prompt {
                name: "x".to_string(),
                body: "exact body".to_string(),
            },
        ];
        assert_eq!(choice_for(&items, 0), Some(QuickChoice::OpenBuilder));
        assert_eq!(
            choice_for(&items, 1),
            Some(QuickChoice::InsertText("exact body".to_string()))
        );
        assert_eq!(choice_for(&items, 7), None);
    }
}
