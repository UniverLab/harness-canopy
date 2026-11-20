//! Watcher unit tests (split from `watchers::mod` so that file stays under
//! the 800-line gate): target resolution, event matching and debounce.
use super::*;
use crate::domain::models::WatchEvent;

#[test]
fn resolve_watch_target_directory() {
    let target = resolve_watch_target("/tmp");
    assert_eq!(target.path, PathBuf::from("/tmp"));
    assert!(target.file_filter.is_none());
}

#[test]
fn resolve_watch_target_nonexistent_file_with_parent() {
    let target = resolve_watch_target("/tmp/nonexistent_file.txt");
    assert_eq!(target.path, PathBuf::from("/tmp"));
    assert_eq!(target.file_filter.as_deref(), Some("nonexistent_file.txt"));
}

#[test]
fn resolve_watch_target_nonexistent_path_no_extension() {
    let target = resolve_watch_target("/tmp/some_dir");
    assert_eq!(target.path, PathBuf::from("/tmp/some_dir"));
    assert!(target.file_filter.is_none());
}

#[test]
fn resolve_watch_target_root_path() {
    let target = resolve_watch_target("/");
    assert_eq!(target.path, PathBuf::from("/"));
    assert!(target.file_filter.is_none());
}

#[test]
fn watch_mode_recursive_no_filter() {
    assert_eq!(watch_mode(true, false), RecursiveMode::Recursive);
}

#[test]
fn watch_mode_non_recursive_no_filter() {
    assert_eq!(watch_mode(false, false), RecursiveMode::NonRecursive);
}

#[test]
fn watch_mode_recursive_with_filter_forces_non_recursive() {
    assert_eq!(watch_mode(true, true), RecursiveMode::NonRecursive);
}

#[test]
fn watch_mode_non_recursive_with_filter() {
    assert_eq!(watch_mode(false, true), RecursiveMode::NonRecursive);
}

#[test]
fn event_matches_exact_match() {
    assert!(event_matches(WatchEvent::Create, &[WatchEvent::Create]));
}

#[test]
fn event_matches_no_match() {
    assert!(!event_matches(WatchEvent::Create, &[WatchEvent::Delete]));
}

#[test]
fn event_matches_modify_matches_create_config() {
    // When "create" is configured, "modify" events also match (notify
    // reports modifies as creates on some platforms).
    assert!(event_matches(WatchEvent::Modify, &[WatchEvent::Create]));
}

#[test]
fn event_matches_modify_does_not_match_only_delete() {
    assert!(!event_matches(WatchEvent::Modify, &[WatchEvent::Delete]));
}

#[test]
fn event_matches_in_list() {
    let configured = vec![WatchEvent::Create, WatchEvent::Delete];
    assert!(event_matches(WatchEvent::Create, &configured));
    assert!(event_matches(WatchEvent::Delete, &configured));
    // Modify matches because Create is in the list (line 337)
    assert!(event_matches(WatchEvent::Modify, &configured));
}

#[test]
fn event_matches_move_exact() {
    assert!(event_matches(WatchEvent::Move, &[WatchEvent::Move]));
}

#[test]
fn event_matches_move_not_in_create_list() {
    assert!(!event_matches(WatchEvent::Move, &[WatchEvent::Create]));
}

#[test]
fn map_event_kind_returns_create_for_create() {
    let kind = notify::EventKind::Create(notify::event::CreateKind::File);
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Create));
}

#[test]
fn map_event_kind_returns_modify_for_data_change() {
    let kind = notify::EventKind::Modify(notify::event::ModifyKind::Data(
        notify::event::DataChange::Any,
    ));
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Modify));
}

#[test]
fn map_event_kind_returns_move_for_rename() {
    let kind = notify::EventKind::Modify(notify::event::ModifyKind::Name(
        notify::event::RenameMode::Any,
    ));
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Move));
}

#[test]
fn map_event_kind_returns_delete_for_remove() {
    let kind = notify::EventKind::Remove(notify::event::RemoveKind::File);
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Delete));
}

#[test]
fn map_event_kind_returns_none_for_access() {
    let kind = notify::EventKind::Access(notify::event::AccessKind::Read);
    assert_eq!(map_event_kind(&kind, "test"), None);
}

#[test]
fn map_event_kind_returns_none_for_other() {
    let kind = notify::EventKind::Any;
    assert_eq!(map_event_kind(&kind, "test"), None);
}

// ── Additional edge cases ────────────────────────────────────

#[test]
fn resolve_watch_target_file_with_extension() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.txt");
    std::fs::write(&file_path, "content").unwrap();
    let target = resolve_watch_target(file_path.to_str().unwrap());
    assert_eq!(target.path, dir.path());
    assert_eq!(target.file_filter.as_deref(), Some("test.txt"));
}

#[test]
fn resolve_watch_target_nested_dir() {
    let target = resolve_watch_target("/tmp/nested/dir");
    assert_eq!(target.path, PathBuf::from("/tmp/nested/dir"));
    assert!(target.file_filter.is_none());
}

#[test]
fn watch_mode_recursive_with_file_filter() {
    assert_eq!(watch_mode(true, true), RecursiveMode::NonRecursive);
}

#[test]
fn event_matches_empty_config() {
    assert!(!event_matches(WatchEvent::Create, &[]));
}

