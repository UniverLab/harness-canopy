//! CM48: MCP tools for the availability roster.
//!
//! These four tools are the only write path (besides the CLI, which calls
//! them) to `roster_entries`. The engine never writes the table — it only
//! reads it at dispatch.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::roster_entries::{NewRosterEntry, RosterEntry, RosterState};
use crate::db::Database;
use crate::domain::roster::effective_state;
use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

fn parse_until(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Whether an entry is an `unavailable` one whose `until` has already passed.
fn is_expired(entry: &RosterEntry, now: &DateTime<Utc>) -> bool {
    entry.state == RosterState::Unavailable
        && entry
            .until
            .as_deref()
            .and_then(parse_until)
            .is_some_and(|until| until <= *now)
}

fn entry_json(entry: &RosterEntry, now: &DateTime<Utc>) -> serde_json::Value {
    let expired = is_expired(entry, now);
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

/// Validate one `roster_set` request and upsert it. Free function so it is
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

#[tool_router(router = tool_router_roster, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "roster_set",
        description = "Declare one platform/model pair available or unavailable. `model_pattern` is omitted for every model of the platform, or an exact model id, or a prefix ending in `*` (e.g. opencode-go/*). `until` (RFC 3339 UTC) is accepted only with state=unavailable and must be in the future."
    )]
    pub(crate) async fn roster_set(
        &self,
        Parameters(params): Parameters<RosterSetParams>,
    ) -> Result<CallToolResult, McpError> {
        roster_set_entry(&self.db, &params, &Utc::now()).map_err(internal_error)
    }

    #[tool(
        name = "roster_clear",
        description = "Delete one roster entry (idempotent). Omit `model_pattern` to delete the platform-wide entry."
    )]
    pub(crate) async fn roster_clear(
        &self,
        Parameters(params): Parameters<RosterClearParams>,
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

    #[tool(
        name = "roster_list",
        description = "List every roster entry, optionally filtered to one platform, each with its effective state now (available, and whether an unavailable entry has expired)."
    )]
    pub(crate) async fn roster_list(
        &self,
        Parameters(params): Parameters<RosterListParams>,
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

    #[tool(
        name = "roster_check",
        description = "Return the effective availability of one platform/model pair and name the roster entry that decided it."
    )]
    pub(crate) async fn roster_check(
        &self,
        Parameters(params): Parameters<RosterCheckParams>,
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
