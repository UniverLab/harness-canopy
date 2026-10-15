//! CM47: spec notes — one working-memory note per spec owner.
//!
//! A node writes its note while it works; every later node, model and
//! iteration of the same spec receives the notes in its prompt. Ownership is
//! enforced by construction: the tool takes no owner argument, so a node can
//! only write its own note. When the writer is an ensemble member, the owner
//! is the ensemble, so every member shares the one note and a replacing
//! member continues where the cut one left off.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{resolve_prefix_or_error, validate_spec_exists};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::graphs::notes::{NewSpecNote, SPEC_NOTE_MAX_CHARS};
use crate::db::Database;
use crate::domain::activity::{self, GraphEvent, GraphEventKind};
use crate::domain::graphs::GraphRunStatus;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

/// CM47: who owns the note a node writes. Shared with the prompt-injection
/// path so the two can never disagree about ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecOwner {
    pub kind: &'static str,
    pub id: String,
    pub name: String,
}

/// Resolve the note owner for a node that is currently running. If the node
/// is an ensemble member, the owner is the ensemble (so every member of one
/// ensemble shares the one note); otherwise it is the node itself.
pub(crate) fn resolve_owner(db: &Database, node_id: &str) -> anyhow::Result<Option<SpecOwner>> {
    let Some(node) = db.get_graph_node(node_id)? else {
        return Ok(None);
    };
    if let Some(details) = db.get_ensemble_by_member_node(node_id)? {
        return Ok(Some(SpecOwner {
            kind: "ensemble",
            id: details.ensemble.id,
            name: details.ensemble.name,
        }));
    }
    Ok(Some(SpecOwner {
        kind: "node",
        id: node.id,
        name: node.name,
    }))
}

fn note_body_len(body: &str) -> usize {
    body.chars().count()
}

fn check_note_body(body: &str) -> Result<(), String> {
    let len = note_body_len(body);
    if len > SPEC_NOTE_MAX_CHARS {
        return Err(format!(
            "Note body is {len} characters; the limit is {SPEC_NOTE_MAX_CHARS} characters."
        ));
    }
    Ok(())
}

/// Record the CM46 `note` event for a write. Skipped when no graph can be
/// resolved (an `operator` note on a standalone spec): `graph_events` needs a
/// `graph_id`, and a notes write is never lost to an event-write failure.
fn record_note_event(
    db: &Database,
    graph_id: Option<&str>,
    spec_id: &str,
    node_id: Option<&str>,
    run_id: Option<&str>,
    owner_name: &str,
    chars: usize,
) {
    let Some(graph_id) = graph_id else {
        return;
    };
    let Ok(Some(graph)) = db.get_graph(graph_id) else {
        return;
    };
    activity::record(
        db,
        GraphEvent {
            graph_id,
            graph_name: &graph.name,
            workdir: &graph.workdir,
            spec_id: Some(spec_id),
            node_id,
            run_id,
            kind: GraphEventKind::Note,
            message: format!("{owner_name} note updated ({chars} chars)"),
            payload: Some(serde_json::json!({"owner_name": owner_name, "chars": chars})),
        },
    );
}

#[tool_router(router = tool_router_notes, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "spec_note_write",
        description = "Write or replace the caller's note for the current spec. When called with a run_id, the owner is resolved from the active run (the node it belongs to, or that node's ensemble if it is an ensemble member, so every member of one ensemble shares one note). It refuses a run_id that is no longer running. When called with a spec_id and no run_id, it writes the `operator` note. The body must be at most 2 500 characters."
    )]
    pub(crate) async fn spec_note_write(
        &self,
        Parameters(params): Parameters<SpecNoteWriteParams>,
    ) -> Result<CallToolResult, McpError> {
        match (params.run_id.as_deref(), params.spec_id.as_deref()) {
            (Some(_), Some(_)) => {
                return Ok(error_result("Pass exactly one of run_id or spec_id."));
            }
            (None, None) => {
                return Ok(error_result(
                    "Pass exactly one of run_id (your note) or spec_id (the operator note).",
                ));
            }
            (Some(run_id), None) => {
                spec_note_write_from_run(&self.db, run_id, &params.body).map_err(internal_error)
            }
            (None, Some(spec_id)) => {
                spec_note_write_operator(&self.db, spec_id, &params.body).map_err(internal_error)
            }
        }
    }

    #[tool(
        name = "spec_notes_get",
        description = "Read every note of a spec — the working-memory notes owner nodes wrote for each other — with owner, author model, updated_at and body."
    )]
    pub(crate) async fn spec_notes_get(
        &self,
        Parameters(params): Parameters<SpecNotesGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        if let Err(e) = validate_spec_exists(&self.db, &spec_id) {
            return Ok(error_result(&e));
        }
        let notes = self.db.spec_notes_get(&spec_id).map_err(internal_error)?;
        let body: Vec<serde_json::Value> = notes
            .iter()
            .map(|note| {
                serde_json::json!({
                    "owner_kind": note.owner_kind,
                    "owner_id": note.owner_id,
                    "owner_name": note.owner_name,
                    "body": note.body,
                    "author_platform": note.author_platform,
                    "author_model": note.author_model,
                    "updated_at": note.updated_at,
                })
            })
            .collect();
        Ok(success_result(
            &serde_json::to_string_pretty(&body).unwrap_or_default(),
        ))
    }
}

