//! Part 06 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

/// A pre-migration database (graph_specs without spec_type/scope/code
/// columns, no spec_code_counters table, no graphs.require_spec_type
/// column) opens unchanged — the migration adds the columns with NULL
/// defaults and the counter table.
#[test]
fn spec_type_migration_opens_pre_migration_db_unchanged() {
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
                spec_start_dirty INTEGER,
                workdir TEXT,
                spec_committed_head TEXT,
                spec_end_dirty INTEGER,
                spec_end_dirty_paths TEXT,
                completed_via TEXT,
                completed_via_reason TEXT,
                completed_via_at INTEGER
            );
            INSERT INTO graph_specs (id, graph_id, name, position, status, workdir)
                VALUES ('legacy-spec', NULL, 'Spec', 1, 'pending', '/tmp');",
        )
        .expect("seed legacy schema");

        let has_column = |col: &str| -> bool {
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = ?1",
                rusqlite::params![col],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
                == 1
        };
        assert!(!has_column("spec_type"));
        assert!(!has_column("spec_scope"));
        assert!(!has_column("spec_code"));
    }

    // Opening the DB (Database::new runs migrate_spec_type_columns) must
    // succeed without error.
    let db = Database::new(&path).expect("open pre-spec_type db, running migration");

    // The legacy row survived and got NULLs for the new columns.
    let spec = db.get_graph_spec("legacy-spec").unwrap().unwrap();
    assert_eq!(spec.name, "Spec");
    assert_eq!(spec.spec_type, None);
    assert_eq!(spec.spec_scope, None);
    assert_eq!(spec.spec_code, None);

    // The counter table works after migration.
    let code = db.next_spec_code("/tmp", SpecType::Fix).unwrap();
    assert_eq!(code, "FIX-1");
}

/// T1: a run records the resolved pair, readable back from the row itself —
/// no join to the node's config.
#[test]
fn cb43_run_records_resolved_pair() {
    let db = test_db();
    let (_, _, run_id) = cb43_setup(&db, (Some("opencode"), Some("opencode/big-pickle")));
    let run = db.get_graph_run(&run_id).unwrap().unwrap();
    assert_eq!(run.executed_platform.as_deref(), Some("opencode"));
    assert_eq!(run.executed_model.as_deref(), Some("opencode/big-pickle"));
}

/// T2: when the platform cannot select a model the stored model is `None`,
/// not the requested string (the `model_not_applied` path stores honestly).
#[test]
fn cb43_unapplied_model_records_none() {
    let db = test_db();
    let (_, _, run_id) = cb43_setup(&db, (Some("antigravity"), None));
    let run = db.get_graph_run(&run_id).unwrap().unwrap();
    assert_eq!(run.executed_platform.as_deref(), Some("antigravity"));
    assert_eq!(run.executed_model, None);
}

/// T3/T9: `list_graph_node_runs` carries the pair; pre-migration rows (NULL)
/// read back as `None`.
#[test]
fn cb43_list_returns_pair_and_null_stays_null() {
    let db = test_db();
    let (graph_id, spec_id, _) = cb43_setup(&db, (Some("opencode"), Some("m")));
    let node = sample_graph_node(&spec_id, "node-cb43-b", 2);
    db.insert_graph_node(&node).unwrap();
    // Pre-migration row: both columns NULL.
    db.insert_graph_run(&cb43_node_run(
        &graph_id,
        &spec_id,
        &node.id,
        None,
        None,
        GraphRunStatus::Pass,
        Utc::now(),
    ))
    .unwrap();

    let runs = db
        .list_graph_node_runs(&graph_id, None, None, 20, 0)
        .unwrap();
    assert_eq!(runs.len(), 2);
    let with_pair = runs
        .iter()
        .find(|r| r.executed_platform.is_some())
        .expect("pair row present");
    assert_eq!(with_pair.executed_model.as_deref(), Some("m"));
    let legacy = runs
        .iter()
        .find(|r| r.executed_platform.is_none())
        .expect("legacy row present");
    assert_eq!(legacy.executed_model, None);
}

/// T5: the 30-day window excludes older pairs.
#[test]
fn cb43_recent_usage_honours_thirty_day_window() {
    let db = test_db();
    let (graph_id, spec_id, _) = cb43_setup(&db, (Some("opencode"), Some("fresh")));
    let node = db
        .get_graph_node("node-cb43")
        .unwrap()
        .expect("fixture node");
    let now = Utc::now();
    db.insert_graph_run(&cb43_node_run(
        &graph_id,
        &spec_id,
        &node.id,
        Some("opencode"),
        Some("stale"),
        GraphRunStatus::Pass,
        now - Duration::days(31),
    ))
    .unwrap();

    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    let models: Vec<_> = rows
        .iter()
        .map(|r| r.model.clone().unwrap_or_default())
        .collect();
    assert!(models.contains(&"fresh".to_string()));
    assert!(!models.contains(&"stale".to_string()));
}

