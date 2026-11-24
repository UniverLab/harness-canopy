//! Tests for [`quoted_marker_error`](super::quoted_marker_error): the
//! scanner contract, the exact refusal text, and every call site that
//! returns it.

use super::{quoted_marker_error, ENV_HINT};
use crate::graph_engine::check_render::{
    check_command_env_vars, check_command_marker_error, render_check_command, CheckCommandCtx,
    CHECK_COMMAND_QUOTED_MARKERS,
};

fn double_quoted(name: &str) -> String {
    format!("echo \"{{{{{name}}}}}\"")
}

fn expected_message(name: &str, inside: &str, var: Option<&str>) -> String {
    let mut message = format!(
        "marker {{{{{name}}}}} sits inside {inside}; markers are inserted as \
         single-quoted words, so use it bare or inside single quotes"
    );
    if let Some(var) = var {
        message.push_str(&format!(", or read ${var} instead"));
    }
    message
}

#[test]
fn bare_marker_is_valid() {
    assert_eq!(quoted_marker_error("echo {{spec_name}}"), None);
}

#[test]
fn single_quoted_marker_is_valid() {
    assert_eq!(quoted_marker_error("echo '{{spec_name}}'"), None);
}

#[test]
fn double_quoted_marker_is_refused_with_exact_message() {
    assert_eq!(
        quoted_marker_error(&double_quoted("spec_name")),
        Some(expected_message(
            "spec_name",
            "double quotes",
            Some("CANOPY_SPEC_NAME")
        ))
    );
}

#[test]
fn backtick_marker_is_refused_with_exact_message() {
    assert_eq!(
        quoted_marker_error("echo `{{spec_name}}`"),
        Some(expected_message(
            "spec_name",
            "backticks",
            Some("CANOPY_SPEC_NAME")
        ))
    );
}

#[test]
fn bare_marker_in_subst_inside_double_quotes_is_valid() {
    assert_eq!(quoted_marker_error("echo \"$(echo {{spec_name}})\""), None);
}

#[test]
fn double_quoted_marker_in_subst_inside_double_quotes_is_refused() {
    assert_eq!(
        quoted_marker_error("echo \"$(echo \"{{spec_name}}\")\""),
        Some(expected_message(
            "spec_name",
            "double quotes",
            Some("CANOPY_SPEC_NAME")
        ))
    );
}

#[test]
fn apostrophe_inside_double_quotes_does_not_hide_marker() {
    assert_eq!(
        quoted_marker_error("echo \"it's {{spec_name}}\""),
        Some(expected_message(
            "spec_name",
            "double quotes",
            Some("CANOPY_SPEC_NAME")
        ))
    );
}

#[test]
fn dollar_single_quote_inside_double_quotes_does_not_hide_marker() {
    assert_eq!(
        quoted_marker_error("echo \"$'{{spec_name}}'\""),
        Some(expected_message(
            "spec_name",
            "double quotes",
            Some("CANOPY_SPEC_NAME")
        ))
    );
}

#[test]
fn escaped_quote_does_not_open() {
    assert_eq!(quoted_marker_error("echo \\\"{{spec_name}}"), None);
}

#[test]
fn marker_after_closed_double_quoted_string_is_valid() {
    assert_eq!(quoted_marker_error("echo \"foo\" {{spec_name}}"), None);
}

#[test]
fn escaped_marker_is_valid() {
    assert_eq!(quoted_marker_error("echo \\{{spec_name}}"), None);
}

#[test]
fn raw_marker_in_double_quotes_is_valid() {
    assert_eq!(quoted_marker_error("echo \"{{spec_start_head}}\""), None);
}

#[test]
fn env_variable_use_is_valid() {
    assert_eq!(quoted_marker_error("echo \"$CANOPY_SPEC_NAME\""), None);
}

#[test]
fn workdir_message_has_no_env_hint() {
    assert_eq!(
        quoted_marker_error(&double_quoted("workdir")),
        Some(expected_message("workdir", "double quotes", None))
    );
}

#[test]
fn every_quoted_marker_rejected_in_double_quotes() {
    for name in CHECK_COMMAND_QUOTED_MARKERS {
        assert!(
            quoted_marker_error(&double_quoted(name)).is_some(),
            "{name} in double quotes must be refused"
        );
    }
}

#[test]
fn every_quoted_marker_valid_bare_and_single_quoted() {
    for name in CHECK_COMMAND_QUOTED_MARKERS {
        let bare = format!("echo {{{{{name}}}}}");
        let single = format!("echo '{{{{{name}}}}}'");
        assert_eq!(quoted_marker_error(&bare), None, "{name} bare");
        assert_eq!(quoted_marker_error(&single), None, "{name} single-quoted");
    }
}

