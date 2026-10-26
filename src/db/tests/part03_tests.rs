//! Part 03 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

// RETIRED-SCHEMA-NAME-BEGIN (see `no_retired_schema_name_identifiers_remain_outside_its_migration`)
#[test]
fn legacy_queue_table_names_migrate_preserving_member_order_and_context_groups() {
    // Simulate a database still on the previous-generation queue schema (the
    // exact shape every pre-existing installation has on disk today, with
    // its own table, column, and index names — see the raw SQL below). Two
    // queues, several members each, deliberately inserted out of position
    // order, spanning multiple (and no) context groups — the thing most
    // worth not losing is the position-driven order and each member's group.
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
                autorun_at INTEGER,
                spec_pool TEXT,
                active_run_pool_id TEXT,
                on_completed TEXT
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                workdir TEXT
             );
             CREATE TABLE pools (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE pool_members (
                pool_id TEXT NOT NULL REFERENCES pools(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                group_name TEXT,
                PRIMARY KEY (pool_id, spec_id)
             );
             CREATE UNIQUE INDEX idx_pool_members_position
                 ON pool_members(pool_id, position);
             INSERT INTO graphs (id, name, workdir, status, created_at, active_run_pool_id)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'paused', 0, 'queue-a');
             INSERT INTO pools (id, name, created_at) VALUES
                 ('queue-a', 'Queue A', 0),
                 ('queue-b', 'Queue B', 0);",
        )
        .expect("seed legacy schema");

        for (spec_id, position) in [
            ("spec-a1", 1),
            ("spec-a2", 2),
            ("spec-a3", 3),
            ("spec-a4", 4),
            ("spec-b1", 1),
            ("spec-b2", 2),
        ] {
            conn.execute(
                "INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES (?1, NULL, ?1, 1, 'pending')",
                [spec_id],
            )
            .unwrap();
            let _ = position;
        }

        // Insert members out of position order to prove the migration
        // preserves `position`, not insertion order.
        conn.execute(
            "INSERT INTO pool_members (pool_id, spec_id, position, group_name) VALUES
                ('queue-a', 'spec-a3', 3, NULL),
                ('queue-a', 'spec-a1', 1, 'group-1'),
                ('queue-a', 'spec-a4', 4, 'group-2'),
                ('queue-a', 'spec-a2', 2, 'group-1'),
                ('queue-b', 'spec-b2', 2, 'group-3'),
                ('queue-b', 'spec-b1', 1, NULL)",
            [],
        )
        .unwrap();
    }

    let db = Database::new(&path).expect("open legacy-schema db, running the rename migration");

    // Table/index/column identity: old names are gone, new ones hold the data.
    let conn = db.conn.lock().unwrap();
    let old_objects: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('pools', 'pool_members', 'idx_pool_members_position')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_objects, 0, "legacy-named objects must not survive");
    let new_objects: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('queues', 'queue_members', 'idx_queue_members_position')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new_objects, 3, "renamed queue objects must exist");
    drop(conn);

    // Data identity: members present, in the same order, in the same groups.
    assert_eq!(
        db.list_queue_member_groups("queue-a").unwrap(),
        vec![
            ("spec-a1".to_string(), Some("group-1".to_string())),
            ("spec-a2".to_string(), Some("group-1".to_string())),
            ("spec-a3".to_string(), None),
            ("spec-a4".to_string(), Some("group-2".to_string())),
        ]
    );
    assert_eq!(
        db.list_queue_member_groups("queue-b").unwrap(),
        vec![
            ("spec-b1".to_string(), None),
            ("spec-b2".to_string(), Some("group-3".to_string())),
        ]
    );

    // The graph's active-run reference survived under its renamed column.
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.active_run_queue_id.as_deref(), Some("queue-a"));
}

#[test]
fn legacy_queue_table_rename_is_a_noop_on_an_already_migrated_database() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    // First open: fresh database, already on the current (post-rename) schema.
    let db = Database::new(&path).expect("create fresh db");
    db.insert_graph_spec(&GraphSpec {
        id: "spec-1".to_string(),
        graph_id: None,
        name: "spec-1".to_string(),
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "Queue".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", "spec-1", Some("group-1"))
        .unwrap();
    drop(db);

    // Reopening must not error and must not touch existing data — the
    // rename's guard (legacy table absent) makes it a pure no-op.
    let db = Database::new(&path).expect("reopen already-migrated db");
    assert_eq!(
        db.list_queue_member_groups("queue-1").unwrap(),
        vec![("spec-1".to_string(), Some("group-1".to_string()))]
    );
    let conn = db.conn.lock().unwrap();
    let legacy_table_present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'pools'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        legacy_table_present, 0,
        "the migration must not resurrect the legacy table"
    );
}
// RETIRED-SCHEMA-NAME-END