/// T6: capped at fifteen, most-recent-first.
#[test]
fn cb43_recent_usage_caps_at_fifteen_ordered_by_recency() {
    let db = test_db();
    let (graph_id, spec_id, _) = cb43_setup(&db, (Some("opencode"), Some("model-00")));
    let node = db
        .get_graph_node("node-cb43")
        .unwrap()
        .expect("fixture node");
    let now = Utc::now();
    for i in 1..20 {
        db.insert_graph_run(&cb43_node_run(
            &graph_id,
            &spec_id,
            &node.id,
            Some("opencode"),
            Some(&format!("model-{i:02}")),
            GraphRunStatus::Pass,
            now - Duration::minutes(i as i64),
        ))
        .unwrap();
    }
    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    assert_eq!(rows.len(), 15);
    // Most recent first: model-00 (now) leads, then model-01, ...
    assert_eq!(rows[0].model.as_deref(), Some("model-00"));
    assert_eq!(rows[1].model.as_deref(), Some("model-01"));
    for pair in rows.windows(2) {
        assert!(pair[0].last_run >= pair[1].last_run);
    }
}

/// T7: the section shows how the most recent run ended, not a frequency vote.
#[test]
fn cb43_recent_usage_reports_last_outcome() {
    let db = test_db();
    // NB: the setup row itself is the newest run in the DB, so it must carry
    // a different pair than the one under test.
    let (graph_id, spec_id, _) = cb43_setup(&db, (Some("opencode"), Some("setup-filler")));
    let node = db
        .get_graph_node("node-cb43")
        .unwrap()
        .expect("fixture node");
    let now = Utc::now();
    for (mins_ago, status) in [
        (120, GraphRunStatus::Pass),
        (60, GraphRunStatus::Fail),
        (30, GraphRunStatus::Fail),
    ] {
        db.insert_graph_run(&cb43_node_run(
            &graph_id,
            &spec_id,
            &node.id,
            Some("opencode"),
            Some("flaky"),
            status,
            now - Duration::minutes(mins_ago),
        ))
        .unwrap();
    }
    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    let flaky = rows
        .iter()
        .find(|r| r.model.as_deref() == Some("flaky"))
        .expect("flaky present");
    assert!(flaky.count >= 3);
    assert_eq!(flaky.last_outcome, "fail");

    // A newer pass flips the outcome without changing the count's meaning.
    db.insert_graph_run(&cb43_node_run(
        &graph_id,
        &spec_id,
        &node.id,
        Some("opencode"),
        Some("flaky"),
        GraphRunStatus::Pass,
        now - Duration::minutes(5),
    ))
    .unwrap();
    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    let flaky = rows
        .iter()
        .find(|r| r.model.as_deref() == Some("flaky"))
        .expect("flaky present");
    assert_eq!(flaky.last_outcome, "pass");
}

/// T8: a pair never run here is absent.
#[test]
fn cb43_never_run_pair_is_absent() {
    let db = test_db();
    cb43_setup(&db, (Some("opencode"), Some("only-a")));
    let rows = db
        .list_recent_platform_model_usage(Utc::now() - Duration::days(30), 15)
        .unwrap();
    assert!(rows.iter().all(|r| r.model.as_deref() != Some("b")));
    assert!(rows.iter().any(|r| r.model.as_deref() == Some("only-a")));
}

/// T9 (usage side): pre-migration rows are omitted, not invented.
#[test]
fn cb43_null_rows_omitted_from_recent_usage() {
    let db = test_db();
    let (graph_id, spec_id, _) = cb43_setup(&db, (Some("opencode"), Some("real")));
    let node = db
        .get_graph_node("node-cb43")
        .unwrap()
        .expect("fixture node");
    db.insert_graph_run(&cb43_node_run(
        &graph_id,
        &spec_id,
        &node.id,
        None,
        None,
        GraphRunStatus::Pass,
        Utc::now(),
    ))
    .unwrap();
    let rows = db
        .list_recent_platform_model_usage(Utc::now() - Duration::days(30), 15)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model.as_deref(), Some("real"));
}

/// T10: every run kind contributes to the section.
#[test]
fn cb43_all_run_kinds_contribute() {
    let db = test_db();
    let (graph_id, _, _) = cb43_setup(&db, (Some("p-graph"), Some("m-graph")));
    let now = Utc::now();

    // Hook run.
    db.insert_graph_completion_hook_run(&crate::domain::graphs::GraphCompletionHookRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id,
        event: crate::domain::graphs::GraphHookEvent::OnCompleted,
        hook_index: 0,
        status: GraphRunStatus::Pass,
        output: None,
        summary: None,
        started_at: now,
        completed_at: Some(now),
        pid: None,
        boot_id: None,
        executed_platform: Some("p-hook".to_string()),
        executed_model: Some("m-hook".to_string()),
        hook_config: None,
    })
    .unwrap();

    // Background agent run.
    db.upsert_agent(&sample_cron_agent("bg-cb43")).unwrap();
    db.insert_run(&RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "bg-cb43".to_string(),
        status: RunStatus::Success,
        trigger_type: TriggerType::Manual,
        summary: None,
        started_at: now,
        finished_at: Some(now),
        exit_code: Some(0),
        timeout_at: None,
        executed_platform: Some("p-bg".to_string()),
        executed_model: Some("m-bg".to_string()),
    })
    .unwrap();

    // Subagent run.
    db.insert_subagent_run(NewSubagentRun {
        id: &uuid::Uuid::new_v4().to_string(),
        platform: "p-sub",
        model: Some("m-sub"),
        prompt: "prompt",
        workdir: "/tmp",
        started_at: &now.to_rfc3339(),
        expires_at: &(now + Duration::hours(1)).to_rfc3339(),
    })
    .unwrap();

    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    let platforms: Vec<_> = rows.iter().map(|r| r.platform.as_str()).collect();
    for expected in ["p-graph", "p-hook", "p-bg", "p-sub"] {
        assert!(
            platforms.contains(&expected),
            "missing {expected} in {platforms:?}"
        );
    }
}

