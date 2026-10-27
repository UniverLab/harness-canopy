//! Part 13 of `endpoint_tests`: write-time refusal of prompt templates
//! whose placeholder nothing can bind (plus the preflight warning for
//! templates stored before the fix).

use super::*;

async fn part13_graph(handler: &TaskTriggerHandler, workdir: &str, name: &str) -> String {
    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: name.to_string(),
            description: None,
            workdir: workdir.to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    extract_id(&created, "graph_id")
}

async fn part13_agent_node(handler: &TaskTriggerHandler, graph_id: &str, name: &str) -> String {
    let added = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            name: name.to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    extract_id(&added, "node_id")
}

fn part13_member(platform: &str, prompt_override: Option<String>) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

async fn part13_ensemble(
    handler: &TaskTriggerHandler,
    graph_id: &str,
    name: &str,
    from_node: &str,
    on_pass_to: &str,
) -> String {
    let added = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            kind: None,
            name: name.to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                part13_member("openrouter", None),
                part13_member("openrouter", None),
            ]),
            condition: "always".to_string(),
            from_node: from_node.to_string(),
            on_pass_to: on_pass_to.to_string(),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    extract_id(&added, "ensemble_id")
}

/// `graph_add_node` refuses a template whose `{{output:...}}` names no node
/// in the graph — at write time, not member by member at spawn time — and
/// writes nothing.
#[tokio::test]
async fn add_node_refuses_unknown_output() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = part13_graph(&handler, &workdir, "Add Refuses Bad Output").await;
    part13_agent_node(&handler, &graph_id, "alpha").await;

    let mut config = agent_node_config("claude");
    config.insert(
        "prompt_template".to_string(),
        serde_json::Value::String("Follow the plan: {{output:Ghost}}".to_string()),
    );
    let result = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "beta".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "expected a refusal, got: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(msg.contains("beta"), "refusal must name the node: {msg}");
    assert!(
        msg.contains("output:Ghost"),
        "refusal must name the placeholder: {msg}"
    );
    assert!(
        msg.contains("Accepted placeholders"),
        "refusal must name the accepted forms: {msg}"
    );
    let nodes = db.list_graph_nodes_for_graph(&graph_id).unwrap();
    assert_eq!(nodes.len(), 1, "refused write must store nothing: {msg}");
}

/// `graph_add_ensemble` refuses a member `prompt_override` carrying an
/// unknown bare placeholder, and writes nothing.
#[tokio::test]
async fn add_ensemble_refuses_bad_override() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = part13_graph(&handler, &workdir, "Add Ensemble Refuses").await;
    let kickoff_id = part13_agent_node(&handler, &graph_id, "kickoff").await;
    let downstream_id = part13_agent_node(&handler, &graph_id, "downstream").await;

    let result = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                part13_member("openrouter", None),
                part13_member(
                    "openrouter",
                    Some("review with {{bogus_marker}}".to_string()),
                ),
            ]),
            condition: "always".to_string(),
            from_node: kickoff_id,
            on_pass_to: downstream_id,
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "expected a refusal, got: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(
        msg.contains("Proposers") && msg.contains("member 2"),
        "refusal must name the ensemble and member: {msg}"
    );
    assert!(
        msg.contains("bogus_marker"),
        "refusal must name the placeholder: {msg}"
    );
    let ensembles = db.list_ensembles_for_graph(&graph_id).unwrap();
    assert!(
        ensembles.is_empty(),
        "refused write must store nothing: {msg}"
    );
}

/// `graph_update_ensemble` refuses a shared prompt whose `{{output:...}}`
/// names an ensemble: outputs are read from nodes, and the message names the
/// ensemble's quorum node as the intended source. The stored prompt is left
/// untouched.
#[tokio::test]
async fn update_ensemble_refuses_ensemble_output() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = part13_graph(&handler, &workdir, "Update Ensemble Refuses").await;
    let kickoff_id = part13_agent_node(&handler, &graph_id, "kickoff").await;
    let downstream_id = part13_agent_node(&handler, &graph_id, "downstream").await;
    part13_ensemble(&handler, &graph_id, "Designer", &kickoff_id, &downstream_id).await;
    let impl_id = part13_ensemble(&handler, &graph_id, "Impl", &kickoff_id, &downstream_id).await;

    let result = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
            ensemble_id: impl_id.clone(),
            kind: None,
            prompt_template: Some("See {{output:Designer}} first".to_string()),
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "expected a refusal, got: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(
        msg.contains("outputs are read from nodes"),
        "refusal must say outputs come from nodes: {msg}"
    );
    assert!(
        msg.contains("Designer (quorum)"),
        "refusal must name the quorum node: {msg}"
    );
    let details = db.get_ensemble_details(&impl_id).unwrap().unwrap();
    assert_eq!(
        details.ensemble.prompt_template, "draft it",
        "refused write must leave the stored prompt untouched: {msg}"
    );
}