#[test]
fn a_schema_version_newer_than_this_binary_supports_fails_loudly_instead_of_starting_empty() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let db = Database::new(&path).expect("create fresh db");
    db.set_state("schema_version", "999").unwrap();
    drop(db);

    let result = Database::new(&path);
    let message = match result {
        Ok(_) => panic!(
            "a database stamped with a newer schema version than this binary knows must not open"
        ),
        Err(e) => e.to_string(),
    };
    assert!(
        message.contains("999") && message.to_lowercase().contains("schema version"),
        "error must name the version mismatch: {message}"
    );
}

#[test]
fn auto_continue_migration_is_idempotent_and_a_pre_migration_database_opens_cleanly() {
    // Simulate a database written before `auto_continue_at`/`auto_continue_action`
    // existed: `graphs` has every other current column, including `autorun_at`.
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
                autorun_at INTEGER,
                spec_queue TEXT,
                active_run_queue_id TEXT,
                on_completed TEXT
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'paused', 0);",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must succeed and add
    // both new columns, NULL on the existing row.
    let db = Database::new(&path).expect("open pre-auto-continue db, running migration");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.name, "Legacy");
    assert_eq!(lp.auto_continue_at, None);
    assert_eq!(lp.auto_continue_action, None);

    // The new columns are actually usable after migration.
    let at = chrono::DateTime::from_timestamp(
        (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        0,
    )
    .unwrap();
    db.schedule_graph_auto_continue("legacy-graph", at, Some("skip_next_spec"))
        .unwrap();
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.auto_continue_at, Some(at));
    assert_eq!(lp.auto_continue_action.as_deref(), Some("skip_next_spec"));
}

#[test]
fn list_cron_and_watch_graphs_filter_by_trigger_type() {
    let db = test_db();
    let cron = graph_with_trigger(
        "wf-cron",
        Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
    );
    let watch = graph_with_trigger(
        "wf-watch",
        Some(Trigger::Watch {
            path: "/tmp/watch".to_string(),
            events: vec![WatchEvent::Create],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    let manual = graph_with_trigger("wf-manual", None);

    db.insert_graph(&cron).unwrap();
    db.insert_graph(&watch).unwrap();
    db.insert_graph(&manual).unwrap();

    let cron_graphs = db.list_cron_graphs().unwrap();
    assert_eq!(cron_graphs.len(), 1);
    assert_eq!(cron_graphs[0].id, "wf-cron");

    let watch_graphs = db.list_watch_graphs().unwrap();
    assert_eq!(watch_graphs.len(), 1);
    assert_eq!(watch_graphs[0].id, "wf-watch");
    assert_eq!(watch_graphs[0].watch_path(), Some("/tmp/watch"));

    // A manual graph appears in neither trigger list — it never self-fires.
    assert!(!cron_graphs.iter().any(|lp| lp.id == "wf-manual"));
    assert!(!watch_graphs.iter().any(|lp| lp.id == "wf-manual"));
}

#[test]
fn update_graph_trigger_sets_and_clears() {
    let db = test_db();
    let manual = graph_with_trigger("wf-swap", None);
    db.insert_graph(&manual).unwrap();
    assert!(db.list_cron_graphs().unwrap().is_empty());

    // Set a cron trigger.
    db.update_graph_trigger(
        "wf-swap",
        Some(&Trigger::Cron {
            schedule_expr: "15 6 * * *".to_string(),
        }),
    )
    .unwrap();
    let cron_graphs = db.list_cron_graphs().unwrap();
    assert_eq!(cron_graphs.len(), 1);
    assert_eq!(cron_graphs[0].schedule_expr(), Some("15 6 * * *"));

    // Clear it back to manual.
    db.update_graph_trigger("wf-swap", None).unwrap();
    assert!(db.list_cron_graphs().unwrap().is_empty());
    assert_eq!(
        db.get_graph("wf-swap")
            .unwrap()
            .unwrap()
            .trigger_type_label(),
        "manual"
    );
}

#[test]
fn test_list_cross_project_dependencies_returns_only_project_links() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("project-b".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project B".to_string()),
        body: Some("B".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-b".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("project-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project A".to_string()),
        body: Some("A".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: Some(vec![IntelligenceRelationInput {
            to_node_id: "project-b".to_string(),
            relation: "depends_on".to_string(),
            weight: Some(1.0),
        }]),
    })
    .unwrap();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Fact".to_string()),
        body: Some("Fact body".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: None,
        session_id: None,
        relations: Some(vec![IntelligenceRelationInput {
            to_node_id: "project-a".to_string(),
            relation: "depends_on".to_string(),
            weight: Some(1.0),
        }]),
    })
    .unwrap();

    let deps = db.list_cross_project_dependencies(10).unwrap();

    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].from_node_id, "project-a");
    assert_eq!(deps[0].to_node_id, "project-b");
    assert_eq!(deps[0].relation, "depends_on");
}

