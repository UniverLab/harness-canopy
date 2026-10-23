use super::*;

/// Read a router node's `routes` + `fallback` straight from its `config`.
/// The shape (`2`-`8` unique-labeled routes, a fallback naming one of them)
/// is already enforced at `graph_add_node`/`graph_update_node` time (see
/// `daemon::handler::validate_node_config`'s `Router` arm) — this only
/// defends against that guard somehow having been bypassed, so it bails with
/// a generic engine error rather than re-deriving the MCP layer's messages.
pub(crate) fn parse_router_config(node: &GraphNode) -> Result<(Vec<RouterRoute>, String)> {
    let map = node
        .config
        .as_object()
        .ok_or_else(|| anyhow!("Router node '{}' has a non-object config.", node.name))?;
    let routes = map
        .get("routes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "Router node '{}' config is missing a 'routes' array.",
                node.name
            )
        })?
        .iter()
        .map(|entry| RouterRoute {
            label: entry
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect::<Vec<_>>();
    let fallback = map
        .get("fallback")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!(
                "Router node '{}' config is missing a 'fallback' label.",
                node.name
            )
        })?
        .to_string();
    Ok((routes, fallback))
}

/// Compose a router node's one-shot prompt: its input (the previous node's
/// output, bounded the same way [`render_agent_prompt`] bounds
/// `{{previous_feedback}}`) plus its declared routes with descriptions and a
/// hard instruction to answer with exactly one route label and nothing else.
/// Deliberately carries none of `render_agent_prompt`'s
/// `graph_complete_node`/`graph_report_blocker` reporting contract — a router
/// never self-reports; its whole answer is read straight from process
/// stdout by [`match_router_token`].
pub(crate) fn render_router_prompt(
    previous_output: Option<&Value>,
    routes: &[RouterRoute],
) -> String {
    let input = previous_output
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(none)".to_string());
    let input = bound_previous_feedback(input);
    let route_list = routes
        .iter()
        .map(|route| format!("- {}: {}", route.label, route.description))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# [INPUT]\n{input}\n\n# [ROUTES]\nPick exactly one route below and answer with ONLY its label — no punctuation, no explanation, nothing before or after it.\n\n{route_list}\n"
    )
}

/// Strict single-token match of a router's raw answer against its declared
/// routes: trim the whole answer and compare it for exact equality against
/// each label — never a substring/`contains` check (that's exactly what
/// would let a bare route word inside narration silently trigger a route).
/// `None` means the raw answer didn't cleanly name a declared route, so the
/// caller falls back to the node's declared fallback route.
pub(crate) fn match_router_token<'a>(
    raw_answer: &str,
    routes: &'a [RouterRoute],
) -> Option<&'a str> {
    let token = raw_answer.trim();
    routes
        .iter()
        .find(|route| route.label == token)
        .map(|route| route.label.as_str())
}

/// Read a router node's `decision` flag and `min_confidence` from its config.
/// Returns `(decision, min_confidence)`. `decision` defaults to `false`,
/// `min_confidence` to `0.0`. An out-of-range or non-numeric `min_confidence`
/// is an error naming the node and field.
pub(crate) fn parse_router_decision(node: &GraphNode) -> Result<(bool, f64)> {
    let map = node
        .config
        .as_object()
        .ok_or_else(|| anyhow!("Router node '{}' has a non-object config.", node.name))?;
    let decision = map
        .get("decision")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let min_confidence = match map.get("min_confidence") {
        None => 0.0,
        Some(value) => {
            let n = value.as_f64().ok_or_else(|| {
                anyhow!(
                    "Router node '{}' config field 'min_confidence' must be a number from 0 to 1.",
                    node.name
                )
            })?;
            if !(0.0..=1.0).contains(&n) || n.is_nan() {
                bail!(
                    "Router node '{}' config field 'min_confidence' must be a number from 0 to 1.",
                    node.name
                );
            }
            n
        }
    };
    Ok((decision, min_confidence))
}