#[test]
fn event_matches_all_event_types() {
    let all = vec![
        WatchEvent::Create,
        WatchEvent::Modify,
        WatchEvent::Delete,
        WatchEvent::Move,
    ];
    assert!(event_matches(WatchEvent::Create, &all));
    assert!(event_matches(WatchEvent::Modify, &all));
    assert!(event_matches(WatchEvent::Delete, &all));
    assert!(event_matches(WatchEvent::Move, &all));
}

#[test]
fn event_matches_only_modify_config() {
    assert!(event_matches(WatchEvent::Modify, &[WatchEvent::Modify]));
    assert!(!event_matches(WatchEvent::Create, &[WatchEvent::Modify]));
}

#[test]
fn event_matches_move_not_in_create_list_v2() {
    assert!(!event_matches(WatchEvent::Move, &[WatchEvent::Create]));
}

#[test]
fn map_event_kind_create_folder() {
    let kind = notify::EventKind::Create(notify::event::CreateKind::Folder);
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Create));
}

#[test]
fn map_event_kind_modify_metadata() {
    let kind = notify::EventKind::Modify(notify::event::ModifyKind::Metadata(
        notify::event::MetadataKind::Any,
    ));
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Modify));
}

#[test]
fn map_event_kind_remove_folder() {
    let kind = notify::EventKind::Remove(notify::event::RemoveKind::Folder);
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Delete));
}

#[test]
fn resolve_watch_target_path_with_trailing_slash() {
    let target = resolve_watch_target("/tmp/");
    assert_eq!(target.path, PathBuf::from("/tmp"));
    assert!(target.file_filter.is_none());
}

#[test]
fn event_matches_create_also_matches_modify() {
    assert!(event_matches(WatchEvent::Modify, &[WatchEvent::Create]));
}

#[test]
fn event_matches_delete_does_not_match_create() {
    assert!(!event_matches(WatchEvent::Delete, &[WatchEvent::Create]));
}

#[test]
fn map_event_kind_modify_name_to() {
    let kind = notify::EventKind::Modify(notify::event::ModifyKind::Name(
        notify::event::RenameMode::To,
    ));
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Move));
}

#[test]
fn map_event_kind_modify_name_from() {
    let kind = notify::EventKind::Modify(notify::event::ModifyKind::Name(
        notify::event::RenameMode::From,
    ));
    assert_eq!(map_event_kind(&kind, "test"), Some(WatchEvent::Move));
}

/// The extracted graph arm must ignore an unknown graph id (warn only,
/// no panic, no launch) — previously reachable only via a live watcher.
#[tokio::test]
async fn fire_watch_graph_missing_graph_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let graph_engine = Arc::new(GraphEngine::new(
        Arc::clone(&db),
        Arc::new(crate::application::notification_service::DefaultNotificationService),
    ));

    fire_watch_graph(
        "missing-graph".to_string(),
        &db,
        graph_engine,
        "/tmp/watched.md",
        "modify",
    );
}

/// An unfireable (paused) graph must take the "event ignored" arm: the
/// call returns without panicking and without touching the graph.
#[tokio::test]
async fn fire_watch_graph_refuses_unfireable_graph() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let graph_engine = Arc::new(GraphEngine::new(
        Arc::clone(&db),
        Arc::new(crate::application::notification_service::DefaultNotificationService),
    ));
    let paused = Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "paused-watch-graph".to_string(),
        name: "Paused watch graph".to_string(),
        description: None,
        workdir: "/tmp/watch-graph-test".to_string(),
        status: crate::domain::graphs::GraphStatus::Paused,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
        enabled: true,
        enable_at: None,
        expires_at: None,
    };
    db.insert_graph(&paused).unwrap();

    fire_watch_graph(paused.id, &db, graph_engine, "/tmp/watched.md", "modify");

    let after = db.get_graph("paused-watch-graph").unwrap().unwrap();
    assert_eq!(
        after.status,
        crate::domain::graphs::GraphStatus::Paused,
        "an unfireable graph must not be launched by a watch event"
    );
}

fn watch_graph(id: &str) -> Graph {
    Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: "/tmp/watch-graph-test".to_string(),
        status: crate::domain::graphs::GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
        enabled: true,
        enable_at: None,
        expires_at: None,
    }
}

/// Disabled and expired watch graphs take the "event ignored" arm: no
/// launch, no status change, no runs — even though they are fireable.
#[tokio::test]
async fn fire_watch_graph_refuses_disabled_and_expired_graphs() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let graph_engine = Arc::new(GraphEngine::new(
        Arc::clone(&db),
        Arc::new(crate::application::notification_service::DefaultNotificationService),
    ));
    let mut disabled = watch_graph("disabled-watch-graph");
    disabled.enabled = false;
    db.insert_graph(&disabled).unwrap();
    let mut expired = watch_graph("expired-watch-graph");
    expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    db.insert_graph(&expired).unwrap();

    fire_watch_graph(
        disabled.id.clone(),
        &db,
        Arc::clone(&graph_engine),
        "/tmp/a.md",
        "modify",
    );
    fire_watch_graph(expired.id.clone(), &db, graph_engine, "/tmp/b.md", "modify");
    // A removed guard would launch asynchronously: wait, then assert
    // nothing started.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    for id in ["disabled-watch-graph", "expired-watch-graph"] {
        let after = db.get_graph(id).unwrap().unwrap();
        assert_eq!(
            after.status,
            crate::domain::graphs::GraphStatus::Draft,
            "a disabled/expired graph must not be launched by a watch event"
        );
        assert!(
            db.list_graph_runs_for_graph(id).unwrap().is_empty(),
            "a disabled/expired graph must record no runs"
        );
    }
}
