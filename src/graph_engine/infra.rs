use super::*;

pub(crate) fn read_infra_config(node: &GraphNode) -> (u32, u64, u64) {
    let resolved = resolve_node_infra_config(node);
    (
        resolved.retry_limit.value as u32,
        resolved.crash_max_seconds.value,
        resolved.backoff_seconds.value,
    )
}

/// CM30: one resolved infra-retry field plus where it came from. `"node"`
/// covers a plain node's own config key AND an already-baked member/ensemble
/// override (see the design doc in canopy-design.md for why those two can't
/// be told apart here). `"platform"` and `"default"` are unambiguous.
pub(crate) struct ResolvedInfraField {
    pub value: u64,
    pub source: &'static str,
}

/// CM30: the three infra-retry fields resolved together, in
/// `read_infra_config`'s own order.
pub(crate) struct NodeInfraConfig {
    pub retry_limit: ResolvedInfraField,
    pub crash_max_seconds: ResolvedInfraField,
    pub backoff_seconds: ResolvedInfraField,
}

pub(crate) fn resolve_infra_field(
    node_config: &Value,
    key: &str,
    platform_cli: Option<&crate::domain::cli_config::CliConfig>,
    platform_value: impl Fn(&crate::domain::cli_config::CliConfig) -> Option<u64>,
    default: u64,
) -> ResolvedInfraField {
    if let Some(v) = node_config.get(key).and_then(Value::as_u64) {
        return ResolvedInfraField {
            value: v,
            source: "node",
        };
    }
    if let Some(cli) = platform_cli {
        if let Some(v) = platform_value(cli) {
            return ResolvedInfraField {
                value: v,
                source: "platform",
            };
        }
    }
    ResolvedInfraField {
        value: default,
        source: "default",
    }
}

/// CM30: the platform named by `node.config`'s `platform` (or `cli`) field,
/// resolved to its `CliConfig` via a freshly-loaded `~/.canopy/config.toml`
/// — same lookup `cli_model_flag` (below) already does, plus the
/// `CANOPY_HOME_OVERRIDE` test hook from `Cli::strategy`, so a `config.toml`
/// edit applies on the very next dispatch with no daemon restart (FR1).
/// `None` when the node names no platform, or the platform has no registry
/// entry.
pub(crate) fn platform_cli_for_node(
    node_config: &Value,
) -> Option<crate::domain::cli_config::CliConfig> {
    let platform = node_config
        .get("platform")
        .or_else(|| node_config.get("cli"))
        .and_then(Value::as_str)?;
    // `CANOPY_HOME_OVERRIDE` lets tests point this at a fixture
    // `.canopy/config.toml` — same pattern as `Cli::strategy`. Unset in
    // production, where this is just `dirs::home_dir()`.
    let home = std::env::var_os("CANOPY_HOME_OVERRIDE")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
    config.get_cli(platform).cloned()
}

/// CM30: resolve a node's effective infra-retry config with provenance —
/// used by `read_infra_config` (dispatch, values only) and by
/// `graph_get`/`graph_audit_node_configs` in `daemon::handler` (values +
/// source, for read-back). `node_config` is `node.config` for a real node,
/// or a bare `{"platform": ...}`/`Value::Null` to resolve a platform-only
/// baseline (see `resolve_platform_only_infra_config`, used by the audit
/// tool to detect "differs from platform's own value").
pub(crate) fn resolve_infra_config_from(node_config: &Value) -> NodeInfraConfig {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let defaults = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir).graphs;
    resolve_infra_config_from_with(node_config, &defaults)
}

/// Pure form of [`resolve_infra_config_from`] over explicit graph defaults,
/// so tests can assert the member > ensemble > platform > setting precedence
/// without touching `HOME`. Precedence (strongest first): the node's own
/// `infra_*` key (covers a member/ensemble override baked into the config),
/// the platform's `clis[]` entry, then the global `graphs.infra_*` setting.
pub(crate) fn resolve_infra_config_from_with(
    node_config: &Value,
    defaults: &crate::domain::canopy_config::GraphsConfig,
) -> NodeInfraConfig {
    let platform_cli = platform_cli_for_node(node_config);
    NodeInfraConfig {
        retry_limit: resolve_infra_field(
            node_config,
            "infra_retry_limit",
            platform_cli.as_ref(),
            |c| c.infra_retry_limit.map(u64::from),
            u64::from(defaults.infra_retry_limit),
        ),
        crash_max_seconds: resolve_infra_field(
            node_config,
            "infra_crash_max_seconds",
            platform_cli.as_ref(),
            |c| c.infra_crash_max_seconds,
            defaults.infra_crash_max_seconds,
        ),
        backoff_seconds: resolve_infra_field(
            node_config,
            "infra_backoff_seconds",
            platform_cli.as_ref(),
            |c| c.infra_backoff_seconds,
            defaults.infra_backoff_seconds,
        ),
    }
}

