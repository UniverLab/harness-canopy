//! Migrated-agent deliver tests (split from `deliver_tests` so that file
//! stays under the 800-line gate): a migrated legacy agent's backing-graph
//! deliver node reaches the same observable result as the legacy path, with
//! `deliver` provenance carrying graph, node and run ids.
use super::super::*;
use super::deliver_tests::{deliver_fixture, live_session, run_input};

/// A migrated watch send-to-session agent's deliver node reaches the same
/// observable result as the legacy path: the session receives the prompt
/// with legacy variables substituted plus the watch block, and the send
/// carries `deliver` provenance with graph, node and run ids (strictly more
/// traceable than the legacy `agent_trigger` provenance).
#[tokio::test]
async fn migrated_watch_send_delivers_substituted_prompt_with_deliver_provenance() {
    use crate::application::ports::AgentRepository;
    use crate::daemon::agent_graph_migration::migrate_all_agents_to_graphs;
    use crate::domain::models::{Agent, AgentAction, Cli, Trigger, WatchEvent};

    let (_dir, db, engine, _notify, _lp, spec) = deliver_fixture();
    live_session(&db, "sess-m", "orch");
    let agent = Agent {
        id: "wmig-1".to_string(),
        prompt: "check {{FILE_PATH}} {{EVENT_TYPE}} as {{spec_name}}".to_string(),
        trigger: Some(Trigger::Watch {
            path: "/tmp".to_string(),
            events: vec![WatchEvent::Modify],
            debounce_seconds: 2,
            recursive: false,
        }),
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/wmig-1.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: AgentAction::send_to_session(Some("orch".to_string()), None).unwrap(),
    };
    db.upsert_agent(&agent).unwrap();
    let report = migrate_all_agents_to_graphs(&db).unwrap();
    assert_eq!(report.migrated, vec!["wmig-1".to_string()]);

    let backing = db.get_graph("wmig-1").unwrap().unwrap();
    let nodes = db.list_graph_nodes_for_graph("wmig-1").unwrap();
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];

    let fire = FireCtx {
        trigger: Some(GraphRunTrigger::Watch),
        file_path: Some("/repo/b.rs".to_string()),
        event_type: Some("modify".to_string()),
    };
    let execution = engine
        .execute_deliver_node(&backing, &spec, node, &run_input(None, "run-w", &fire))
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);

    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-m")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0]
            .prompt
            .contains("check /repo/b.rs modify as wmig-1"),
        "{}",
        pending[0].prompt
    );
    assert!(
        pending[0]
            .prompt
            .contains("Triggered by canopy watch agent 'wmig-1'"),
        "{}",
        pending[0].prompt
    );
    let provenance = pending[0].provenance.as_ref().expect("provenance stored");
    assert_eq!(provenance.kind, "deliver");
    assert_eq!(provenance.graph_id, "wmig-1");
    assert_eq!(provenance.node_id.as_deref(), Some(node.id.as_str()));
    assert_eq!(provenance.run_id.as_deref(), Some("run-w"));
}

/// Graph equivalent of the legacy
/// `cron_fire_enqueues_one_send_with_agent_trigger_provenance` executor test
/// (moved here per spec FR5): a migrated cron send-to-session agent fires
/// through its backing graph and the send carries `deliver` provenance with
/// graph, node and run ids — strictly more traceable than the legacy
/// `agent_trigger` kind — with legacy variables substituted identically.
#[tokio::test]
async fn migrated_cron_send_delivers_substituted_prompt_with_deliver_provenance() {
    use crate::application::ports::AgentRepository;
    use crate::daemon::agent_graph_migration::migrate_all_agents_to_graphs;
    use crate::domain::models::{Agent, AgentAction, Cli, Trigger};

    let (_dir, db, engine, _notify, _lp, spec) = deliver_fixture();
    live_session(&db, "sess-c", "orch");
    let agent = Agent {
        id: "cmig-1".to_string(),
        prompt: "report {{TASK_ID}} at {{TIMESTAMP}} log {{LOG_PATH}} as {{spec_name}}".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 * * * *".to_string(),
        }),
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: Some("/tmp/proj".to_string()),
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/cmig-1.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: AgentAction::send_to_session(Some("orch".to_string()), None).unwrap(),
    };
    db.upsert_agent(&agent).unwrap();
    let report = migrate_all_agents_to_graphs(&db).unwrap();
    assert_eq!(report.migrated, vec!["cmig-1".to_string()]);

    let backing = db.get_graph("cmig-1").unwrap().unwrap();
    let nodes = db.list_graph_nodes_for_graph("cmig-1").unwrap();
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(node.kind, GraphNodeKind::Deliver);

    let fire = FireCtx {
        trigger: Some(GraphRunTrigger::Cron),
        file_path: None,
        event_type: None,
    };
    let execution = engine
        .execute_deliver_node(&backing, &spec, node, &run_input(None, "run-c", &fire))
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);

    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-c")
        .unwrap();
    assert_eq!(pending.len(), 1);
    // Legacy variables substitute identically; cron fires never append the
    // watch block.
    assert!(
        pending[0].prompt.contains("report cmig-1 at "),
        "{}",
        pending[0].prompt
    );
    assert!(
        pending[0].prompt.contains(" as cmig-1"),
        "{}",
        pending[0].prompt
    );
    assert!(!pending[0].prompt.contains("{{"), "{}", pending[0].prompt);
    assert!(
        !pending[0].prompt.contains("Triggered by canopy"),
        "{}",
        pending[0].prompt
    );
    let provenance = pending[0].provenance.as_ref().expect("provenance stored");
    assert_eq!(provenance.kind, "deliver");
    assert_eq!(provenance.graph_id, "cmig-1");
    assert_eq!(provenance.node_id.as_deref(), Some(node.id.as_str()));
    assert_eq!(provenance.run_id.as_deref(), Some("run-c"));
}
