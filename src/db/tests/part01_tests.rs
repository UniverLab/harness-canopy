//! Part 01 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

/// CB52: an existing database whose `ensembles` entry/exit FKs are still
/// `ON DELETE CASCADE` / `ON DELETE SET NULL` is rebuilt to `ON DELETE
/// RESTRICT` — data preserved, migration idempotent, and afterwards a
/// direct SQL delete of a referenced node fails instead of silently
/// deleting the ensemble.
#[test]
fn cb52_ensemble_fk_migration_rebuilds_entry_exit_to_restrict() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE graphs (id TEXT PRIMARY KEY);
         CREATE TABLE graph_specs (id TEXT PRIMARY KEY);
         CREATE TABLE graph_nodes (id TEXT PRIMARY KEY);
         CREATE TABLE ensembles (
             id TEXT PRIMARY KEY,
             spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
             graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             prompt_template TEXT NOT NULL,
             join_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
             entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
             entry_condition TEXT NOT NULL,
             min_pass INTEGER NOT NULL,
             timeout_minutes INTEGER NOT NULL,
             on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
             on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE SET NULL,
             created_at INTEGER NOT NULL,
             CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
         );
         CREATE INDEX idx_ensembles_join_node ON ensembles(join_node_id);
         INSERT INTO graphs VALUES ('g1');
         INSERT INTO graph_nodes VALUES ('kickoff'), ('join1'), ('arbiter'), ('loner');
         INSERT INTO ensembles VALUES ('ens1', NULL, 'g1', 'Ens', 'tmpl', 'join1', 'kickoff', 'always', 2, 30, 'arbiter', NULL, 0);",
    )
    .unwrap();

    Database::migrate_cb52_ensemble_fk(&conn).unwrap();
    // Idempotent: a second run is a no-op.
    Database::migrate_cb52_ensemble_fk(&conn).unwrap();

    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'ensembles'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        ddl.contains("entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT"),
        "{ddl}"
    );
    assert!(
        ddl.contains("on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT"),
        "{ddl}"
    );
    assert!(
        ddl.contains("on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE RESTRICT"),
        "{ddl}"
    );
    // The ensemble still owns its join: that cascade stays.
    assert!(
        ddl.contains("join_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE"),
        "{ddl}"
    );

    // Data survived the rebuild.
    let entry: String = conn
        .query_row(
            "SELECT entry_from_node FROM ensembles WHERE id = 'ens1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(entry, "kickoff");

    // The helper index survived the drop/rename.
    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_ensembles_join_node'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 1);

    // RESTRICT now bites: deleting a referenced node fails instead of
    // silently deleting the ensemble row.
    assert!(conn
        .execute("DELETE FROM graph_nodes WHERE id = 'kickoff'", [])
        .is_err());
    assert!(conn
        .execute("DELETE FROM graph_nodes WHERE id = 'arbiter'", [])
        .is_err());
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ensembles WHERE id = 'ens1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1);

    // An unreferenced node still deletes fine.
    assert_eq!(
        conn.execute("DELETE FROM graph_nodes WHERE id = 'loner'", [])
            .unwrap(),
        1
    );

    // The join cascade (FR2 exception) still works: deleting the join
    // removes the ensemble row it owns.
    assert_eq!(
        conn.execute("DELETE FROM graph_nodes WHERE id = 'join1'", [])
            .unwrap(),
        1
    );
    let remaining: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ensembles WHERE id = 'ens1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

// RETIRED-SCHEMA-NAME-BEGIN (CC3 loop → graph migration: the old names below
// are the pre-migration schema this test simulates, so they are exempt from
// the retired-vocabulary guard by design)
#[test]
fn graph_schema_migration_is_idempotent_and_preserves_error_edges() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE loops (id TEXT PRIMARY KEY, name TEXT NOT NULL, workdir TEXT NOT NULL, status TEXT NOT NULL, created_at INTEGER NOT NULL);
         CREATE TABLE loop_specs (id TEXT PRIMARY KEY, loop_id TEXT NOT NULL, name TEXT NOT NULL, position INTEGER NOT NULL, status TEXT NOT NULL);
         CREATE TABLE loop_nodes (id TEXT PRIMARY KEY, spec_id TEXT, loop_id TEXT, name TEXT NOT NULL, kind TEXT NOT NULL, config TEXT NOT NULL, position INTEGER NOT NULL, created_at INTEGER NOT NULL);
         CREATE TABLE loop_edges (id TEXT PRIMARY KEY, spec_id TEXT, loop_id TEXT, from_node TEXT NOT NULL, to_node TEXT NOT NULL, condition TEXT NOT NULL);
         CREATE TABLE loop_runs (id TEXT PRIMARY KEY, loop_id TEXT NOT NULL, spec_id TEXT NOT NULL, node_id TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL, iteration INTEGER NOT NULL DEFAULT 1);
         CREATE TABLE loop_completion_hook_runs (id TEXT PRIMARY KEY, loop_id TEXT NOT NULL, status TEXT NOT NULL, started_at INTEGER NOT NULL);
         CREATE TABLE ensembles (id TEXT PRIMARY KEY, spec_id TEXT, loop_id TEXT, name TEXT NOT NULL, prompt_template TEXT NOT NULL, join_node_id TEXT NOT NULL, entry_from_node TEXT NOT NULL, entry_condition TEXT NOT NULL, min_pass INTEGER NOT NULL, timeout_minutes INTEGER NOT NULL, on_pass_to TEXT NOT NULL, created_at INTEGER NOT NULL);
         CREATE UNIQUE INDEX idx_loop_specs_position ON loop_specs(loop_id, position);
         INSERT INTO loops VALUES ('g1', 'Graph', '/tmp', 'running', 0);
         INSERT INTO loop_specs VALUES ('s1', 'g1', 'Spec', 1, 'running');
         INSERT INTO loop_nodes VALUES ('n1', 's1', NULL, 'Node', 'check', '{\"prompt_template\": \"hi {{loop_name}}\"}', 1, 0);
         INSERT INTO loop_nodes VALUES ('n2', 's1', NULL, 'Next', 'check', '{}', 2, 0);
         INSERT INTO loop_edges VALUES ('e1', 's1', NULL, 'n1', 'n2', 'break');
         INSERT INTO loop_runs VALUES ('r1', 'g1', 's1', 'n1', 'running', 0, 1);
         INSERT INTO loop_completion_hook_runs VALUES ('h1', 'g1', 'running', 0);
         INSERT INTO ensembles VALUES ('en1', 's1', NULL, 'Ens', 'tmpl', 'n1', 'n1', 'break', 1, 5, 'n2', 0);",
    )
    .unwrap();

    Database::migrate_loop_to_graph_schema(&conn).unwrap();
    // Re-running after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    Database::migrate_loop_to_graph_schema(&conn).unwrap();

    let table_names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for new in [
        "graphs",
        "graph_specs",
        "graph_nodes",
        "graph_edges",
        "graph_runs",
        "graph_completion_hook_runs",
    ] {
        assert!(table_names.iter().any(|name| name == new), "missing {new}");
    }
    for old in [
        "loops",
        "loop_specs",
        "loop_nodes",
        "loop_edges",
        "loop_runs",
        "loop_completion_hook_runs",
    ] {
        assert!(
            !table_names.iter().any(|name| name == old),
            "retired table {old} still present"
        );
    }
    // Running graph survives with its status intact.
    let status: String = conn
        .query_row("SELECT status FROM graphs WHERE id = 'g1'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "running");
    // `loop_id` columns became `graph_id`.
    let spec_cols: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('graph_specs')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(spec_cols.iter().any(|c| c == "graph_id"));
    assert!(!spec_cols.iter().any(|c| c == "loop_id"));
    // `break` conditions read as `error` everywhere they were stored.
    assert_eq!(
        conn.query_row(
            "SELECT condition FROM graph_edges WHERE id = 'e1'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "error"
    );
    assert_eq!(
        conn.query_row(
            "SELECT entry_condition FROM ensembles WHERE id = 'en1'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "error"
    );
    // Stored `{{loop_name}}` prompt templates were rewritten.
    assert_eq!(
        conn.query_row(
            "SELECT config FROM graph_nodes WHERE id = 'n1'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "{\"prompt_template\": \"hi {{graph_name}}\"}"
    );
    assert_eq!(
        conn.query_row(
            "SELECT graph_id FROM graph_runs WHERE id = 'r1'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "g1"
    );
}
// RETIRED-SCHEMA-NAME-END

// ── Terminal session lifecycle ──────────────────────────────────

#[test]
fn test_terminal_session_finish_removes_from_active_list() {
    let db = test_db();
    db.insert_terminal_session("term-1", "shell-1", "bash", "/tmp")
        .unwrap();

    let active = db.get_active_terminal_sessions().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, "term-1");

    db.finish_terminal_session("term-1").unwrap();
    assert!(db.get_active_terminal_sessions().unwrap().is_empty());
}

#[test]
fn test_mark_orphaned_terminal_sessions_clears_idle_records() {
    let db = test_db();
    db.insert_terminal_session("term-1", "shell-1", "bash", "/tmp")
        .unwrap();
    db.insert_terminal_session("term-2", "shell-2", "zsh", "/tmp")
        .unwrap();

    assert_eq!(db.get_active_terminal_sessions().unwrap().len(), 2);
    db.mark_orphaned_terminal_sessions().unwrap();
    assert!(db.get_active_terminal_sessions().unwrap().is_empty());
}

// ── B32: unrecoverable interactive sessions close instead of orphaning ──

#[test]
fn mark_session_closed_retires_active_session_to_completed() {
    let db = test_db();
    db.insert_interactive_session(
        "sess-dead",
        "s",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    assert_eq!(db.get_active_sessions().unwrap().len(), 1);

    db.mark_session_closed("sess-dead").unwrap();

    // Gone from the active sidebar list, never surfaces as a red orphan, and
    // the row is kept for history in the terminal `completed` status.
    assert!(db.get_active_sessions().unwrap().is_empty());
    assert!(db.get_orphaned_sessions().unwrap().is_empty());
    assert_eq!(
        db.get_interactive_session_status("sess-dead").unwrap(),
        Some("completed".to_string())
    );
}

#[test]
fn mark_session_closed_is_noop_for_non_active_session() {
    let db = test_db();
    db.insert_interactive_session(
        "sess-resumed",
        "s",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.mark_session_resumed("sess-resumed").unwrap();

    // Guarded to `active` rows: a session already resumed elsewhere is left
    // untouched rather than being clobbered to `completed`.
    db.mark_session_closed("sess-resumed").unwrap();
    assert_eq!(
        db.get_interactive_session_status("sess-resumed").unwrap(),
        Some("resumed".to_string())
    );
}

#[test]
fn close_orphaned_interactive_sessions_sweeps_historic_orphans() {
    let db = test_db();
    // A historic orphan (written before orphaning was removed) plus a healthy
    // active session that must be left alone.
    db.insert_interactive_session(
        "sess-old",
        "old",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.mark_session_orphaned("sess-old").unwrap();
    db.insert_interactive_session(
        "sess-live",
        "live",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    assert_eq!(db.get_orphaned_sessions().unwrap().len(), 1);

    let swept = db.close_orphaned_interactive_sessions().unwrap();
    assert_eq!(swept, 1);

    // The orphan row disappears from the orphaned list (now `completed`, kept
    // for history); the active session is untouched.
    assert!(db.get_orphaned_sessions().unwrap().is_empty());
    assert_eq!(
        db.get_interactive_session_status("sess-old").unwrap(),
        Some("completed".to_string())
    );
    assert_eq!(
        db.get_interactive_session_status("sess-live").unwrap(),
        Some("active".to_string())
    );
}

#[test]
fn scheduled_sends_for_closed_session_are_dropped_on_missing_target_restore() {
    let db = test_db();
    db.insert_interactive_session(
        "sess-x",
        "s",
        "claude",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_scheduled_send("ss-x", "ping", "sess-x", None, Utc::now(), None, None)
        .unwrap();
    assert_eq!(
        db.list_pending_scheduled_sends_for_session("sess-x")
            .unwrap()
            .len(),
        1
    );

    // The session is closed as unrecoverable, so it is not resumed and never
    // joins the live-agent set. `restore_scheduled_sends`' missing-target drop
    // (modelled here with an empty live list) then discards its schedules —
    // the same path a session with a missing target already takes.
    db.mark_session_closed("sess-x").unwrap();
    let dropped = db.drop_scheduled_sends_missing_targets(&[]).unwrap();
    assert_eq!(dropped, 1);
    assert!(db
        .list_pending_scheduled_sends_for_session("sess-x")
        .unwrap()
        .is_empty());
}

// ── Sync message lifecycle ───────────────────────────────────────

#[test]
fn test_list_sync_messages_returns_chronological_order() {
    let db = test_db();
    db.insert_sync_message(
        "/tmp/project",
        "agent-a",
        "copilot",
        MessageKind::Info,
        "one",
        None,
    )
    .unwrap();
    db.insert_sync_message(
        "/tmp/project",
        "agent-b",
        "claude",
        MessageKind::Query,
        "two",
        None,
    )
    .unwrap();

    let messages = db.list_sync_messages("/tmp/project", 10).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message, "one");
    assert_eq!(messages[1].message, "two");
}

#[test]
fn test_list_active_sync_agent_ids_includes_live_sessions_and_running_background_agents() {
    let db = test_db();
    db.insert_interactive_session(
        "ix-1",
        "copilot",
        "copilot",
        "/tmp/project",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_terminal_session("term-1", "shell", "bash", "/tmp/project")
        .unwrap();
    db.upsert_agent(&sample_cron_agent("bg-1")).unwrap();
    let run = RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "bg-1".to_string(),
        status: RunStatus::InProgress,
        trigger_type: TriggerType::Scheduled,
        summary: None,
        started_at: Utc::now(),
        finished_at: None,
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_run(&run).unwrap();

    let mut ids = db.list_active_sync_agent_ids("/tmp/project").unwrap();
    ids.sort();

    assert_eq!(
        ids,
        vec!["bg-1".to_string(), "ix-1".to_string(), "term-1".to_string()]
    );
}

#[test]
fn test_sync_message_payload_roundtrip() {
    let db = test_db();
    let payload = serde_json::to_string(&IntentPayload {
        mission: "Refactor auth".to_string(),
        impact: MissionImpact::High,
        description: "touching login flow".to_string(),
    })
    .unwrap();
    db.insert_sync_message(
        "/tmp/project",
        "agent-a",
        "copilot",
        MessageKind::Intent,
        "copilot: Refactor auth",
        Some(&payload),
    )
    .unwrap();
    let status_payload = serde_json::to_string(&StatusPayload {
        status: WorkspaceStatus::Testing,
        message: "running smoke tests".to_string(),
    })
    .unwrap();
    db.insert_sync_message(
        "/tmp/project",
        "agent-a",
        "copilot",
        MessageKind::Status,
        "running smoke tests",
        Some(&status_payload),
    )
    .unwrap();

    let messages = db.list_sync_messages("/tmp/project", 10).unwrap();

    assert_eq!(messages[0].kind, MessageKind::Intent);
    assert_eq!(messages[1].kind, MessageKind::Status);
    assert!(messages[0]
        .payload
        .as_deref()
        .unwrap_or_default()
        .contains("Refactor auth"));
    assert!(messages[1]
        .payload
        .as_deref()
        .unwrap_or_default()
        .contains("testing"));
}

#[test]
fn test_recent_sync_messages_returns_global_order() {
    let db = test_db();
    db.insert_sync_message(
        "/tmp/project-a",
        "agent-a",
        "copilot",
        MessageKind::Info,
        "one",
        None,
    )
    .unwrap();
    db.insert_sync_message(
        "/tmp/project-b",
        "agent-b",
        "claude",
        MessageKind::Info,
        "two",
        None,
    )
    .unwrap();

    let messages = db.list_recent_sync_messages(10).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].message, "one");
    assert_eq!(messages[1].message, "two");
}

#[test]
fn test_resolve_sync_actor_name_prefers_interactive_session_name() {
    let db = test_db();
    db.insert_interactive_session(
        "ix-1",
        "violet-river",
        "copilot",
        "/tmp/project",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.insert_sync_message(
        "/tmp/project",
        "ix-1",
        "Copilot CLI",
        MessageKind::Info,
        "hello",
        None,
    )
    .unwrap();

    let resolved = db.resolve_sync_actor_name("/tmp/project", "ix-1").unwrap();

    assert_eq!(resolved.as_deref(), Some("violet-river · copilot"));
}

#[test]
fn test_resolve_sync_actor_name_prefers_terminal_session_name() {
    let db = test_db();
    db.insert_terminal_session("term-1", "shell-sage", "bash", "/tmp/project")
        .unwrap();

    let resolved = db
        .resolve_sync_actor_name("/tmp/project", "term-1")
        .unwrap();

    assert_eq!(resolved.as_deref(), Some("shell-sage"));
}

#[test]
fn test_resolve_sync_actor_display_name_falls_back_to_agent_id() {
    let db = test_db();

    let resolved = db
        .resolve_sync_actor_display_name("/tmp/project", "bg-1")
        .unwrap();

    assert_eq!(resolved, "bg-1");
}

// ── Project context layer ─────────────────────────────────────────

#[test]
fn test_intelligence_upsert_search_and_graph_walk() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-b".to_string()),
        kind: Some("pattern".to_string()),
        status: None,
        title: Some("Connection caching".to_string()),
        body: Some("Cache expensive clients".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("project-1".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    let base = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("node-a".to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some("Database rule".to_string()),
            body: Some("Use a single connection".to_string()),
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"topic": "db"}))),
            project_hash: Some(Some("project-1".to_string())),
            session_id: Some(Some("session-1".to_string())),
            relations: Some(vec![IntelligenceRelationInput {
                to_node_id: "node-b".to_string(),
                relation: "extends".to_string(),
                weight: Some(0.8),
            }]),
        })
        .unwrap()
        .record;

    let search = db
        .search_intelligence_nodes("connection", Some("pattern"), 10)
        .unwrap();
    assert_eq!(search.results.len(), 1);
    assert_eq!(search.results[0].id, "node-b");

    let walk = db
        .walk_intelligence_graph(&base.id, 2)
        .unwrap()
        .expect("graph walk should find node");
    assert_eq!(walk.root.id, "node-a");
    assert!(walk.nodes.iter().any(|node| node.id == "node-b"));
    assert!(walk.edges.iter().any(|edge| edge.from_node_id == "node-a"));
}

#[test]
fn test_intelligence_search_tokenizes_multi_term_queries() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-multi".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Alpha overview".to_string()),
        body: Some(
            "This section covers alpha in detail. Later on we discuss beta too.".to_string(),
        ),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("project-1".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    // 1. Both terms match the same node — still found (OR semantics).
    let both = db
        .search_intelligence_nodes("alpha beta", None, 10)
        .unwrap();
    assert_eq!(both.results.len(), 1);
    assert_eq!(both.results[0].id, "node-multi");

    // 2. One present term + one absent term now returns the partial match (OR).
    let partial = db.search_intelligence_nodes("alpha zzz", None, 10).unwrap();
    assert_eq!(partial.results.len(), 1);
    assert_eq!(partial.results[0].id, "node-multi");
    assert_eq!(partial.examined_count, 1);

    // 3. Single-term queries keep working as before.
    let single = db.search_intelligence_nodes("beta", None, 10).unwrap();
    assert_eq!(single.results.len(), 1);
    assert_eq!(single.results[0].id, "node-multi");

    // 4. Empty/whitespace-only queries return empty results but report count.
    let empty = db.search_intelligence_nodes("   ", None, 10).unwrap();
    assert!(empty.results.is_empty());
    assert_eq!(empty.examined_count, 1);
}

#[test]
fn test_intelligence_search_or_ranking() {
    let db = test_db();

    // Node A: matches "retry" in body only → 1pt
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-a".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Unrelated title".to_string()),
        body: Some("Discusses retry strategies.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    // Node B: matches "retry" in title + "backoff" in body → 2 terms
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-b".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Retry patterns".to_string()),
        body: Some("Covers backoff strategies.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    // Node C: matches "retry", "backoff" and "future" in body → 3 terms
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-c".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Overview".to_string()),
        body: Some("Retry logic, backoff, and future plans.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    // Node D: matches nothing
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("node-d".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Other".to_string()),
        body: Some("Nothing relevant.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    // Seven-word query whose matching terms are spread across the nodes; the
    // four trailing words match nothing and must degrade rank without emptying.
    let result = db
        .search_intelligence_nodes(
            "retry backoff future missingp missingq missingr missings",
            None,
            10,
        )
        .unwrap();

    // Primary ordering is number of matching terms (guideline), so the
    // three-term body match outranks the two-term match that includes a title:
    //   node-c: 3 terms (retry+backoff+future, all body)
    //   node-b: 2 terms (retry in title, backoff in body)
    //   node-a: 1 term  (retry in body)
    //   node-d: excluded
    assert_eq!(result.results.len(), 3);
    assert_eq!(result.results[0].id, "node-c");
    assert_eq!(result.results[1].id, "node-b");
    assert_eq!(result.results[2].id, "node-a");
    assert_eq!(result.examined_count, 4);
}

#[test]
fn test_intelligence_search_ranks_title_over_body_on_equal_term_count() {
    let db = test_db();

    // Both nodes match exactly one term; the tiebreak is *where* it matches.
    // The title match is inserted first so that a naive recency-only ordering
    // would put the body match on top — only the field weighting flips them.
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("title-hit".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Resilience".to_string()),
        body: Some("Body text with no query terms at all.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("body-hit".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Generic heading".to_string()),
        body: Some("A passing mention of resilience somewhere in here.".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("proj".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let result = db
        .search_intelligence_nodes("resilience", None, 10)
        .unwrap();
    assert_eq!(result.results.len(), 2);
    assert_eq!(result.results[0].id, "title-hit");
    assert_eq!(result.results[1].id, "body-hit");
}

#[test]
fn test_intelligence_search_zero_match_returns_count() {
    let db = test_db();

    for i in 0..3 {
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(format!("node-{i}")),
            kind: Some("fact".to_string()),
            status: None,
            title: Some(format!("Fact {i}")),
            body: Some(format!("Body {i}")),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }

    let result = db
        .search_intelligence_nodes("xyzzy nonexistent", None, 10)
        .unwrap();
    assert_eq!(result.results.len(), 0);
    assert_eq!(result.examined_count, 3);
}

// ── Graph persistence ──────────────────────────────────────────

#[test]
fn graph_details_roundtrip_preserves_order_and_graph() {
    let db = test_db();
    let lp = sample_graph("wf-1");
    let spec_one = sample_graph_spec(&lp.id, "spec-1", 1);
    let spec_two = sample_graph_spec(&lp.id, "spec-2", 2);
    let node_one = sample_graph_node(&spec_one.id, "node-1", 1);
    let node_two = sample_graph_node(&spec_one.id, "node-2", 2);
    let edge = GraphEdge {
        id: "edge-1".to_string(),
        spec_id: Some(spec_one.id.clone()),
        graph_id: None,
        from_node: node_one.id.clone(),
        to_node: node_two.id.clone(),
        condition: GraphEdgeCondition::Pass,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec_two).unwrap();
    db.insert_graph_spec(&spec_one).unwrap();
    db.insert_graph_node(&node_two).unwrap();
    db.insert_graph_node(&node_one).unwrap();
    db.insert_graph_edge(&edge).unwrap();

    let details = db.get_graph_details(&lp.id).unwrap().unwrap();

    assert_eq!(details.lp.id, lp.id);
    assert_eq!(details.specs.len(), 2);
    assert_eq!(details.specs[0].spec.id, spec_one.id);
    assert_eq!(details.specs[0].nodes[0].id, node_one.id);
    assert_eq!(details.specs[0].nodes[1].id, node_two.id);
    assert_eq!(details.specs[0].edges[0].id, edge.id);
    assert_eq!(details.specs[1].spec.id, spec_two.id);
    // Spec-level graph must be untouched by the graph-level graph work: no
    // graph-level nodes/edges were defined for this graph.
    assert!(details.graph_nodes.is_empty());
    assert!(details.graph_edges.is_empty());
}

#[test]
fn graph_level_graph_round_trips_through_insert_and_get_graph_details() {
    // R1: a graph can define its graph once, at the graph level, instead of
    // repeating the same nodes/edges in every spec. `get_graph_details` is
    // exactly what the `graph` MCP tool returns.
    let db = test_db();
    let lp = sample_graph("wf-graph");
    let node_one = GraphNode {
        id: "graph-node-1".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: Utc::now(),
    };
    let node_two = GraphNode {
        id: "graph-node-2".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "review".to_string(),
        kind: GraphNodeKind::Gate,
        config: serde_json::json!({}),
        position: 2,
        created_at: Utc::now(),
    };
    let edge = GraphEdge {
        id: "graph-edge-1".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: node_one.id.clone(),
        to_node: node_two.id.clone(),
        condition: GraphEdgeCondition::Always,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_node(&node_one).unwrap();
    db.insert_graph_node(&node_two).unwrap();
    db.insert_graph_edge(&edge).unwrap();

    let details = db.get_graph_details(&lp.id).unwrap().unwrap();

    assert_eq!(details.graph_nodes.len(), 2);
    assert_eq!(details.graph_nodes[0].id, node_one.id);
    assert_eq!(
        details.graph_nodes[0].graph_id.as_deref(),
        Some(lp.id.as_str())
    );
    assert_eq!(details.graph_nodes[0].spec_id, None);
    assert_eq!(details.graph_edges.len(), 1);
    assert_eq!(details.graph_edges[0].id, edge.id);
    assert_eq!(
        details.graph_edges[0].graph_id.as_deref(),
        Some(lp.id.as_str())
    );
    // A graph with no specs at all still round-trips a graph-only graph.
    assert!(details.specs.is_empty());
}

#[test]
fn graph_node_and_edge_require_exactly_one_target() {
    // R1: every node/edge must target exactly one of (spec_id, graph_id).
    // Enforced in the DB layer with an actionable error, not a raw SQLite
    // CHECK constraint failure.
    let db = test_db();
    let lp = sample_graph("wf-target-validation");
    let spec = sample_graph_spec(&lp.id, "spec-target-validation", 1);
    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();

    let base_node = GraphNode {
        id: "node-both-or-neither".to_string(),
        spec_id: None,
        graph_id: None,
        name: "n".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: Utc::now(),
    };

    let neither_err = db.insert_graph_node(&base_node).unwrap_err().to_string();
    assert!(
        neither_err.contains("exactly one"),
        "expected actionable message, got: {neither_err}"
    );

    let mut both_node = base_node;
    both_node.spec_id = Some(spec.id.clone());
    both_node.graph_id = Some(lp.id.clone());
    let both_err = db.insert_graph_node(&both_node).unwrap_err().to_string();
    assert!(
        both_err.contains("exactly one"),
        "expected actionable message, got: {both_err}"
    );

    let base_edge = GraphEdge {
        id: "edge-both-or-neither".to_string(),
        spec_id: None,
        graph_id: None,
        from_node: "a".to_string(),
        to_node: "b".to_string(),
        condition: GraphEdgeCondition::Always,
    };
    let neither_edge_err = db.insert_graph_edge(&base_edge).unwrap_err().to_string();
    assert!(neither_edge_err.contains("exactly one"));

    let mut both_edge = base_edge;
    both_edge.spec_id = Some(spec.id);
    both_edge.graph_id = Some(lp.id);
    let both_edge_err = db.insert_graph_edge(&both_edge).unwrap_err().to_string();
    assert!(both_edge_err.contains("exactly one"));
}

#[test]
fn graph_migration_adds_graph_id_and_is_idempotent_across_reopen() {
    // Simulate a pre-R1 database: `graph_nodes`/`graph_edges` with `spec_id
    // NOT NULL` and no `graph_id` column — the real shape of databases in the
    // field before this migration. The migration must rebuild both tables
    // (SQLite can't relax NOT NULL via ALTER TABLE ADD COLUMN) without
    // losing the existing rows, and running it again on an already-migrated
    // database must be a no-op.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER
             );
             CREATE TABLE graph_nodes (
                id TEXT PRIMARY KEY,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                position INTEGER NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE UNIQUE INDEX idx_graph_nodes_position ON graph_nodes(spec_id, position);
             CREATE TABLE graph_edges (
                id TEXT PRIMARY KEY,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                to_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'draft', 0);
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', 'legacy-graph', 'Spec', 1, 'pending');
             INSERT INTO graph_nodes (id, spec_id, name, kind, config, position, created_at)
                 VALUES ('legacy-node', 'legacy-spec', 'Node', 'check', '{\"command\":\"true\"}', 1, 0);
             INSERT INTO graph_edges (id, spec_id, from_node, to_node, condition)
                 VALUES ('legacy-edge', 'legacy-spec', 'legacy-node', 'legacy-node', 'always');",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must rebuild both
    // tables without losing the pre-existing rows.
    let db = Database::new(&path).expect("open db, running migration");
    let node = db.get_graph_node("legacy-node").unwrap().unwrap();
    assert_eq!(node.spec_id.as_deref(), Some("legacy-spec"));
    assert_eq!(node.graph_id, None);
    let edge = db.get_graph_edge("legacy-edge").unwrap().unwrap();
    assert_eq!(edge.spec_id.as_deref(), Some("legacy-spec"));
    assert_eq!(edge.graph_id, None);
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let node = db.get_graph_node("legacy-node").unwrap().unwrap();
    assert_eq!(node.spec_id.as_deref(), Some("legacy-spec"));
    assert_eq!(
        db.list_graph_edges("legacy-spec").unwrap().len(),
        1,
        "spec-level edge must survive the rebuild"
    );

    // The rebuilt table now supports graph-level nodes/edges too.
    db.insert_graph_node(&GraphNode {
        id: "graph-node".to_string(),
        spec_id: None,
        graph_id: Some("legacy-graph".to_string()),
        name: "Graph node".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        db.list_graph_nodes_for_graph("legacy-graph").unwrap().len(),
        1
    );
}

#[test]
fn graph_specs_migration_relaxes_graph_id_and_adds_workdir_and_is_idempotent() {
    // Simulate a pre-R3 database: `graph_specs` with `graph_id NOT NULL` and
    // no `workdir` column — the real shape of databases in the field before
    // this migration. The migration must rebuild the table (SQLite can't
    // relax NOT NULL via ALTER TABLE ADD COLUMN) without losing existing
    // (graph-bound) rows, and running it again on an already-migrated
    // database must be a no-op.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'draft', 0);
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', 'legacy-graph', 'Spec', 1, 'pending');",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must rebuild the
    // table without losing the pre-existing, graph-bound row.
    let db = Database::new(&path).expect("open db, running migration");
    let spec = db.get_graph_spec("legacy-spec").unwrap().unwrap();
    assert_eq!(spec.graph_id.as_deref(), Some("legacy-graph"));
    assert_eq!(spec.workdir, None);
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let spec = db.get_graph_spec("legacy-spec").unwrap().unwrap();
    assert_eq!(spec.graph_id.as_deref(), Some("legacy-graph"));
    assert_eq!(
        db.list_graph_specs("legacy-graph").unwrap().len(),
        1,
        "graph-bound spec must survive the rebuild"
    );

    // The rebuilt table now supports standalone specs (no graph) too.
    db.insert_graph_spec(&GraphSpec {
        id: "standalone-spec".to_string(),
        graph_id: None,
        name: "Backlog item".to_string(),
        description: Some("Do a thing".to_string()),
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
        workdir: Some("/tmp/project".to_string()),
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();
    let standalone = db.get_graph_spec("standalone-spec").unwrap().unwrap();
    assert_eq!(standalone.graph_id, None);
    assert_eq!(standalone.workdir.as_deref(), Some("/tmp/project"));
}

#[test]
fn graph_specs_migration_carries_spec_end_dirty_columns() {
    // Same pre-R3 legacy shape as the sibling migration test above, missing
    // every column CM29 adds — `spec_end_dirty`/`spec_end_dirty_paths` on
    // `graph_specs` and `allow_dirty_start` on `graphs`.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'draft', 0);
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', 'legacy-graph', 'Spec', 1, 'pending');",
        )
        .expect("seed legacy schema");
    }

    let has_column = |table: &str, column: &str| -> bool {
        let conn = rusqlite::Connection::open(&path).expect("open db for pragma check");
        conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = '{column}'"),
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("pragma_table_info query")
            > 0
    };

    let db = Database::new(&path).expect("open db, running migration");
    // Existing rows read back as unknown, not as clean (non-functional
    // requirement) — `None`, not `Some(0)`.
    let spec = db.get_graph_spec("legacy-spec").unwrap().unwrap();
    assert_eq!(spec.spec_end_dirty, None);
    assert_eq!(spec.spec_end_dirty_paths, None);
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert!(!lp.allow_dirty_start);
    drop(db);

    assert!(has_column("graph_specs", "spec_end_dirty"));
    assert!(has_column("graph_specs", "spec_end_dirty_paths"));
    assert!(has_column("graphs", "allow_dirty_start"));

    // Running the migration again (reopening) must be a no-op.
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let spec = db.get_graph_spec("legacy-spec").unwrap().unwrap();
    assert_eq!(spec.spec_end_dirty, None);
    assert!(has_column("graph_specs", "spec_end_dirty"));
    assert!(has_column("graphs", "allow_dirty_start"));
}

#[test]
fn standalone_spec_crud_round_trip() {
    let db = test_db();
    let spec = sample_standalone_spec("backlog-1", Some("/tmp/project-a"));
    db.insert_graph_spec(&spec).unwrap();

    let fetched = db.get_graph_spec("backlog-1").unwrap().unwrap();
    assert_eq!(fetched.graph_id, None);
    assert_eq!(fetched.workdir.as_deref(), Some("/tmp/project-a"));
    assert_eq!(fetched.name, "Backlog backlog-1");

    let updated = db
        .update_spec_tag_details(
            "backlog-1",
            Some("Renamed"),
            None,
            Some(Some("/tmp/project-b")),
        )
        .unwrap();
    assert!(updated);
    let fetched = db.get_graph_spec("backlog-1").unwrap().unwrap();
    assert_eq!(fetched.name, "Renamed");
    assert_eq!(fetched.workdir.as_deref(), Some("/tmp/project-b"));

    let deleted = db.delete_graph_spec("backlog-1").unwrap();
    assert!(deleted);
    assert!(db.get_graph_spec("backlog-1").unwrap().is_none());
}

#[test]
fn list_specs_filters_by_workdir_and_unassigned_only() {
    let db = test_db();
    let lp = sample_graph("wf-backlog");
    db.insert_graph(&lp).unwrap();
    let bound_spec = sample_graph_spec(&lp.id, "bound-spec", 1);
    db.insert_graph_spec(&bound_spec).unwrap();

    let standalone_a = sample_standalone_spec("standalone-a", Some("/tmp/project-a"));
    let standalone_b = sample_standalone_spec("standalone-b", Some("/tmp/project-b"));
    db.insert_graph_spec(&standalone_a).unwrap();
    db.insert_graph_spec(&standalone_b).unwrap();

    // No filters: every spec, bound or not.
    let all = db.list_specs(None, None, false, None).unwrap();
    assert_eq!(all.len(), 3);

    // Filter by workdir: only the matching standalone spec.
    let by_workdir = db
        .list_specs(Some("/tmp/project-a"), None, false, None)
        .unwrap();
    assert_eq!(by_workdir.len(), 1);
    assert_eq!(by_workdir[0].id, "standalone-a");

    // unassigned_only excludes the graph-bound spec.
    let unassigned = db.list_specs(None, None, true, None).unwrap();
    assert_eq!(unassigned.len(), 2);
    assert!(unassigned.iter().all(|s| s.graph_id.is_none()));
    assert!(unassigned.iter().any(|s| s.id == "standalone-a"));
    assert!(unassigned.iter().any(|s| s.id == "standalone-b"));

    // Filter by status: none of these are running.
    let running = db
        .list_specs(None, Some(GraphSpecStatus::Running), false, None)
        .unwrap();
    assert!(running.is_empty());
    let pending = db
        .list_specs(None, Some(GraphSpecStatus::Pending), false, None)
        .unwrap();
    assert_eq!(pending.len(), 3);
}
