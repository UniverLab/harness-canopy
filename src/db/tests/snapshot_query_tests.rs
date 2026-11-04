//! FR2–FR4 list/count queries: the newest-run summary, the per-spec queue
//! states, the spec/knowledge counts and the active-runs map. Each test seeds
//! a fixture DB in a temp dir and asserts the new query agrees with the full
//! load it replaces (or the seeded ground truth).
use super::*;
use crate::application::ports::RunRepository;

struct SeedRun<'a> {
    status: GraphRunStatus,
    started_at: chrono::DateTime<Utc>,
    output: Option<serde_json::Value>,
    id: &'a str,
    graph_id: &'a str,
    spec_id: &'a str,
    node_id: &'a str,
}

fn seed_run(db: &Database, seed: SeedRun<'_>) {
    db.insert_graph_run(&GraphNodeRun {
        id: seed.id.to_string(),
        graph_id: seed.graph_id.to_string(),
        spec_id: seed.spec_id.to_string(),
        node_id: seed.node_id.to_string(),
        status: seed.status,
        input: None,
        output: seed.output,
        started_at: seed.started_at,
        completed_at: Some(seed.started_at),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
}

fn seed_graph_with_spec_node(db: &Database, graph_id: &str, spec_id: &str, node_id: &str) {
    db.insert_graph(&sample_graph(graph_id)).unwrap();
    db.insert_graph_spec(&sample_graph_spec(graph_id, spec_id, 1))
        .unwrap();
    db.insert_graph_node(&sample_graph_node(spec_id, node_id, 1))
        .unwrap();
}

#[test]
fn last_graph_run_summary_returns_the_newest_row() {
    let db = test_db();
    seed_graph_with_spec_node(&db, "g-newest", "s-newest", "n-newest");
    let base = Utc::now();
    seed_run(
        &db,
        SeedRun {
            id: "run-old",
            graph_id: "g-newest",
            spec_id: "s-newest",
            node_id: "n-newest",
            status: GraphRunStatus::Pass,
            started_at: base - Duration::seconds(60),
            output: None,
        },
    );
    seed_run(
        &db,
        SeedRun {
            id: "run-new",
            graph_id: "g-newest",
            spec_id: "s-newest",
            node_id: "n-newest",
            status: GraphRunStatus::Fail,
            started_at: base,
            output: Some(serde_json::json!({"summary": "boom"})),
        },
    );

    let summary = db.last_graph_run_summary("g-newest").unwrap().unwrap();
    assert_eq!(summary.id, "run-new");
    assert_eq!(summary.node_id, "n-newest");
    assert!(matches!(summary.status, GraphRunStatus::Fail));
    assert!(!summary.has_blocker);
}

#[test]
fn last_graph_run_summary_flags_a_blocker_output() {
    let db = test_db();
    seed_graph_with_spec_node(&db, "g-blocked", "s-blocked", "n-blocked");
    seed_run(
        &db,
        SeedRun {
            id: "run-blocked",
            graph_id: "g-blocked",
            spec_id: "s-blocked",
            node_id: "n-blocked",
            status: GraphRunStatus::Fail,
            started_at: Utc::now(),
            output: Some(serde_json::json!({"blocker": "needs a human"})),
        },
    );

    let summary = db.last_graph_run_summary("g-blocked").unwrap().unwrap();
    assert!(summary.has_blocker);
}

#[test]
fn last_graph_run_summary_no_blocker_without_output() {
    let db = test_db();
    seed_graph_with_spec_node(&db, "g-clean", "s-clean", "n-clean");
    seed_run(
        &db,
        SeedRun {
            id: "run-clean",
            graph_id: "g-clean",
            spec_id: "s-clean",
            node_id: "n-clean",
            status: GraphRunStatus::Pass,
            started_at: Utc::now(),
            output: None,
        },
    );

    let summary = db.last_graph_run_summary("g-clean").unwrap().unwrap();
    assert!(!summary.has_blocker);
}

#[test]
fn last_graph_run_summary_none_without_runs() {
    let db = test_db();
    db.insert_graph(&sample_graph("g-empty")).unwrap();
    assert!(db.last_graph_run_summary("g-empty").unwrap().is_none());
    assert!(db.last_graph_run_summary("g-missing").unwrap().is_none());
}

#[test]
fn queue_states_follow_bound_spec_order_with_latest_runs() {
    let db = test_db();
    db.insert_graph(&sample_graph("g-queue")).unwrap();
    let mut first = sample_graph_spec("g-queue", "s-first", 1);
    first.spec_code = Some("FIX-1".to_string());
    db.insert_graph_spec(&first).unwrap();
    let mut second = sample_graph_spec("g-queue", "s-second", 2);
    second.status = GraphSpecStatus::Failed;
    second.completed_via_reason = Some("admin says no".to_string());
    db.insert_graph_spec(&second).unwrap();
    db.insert_graph_node(&sample_graph_node("s-first", "n-first", 1))
        .unwrap();
    let base = Utc::now();
    seed_run(
        &db,
        SeedRun {
            id: "run-s1-old",
            graph_id: "g-queue",
            spec_id: "s-first",
            node_id: "n-first",
            status: GraphRunStatus::Pass,
            started_at: base - Duration::seconds(30),
            output: None,
        },
    );
    seed_run(
        &db,
        SeedRun {
            id: "run-s1-new",
            graph_id: "g-queue",
            spec_id: "s-first",
            node_id: "n-first",
            status: GraphRunStatus::Running,
            started_at: base,
            output: None,
        },
    );

    let states = db.graph_queue_spec_states("g-queue", None).unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].spec_id, "s-first");
    assert_eq!(states[1].spec_id, "s-second");
    assert!(matches!(
        states[0].latest_run_status,
        Some(GraphRunStatus::Running)
    ));
    assert_eq!(states[0].latest_run_started_at, Some(base.timestamp()));
    assert_eq!(states[0].spec_code.as_deref(), Some("FIX-1"));
    assert!(states[1].latest_run_status.is_none());
    assert!(states[1].latest_run_started_at.is_none());
    assert_eq!(
        states[1].completed_via_reason.as_deref(),
        Some("admin says no")
    );
}

