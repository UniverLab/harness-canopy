//! Per-section wheel scrolling for the Live sidebar tab.
//!
//! The Live tab renders two sub-lists (interactive sessions, terminal
//! sessions) that used to share one `sidebar_scroll_offset`,
//! so a wheel event over one section scrolled every section at once. Each
//! section now owns its view offset: the draw path records the section's
//! inner rect (the `prompt_raw_content_rect` pattern), the wheel handler
//! hit-tests the pointer against those rects, and keyboard navigation keeps
//! the selected row visible via `clamp_scroll`.

use ratatui::crossterm::event::{KeyModifiers, MouseEvent};

use super::mouse::rect_contains_point;
use super::scroll::scroll_sidebar;
use crate::tui::app::types::{AgentEntry, App, SidebarLayer};

/// One of the Live tab's two scrollable sub-lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Section {
    Interactive,
    Terminal,
}

/// Which Live section (if any) contains the pointer. `None` outside the
/// recorded section rects — the caller keeps today's behaviour there.
pub(super) fn section_at(app: &App, col: u16, row: u16) -> Option<Section> {
    if app
        .interactive_rect
        .is_some_and(|rect| rect_contains_point(rect, col, row))
    {
        return Some(Section::Interactive);
    }
    if app
        .terminal_rect
        .is_some_and(|rect| rect_contains_point(rect, col, row))
    {
        return Some(Section::Terminal);
    }
    None
}

/// Wheel routing for the sidebar: a wheel over one of the Live tab's
/// section rects scrolls that section's view; anywhere else in the sidebar
/// keeps today's shared-offset behaviour. Always consumed.
pub(super) fn scroll_sidebar_wheel(app: &mut App, mouse: &MouseEvent, dir: i32) -> bool {
    if app.sidebar_layer == SidebarLayer::Live {
        if let Some(sec) = section_at(app, mouse.column, mouse.row) {
            let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
            scroll_section(app, sec, dir, shift);
            return true;
        }
    }
    scroll_sidebar(app, dir);
    true
}

/// Wheel-scroll one section's view by 1 row per notch (3 with Shift),
/// clamped to `len - visible`. Never touches `selected`.
pub(super) fn scroll_section(app: &mut App, sec: Section, dir: i32, shift: bool) {
    let step: usize = if shift { 3 } else { 1 };
    let max = section_len(app, sec).saturating_sub(section_visible(app, sec));
    let off = section_offset(app, sec);
    let next = if dir < 0 {
        off.saturating_add(step).min(max)
    } else {
        off.saturating_sub(step)
    };
    set_section_offset(app, sec, next);
}

/// Visible card rows for an inner height: 3-row cards + 1-row gap,
/// mirroring `draw_agent_list`'s `max_visible` math.
pub(crate) fn card_visible_rows(inner_height: u16) -> usize {
    ((inner_height.saturating_sub(3)) / 4 + 1) as usize
}

/// Row count of one Live section.
pub(crate) fn section_len(app: &App, sec: Section) -> usize {
    match sec {
        Section::Interactive => count_entries(app, true),
        Section::Terminal => count_entries(app, false),
    }
}

fn count_entries(app: &App, interactive: bool) -> usize {
    app.agents
        .iter()
        .filter(|entry| match entry {
            AgentEntry::Interactive(_) | AgentEntry::Orphaned(_) => interactive,
            AgentEntry::Terminal(_) => !interactive,
            _ => false,
        })
        .count()
}

/// Visible rows of one Live section, derived from its last recorded inner
/// rect. Unrecorded sections report 1 so clamping stays conservative.
pub(crate) fn section_visible(app: &App, sec: Section) -> usize {
    let height = match sec {
        Section::Interactive => app.interactive_rect.map(|rect| rect.height),
        Section::Terminal => app.terminal_rect.map(|rect| rect.height),
    };
    let Some(height) = height else {
        return 1;
    };
    card_visible_rows(height).max(1)
}

/// Current view offset of one Live section.
pub(crate) fn section_offset(app: &App, sec: Section) -> usize {
    match sec {
        Section::Interactive => app.interactive_off,
        Section::Terminal => app.terminal_off,
    }
}

pub(crate) fn set_section_offset(app: &mut App, sec: Section, off: usize) {
    match sec {
        Section::Interactive => app.interactive_off = off,
        Section::Terminal => app.terminal_off = off,
    }
}

impl App {
    /// Clamp every Live section offset to its current `len - visible` after
    /// the lists changed size (session closed or added).
    pub(crate) fn clamp_live_section_offsets(&mut self) {
        for sec in [Section::Interactive, Section::Terminal] {
            let max = section_len(self, sec).saturating_sub(section_visible(self, sec));
            let off = section_offset(self, sec).min(max);
            set_section_offset(self, sec, off);
        }
    }

    /// Slide the selected row's own section just enough to keep it visible
    /// after keyboard navigation. Other sections are untouched.
    pub(crate) fn ensure_live_selection_visible(&mut self) {
        let Some((sec, local)) = selected_section_local(self) else {
            return;
        };
        let off = crate::tui::selection::clamp_scroll(
            local,
            section_offset(self, sec),
            section_len(self, sec),
            section_visible(self, sec),
        );
        set_section_offset(self, sec, off);
    }
}

/// Flat `selected` as (section, section-local index), or `None` for rows
/// that render outside the two Live sections.
fn selected_section_local(app: &App) -> Option<(Section, usize)> {
    let sec = match app.agents.get(app.selected)? {
        AgentEntry::Interactive(_) | AgentEntry::Orphaned(_) => Section::Interactive,
        AgentEntry::Terminal(_) => Section::Terminal,
        AgentEntry::Agent(_) | AgentEntry::Corrupt(_) => return None,
    };
    let local = app
        .agents
        .iter()
        .take(app.selected)
        .filter(|entry| section_of_entry(entry) == Some(sec))
        .count();
    Some((sec, local))
}

fn section_of_entry(entry: &AgentEntry) -> Option<Section> {
    match entry {
        AgentEntry::Interactive(_) | AgentEntry::Orphaned(_) => Some(Section::Interactive),
        AgentEntry::Terminal(_) => Some(Section::Terminal),
        AgentEntry::Agent(_) | AgentEntry::Corrupt(_) => None,
    }
}