/// CM30: resolve a real node's effective config (node.config first, then
/// its platform, then the engine default). Used by `read_infra_config` and
/// by `graph_get`'s per-node `infra_config` block.
pub(crate) fn resolve_node_infra_config(node: &GraphNode) -> NodeInfraConfig {
    resolve_infra_config_from(&node.config)
}

/// CM30: resolve what `platform` alone would give (node-level config never
/// consulted) — the baseline `graph_audit_node_configs` compares the real
/// effective value against to decide "differs from platform's own value"
/// (FR5). `platform: None` resolves straight to the engine default.
pub(crate) fn resolve_platform_only_infra_config(platform: Option<&str>) -> NodeInfraConfig {
    let bare = match platform {
        Some(p) => serde_json::json!({ "platform": p }),
        None => Value::Null,
    };
    resolve_infra_config_from(&bare)
}

pub(crate) fn merge_attempt_marker(output: &Value, attempt: u32, is_crash: bool) -> Value {
    let mut obj = output.clone();
    if let serde_json::Value::Object(ref mut map) = obj {
        map.insert("infra_attempt".to_string(), Value::from(attempt));
        map.insert("infra_crash".to_string(), Value::from(is_crash));
    }
    obj
}

/// B19/B39/CM13 infra-crash decision for one agent attempt: a
/// non-self-reported failure of an AGENT node with retry budget still left.
/// A self-reported result, a Check/Gate node, or a permanent spawn failure
/// (binary not found, permission denied) is never an infra crash. CM13:
/// duration, output, and exit code are no longer part of the shape — a run
/// that never filed a verdict is infrastructure at any duration, with any
/// output, at any exit code.
///
/// Shared by the sequential node path ([`GraphEngine::run_spec`]) and, since
/// B26, by ensemble members ([`GraphEngine::execute_ensemble`]) — both use the
/// identical rule so a crashed member is retried exactly like a lone node and
/// only counts as failed for the join once its retries are exhausted.
pub(crate) fn is_infra_crash(
    node: &GraphNode,
    execution: &NodeExecution,
    run: &GraphNodeRun,
    attempt: u32,
    retry_limit: u32,
    crash_max_secs: u64,
) -> bool {
    is_infra_crash_shape(node, execution, run, crash_max_secs) && attempt < retry_limit
}