// ── Project registry / RAG metadata ──────────────────────────────

#[test]
fn test_register_project_path_extracts_readme_description() {
    let db = test_db();
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("README.md"),
        "# Title\n\nThis project description has enough words to satisfy the extractor and should become the default description for the registered project before any manual edits.\n",
    )
    .unwrap();

    let project = db.register_project_path(dir.path()).unwrap();

    assert_eq!(
        project.name,
        dir.path().file_name().unwrap().to_string_lossy()
    );
    assert!(project
        .description
        .as_deref()
        .unwrap_or_default()
        .contains("enough words"));
}

#[test]
fn test_upsert_project_preserves_existing_manual_description() {
    let db = test_db();
    let mut project = crate::domain::project::Project::new("/tmp/project");
    project.description = Some("Manual description".to_string());
    db.upsert_project(&project).unwrap();

    let mut updated = crate::domain::project::Project::new("/tmp/project");
    updated.description = Some("README description".to_string());
    db.upsert_project(&updated).unwrap();

    let stored = db.get_project(&project.hash).unwrap().unwrap();
    assert_eq!(stored.description.as_deref(), Some("Manual description"));
}

#[test]
fn test_mark_project_indexed_updates_timestamp() {
    let db = test_db();
    let project = crate::domain::project::Project::new("/tmp/project");
    db.upsert_project(&project).unwrap();

    let updated = db.mark_project_indexed(&project.hash, 1234).unwrap();
    assert!(updated);

    let stored = db.get_project(&project.hash).unwrap().unwrap();
    assert_eq!(stored.indexed_at, Some(1234));
}

#[test]
fn test_rag_queue_roundtrip() {
    let db = test_db();
    let project = crate::domain::project::Project::new("/tmp/project");
    db.upsert_project(&project).unwrap();

    db.enqueue_rag_item("/tmp/project/src/lib.rs", 111).unwrap();
    db.mark_rag_item_processing("/tmp/project/src/lib.rs", 222)
        .unwrap();

    let items = db.list_rag_queue(10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].status, "processing");

    db.remove_rag_item("/tmp/project/src/lib.rs").unwrap();
    assert!(db.list_rag_queue(10).unwrap().is_empty());
}

#[test]
fn test_rag_queue_counts() {
    let db = test_db();
    db.enqueue_rag_item("/tmp/a.rs", 111).unwrap();
    db.enqueue_rag_item("/tmp/b.rs", 112).unwrap();
    db.mark_rag_item_processing("/tmp/a.rs", 113).unwrap();

    let (queued, processing) = db.rag_queue_counts().unwrap();
    assert_eq!(queued, 1);
    assert_eq!(processing, 1);
}

#[test]
fn test_requeue_processing_rag_items() {
    let db = test_db();
    db.enqueue_rag_item("/tmp/a.rs", 111).unwrap();
    db.enqueue_rag_item("/tmp/b.rs", 112).unwrap();
    db.mark_rag_item_processing("/tmp/a.rs", 113).unwrap();

    let recovered = db.requeue_processing_rag_items(999).unwrap();
    assert_eq!(recovered, 1);

    let items = db.list_rag_queue(10).unwrap();
    let a = items
        .iter()
        .find(|item| item.source_path == "/tmp/a.rs")
        .expect("requeued item exists");
    assert_eq!(a.status, "queued");
    assert_eq!(a.queued_at, 999);

    let (queued, processing) = db.rag_queue_counts().unwrap();
    assert_eq!(queued, 2);
    assert_eq!(processing, 0);
}