/// Execute a router node (M2): spawn the configured platform/model exactly
/// like an agent node's cold start ([`execute_agent_node`]), then read its
/// one-shot answer straight from stdout — a router never self-reports via
/// `graph_complete_node`, so [`self_reported_execution`] never applies here,
/// and it never resumes a prior session (there is nothing to continue: each
/// visit is an independent classification).
///
/// When `decision: true`, the node uses the platform's decision-model
/// invocation instead of spawning a full agent — see `execute_decision_router_node`.
///
/// A spawn failure or timeout is a node failure like any other node's —
/// `run_agent_process`'s own `Fail` verdict is returned unchanged, and the
/// caller in `run_spec` routes it through the graph's ordinary fail edge
/// (see the `select_router_step` vs. `select_next_step` branch there). Any
/// run that actually finished instead always resolves `Pass` with a chosen
/// route: the raw answer matched against a declared label if it is exactly
/// one, or the node's declared fallback — logged either way (chosen route +
/// raw answer) so an operator can see what the model actually said, per
/// B43's node-run lifecycle logging.
pub(crate) async fn execute_router_node(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    previous_output: Option<&Value>,
    run_id: &str,
    workdir: &str,
) -> Result<NodeExecution> {
    let (routes, fallback) = parse_router_config(node)?;
    let (decision, min_confidence) = parse_router_decision(node)?;

    if !decision {
        return execute_agent_router_node(
            db,
            &routes,
            &fallback,
            node,
            previous_output,
            run_id,
            workdir,
        )
        .await;
    }

    execute_decision_router_node(
        db,
        lp,
        spec,
        node,
        &routes,
        &fallback,
        min_confidence,
        previous_output,
        run_id,
        workdir,
    )
    .await
}

/// The classic agent-mode router path — unchanged byte-for-byte.
async fn execute_agent_router_node(
    db: &Database,
    routes: &[RouterRoute],
    fallback: &str,
    node: &GraphNode,
    previous_output: Option<&Value>,
    run_id: &str,
    workdir: &str,
) -> Result<NodeExecution> {
    let cli_name = node
        .config
        .get("platform")
        .or_else(|| node.config.get("cli"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Router node '{}' is missing a platform/cli.", node.name))?;
    let cli = Cli::resolve(Some(cli_name)).map_err(anyhow::Error::msg)?;
    let model = node.config.get("model").and_then(Value::as_str);
    let timeout_minutes = node
        .config
        .get("timeout_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    let base_strategy = cli.strategy();

    let prompt = render_router_prompt(previous_output, routes);
    let strategy = sized_strategy(&base_strategy, &prompt);

    let execution = run_agent_process(
        db,
        run_id,
        &cli,
        &strategy,
        node,
        &prompt,
        model,
        None,
        workdir,
        timeout_minutes,
        None,
    )
    .await?;

    if execution.status != GraphRunStatus::Pass {
        return Ok(execution);
    }

    let raw_answer = execution
        .output
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (chosen_route, used_fallback) = match match_router_token(&raw_answer, routes) {
        Some(label) => (label.to_string(), false),
        None => (fallback.to_string(), true),
    };

    tracing::info!(
        run_id,
        node = %node.name,
        route = %chosen_route,
        raw_answer = %raw_answer,
        used_fallback,
        "router node decided route"
    );

    Ok(NodeExecution {
        status: GraphRunStatus::Pass,
        output: serde_json::json!({
            "kind": "router",
            "node_id": node.id,
            "cli": cli.as_str(),
            "model": model,
            "raw_answer": raw_answer,
            "route": chosen_route,
            "used_fallback": used_fallback,
        }),
        summary: format!(
            "Router node '{}' selected route '{}'{}.",
            node.name,
            chosen_route,
            if used_fallback { " (fallback)" } else { "" }
        ),
    })
}

/// The decision-mode router path (CB88): classify via the platform's decision
/// model instead of spawning a full agent.
#[expect(clippy::too_many_arguments)]
async fn execute_decision_router_node(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    routes: &[RouterRoute],
    fallback: &str,
    min_confidence: f64,
    previous_output: Option<&Value>,
    run_id: &str,
    workdir: &str,
) -> Result<NodeExecution> {
    let cli_name = node
        .config
        .get("platform")
        .or_else(|| node.config.get("cli"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Router node '{}' is missing a platform/cli.", node.name))?;
    let cli = Cli::resolve(Some(cli_name)).map_err(anyhow::Error::msg)?;
    let model = node.config.get("model").and_then(Value::as_str);
    let timeout_minutes = node
        .config
        .get("timeout_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    let base_strategy = cli.strategy();

    if !decision_invocation_available(&base_strategy) {
        bail!(
            "Router node '{}' uses decision mode but platform '{}' has no decision-model invocation in the registry.",
            node.name,
            cli.as_str()
        );
    }

    let routing = DecisionRouting {
        routes,
        fallback,
        min_confidence,
    };
    let state = match render_decision_state(lp, spec, node, previous_output) {
        Ok(s) => s,
        Err(e) => return Ok(decision_template_failure(node, &cli, model, &e)),
    };

    let instructions = decision_instructions(node);

    let request = build_decision_request(&state, instructions, routing.routes);

    let outcome = run_decision_process(
        db,
        run_id,
        &base_strategy,
        model,
        &request,
        workdir,
        timeout_minutes,
    )
    .await;

    match outcome {
        Err(error) => {
            let mut exec = agent_spawn_failure(node, &cli, model, &error);
            exec.output["mode"] = Value::String("decision".to_string());
            Ok(exec)
        }
        Ok(CliProcessOutcome::TimedOut) => Ok(finish_decision_failure(
            db,
            run_id,
            serde_json::json!({
                "kind": "router",
                "mode": "decision",
                "node_id": node.id,
                "cli": cli.as_str(),
                "model": model,
                "error": "timed out",
                "timeout_minutes": timeout_minutes,
            }),
            format!(
                "Router node '{}' decision timed out after {timeout_minutes}m.",
                node.name
            ),
        )),
        Ok(CliProcessOutcome::Finished {
            exit_code,
            stdout,
            stderr,
        }) => {
            if exit_code != 0 {
                return Ok(finish_decision_failure(
                    db,
                    run_id,
                    serde_json::json!({
                        "kind": "router",
                        "mode": "decision",
                        "node_id": node.id,
                        "cli": cli.as_str(),
                        "model": model,
                        "exit_code": exit_code,
                        "stdout": stdout,
                        "stderr": stderr,
                    }),
                    format!(
                        "Router node '{}' decision process exited with code {exit_code}.",
                        node.name
                    ),
                ));
            }
            Ok(decision_success_execution(
                run_id, node, &cli, model, &routing, &stdout,
            ))
        }
    }
}

/// The routing the decision-mode router was configured with.
struct DecisionRouting<'a> {
    routes: &'a [RouterRoute],
    fallback: &'a str,
    min_confidence: f64,
}

/// The router node's decision instructions: its `prompt_template` when set,
/// else the default.
fn decision_instructions(node: &GraphNode) -> &str {
    node.config
        .get("prompt_template")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DECISION_DEFAULT_INSTRUCTIONS)
}

/// The node execution for a decision template that could not be rendered.
fn decision_template_failure(
    node: &GraphNode,
    cli: &Cli,
    model: Option<&str>,
    error: &anyhow::Error,
) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "router",
            "mode": "decision",
            "node_id": node.id,
            "cli": cli.as_str(),
            "model": model,
            "error": error.to_string(),
        }),
        summary: format!(
            "Router node '{}' decision template failed: {}",
            node.name, error
        ),
    }
}