/// The infra-crash *shape*: every condition [`is_infra_crash`] tests except
/// the `attempt < retry_limit` retry gate — the run never filed a verdict.
/// Checked again after the retry graph settles so that a retry-exhausted
/// infra crash (CM2: route `Error`) is told apart from a genuine negative
/// verdict or an attempt that recovered on retry (route `Fail`/`Pass`),
/// neither of which has this shape.
///
/// CM13: A run that never called `graph_complete_node` or
/// `graph_report_blocker` is infrastructure failure — regardless of
/// duration, output, or exit code. The single fact is whether the run
/// self-reported; everything else (time, stdout, exit code) is noise that
/// let three different failures escape classification on 2026-09-03.
pub(crate) fn is_infra_crash_shape(
    node: &GraphNode,
    execution: &NodeExecution,
    run: &GraphNodeRun,
    _crash_max_secs: u64, // CM13: unused; kept for API compatibility
) -> bool {
    let self_reported = run_self_reported(run);
    let permanent = execution
        .output
        .get("spawn_permanent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // CM13: removed `!no_output`, `!no_report`, `execution.status == Fail`,
    // and the time check. A run that never reported is infrastructure at
    // any duration, with any output, at any exit code.
    !self_reported && !permanent && node.kind == GraphNodeKind::Agent
}

/// CM30/FR4: tag a retry-skipped infra crash's own run row with
/// `infra_crash: true, infra_attempt: 0, infra_retry_skipped: "limit 0"` —
/// the shape `begin_infra_retry` would have written had a retry actually
/// been attempted, for the one case (`infra_retry_limit: 0`) where no retry
/// ever runs to write it. Mutates `execution`/`run` in place (both the
/// in-memory value used for immediate routing/consolidation and the value a
/// later `graph_node_run_get`/`graph_audit_node_configs` reads must agree)
/// and persists the tagged output. A no-op — returns the plain
/// `is_infra_crash_shape` result — for every other case, including a
/// retry_limit > 0 exhaustion (already partially visible via the retried
/// attempts' own markers) and any non-crash outcome, so a config that sets
/// nothing behaves exactly as today (no new fields appear).
#[expect(clippy::too_many_arguments)]
pub(crate) async fn finalize_infra_crash_shape(
    db: &Database,
    run_id: &str,
    node: &GraphNode,
    execution: &mut NodeExecution,
    run: &mut GraphNodeRun,
    attempt: u32,
    retry_limit: u32,
    crash_max_secs: u64,
) -> Result<bool> {
    let is_crash = is_infra_crash_shape(node, execution, run, crash_max_secs);
    if is_crash && attempt == 0 && retry_limit == 0 {
        let mut tagged = merge_attempt_marker(&execution.output, attempt, true);
        if let Value::Object(ref mut map) = tagged {
            map.insert(
                "infra_retry_skipped".to_string(),
                Value::String("limit 0".to_string()),
            );
        }
        db.update_graph_run_result(run_id, run.status, Some(&tagged), Some(chrono::Utc::now()))?;
        execution.output = tagged.clone();
        run.output = Some(tagged);
    }
    Ok(is_crash)
}

/// C19: whether `output` reflects an infrastructure failure — a crash,
/// empty response, or dropped/never-filed report — rather than a genuine
/// verdict an agent (or check/gate) actually produced. Reuses the exact
/// markers [`is_infra_crash`]/`agent_finished_execution` already write
/// (`infra_crash`, `no_output`, `failure_kind: "no_report"`,
/// `failure_kind: "unreported"`) instead of re-deriving the distinction —
/// see [`GraphEngine::record_spec_attempt`], the only caller: an infra
/// failure never consumes the persisted cross-run attempt budget.
pub(crate) fn execution_is_infra_failure(output: &Value) -> bool {
    output
        .get("infra_crash")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || output
            .get("no_output")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || output.get("failure_kind").and_then(Value::as_str) == Some("no_report")
        || output.get("failure_kind").and_then(Value::as_str) == Some("unreported")
        || output
            .get("no_verdict")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Persist a crashed agent attempt with B19 `infra_attempt`/`infra_crash`
/// markers, wait the doubling backoff (`backoff_secs * 2^attempt`), then
/// insert a fresh `Running` run row for the retry and return its id. `attempt`
/// is the zero-based index of the attempt that just crashed. Shared by the
/// sequential node path and ensemble members (B26) so every infra retry — no
/// matter which path — leaves the same distinct, marker-carrying run rows.
#[expect(clippy::too_many_arguments)]
pub(crate) async fn begin_infra_retry(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    previous_output: Option<&Value>,
    iteration: i64,
    crashed_run_id: &str,
    crashed_output: &Value,
    attempt: u32,
    backoff_secs: u64,
) -> Result<String> {
    db.update_graph_run_result(
        crashed_run_id,
        GraphRunStatus::Fail,
        Some(&merge_attempt_marker(crashed_output, attempt, true)),
        Some(chrono::Utc::now()),
    )?;
    tokio::time::sleep(std::time::Duration::from_secs(
        backoff_secs * 2u64.pow(attempt),
    ))
    .await;
    let run_id = uuid::Uuid::new_v4().to_string();
    // CB43: the retry is the same dispatch as the crashed attempt — carry
    // its recorded pair (re-resolve from config only if the original has none,
    // e.g. a pre-migration row). CM49: a crew-bound node ROTATES to another
    // member on retry, so its run must record the (already injected) pair of
    // the node it will actually run, not the crashed attempt's.
    let (executed_platform, executed_model) = if node.config.get("crew").is_some() {
        executed_pair_for_node(node)
    } else {
        db.get_graph_run(crashed_run_id)
            .ok()
            .flatten()
            .map(|run| (run.executed_platform, run.executed_model))
            .filter(|(platform, _)| platform.is_some())
            .unwrap_or_else(|| executed_pair_for_node(node))
    };
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.clone(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: previous_output.cloned(),
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration,
        pid: None,
        boot_id: crate::system::boot_id(),
        session_id: None,
        executed_platform,
        executed_model,
    })?;
    Ok(run_id)
}