#[test]
fn test_indexed_files_timestamps_uses_last_success_unless_deleted() {
    let db = test_db();

    db.log_rag_event("/tmp/a.md", "indexed", None, 100).unwrap();
    db.log_rag_event("/tmp/a.md", "error", Some("transient"), 110)
        .unwrap();

    db.log_rag_event("/tmp/b.md", "indexed", None, 120).unwrap();
    db.log_rag_event("/tmp/b.md", "deleted", None, 130).unwrap();

    db.log_rag_event("/tmp/c.md", "indexed", None, 90).unwrap();
    db.log_rag_event("/tmp/c.md", "indexed", None, 140).unwrap();

    db.log_rag_event("/tmp/d.md", "error", Some("never indexed"), 150)
        .unwrap();

    let timestamps = db.indexed_files_timestamps().unwrap();

    assert_eq!(timestamps.get("/tmp/a.md"), Some(&100));
    assert_eq!(timestamps.get("/tmp/c.md"), Some(&140));
    assert!(!timestamps.contains_key("/tmp/b.md"));
    assert!(!timestamps.contains_key("/tmp/d.md"));
}

#[test]
fn test_rag_error_count_and_permanently_failed_files() {
    let db = test_db();

    db.log_rag_event("/tmp/bad.pdf", "error", Some("attempt 1"), 100)
        .unwrap();
    db.log_rag_event("/tmp/bad.pdf", "error", Some("attempt 2"), 110)
        .unwrap();
    db.log_rag_event("/tmp/bad.pdf", "error", Some("attempt 3"), 120)
        .unwrap();
    db.log_rag_event("/tmp/bad.pdf", "failed", Some("giving up"), 120)
        .unwrap();

    db.log_rag_event("/tmp/ok.md", "indexed", None, 50).unwrap();
    db.log_rag_event("/tmp/ok.md", "error", Some("transient"), 60)
        .unwrap();

    assert_eq!(db.rag_error_count("/tmp/bad.pdf").unwrap(), 3);
    assert_eq!(db.rag_error_count("/tmp/ok.md").unwrap(), 1);
    assert_eq!(db.rag_error_count("/tmp/unknown.md").unwrap(), 0);

    let failed = db.permanently_failed_rag_files().unwrap();
    assert!(failed.contains("/tmp/bad.pdf"));
    assert!(!failed.contains("/tmp/ok.md"));
}

#[test]
fn test_permanently_failed_rag_files_cleared_by_later_index() {
    let db = test_db();

    db.log_rag_event("/tmp/retry.pdf", "error", Some("attempt 1"), 100)
        .unwrap();
    db.log_rag_event("/tmp/retry.pdf", "failed", Some("giving up"), 100)
        .unwrap();

    let failed = db.permanently_failed_rag_files().unwrap();
    assert!(failed.contains("/tmp/retry.pdf"));

    // A manual re-add later succeeds — the file should no longer be
    // considered permanently failed.
    db.log_rag_event("/tmp/retry.pdf", "indexed", None, 200)
        .unwrap();

    let failed = db.permanently_failed_rag_files().unwrap();
    assert!(!failed.contains("/tmp/retry.pdf"));
}

// ── Agent CRUD ─────────────────────────────────────────────────────

#[test]
fn test_upsert_and_get_cron_agent() {
    let db = test_db();
    let agent = sample_cron_agent("build-daily");
    db.upsert_agent(&agent).unwrap();

    let retrieved = db.get_agent("build-daily").unwrap().expect("agent exists");
    assert_eq!(retrieved.id, "build-daily");
    assert_eq!(retrieved.prompt, "Run tests");
    assert!(retrieved.is_cron());
    assert_eq!(retrieved.schedule_expr(), Some("0 9 * * *"));
    assert_eq!(retrieved.cli.as_str(), "opencode");
    assert_eq!(retrieved.working_dir.as_deref(), Some("/tmp/project"));
    assert!(retrieved.enabled);
}

#[test]
fn test_upsert_and_get_watch_agent() {
    let db = test_db();
    let agent = sample_watch_agent("watch-src");
    db.upsert_agent(&agent).unwrap();

    let retrieved = db.get_agent("watch-src").unwrap().expect("agent exists");
    assert_eq!(retrieved.id, "watch-src");
    assert!(retrieved.is_watch());
    assert_eq!(retrieved.watch_path(), Some("/tmp/watched"));
    let events = retrieved.watch_events().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.contains(&WatchEvent::Create));
    assert!(events.contains(&WatchEvent::Modify));
    assert_eq!(retrieved.cli.as_str(), "kiro");
    assert_eq!(retrieved.model.as_deref(), Some("claude-4"));
}

#[test]
fn test_get_nonexistent_agent() {
    let db = test_db();
    let result = db.get_agent("does-not-exist").unwrap();
    assert!(result.is_none());
}

