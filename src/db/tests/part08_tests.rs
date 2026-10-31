//! Part 08 of `db::tests` top-level tests: the sandbox Intelligence
//! attribution migration (FR3) — stray nodes written from a sandbox worktree
//! are re-attributed to the project the sandbox was cut from.
use super::*;
use rusqlite::params;

use crate::domain::project::workdir_hash;
use crate::domain::sandbox::Sandbox;

const WT: &str = "/tmp/sbx-mig-wt/aaaa1111/22223333-sandbox";
const ORIG: &str = "/tmp/sbx-mig-real-project";

fn seed_sandbox_run(db: &Database, id: &str) {
    let sandbox = Sandbox {
        id: id.to_string(),
        project_hash: workdir_hash(ORIG),
        base_branch: "main".to_string(),
        sandbox_branch: format!("canopy/sandbox-{id}"),
        worktree_path: WT.into(),
        cli_name: "opencode".to_string(),
        original_workdir: ORIG.to_string(),
        created_at: Utc::now(),
    };
    db.insert_sandbox_run(&sandbox, "graph", &format!("graph-{id}"))
        .unwrap();
}

fn insert_node(conn: &rusqlite::Connection, id: &str, project_hash: &str) {
    conn.execute(
        "INSERT INTO intelligence_nodes (id, kind, title, body, created_at, updated_at, project_hash)
         VALUES (?1, 'fact', 'SBX-NODE', '', 0, 0, ?2)",
        params![id, project_hash],
    )
    .unwrap();
}

#[test]
fn migrate_sandbox_intelligence_attribution_moves_stray_project_row_and_is_idempotent() {
    let db = test_db();
    let stray = workdir_hash(WT);
    let target = workdir_hash(ORIG);
    assert_ne!(stray, target, "test paths must hash to different projects");
    seed_sandbox_run(&db, "sbx-mig-1");

    {
        let conn = db.conn.lock().unwrap();
        // A project the sandboxed agent registered from the worktree, with
        // its `project:{hash}` graph root.
        conn.execute(
            "INSERT INTO projects (hash, path, name, created_at)
             VALUES (?1, ?2, 'stray-sandbox-project', 0)",
            params![stray, WT],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, created_at, updated_at, project_hash)
             VALUES ('project:' || ?1, 'project', 'stray root', '', 0, 0, ?1)",
            params![stray],
        )
        .unwrap();
        // A stray Intelligence node, and one already on the original project.
        insert_node(&conn, "n-stray", &stray);
        insert_node(&conn, "n-target", &target);
        // A session registered at the worktree path (pre-fix behaviour).
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at, status, session_type)
             VALUES ('sess-mig', 'sess', 'opencode', ?1, '2026-01-01T00:00:00Z', 'active', 'interactive')",
            params![WT],
        )
        .unwrap();
    }

    // Called twice: the second pass must be a silent no-op, not an error.
    Database::migrate_sandbox_intelligence_attribution(&db.conn.lock().unwrap()).unwrap();
    Database::migrate_sandbox_intelligence_attribution(&db.conn.lock().unwrap()).unwrap();

    let conn = db.conn.lock().unwrap();
    // The stray node was re-attributed to the original project...
    let n_stray: String = conn
        .query_row(
            "SELECT project_hash FROM intelligence_nodes WHERE id = 'n-stray'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n_stray, target);
    // ...the original project's node was untouched...
    let n_target: String = conn
        .query_row(
            "SELECT project_hash FROM intelligence_nodes WHERE id = 'n-target'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(n_target, target);
    // ...the stray project row and its `project:{hash}` root are gone...
    let stray_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE hash = ?1",
            params![stray],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stray_rows, 0);
    let stray_root: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intelligence_nodes WHERE id = ?1",
            params![format!("project:{stray}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stray_root, 0);
    // ...and the project row itself now names the original path, under the
    // original hash — the same re-key `project_remap` performs.
    let moved_path: String = conn
        .query_row(
            "SELECT path FROM projects WHERE hash = ?1",
            params![target],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(moved_path, ORIG);
    let moved_root: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intelligence_nodes WHERE id = ?1",
            params![format!("project:{target}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(moved_root, 1);
    // The session registered at the worktree path was re-homed too.
    let wd: String = conn
        .query_row(
            "SELECT working_dir FROM interactive_sessions WHERE id = 'sess-mig'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(wd, ORIG);
}

#[test]
fn migrate_sandbox_intelligence_attribution_moves_nodes_without_stray_project_row() {
    let db = test_db();
    let stray = workdir_hash(WT);
    let target = workdir_hash(ORIG);
    seed_sandbox_run(&db, "sbx-mig-2");

    {
        let conn = db.conn.lock().unwrap();
        // A stray node whose worktree hash never got a project row (the
        // agent wrote Intelligence but never registered the worktree).
        insert_node(&conn, "n-orphan", &stray);
    }

    Database::migrate_sandbox_intelligence_attribution(&db.conn.lock().unwrap()).unwrap();

    let conn = db.conn.lock().unwrap();
    let ph: String = conn
        .query_row(
            "SELECT project_hash FROM intelligence_nodes WHERE id = 'n-orphan'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ph, target);
    // No project row is invented for the stray hash.
    let project_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(project_rows, 0);
}

/// An existing database whose `graph_events.kind` CHECK still ends at
/// `mutex_acquired` is widened to admit `pause_requested`/`pause_cancelled` —
/// data preserved, migration idempotent, and afterwards the new kinds insert.
#[test]
fn graph_events_pause_kinds_migration_widens_check_and_preserves_rows() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE graph_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            graph_id TEXT NOT NULL,
            spec_id TEXT,
            node_id TEXT,
            run_id TEXT,
            kind TEXT NOT NULL
                CHECK (kind IN (
                    'graph_started','graph_paused','graph_resumed','graph_completed','graph_failed','graph_blocked',
                    'spec_started','spec_completed','spec_failed','spec_skipped',
                    'node_started','node_finished',
                    'hook_fired',
                    'config_changed',
                    'note',
                    'mutex_waiting','mutex_acquired'
                )),
            message TEXT NOT NULL,
            payload TEXT,
            created_at TEXT NOT NULL
        );
        CREATE INDEX idx_graph_events_graph_id_id ON graph_events(graph_id, id);
        INSERT INTO graph_events (graph_id, kind, message, created_at)
            VALUES ('g1', 'graph_started', 'started', '2026-01-01T00:00:00Z');",
    )
    .unwrap();

    Database::migrate_graph_events_pause_kinds(&conn).unwrap();
    // Idempotent: a second run is a no-op.
    Database::migrate_graph_events_pause_kinds(&conn).unwrap();

    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'graph_events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ddl.contains("'pause_requested'"), "{ddl}");
    assert!(ddl.contains("'pause_cancelled'"), "{ddl}");

    // The pre-migration row survived the rebuild.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);

    // The new kinds insert now.
    for kind in ["pause_requested", "pause_cancelled"] {
        conn.execute(
            "INSERT INTO graph_events (graph_id, kind, message, created_at)
                VALUES ('g1', ?1, 'x', '2026-01-01T00:00:00Z')",
            [kind],
        )
        .unwrap();
    }
}