/// Record a decision-mode router failure on the run row and build its
/// execution.
fn finish_decision_failure(
    db: &Database,
    run_id: &str,
    output: Value,
    summary: String,
) -> NodeExecution {
    let _ = db.update_graph_run_result(
        run_id,
        GraphRunStatus::Fail,
        Some(&output),
        Some(chrono::Utc::now()),
    );
    NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary,
    }
}

/// Build the pass execution for a decision-mode router that ran to completion.
fn decision_success_execution(
    run_id: &str,
    node: &GraphNode,
    cli: &Cli,
    model: Option<&str>,
    routing: &DecisionRouting<'_>,
    stdout: &str,
) -> NodeExecution {
    let (choice, confidence, probabilities, usage) = match parse_decision_answer(stdout) {
        Some(ans) => {
            let choice = ans.choice.clone();
            (Some(choice), ans.confidence, ans.probabilities, ans.usage)
        }
        None => (None, 0.0, HashMap::new(), Value::Null),
    };
    let choice = choice.as_deref();

    let (route, used_fallback) = resolve_decision_route(
        choice,
        confidence,
        routing.routes,
        routing.fallback,
        routing.min_confidence,
    );

    tracing::info!(
        run_id,
        node = %node.name,
        route = %route,
        confidence,
        used_fallback,
        "router node decided route (decision mode)"
    );

    let probabilities_json: serde_json::Map<String, Value> = probabilities
        .into_iter()
        .map(|(k, v)| (k, Value::from(v)))
        .collect();

    NodeExecution {
        status: GraphRunStatus::Pass,
        output: serde_json::json!({
            "kind": "router",
            "mode": "decision",
            "node_id": node.id,
            "cli": cli.as_str(),
            "model": model,
            "route": route,
            "used_fallback": used_fallback,
            "confidence": confidence,
            "probabilities": probabilities_json,
            "usage": usage,
            "raw_answer": stdout,
        }),
        summary: format!(
            "Router node '{}' selected route '{}' (decision, confidence {confidence:.2}){}.",
            node.name,
            route,
            if used_fallback { " (fallback)" } else { "" }
        ),
    }
}

