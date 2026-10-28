//! CT31: the F1 Canopy hub — tab state, tab switching and the Missions tab's
//! cursor. Rendering lives in `tui/ui/dialogs/hub*.rs`; the settings tab's
//! data and editors live in `hub_settings.rs`.

use super::types::App;

/// The five hub tabs, in the order the tab bar shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HubTab {
    #[default]
    Settings,
    Keys,
    Missions,
    Shelf,
    Remote,
}

pub(crate) const HUB_TABS: [HubTab; 5] = [
    HubTab::Settings,
    HubTab::Keys,
    HubTab::Missions,
    HubTab::Shelf,
    HubTab::Remote,
];

impl HubTab {
    pub(crate) fn label(self) -> &'static str {
        match self {
            HubTab::Settings => "Settings",
            HubTab::Keys => "Keys",
            HubTab::Missions => "Missions & stats",
            HubTab::Shelf => "Shelf",
            HubTab::Remote => "Remote",
        }
    }

    /// The `1`/`2`/`3`/`4`/`5` shortcut for a tab, or `None`.
    pub(crate) fn from_digit(c: char) -> Option<HubTab> {
        match c {
            '1' => Some(HubTab::Settings),
            '2' => Some(HubTab::Keys),
            '3' => Some(HubTab::Missions),
            '4' => Some(HubTab::Shelf),
            '5' => Some(HubTab::Remote),
            _ => None,
        }
    }

    /// The next tab, wrapping over [`HUB_TABS`].
    pub(crate) fn next(self, forward: bool) -> HubTab {
        let index = HUB_TABS.iter().position(|tab| *tab == self).unwrap_or(0);
        let len = HUB_TABS.len();
        let next = if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        };
        HUB_TABS[next]
    }
}

/// The hub's session-only state. Last-used tab is remembered here, never in
/// config (requirement 1).
#[derive(Default)]
pub(crate) struct HubState {
    pub tab: HubTab,
    /// The Settings tab's data and editors.
    pub settings: super::hub_settings::SettingsState,
    /// Keys tab scroll offset.
    pub keys_scroll: usize,
    /// The Shelf tab's merged file list, selection and toast.
    pub shelf: super::hub_shelf::ShelfState,
    /// The Remote tab's cached status view, cursor and flows.
    pub remote: super::hub_remote::RemoteState,
}

impl App {
    /// Open the hub on the last-used tab (in-memory only) and ask that tab
    /// for fresh data — or, on the Shelf tab, re-list the shelves so
    /// reopening never shows a stale list.
    pub(crate) fn open_hub(&mut self) {
        self.hub_open = true;
        if self.hub.tab == HubTab::Shelf {
            self.refresh_shelf();
        } else if self.hub.tab == HubTab::Remote {
            self.remote_request_refresh();
        } else {
            self.hub.settings.request_refresh();
        }
    }

    pub(crate) fn close_hub(&mut self) {
        self.hub_open = false;
    }

    pub(crate) fn toggle_hub(&mut self) {
        if self.hub_open {
            self.close_hub();
        } else {
            self.open_hub();
        }
    }

    /// Switch to `tab`, fetching what that tab shows: the Settings rows,
    /// the shelf listings, or the remote status.
    pub(crate) fn set_hub_tab(&mut self, tab: HubTab) {
        self.hub.tab = tab;
        if tab == HubTab::Settings {
            self.hub.settings.request_refresh();
        }
        if tab == HubTab::Shelf {
            self.refresh_shelf();
        }
        if tab == HubTab::Remote {
            self.remote_request_refresh();
        }
    }

    pub(crate) fn cycle_hub_tab(&mut self, forward: bool) {
        let tab = self.hub.tab.next(forward);
        self.set_hub_tab(tab);
    }

    /// Scroll the Keys tab, clamped to its rendered line count: one header per
    /// context plus one line per shortcut.
    pub(crate) fn move_keys_scroll(&mut self, forward: bool) {
        let total = crate::tui::keymap_reference::KEY_REFS.len()
            + crate::tui::keymap_reference::CONTEXTS.len();
        let current = self.hub.keys_scroll;
        self.hub.keys_scroll = if forward {
            (current + 1).min(total.saturating_sub(1))
        } else {
            current.saturating_sub(1)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_tab_digits_map_to_the_five_tabs() {
        assert_eq!(HubTab::from_digit('1'), Some(HubTab::Settings));
        assert_eq!(HubTab::from_digit('2'), Some(HubTab::Keys));
        assert_eq!(HubTab::from_digit('3'), Some(HubTab::Missions));
        assert_eq!(HubTab::from_digit('4'), Some(HubTab::Shelf));
        assert_eq!(HubTab::from_digit('5'), Some(HubTab::Remote));
        assert_eq!(HubTab::from_digit('6'), None);
    }

    #[test]
    fn hub_tab_next_wraps_both_ways() {
        assert_eq!(HubTab::Settings.next(true), HubTab::Keys);
        assert_eq!(HubTab::Keys.next(true), HubTab::Missions);
        assert_eq!(HubTab::Missions.next(true), HubTab::Shelf);
        assert_eq!(HubTab::Shelf.next(true), HubTab::Remote);
        assert_eq!(HubTab::Remote.next(true), HubTab::Settings);
        assert_eq!(HubTab::Settings.next(false), HubTab::Remote);
        assert_eq!(HubTab::Remote.next(false), HubTab::Shelf);
        assert_eq!(HubTab::Shelf.next(false), HubTab::Missions);
    }
}
