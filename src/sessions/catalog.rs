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
use super::{HubSession, SessionStatus};

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
            let models: Vec<String> = model_catalog
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
