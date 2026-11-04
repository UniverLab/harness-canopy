//! Part 05 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

#[test]
fn list_project_history_merges_finished_graphs_and_past_sessions_newest_first() {
    use crate::db::project::ProjectHistoryKind;

    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Database::new(&path).expect("create test db");
    let workdir = "/tmp/history-project";
    let other_workdir = "/tmp/other-project";

    // A finished graph, scoped to our workdir — the oldest event.
    let mut finished_graph = sample_graph("graph-finished");
    finished_graph.workdir = workdir.to_string();
    finished_graph.name = "Finished graph".to_string();
    finished_graph.status = GraphStatus::Completed;
    finished_graph.completed_at = Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 1, 0).unwrap());
    db.insert_graph(&finished_graph).unwrap();

    // A still-running graph in the same workdir must NOT show up in history —
    // only completed/failed graphs are "finished".
    let mut running_graph = sample_graph("graph-running");
    running_graph.workdir = workdir.to_string();
    running_graph.status = GraphStatus::Running;
    db.insert_graph(&running_graph).unwrap();

    // A finished graph in a *different* workdir must not leak into our results.
    let mut other_graph = sample_graph("graph-other-project");
    other_graph.workdir = other_workdir.to_string();
    other_graph.status = GraphStatus::Completed;
    other_graph.completed_at = Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 1, 30).unwrap());
    db.insert_graph(&other_graph).unwrap();

    // A past (closed) interactive session in our workdir — the middle event.
    db.insert_interactive_session(
        "session-past",
        "Past session",
        "opencode",
        workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.mark_session_closed("session-past").unwrap();

    // A still-active interactive session must NOT show up in history.
    db.insert_interactive_session(
        "session-active",
        "Active session",
        "opencode",
        workdir,
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    // A finished terminal session in our workdir, the most recent event.
    db.insert_terminal_session("term-finished", "Finished terminal", "bash", workdir)
        .unwrap();
    db.finish_terminal_session("term-finished").unwrap();

    // A still-idle terminal session must NOT show up in history.
    db.insert_terminal_session("term-idle", "Idle terminal", "bash", workdir)
        .unwrap();

    // Pin down exact timestamps for the two session rows via a short-lived raw
    // connection: `list_project_history` truncates rfc3339 timestamps to whole
    // seconds (`DateTime::timestamp()`), so two `Utc::now()` calls made back to
    // back in this test could otherwise tie and make the ordering assertions
    // flaky. Deterministic timestamps make the newest-first order exact.
    {
        let conn = rusqlite::Connection::open(&path).expect("open raw conn for timestamp fixup");
        conn.execute(
            "UPDATE interactive_sessions SET started_at = ?1 WHERE id = 'session-past'",
            rusqlite::params!["2024-01-01T00:02:00Z"],
        )
        .unwrap();
        conn.execute(
            "UPDATE terminal_sessions SET last_active = ?1 WHERE id = 'term-finished'",
            rusqlite::params!["2024-01-01T00:03:00Z"],
        )
        .unwrap();
    }

    let history = db.list_project_history(workdir, 100).unwrap();

    assert_eq!(
        history.len(),
        3,
        "only the finished graph and the two closed/finished sessions for this workdir should appear: {:?}",
        history.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    assert!(history.iter().any(|e| e.name == "Finished graph"
        && e.kind == ProjectHistoryKind::Graph
        && e.status == "completed"));
    assert!(history
        .iter()
        .any(|e| e.name == "Past session" && e.kind == ProjectHistoryKind::InteractiveSession));
    assert!(history
        .iter()
        .any(|e| e.name == "Finished terminal" && e.kind == ProjectHistoryKind::TerminalSession));

    // Newest-first ordering: the terminal session finished most recently,
    // then the interactive session was closed, then the graph completed.
    assert_eq!(history[0].name, "Finished terminal");
    assert_eq!(history[1].name, "Past session");
    assert_eq!(history[2].name, "Finished graph");

    // Sessions/graphs for other workdirs or still-active never leak in.
    assert!(!history.iter().any(|e| e.name.contains("other-project")));
    assert!(!history.iter().any(|e| e.name == "Active session"));
    assert!(!history.iter().any(|e| e.name == "Idle terminal"));
}

// ── Archiving (F4): archive/restore preserve identity and history ──────

/// The property that matters most: archiving a graph with real run history,
/// then restoring it, must leave that history byte-for-byte intact — not
/// just flip the `archived` flag. Archiving is deliberately a flag flip, not
/// a delete-and-recreate or a move to another table, so the graph's id and
/// its specs/nodes/runs (all foreign-keyed to that id) never move either.
#[test]
fn archive_then_restore_preserves_identity_and_run_history() {
    let db = test_db();
    let lp = sample_graph("wf-archive");
    let spec = sample_graph_spec(&lp.id, "spec-archive", 1);
    let node = sample_graph_node(&spec.id, "node-archive", 1);
    let run = GraphNodeRun {
        id: "run-archive".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Pass,
        input: Some(serde_json::json!({"feedback": "prior attempt"})),
        output: Some(serde_json::json!({"summary": "diagnosed the failure"})),
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 3,
        pid: None,
        boot_id: Some("boot-archive".to_string()),
        session_id: Some("session-archive".to_string()),
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    let outcome = db.archive_graph(&lp.id).unwrap();
    assert_eq!(
        outcome,
        crate::domain::graphs::ArchiveGraphOutcome::Archived
    );

    // Excluded from the browsing listing (the query itself filters, not an
    // in-memory pass) ...
    assert!(!db
        .list_graphs(None, false)
        .unwrap()
        .iter()
        .any(|l| l.id == lp.id));
    // ... but still resolvable directly by id, and it kept its own id (no
    // delete-and-recreate).
    let archived = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(archived.id, lp.id);
    assert!(archived.archived);
    // ... and included when a caller explicitly asks for archived graphs too.
    assert!(db
        .list_graphs(None, true)
        .unwrap()
        .iter()
        .any(|l| l.id == lp.id));

    // The run history — the entire point of archiving over deleting — is
    // untouched: same spec, same node, same run with its exact payloads.
    let specs = db.list_graph_specs(&lp.id).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].id, spec.id);
    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
    assert_eq!(runs[0].iteration, 3);
    assert_eq!(runs[0].session_id.as_deref(), Some("session-archive"));
    assert_eq!(
        runs[0]
            .output
            .as_ref()
            .and_then(|value| value.get("summary")),
        Some(&serde_json::json!("diagnosed the failure"))
    );

    // Restoring flips the flag back and the graph reappears in the main
    // listing — still the same row, still the same history.
    assert!(db.restore_graph(&lp.id).unwrap());
    let restored = db.get_graph(&lp.id).unwrap().unwrap();
    assert!(!restored.archived);
    assert!(db
        .list_graphs(None, false)
        .unwrap()
        .iter()
        .any(|l| l.id == lp.id));
    let runs_after_restore = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs_after_restore.len(), 1);
    assert_eq!(runs_after_restore[0].id, run.id);
}

