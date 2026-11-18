//! Built-in adversarial spec review worker.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use serde::Deserialize;
use tokio::sync::Notify;

use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::graphs::{SpecReviewFinding, SpecReviewState, SpecReviewTier};

pub const SPEC_REVIEW_PROMPT: &str = r#"You are an adversarial spec reviewer.

Review the spec below against its intent. Work in this order:
(a) Check each functional requirement against the intent and name the first that contradicts or exceeds it. When the spec has no intent, the objective is the intent for this check.
(b) Check that the spec decides everything and contains no "choose", "consider" or "if needed".
(c) Check that no requirement asks the executor to measure, reproduce or report upstream.
(d) Classify `trivial` when the change is one file or under 40 changed lines with no new public surface, otherwise `normal`.

Return exactly one JSON object and nothing else:
{"verdict":"approve"|"reject","tier":"trivial"|"normal","findings":[{"severity":"high"|"medium"|"low","text":"..."}]}
"#;

pub type ReviewRunner = Arc<
    dyn Fn(String, String, String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>>
        + Send
        + Sync,
>;

static SPEC_REVIEW_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static DELIVERY_NOTIFY: OnceLock<Arc<Notify>> = OnceLock::new();
static STARTED: OnceLock<()> = OnceLock::new();
static RUNNER_OVERRIDE: OnceLock<std::sync::RwLock<Option<ReviewRunner>>> = OnceLock::new();

pub fn spec_review_notify() -> Arc<Notify> {
    SPEC_REVIEW_NOTIFY
        .get_or_init(|| Arc::new(Notify::new()))
        .clone()
}

pub fn wake_spec_review_worker() {
    spec_review_notify().notify_one();
}

fn delivery_notify() -> Option<Arc<Notify>> {
    DELIVERY_NOTIFY.get().cloned()
}

fn runner_override() -> &'static std::sync::RwLock<Option<ReviewRunner>> {
    RUNNER_OVERRIDE.get_or_init(|| std::sync::RwLock::new(None))
}

#[allow(dead_code)]
pub fn set_review_runner_for_tests(runner: ReviewRunner) {
    *runner_override().write().unwrap() = Some(runner);
    wake_spec_review_worker();
}

#[allow(dead_code)]
pub fn clear_review_runner_for_tests() {
    *runner_override().write().unwrap() = None;
}

pub fn start_worker(db: Arc<Database>, scheduled_prompt_notify: Arc<Notify>) {
    let _ = DELIVERY_NOTIFY.set(scheduled_prompt_notify);
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let _ = db.requeue_in_review_specs();
        loop {
            if let Err(e) = run_pending_reviews(Arc::clone(&db)).await {
                tracing::warn!("spec review worker: {e:#}");
            }
            spec_review_notify().notified().await;
        }
    });
}

#[derive(Debug, Deserialize)]
struct ReviewVerdict {
    verdict: String,
    tier: String,
    #[serde(default)]
    findings: Vec<SpecReviewFinding>,
}

pub(crate) async fn run_pending_reviews(db: Arc<Database>) -> Result<()> {
    loop {
        let cfg = current_config();
        if !cfg.spec_review.enabled {
            break;
        }
        let max_parallel = (cfg.spec_review.max_parallel as usize).max(1);
        let in_review = db.count_in_review_specs()?;
        if in_review >= max_parallel {
            break;
        }
        let Some(spec) = db.claim_next_unreviewed_spec()? else {
            break;
        };
        let db2 = Arc::clone(&db);
        tokio::spawn(async move {
            let _ = review_one(db2, spec).await;
            wake_spec_review_worker();
        });
    }
    Ok(())
}

fn current_config() -> CanopyConfig {
    let dir =
        crate::daemon::helpers::data_dir().unwrap_or_else(|_| crate::domain::prompts::canopy_dir());
    CanopyConfig::load(&dir)
}

pub(crate) async fn review_one(
    db: Arc<Database>,
    spec: crate::domain::graphs::GraphSpec,
) -> Result<()> {
    let cfg = current_config();
    let prompt = build_prompt(&spec);
    let session_id = spec.created_by_session.clone();
    let mut last_err: Option<String> = None;
    for _ in 0..3 {
        let runner = runner_override().read().unwrap().clone();
        let out = if let Some(runner) = runner {
            runner(
                cfg.spec_review.platform.clone(),
                cfg.spec_review.model.clone(),
                prompt.clone(),
            )
            .await
        } else {
            invoke_headless(&cfg, &prompt).await
        };
        match out.and_then(|text| parse_verdict(&text)) {
            Ok(v) => {
                apply_verdict(&db, &spec.id, &v)?;
                crate::graph_engine::spec_review_drain::drain_if_ready(
                    &db,
                    session_id.as_deref(),
                    delivery_notify().as_ref(),
                )?;
                return Ok(());
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    let cause = last_err.unwrap_or_else(|| "unknown error".to_string());
    let findings = vec![SpecReviewFinding {
        severity: "high".to_string(),
        text: cause,
    }];
    db.set_spec_review_result(
        &spec.id,
        SpecReviewState::ReviewError,
        None,
        Some(&findings),
    )?;
    crate::graph_engine::spec_review_drain::drain_if_ready(
        &db,
        session_id.as_deref(),
        delivery_notify().as_ref(),
    )?;
    Ok(())
}

fn build_prompt(spec: &crate::domain::graphs::GraphSpec) -> String {
    let intent = spec.intent.as_deref().unwrap_or("none");
    let objective = spec.description.as_deref().unwrap_or("");
    format!(
        "{SPEC_REVIEW_PROMPT}\n\nSpec name: {}\nIntent: {}\nObjective: {}\nRequirements: {}\n",
        spec.name, intent, objective, objective
    )
}

fn parse_verdict(text: &str) -> Result<ReviewVerdict> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<ReviewVerdict>(trimmed) {
        return Ok(v);
    }
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end >= start {
                if let Ok(v) = serde_json::from_str::<ReviewVerdict>(&trimmed[start..=end]) {
                    return Ok(v);
                }
            }
        }
    }
    Err(anyhow!("unparsable review verdict"))
}

