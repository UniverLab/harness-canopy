//! The hub's Prompt Builder catalog: what the remote lists, and the
//! hub-side draft builds behind `compose` and draft `prompt` frames.
//!
//! [`CatalogData`] is the wire `catalog` answer — CLIs with models (the New
//! dialog's sources), projects, presets and skills (the Prompt Builder
//! pickers' sources). Draft builds resolve through the domain
//! [`PromptCatalog`](crate::domain::prompt_draft::PromptCatalog) loaded from
//! those same sources, so the remote can never offer a choice the hub
//! cannot build.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::domain::models_db::{self, ModelCatalog};
use crate::domain::prompt_draft::{build_prompt_text, PromptCatalog, PromptContext, PromptDraft};
use crate::tui::{available_clis_for_catalog, cli_label};

use super::protocol::{
    error_frame, ok_frame, ERR_BAD_REQUEST, ERR_DELIVERY_FAILED, ERR_FORBIDDEN, ERR_INTERNAL,
    ERR_NOT_FOUND,
};
use super::ws::{no_such_session, OutTx, SessionsState};
use super::{HubSession, SessionKind, SessionStatus};

/// One CLI row of the wire catalog: the stored slug, the New dialog's
/// display label, the model ids the New dialog would suggest for it, and
/// whether the harness has a yolo mode (`yolo_flag` in the registry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogCli {
    pub cli: String,
    pub label: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub yolo: bool,
}

/// One project row of the wire catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogProject {
    pub name: String,
    pub path: String,
}

/// One preset row of the wire catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogPreset {
    pub name: String,
    pub preview: String,
}

/// One skill row of the wire catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSkill {
    pub name: String,
    pub label: String,
}

/// The wire `catalog` answer: `clis`, `projects`, `presets`, `skills`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogData {
    pub clis: Vec<CatalogCli>,
    pub projects: Vec<CatalogProject>,
    pub presets: Vec<CatalogPreset>,
    pub skills: Vec<CatalogSkill>,
}

impl CatalogData {
    /// Load the catalog from the pickers' own sources: the New dialog's
    /// CLIs and model catalog, plus the domain prompt catalog for
    /// `db`/`workdir`/`canopy_dir`.
    pub fn load(db: &Database, workdir: &Path, canopy_dir: &Path) -> anyhow::Result<CatalogData> {
        let catalog = PromptCatalog::load(db, workdir, canopy_dir)?;
        let config = crate::domain::canopy_config::CanopyConfig::load(canopy_dir);
        let model_catalog = models_db::load_catalog_nonblocking(config.models.catalog_ttl());
        Ok(CatalogData {
            clis: load_cli_entries(model_catalog.as_ref()),
            projects: catalog
                .projects
                .into_iter()
                .map(|project| CatalogProject {
                    name: project.name,
                    path: project.path,
                })
                .collect(),
            presets: catalog
                .presets
                .into_iter()
                .map(|preset| CatalogPreset {
                    name: preset.name,
                    preview: preset.preview,
                })
                .collect(),
            skills: catalog
                .skills
                .into_iter()
                .map(|skill| CatalogSkill {
                    name: skill.name,
                    label: skill.label,
                })
                .collect(),
        })
    }
}

fn load_cli_entries(model_catalog: Option<&ModelCatalog>) -> Vec<CatalogCli> {
    let (available, configs) = available_clis_for_catalog();
    available
        .into_iter()
        .zip(configs)
        .map(|(cli, config)| {
            // A CLI with no provider mapping would get the whole registry
            // (thousands of ids per CLI, megabytes per catalog), which no
            // picker can use and no data channel can carry; its model stays
            // free text instead.
            let models: Vec<String> = model_catalog
                .filter(|_| !models_db::providers_for_cli(cli.as_str()).is_empty())
                .map(|catalog| {
                    models_db::suggestions_for(catalog, cli.as_str(), "")
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect()
                })
                .unwrap_or_default();
            CatalogCli {
                cli: cli.as_str().to_string(),
                label: cli_label(cli.as_str(), config.as_ref()),
                models,
                yolo: config
                    .as_ref()
                    .is_some_and(|config| config.yolo_flag.is_some()),
            }
        })
        .collect()
}

/// `composed{req,text}`: the answer to `compose`.
pub fn composed_frame(req: &str, text: &str) -> String {
    serde_json::json!({"t": "composed", "req": req, "text": text}).to_string()
}

/// `catalog{req,clis,projects,presets,skills}`: the answer to `catalog`.
/// Each `clis` row carries `yolo`, true exactly when that harness's registry
/// entry defines a `yolo_flag` — the switch the New dialog offers.
pub fn catalog_frame(req: &str, data: &CatalogData) -> String {
    serde_json::json!({
        "t": "catalog", "req": req, "clis": data.clis,
        "projects": data.projects, "presets": data.presets, "skills": data.skills,
    })
    .to_string()
}

/// Why a hub-side draft build or delivery failed: an unknown choice (the
/// client's fault, answered `bad_request` naming it), a catalog failure
/// (ours, answered `internal`), or a refused paste path.
enum DraftFailure {
    BadRequest(String),
    Internal,
    Delivery(anyhow::Error),
}