#[test]
fn cb46_sources_keep_every_run_kind_attribution() {
    let db = test_db();
    let (graph_id, _, _) = cb43_setup(&db, (Some("graph-platform"), Some("graph-model")));
    let now = Utc::now();
    db.insert_graph_completion_hook_run(&crate::domain::graphs::GraphCompletionHookRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: graph_id.clone(),
        event: crate::domain::graphs::GraphHookEvent::OnCompleted,
        hook_index: 0,
        status: GraphRunStatus::Pass,
        output: None,
        summary: None,
        started_at: now,
        completed_at: Some(now),
        pid: None,
        boot_id: None,
        executed_platform: Some("hook-platform".into()),
        executed_model: Some("hook-model".into()),
        hook_config: None,
    })
    .unwrap();
    db.upsert_agent(&sample_cron_agent("cb46-agent")).unwrap();
    db.insert_run(&RunLog {
        id: uuid::Uuid::new_v4().to_string(),
        background_agent_id: "cb46-agent".into(),
        status: RunStatus::Success,
        trigger_type: TriggerType::Manual,
        summary: None,
        started_at: now,
        finished_at: Some(now),
        exit_code: Some(0),
        timeout_at: None,
        executed_platform: Some("agent-platform".into()),
        executed_model: Some("agent-model".into()),
    })
    .unwrap();
    db.insert_subagent_run(NewSubagentRun {
        id: &uuid::Uuid::new_v4().to_string(),
        platform: "sub-platform",
        model: Some("sub-model"),
        prompt: "prompt",
        workdir: "/tmp/cb46",
        started_at: &now.to_rfc3339(),
        expires_at: &(now + Duration::hours(1)).to_rfc3339(),
    })
    .unwrap();

    let rows = db
        .list_recent_platform_model_usage_sources(now - Duration::days(30))
        .unwrap();
    assert!(rows.iter().any(|row| matches!(row.source,
        crate::db::graphs::RecentPairSourceKind::GraphNode { graph_id: ref id, .. } if id == &graph_id)));
    assert!(rows.iter().any(|row| matches!(row.source,
        crate::db::graphs::RecentPairSourceKind::CompletionHook { graph_id: ref id, hook_index: 0, .. } if id == &graph_id)));
    assert!(rows.iter().any(|row| matches!(row.source,
        crate::db::graphs::RecentPairSourceKind::BackgroundAgent { ref agent_id, .. } if agent_id == "cb46-agent")));
    assert!(rows.iter().any(|row| matches!(row.source,
        crate::db::graphs::RecentPairSourceKind::SubagentSpawn { ref workdir } if workdir == "/tmp/cb46")));
}

/// Rows whose RFC3339 `started_at` cannot be parsed are dropped, never fatal.
#[test]
fn cb43_rfc3339_usage_rows_with_unparseable_started_at_are_dropped() {
    let db = test_db();
    let now = Utc::now();
    db.upsert_agent(&sample_cron_agent("bg-cb43-bad-ts"))
        .unwrap();
    let good_ts = now.to_rfc3339();
    db.exec_test_sql(&format!(
        "INSERT INTO runs (id, background_agent_id, status, trigger_type, summary, started_at, finished_at, exit_code, timeout_at, executed_platform, executed_model) VALUES \
         ('run-bad-ts', 'bg-cb43-bad-ts', 'success', 'manual', NULL, 'not-a-timestamp', NULL, 0, NULL, 'p-bg', 'm-bg'), \
         ('run-good-ts', 'bg-cb43-bad-ts', 'success', 'manual', NULL, '{good_ts}', NULL, 0, NULL, 'p-bg', 'm-bg');"
    ))
    .unwrap();
    let rows = db
        .list_recent_platform_model_usage(now - Duration::days(30), 15)
        .unwrap();
    let pair = rows
        .iter()
        .find(|r| r.platform == "p-bg" && r.model.as_deref() == Some("m-bg"))
        .expect("well-formed row present");
    assert_eq!(pair.count, 1);
}