#[test]
fn count_archived_graphs_reflects_archive_state() {
    let db = test_db();
    let a = sample_graph("wf-count-a");
    let b = sample_graph("wf-count-b");
    db.insert_graph(&a).unwrap();
    db.insert_graph(&b).unwrap();
    assert_eq!(db.count_archived_graphs().unwrap(), 0);

    db.archive_graph(&a.id).unwrap();
    assert_eq!(db.count_archived_graphs().unwrap(), 1);

    db.archive_graph(&b.id).unwrap();
    assert_eq!(db.count_archived_graphs().unwrap(), 2);

    db.restore_graph(&a.id).unwrap();
    assert_eq!(db.count_archived_graphs().unwrap(), 1);
}

#[test]
fn archive_graph_refuses_a_running_graph() {
    let db = test_db();
    let mut lp = sample_graph("wf-running");
    lp.status = GraphStatus::Running;
    db.insert_graph(&lp).unwrap();

    let outcome = db.archive_graph(&lp.id).unwrap();
    assert_eq!(outcome, crate::domain::graphs::ArchiveGraphOutcome::Running);

    // Refused, not silently ignored: the graph is still in the main listing.
    let reloaded = db.get_graph(&lp.id).unwrap().unwrap();
    assert!(!reloaded.archived);
    assert_eq!(db.count_archived_graphs().unwrap(), 0);
}

#[test]
fn archive_graph_already_archived_reports_already_archived() {
    let db = test_db();
    let lp = sample_graph("wf-double-archive");
    db.insert_graph(&lp).unwrap();

    assert_eq!(
        db.archive_graph(&lp.id).unwrap(),
        crate::domain::graphs::ArchiveGraphOutcome::Archived
    );
    assert_eq!(
        db.archive_graph(&lp.id).unwrap(),
        crate::domain::graphs::ArchiveGraphOutcome::AlreadyArchived
    );
}

#[test]
fn restore_graph_not_archived_is_a_noop() {
    let db = test_db();
    let lp = sample_graph("wf-not-archived");
    db.insert_graph(&lp).unwrap();

    assert!(!db.restore_graph(&lp.id).unwrap());
    assert!(!db.restore_graph("ghost-graph").unwrap());
}

/// Permanent deletion — reachable only from the archive on an
/// already-archived graph — must actually destroy the run history it warns
/// about, unlike archiving. Exercises the same cascade `ON DELETE CASCADE`
/// relationships archiving is designed to never touch.
#[test]
fn permanent_delete_removes_the_graph_and_its_run_history() {
    let db = test_db();
    let lp = sample_graph("wf-permanent-delete");
    let spec = sample_graph_spec(&lp.id, "spec-permanent-delete", 1);
    let node = sample_graph_node(&spec.id, "node-permanent-delete", 1);
    let run = GraphNodeRun {
        id: "run-permanent-delete".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Fail,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();
    db.archive_graph(&lp.id).unwrap();

    db.delete_graph(&lp.id).unwrap();

    assert!(db.get_graph(&lp.id).unwrap().is_none());
    assert!(db.list_graph_specs(&lp.id).unwrap().is_empty());
    assert!(db.get_graph_run(&run.id).unwrap().is_none());
}

/// Older databases predate the `archived` column entirely. Opening one
/// (`Database::new` runs the migration) must add the column, defaulting
/// every existing row to not-archived with no data movement, and the
/// migration must be a no-op on a second open.
#[test]
fn archived_migration_defaults_existing_rows_and_is_idempotent() {
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
                on_completed TEXT,
                auto_continue_at INTEGER,
                auto_continue_action TEXT
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'completed', 0);",
        )
        .expect("seed legacy schema");
    }

    let db = Database::new(&path).expect("open pre-archived db, running migration");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.name, "Legacy");
    assert!(
        !lp.archived,
        "pre-existing rows must default to not archived"
    );

    // The new column is actually usable after migration.
    assert_eq!(
        db.archive_graph("legacy-graph").unwrap(),
        crate::domain::graphs::ArchiveGraphOutcome::Archived
    );
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent), archived state preserved.
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert!(lp.archived);
}