/// Build a draft's text against a freshly loaded catalog. Pure blocking
/// work — the handlers run it under `spawn_blocking`.
fn build_draft_text(db: &Database, draft: &PromptDraft) -> Result<String, DraftFailure> {
    let workdir = std::env::current_dir().map_err(|_| DraftFailure::Internal)?;
    let catalog = PromptCatalog::load(db, &workdir, &crate::domain::prompts::canopy_dir())
        .map_err(|_| DraftFailure::Internal)?;
    let ctx = PromptContext {
        catalog,
        ..PromptContext::default()
    };
    build_prompt_text(draft, &ctx).map_err(|e| DraftFailure::BadRequest(e.to_string()))
}

/// `compose`: build the draft's text and answer `composed`. An unknown
/// preset, skill or project answers `bad_request` naming it.
pub fn handle_compose(state: &Arc<SessionsState>, out_tx: &OutTx, req: String, draft: PromptDraft) {
    serve_built_text(Arc::clone(state.hub.db()), out_tx, req, draft, None);
}

/// `catalog`: answer the builder catalog.
pub fn handle_catalog(state: &Arc<SessionsState>, out_tx: &OutTx, req: String) {
    let db = Arc::clone(state.hub.db());
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let loaded = tokio::task::spawn_blocking(move || -> anyhow::Result<CatalogData> {
            let workdir = std::env::current_dir()?;
            CatalogData::load(&db, &workdir, &crate::domain::prompts::canopy_dir())
        })
        .await;
        match loaded {
            Ok(Ok(data)) => {
                let _ = out_tx.send(catalog_frame(&req, &data));
            }
            Ok(Err(_)) | Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "catalog failed"));
            }
        }
    });
}

/// Draft `prompt`: build the text hub-side, then deliver it like a text
/// `prompt`. Unknown choices fail before anything is delivered.
pub fn handle_prompt_draft(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: &str,
    draft: PromptDraft,
) {
    let Some(session) = state.hub.get(id) else {
        let _ = out_tx.send(error_frame(Some(&req), ERR_NOT_FOUND, &no_such_session(id)));
        return;
    };
    if session.status() != SessionStatus::Running {
        let _ = out_tx.send(error_frame(Some(&req), ERR_FORBIDDEN, "session has exited"));
        return;
    }
    serve_built_text(
        Arc::clone(state.hub.db()),
        out_tx,
        req,
        draft,
        Some(session),
    );
}

/// One pending scheduled prompt, as carried by `scheduled` / `scheduled_list`:
/// the row id, the fire time in RFC 3339 UTC, and the first 120 chars of the
/// composed prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledPromptItem {
    pub id: String,
    pub fire_at: String,
    pub preview: String,
}

/// `scheduled{req,item}`: the answer to `prompt_schedule`.
pub fn scheduled_frame(req: &str, item: &ScheduledPromptItem) -> String {
    serde_json::json!({"t": "scheduled", "req": req, "item": item}).to_string()
}

/// `scheduled_list{req,id,items}`: the answer to `scheduled_list`, soonest
/// first.
pub fn scheduled_list_frame(req: &str, id: &str, items: &[ScheduledPromptItem]) -> String {
    serde_json::json!({"t": "scheduled_list", "req": req, "id": id, "items": items}).to_string()
}

/// The first 120 chars of the composed prompt, cut on char boundaries so a
/// multibyte char is kept whole or dropped, never split.
fn preview_of(text: &str) -> String {
    text.chars().take(120).collect()
}

/// Parse `fire_at` as RFC 3339 and require it to be in the future. The stored
/// (and echoed) time is truncated to whole seconds, matching the database.
fn parse_future_fire_at(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let parsed = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    if parsed <= chrono::Utc::now() {
        return None;
    }
    chrono::DateTime::from_timestamp(parsed.timestamp(), 0)
}

/// The session gate the three schedule frames share: the session must exist,
/// still be running, and be an agent — the daemon only delivers to those.
/// `Err` carries the already-framed error answer.
fn scheduled_session(
    state: &SessionsState,
    req: &str,
    id: &str,
) -> Result<Arc<HubSession>, String> {
    let Some(session) = state.hub.get(id) else {
        return Err(error_frame(Some(req), ERR_NOT_FOUND, &no_such_session(id)));
    };
    if session.status() != SessionStatus::Running {
        return Err(error_frame(Some(req), ERR_FORBIDDEN, "session has exited"));
    }
    if session.kind != SessionKind::Agent {
        return Err(error_frame(
            Some(req),
            ERR_FORBIDDEN,
            "only agent sessions take a scheduled prompt",
        ));
    }
    Ok(session)
}