#[test]
fn queue_states_follow_queue_member_order() {
    let db = test_db();
    db.insert_graph(&sample_graph("g-qm")).unwrap();
    for id in ["s-qa", "s-qb"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("q-1")).unwrap();
    // Appended in reverse: queue order wins over anything else.
    db.append_queue_member("q-1", "s-qb", None).unwrap();
    db.append_queue_member("q-1", "s-qa", None).unwrap();

    let states = db.graph_queue_spec_states("g-qm", Some("q-1")).unwrap();
    let ids: Vec<&str> = states.iter().map(|s| s.spec_id.as_str()).collect();
    assert_eq!(ids, vec!["s-qb", "s-qa"]);
}

#[test]
fn queue_states_empty_for_a_specless_graph() {
    let db = test_db();
    db.insert_graph(&sample_graph("g-bare")).unwrap();
    assert!(db
        .graph_queue_spec_states("g-bare", None)
        .unwrap()
        .is_empty());
}

#[test]
fn count_specs_matches_the_list_it_replaces() {
    let db = test_db();
    let mut pending_w1 = sample_standalone_spec("cs-1", Some("/w1"));
    pending_w1.status = GraphSpecStatus::Pending;
    let mut done_w1 = sample_standalone_spec("cs-2", Some("/w1"));
    done_w1.status = GraphSpecStatus::Completed;
    let pending_w2 = sample_standalone_spec("cs-3", Some("/w2"));
    db.insert_graph_spec(&pending_w1).unwrap();
    db.insert_graph_spec(&done_w1).unwrap();
    db.insert_graph_spec(&pending_w2).unwrap();
    // Bound specs never count, whatever their workdir tag says.
    db.insert_graph(&sample_graph("g-cs")).unwrap();
    let mut bound = sample_graph_spec("g-cs", "cs-bound", 1);
    bound.workdir = Some("/w1".to_string());
    db.insert_graph_spec(&bound).unwrap();

    assert_eq!(db.count_specs(Some("/w1"), false).unwrap(), 2);
    assert_eq!(db.count_specs(Some("/w1"), true).unwrap(), 1);
    assert_eq!(db.count_specs(Some("/w2"), true).unwrap(), 1);
    assert_eq!(db.count_specs(None, false).unwrap(), 3);
    // The replacement contract: unfiltered count equals the old list length.
    let listed = db.list_specs(None, None, true, None).unwrap().len();
    assert_eq!(db.count_specs(None, false).unwrap(), listed);
}

#[test]
fn count_project_knowledge_counts_facts_and_patterns_only() {
    let db = test_db();
    for (id, kind, hash) in [
        ("ck-fact", "fact", "h-1"),
        ("ck-pattern", "pattern", "h-1"),
        ("ck-idea", "idea", "h-1"),
        ("ck-other", "fact", "h-2"),
    ] {
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some(kind.to_string()),
            status: None,
            title: Some(format!("{id} title")),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some(hash.to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }

    assert_eq!(db.count_project_knowledge("h-1").unwrap(), 2);
    assert_eq!(db.count_project_knowledge("h-2").unwrap(), 1);
    // The replacement contract: count equals the old list length.
    let listed = db.list_project_knowledge("h-1", None, 200).unwrap().len();
    assert_eq!(db.count_project_knowledge("h-1").unwrap(), listed);
}

fn seed_background_run(db: &Database, id: &str, agent_id: &str, status: RunStatus) {
    db.insert_run(&RunLog {
        id: id.to_string(),
        background_agent_id: agent_id.to_string(),
        status,
        trigger_type: TriggerType::Manual,
        summary: Some("test run".to_string()),
        started_at: Utc::now(),
        finished_at: None,
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
}

#[test]
fn list_active_runs_returns_pending_and_in_progress_keyed_by_agent() {
    let db = test_db();
    seed_background_run(&db, "run-p", "agent-p", RunStatus::Pending);
    seed_background_run(&db, "run-i", "agent-i", RunStatus::InProgress);
    seed_background_run(&db, "run-s", "agent-s", RunStatus::Success);
    seed_background_run(&db, "run-e", "agent-e", RunStatus::Error);

    let active = db.list_active_runs().unwrap();
    assert_eq!(active.len(), 2);
    assert_eq!(active["agent-p"].id, "run-p");
    assert_eq!(active["agent-i"].id, "run-i");
}