#[test]
fn cross_run_attempts_migration_defaults_existing_rows_and_is_idempotent() {
    // Simulate a pre-C19 database: `graph_specs` without `cross_run_attempts`.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                spec_start_head TEXT,
                workdir TEXT,
                completed_via TEXT,
                completed_via_reason TEXT,
                completed_via_at INTEGER,
                spec_committed_head TEXT
             );
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', NULL, 'Spec', 1, 'failed');",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must add the column
    // without erroring, defaulting the pre-existing row to zero attempts.
    let db = Database::new(&path).expect("open pre-cross_run_attempts db, running migration");
    assert_eq!(
        db.get_graph_spec_cross_run_attempts("legacy-spec").unwrap(),
        0,
        "pre-existing rows must default to zero attempts"
    );

    // The new column is actually usable after migration.
    assert_eq!(
        db.increment_graph_spec_cross_run_attempts("legacy-spec")
            .unwrap(),
        1
    );
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent), the incremented count preserved.
    let db = Database::new(&path).expect("reopen db after migration already applied");
    assert_eq!(
        db.get_graph_spec_cross_run_attempts("legacy-spec").unwrap(),
        1
    );
}

// RETIRED-SCHEMA-NAME-BEGIN
/// Guards the pool-to-queue rename from regressing a third time: the public
/// surface and the schema were already renamed once each, in two separate
/// passes, which is exactly how this codebase ended up with a schema still
/// naming the retired concept. Every `.rs` file under `src/` is scanned for
/// the retired name as a whole word or a `snake_case` suffix; a hit is
/// exempt only between a `RETIRED-SCHEMA-NAME-BEGIN` / `-END` marker pair,
/// which brackets the legacy migration, its tests, and this guard itself —
/// all three must name the retired schema literally to do their job.
#[test]
fn no_retired_schema_name_identifiers_remain_outside_its_migration() {
    // `\b` treats `_` as a word char, so it won't fire between `_` and
    // `pool` (e.g. `test_pool_marker`). Normalizing `_` to a space first
    // turns every snake_case segment boundary into a real `\b`, so one
    // simple pattern catches prefix (`PoolMember`), suffix (`spec_pool`),
    // and mid-identifier (`test_pool_marker`) forms alike.
    let retired_name = regex::Regex::new(r"(?i)\bpool[a-zA-Z]*").unwrap();
    let begin_marker = "RETIRED-SCHEMA-NAME-BEGIN";
    let end_marker = "RETIRED-SCHEMA-NAME-END";
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let src_dir = std::path::Path::new(manifest_dir).join("src");

    let mut violations = Vec::new();
    for entry in walkdir::WalkDir::new(&src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let rel_path = entry
            .path()
            .strip_prefix(manifest_dir)
            .unwrap_or(entry.path())
            .to_path_buf();
        let source = std::fs::read_to_string(entry.path())
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", rel_path.display()));

        let mut exempt = false;
        for (i, line) in source.lines().enumerate() {
            if line.contains(end_marker) {
                exempt = false;
                continue;
            }
            if line.contains(begin_marker) {
                exempt = true;
                continue;
            }
            if exempt {
                continue;
            }
            let normalized = line.replace('_', " ");
            if let Some(m) = retired_name.find(&normalized) {
                violations.push(format!("{}:{} — {}", rel_path.display(), i + 1, m.as_str()));
            }
        }
        assert!(
            !exempt,
            "{} has an unclosed {begin_marker} region",
            rel_path.display()
        );
    }

    assert!(
        violations.is_empty(),
        "retired schema name found outside a RETIRED-SCHEMA-NAME-BEGIN/-END \
         region (queue is the decided term — see migrate_legacy_queue_schema \
         for the one place the retired name may still appear):\n{}",
        violations.join("\n")
    );
}
// RETIRED-SCHEMA-NAME-END

#[test]
fn ensemble_kind_migration_is_idempotent_and_pre_migration_db_opens_cleanly() {
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
                status TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0,
                trigger_kind TEXT,
                trigger_schedule TEXT,
                trigger_path TEXT,
                trigger_events TEXT,
                trigger_debounce_seconds INTEGER,
                trigger_recursive INTEGER,
                on_completed_hook_platform TEXT,
                on_completed_hook_prompt TEXT,
                on_completed_hook_model TEXT,
                on_completed_hook_timeout_minutes INTEGER
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
                spec_start_head TEXT,
                workdir TEXT,
                completed_via TEXT,
                completed_via_reason TEXT,
                completed_via_at INTEGER,
                spec_committed_head TEXT
            );
            CREATE TABLE graph_nodes (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                position INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE graph_edges (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                to_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL
            );
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
                straggler_timeout_minutes INTEGER,
                timeout_minutes INTEGER NOT NULL,
                on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE SET NULL,
                created_at INTEGER NOT NULL,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );",
        )
        .expect("create legacy schema");
    }

    let db = Database::new(&path).expect("open pre-ensemble-kind db, running migration");

    let has_kind: bool = {
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'kind'",
            [],
            |row| Ok(row.get::<_, i32>(0)? > 0),
        )
        .unwrap()
    };
    assert!(has_kind, "migration should have added 'kind' column");

    let has_rri: bool = {
        let conn = rusqlite::Connection::open(&path).expect("reopen");
        conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'round_robin_index'",
            [],
            |row| Ok(row.get::<_, i32>(0)? > 0),
        )
        .unwrap()
    };
    assert!(
        has_rri,
        "migration should have added 'round_robin_index' column"
    );

    drop(db);
    let _db2 = Database::new(&path).expect("reopen after migration — idempotent");
}

