use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};

use serde_json::Value;

use super::*;
use crate::daemon::process::KILL_GRACE;

/// Default instructions when a router node has no `prompt_template`.
pub(crate) const DECISION_DEFAULT_INSTRUCTIONS: &str = "Pick the route that fits.";

/// The `{{...}}` markers [`render_decision_state`] can bind.
pub(crate) const ROUTER_DECISION_BINDINGS: &[&str] = &[
    "previous_feedback",
    "previous_output",
    "spec_content",
    "spec_name",
];

/// Parsed answer from a decision-model invocation.
pub(crate) struct DecisionAnswer {
    pub(crate) choice: String,
    pub(crate) confidence: f64,
    pub(crate) probabilities: HashMap<String, f64>,
    pub(crate) usage: Value,
}

/// Build the jev decision request JSON: `{"state": ..., "questions": {"route": {"type": "choice", "instructions": ..., "criteria": {...}}}}`.
pub(crate) fn build_decision_request(
    state: &str,
    instructions: &str,
    routes: &[RouterRoute],
) -> Value {
    let criteria: serde_json::Map<String, Value> = routes
        .iter()
        .map(|route| {
            (
                route.label.clone(),
                Value::String(route.description.clone()),
            )
        })
        .collect();
    serde_json::json!({
        "state": state,
        "questions": {
            "route": {
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }
        }
    })
}

/// Render the decision node's state text from its template placeholders.
/// When the template is empty, the state is the previous node's output JSON.
pub(crate) fn render_decision_state(
    _lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    previous_output: Option<&Value>,
) -> Result<String> {
    let template = node
        .config
        .get("prompt_template")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("{{previous_output}}");

    refuse_unbindable_template(&node.name, template, ROUTER_DECISION_BINDINGS, None)?;

    let previous_output_json = previous_output
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(none)".to_string());
    let bounded = bound_previous_feedback(previous_output_json);
    let spec_content = spec.description.as_deref().unwrap_or(&spec.name);

    Ok(crate::domain::prompts::render_template(
        template,
        |raw| match raw {
            "previous_feedback" => Some(bounded.clone()),
            "previous_output" => Some(bounded.clone()),
            "spec_content" => Some(spec_content.to_string()),
            "spec_name" => Some(spec.name.clone()),
            _ => None,
        },
    ))
}

/// Parse a decision-model answer from the process stdout. `None` means the
/// answer was unparseable in any way — caller falls back.
pub(crate) fn parse_decision_answer(stdout: &str) -> Option<DecisionAnswer> {
    let parsed: Value = serde_json::from_str(stdout).ok()?;
    let route = parsed.get("answers")?.get("route")?;
    let choice = route.get("choice")?.as_str()?;
    if choice.is_empty() {
        return None;
    }
    let confidence = route.get("confidence")?.as_f64()?;
    if !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    let probabilities = route
        .get("probabilities")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    let usage = parsed.get("usage").cloned().unwrap_or(Value::Null);
    Some(DecisionAnswer {
        choice: choice.to_string(),
        confidence,
        probabilities,
        usage,
    })
}

/// Resolve the route from a decision answer. Returns `(route, used_fallback)`.
pub(crate) fn resolve_decision_route(
    choice: Option<&str>,
    confidence: f64,
    routes: &[RouterRoute],
    fallback: &str,
    min_confidence: f64,
) -> (String, bool) {
    if let Some(label) = choice {
        if confidence >= min_confidence && routes.iter().any(|r| r.label == label) {
            return (label.to_string(), false);
        }
    }
    (fallback.to_string(), true)
}

/// Whether the platform's strategy has a decision-model invocation configured.
pub(crate) fn decision_invocation_available(
    strategy: &crate::domain::cli_strategy::CliStrategy,
) -> bool {
    strategy.decision_args.is_some()
}

/// Build the argv for a decision-model invocation. Returns the binary path and
/// the argument list (excluding the binary itself).
pub(crate) fn build_decision_argv(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    model: Option<&str>,
) -> Result<(std::path::PathBuf, Vec<String>)> {
    let binary = crate::domain::cli_strategy::resolve_binary(&strategy.binary)?;
    let mut argv = Vec::new();
    if let Some(ref args) = strategy.decision_args {
        for arg in shell_words::split(args).unwrap_or_default() {
            argv.push(arg);
        }
    }
    if let Some(m) = model {
        if let Some(flag) = strategy.model_flag.as_deref() {
            if crate::domain::cli_config::model_flag_selects_model(Some(flag)) {
                argv.push(flag.to_string());
                argv.push(m.to_string());
            }
        }
    }
    Ok((binary, argv))
}

/// Run a decision-model process: write the request JSON to a temp file, spawn
/// the platform's CLI with it on stdin, wait for it (or a timeout). Returns the
/// outcome exactly like `spawn_and_wait_cli_process`.
pub(crate) async fn run_decision_process(
    db: &Database,
    run_id: &str,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    model: Option<&str>,
    request_json: &Value,
    workdir: &str,
    timeout_minutes: u64,
) -> Result<CliProcessOutcome, SpawnError> {
    let (binary, argv) =
        build_decision_argv(strategy, model).map_err(|e| SpawnError::from_build(&e))?;

    let mut cmd = tokio::process::Command::new(binary);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    for (key, value) in &strategy.env_vars {
        cmd.env(key, value);
    }
    cmd.current_dir(workdir);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut file = tempfile::tempfile().map_err(|e| SpawnError::transient(e.to_string()))?;
    file.write_all(
        serde_json::to_string(request_json)
            .unwrap_or_default()
            .as_bytes(),
    )
    .map_err(|e| SpawnError::transient(e.to_string()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| SpawnError::transient(e.to_string()))?;
    cmd.stdin(std::process::Stdio::from(file));

    for arg in argv {
        cmd.arg(arg);
    }

    let child = cmd.spawn().map_err(|error| SpawnError::from_io(&error))?;
    let pid = child.id();
    if let Some(pid) = pid {
        let _ = db.set_graph_run_pid(run_id, pid as i64, crate::system::boot_id().as_deref());
    }

    let timeout_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_minutes * 60),
        child.wait_with_output(),
    )
    .await;

    match timeout_result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Ok(CliProcessOutcome::Finished {
                exit_code,
                stdout,
                stderr,
            })
        }
        Ok(Err(error)) => Err(SpawnError::transient(error.to_string())),
        Err(_elapsed) => {
            if let Some(pid) = pid {
                crate::daemon::process::terminate_process_group_async(pid as i64, KILL_GRACE);
            }
            Ok(CliProcessOutcome::TimedOut)
        }
    }
}