/// CB56 regression guard: every `CREATE INDEX` in the base schema batch
/// (the first `execute_batch` call in `Database::init`, covering `agents`
/// through `sandbox_runs`) must reference only columns that batch's own
/// `CREATE TABLE` for that table declares. CB56 shipped because an index
/// referenced a column added by a *later* migration — on a pre-existing
/// database, `CREATE TABLE IF NOT EXISTS` is a no-op, so the column didn't
/// exist yet and the index creation failed with `no such column`. Every
/// test started from an empty database, where `CREATE TABLE` always
/// carries every column, so nothing caught it. This test parses the batch
/// text itself (via `include_str!`, not a hand-copied mirror) so it can't
/// drift from what `init()` actually executes.
#[test]
fn base_schema_batch_indexes_reference_only_columns_in_same_batch() {
    let source = include_str!("../mod.rs");
    let batch_start = source.find("\"CREATE TABLE IF NOT EXISTS agents (").expect(
        "base schema batch start marker not found in mod.rs — did the batch move or get reordered?",
    );
    let close_marker = "\n            );\",";
    let close_rel = source[batch_start..]
        .find(close_marker)
        .expect("base schema batch end marker not found in mod.rs — did the batch move?");
    let sql_close_offset = close_marker.find(");").unwrap() + 2;
    let batch_end = batch_start + close_rel + sql_close_offset;
    let batch = &source[batch_start..batch_end];

    // table name -> declared column names
    let table_re = regex::Regex::new(r"CREATE TABLE IF NOT EXISTS (\w+) \(").unwrap();
    let mut table_columns: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for caps in table_re.captures_iter(batch) {
        let table = caps[1].to_string();
        let match_end = caps.get(0).unwrap().end(); // one past the '(' of this CREATE TABLE
        let open_paren = match_end - 1;
        let close_paren = find_matching_paren(batch, open_paren);
        let body = &batch[open_paren + 1..close_paren];
        let cols: Vec<String> = split_top_level_commas(body)
            .into_iter()
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .filter(|item| {
                let upper = item.to_uppercase();
                !(upper.starts_with("CHECK")
                    || upper.starts_with("FOREIGN KEY")
                    || upper.starts_with("PRIMARY KEY")
                    || upper.starts_with("UNIQUE ("))
            })
            .map(|item| item.split_whitespace().next().unwrap().to_string())
            .collect();
        table_columns.insert(table, cols);
    }
    assert!(
        table_columns.len() > 10,
        "expected dozens of CREATE TABLE statements in the base batch, found {} — the table_re regex may not be matching",
        table_columns.len()
    );

    // (table, [columns]) for every CREATE INDEX / CREATE UNIQUE INDEX in the batch
    let index_re =
        regex::Regex::new(r"CREATE (?:UNIQUE )?INDEX IF NOT EXISTS \w+\s+ON (\w+)\(([^()]*)\)")
            .unwrap();
    let mut checked = 0;
    for caps in index_re.captures_iter(batch) {
        let table = &caps[1];
        let table_cols = table_columns.get(table).unwrap_or_else(|| {
            panic!(
                "index references table `{table}` but no CREATE TABLE IF NOT EXISTS {table} found in the same base schema batch"
            )
        });
        for raw_col in caps[2].split(',') {
            let col = raw_col.trim();
            let col = col
                .strip_suffix("DESC")
                .or_else(|| col.strip_suffix("ASC"))
                .unwrap_or(col)
                .trim();
            assert!(
                table_cols.iter().any(|c| c == col),
                "CREATE INDEX on {table}({col}) but the batch's CREATE TABLE {table} declares only {table_cols:?} — \
                 an index must never reference a column added by a later migration (CB56)"
            );
            checked += 1;
        }
    }
    assert!(
        checked > 15,
        "expected to check many indexed columns across the batch, only checked {checked} — the index_re regex may not be matching"
    );
}

#[test]
fn pre_ct17_intelligence_nodes_without_content_touched_at_opens_and_migrates() {
    // CB56: a database created before CT17 (2026-09-18) has this exact
    // `intelligence_nodes` shape — no `content_touched_at` column. Opening
    // it must succeed (the base batch must not reference a column this
    // table doesn't have yet), and the CT17 migration further down in
    // `init()` must then add the column and its index.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw pre-CT17 db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS intelligence_nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'noted',
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             INSERT INTO intelligence_nodes
                 (id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at)
                 VALUES ('node1', 'note', 'noted', 'Pre-CT17 title', 'Pre-CT17 body', NULL, 'ph1', 'sess1', 1000, 1000);",
        )
        .expect("seed pre-CT17 intelligence_nodes");
    }

    let db = Database::new(&path)
        .expect("open pre-CT17 db — must not fail with 'no such column: content_touched_at'");

    let conn = rusqlite::Connection::open(&path).expect("reopen raw db for assertions");
    let has_column: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('intelligence_nodes') WHERE name = 'content_touched_at'",
            [],
            |row| Ok(row.get::<_, i32>(0)? > 0),
        )
        .unwrap();
    assert!(
        has_column,
        "content_touched_at column must exist after opening"
    );

    let has_index: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_intelligence_nodes_project_hash_touched'",
            [],
            |row| Ok(row.get::<_, i32>(0)? > 0),
        )
        .unwrap();
    assert!(
        has_index,
        "idx_intelligence_nodes_project_hash_touched index must exist after opening"
    );

    let title: String = conn
        .query_row(
            "SELECT title FROM intelligence_nodes WHERE id = 'node1'",
            [],
            |row| row.get(0),
        )
        .expect("pre-existing row must still be readable");
    assert_eq!(title, "Pre-CT17 title");

    drop(db);
}