#[test]
fn insert_and_collect_subagent_run() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::hours(1)).to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-1",
        platform: "opencode",
        model: Some("model-x"),
        prompt: "do stuff",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &expires,
    })
    .unwrap();
    db.complete_subagent_run("run-1", 0, "hello", "", Some("blind"), &now)
        .unwrap();

    let record = db.collect_subagent_run("run-1").unwrap();
    assert!(record.is_some());
    let r = record.unwrap();
    assert_eq!(r.id, "run-1");
    assert_eq!(r.status, "finished");
    assert_eq!(r.stdout.as_deref(), Some("hello"));

    let second = db.collect_subagent_run("run-1").unwrap();
    assert!(second.is_none(), "row must be deleted after collection");
}

#[test]
fn collect_while_running_does_not_discard_the_result() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::hours(1)).to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-2",
        platform: "opencode",
        model: None,
        prompt: "p",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &expires,
    })
    .unwrap();

    // A poll before the subagent finishes reports "running" but must NOT
    // delete the row — otherwise the eventual result is lost forever.
    let early = db.collect_subagent_run("run-2").unwrap().unwrap();
    assert_eq!(early.status, "running");

    db.complete_subagent_run("run-2", 0, "the answer", "", Some("blind"), &now)
        .unwrap();

    let collected = db.collect_subagent_run("run-2").unwrap().unwrap();
    assert_eq!(collected.status, "finished");
    assert_eq!(collected.stdout.as_deref(), Some("the answer"));
    assert!(
        db.collect_subagent_run("run-2").unwrap().is_none(),
        "row is discarded only after a terminal result is collected"
    );
}

#[test]
fn expire_subagent_runs_deletes_expired() {
    let db = test_db();
    let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let now = Utc::now().to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-exp",
        platform: "opencode",
        model: None,
        prompt: "p",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &past,
    })
    .unwrap();
    let deleted = db.expire_subagent_runs().unwrap();
    assert_eq!(deleted, 1);
    assert!(db.get_subagent_run("run-exp").unwrap().is_none());
}

#[test]
fn expire_subagent_runs_keeps_unexpired() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-keep",
        platform: "opencode",
        model: None,
        prompt: "p",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &future,
    })
    .unwrap();
    let deleted = db.expire_subagent_runs().unwrap();
    assert_eq!(deleted, 0);
    assert!(db.get_subagent_run("run-keep").unwrap().is_some());
}

#[test]
fn fail_subagent_run_sets_status() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::hours(1)).to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-fail",
        platform: "opencode",
        model: None,
        prompt: "p",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &expires,
    })
    .unwrap();
    db.fail_subagent_run("run-fail", "oops", Some("blind"), &now)
        .unwrap();
    let record = db.get_subagent_run("run-fail").unwrap().unwrap();
    assert_eq!(record.status, "failed");
    assert_eq!(record.stderr.as_deref(), Some("oops"));
    assert!(record.exit_code.is_none());
}

// ── CM18: blocking subagent delivery tombstones ─────────────────────────

#[test]
fn tombstone_distinguishes_delivered_from_missing() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();

    // An id that was never delivered has no tombstone.
    assert!(
        !db.is_delivered_tombstone("never-existed").unwrap(),
        "unknown id must not look delivered"
    );

    db.insert_delivered_tombstone("done-1", &now, &future)
        .unwrap();
    assert!(
        db.is_delivered_tombstone("done-1").unwrap(),
        "blocking-delivered id must be distinguishable from missing"
    );
    // Recording a delivery must not create or disturb async rows.
    assert!(db.get_subagent_run("done-1").unwrap().is_none());
}

#[test]
fn expire_cleans_tombstones() {
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let past = (Utc::now() - Duration::hours(1)).to_rfc3339();
    let future = (Utc::now() + Duration::hours(1)).to_rfc3339();

    db.insert_delivered_tombstone("old-1", &now, &past).unwrap();
    db.insert_delivered_tombstone("fresh-1", &now, &future)
        .unwrap();

    db.expire_subagent_runs().unwrap();

    assert!(
        !db.is_delivered_tombstone("old-1").unwrap(),
        "expired tombstone must be cleaned on the same schedule as rows"
    );
    assert!(
        db.is_delivered_tombstone("fresh-1").unwrap(),
        "unexpired tombstone must survive expiry"
    );
}

