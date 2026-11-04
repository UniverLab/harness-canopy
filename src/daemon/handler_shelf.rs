//! MCP handler tools split out of `daemon::handler` (session shelf tools):
//! `session_send_file` (agent → shelf), `session_list_files` and
//! `session_read_file` (agent → shelf reads). Remote users reach the same
//! shelf through the `shelf_put_*` protocol frames.

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
        description = "Copy files onto the session shelf — the session's file tray, surfaced live to its TUI over `/sessions/ws`. Pass 1 to 10 absolute paths per call; any file type is accepted (content sniffing only decides how it is displayed) and each file is at most 20 MiB. Omit `session_id` to shelve under your own session; a caller with no session shelves on the host shelf. One bad path never blocks the others: the result lists `items` (`file_id`, `name`, `mime`, `size`) and per-path `errors` (`path`, `code`, `message`), with codes `not_found`, `not_a_file`, `too_large`, `io`."
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
        // shelf (`None`). The `from` label is the *sender's* session name
        // (FR1): the caller's own session, even when `session_id` names
        // another shelf; `host` when the caller has no session. ─────────
        let caller_session = self
            .resolve_sync_agent_id(parts.as_ref())
            .ok()
            .and_then(|id| self.db.get_hookable_session(&id).ok().flatten());
        let session_id: Option<String> = params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| caller_session.as_ref().map(|s| s.id.clone()));
        let from_label = caller_session
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

    /// List one session shelf with `origin`, which the wire `shelf` frame
    /// omits.
    #[tool(
        name = "session_list_files",
        description = "List one session shelf, newest first. Each item carries `file_id`, `name`, `size`, `mime`, `origin` (`agent` when an agent sent it, `user` when a remote user uploaded it) and `created_at`. Omit `session_id` to list your own session's shelf; a caller with no session lists the host shelf."
    )]
    pub(crate) async fn session_list_files(
        &self,
        Parameters(params): Parameters<SessionListFilesParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let session_id = params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                self.resolve_sync_agent_id(parts.as_ref())
                    .ok()
                    .and_then(|id| self.db.get_hookable_session(&id).ok().flatten())
                    .map(|session| session.id)
            });
        let shelf = Arc::clone(&self.shelf);
        let rows = tokio::task::spawn_blocking(move || shelf.list_rows(session_id.as_deref()))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let items: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|row| {
                serde_json::json!({
                    "file_id": row.file_id,
                    "name": row.name,
                    "size": row.size,
                    "mime": row.mime,
                    "origin": row.origin,
                    "created_at": row.created_at,
                })
            })
            .collect();
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&serde_json::json!({ "items": items }))
                .unwrap_or_else(|_| "{\"items\":[]}".to_string()),
        )]))
    }

    /// Read one shelf file: text when the bytes are valid UTF-8, base64
    /// otherwise. Binary reads are capped at 1 MiB (`truncated:true` past
    /// it).
    #[tool(
        name = "session_read_file",
        description = "Read one shelf file by `file_id`. Returns `file_id`, `name`, `mime`, `size`, `enc` (`text` or `b64`), `content` and `truncated`. Text files come back as UTF-8 text; binary files come back as base64 capped at 1 MiB, with `truncated:true` when the file is larger."
    )]
    pub(crate) async fn session_read_file(
        &self,
        Parameters(params): Parameters<SessionReadFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(row) = self.shelf.get(params.file_id.trim()) else {
            return Ok(error_result(&format!(
                "no shelf file '{}'",
                params.file_id.trim()
            )));
        };
        let shelf = Arc::clone(&self.shelf);
        let file_id = row.file_id.clone();
        let bytes = tokio::task::spawn_blocking(move || shelf.read_file(&file_id))
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
            .ok_or_else(|| {
                McpError::internal_error(
                    format!("shelf file '{}' has no bytes on disk", row.file_id),
                    None,
                )
            })?;
        const CAP: usize = 1024 * 1024;
        let truncated = bytes.len() > CAP;
        // The encoding is decided by the whole file, not the capped prefix:
        // deciding from the prefix would mislabel a text file as binary
        // whenever the cap splits a multi-byte character. Text truncates at
        // a character boundary; binary truncates at the raw byte cap.
        let (enc, content) = match std::str::from_utf8(&bytes) {
            Ok(text) => {
                let mut end = text.len().min(CAP);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                ("text", text[..end].to_string())
            }
            Err(_) => {
                use base64::Engine as _;
                (
                    "b64",
                    base64::engine::general_purpose::STANDARD
                        .encode(&bytes[..bytes.len().min(CAP)]),
                )
            }
        };
        let out = serde_json::json!({
            "file_id": row.file_id,
            "name": row.name,
            "mime": row.mime,
            "size": row.size,
            "enc": enc,
            "content": content,
            "truncated": truncated,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&out).unwrap_or_else(|_| out.to_string()),
        )]))
    }
}
