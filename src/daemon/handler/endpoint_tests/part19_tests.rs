//! Part 19 of `endpoint_tests`: Intelligence written/read from inside a
//! sandbox worktree resolves to the project the sandbox was cut from
//! (never to a hash of the ephemeral worktree path).
use super::*;

use crate::domain::project::workdir_hash;
use crate::domain::sandbox::Sandbox;

const WT: &str = "/tmp/sbx-attr-wt/9999aaaa/11112222-sandbox";
const ORIG: &str = "/tmp/sbx-attr-real-project";

fn sandbox_run(id: &str) -> Sandbox {
    Sandbox {
        id: id.to_string(),
        project_hash: workdir_hash(ORIG),
        base_branch: "main".to_string(),
        sandbox_branch: format!("canopy/sandbox-{id}"),
        worktree_path: WT.into(),
        cli_name: "opencode".to_string(),
        original_workdir: ORIG.to_string(),
        created_at: chrono::Utc::now(),
    }
}

/// Registers the session at the raw worktree path (pre-fix behaviour: the
/// sandbox run row is only inserted afterwards, so nothing maps the workdir
/// at registration time). The read-side mapping is therefore what must make
/// these tests pass.
fn register_sandbox_session(db: &Database, agent_id: &str) {
    db.insert_interactive_session(
        agent_id,
        "attr",
        "opencode",
        WT,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_sandbox_run(
        &sandbox_run(agent_id),
        "graph",
        &format!("graph-{agent_id}"),
    )
    .unwrap();
}

#[tokio::test]
async fn upsert_from_sandbox_worktree_lands_in_original_project() {
    let (_dir, db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-sbx-attr-1");
    register_sandbox_session(&db, "agent-sbx-attr-1");

    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("sbx-attr-fact".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Sandbox attribution fact".to_string()),
                    body: Some("Lands in the original project.".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let node = db
        .get_intelligence_node("sbx-attr-fact")
        .unwrap()
        .expect("upserted node exists");
    let orig_hash = workdir_hash(ORIG);
    let stray_hash = workdir_hash(WT);
    assert_eq!(
        node.project_hash.as_deref(),
        Some(orig_hash.as_str()),
        "a sandbox upsert must be attributed to the original project"
    );
    assert_ne!(node.project_hash.as_deref(), Some(stray_hash.as_str()));
}

#[tokio::test]
async fn get_context_from_sandbox_sees_original_project_nodes() {
    let (_dir, db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-sbx-ctx-1");
    register_sandbox_session(&db, "agent-sbx-ctx-1");

    // A node recorded on the ORIGINAL project (what the fixed upsert path
    // writes for sandboxed agents).
    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("sbx-orig-fact".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("SBX-ORIG-FACT".to_string()),
                    body: Some("Recorded on the original project.".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: Some(Some(workdir_hash(ORIG))),
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    // Reading with no explicit project_hash from inside the sandbox worktree
    // must resolve to the original project and see its nodes.
    let ctx = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "full".to_string(),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&ctx), "{}", text(&ctx));
    assert!(
        raw_text(&ctx).contains("SBX-ORIG-FACT"),
        "get_context from the sandbox must see the original project's nodes: {}",
        raw_text(&ctx)
    );
}