fn apply_verdict(db: &Database, spec_id: &str, v: &ReviewVerdict) -> Result<()> {
    let tier = match v.tier.as_str() {
        "trivial" => SpecReviewTier::Trivial,
        "normal" => SpecReviewTier::Normal,
        _ => return Err(anyhow!("invalid tier")),
    };
    match v.verdict.as_str() {
        "approve" => {
            db.set_spec_review_result(
                spec_id,
                SpecReviewState::Approved,
                Some(tier),
                Some(&v.findings),
            )?;
        }
        "reject" => {
            db.set_spec_review_result(spec_id, SpecReviewState::Rejected, None, Some(&v.findings))?;
        }
        _ => return Err(anyhow!("invalid verdict")),
    }
    Ok(())
}

async fn invoke_headless(cfg: &CanopyConfig, prompt: &str) -> Result<String> {
    let platform = cfg.spec_review.platform.trim();
    let model = cfg.spec_review.model.trim();
    let model = if model.is_empty() { None } else { Some(model) };
    let Some(cli_config) = cfg.get_cli(platform) else {
        return Err(anyhow!("spec_review platform '{platform}' not configured"));
    };
    let strategy = crate::domain::cli_strategy::CliStrategy::from_cli_config(cli_config);
    let mut cmd = strategy.build_command(prompt, model, None, None)?;
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let child = cmd.spawn()?;
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(cfg.agents.probe_timeout_seconds.max(30)),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow!("spec review timed out"))??;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() && stdout.trim().is_empty() {
        return Err(anyhow!("reviewer failed: {}", stderr.trim()));
    }
    if stdout.trim().is_empty() && !stderr.trim().is_empty() {
        Ok(stderr)
    } else {
        Ok(stdout)
    }
}

fn findings_str(spec: &crate::domain::graphs::GraphSpec) -> String {
    spec.review_findings
        .as_ref()
        .and_then(|f| serde_json::to_string(f).ok())
        .unwrap_or_else(|| "[]".to_string())
}

/// Bail for an unqueued (bound-spec) run when any pending, non-placeholder
/// spec is not approved for execution.
pub(crate) fn bail_if_unqueued_specs_unapproved(db: &Database, graph_id: &str) -> Result<()> {
    for spec in db.list_graph_specs(graph_id)? {
        if spec.status == crate::domain::graphs::GraphSpecStatus::Pending
            && !crate::graph_engine::is_no_spec_placeholder(&spec)
            && !spec.review_state.is_runnable()
        {
            anyhow::bail!(
                "spec '{}' is {}: review findings: {}",
                spec.name,
                spec.review_state.as_str(),
                findings_str(&spec)
            );
        }
    }
    Ok(())
}

/// Warn about stale running members and skip non-approved pending members for
/// a queue-driven dispatch.
pub(crate) fn warn_queue_dispatch_review_gates(
    db: &Database,
    graph_id: &str,
    queue_id: &str,
) -> Result<()> {
    for spec_id in db.queue_stale_running_members(queue_id, crate::system::boot_id().as_deref())? {
        tracing::warn!(
            "Graph '{}' queue run against '{}': member spec '{}' is 'running' with no              live node run in this daemon's lifetime; leaving it as-is. Reset it via              graph_control reset to resume if it's genuinely stuck.",
            graph_id,
            queue_id,
            spec_id
        );
    }
    for spec in db.queue_unapproved_pending_members(queue_id)? {
        tracing::warn!(
            "queue run skipped non-approved spec '{}' ({}): review findings: {}",
            spec.name,
            spec.review_state.as_str(),
            findings_str(&spec)
        );
    }
    Ok(())
}

/// For tests: process reviews until drained (best effort).
#[allow(dead_code)]
pub async fn drain_for_tests(db: &Arc<Database>) -> Result<()> {
    let _ = db.requeue_in_review_specs();
    // run multiple passes to handle spawns
    for _ in 0..20 {
        run_pending_reviews(Arc::clone(db)).await?;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if db.count_in_review_specs()? == 0 {
            let cfg = current_config();
            if !cfg.spec_review.enabled {
                break;
            }
            // check if any unreviewed left
            let has = db.claim_next_unreviewed_spec()?;
            if let Some(s) = has {
                // put back as unreviewed
                db.mark_spec_unreviewed(&s.id)?;
            } else {
                break;
            }
        }
    }
    Ok(())
}