/// `graph_update_node` accepts a template whose only unknown markers are
/// escaped (`\{{...}}`): they are literal text, not binding sites.
#[tokio::test]
async fn update_node_accepts_escaped_placeholder() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = part13_graph(&handler, &workdir, "Update Accepts Escaped").await;
    let alpha_id = part13_agent_node(&handler, &graph_id, "alpha").await;

    let mut config = serde_json::Map::new();
    config.insert(
        "prompt_template".to_string(),
        serde_json::Value::String(
            "Literal \\{{output:Ghost}} and \\{{bogus}} plus {{spec_content}}".to_string(),
        ),
    );
    let result = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: alpha_id,
            name: None,
            kind: None,
            config: Some(config),
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_err(&result),
        "escaped markers must be accepted: {}",
        text(&result)
    );
}

/// `graph_update_node` refuses a template edit carrying an unknown output
/// target — in `prompt_template` and in `resume_prompt`.
#[tokio::test]
async fn update_node_refuses_unknown_output() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = part13_graph(&handler, &workdir, "Update Refuses Bad Output").await;
    let alpha_id = part13_agent_node(&handler, &graph_id, "alpha").await;

    for field in ["prompt_template", "resume_prompt"] {
        let mut config = serde_json::Map::new();
        config.insert(
            field.to_string(),
            serde_json::Value::String("Follow the plan: {{output:Ghost}}".to_string()),
        );
        let result = handler
            .graph_update_node(Parameters(GraphUpdateNodeParams {
                node_id: alpha_id.clone(),
                name: None,
                kind: None,
                config: Some(config),
                config_replace: None,
                position: None,
            }))
            .await
            .unwrap();
        assert!(
            is_err(&result),
            "expected a refusal for {field}, got: {}",
            text(&result)
        );
        let msg = text(&result);
        assert!(
            msg.contains("alpha") && msg.contains("output:Ghost"),
            "refusal must name the node and placeholder: {msg}"
        );
    }
}

/// `graph_import` refuses a document whose node template carries an unknown
/// placeholder, all-or-nothing: no graph is created.
#[tokio::test]
async fn import_refuses_unbindable() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;
    let exported = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: source_graph_id,
        }))
        .await
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
    document["nodes"][0]["config"]["prompt_template"] = serde_json::json!("Do {{bogus_thing}} now");

    let result = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: Some("Bad Import".to_string()),
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "expected a refusal, got: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(
        msg.contains("bogus_thing"),
        "refusal must name the placeholder: {msg}"
    );
    let names: Vec<String> = db
        .list_graphs(None, true)
        .unwrap()
        .into_iter()
        .map(|lp| lp.name)
        .collect();
    assert!(
        !names.iter().any(|n| n == "Bad Import"),
        "refused import must create no graph: {names:?}"
    );
}

/// `graph_preflight` reports a pre-existing template with an unbindable
/// placeholder as a `graph_warnings` entry naming node and placeholder —
/// advisory, not a tool error. The row is inserted straight into the DB:
/// the write path now refuses such a template, so only a graph written
/// before the fix can carry one.
#[tokio::test]
async fn preflight_warns_preexisting_bad() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let mk_node = |name: &str, position: i64, prompt: Option<&str>| GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        // Unconfigured platform -> probe returns NotConfigured instantly,
        // no quota spent, preflight still succeeds.
        config: match prompt {
            Some(p) => serde_json::json!({
                "platform": "cm19-unconfigured-platform",
                "prompt_template": p,
            }),
            None => serde_json::json!({"platform": "cm19-unconfigured-platform"}),
        },
        position,
        created_at: chrono::Utc::now(),
    };
    let alpha = mk_node("alpha", 1, None);
    let beta = mk_node("beta", 2, Some("Do {{bogus_marker}} now"));
    db.insert_graph_node(&alpha).unwrap();
    db.insert_graph_node(&beta).unwrap();

    let mk_edge = |from: &str, to: &str, cond: GraphEdgeCondition| GraphEdge {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition: cond,
    };
    db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Pass))
        .unwrap();
    db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Fail))
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_err(&result),
        "a preflight warning must not be a tool error: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let warnings = body["graph_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include graph_warnings: {body}"));
    assert_eq!(warnings.len(), 1, "expected exactly one warning: {body}");
    assert_eq!(warnings[0]["name"], "beta");
    assert_eq!(warnings[0]["placeholder"], "bogus_marker");
    let warn_text = warnings[0]["warning"].as_str().unwrap_or_default();
    assert!(
        warn_text.contains("beta") && warn_text.contains("bogus_marker"),
        "warning must name the node and placeholder: {body}"
    );
}
