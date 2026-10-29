//! CM51: the `config_*` MCP tools — read and write `~/.canopy/config.toml`
//! through the settings registry.
//!
//! Every write goes through `settings::writer` (the one writer) after
//! validating against the setting's `SettingDef`, and publishes a
//! `settings_changed` revision so a running TUI re-reads what it renders.

use std::path::Path;

use anyhow::{bail, Result};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use serde_json::json;

use crate::daemon::config_watch;
use crate::daemon::cpu_quota;
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::build_json_result;
use crate::daemon::helpers::{data_dir, error_result};
use crate::daemon::params::*;
use crate::daemon::process::RealRunner;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::settings::{self, inspect, Kind};

impl TaskTriggerHandler {
    pub(crate) async fn config_list_impl(
        &self,
        params: ConfigListParams,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        let rows = inspect::list(&dir, params.group.as_deref());
        to_json(&rows)
    }

    pub(crate) async fn config_get_impl(
        &self,
        params: ConfigGetParams,
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

    pub(crate) async fn config_set_impl(
        &self,
        params: ConfigSetParams,
    ) -> Result<CallToolResult, McpError> {
        let dir = data_dir()?;
        match apply_set(&self.db, &dir, &params.key, Some(&params.value)) {
            Ok(result) => to_json(&result),
            Err(error) => Ok(error_result(&error.to_string())),
        }
    }

    pub(crate) async fn config_reset_impl(
        &self,
        params: ConfigResetParams,
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
    let mut result = json!({
        "key": key,
        "value": value.to_string(),
        "effective": inspect::config_value(&effective, key).to_string(),
        "action": settings::action_for(def.apply),
    });

    // FR2: the quota applies to the running daemon without a restart. The
    // value is already written and published above, so a failed live apply
    // keeps it written and only fails the response, naming the cause.
    if key == cpu_quota::CONFIG_KEY {
        let quota = u32::try_from(value.as_integer().unwrap_or(0)).unwrap_or(0);
        cpu_quota::apply_with(&RealRunner, quota)
            .map_err(|cause| anyhow::anyhow!("quota saved but live apply failed: {cause}"))?;
        result["applied_live"] = json!(true);
        result["note"] = json!(
            "CPU quota applied live via systemctl set-property (runtime and persistent); no daemon restart needed"
        );
    }

    Ok(result)
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

    /// Restores `PATH` when dropped, so the fake `systemctl` below never
    /// leaks into another test.
    struct RestorePath {
        old: Option<std::ffi::OsString>,
    }

    impl Drop for RestorePath {
        fn drop(&mut self) {
            match &self.old {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    /// FR2 end to end through the real runner: `config set` on the quota
    /// key applies live via `systemctl set-property` and reports it, and a
    /// failed apply keeps the written value while naming the cause. The
    /// only test that executes a `systemctl`, and it is a fake on a
    /// prepended `PATH` — the real unit and the running daemon are never
    /// touched.
    #[cfg(unix)]
    #[test]
    fn config_set_cpu_quota_applies_live_and_keeps_the_value_on_failure() {
        use std::os::unix::fs::PermissionsExt;

        let bin = TempDir::new().unwrap();
        let bin_path = bin.path().to_string_lossy().to_string();
        std::fs::write(
            bin.path().join("systemctl"),
            format!(
                "#!/bin/sh\necho \"$@\" >> '{bin_path}/calls.log'\nif [ -e '{bin_path}/fail' ]; then exit 1; fi\nexit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            bin.path().join("systemctl"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let old_path = std::env::var_os("PATH");
        let joined = match &old_path {
            Some(path) => format!("{bin_path}:{}", path.to_string_lossy()),
            None => bin_path,
        };
        std::env::set_var("PATH", joined);
        let _restore = RestorePath { old: old_path };

        let dir = TempDir::new().unwrap();
        let db = test_db(&dir);

        let result = apply_set(&db, dir.path(), "daemon.cpu_quota_percent", Some("600")).unwrap();
        assert_eq!(result["applied_live"], true);
        assert!(result["note"].as_str().unwrap().contains("set-property"));
        assert_eq!(result["effective"], "600");
        let calls = std::fs::read_to_string(bin.path().join("calls.log")).unwrap();
        assert!(
            calls.contains("--user set-property canopy.service CPUQuota=600%"),
            "fake systemctl saw: {calls}"
        );

        std::fs::write(bin.path().join("fail"), "yes").unwrap();
        let error = apply_set(&db, dir.path(), "daemon.cpu_quota_percent", Some("400"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("live apply failed"), "{error}");
        assert!(error.contains("set-property"), "{error}");
        assert_eq!(
            CanopyConfig::load(dir.path()).daemon.cpu_quota_percent,
            400,
            "the value stays written when the live apply fails"
        );
    }
}