#[test]
fn env_hint_table_matches_env_vars() {
    assert_eq!(ENV_HINT.len(), CHECK_COMMAND_QUOTED_MARKERS.len());
    for name in CHECK_COMMAND_QUOTED_MARKERS {
        assert!(
            ENV_HINT.iter().any(|(marker, _)| marker == name),
            "{name} missing from the env hint table"
        );
    }
    let vars = check_command_env_vars(&ctx("n"), "/s.md", "/n.md");
    let keys: Vec<&str> = vars.iter().map(|(key, _)| key.as_str()).collect();
    let mut hinted = 0;
    for (_, var) in ENV_HINT {
        if let Some(var) = var {
            assert!(
                keys.contains(var),
                "{var} missing from check_command_env_vars"
            );
            hinted += 1;
        }
    }
    assert_eq!(hinted, 7);
}

#[test]
fn node_validator_refuses_on_save_with_bare_message() {
    use crate::domain::graphs::GraphNodeKind;
    let config = serde_json::json!({ "command": double_quoted("spec_name") });
    let err = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Check, &config)
        .unwrap_err();
    assert_eq!(
        err,
        expected_message("spec_name", "double quotes", Some("CANOPY_SPEC_NAME"))
    );
}

#[test]
fn node_validator_accepts_bare_marker_on_save() {
    use crate::domain::graphs::GraphNodeKind;
    let config = serde_json::json!({ "command": "echo {{spec_name}}" });
    crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Check, &config).unwrap();
}

#[test]
fn import_refuses_with_node_name() {
    use crate::domain::graph_transfer::{GraphExportDocument, GraphExportNode};
    use crate::domain::graphs::GraphNodeKind;
    let document = GraphExportDocument {
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![GraphExportNode {
            name: "bad".to_string(),
            kind: GraphNodeKind::Check,
            position: 1,
            config: serde_json::json!({ "command": double_quoted("spec_name") }),
        }],
        edges: vec![],
        ensembles: vec![],
        infra_node: None,
        hooks: Default::default(),
        prompt_presets: vec![],
        enabled: true,
        enable_at: None,
        expires_at: None,
    };
    let err = crate::daemon::handler_transfer::check_import_node_configs(&document).unwrap_err();
    assert_eq!(
        err,
        format!(
            "node 'bad': {}",
            expected_message("spec_name", "double quotes", Some("CANOPY_SPEC_NAME"))
        )
    );
}

#[test]
fn graph_validate_reports_problem() {
    use crate::domain::graphs::GraphNodeKind;
    let config = serde_json::json!({ "command": double_quoted("spec_name") });
    let mut problems = Vec::new();
    crate::daemon::graph_validate::validate_node_config(
        GraphNodeKind::Check,
        &config,
        &mut problems,
    );
    let value = serde_json::to_value(&problems).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 1);
    assert_eq!(value[0]["path"], "config");
    assert_eq!(
        value[0]["message"],
        expected_message("spec_name", "double quotes", Some("CANOPY_SPEC_NAME")).as_str()
    );
}

#[test]
fn render_refuses_at_run_with_bare_message() {
    let err = render_check_command(&double_quoted("spec_name"), &ctx("a b")).unwrap_err();
    assert_eq!(
        err.to_string(),
        expected_message("spec_name", "double quotes", Some("CANOPY_SPEC_NAME"))
    );
}

#[test]
fn preflight_reports_spec_node_with_suffix() {
    use crate::domain::graphs::{
        Graph, GraphDetails, GraphNode, GraphNodeKind, GraphSpec, GraphSpecDetails,
        GraphSpecStatus, GraphStatus, SpecReviewState,
    };
    let node = GraphNode {
        id: "n".to_string(),
        spec_id: None,
        graph_id: Some("g".to_string()),
        name: "spec-gate".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": double_quoted("spec_name") }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let details = GraphDetails {
        lp: Graph {
            trigger_prompt: None,
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "g".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/w".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        },
        graph_nodes: vec![],
        graph_edges: vec![],
        specs: vec![GraphSpecDetails {
            spec: GraphSpec {
                intent: None,
                id: "s".to_string(),
                graph_id: Some("g".to_string()),
                name: "Spec".to_string(),
                description: None,
                position: 1,
                parallelizable: false,
                status: GraphSpecStatus::Pending,
                started_at: None,
                completed_at: None,
                spec_start_head: None,
                spec_start_dirty: None,
                spec_end_dirty: None,
                spec_end_dirty_paths: None,
                spec_committed_head: None,
                workdir: None,
                completed_via: None,
                completed_via_reason: None,
                completed_via_at: None,
                spec_code: None,
                spec_scope: None,
                spec_type: None,
                review_state: SpecReviewState::Approved,
                review_tier: None,
                review_findings: None,
                created_by_session: None,
            },
            nodes: vec![node],
            edges: vec![],
        }],
        completion_hook_runs: vec![],
    };
    assert_eq!(
        check_command_marker_error(&details).unwrap(),
        format!(
            "{} (check node 'spec-gate').",
            expected_message("spec_name", "double quotes", Some("CANOPY_SPEC_NAME"))
        )
    );
}

fn ctx(name: &str) -> CheckCommandCtx<'_> {
    CheckCommandCtx {
        graph_id: "g",
        graph_name: "Graph",
        workdir: "/w",
        spec_id: "s",
        spec_name: name,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
        spec_commit_message: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_committed_head: None,
    }
}
