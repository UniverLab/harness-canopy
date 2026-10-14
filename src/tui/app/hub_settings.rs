//! CT31: data, editors, planning and the async I/O for the hub's Settings
//! tab.
//!
//! Structure (groups, kinds, apply modes) comes from the compile-time
//! registry; live state (`effective`, `source`, `status`) comes from a
//! `config_list` daemon call cached here. Every write goes through the daemon
//! client on a background thread, so the UI never blocks and never writes
//! `config.toml` itself.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::types::App;
use crate::application::ports::StateRepository;
use crate::domain::settings::{find, Apply, Group, Kind};
use crate::tui::app::dialog::GraphActionMessage;

/// Minimum gap between two `config_list` fetches while the tab is visible.
pub(crate) const SETTINGS_REFRESH_EVERY: Duration = Duration::from_secs(5);

/// One setting as the Settings tab shows it, from a `config_list` row.
pub(crate) struct SettingRow {
    pub key: String,
    pub group: String,
    pub description: String,
    /// TOML-rendered, from `config_list`.
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
        "config_set",
        serde_json::json!({ "key": key, "value": value_text }),
    )
}

/// The plan for resetting `key` to its default.
pub(crate) fn plan_reset(key: &str) -> (&'static str, serde_json::Value) {
    ("config_reset", serde_json::json!({ "key": key }))
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

/// Parse the `config_list` text payload (a JSON array of rows).
pub(crate) fn parse_settings_rows(text: &str) -> Result<Vec<SettingRow>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| format!("response is not JSON: {error}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "config_list: expected a JSON array".to_string())?;
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
/// `config_list` reports) back into a list.
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
    /// Request a fresh `config_list` as soon as possible.
    pub(crate) fn settings_request_refresh(&mut self) {
        self.hub.settings.request_refresh();
    }

    /// Spawn a `config_list` fetch unless the hub's Settings tab is not
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
        let port = self.settings_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = fetch_settings_rows(&port);
            let _ = tx.send(result);
        });
        self.hub.settings.rows_rx = Some(rx);
        self.hub.settings.last_fetch = Some(Instant::now());
        self.hub.settings.stale = false;
    }

    /// Apply a finished `config_list` fetch, if any.
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
        let port = self.settings_port();
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

    fn settings_port(&self) -> String {
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

    /// Drop the status line once its TTL has passed.
    pub(crate) fn dismiss_settings_message(&mut self) {
        let elapsed = self
            .hub
            .settings
            .message_at
            .map(|at| at.elapsed())
            .unwrap_or(Duration::MAX);
        if elapsed >= crate::tui::app::dialog::GRAPH_ACTION_MESSAGE_TTL {
            self.hub.settings.message = None;
        }
    }

    /// The hub's group labels, in presentation order.
    pub(crate) fn settings_group_labels() -> Vec<&'static str> {
        Group::ALL.iter().map(|group| group.label()).collect()
    }

    /// The selected group's visible rows as `(flat row index)` pairs wrapped
    /// as `(group_index, row_index)` — filtered by `filter`.
    pub(crate) fn settings_visible_rows(&self) -> Vec<(usize, usize)> {
        let labels = Self::settings_group_labels();
        let group_idx = self.hub.settings.group_idx.min(labels.len() - 1);
        let group_label = labels[group_idx];
        let filter = self.hub.settings.filter.to_lowercase();
        self.hub
            .settings
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.group == group_label)
            .filter(|(_, row)| {
                filter.is_empty()
                    || row.key.to_lowercase().contains(&filter)
                    || row.description.to_lowercase().contains(&filter)
            })
            .map(|(index, _)| (group_idx, index))
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

    /// Space on a bool row: flip it through `config_set`.
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

    /// Enter in the editor: write the pending value through `config_set`.
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
    let outcome =
        crate::tui::mcp_client::call_daemon_tool(port, "config_list", &serde_json::json!({}))
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

    /// Each editor kind plans the right `config_set` call and value: a bool
    /// sends `true`, an enum its option, an int `5`, a float `0.5`, a
    /// duration `30`, a string its text, and a PathList the TOML array.
    #[test]
    fn editors_plan_the_right_config_set_value() {
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
            assert_eq!(tool, "config_set");
            assert_eq!(args["key"], key);
            assert_eq!(args["value"], value);
        }
    }

    #[test]
    fn bool_toggles_and_enum_cycles() {
        let bool_row = row("tui.atmosphere", "TUI & appearance", "true");
        let (tool, args) = bool_toggle_plan(&bool_row).expect("bool plan");
        assert_eq!(tool, "config_set");
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
    fn reset_plans_config_reset() {
        let (tool, args) = plan_reset("theme");
        assert_eq!(tool, "config_reset");
        assert_eq!(args["key"], "theme");
        assert!(args.get("value").is_none());
    }

    #[test]
    fn slash_filter_narrows_rows() {
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
        ];

        app.hub.settings.filter.clear();
        assert_eq!(app.settings_visible_rows().len(), 3);

        // "theme" matches one key and one description.
        app.hub.settings.filter = "theme".to_string();
        let keys: Vec<&str> = app
            .settings_visible_rows()
            .iter()
            .map(|(_, flat)| app.hub.settings.rows[*flat].key.as_str())
            .collect();
        assert_eq!(keys, vec!["theme", "tui.brians_brain"]);

        // Case-insensitive, and matching by key alone.
        app.hub.settings.filter = "SPEC_ATTEMPT".to_string();
        let keys: Vec<&str> = app
            .settings_visible_rows()
            .iter()
            .map(|(_, flat)| app.hub.settings.rows[*flat].key.as_str())
            .collect();
        assert_eq!(keys, vec!["spec_attempt_limit"]);

        // An empty filter restores everything.
        app.hub.settings.filter.clear();
        assert_eq!(app.settings_visible_rows().len(), 3);
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
}