/// CM47: the `run_id` write path — owner resolved from the active run, so a
/// node can only ever write its own note. Free function (not a method) so it
/// is unit-testable with a bare `&Database`.
pub(crate) fn spec_note_write_from_run(
    db: &Database,
    run_id: &str,
    body: &str,
) -> anyhow::Result<CallToolResult> {
    if let Err(e) = check_note_body(body) {
        return Ok(error_result(&e));
    }
    let resolved = match db.resolve_run_id_by_prefix(run_id.trim())? {
        Some(id) => id,
        None => run_id.trim().to_string(),
    };
    let Some(run) = db.get_graph_run(&resolved)? else {
        return Ok(error_result(&format!(
            "No graph run found with id '{resolved}'."
        )));
    };
    if run.status != GraphRunStatus::Running {
        return Ok(error_result(&format!(
            "Run '{resolved}' is no longer active; the note was not written."
        )));
    }
    let Some(owner) = resolve_owner(db, &run.node_id)? else {
        return Ok(error_result(&format!(
            "Node '{}' not found; the note was not written.",
            run.node_id
        )));
    };
    let chars = note_body_len(body);
    db.spec_note_write(NewSpecNote {
        spec_id: &run.spec_id,
        owner_kind: owner.kind,
        owner_id: &owner.id,
        owner_name: &owner.name,
        body,
        author_platform: run.executed_platform.as_deref(),
        author_model: run.executed_model.as_deref(),
    })?;
    record_note_event(
        db,
        Some(&run.graph_id),
        &run.spec_id,
        Some(&run.node_id),
        Some(&run.id),
        &owner.name,
        chars,
    );
    Ok(success_result(&format!(
        "Note written for '{}' ({chars} chars).",
        owner.name
    )))
}

