//! CM51: the `config_*` MCP tools — read and write `~/.canopy/config.toml`
//! through the settings registry.
//!
//! Every write goes through `settings::writer` (the one writer) after
//! validating against the setting's `SettingDef`, and publishes a
//! `settings_changed` revision so a running TUI re-reads what it renders.

use std::path::Path;

use anyhow::{bail, Result};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use serde_json::json;

use crate::daemon::config_watch;
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::build_json_result;
use crate::daemon::helpers::{data_dir, error_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::settings::{self, inspect, Kind};

#[tool_router(router = tool_router_settings, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "config_list",
        description = "List every canopy setting with its group, kind, default, effective value, where that value came from (default, file, or env), when it applies, and its status: ok, invalid: <reason>, pending_restart, or unknown (a hand-written key nothing declares; never deleted). Optional `group` filters to one group."
    )]
    pub(crate) async fn config_list(
        &self,
        Parameters(params): Parameters<ConfigListParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        let rows = inspect::list(&dir, params.group.as_deref());
        to_json(&rows)
    }

    #[tool(
        name = "config_get",
        description = "Read one canopy setting by its dotted key (e.g. `theme`, `clean.retention_days`), in the same shape config_list reports."
    )]
    pub(crate) async fn config_get(
        &self,
        Parameters(params): Parameters<ConfigGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        if settings::find(&params.key).is_none() {
            return Ok(error_result(&format!("unknown setting: {}", params.key)));
        }
        match inspect::list(&dir, None)
            .into_iter()
            .find(|row| row.key == params.key)
        {
            Some(row) => to_json(&row),
            None => Ok(error_result(&format!("unknown setting: {}", params.key))),
        }
    }

    #[tool(
        name = "config_set",
        description = "Change one canopy setting. The value is validated against the setting's type and range first, and nothing is written if it is refused. Returns the new effective value and what the user must do for it to take effect: applied, applies on next run, restart the daemon, or restart the TUI. A list-of-tables setting (skills.sources) must be edited by hand."
    )]
    pub(crate) async fn config_set(
        &self,
        Parameters(params): Parameters<ConfigSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        match apply_set(&self.db, &dir, &params.key, Some(&params.value)) {
            Ok(result) => to_json(&result),
            Err(error) => Ok(error_result(&error.to_string())),
        }
    }

    #[tool(
        name = "config_reset",
        description = "Reset one canopy setting to its registry default, written through the same validating path as config_set."
    )]
    pub(crate) async fn config_reset(
        &self,
        Parameters(params): Parameters<ConfigResetParams>,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        match apply_set(&self.db, &dir, &params.key, None) {
            Ok(result) => to_json(&result),
            Err(error) => Ok(error_result(&error.to_string())),
        }
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let value = serde_json::to_value(value).map_err(|error| {
        McpError::internal_error(format!("config: could not serialize: {error}"), None)
    })?;
    Ok(build_json_result(&value))
}

/// Validate, write and report one change. `raw` of `None` resets to the
/// registry default.
fn apply_set(
    db: &Database,
    canopy_dir: &Path,
    key: &str,
    raw: Option<&str>,
) -> Result<serde_json::Value> {
    let def = settings::find(key).ok_or_else(|| anyhow::anyhow!("unknown setting: {key}"))?;
    if matches!(def.kind, Kind::Structured) {
        bail!("{key} is a list of tables: edit config.toml by hand");
    }
    if key == "rag_personal_root" {
        bail!("rag_personal_root is legacy: use rag_personal_dirs");
    }

    let value = match raw {
        Some(raw) => settings::validate::parse_cli_value(def, raw)
            .map_err(|reason| anyhow::anyhow!(reason))?,
        None => def.default_value(),
    };
    settings::validate::validate_value(def, &value).map_err(|reason| anyhow::anyhow!(reason))?;

    // D8: an absent `pinned_panel_face` is automatic mode, so writing
    // "auto" (and resetting to it) removes the key instead of storing it.
    let stored = if key == "pinned_panel_face" && value.as_str() == Some("auto") {
        None
    } else {
        Some(value.clone())
    };
    settings::writer::write_keys(canopy_dir, &[(key, stored)])?;
    config_watch::publish_revision(db, &[key.to_string()]);

    let effective = CanopyConfig::load(canopy_dir);
    Ok(json!({
        "key": key,
        "value": value.to_string(),
        "effective": inspect::config_value(&effective, key).to_string(),
        "action": settings::action_for(def.apply),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db(dir: &TempDir) -> Database {
        Database::new(&dir.path().join("state.db")).unwrap()
    }

    #[test]
    fn config_set_applies_a_hot_setting_and_reports_it() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);
        let result = apply_set(&db, dir.path(), "theme", Some("modern")).unwrap();
        assert_eq!(result["action"], "applied");
        assert_eq!(result["effective"], "\"modern\"");
        assert_eq!(CanopyConfig::load(dir.path()).theme, "modern");
    }

    #[test]
    fn config_set_refuses_a_bad_value_without_writing() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);
        let error = apply_set(&db, dir.path(), "spec_attempt_limit", Some("0")).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("spec_attempt_limit"), "{message}");
        assert!(message.contains("1..=20"), "{message}");
        assert!(!dir.path().join("config.toml").exists());
    }

    #[test]
    fn config_set_reports_the_apply_mode_of_each_kind() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);
        assert_eq!(
            apply_set(&db, dir.path(), "spec_attempt_limit", Some("5")).unwrap()["action"],
            "restart the daemon"
        );
        assert_eq!(
            apply_set(&db, dir.path(), "clean.retention_days", Some("3")).unwrap()["action"],
            "applies on next run"
        );
        assert_eq!(
            apply_set(&db, dir.path(), "temperature_unit", Some("fahrenheit")).unwrap()["action"],
            "restart the TUI"
        );
    }

    #[test]
    fn config_set_refuses_unknown_structured_and_legacy_keys() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);
        assert!(apply_set(&db, dir.path(), "nope", Some("1"))
            .unwrap_err()
            .to_string()
            .contains("unknown setting: nope"));
        assert!(apply_set(&db, dir.path(), "skills.sources", Some("[]"))
            .unwrap_err()
            .to_string()
            .contains("edit config.toml by hand"));
        assert!(apply_set(&db, dir.path(), "rag_personal_root", Some("/x"))
            .unwrap_err()
            .to_string()
            .contains("legacy"));
    }

    #[test]
    fn config_reset_writes_the_registry_default_and_auto_removes_the_pin() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);
        apply_set(&db, dir.path(), "theme", Some("modern")).unwrap();
        let result = apply_set(&db, dir.path(), "theme", None).unwrap();
        assert_eq!(result["effective"], "\"classic\"");
        assert_eq!(CanopyConfig::load(dir.path()).theme, "classic");

        apply_set(&db, dir.path(), "pinned_panel_face", Some("knowledge")).unwrap();
        assert_eq!(
            CanopyConfig::load(dir.path()).pinned_panel_face.as_deref(),
            Some("knowledge")
        );
        apply_set(&db, dir.path(), "pinned_panel_face", None).unwrap();
        assert_eq!(CanopyConfig::load(dir.path()).pinned_panel_face, None);
    }
}
