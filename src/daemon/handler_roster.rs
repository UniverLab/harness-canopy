//! CM48: MCP tools for the availability roster.
//!
//! These four tools are the only write path (besides the CLI, which calls
//! them) to `roster_entries`. The engine never writes the table — it only
//! reads it at dispatch.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::roster_entries::{NewRosterEntry, RosterEntry, RosterState};
use crate::db::Database;
use crate::domain::roster::{effective_state, entry_expired, parse_until};
use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

fn entry_json(entry: &RosterEntry, now: &DateTime<Utc>) -> serde_json::Value {
    let expired = entry_expired(entry, now);
    let available = entry.state == RosterState::Available || expired;
    serde_json::json!({
        "id": entry.id,
        "platform": entry.platform,
        "model_pattern": entry.model_pattern,
        "state": entry.state.as_str(),
        "reason": entry.reason,
        "until": entry.until,
        "updated_at": entry.updated_at,
        "effective": { "available": available, "expired": expired },
    })
}

/// Validate one `harness roster_set` action and upsert it. Free function so it is
/// unit-testable with a bare `&Database` and an injected `now`.
pub(crate) fn roster_set_entry(
    db: &Database,
    params: &RosterSetParams,
    now: &DateTime<Utc>,
) -> anyhow::Result<CallToolResult> {
    let state = match RosterState::parse(&params.state) {
        Ok(state) => state,
        Err(e) => return Ok(error_result(&e.to_string())),
    };
    if let Some(pattern) = params.model_pattern.as_deref() {
        if let Some(prefix) = pattern.strip_suffix('*') {
            if prefix.is_empty() {
                return Ok(error_result(
                    "model_pattern '*' matches nothing; use a prefix like 'opencode-go/*'.",
                ));
            }
        }
    }
    if let Some(until) = params.until.as_deref() {
        if state != RosterState::Unavailable {
            return Ok(error_result("until is only valid with state=unavailable."));
        }
        match parse_until(until) {
            Some(when) if when > *now => {}
            Some(_) => return Ok(error_result("until must be in the future.")),
            None => {
                return Ok(error_result(
                    "until must be an RFC 3339 UTC timestamp, e.g. 2026-07-01T00:00:00Z.",
                ));
            }
        }
    }

    db.roster_upsert(NewRosterEntry {
        platform: &params.platform,
        model_pattern: params.model_pattern.as_deref(),
        state,
        reason: params.reason.as_deref(),
        until: params.until.as_deref(),
    })?;
    Ok(success_result(&format!(
        "Roster entry for '{}/{}' set to {}.",
        params.platform,
        params.model_pattern.as_deref().unwrap_or("(all)"),
        state.as_str()
    )))
}

impl TaskTriggerHandler {
    pub(crate) async fn harness_roster_set_impl(
        &self,
        params: RosterSetParams,
    ) -> Result<CallToolResult, McpError> {
        roster_set_entry(&self.db, &params, &Utc::now()).map_err(internal_error)
    }

    pub(crate) async fn harness_roster_clear_impl(
        &self,
        params: RosterClearParams,
    ) -> Result<CallToolResult, McpError> {
        self.db
            .roster_delete(&params.platform, params.model_pattern.as_deref())
            .map_err(internal_error)?;
        Ok(success_result(&format!(
            "Roster entry for '{}/{}' cleared.",
            params.platform,
            params.model_pattern.as_deref().unwrap_or("(all)")
        )))
    }

    pub(crate) async fn harness_roster_list_impl(
        &self,
        params: RosterListParams,
    ) -> Result<CallToolResult, McpError> {
        let entries = self
            .db
            .roster_list(params.platform.as_deref())
            .map_err(internal_error)?;
        let now = Utc::now();
        let body: Vec<serde_json::Value> = entries.iter().map(|e| entry_json(e, &now)).collect();
        Ok(success_result(
            &serde_json::to_string_pretty(&body).unwrap_or_default(),
        ))
    }

    pub(crate) async fn harness_roster_check_impl(
        &self,
        params: RosterCheckParams,
    ) -> Result<CallToolResult, McpError> {
        let entries = self
            .db
            .roster_list_for_platform(&params.platform)
            .map_err(internal_error)?;
        let avail = effective_state(
            &entries,
            &params.platform,
            params.model.as_deref(),
            &Utc::now(),
        );
        Ok(success_result(
            &serde_json::to_string_pretty(&serde_json::json!({
                "platform": params.platform,
                "model": params.model,
                "available": avail.available,
                "reason": avail.reason,
                "until": avail.until,
                "expired": avail.expired,
                "matched_entry_id": avail.matched_entry_id,
            }))
            .unwrap_or_default(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn set(
        platform: &str,
        pattern: Option<&str>,
        state: &str,
        until: Option<&str>,
    ) -> RosterSetParams {
        RosterSetParams {
            platform: platform.to_string(),
            model_pattern: pattern.map(str::to_string),
            state: state.to_string(),
            reason: Some("quota".to_string()),
            until: until.map(str::to_string),
        }
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn set_refuses_until_with_available() {
        let db = test_db();
        let result = roster_set_entry(
            &db,
            &set("claude", None, "available", Some("2026-07-01T00:00:00Z")),
            &now(),
        )
        .unwrap();
        assert!(result.is_error.unwrap_or(false));
        assert!(text(&result).contains("until is only valid with state=unavailable"));
        assert!(db.roster_list(None).unwrap().is_empty());
    }

    #[test]
    fn set_refuses_until_in_the_past() {
        let db = test_db();
        let result = roster_set_entry(
            &db,
            &set("claude", None, "unavailable", Some("2026-05-01T00:00:00Z")),
            &now(),
        )
        .unwrap();
        assert!(result.is_error.unwrap_or(false));
        assert!(text(&result).contains("until must be in the future"));
        assert!(db.roster_list(None).unwrap().is_empty());
    }

    #[test]
    fn set_accepts_future_until() {
        let db = test_db();
        let result = roster_set_entry(
            &db,
            &set("claude", None, "unavailable", Some("2026-07-01T00:00:00Z")),
            &now(),
        )
        .unwrap();
        assert!(!result.is_error.unwrap_or(false), "{}", text(&result));
        assert_eq!(db.roster_list(None).unwrap().len(), 1);
    }

    #[test]
    fn set_refuses_bare_star_pattern() {
        let db = test_db();
        let result =
            roster_set_entry(&db, &set("claude", Some("*"), "unavailable", None), &now()).unwrap();
        assert!(result.is_error.unwrap_or(false));
        assert!(text(&result).contains("matches nothing"));
    }

    #[test]
    fn set_refuses_unknown_state() {
        let db = test_db();
        let result = roster_set_entry(&db, &set("claude", None, "maybe", None), &now()).unwrap();
        assert!(result.is_error.unwrap_or(false));
    }

    #[test]
    fn check_names_the_deciding_entry() {
        let db = test_db();
        roster_set_entry(&db, &set("opencode-go", None, "unavailable", None), &now()).unwrap();
        let entries = db.roster_list_for_platform("opencode-go").unwrap();
        let avail = effective_state(&entries, "opencode-go", Some("opencode-go/claude"), &now());
        assert!(!avail.available);
        assert_eq!(avail.matched_entry_id, Some(entries[0].id));
    }
}
