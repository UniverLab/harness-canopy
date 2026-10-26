//! `tests` tests (split from `src/db/intelligence.rs`, pure move).
use super::*;
use tempfile::tempdir;

fn test_db() -> Database {
    let dir = tempdir().unwrap();
    Database::new(&dir.path().join("test.db")).unwrap()
}

fn sample_node_input(id: &str) -> IntelligenceNodeInput {
    IntelligenceNodeInput {
        id: Some(id.to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some(format!("Node {id}")),
        body: Some("Test body".to_string()),
        body_replace: None,
        project_hash: Some(Some("proj1".to_string())),
        session_id: None,
        metadata: None,
        relations: None,
    }
}

mod part01_tests;
mod part02_tests;