/// CM47: the `spec_id` write path — the `operator` note, writable without a
/// run. Free function so it is unit-testable with a bare `&Database`.
pub(crate) fn spec_note_write_operator(
    db: &Database,
    spec_id: &str,
    body: &str,
) -> anyhow::Result<CallToolResult> {
    if let Err(e) = check_note_body(body) {
        return Ok(error_result(&e));
    }
    let resolved = match resolve_prefix_or_error(
        db,
        spec_id.trim(),
        Database::resolve_spec_id_by_prefix,
        "spec",
    ) {
        Ok(id) => id,
        Err(e) => return Ok(e),
    };
    let spec = match validate_spec_exists(db, &resolved) {
        Ok(spec) => spec,
        Err(e) => return Ok(error_result(&e)),
    };
    let chars = note_body_len(body);
    db.spec_note_write(NewSpecNote {
        spec_id: &resolved,
        owner_kind: "operator",
        owner_id: "operator",
        owner_name: "operator",
        body,
        author_platform: None,
        author_model: None,
    })?;
    record_note_event(
        db,
        spec.graph_id.as_deref(),
        &resolved,
        None,
        None,
        "Operator",
        chars,
    );
    Ok(success_result(&format!(
        "Operator note written ({chars} chars)."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{
        Ensemble, EnsembleKind, EnsembleMember, GraphEdgeCondition, GraphNode, GraphNodeKind,
        GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
    };

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn seed_graph(db: &Database) {
        use crate::domain::graphs::{Graph, GraphStatus};
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "g".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
    }

    fn spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 0,
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
        }
    }

    fn agent_node(id: &str, spec_id: &str, name: &str) -> GraphNode {
        agent_node_at(id, spec_id, name, 0)
    }

    fn agent_node_at(id: &str, spec_id: &str, name: &str, position: i64) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position,
            created_at: chrono::Utc::now(),
        }
    }

    fn running_run(id: &str, spec_id: &str, node_id: &str) -> GraphNodeRun {
        GraphNodeRun {
            id: id.to_string(),
            graph_id: "g".to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: Some("claude".to_string()),
            executed_model: Some("opus".to_string()),
        }
    }

    fn result_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn plain_node_owns_its_note() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        db.insert_graph_node(&agent_node("designer", "spec-1", "Designer"))
            .unwrap();
        db.insert_graph_run(&running_run("run-1", "spec-1", "designer"))
            .unwrap();

        let owner = resolve_owner(&db, "designer").unwrap().unwrap();
        assert_eq!(owner.kind, "node");
        assert_eq!(owner.id, "designer");
        assert_eq!(owner.name, "Designer");
    }

    /// Both members of one ensemble resolve to the ensemble owner — so the
    /// second member's write lands in the first member's row, and the next
    /// member's prompt shows that note under the ensemble's name.
    #[test]
    fn ensemble_members_share_one_owner() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        // Nodes go in through `insert_ensemble_unit` (it owns the member +
        // join rows); entry/on_pass targets must exist as rows for the FKs.
        db.insert_graph_node(&agent_node_at("entry", "spec-1", "Entry", 0))
            .unwrap();
        let member = |node_id: &str, position: i64| EnsembleMember {
            ensemble_id: "ens-1".to_string(),
            node_id: node_id.to_string(),
            position,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        db.insert_ensemble_unit(
            &Ensemble {
                id: "ens-1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Implementer".to_string(),
                prompt_template: "{{spec_content}}".to_string(),
                join_node_id: "join".to_string(),
                entry_from_node: "entry".to_string(),
                entry_condition: GraphEdgeCondition::Always,
                min_pass: 1,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: 30,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: "entry".to_string(),
                on_fail_to: None,
                kind: EnsembleKind::RoundRobin,
                round_robin_index: Some(0),
                commit_rights: false,
                created_at: chrono::Utc::now(),

                crew_id: None,
                crew_name: None,
                crew_version_applied: None,
            },
            &[member("m1", 0), member("m2", 1)],
            &[
                agent_node_at("m1", "spec-1", "Member 1", 1),
                agent_node_at("m2", "spec-1", "Member 2", 2),
            ],
            &{
                let mut join = agent_node_at("join", "spec-1", "Join", 3);
                join.kind = GraphNodeKind::Join;
                join
            },
            &[],
        )
        .unwrap();

        db.insert_graph_run(&running_run("run-m1", "spec-1", "m1"))
            .unwrap();
        let first = spec_note_write_from_run(&db, "run-m1", "halfway: schema done").unwrap();
        assert!(
            !first.is_error.unwrap_or(false),
            "first write: {}",
            result_text(&first)
        );

        // Cut member one, start member two: its prompt must show the note
        // under the ENSEMBLE name, not the departed member's.
        db.insert_graph_run(&running_run("run-m2", "spec-1", "m2"))
            .unwrap();
        let owner = resolve_owner(&db, "m2").unwrap().unwrap();
        assert_eq!((owner.kind, owner.id.as_str()), ("ensemble", "ens-1"));
        assert_eq!(owner.name, "Implementer");
        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].owner_name, "Implementer");
        assert_eq!(notes[0].body, "halfway: schema done");
    }

    /// The implementer cannot overwrite the Designer's note: its write lands
    /// in its own owner row and the Designer's row is untouched.
    #[test]
    fn implementer_write_leaves_designer_note_untouched() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        db.insert_graph_node(&agent_node("designer", "spec-1", "Designer"))
            .unwrap();
        db.insert_graph_node(&agent_node_at("impl", "spec-1", "Implementer", 1))
            .unwrap();
        db.insert_graph_run(&running_run("run-designer", "spec-1", "designer"))
            .unwrap();
        db.insert_graph_run(&running_run("run-impl", "spec-1", "impl"))
            .unwrap();
        spec_note_write_from_run(&db, "run-designer", "the plan").unwrap();
        spec_note_write_from_run(&db, "run-impl", "built it").unwrap();

        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 2);
        let designer = notes.iter().find(|n| n.owner_id == "designer").unwrap();
        assert_eq!(designer.body, "the plan");
        let impl_note = notes.iter().find(|n| n.owner_id == "impl").unwrap();
        assert_eq!(impl_note.body, "built it");
    }

    #[test]
    fn write_with_2501_chars_is_refused_and_writes_nothing() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        db.insert_graph_node(&agent_node_at("impl", "spec-1", "Implementer", 1))
            .unwrap();
        db.insert_graph_run(&running_run("run-1", "spec-1", "impl"))
            .unwrap();

        let result = spec_note_write_from_run(&db, "run-1", &"x".repeat(2501)).unwrap();
        assert!(result.is_error.unwrap_or(false));
        assert!(result_text(&result).contains("2500"));
        assert!(db.spec_notes_get("spec-1").unwrap().is_empty());
    }

    #[test]
    fn write_with_finished_run_id_is_refused() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        db.insert_graph_node(&agent_node_at("impl", "spec-1", "Implementer", 1))
            .unwrap();
        db.insert_graph_run(&running_run("run-1", "spec-1", "impl"))
            .unwrap();
        db.update_graph_run_result(
            "run-1",
            GraphRunStatus::Pass,
            None,
            Some(chrono::Utc::now()),
        )
        .unwrap();

        let result = spec_note_write_from_run(&db, "run-1", "late note").unwrap();
        assert!(result.is_error.unwrap_or(false));
        assert!(result_text(&result).contains("no longer active"));
        assert!(db.spec_notes_get("spec-1").unwrap().is_empty());
    }

    #[test]
    fn operator_write_needs_no_run() {
        let db = test_db();
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        let result = spec_note_write_operator(&db, "spec-1", "operator context").unwrap();
        assert!(
            !result.is_error.unwrap_or(false),
            "operator write: {}",
            result_text(&result)
        );
        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].owner_kind, "operator");
        assert_eq!(notes[0].body, "operator context");
    }
}
