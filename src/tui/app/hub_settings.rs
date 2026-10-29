//! CT31: data, editors, planning and the async I/O for the hub's Settings
//! tab.
//!
//! Structure (groups, kinds, apply modes) comes from the compile-time
//! registry; live state (`effective`, `source`, `status`) comes from a
//! `config list` daemon call cached here. Every write goes through the daemon
//! client on a background thread, so the UI never blocks and never writes
//! `config.toml` itself.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::types::App;
use crate::application::ports::StateRepository;
use crate::domain::settings::{find, Apply, Group, Kind};
use crate::tui::app::dialog::GraphActionMessage;

/// Minimum gap between two `config list` fetches while the tab is visible.
pub(crate) const SETTINGS_REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Which Settings column holds the focus: the category list or its
/// properties. ←/→ move between the two levels; ↑↓ move within one.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsFocus {
    #[default]
    Categories,
    Properties,
}

/// One setting as the Settings tab shows it, from a `config list` row.
pub(crate) struct SettingRow {
    pub key: String,
    pub group: String,
    pub description: String,
    /// TOML-rendered, from `config list`.
    pub effective: String,
    pub default: String,
    /// "default" | "file" | "env"
    pub source: String,
    /// "ok" | "invalid: …" | "pending_restart" | "unknown"
    pub status: String,
}

/// The Settings tab's state.
#[derive(Default)]
pub(crate) struct SettingsState {
    pub rows: Vec<SettingRow>,
    pub group_idx: usize,
    pub row_idx: usize,
    /// The category list or the property list (requirement 3).
    pub focus: SettingsFocus,
    pub filter: String,
    pub filter_mode: bool,
    pub editor: Option<SettingsEditor>,
    pub dir_picker: Option<HubDirPicker>,
    pub rows_rx: Option<mpsc::Receiver<Result<Vec<SettingRow>, String>>>,
    /// One config write in flight: `(is_error, text)` — `text` is the daemon's
    /// `action` instruction on success, or its refusal message on failure.
    pub write_rx: Option<mpsc::Receiver<(bool, String)>>,
    pub last_fetch: Option<Instant>,
    pub stale: bool,
    pub message: Option<GraphActionMessage>,
    pub message_at: Option<Instant>,
}

impl SettingsState {
    /// Request a refresh as soon as possible (clears the throttle).
    pub(crate) fn request_refresh(&mut self) {
        self.stale = true;
        self.last_fetch = None;
    }
}

/// The inline editor open over the Settings tab.
pub(crate) enum SettingsEditor {
    /// Int / Float / Duration / String / ChronoFormat: inline text input.
    Text { key: String, buffer: String },
    /// PathList: list editor overlay.
    PathList {
        key: String,
        list: Vec<String>,
        idx: usize,
    },
}

/// One directory-browser step for a PathList add.
pub(crate) struct HubDirPicker {
    pub current: PathBuf,
    pub entries: Vec<String>,
    pub idx: usize,
}

/// The plan for setting `key` to `value_text`: the tool name and its args.
pub(crate) fn plan_set(key: &str, value_text: &str) -> (&'static str, serde_json::Value) {
    (
        "config",
        serde_json::json!({ "action": "set", "key": key, "value": value_text }),
    )
}

/// The plan for resetting `key` to its default.
pub(crate) fn plan_reset(key: &str) -> (&'static str, serde_json::Value) {
    (
        "config",
        serde_json::json!({ "action": "reset", "key": key }),
    )
}

/// Render a list of paths as a TOML array, escaping `\` and `"`.
pub(crate) fn toml_array(paths: &[String]) -> String {
    let escaped: Vec<String> = paths
        .iter()
        .map(|path| format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", escaped.join(","))
}

/// The value as shown, stripping one layer of TOML string quotes when present.
pub(crate) fn display_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Whether the effective value equals the default (the default is then dimmed).
pub(crate) fn effective_eq_default(row: &SettingRow) -> bool {
    row.effective == row.default
}

/// The apply-mode badge for a registry `Apply`.
pub(crate) fn settings_badge_for(apply: Apply) -> &'static str {
    apply.badge()
}

/// Parse the `config list` text payload (a JSON array of rows).
pub(crate) fn parse_settings_rows(text: &str) -> Result<Vec<SettingRow>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("response is not JSON: {error}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "config list: expected a JSON array".to_string())?;
    Ok(items.iter().map(parse_setting_row).collect())
}

