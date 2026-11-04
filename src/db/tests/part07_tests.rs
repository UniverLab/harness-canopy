//! Part 07 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

#[test]
fn clear_group_moves_member_after_its_old_group_when_stranded() {
    let db = test_db();
    for id in ["a", "b", "c"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "c", Some("g")).unwrap();

    db.set_queue_member_group("queue-1", "b", None).unwrap();

    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["a", "c", "b"]
    );
    assert!(db.queue_member_group("queue-1", "b").unwrap().is_none());
}

#[test]
fn running_member_keeps_its_position() {
    let db = test_db();
    for id in ["r", "a", "b", "d", "x"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "r", None).unwrap();
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "d", None).unwrap();
    db.update_graph_spec_status("r", GraphSpecStatus::Running, None, None)
        .unwrap();

    let p = db.queue_member_position("queue-1", "r").unwrap();
    assert_eq!(p, Some(1));

    db.add_queue_member_grouped("queue-1", "x", Some("g"), true)
        .unwrap();

    assert_eq!(db.queue_member_position("queue-1", "r").unwrap(), p);
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["r", "a", "b", "x", "d"]
    );
}

#[test]
fn completed_member_keeps_its_position() {
    let db = test_db();
    for id in ["r", "a", "b", "d", "x"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "r", None).unwrap();
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "d", None).unwrap();
    db.update_graph_spec_status(
        "r",
        GraphSpecStatus::Completed,
        Some(Utc::now()),
        Some(Utc::now()),
    )
    .unwrap();

    let p = db.queue_member_position("queue-1", "r").unwrap();
    assert_eq!(p, Some(1));

    db.add_queue_member_grouped("queue-1", "x", Some("g"), true)
        .unwrap();

    assert_eq!(db.queue_member_position("queue-1", "r").unwrap(), p);
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["r", "a", "b", "x", "d"]
    );
}

#[test]
fn v12_migration_drops_retired_blueprint_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE blueprints (id TEXT PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE ensemble_blueprints (id TEXT PRIMARY KEY, name TEXT NOT NULL);",
        )
        .unwrap();
    }
    let db = Database::new(&path).unwrap();
    drop(db);
    let conn = rusqlite::Connection::open(&path).unwrap();
    for table in ["blueprints", "ensemble_blueprints"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table} must be dropped by the v12 migration");
    }
}
