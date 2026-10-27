//! MCP handler tools split out of `daemon::handler` (session shelf tools).
//! One tool, `session_send_file`: copy files onto the session shelf the
//! session's TUI (and `/sessions/ws` clients) see them on.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::sessions::shelf::{MAX_CAPTION_CHARS, MAX_PATHS_PER_CALL};
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_shelf, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Copy files onto the session shelf (FR2).
    #[tool(
        name = "session_send_file",
        description = "Copy files onto the session shelf — the session's file tray, surfaced live to its TUI over `/sessions/ws`. Pass 1 to 10 absolute paths per call; each file must be PNG, JPEG, WebP, GIF or SVG (sniffed from content, never the extension) and at most 20 MiB. Omit `session_id` to shelve under your own session; a caller with no session shelves on the host shelf. One bad path never blocks the others: the result lists `items` (`file_id`, `name`, `mime`, `size`) and per-path `errors` (`path`, `code`, `message`), with codes `not_found`, `not_a_file`, `too_large`, `unsupported_type`, `io`."
    )]
    pub(crate) async fn session_send_file(
        &self,
        Parameters(params): Parameters<SessionSendFileParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        // Nursery sessions stay out — but only when the caller has an
        // identity to check. An anonymous caller is not a nursery session;
        // it shelves on the host shelf (FR2's third resolution case), so a
        // missing identity must fall through instead of failing here.
        if self.resolve_sync_agent_id(parts.as_ref()).is_ok() {
            self.reject_if_nursery(parts.as_ref())?;
        }
        self.record_harness_id(parts.as_ref());

        // ── Input validation (whole-call rejects) ────────────────────
        if params.paths.is_empty() || params.paths.len() > MAX_PATHS_PER_CALL {
            return Ok(error_result(&format!(
                "Pass between 1 and {MAX_PATHS_PER_CALL} `paths`."
            )));
        }
        for path in &params.paths {
            if !Path::new(path).is_absolute() {
                return Ok(error_result(&format!(
                    "Path '{path}' is not absolute: pass absolute file paths."
                )));
            }
        }
        if let Some(caption) = params.caption.as_deref() {
            if caption.trim().chars().count() > MAX_CAPTION_CHARS {
                return Ok(error_result(&format!(
                    "`caption` is at most {MAX_CAPTION_CHARS} characters."
                )));
            }
        }

        // ── Session resolution (FR2): an explicit id wins, else the
        // session this caller runs in (header/env identity mapped to its
        // session row — a mapping failure falls through), else the host
        // shelf (`None`). ────────────────────────────────────────────
        let session_id: Option<String> = match params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => Some(id.to_string()),
            None => self
                .resolve_sync_agent_id(parts.as_ref())
                .ok()
                .and_then(|id| {
                    self.db
                        .get_hookable_session(&id)
                        .ok()
                        .flatten()
                        .map(|s| s.id)
                }),
        };
        // The sending session's name, or `host` for the host shelf or an
        // id with no session row.
        let from_label = session_id
            .as_deref()
            .and_then(|id| self.db.get_hookable_session(id).ok().flatten())
            .map(|s| s.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "host".to_string());

        // ── Per-path inserts: file IO up to 20 MiB per file, off the
        // async runtime. One bad path is an entry in `errors`, never a
        // tool error and never a block on the other paths. ───────────
        let shelf = Arc::clone(&self.shelf);
        let paths = params.paths.clone();
        let caption = params.caption.clone();
        let (items, errors) = tokio::task::spawn_blocking(move || {
            let mut items = Vec::new();
            let mut errors = Vec::new();
            for path in paths {
                match shelf.insert(
                    &PathBuf::from(&path),
                    session_id.as_deref(),
                    caption.as_deref(),
                    &from_label,
                ) {
                    Ok(item) => items.push(serde_json::json!({
                        "file_id": item.file_id,
                        "name": item.name,
                        "mime": item.mime,
                        "size": item.size,
                    })),
                    Err(e) => errors.push(serde_json::json!({
                        "path": path,
                        "code": e.code,
                        "message": e.message,
                    })),
                }
            }
            (items, errors)
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let out = serde_json::json!({ "items": items, "errors": errors });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()),
        )]))
    }
}