#[test]
fn blocking_delivery_sequence_collect_reports_delivered_not_missing() {
    // Drives the exact tail `spawn_subagent_blocking` performs after its
    // direct await: the terminal row is collected once (deleted, result
    // returned inline) and a tombstone is left so the next `collect`
    // reports "already delivered" instead of "not found" (FR4).
    let db = test_db();
    let now = Utc::now().to_rfc3339();
    let expires = (Utc::now() + Duration::hours(1)).to_rfc3339();
    db.insert_subagent_run(NewSubagentRun {
        id: "run-block",
        platform: "opencode",
        model: None,
        prompt: "p",
        workdir: "/tmp",
        started_at: &now,
        expires_at: &expires,
    })
    .unwrap();
    db.complete_subagent_run("run-block", 0, "the answer", "", Some("blind"), &now)
        .unwrap();

    // Inline delivery: collect deletes the row and returns the result.
    let delivered = db.collect_subagent_run("run-block").unwrap().unwrap();
    assert_eq!(delivered.status, "finished");
    assert_eq!(delivered.stdout.as_deref(), Some("the answer"));
    db.insert_delivered_tombstone("run-block", &now, &delivered.expires_at)
        .unwrap();

    // No second delivery: the row is gone, but the tombstone says why.
    assert!(db.collect_subagent_run("run-block").unwrap().is_none());
    assert!(db.is_delivered_tombstone("run-block").unwrap());
    // And a genuinely unknown id still has no tombstone.
    assert!(!db.is_delivered_tombstone("run-never").unwrap());
}

#[test]
fn cm9_link_rejects_unknown_relation() {
    let db = test_db();
    let dir = tempdir().unwrap();
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    // create dirs so register works on real paths below; here direct upsert is enough
    let _ = std::fs::create_dir_all(dir.path().join("a"));
    let _ = std::fs::create_dir_all(dir.path().join("b"));
    let hb = cm9_register_at(&db, &dir.path().join("b"));

    let err = db.link_projects(&ha, &hb, "blocks", None).unwrap_err();
    assert!(
        err.to_string().contains("allowed:"),
        "error must name allowed relations, got: {err}"
    );
    for rel in [
        "depends_on",
        "complements",
        "extends",
        "publishes",
        "contains",
        "relates_to",
    ] {
        // contains is rejected at the write path, not the validator; the rest succeed
        if rel == "contains" {
            continue;
        }
        db.link_projects(&ha, &hb, rel, None).unwrap();
    }
}

#[test]
fn cm9_link_rejects_contains() {
    let db = test_db();
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    let hb = cm9_register_at(&db, &dir.path().join("b"));
    let err = db.link_projects(&ha, &hb, "contains", None).unwrap_err();
    assert!(
        err.to_string().contains("derived"),
        "contains guard must mention derived, got: {err}"
    );
}

#[test]
fn cm9_containment_derived_parent_child() {
    let db = test_db();
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("parent")).unwrap();
    std::fs::create_dir_all(root.path().join("parent/child")).unwrap();
    std::fs::create_dir_all(root.path().join("other")).unwrap();
    std::fs::create_dir_all(root.path().join("parent2")).unwrap();
    let hp = cm9_register_at(&db, &root.path().join("parent"));
    let hc = cm9_register_at(&db, &root.path().join("parent/child"));
    let ho = cm9_register_at(&db, &root.path().join("other"));
    let _hp2 = cm9_register_at(&db, &root.path().join("parent2"));

    let n = db.rebuild_containment_edges().unwrap();
    assert_eq!(n, 1, "exactly one contains edge expected");

    let related = db.list_related_projects(&hp, 10).unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].0.project_hash.as_deref(), Some(hc.as_str()));
    assert_eq!(related[0].1.relation, "contains");
    // Parent → child direction: edge stored from container to contained.
    assert_eq!(related[0].1.from_node_id, format!("project:{hp}"));
    assert_eq!(related[0].1.to_node_id, format!("project:{hc}"));

    // Sibling and string-prefix trap get no edges.
    assert!(db.list_related_projects(&ho, 10).unwrap().is_empty());
    let trap: Vec<_> = db
        .list_related_projects(&_hp2, 10)
        .unwrap()
        .into_iter()
        .filter(|(_, e)| e.relation == "contains")
        .collect();
    assert!(trap.is_empty(), "/parent must not contain /parent2");
}

#[test]
fn cm9_containment_recomputed_on_delete_and_remap() {
    let db = test_db();
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("parent")).unwrap();
    std::fs::create_dir_all(root.path().join("parent/child")).unwrap();
    let hp = cm9_register_at(&db, &root.path().join("parent"));
    let hc = cm9_register_at(&db, &root.path().join("parent/child"));
    assert_eq!(db.rebuild_containment_edges().unwrap(), 1);

    // Remap child outside the parent: edge must disappear.
    std::fs::create_dir_all(root.path().join("elsewhere")).unwrap();
    let new_path = root
        .path()
        .join("elsewhere")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    db.remap_project(&hc, &new_path).unwrap();
    let after_remap: Vec<_> = db
        .list_related_projects(&hp, 10)
        .unwrap()
        .into_iter()
        .filter(|(_, e)| e.relation == "contains")
        .collect();
    assert!(
        after_remap.is_empty(),
        "contains edge must be gone after remap"
    );

    // Delete parent: root node gone, no dangling contains edges.
    db.delete_project(&hp).unwrap();
    assert!(db.get_project(&hp).unwrap().is_none());
    let conn = db.conn.lock().unwrap();
    let dangling: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intelligence_edges WHERE relation = 'contains'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dangling, 0);
}