#[test]
fn v3_close_schema_with_break_condition_migrates_loops_to_graphs() {
    // The shape the machine actually had on 2026-09-21 (v3 close, 7cd92e1,
    // 2026-09-15): loop_*/loops naming, a 'break' edge condition, no
    // content_touched_at. Database::new must run migrate_loop_to_graph_schema
    // (rename loops->graphs etc.) and every later guarded ALTER cleanly.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw v3-close db");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                cli TEXT NOT NULL,
                model TEXT,
                effort TEXT,
                working_dir TEXT,
                enabled BOOLEAN NOT NULL DEFAULT 1,
                enable_at TEXT,
                created_at TEXT NOT NULL,
                log_path TEXT NOT NULL,
                timeout_minutes INTEGER NOT NULL DEFAULT 15,
                expires_at TEXT,
                last_run_at TEXT,
                last_run_ok BOOLEAN,
                last_triggered_at TEXT,
                trigger_count INTEGER NOT NULL DEFAULT 0,
                notify_on_success BOOLEAN NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                background_agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                trigger_type TEXT NOT NULL,
                summary TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                exit_code INTEGER,
                timeout_at TEXT,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE TABLE IF NOT EXISTS daemon_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS interactive_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cli TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                args TEXT,
                started_at TEXT NOT NULL,
                exited_at TEXT,
                exit_code INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                session_type TEXT NOT NULL DEFAULT 'interactive',
                pid INTEGER,
                boot_id TEXT
            );

            CREATE TABLE IF NOT EXISTS terminal_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                shell TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_active TEXT,
                status TEXT NOT NULL DEFAULT 'idle'
            );

            CREATE INDEX IF NOT EXISTS idx_interactive_sessions_workdir
                ON interactive_sessions(working_dir, started_at DESC);

            CREATE INDEX IF NOT EXISTS idx_terminal_sessions_workdir
                ON terminal_sessions(working_dir, created_at DESC);

            CREATE TABLE IF NOT EXISTS groups (
                id TEXT PRIMARY KEY,
                orientation TEXT NOT NULL DEFAULT 'horizontal',
                session_a TEXT NOT NULL,
                session_b TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                source TEXT NOT NULL,
                source_id TEXT,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_activity_log_workdir_created
                ON activity_log(workdir, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_log_kind
                ON activity_log(kind);

            CREATE TABLE IF NOT EXISTS sync_locks (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                lock_type TEXT NOT NULL,
                resource TEXT NOT NULL,
                acquired_at INTEGER NOT NULL,
                expires_at INTEGER,
                released_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS projects (
                hash TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                tags TEXT,
                indexed_at INTEGER,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_queue (
                source_path TEXT NOT NULL PRIMARY KEY,
                status TEXT NOT NULL,
                queued_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_file_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL,
                event_type TEXT NOT NULL,
                detail TEXT,
                occurred_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rag_file_events_path
                ON rag_file_events(file_path);

            CREATE TABLE IF NOT EXISTS intelligence_nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'noted',
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_kind_updated
                ON intelligence_nodes(kind, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_project_hash
                ON intelligence_nodes(project_hash);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_session_id
                ON intelligence_nodes(session_id);

            CREATE TABLE IF NOT EXISTS intelligence_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_node_id TEXT NOT NULL,
                to_node_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                weight REAL NOT NULL DEFAULT 1.0,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(from_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE,
                FOREIGN KEY(to_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_from
                ON intelligence_edges(from_node_id);
            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_to
                ON intelligence_edges(to_node_id);

            CREATE TABLE IF NOT EXISTS operational_sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_operational_sessions_project_hash
                ON operational_sessions(project_hash);
            CREATE INDEX IF NOT EXISTS idx_operational_sessions_updated
                ON operational_sessions(updated_at DESC);

            CREATE TABLE IF NOT EXISTS loops (
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
                auto_continue_action TEXT,
                hooks TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_loops_workdir_created
                ON loops(workdir, created_at DESC);

            CREATE TABLE IF NOT EXISTS loop_specs (
                id TEXT PRIMARY KEY,
                loop_id TEXT REFERENCES loops(id) ON DELETE CASCADE,
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
                spec_committed_head TEXT,
                cross_run_attempts INTEGER NOT NULL DEFAULT 0
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_loop_specs_position
                ON loop_specs(loop_id, position);

            CREATE TABLE IF NOT EXISTS loop_nodes (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES loop_specs(id) ON DELETE CASCADE,
                loop_id TEXT REFERENCES loops(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                position INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                CHECK ((spec_id IS NULL) <> (loop_id IS NULL))
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_loop_nodes_position
                ON loop_nodes(spec_id, position);

            CREATE TABLE IF NOT EXISTS loop_edges (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES loop_specs(id) ON DELETE CASCADE,
                loop_id TEXT REFERENCES loops(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                to_node TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL,
                route TEXT,
                CHECK ((spec_id IS NULL) <> (loop_id IS NULL))
            );

            CREATE INDEX IF NOT EXISTS idx_loop_edges_spec_from
                ON loop_edges(spec_id, from_node);

            CREATE TABLE IF NOT EXISTS loop_runs (
                id TEXT PRIMARY KEY,
                loop_id TEXT NOT NULL REFERENCES loops(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES loop_specs(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                input TEXT,
                output TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                iteration INTEGER NOT NULL DEFAULT 1,
                pid INTEGER,
                boot_id TEXT,
                session_id TEXT,
                paused_through INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_loop_runs_spec_started
                ON loop_runs(spec_id, started_at ASC);

            CREATE INDEX IF NOT EXISTS idx_loop_runs_node_iteration
                ON loop_runs(node_id, iteration DESC);

            -- Speeds the sidebar's last-activity-per-loop aggregate
            -- (MAX(started_at) GROUP BY loop_id) into a loose index scan
            -- instead of a full table scan.
            CREATE INDEX IF NOT EXISTS idx_loop_runs_loop_started
                ON loop_runs(loop_id, started_at DESC);

            -- N2: firings of a loop's `on_completed` hook. Deliberately not
            -- `loop_runs` — that table's spec_id/node_id are NOT NULL FKs into
            -- a spec's graph, which a completion hook (no spec, no graph node)
            -- can never satisfy.
            CREATE TABLE IF NOT EXISTS loop_completion_hook_runs (
                id TEXT PRIMARY KEY,
                loop_id TEXT NOT NULL REFERENCES loops(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                output TEXT,
                summary TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                pid INTEGER,
                boot_id TEXT,
                event TEXT NOT NULL DEFAULT 'on_completed',
                hook_index INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_loop_completion_hook_runs_loop_started
                ON loop_completion_hook_runs(loop_id, started_at ASC);

            -- F1: an ensemble unit -- the members and join are ordinary
            -- loop_nodes/loop_edges rows (the engine's graph-walking code is
            -- reused as-is); this row is what lets loop_get/loop_update_ensemble
            -- address the whole ensemble as one thing instead of N+1 nodes.
            CREATE TABLE IF NOT EXISTS ensembles (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES loop_specs(id) ON DELETE CASCADE,
                loop_id TEXT REFERENCES loops(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                prompt_template TEXT NOT NULL,
                join_node_id TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                entry_from_node TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                entry_condition TEXT NOT NULL,
                min_pass INTEGER NOT NULL,
                straggler_timeout_minutes INTEGER,
                timeout_minutes INTEGER NOT NULL,
                on_pass_to TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                on_fail_to TEXT REFERENCES loop_nodes(id) ON DELETE SET NULL,
                created_at INTEGER NOT NULL,
                CHECK ((spec_id IS NULL) <> (loop_id IS NULL))
            );

            CREATE TABLE IF NOT EXISTS ensemble_members (
                ensemble_id TEXT NOT NULL REFERENCES ensembles(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES loop_nodes(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                platform TEXT NOT NULL,
                model TEXT,
                prompt_override TEXT,
                PRIMARY KEY (ensemble_id, node_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_ensemble_members_position
                ON ensemble_members(ensemble_id, position);

            CREATE INDEX IF NOT EXISTS idx_ensemble_members_node
                ON ensemble_members(node_id);

            CREATE INDEX IF NOT EXISTS idx_ensembles_join_node
                ON ensembles(join_node_id);

            -- F1: ensemble blueprints -- a whole ensemble's shared prompt +
            -- member list (unlike `blueprints`, which templates a single
            -- node), seeded with the builtin ensemble-proposers pattern.
            CREATE TABLE IF NOT EXISTS ensemble_blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                prompt_template TEXT NOT NULL,
                members TEXT NOT NULL,
                min_pass INTEGER,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queues (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_members (
                queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES loop_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                group_name TEXT,
                PRIMARY KEY (queue_id, spec_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_members_position
                ON queue_members(queue_id, position);

            CREATE TABLE IF NOT EXISTS seed_sessions (
                session_id TEXT PRIMARY KEY,
                seed_id TEXT NOT NULL,
                bound_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES interactive_sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_seed_sessions_seed
                ON seed_sessions(seed_id);

            CREATE TABLE IF NOT EXISTS blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                provenance TEXT
            );

            CREATE TABLE IF NOT EXISTS failed_scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                workdir TEXT,
                failed_at INTEGER NOT NULL,
                provenance TEXT
            );

            -- U8: the prompt builder's last-sent prompt per project, recalled
            -- with Ctrl+L. Insert-only with a timestamp (rather than one row
            -- per workdir) so this can grow into a browsable history later —
            -- today's reads take the most recent row per workdir (LIMIT 1).
            CREATE TABLE IF NOT EXISTS last_prompts (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                prompt_text TEXT NOT NULL,
                builder_state TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_last_prompts_workdir_created
                ON last_prompts(workdir, created_at DESC);

            -- CM5: ephemeral subagent runs. Not `agents` (no trigger, no
            -- schedule, no permanent state) and not `loop_runs` (no spec/graph).
            -- A row lives only until it is collected or its TTL (`expires_at`)
            -- passes; the health routine deletes both on its periodic tick.
            -- CB43: `platform`/`model` below mean RESOLVED at dispatch (the
            -- model actually handed to the CLI argv; NULL when the platform's
            -- `model_flag` cannot select one) — never the requested value when
            -- it was not applied.
            CREATE TABLE IF NOT EXISTS subagent_runs (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                model TEXT,
                prompt TEXT NOT NULL,
                workdir TEXT NOT NULL,
                mcp_surface TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                exit_code INTEGER,
                stdout TEXT,
                stderr TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                collected_at TEXT,
                expires_at TEXT NOT NULL,
                pid INTEGER,
                boot_id TEXT
            );

            -- CM18: tombstones for blocking `subagent_spawn` deliveries. A
            -- blocking spawn returns its result inline and deletes the
            -- `subagent_runs` row immediately, so a later `subagent_collect`
            -- would otherwise be indistinguishable from never-existed.
            -- A tombstone records the delivered id until the original
            -- `expires_at`, letting `collect` answer already-delivered.
            -- Async rows never touch this table (no change to async
            -- storage or TTL).
            CREATE TABLE IF NOT EXISTS subagent_delivered_tombstones (
                id TEXT PRIMARY KEY,
                delivered_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sandbox_runs (
                id TEXT PRIMARY KEY,
                project_hash TEXT NOT NULL,
                base_branch TEXT NOT NULL,
                sandbox_branch TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                original_workdir TEXT NOT NULL,
                owner_type TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                cleanup_error TEXT
            );

             INSERT INTO loops (id, name, workdir, status, created_at)
                 VALUES ('g1', 'V3 Graph', '/tmp/v3close', 'running', 1000);
             INSERT INTO loop_specs (id, loop_id, name, position, status)
                 VALUES ('s1', 'g1', 'Spec 1', 0, 'pending');
             INSERT INTO loop_nodes (id, spec_id, name, kind, config, position, created_at)
                 VALUES ('n1', 's1', 'Node A', 'agent', '{}', 0, 1000);
             INSERT INTO loop_nodes (id, spec_id, name, kind, config, position, created_at)
                 VALUES ('n2', 's1', 'Node B', 'agent', '{}', 1, 1000);
             INSERT INTO loop_edges (id, spec_id, from_node, to_node, condition)
                 VALUES ('e1', 's1', 'n1', 'n2', 'break');",
        )
        .expect("seed v3-close schema");
    }

    let db = Database::new(&path).expect("open v3-close db — must migrate loops to graphs cleanly");

    let conn = rusqlite::Connection::open(&path).expect("reopen raw db for assertions");
    let graph_name: String = conn
        .query_row("SELECT name FROM graphs WHERE id = 'g1'", [], |row| {
            row.get(0)
        })
        .expect("loops row must have migrated into graphs");
    assert_eq!(graph_name, "V3 Graph");

    let edge_condition: String = conn
        .query_row(
            "SELECT condition FROM graph_edges WHERE id = 'e1'",
            [],
            |row| row.get(0),
        )
        .expect("loop_edges row must have migrated into graph_edges");
    assert_eq!(
        edge_condition, "error",
        "migrate_loop_to_graph_schema must rewrite 'break' to 'error'"
    );

    drop(db);
}

// RETIRED-VOCAB-BEGIN (CB63 migration test — seeds rows carrying the
// retired prefix on purpose; exempt from the vocabulary guard below)
#[test]
fn activity_attribution_prefix_migration_renames_old_rows_and_is_idempotent() {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    // Fresh database: `init()` runs the attribution migration before the
    // `CREATE TABLE IF NOT EXISTS` batch, so on a brand-new file the
    // guard must make it a no-op, not an error.
    let db = Database::new(&path).expect("fresh db opens with the attribution migration");
    drop(db);

    // Seed one row carrying the retired prefix (as a pre-CB63 binary
    // wrote it) plus rows that must survive untouched: a plain source,
    // a source merely starting with the letters "loop", and a source
    // containing the old prefix NOT at position 0 (anchoring check).
    {
        let conn = rusqlite::Connection::open(&path).expect("reopen raw db to seed rows");
        conn.execute_batch(
            "INSERT INTO activity_log (workdir, source, source_id, kind, message, created_at)
             VALUES
                ('/wd', 'loop:Graph', 'g1', 'info', 'old engine row', 1),
                ('/wd', 'agent', 'a1', 'info', 'agent row', 2),
                ('/wd', 'loopish', 'x', 'info', 'not a prefix', 3),
                ('/wd', 'my-loop:thing', 'x', 'info', 'not anchored', 4);",
        )
        .expect("seed activity rows");
    }

    let sources = || -> Vec<String> {
        let conn = rusqlite::Connection::open(&path).expect("read sources");
        let mut stmt = conn
            .prepare("SELECT source FROM activity_log ORDER BY created_at")
            .expect("select sources");
        stmt.query_map([], |r| r.get::<_, String>(0))
            .expect("map sources")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect sources")
    };

    // Reopen: the migration rewrites the one anchored prefix row.
    let db = Database::new(&path).expect("reopen applies attribution migration");
    drop(db);
    assert_eq!(
        sources(),
        vec!["graph:Graph", "agent", "loopish", "my-loop:thing"],
        "migration must rewrite only rows anchored with the retired prefix"
    );

    // Re-run must be a no-op: same rows, no error (idempotent,
    // re-runnable — the CC3 shape requirement 1 demands).
    let db = Database::new(&path).expect("third open must be idempotent");
    drop(db);
    assert_eq!(
        sources(),
        vec!["graph:Graph", "agent", "loopish", "my-loop:thing"]
    );
}
// RETIRED-VOCAB-END

// RETIRED-VOCAB-BEGIN (this guard test is the third place — with the CB63
// migration and its test above — that must name the retired forms
// literally, so it exempts itself with the same markers it enforces)
/// Guards the CB63 rename from regressing in the two surfaces a person
/// actually reads: documentation and the engine's activity attribution.
///
/// Scans every `docs/**/*.md` plus `README.md` for the retired word
/// "loop" in any form (`loop lifecycle`, `` `loop:<name>` ``,
/// `loop_complete_node`), and every `src/**/*.rs` for the retired
/// attribution prefix inside a Rust string literal — a `"` immediately
/// followed by `loop:`. A hit fails the test naming its file and line.
/// The only exempt places are between marker pairs (the `RETIRED-VOCAB-`
/// prefix plus `BEGIN` / `END`), the established convention
/// (same shape as the queue-schema guard
/// `no_retired_schema_name_identifiers_remain_outside_its_migration`):
/// the migration that rewrites old rows, its test, this guard, and the
/// 2.x alias history in `docs/mcp-tools.md`.
///
/// What is deliberately NOT scanned, and why (Decision D3): the Rust
/// `loop` keyword and identifiers like `loop_id`, and the word `break`.
/// Those forms are indistinguishable from genuine unrelated uses by any
/// mechanical rule, they are not attribution, and the CB63 constraint
/// forbids renaming them anyway. A stale mention that WOULD be a defect
/// — prose "loop" in docs, a `loop:` prefix in any string — cannot hide
/// from the patterns above.
#[test]
fn no_retired_vocabulary_in_user_visible_surfaces() {
    let docs_word = regex::Regex::new(r"(?i)\bloop").unwrap();
    // Built with `concat!` so these definition lines do not contain the
    // marker literals themselves: a literal here would toggle the
    // exempt-region state machine while it scans this very function
    // (a literal end marker here would close the exemption early and the
    // retired-prefix needles below would fail the guard against itself).
    let begin_marker = concat!("RETIRED-VOCAB-", "BEGIN");
    let end_marker = concat!("RETIRED-VOCAB-", "END");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // Collect: docs/**/*.md, README.md, src/**/*.rs.
    let mut doc_files: Vec<std::path::PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(std::path::Path::new(manifest_dir).join("docs"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
    {
        doc_files.push(entry.path().to_path_buf());
    }
    doc_files.push(std::path::Path::new(manifest_dir).join("README.md"));
    let mut src_files: Vec<std::path::PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(std::path::Path::new(manifest_dir).join("src"))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        src_files.push(entry.path().to_path_buf());
    }

    let mut violations = Vec::new();
    for (is_src, path) in src_files
        .iter()
        .map(|p| (true, p))
        .chain(doc_files.iter().map(|p| (false, p)))
    {
        let rel_path = path.strip_prefix(manifest_dir).unwrap_or(path.as_path());
        let source = std::fs::read_to_string(path)
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
            let hit = if is_src {
                // The retired attribution appears in Rust source only
                // inside a string literal: a double quote directly before
                // `loop:`. Plain substring — no escaping ambiguity, and
                // `format!("loop:{…}")` matches.
                line.contains("\"loop:")
                    .then(|| "\"loop: attribution prefix".to_string())
            } else {
                docs_word.find(line).map(|m| m.as_str()).map(Into::into)
            };
            if let Some(text) = hit {
                violations.push(format!("{}:{} — {}", rel_path.display(), i + 1, text));
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
        "retired CB63 vocabulary found in a user-visible surface — the engine attributes activity as `graph:<name>` and the docs must say so; the only places naming the old term on purpose are wrapped in RETIRED-VOCAB-BEGIN/-END markers:\n{}",
        violations.join("\n")
    );
}
// RETIRED-VOCAB-END

// ── CM44: group contiguity on add/set-group ─────────────────────

#[test]
fn add_member_to_existing_group_lands_after_the_group() {
    let db = test_db();
    for id in ["a", "b", "c", "x"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "c", None).unwrap();

    db.add_queue_member_grouped("queue-1", "x", Some("g"), true)
        .unwrap();

    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["a", "b", "x", "c"]
    );
    assert_eq!(
        db.queue_member_group("queue-1", "x").unwrap().as_deref(),
        Some("g")
    );
}

#[test]
fn set_group_of_a_middle_member_moves_it_after_its_new_group() {
    let db = test_db();
    for id in ["a", "b", "c"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "a", Some("g1")).unwrap();
    db.append_queue_member("queue-1", "b", None).unwrap();
    db.append_queue_member("queue-1", "c", Some("g1")).unwrap();

    db.set_queue_member_group("queue-1", "b", Some("g1"))
        .unwrap();

    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["a", "c", "b"]
    );
    assert_eq!(
        db.queue_member_group("queue-1", "b").unwrap().as_deref(),
        Some("g1")
    );
}