#[test]
fn test_upsert_agent_overwrites() {
    let db = test_db();
    let mut agent = sample_cron_agent("my-agent");
    db.upsert_agent(&agent).unwrap();

    agent.prompt = "Updated prompt".to_string();
    agent.trigger = Some(Trigger::Cron {
        schedule_expr: "*/10 * * * *".to_string(),
    });
    db.upsert_agent(&agent).unwrap();

    let retrieved = db.get_agent("my-agent").unwrap().unwrap();
    assert_eq!(retrieved.prompt, "Updated prompt");
    assert_eq!(retrieved.schedule_expr(), Some("*/10 * * * *"));
}

#[test]
fn test_list_agents_ordered_by_created_at_desc() {
    let db = test_db();

    let mut a1 = sample_cron_agent("first");
    a1.created_at = Utc::now() - Duration::hours(2);
    let mut a2 = sample_cron_agent("second");
    a2.created_at = Utc::now() - Duration::hours(1);
    let mut a3 = sample_cron_agent("third");
    a3.created_at = Utc::now();

    db.upsert_agent(&a1).unwrap();
    db.upsert_agent(&a2).unwrap();
    db.upsert_agent(&a3).unwrap();

    let agents = db.list_agents().unwrap();
    assert_eq!(agents.len(), 3);
    assert_eq!(agents[0].id, "third");
    assert_eq!(agents[1].id, "second");
    assert_eq!(agents[2].id, "first");
}

#[test]
fn test_list_cron_agents_filters_correctly() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("cron-1")).unwrap();
    db.upsert_agent(&sample_watch_agent("watch-1")).unwrap();
    db.upsert_agent(&sample_manual_agent("manual-1")).unwrap();

    let cron_agents = db.list_cron_agents().unwrap();
    assert_eq!(cron_agents.len(), 1);
    assert!(cron_agents[0].is_cron());
}

#[test]
fn test_list_watch_agents_filters_correctly() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("cron-1")).unwrap();
    db.upsert_agent(&sample_watch_agent("watch-1")).unwrap();
    db.upsert_agent(&sample_manual_agent("manual-1")).unwrap();

    let watch_agents = db.list_watch_agents().unwrap();
    assert_eq!(watch_agents.len(), 1);
    assert!(watch_agents[0].is_watch());
}

#[test]
fn test_delete_agent() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("to-delete")).unwrap();
    assert!(db.get_agent("to-delete").unwrap().is_some());

    assert!(db.delete_agent("to-delete").unwrap(), "row existed");
    assert!(db.get_agent("to-delete").unwrap().is_none());
}

#[test]
fn test_delete_agent_reports_whether_a_row_existed() {
    let db = test_db();
    assert!(
        !db.delete_agent("never-existed").unwrap(),
        "deleting a missing id must report false, not error"
    );
}

/// B7: a single corrupt row (malformed `trigger_config`, e.g. a raw cron
/// string inserted directly via SQL by an external tool) must not take down
/// `list_agents`/`list_cron_agents`/`list_watch_agents` — it must simply be
/// absent from those healthy-only lists.
#[test]
fn list_queries_skip_corrupt_row_without_erroring() {
    let db = test_db();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();
    db.upsert_agent(&sample_cron_agent("cron-1")).unwrap();
    db.upsert_agent(&sample_watch_agent("watch-1")).unwrap();

    let all = db
        .list_agents()
        .expect("a corrupt row must not error the whole query");
    let mut ids: Vec<&str> = all.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["cron-1", "watch-1"]);

    let cron = db.list_cron_agents().expect("must not error");
    assert_eq!(cron.len(), 1);
    assert_eq!(cron[0].id, "cron-1");

    let watch = db.list_watch_agents().expect("must not error");
    assert_eq!(watch.len(), 1);
    assert_eq!(watch[0].id, "watch-1");
}

/// `list_corrupt_agents` is the one place corrupt rows are surfaced —
/// callers (scheduler quarantine, TUI, MCP `agent_list`) use it to flag the
/// row instead of guessing at its contents.
#[test]
fn list_corrupt_agents_flags_the_row_with_its_parse_error() {
    let db = test_db();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();
    db.upsert_agent(&sample_cron_agent("cron-1")).unwrap();

    let corrupt = db.list_corrupt_agents().unwrap();
    assert_eq!(corrupt.len(), 1);
    assert_eq!(corrupt[0].id, "corrupt-1");
    assert!(corrupt[0].enabled);
    assert!(
        !corrupt[0].error.is_empty(),
        "must carry the parse error for diagnosis"
    );
}