fn parse_setting_row(item: &serde_json::Value) -> SettingRow {
    let field = |name: &str| {
        item.get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    SettingRow {
        key: field("key"),
        group: field("group"),
        description: field("description"),
        effective: field("effective"),
        default: field("default"),
        source: field("source"),
        status: field("status"),
    }
}

/// Parse a TOML array of strings (the form `toml_array` writes and
/// `config list` reports) back into a list.
pub(crate) fn parse_toml_array(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    inner
        .split(',')
        .map(|entry| display_value(entry.trim()))
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The write plan for toggling a bool row (Space). `None` when the row's
/// effective value is not a bool.
pub(crate) fn bool_toggle_plan(row: &SettingRow) -> Option<(&'static str, serde_json::Value)> {
    let current = display_value(&row.effective);
    let next = match current.as_str() {
        "true" => "false",
        "false" => "true",
        _ => return None,
    };
    Some(plan_set(&row.key, next))
}

/// The write plan for cycling an enum row (Left/Right). `None` when the row's
/// key is not an enum.
pub(crate) fn enum_cycle_plan(
    row: &SettingRow,
    forward: bool,
) -> Option<(&'static str, serde_json::Value)> {
    let def = find(&row.key)?;
    let Kind::Enum { options } = def.kind else {
        return None;
    };
    if options.is_empty() {
        return None;
    }
    let current = display_value(&row.effective);
    let index = options
        .iter()
        .position(|option| *option == current)
        .unwrap_or(0);
    let len = options.len();
    let next = if forward {
        (index + 1) % len
    } else {
        (index + len - 1) % len
    };
    Some(plan_set(&row.key, options[next]))
}

/// Insert `path` at `idx`, moving the cursor to it.
pub(crate) fn list_insert(list: &mut Vec<String>, idx: &mut usize, path: String) {
    let at = (*idx).min(list.len());
    list.insert(at, path);
    *idx = at;
}

/// Delete the entry at `idx`, clamping the cursor.
pub(crate) fn list_delete(list: &mut Vec<String>, idx: &mut usize) {
    if *idx >= list.len() {
        return;
    }
    list.remove(*idx);
    *idx = (*idx).min(list.len().saturating_sub(1));
}

/// Move the entry at `idx` one step, keeping the cursor on it.
pub(crate) fn list_move(list: &mut [String], idx: &mut usize, forward: bool) {
    if list.len() < 2 {
        return;
    }
    let target = if forward {
        (*idx + 1) % list.len()
    } else {
        (*idx + list.len() - 1) % list.len()
    };
    list.swap(*idx, target);
    *idx = target;
}

impl App {
    /// Request a fresh `config list` as soon as possible.
    pub(crate) fn settings_request_refresh(&mut self) {
        self.hub.settings.request_refresh();
    }

    /// Spawn a `config list` fetch unless the hub's Settings tab is not
    /// visible, a fetch is already in flight, or the throttle still holds.
    pub(crate) fn refresh_settings_rows(&mut self) {
        if !self.hub_open || self.hub.tab != super::hub::HubTab::Settings {
            return;
        }
        if self.hub.settings.rows_rx.is_some() {
            return;
        }
        if !self.hub.settings.stale
            && self
                .hub
                .settings
                .last_fetch
                .is_some_and(|at| at.elapsed() < SETTINGS_REFRESH_EVERY)
        {
            return;
        }
        let port = self.daemon_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = fetch_settings_rows(&port);
            let _ = tx.send(result);
        });
        self.hub.settings.rows_rx = Some(rx);
        self.hub.settings.last_fetch = Some(Instant::now());
        self.hub.settings.stale = false;
    }

    /// Apply a finished `config list` fetch, if any.
    pub(crate) fn poll_settings_rows(&mut self) {
        let Some(rx) = self.hub.settings.rows_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(rows)) => {
                self.hub.settings.rows = rows;
                self.clamp_settings_selection();
            }
            Ok(Err(text)) => {
                self.set_settings_message(true, format!("Settings unavailable: {text}"));
            }
            Err(mpsc::TryRecvError::Empty) => self.hub.settings.rows_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Apply a finished settings write, if any.
    pub(crate) fn poll_settings_write(&mut self) {
        let Some(rx) = self.hub.settings.write_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok((is_error, text)) => self.apply_write_result(is_error, text),
            Err(mpsc::TryRecvError::Empty) => self.hub.settings.write_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Run one config write on a background thread. No-op while a previous
    /// write is still in flight.
    pub(crate) fn spawn_config_write(&mut self, tool: &'static str, args: serde_json::Value) {
        if self.hub.settings.write_rx.is_some() {
            self.set_settings_message(true, "A settings write is still running");
            return;
        }
        let port = self.daemon_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let message = match crate::tui::mcp_client::call_daemon_tool(&port, tool, &args) {
                Ok(outcome) => (outcome.is_error, outcome.text),
                Err(error) => (true, format!("{error:#}")),
            };
            let _ = tx.send(message);
        });
        self.hub.settings.write_rx = Some(rx);
    }

    /// Apply one finished write to the status line. A refusal leaves `rows`
    /// untouched, so the old effective value stays on screen; a success asks
    /// for a fresh snapshot.
    pub(crate) fn apply_write_result(&mut self, is_error: bool, text: String) {
        self.set_settings_message(is_error, text);
        if !is_error {
            self.settings_request_refresh();
        }
    }

    pub(crate) fn daemon_port(&self) -> String {
        self.db
            .get_state("port")
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::resolve_port(None).to_string())
    }

    pub(crate) fn set_settings_message(&mut self, is_error: bool, text: impl Into<String>) {
        self.hub.settings.message = Some(GraphActionMessage {
            is_error,
            text: text.into(),
        });
        self.hub.settings.message_at = Some(Instant::now());
    }

    /// Drop the status line once its TTL has passed. True when it cleared
    /// this tick (a visible change).
    pub(crate) fn dismiss_settings_message(&mut self) -> bool {
        if self.hub.settings.message.is_none() {
            return false;
        }
        let elapsed = self
            .hub
            .settings
            .message_at
            .map(|at| at.elapsed())
            .unwrap_or(Duration::MAX);
        if elapsed >= crate::tui::app::dialog::GRAPH_ACTION_MESSAGE_TTL {
            self.hub.settings.message = None;
            return true;
        }
        false
    }

    /// The hub's group labels, in presentation order.
    pub(crate) fn settings_group_labels() -> Vec<&'static str> {
        Group::ALL.iter().map(|group| group.label()).collect()
    }

    /// The rows the Settings tab shows, as `(group_index, row_index)` pairs
    /// into `rows`. With no filter that is the selected group's rows; a filter
    /// searches every group by key or description (spec: `/` filters every
    /// group).
    pub(crate) fn settings_visible_rows(&self) -> Vec<(usize, usize)> {
        let labels = Self::settings_group_labels();
        let group_idx = self.hub.settings.group_idx.min(labels.len() - 1);
        let group_label = labels[group_idx];
        let filter = self.hub.settings.filter.to_lowercase();
        let filtering = !filter.is_empty();
        self.hub
            .settings
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| filtering || row.group == group_label)
            .filter(|(_, row)| {
                !filtering
                    || row.key.to_lowercase().contains(&filter)
                    || row.description.to_lowercase().contains(&filter)
            })
            .map(|(index, row)| {
                let owner = labels
                    .iter()
                    .position(|label| *label == row.group)
                    .unwrap_or(group_idx);
                (owner, index)
            })
            .collect()
    }

    /// The SettingRow under the cursor, if any.
    pub(crate) fn selected_setting(&self) -> Option<&SettingRow> {
        let visible = self.settings_visible_rows();
        let (_, index) = visible.get(self.hub.settings.row_idx)?;
        self.hub.settings.rows.get(*index)
    }

    pub(crate) fn clamp_settings_selection(&mut self) {
        let labels = Self::settings_group_labels();
        self.hub.settings.group_idx = self.hub.settings.group_idx.min(labels.len() - 1);
        let count = self.settings_visible_rows().len();
        self.hub.settings.row_idx = if count == 0 {
            0
        } else {
            self.hub.settings.row_idx.min(count - 1)
        };
    }

    pub(crate) fn settings_move_group(&mut self, forward: bool) {
        let len = Self::settings_group_labels().len();
        self.hub.settings.group_idx =
            crate::tui::selection::move_index(self.hub.settings.group_idx, len, forward);
        self.hub.settings.row_idx = 0;
    }

    pub(crate) fn settings_move_row(&mut self, forward: bool) {
        let count = self.settings_visible_rows().len();
        if count == 0 {
            self.hub.settings.row_idx = 0;
            return;
        }
        self.hub.settings.row_idx =
            crate::tui::selection::move_index(self.hub.settings.row_idx, count, forward);
    }

    /// `→`/Enter at the categories level: focus the properties with the first
    /// property selected (requirement 3).
    pub(crate) fn settings_enter_properties(&mut self) {
        self.hub.settings.focus = SettingsFocus::Properties;
        self.hub.settings.row_idx = 0;
        self.clamp_settings_selection();
    }

    /// `←`/Esc at the properties level: back to the categories, keeping the
    /// selected category.
    pub(crate) fn settings_back_to_categories(&mut self) {
        self.hub.settings.focus = SettingsFocus::Categories;
        self.hub.settings.row_idx = 0;
    }

    /// Enter/Space on the focused property: a bool toggles, an enum cycles
    /// forward, the editing kinds open their editor, a table does nothing.
    /// ←/→ never reach here, so they never change a value.
    pub(crate) fn settings_activate_property(&mut self) {
        let Some(kind) = self
            .selected_setting()
            .and_then(|row| find(&row.key))
            .map(|def| def.kind)
        else {
            return;
        };
        match kind {
            Kind::Bool => self.toggle_bool(),
            Kind::Enum { .. } => self.cycle_enum(true),
            Kind::Structured => {}
            Kind::Int { .. }
            | Kind::Float { .. }
            | Kind::Duration { .. }
            | Kind::String
            | Kind::ChronoFormat
            | Kind::PathList => self.open_setting_editor(),
        }
    }

    /// Space on a bool row: flip it through `config set`.
    pub(crate) fn toggle_bool(&mut self) {
        let Some(plan) = self.selected_setting().and_then(bool_toggle_plan) else {
            return;
        };
        self.spawn_config_write(plan.0, plan.1);
    }

    /// Left/Right on an enum row: cycle through its options.
    pub(crate) fn cycle_enum(&mut self, forward: bool) {
        let Some(plan) = self
            .selected_setting()
            .and_then(|row| enum_cycle_plan(row, forward))
        else {
            return;
        };
        self.spawn_config_write(plan.0, plan.1);
    }

    /// Enter on an editable row: open the right editor for its kind.
    pub(crate) fn open_setting_editor(&mut self) {
        let Some(row) = self.selected_setting() else {
            return;
        };
        let key = row.key.clone();
        let current = display_value(&row.effective);
        let Some(def) = find(&key) else {
            return;
        };
        self.hub.settings.editor = Some(match def.kind {
            Kind::PathList => SettingsEditor::PathList {
                key,
                list: parse_toml_array(&row.effective),
                idx: 0,
            },
            Kind::Int { .. }
            | Kind::Float { .. }
            | Kind::Duration { .. }
            | Kind::String
            | Kind::ChronoFormat => SettingsEditor::Text {
                key,
                buffer: current,
            },
            Kind::Bool | Kind::Enum { .. } | Kind::Structured => return,
        });
    }

    /// Enter in the editor: write the pending value through `config set`.
    pub(crate) fn commit_editor_value(&mut self) {
        let Some(editor) = self.hub.settings.editor.take() else {
            return;
        };
        let (tool, args) = match &editor {
            SettingsEditor::Text { key, buffer } => plan_set(key, buffer),
            SettingsEditor::PathList { key, list, .. } => plan_set(key, &toml_array(list)),
        };
        self.spawn_config_write(tool, args);
    }

    pub(crate) fn cancel_editor(&mut self) {
        self.hub.settings.editor = None;
        self.hub.settings.dir_picker = None;
    }

    /// `r` on a row: reset it to its registry default.
    pub(crate) fn reset_selected_setting(&mut self) {
        let Some(key) = self.selected_setting().map(|row| row.key.clone()) else {
            return;
        };
        let (tool, args) = plan_reset(&key);
        self.spawn_config_write(tool, args);
    }

    /// Open the directory picker for a PathList add, starting at home.
    pub(crate) fn path_list_open_dir_picker(&mut self) {
        let start = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        self.hub.settings.dir_picker = Some(dir_picker_at(start));
    }

    /// `d` in the PathList editor: drop the highlighted entry.
    pub(crate) fn path_list_delete(&mut self) {
        if let Some(SettingsEditor::PathList { list, idx, .. }) = self.hub.settings.editor.as_mut()
        {
            list_delete(list, idx);
        }
    }

    /// Shift+↑/↓ in the PathList editor: reorder the highlighted entry.
    pub(crate) fn path_list_move(&mut self, forward: bool) {
        if let Some(SettingsEditor::PathList { list, idx, .. }) = self.hub.settings.editor.as_mut()
        {
            list_move(list, idx, forward);
        }
    }

    /// ↑/↓ in the PathList editor: move the highlight over the list.
    pub(crate) fn path_list_cursor(&mut self, forward: bool) {
        if let Some(SettingsEditor::PathList { list, idx, .. }) = self.hub.settings.editor.as_mut()
        {
            if list.is_empty() {
                return;
            }
            *idx = crate::tui::selection::move_index(*idx, list.len(), forward);
        }
    }

    /// `a` in the PathList editor: open the directory picker to add one.
    pub(crate) fn path_list_add(&mut self) {
        self.path_list_open_dir_picker();
    }
}