#[test]
fn cm9_traversal_outbound_and_upward() {
    let db = test_db();
    let dir = tempdir().unwrap();
    for sub in ["a", "a/b", "c"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    let hb = cm9_register_at(&db, &dir.path().join("a/b"));
    let hc = cm9_register_at(&db, &dir.path().join("c"));
    db.rebuild_containment_edges().unwrap();
    // A depends_on C.
    db.link_projects(&ha, &hc, "depends_on", None).unwrap();

    // Depth 1 from B: B direct + A via contains. C is two hops away.
    let scope1 = db.traverse_project_scope(&hb, 1).unwrap();
    let mut got: Vec<(&str, Option<&str>)> = scope1
        .projects
        .iter()
        .map(|p| (p.hash.as_str(), p.via_relation.as_deref()))
        .collect();
    got.sort_by_key(|(hash, _)| *hash);
    let mut expected = vec![(ha.as_str(), Some("contains")), (hb.as_str(), None)];
    expected.sort_by_key(|(hash, _)| *hash);
    assert_eq!(got, expected);

    // Depth 2 also reaches C via depends_on.
    let scope2 = db.traverse_project_scope(&hb, 2).unwrap();
    let c_hop = scope2.projects.iter().find(|p| p.hash == hc).unwrap();
    assert_eq!(c_hop.via_relation.as_deref(), Some("depends_on"));
    assert_eq!(c_hop.depth, 2);
}

#[test]
fn cm9_inbound_depends_warns_not_pulls() {
    let db = test_db();
    let dir = tempdir().unwrap();
    for sub in ["a", "d"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    let hd = cm9_register_at(&db, &dir.path().join("d"));
    db.link_projects(&hd, &ha, "depends_on", None).unwrap();

    // Traversal from A (even deep) never pulls D inbound.
    let scope = db.traverse_project_scope(&ha, 5).unwrap();
    assert!(
        scope.projects.iter().all(|p| p.hash != hd),
        "inbound dependent must not be traversed"
    );
    // But the impact warning source lists D.
    let dependents = db.list_project_dependents(&ha).unwrap();
    assert_eq!(dependents.len(), 1);
    assert_eq!(
        dependents[0].from_project_hash.as_deref(),
        Some(hd.as_str())
    );
    assert_eq!(dependents[0].relation, "depends_on");
}

#[test]
fn cm9_complements_bidirectional() {
    let db = test_db();
    let dir = tempdir().unwrap();
    for sub in ["x", "y"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let hx = cm9_register_at(&db, &dir.path().join("x"));
    let hy = cm9_register_at(&db, &dir.path().join("y"));
    db.link_projects(&hx, &hy, "complements", None).unwrap();

    for (from, to) in [(&hx, &hy), (&hy, &hx)] {
        let scope = db.traverse_project_scope(from, 1).unwrap();
        let hop = scope.projects.iter().find(|p| &p.hash == to).unwrap();
        assert_eq!(hop.via_relation.as_deref(), Some("complements"));
    }
}

#[test]
fn cm9_depth_bounded() {
    let db = test_db();
    let dir = tempdir().unwrap();
    for sub in ["a", "b", "c"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    let hb = cm9_register_at(&db, &dir.path().join("b"));
    let hc = cm9_register_at(&db, &dir.path().join("c"));
    db.link_projects(&ha, &hb, "depends_on", None).unwrap();
    db.link_projects(&hb, &hc, "depends_on", None).unwrap();

    let s1 = db.traverse_project_scope(&ha, 1).unwrap();
    assert_eq!(s1.projects.len(), 2, "depth 1 reaches only B");
    let s2 = db.traverse_project_scope(&ha, 2).unwrap();
    assert_eq!(s2.projects.len(), 3, "depth 2 reaches B and C");
    // Depth 99 clamps to MAX_TRAVERSAL_DEPTH (no unbounded walk).
    let s99 = db.traverse_project_scope(&ha, 99).unwrap();
    assert_eq!(s99.projects.len(), 3);
    let s0 = db.traverse_project_scope(&ha, 0).unwrap();
    assert_eq!(s0.projects.len(), 1);
    assert_eq!(s0.reached, 0);
}

#[test]
fn cm9_search_scoped_marks_origin() {
    let db = test_db();
    let dir = tempdir().unwrap();
    for sub in ["a", "b", "c"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let ha = cm9_register_at(&db, &dir.path().join("a"));
    let hb = cm9_register_at(&db, &dir.path().join("b"));
    let hc = cm9_register_at(&db, &dir.path().join("c"));
    db.link_projects(&ha, &hb, "depends_on", None).unwrap();
    cm9_put_fact(&db, "fact-a", &ha, "alpha convention");
    cm9_put_fact(&db, "fact-b", &hb, "alpha convention");
    cm9_put_fact(&db, "fact-c", &hc, "alpha convention");

    let scope = db.traverse_project_scope(&ha, 1).unwrap();
    let hashes: Vec<String> = scope.projects.iter().map(|p| p.hash.clone()).collect();
    let result = db
        .search_intelligence_nodes_scoped("alpha convention", Some("fact"), 10, &hashes)
        .unwrap();
    let mut found: Vec<(&str, &str)> = result
        .results
        .iter()
        .map(|r| (r.id.as_str(), r.project_hash.as_deref().unwrap()))
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![("fact-a", ha.as_str()), ("fact-b", hb.as_str())]
    );

    // Empty hash set yields empty results (never leaks unscoped rows).
    let empty = db
        .search_intelligence_nodes_scoped("alpha", None, 10, &[])
        .unwrap();
    assert!(empty.results.is_empty());
}

#[test]
fn cm9_no_edges_behaves_as_today() {
    let db = test_db();
    let dir = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("lone")).unwrap();
    let hl = cm9_register_at(&db, &dir.path().join("lone"));
    cm9_put_fact(&db, "fact-lone", &hl, "lone knowledge");

    let scope = db.traverse_project_scope(&hl, 1).unwrap();
    assert_eq!(scope.projects.len(), 1);
    assert_eq!(scope.reached, 0);

    let rows = db.list_project_knowledge(&hl, None, 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "fact-lone");
}

#[test]
fn cm9_remap_renames_project_node() {
    let db = test_db();
    let root = tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("old")).unwrap();
    std::fs::create_dir_all(root.path().join("new")).unwrap();
    let old_hash = cm9_register_at(&db, &root.path().join("old"));
    cm9_put_fact(&db, "fact-move", &old_hash, "moving knowledge");

    let new_path = root
        .path()
        .join("new")
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let outcome = db.remap_project(&old_hash, &new_path).unwrap();

    // Old graph root gone, new root present with updated path metadata.
    assert!(db
        .get_intelligence_node(&format!("project:{old_hash}"))
        .unwrap()
        .is_none());
    let node = db
        .get_intelligence_node(&format!("project:{}", outcome.new_hash))
        .unwrap()
        .expect("renamed project node must exist");
    assert_eq!(node.kind, "project");
    assert!(node.metadata.as_deref().unwrap().contains(&new_path));
    // Knowledge re-keyed to the new hash.
    let rows = db
        .list_project_knowledge(&outcome.new_hash, None, 10)
        .unwrap();
    assert!(rows.iter().any(|r| r.id == "fact-move"));
}

#[test]
fn cm9_retype_backlog_nodes() {
    let db = test_db();
    // Exercise the UPDATE path with fixed ids carrying the real prefixes.
    for (id, title) in [
        ("d8c3230b-note", "backlog grooming notes"),
        ("508e398c-note", "sprint backlog notes"),
    ] {
        db.upsert_intelligence_node(IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some("project".to_string()),
            status: None,
            title: Some(title.to_string()),
            body: Some(format!("{title} body text")),
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"origin": "backlog"}))),
            project_hash: Some(Some("hash-backlog".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }
    let n = db.retype_backlog_project_nodes().unwrap();
    assert_eq!(n, 2);
    for id in ["d8c3230b-note", "508e398c-note"] {
        let node = db.get_intelligence_node(id).unwrap().unwrap();
        assert_eq!(node.kind, "fact", "retype must update, not delete");
        assert!(node.title.contains("backlog"));
    }

    // Registry nodes are refused, never retyped.
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("d8c3230b-real".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Real Project".to_string()),
        body: Some("real".to_string()),
        body_replace: None,
        metadata: Some(Some(
            serde_json::json!({"source": "registry", "path": "/x"}),
        )),
        project_hash: Some(Some("hash-real".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    // Prefix d8c3230b is now ambiguous (two matches) → Err, and the real
    // node must be untouched.
    let err = db.retype_backlog_project_nodes().unwrap_err();
    assert!(err.to_string().contains("Ambiguous"));
    assert_eq!(
        db.get_intelligence_node("d8c3230b-real")
            .unwrap()
            .unwrap()
            .kind,
        "project"
    );
}

/// The refusal bail: a node the `d8c3230b` prefix resolves to exactly that
/// carries registry metadata is refused (Err), never retyped.
#[test]
fn cm9_retype_backlog_nodes_refuses_a_registry_project_node() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("d8c3230b".to_string()),
        kind: Some("project".to_string()),
        status: None,
        title: Some("Real Project".to_string()),
        body: Some("real project body".to_string()),
        body_replace: None,
        metadata: Some(Some(
            serde_json::json!({"source": "registry", "path": "/x"}),
        )),
        project_hash: Some(Some("hash-real".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    // Exact id matches without a LIKE scan, so resolution succeeds and the
    // registry guard itself fires.
    let err = db.retype_backlog_project_nodes().unwrap_err();
    assert!(err.to_string().contains("refusing to retype"));
    assert_eq!(
        db.get_intelligence_node("d8c3230b").unwrap().unwrap().kind,
        "project"
    );
}

// ── SPECCODE: spec type / scope / code / commit message ──────────────

/// Two `fix` specs in one workdir get FIX-1 and FIX-2 while a `feat` gets
/// FEAT-1 — the counter is per (workdir, type), not per workdir overall.
#[test]
fn spec_code_numbers_per_type_in_workdir() {
    let db = test_db();
    let workdir = "/tmp/project-a";

    let code1 = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(code1, "FIX-1");

    let code2 = db.next_spec_code(workdir, SpecType::Feat).unwrap();
    assert_eq!(code2, "FEAT-1");

    let code3 = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(code3, "FIX-2");
}

/// A deleted spec's number is never reused — the counter only moves
/// forward, even after a deletion.
#[test]
fn spec_code_not_reused_after_delete() {
    let db = test_db();
    let workdir = "/tmp/project-b";

    let fix1 = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(fix1, "FIX-1");
    let fix2 = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(fix2, "FIX-2");

    // Simulate the deleted spec's slot being gone — the next number must not
    // fill the gap.
    let fix3 = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(fix3, "FIX-3");
}

/// Another workdir starts its own counter at 1 for each type.
#[test]
fn spec_code_workdir_independent() {
    let db = test_db();

    let code_a = db.next_spec_code("/tmp/proj-a", SpecType::Fix).unwrap();
    assert_eq!(code_a, "FIX-1");

    let code_b = db.next_spec_code("/tmp/proj-b", SpecType::Fix).unwrap();
    assert_eq!(code_b, "FIX-1");

    let code_a2 = db.next_spec_code("/tmp/proj-a", SpecType::Fix).unwrap();
    assert_eq!(code_a2, "FIX-2");
}

/// A spec's code is accepted wherever a spec id is (requirement 6).
#[test]
fn resolve_spec_id_by_prefix_resolves_code() {
    let db = test_db();
    let workdir = "/tmp/project-c";

    let code = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    assert_eq!(code, "FIX-1");

    let mut spec = sample_standalone_spec("spec-code-resolve", Some(workdir));
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    spec.spec_code = Some(code);
    db.insert_graph_spec(&spec).unwrap();

    let resolved = db.resolve_spec_id_by_prefix("FIX-1", None).unwrap();
    assert_eq!(resolved, Some("spec-code-resolve".to_string()));

    let fetched = db.get_graph_spec("spec-code-resolve").unwrap().unwrap();
    assert_eq!(fetched.spec_type, Some(SpecType::Fix));
    assert_eq!(fetched.spec_scope.as_deref(), Some("tui"));
    assert_eq!(fetched.spec_code.as_deref(), Some("FIX-1"));
}

/// Codes are per-(workdir, type), so `FIX-1` exists in two workdirs at once:
/// scoped resolution returns each workdir's own spec, and an unscoped lookup
/// refuses instead of silently picking one.
#[test]
fn resolve_spec_code_scoped_to_workdir() {
    let db = test_db();

    for (idx, workdir) in ["/tmp/project-a", "/tmp/project-b"].iter().enumerate() {
        let code = db.next_spec_code(workdir, SpecType::Fix).unwrap();
        assert_eq!(code, "FIX-1");
        let mut spec = sample_standalone_spec(&format!("spec-{idx}"), Some(workdir));
        spec.spec_type = Some(SpecType::Fix);
        spec.spec_code = Some(code);
        db.insert_graph_spec(&spec).unwrap();
    }

    let resolved_a = db
        .resolve_spec_id_by_prefix("FIX-1", Some("/tmp/project-a"))
        .unwrap();
    assert_eq!(resolved_a, Some("spec-0".to_string()));
    let resolved_b = db
        .resolve_spec_id_by_prefix("FIX-1", Some("/tmp/project-b"))
        .unwrap();
    assert_eq!(resolved_b, Some("spec-1".to_string()));

    // Without a workdir the shared code is ambiguous: refuse, don't guess.
    assert!(db.resolve_spec_id_by_prefix("FIX-1", None).is_err());
}

/// A legacy spec (no type) returns empty type/scope/code — the fields exist
/// but hold `NULL`.
#[test]
fn legacy_spec_has_empty_type_scope_code() {
    let db = test_db();
    let spec = sample_standalone_spec("legacy-empty", None);
    db.insert_graph_spec(&spec).unwrap();

    let fetched = db.get_graph_spec("legacy-empty").unwrap().unwrap();
    assert_eq!(fetched.spec_type, None);
    assert_eq!(fetched.spec_scope, None);
    assert_eq!(fetched.spec_code, None);
}

/// Export → import (here: persist → read back) keeps type, scope and code.
/// A spec with a type keeps its code through a DB round-trip; import never
/// renumbers.
#[test]
fn spec_persists_type_scope_code_through_db_round_trip() {
    let db = test_db();
    let workdir = "/tmp/project-d";

    let code = db.next_spec_code(workdir, SpecType::Fix).unwrap();
    let mut spec = sample_standalone_spec("round-trip", Some(workdir));
    spec.spec_type = Some(SpecType::Fix);
    spec.spec_scope = Some("tui".to_string());
    spec.spec_code = Some(code);
    db.insert_graph_spec(&spec).unwrap();

    // Read back: type, scope, code all survive.
    let fetched = db.get_graph_spec("round-trip").unwrap().unwrap();
    assert_eq!(fetched.spec_type, Some(SpecType::Fix));
    assert_eq!(fetched.spec_scope.as_deref(), Some("tui"));
    assert_eq!(fetched.spec_code.as_deref(), Some("FIX-1"));

    // list_specs with a type filter sees only matching specs.
    let only_fix = db
        .list_specs(Some(workdir), None, false, Some(SpecType::Fix))
        .unwrap();
    assert_eq!(only_fix.len(), 1);
    assert_eq!(only_fix[0].spec_code.as_deref(), Some("FIX-1"));

    let no_fix = db
        .list_specs(Some(workdir), None, false, Some(SpecType::Feat))
        .unwrap();
    assert!(no_fix.is_empty(), "no feat specs in this workdir");
}