/// `agent_remove` must always succeed against a corrupt row — it deletes by
/// id and never has to parse `trigger_config` to do it.
#[test]
fn delete_agent_removes_corrupt_row_without_parsing_it() {
    let db = test_db();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();

    assert!(db.delete_agent("corrupt-1").unwrap());
    assert!(db.list_corrupt_agents().unwrap().is_empty());
    assert!(db.list_agents().unwrap().is_empty());
}

#[test]
fn test_update_agent_enabled() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("toggle-me")).unwrap();

    db.update_agent_enabled("toggle-me", false).unwrap();
    let agent = db.get_agent("toggle-me").unwrap().unwrap();
    assert!(!agent.enabled);

    db.update_agent_enabled("toggle-me", true).unwrap();
    let agent = db.get_agent("toggle-me").unwrap().unwrap();
    assert!(agent.enabled);
}

#[test]
fn test_schedule_agent_enable_persists_enable_at_and_stays_disabled() {
    let db = test_db();
    let mut agent = sample_cron_agent("wake-me");
    agent.enabled = false;
    db.upsert_agent(&agent).unwrap();

    let at = Utc::now() + Duration::hours(1);
    db.schedule_agent_enable("wake-me", at).unwrap();

    let agent = db.get_agent("wake-me").unwrap().unwrap();
    assert!(
        !agent.enabled,
        "scheduling enable must not enable immediately"
    );
    assert_eq!(agent.enable_at.map(|t| t.timestamp()), Some(at.timestamp()));
}

#[test]
fn test_activate_scheduled_enable_enables_and_clears_enable_at() {
    let db = test_db();
    let mut agent = sample_cron_agent("wake-me-2");
    agent.enabled = false;
    db.upsert_agent(&agent).unwrap();
    db.schedule_agent_enable("wake-me-2", Utc::now() - Duration::minutes(5))
        .unwrap();

    db.activate_scheduled_enable("wake-me-2").unwrap();

    let agent = db.get_agent("wake-me-2").unwrap().unwrap();
    assert!(agent.enabled, "activation must enable the agent");
    assert!(agent.enable_at.is_none(), "activation must clear enable_at");
}

#[test]
fn test_list_pending_enable_agents_filters_correctly() {
    let db = test_db();

    let mut pending = sample_cron_agent("pending-1");
    pending.enabled = false;
    db.upsert_agent(&pending).unwrap();
    db.schedule_agent_enable("pending-1", Utc::now() + Duration::hours(1))
        .unwrap();

    // Enabled agent with no enable_at — must not show up.
    db.upsert_agent(&sample_cron_agent("already-enabled"))
        .unwrap();

    // Disabled agent with no enable_at set — must not show up.
    let mut disabled_only = sample_cron_agent("disabled-only");
    disabled_only.enabled = false;
    db.upsert_agent(&disabled_only).unwrap();

    let results = db.list_pending_enable_agents().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "pending-1");
}

#[test]
fn test_update_agent_last_run() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("run-me")).unwrap();

    db.update_agent_last_run("run-me", true).unwrap();
    let agent = db.get_agent("run-me").unwrap().unwrap();
    assert!(agent.last_run_at.is_some());
    assert_eq!(agent.last_run_ok, Some(true));

    db.update_agent_last_run("run-me", false).unwrap();
    let agent = db.get_agent("run-me").unwrap().unwrap();
    assert_eq!(agent.last_run_ok, Some(false));
}

#[test]
fn test_update_agent_triggered() {
    let db = test_db();
    db.upsert_agent(&sample_watch_agent("trig-w")).unwrap();

    db.update_agent_triggered("trig-w").unwrap();
    let agent = db.get_agent("trig-w").unwrap().unwrap();
    assert!(agent.last_triggered_at.is_some());
    assert_eq!(agent.trigger_count, 1);

    db.update_agent_triggered("trig-w").unwrap();
    let agent = db.get_agent("trig-w").unwrap().unwrap();
    assert_eq!(agent.trigger_count, 2);
}

#[test]
fn test_agent_with_expiration() {
    let db = test_db();
    let mut agent = sample_cron_agent("expiring");
    agent.expires_at = Some(Utc::now() + Duration::hours(1));
    db.upsert_agent(&agent).unwrap();

    let retrieved = db.get_agent("expiring").unwrap().unwrap();
    assert!(retrieved.expires_at.is_some());
    assert!(!retrieved.is_expired());
}