/// Build a picker step for `current`, listing its non-hidden subdirectories.
pub(crate) fn dir_picker_at(current: PathBuf) -> HubDirPicker {
    let entries = read_dir_names(&current);
    HubDirPicker {
        current,
        entries,
        idx: 0,
    }
}

fn read_dir_names(path: &std::path::Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let entries: Vec<_> = read.filter_map(|entry| entry.ok()).collect();
    let mut names: Vec<String> =
        crate::tui::app::dialog::new_agent::dir_browser::collect_dir_names(&entries)
            .into_iter()
            .map(|name| name.trim_end_matches('/').to_string())
            .collect();
    names.sort();
    names
}

/// Fetch and parse every setting row through the daemon.
fn fetch_settings_rows(port: &str) -> Result<Vec<SettingRow>, String> {
    let outcome = crate::tui::mcp_client::call_daemon_tool(
        port,
        "config",
        &serde_json::json!({ "action": "list" }),
    )
    .map_err(|error| format!("{error:#}"))?;
    if outcome.is_error {
        return Err(outcome.text);
    }
    parse_settings_rows(&outcome.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;

    fn test_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn row(key: &str, group: &str, effective: &str) -> SettingRow {
        SettingRow {
            key: key.to_string(),
            group: group.to_string(),
            description: format!("{key} description"),
            effective: effective.to_string(),
            default: "\"classic\"".to_string(),
            source: "file".to_string(),
            status: "ok".to_string(),
        }
    }

    /// Each editor kind plans the right `config set` call and value: a bool
    /// sends `true`, an enum its option, an int `5`, a float `0.5`, a
    /// duration `30`, a string its text, and a PathList the TOML array.
    #[test]
    fn editors_plan_the_right_config_set_action_value() {
        let cases = [
            ("tui.brians_brain", "true"),
            ("theme", "modern"),
            ("spec_attempt_limit", "5"),
            ("similarity_threshold", "0.5"),
            ("clean.retention_days", "30"),
            ("tui.datetime_format", "%d/%m/%Y"),
            ("rag_personal_dirs", "[\"/a\",\"/b\"]"),
        ];
        for (key, value) in cases {
            let (tool, args) = plan_set(key, value);
            assert_eq!(tool, "config");
            assert_eq!(args["action"], "set");
            assert_eq!(args["key"], key);
            assert_eq!(args["value"], value);
        }
    }

    #[test]
    fn bool_toggles_and_enum_cycles() {
        let bool_row = row("tui.atmosphere", "TUI & appearance", "true");
        let (tool, args) = bool_toggle_plan(&bool_row).expect("bool plan");
        assert_eq!(tool, "config");
        assert_eq!(args["action"], "set");
        assert_eq!(args["value"], "false");

        let enum_row = row("theme", "General", "\"classic\"");
        let (_, args) = enum_cycle_plan(&enum_row, true).expect("enum plan");
        assert_eq!(args["value"], "modern");
        // Wrapping forward past the last option returns to the first.
        let (_, args) = enum_cycle_plan(&enum_row, false).expect("enum plan");
        assert_eq!(args["value"], "modern");
        let enum_wrap = row("theme", "General", "\"modern\"");
        let (_, args) = enum_cycle_plan(&enum_wrap, true).expect("enum plan");
        assert_eq!(args["value"], "classic");
    }

    /// A refusal must not optimistically mutate the row: the old effective
    /// value stays and the daemon's message is shown as an error.
    #[test]
    fn refusal_keeps_the_old_effective_value() {
        let mut app = test_app();
        app.hub.settings.rows = vec![row("theme", "General", "\"classic\"")];
        app.apply_write_result(true, "theme: \"neon\" is not valid".into());

        assert_eq!(app.hub.settings.rows[0].effective, "\"classic\"");
        let message = app.hub.settings.message.as_ref().expect("message");
        assert!(message.is_error);
        assert_eq!(message.text, "theme: \"neon\" is not valid");
    }

    #[test]
    fn reset_plans_config_reset_action() {
        let (tool, args) = plan_reset("theme");
        assert_eq!(tool, "config");
        assert_eq!(args["action"], "reset");
        assert_eq!(args["key"], "theme");
        assert!(args.get("value").is_none());
    }

    fn visible_keys(app: &App) -> Vec<&str> {
        app.settings_visible_rows()
            .iter()
            .map(|(_, flat)| app.hub.settings.rows[*flat].key.as_str())
            .collect()
    }

    /// With no filter only the selected group shows; a filter searches every
    /// group by key or description.
    #[test]
    fn slash_filter_searches_every_group() {
        let mut app = test_app();
        app.hub.settings.rows = vec![
            // Key match.
            row("theme", "General", "\"classic\""),
            // Description-only match.
            SettingRow {
                description: "the tui theme knobs".to_string(),
                ..row("tui.brians_brain", "General", "true")
            },
            // No match at all.
            row("spec_attempt_limit", "General", "5"),
            // A match in another group.
            row("notify.theme_sound", "Notifications", "true"),
        ];

        app.hub.settings.filter.clear();
        assert_eq!(visible_keys(&app).len(), 3, "only General without a filter");

        // "theme" matches a key and a description in General, and a key in
        // Notifications.
        app.hub.settings.filter = "theme".to_string();
        assert_eq!(
            visible_keys(&app),
            vec!["theme", "tui.brians_brain", "notify.theme_sound"]
        );

        // Case-insensitive, and matching by key alone.
        app.hub.settings.filter = "SPEC_ATTEMPT".to_string();
        assert_eq!(visible_keys(&app), vec!["spec_attempt_limit"]);

        // An empty filter restores the selected group.
        app.hub.settings.filter.clear();
        assert_eq!(visible_keys(&app).len(), 3);
    }

    /// Spawn a fake daemon whose every tool call answers with `result`, and
    /// point the app at it.
    fn app_on_daemon(
        result: serde_json::Value,
    ) -> (App, crate::tui::mcp_client::test_support::FakeDaemon) {
        let fake = crate::tui::mcp_client::test_support::spawn_fake_daemon(result);
        let app = test_app();
        app.db.set_state("port", &fake.port).unwrap();
        (app, fake)
    }

    fn ok_result() -> serde_json::Value {
        serde_json::json!({ "content": [{ "type": "text", "text": "{}" }], "isError": false })
    }

    /// Wait for the background write to finish and apply its result.
    fn finish_write(app: &mut App) {
        for _ in 0..1000 {
            if app.hub.settings.write_rx.is_none() {
                return;
            }
            app.poll_settings_write();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("settings write did not finish");
    }

    #[test]
    fn enter_on_an_int_row_calls_config_set_action_with_the_typed_value() {
        let (mut app, fake) = app_on_daemon(ok_result());
        app.hub.settings.rows = vec![row("spec_attempt_limit", "General", "5")];

        app.open_setting_editor();
        if let Some(SettingsEditor::Text { buffer, .. }) = app.hub.settings.editor.as_mut() {
            *buffer = "7".to_string();
        }
        app.commit_editor_value();
        finish_write(&mut app);

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(calls[0]["arguments"]["action"], "set");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "set", "key": "spec_attempt_limit", "value": "7" })
        );
    }

    #[test]
    fn space_on_a_bool_row_calls_config_set_action_with_the_flip() {
        let (mut app, fake) = app_on_daemon(ok_result());
        app.hub.settings.rows = vec![row("tui.atmosphere", "General", "true")];

        app.toggle_bool();
        finish_write(&mut app);

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(calls[0]["arguments"]["action"], "set");
        assert_eq!(calls[0]["arguments"]["value"], "false");
    }

    #[test]
    fn arrows_on_an_enum_row_call_config_set_action_with_the_next_option() {
        let (mut app, fake) = app_on_daemon(ok_result());
        app.hub.settings.rows = vec![row("theme", "General", "\"classic\"")];

        app.cycle_enum(true);
        finish_write(&mut app);

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(calls[0]["arguments"]["action"], "set");
        assert_eq!(calls[0]["arguments"]["value"], "modern");
    }

    #[test]
    fn path_list_commit_calls_config_set_action_with_the_toml_array() {
        let (mut app, fake) = app_on_daemon(ok_result());
        app.hub.settings.rows = vec![row("rag_personal_dirs", "General", "[\"/a\",\"/b\"]")];

        app.open_setting_editor();
        app.path_list_delete();
        app.commit_editor_value();
        finish_write(&mut app);

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(calls[0]["arguments"]["action"], "set");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "set", "key": "rag_personal_dirs", "value": "[\"/b\"]" })
        );
    }

    #[test]
    fn r_calls_config_reset_action_for_the_selected_row() {
        let (mut app, fake) = app_on_daemon(ok_result());
        app.hub.settings.rows = vec![row("theme", "General", "\"modern\"")];

        app.reset_selected_setting();
        finish_write(&mut app);

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "reset", "key": "theme" })
        );
    }

    /// A range refusal from the daemon leaves the old value on screen and
    /// shows the daemon's message as an error.
    #[test]
    fn out_of_range_refusal_keeps_the_old_value() {
        let (mut app, _fake) = app_on_daemon(serde_json::json!({
            "content": [{ "type": "text", "text": "spec_attempt_limit: 99 is outside 1..=20" }],
            "isError": true
        }));
        app.hub.settings.rows = vec![row("spec_attempt_limit", "General", "5")];

        app.open_setting_editor();
        if let Some(SettingsEditor::Text { buffer, .. }) = app.hub.settings.editor.as_mut() {
            *buffer = "99".to_string();
        }
        app.commit_editor_value();
        finish_write(&mut app);

        assert_eq!(app.hub.settings.rows[0].effective, "5");
        let message = app.hub.settings.message.as_ref().expect("message");
        assert!(message.is_error);
        assert_eq!(message.text, "spec_attempt_limit: 99 is outside 1..=20");
    }

    #[test]
    fn path_list_add_delete_reorder() {
        let mut list = vec!["/a".to_string(), "/b".to_string()];
        let mut idx = 0;
        list_insert(&mut list, &mut idx, "/c".to_string());
        assert_eq!(list, vec!["/c", "/a", "/b"]);
        assert_eq!(idx, 0);

        list_move(&mut list, &mut idx, true);
        assert_eq!(list, vec!["/a", "/c", "/b"]);
        assert_eq!(idx, 1);

        list_delete(&mut list, &mut idx);
        assert_eq!(list, vec!["/a", "/b"]);
        assert_eq!(idx, 1);
        // Cursor clamps after deleting the last element.
        idx = 1;
        list_delete(&mut list, &mut idx);
        assert_eq!(list, vec!["/a"]);
        assert_eq!(idx, 0);

        assert_eq!(toml_array(&list), "[\"/a\"]");
        assert_eq!(
            toml_array(&["/a\"b".to_string(), "/c\\d".to_string()]),
            "[\"/a\\\"b\",\"/c\\\\d\"]"
        );
    }

    #[test]
    fn parse_toml_array_round_trips() {
        assert_eq!(
            parse_toml_array(&toml_array(&["/a".to_string(), "/b".to_string()])),
            vec!["/a", "/b"]
        );
    }

    /// The TTL dismissal reports whether it cleared: no message or a fresh
    /// one returns false, an expired one clears and returns true so the tick
    /// redraws without it.
    #[test]
    fn dismiss_settings_message_reports_whether_it_cleared() {
        let mut app = test_app();
        assert!(!app.dismiss_settings_message(), "no message must not clear");

        app.set_settings_message(false, "saved");
        assert!(
            !app.dismiss_settings_message(),
            "a fresh message must survive"
        );

        app.hub.settings.message_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        assert!(
            app.dismiss_settings_message(),
            "an expired message must clear"
        );
        assert!(app.hub.settings.message.is_none());
        assert!(
            !app.dismiss_settings_message(),
            "a cleared message must not clear again"
        );
    }
}