/// Result of one `on_completed` hook firing (N2) — deliberately not a
/// [`NodeExecution`]: the hook belongs to no node, and unlike a node's
/// result, this one must never feed back into the run's routing or final
/// status (the run is already `Completed` by the time this fires).
pub(crate) struct HookExecution {
    pub(crate) status: GraphRunStatus,
    pub(crate) output: Value,
    pub(crate) summary: String,
}

/// Run the `on_completed` hook's process via [`spawn_and_wait_cli_process`] —
/// the same spawn path as [`run_agent_process`], minus the parts that are
/// specific to a graph node run (no `GraphNodeRun` id to route a late report
/// against, no hard-error timeout: a hook failure is always recorded and
/// reported to the caller as data, never propagated as an `Err`, since it
/// must never affect the already-finalized graph run that spawned it).
#[expect(clippy::too_many_arguments)]
pub(crate) async fn run_completion_hook_process(
    db: &Database,
    hook_run_id: &str,
    cli: &Cli,
    strategy: &crate::domain::cli_strategy::CliStrategy,
    prompt: &str,
    model: Option<&str>,
    effort: Option<&str>,
    workdir: &str,
    timeout_minutes: u64,
) -> HookExecution {
    let outcome = spawn_and_wait_cli_process(
        strategy,
        prompt,
        model,
        effort,
        workdir,
        timeout_minutes,
        None,
        None,
        false,
        None,
        |pid| {
            let _ = db.set_graph_completion_hook_run_pid(
                hook_run_id,
                pid as i64,
                crate::system::boot_id().as_deref(),
            );
        },
        None,
        None,  // Hooks don't get verdict monitoring
        false, // Hooks keep the full tool list, not the node profile.
    )
    .await;

    match outcome {
        Err(error) => HookExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({
                "cli": cli.as_str(),
                "model": model,
                "error": error.message,
            }),
            summary: format!("on_completed hook failed to spawn: {}", error.message),
        },
        Ok(CliProcessOutcome::TimedOut) => HookExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({
                "cli": cli.as_str(),
                "model": model,
                "error": "timed out",
                "timeout_minutes": timeout_minutes,
            }),
            summary: format!("on_completed hook timed out after {timeout_minutes}m."),
        },
        Ok(CliProcessOutcome::Finished {
            exit_code,
            stdout,
            stderr,
        }) => HookExecution {
            status: if exit_code == 0 {
                GraphRunStatus::Pass
            } else {
                GraphRunStatus::Fail
            },
            output: serde_json::json!({
                "cli": cli.as_str(),
                "model": model,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
            }),
            summary: format!("on_completed hook exited with code {exit_code}."),
        },
    }
}

pub(crate) fn execute_gate_node(
    node: &GraphNode,
    previous_output: Option<&Value>,
) -> Result<NodeExecution> {
    let previous_output = previous_output
        .ok_or_else(|| anyhow!("Gate node '{}' requires previous node output.", node.name))?;
    let evaluate = node
        .config
        .get("evaluate")
        .and_then(Value::as_str)
        .unwrap_or("output_contains");
    let expected = node
        .config
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let haystack = serde_json::to_string(previous_output)?;

    let passed = match evaluate {
        "output_contains" => haystack.contains(expected),
        other => bail!("Unsupported gate evaluate mode '{}'.", other),
    };

    Ok(NodeExecution {
        status: if passed {
            GraphRunStatus::Pass
        } else {
            GraphRunStatus::Fail
        },
        output: serde_json::json!({
            "kind": "gate",
            "node_id": node.id,
            "evaluate": evaluate,
            "value": expected,
            "passed": passed,
        }),
        summary: format!(
            "Gate node '{}' {}.",
            node.name,
            if passed { "passed" } else { "failed" }
        ),
    })
}

pub(crate) fn evaluate_success_condition(
    condition: &str,
    exit_code: i32,
    output: &str,
) -> Result<bool> {
    if condition == "exit_code_0" {
        return Ok(exit_code == 0);
    }

    if let Some(expected) = condition.strip_prefix("exit_code_0_and_output_contains:") {
        return Ok(exit_code == 0 && output.contains(expected.trim().trim_matches('"')));
    }

    if let Some(expected) = condition.strip_prefix("output_not_contains:") {
        return Ok(!output.contains(expected.trim().trim_matches('"')));
    }

    bail!("Unsupported success condition '{}'.", condition)
}