#[test]
fn test_manual_agent_roundtrip() {
    let db = test_db();
    let agent = sample_manual_agent("manual-task");
    db.upsert_agent(&agent).unwrap();

    let retrieved = db.get_agent("manual-task").unwrap().unwrap();
    assert_eq!(retrieved.id, "manual-task");
    assert!(retrieved.trigger.is_none());
    assert!(!retrieved.is_cron());
    assert!(!retrieved.is_watch());
    assert_eq!(retrieved.trigger_type_label(), "manual");
}

#[test]
fn test_rename_agent_updates_agent_and_run_references() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("old-name")).unwrap();
    let run = RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "old-name".to_string(),
        status: RunStatus::Success,
        trigger_type: TriggerType::Scheduled,
        summary: None,
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        exit_code: Some(0),
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_run(&run).unwrap();

    db.rename_agent("old-name", "new-name", "/tmp/new-name.log")
        .unwrap();

    assert!(db.get_agent("old-name").unwrap().is_none());
    let renamed = db
        .get_agent("new-name")
        .unwrap()
        .expect("renamed agent exists under new id");
    assert_eq!(renamed.log_path, "/tmp/new-name.log");

    let runs = db.list_runs("new-name", 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
    assert!(db.list_runs("old-name", 10).unwrap().is_empty());
}

#[test]
fn test_rename_agent_fails_when_new_id_already_exists() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("agent-a")).unwrap();
    db.upsert_agent(&sample_cron_agent("agent-b")).unwrap();

    let result = db.rename_agent("agent-a", "agent-b", "/tmp/agent-b.log");
    assert!(result.is_err());

    // Neither agent should have been touched by the rejected rename.
    assert!(db.get_agent("agent-a").unwrap().is_some());
    let b = db.get_agent("agent-b").unwrap().unwrap();
    assert_eq!(b.log_path, "/tmp/test.log");
}

#[test]
fn test_rename_agent_fails_when_old_id_missing() {
    let db = test_db();
    let result = db.rename_agent("does-not-exist", "new-id", "/tmp/new-id.log");
    assert!(result.is_err());
}

// ── Run log operations ────────────────────────────────────────────

#[test]
fn test_insert_and_list_runs() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("run-agent")).unwrap();

    let run = RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "run-agent".to_string(),
        status: RunStatus::Success,
        trigger_type: TriggerType::Scheduled,
        summary: None,
        started_at: Utc::now() - Duration::minutes(5),
        finished_at: Some(Utc::now()),
        exit_code: Some(0),
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_run(&run).unwrap();

    let runs = db.list_runs("run-agent", 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].background_agent_id, "run-agent");
    assert_eq!(runs[0].exit_code, Some(0));
    assert!(matches!(runs[0].trigger_type, TriggerType::Scheduled));
}

#[test]
fn test_list_runs_limit() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("many-runs")).unwrap();

    for i in 0..10 {
        let run = RunLog {
            id: uuid::Uuid::new_v4().to_string(),
            background_agent_id: "many-runs".to_string(),
            status: RunStatus::Success,
            trigger_type: TriggerType::Manual,
            summary: None,
            started_at: Utc::now() - Duration::minutes(i),
            finished_at: Some(Utc::now()),
            exit_code: Some(0),
            timeout_at: None,
            executed_platform: None,
            executed_model: None,
        };
        db.insert_run(&run).unwrap();
    }

    let runs = db.list_runs("many-runs", 3).unwrap();
    assert_eq!(runs.len(), 3);
}

#[test]
fn test_delete_agent_cascades_runs() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("cascade-agent"))
        .unwrap();
    let run = RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "cascade-agent".to_string(),
        status: RunStatus::Pending,
        trigger_type: TriggerType::Watch,
        summary: None,
        started_at: Utc::now(),
        finished_at: None,
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_run(&run).unwrap();
    assert_eq!(db.list_runs("cascade-agent", 10).unwrap().len(), 1);

    db.delete_agent("cascade-agent").unwrap();
    assert_eq!(db.list_runs("cascade-agent", 10).unwrap().len(), 0);
}