/// `prompt_schedule`: build the draft's text and store it for delivery at
/// `fire_at`. Unknown choices fail exactly like `compose`; the row carries
/// the session's workdir with no provenance or builder state, like the TUI's
/// raw path. Delivery itself is the daemon scheduler's tick.
pub fn handle_prompt_schedule(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: &str,
    draft: PromptDraft,
    fire_at_raw: &str,
) {
    let session = match scheduled_session(state, &req, id) {
        Ok(session) => session,
        Err(answer) => {
            let _ = out_tx.send(answer);
            return;
        }
    };
    let Some(fire_at) = parse_future_fire_at(fire_at_raw) else {
        let _ = out_tx.send(error_frame(
            Some(&req),
            ERR_BAD_REQUEST,
            "fire_at must be a future RFC 3339 time",
        ));
        return;
    };
    let db = Arc::clone(state.hub.db());
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || {
            let text = build_draft_text(&db, &draft)?;
            let schedule_id = format!("ss-{}", uuid::Uuid::new_v4());
            let workdir_opt = (!session.workdir.is_empty()).then_some(session.workdir.as_str());
            db.insert_scheduled_prompt(
                &schedule_id,
                &text,
                &session.id,
                workdir_opt,
                fire_at,
                None,
                None,
            )
            .map_err(|_| DraftFailure::Internal)?;
            Ok::<_, DraftFailure>((schedule_id, text))
        })
        .await;
        // The failure mapping mirrors `serve_built_text` arm for arm.
        match outcome {
            Ok(Ok((schedule_id, text))) => {
                let item = ScheduledPromptItem {
                    id: schedule_id,
                    fire_at: fire_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    preview: preview_of(&text),
                };
                let _ = out_tx.send(scheduled_frame(&req, &item));
            }
            Ok(Err(DraftFailure::BadRequest(message))) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_BAD_REQUEST, &message));
            }
            Ok(Err(DraftFailure::Internal)) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"));
            }
            Ok(Err(DraftFailure::Delivery(e))) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_DELIVERY_FAILED, &e.to_string()));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"));
            }
        }
    });
}

/// `scheduled_list`: the session's pending schedules, soonest first.
pub fn handle_scheduled_list(state: &Arc<SessionsState>, out_tx: &OutTx, req: String, id: &str) {
    let session = match scheduled_session(state, &req, id) {
        Ok(session) => session,
        Err(answer) => {
            let _ = out_tx.send(answer);
            return;
        }
    };
    let db = Arc::clone(state.hub.db());
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let session_id = session.id.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            db.list_pending_scheduled_prompts_for_session(&session_id)
        })
        .await;
        match outcome {
            Ok(Ok(rows)) => {
                let items: Vec<ScheduledPromptItem> = rows
                    .iter()
                    .map(|row| ScheduledPromptItem {
                        id: row.id.clone(),
                        fire_at: row
                            .fire_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        preview: preview_of(&row.prompt),
                    })
                    .collect();
                let _ = out_tx.send(scheduled_list_frame(&req, &session.id, &items));
            }
            Ok(Err(_)) | Err(_) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    "schedule list failed",
                ));
            }
        }
    });
}

/// `scheduled_cancel`: drop one pending schedule. The row must belong to the
/// named session — another session's row answers `not_found` and is kept.
pub fn handle_scheduled_cancel(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: &str,
    schedule_id: String,
) {
    if let Err(answer) = scheduled_session(state, &req, id) {
        let _ = out_tx.send(answer);
        return;
    }
    let db = Arc::clone(state.hub.db());
    let out_tx = out_tx.clone();
    let session_id = id.to_string();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
            let pending = db.list_pending_scheduled_prompts_for_session(&session_id)?;
            if !pending.iter().any(|row| row.id == schedule_id) {
                return Ok(false);
            }
            db.delete_scheduled_prompt(&schedule_id)
        })
        .await;
        match outcome {
            Ok(Ok(true)) => {
                let _ = out_tx.send(ok_frame(&req));
            }
            Ok(Ok(false)) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    "no such scheduled prompt",
                ));
            }
            Ok(Err(_)) | Err(_) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    "schedule cancel failed",
                ));
            }
        }
    });
}

/// Shared tail of `compose` and draft `prompt`: build the text off the
/// executor, then either answer `composed` or deliver it through the paste
/// path (like text `prompt`) and answer `ok`.
fn serve_built_text(
    db: Arc<Database>,
    out_tx: &OutTx,
    req: String,
    draft: PromptDraft,
    deliver_to: Option<Arc<HubSession>>,
) {
    let answer_composed = deliver_to.is_none();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let outcome = tokio::task::spawn_blocking(move || {
            let text = build_draft_text(&db, &draft)?;
            if let Some(session) = deliver_to {
                session.prompt(&text).map_err(DraftFailure::Delivery)?;
            }
            Ok::<_, DraftFailure>(text)
        })
        .await;
        match outcome {
            Ok(Ok(text)) => {
                if answer_composed {
                    let _ = out_tx.send(composed_frame(&req, &text));
                } else {
                    let _ = out_tx.send(ok_frame(&req));
                }
            }
            Ok(Err(DraftFailure::BadRequest(message))) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_BAD_REQUEST, &message));
            }
            Ok(Err(DraftFailure::Internal)) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"));
            }
            Ok(Err(DraftFailure::Delivery(e))) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_DELIVERY_FAILED, &e.to_string()));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"));
            }
        }
    });
}