#[test]
fn test_update_run_status() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("status-agent")).unwrap();

    let run_id = uuid::Uuid::new_v4().to_string();
    let run = RunLog {
        id: run_id.clone(),
        background_agent_id: "status-agent".to_string(),
        status: RunStatus::Pending,
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

    let ok = db
        .update_run_status(&run_id, RunStatus::Success, Some("Done"))
        .unwrap();
    assert!(ok);

    let updated = db.get_run(&run_id).unwrap().unwrap();
    assert!(matches!(updated.status, RunStatus::Success));
    assert_eq!(updated.summary.as_deref(), Some("Done"));
    assert!(updated.finished_at.is_some());

    let snapshot = db
        .get_operational_session(&format!("run:{run_id}"))
        .unwrap()
        .unwrap();
    assert!(snapshot.body.contains("Done"));
    assert!(snapshot
        .metadata
        .as_deref()
        .unwrap_or_default()
        .contains("/tmp/project"));
}

#[test]
fn test_update_run_exit_code() {
    let db = test_db();
    db.upsert_agent(&sample_cron_agent("exit-agent")).unwrap();

    let run_id = uuid::Uuid::new_v4().to_string();
    let run = RunLog {
        id: run_id.clone(),
        background_agent_id: "exit-agent".to_string(),
        status: RunStatus::Success,
        trigger_type: TriggerType::Manual,
        summary: Some("OK".to_string()),
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_run(&run).unwrap();

    let ok = db.update_run_exit_code(&run_id, 0).unwrap();
    assert!(ok);

    let updated = db.get_run(&run_id).unwrap().unwrap();
    assert_eq!(updated.exit_code, Some(0));
}

// ── Daemon state ──────────────────────────────────────────────

#[test]
fn test_set_and_get_state() {
    let db = test_db();
    db.set_state("port", "7755").unwrap();
    assert_eq!(db.get_state("port").unwrap(), Some("7755".to_string()));
}

#[test]
fn test_get_state_missing_key() {
    let db = test_db();
    assert!(db.get_state("missing").unwrap().is_none());
}

#[test]
fn test_set_state_overwrites() {
    let db = test_db();
    db.set_state("version", "0.1.0").unwrap();
    db.set_state("version", "0.2.0").unwrap();
    assert_eq!(db.get_state("version").unwrap(), Some("0.2.0".to_string()));
}

// ── Intelligence V2: Project-Linked Knowledge ─────────────────────

#[test]
fn test_list_projects_returns_project_nodes() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project Alpha".to_string()),
        body: Some("Alpha project description".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-a"}))),
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Some fact".to_string()),
        body: Some("fact body".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let projects = db.list_intelligence_projects(None, 10).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, "proj-a");
    assert_eq!(projects[0].kind, "project");
}

#[test]
fn test_list_projects_filters_by_query() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Alpha Backend".to_string()),
        body: Some("Backend services".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-a"}))),
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-b".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Beta Frontend".to_string()),
        body: Some("Frontend app".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-b"}))),
        project_hash: Some(Some("hash-b".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let results = db.list_intelligence_projects(Some("frontend"), 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Beta Frontend");
}

#[test]
fn test_link_projects_creates_edge_between_projects() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project A".to_string()),
        body: Some("A".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-a"}))),
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-b".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project B".to_string()),
        body: Some("B".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-b"}))),
        project_hash: Some(Some("hash-b".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let edge = db
        .link_projects("hash-a", "hash-b", "depends_on", Some(2.0))
        .unwrap();
    assert_eq!(edge.relation, "depends_on");
    assert_eq!(edge.weight, 2.0);
    assert_eq!(edge.from_node_id, "proj-a");
    assert_eq!(edge.to_node_id, "proj-b");
}

#[test]
fn test_link_projects_fails_when_project_missing() {
    let db = test_db();
    let result = db.link_projects("nonexistent-a", "nonexistent-b", "relates_to", None);
    assert!(result.is_err());
}

#[test]
fn test_list_project_knowledge_returns_facts_and_patterns() {
    let db = test_db();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("proj-a".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Project A".to_string()),
        body: Some("A".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"hash": "hash-a"}))),
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("DB convention".to_string()),
        body: Some("Always use SQLite".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("pattern-1".to_string()),
        kind: Some("pattern".to_string()),
        status: None,
        title: Some("Error handling".to_string()),
        body: Some("Use anyhow".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-other".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("Other fact".to_string()),
        body: Some("unrelated".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("hash-other".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let knowledge = db.list_project_knowledge("hash-a", None, 10).unwrap();
    assert_eq!(knowledge.len(), 2);

    let facts = db
        .list_project_knowledge("hash-a", Some("fact"), 10)
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].kind, "fact");

    let patterns = db
        .list_project_knowledge("hash-a", Some("pattern"), 10)
        .unwrap();
    assert_eq!(patterns.len(), 1);
    assert_eq!(patterns[0].kind, "pattern");
}
