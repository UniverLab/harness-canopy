//! Part 02 of `tests` (split from `src/db/intelligence.rs`, pure move).
use super::*;

#[test]
fn body_replace_single_match_changes_only_fragment() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("br-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("T".to_string()),
        body: Some("foo bar baz".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: None,
        session_id: None,
        relations: None,
    })
    .unwrap();

    let result = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-1".to_string()),
            kind: None,
            status: None,
            title: None,
            body: None,
            body_replace: Some(BodyReplace {
                fragment: "bar".to_string(),
                replacement: "qux".to_string(),
            }),
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

    assert!(!result.created);
    assert_eq!(result.record.body, "foo qux baz");
    assert_eq!(
        result.changed_fields,
        vec!["body".to_string(), "updated_at".to_string()]
    );
}

#[test]
fn body_replace_zero_matches_fails_and_changes_nothing() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("br-zero".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("T".to_string()),
        body: Some("foo bar".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: None,
        session_id: None,
        relations: None,
    })
    .unwrap();

    let err = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-zero".to_string()),
            kind: None,
            status: None,
            title: None,
            body: None,
            body_replace: Some(BodyReplace {
                fragment: "xyz".to_string(),
                replacement: "qux".to_string(),
            }),
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected a not-found refusal, got: {err}"
    );
    let after = db.get_intelligence_node("br-zero").unwrap().unwrap();
    assert_eq!(after.body, "foo bar");
}

#[test]
fn body_replace_two_matches_fails_and_changes_nothing() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("br-two".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("T".to_string()),
        body: Some("bar foo bar".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: None,
        session_id: None,
        relations: None,
    })
    .unwrap();

    let err = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("br-two".to_string()),
            kind: None,
            status: None,
            title: None,
            body: None,
            body_replace: Some(BodyReplace {
                fragment: "bar".to_string(),
                replacement: "qux".to_string(),
            }),
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("ambiguous") && msg.contains("2 times"),
        "expected an ambiguous-target refusal naming the count, got: {msg}"
    );
    let after = db.get_intelligence_node("br-two").unwrap().unwrap();
    assert_eq!(after.body, "bar foo bar");
}

#[test]
fn body_and_body_replace_are_mutually_exclusive() {
    let db = test_db();
    db.upsert_intelligence_node(sample_node_input("br-excl"))
        .unwrap();
    let err = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            body: Some("whole new body".to_string()),
            body_replace: Some(BodyReplace {
                fragment: "Test".to_string(),
                replacement: "Best".to_string(),
            }),
            ..sample_node_input("br-excl")
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("mutually exclusive"),
        "expected a mutual-exclusion refusal, got: {err}"
    );
}

#[test]
fn create_without_id_still_works() {
    let db = test_db();
    let result = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: None,
            kind: Some("fact".to_string()),
            status: None,
            title: Some("Created".to_string()),
            body: Some("Fresh body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("proj-a".to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    assert!(result.created);
    assert!(!result.record.id.is_empty());
    assert_eq!(result.record.title, "Created");
    assert!(result.changed_fields.contains(&"title".to_string()));
}

#[test]
fn create_requires_kind_title_body() {
    let db = test_db();
    let err = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: None,
            kind: None,
            status: None,
            title: Some("T".to_string()),
            body: Some("B".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("create requires"),
        "expected a create-requires refusal, got: {err}"
    );
}

#[test]
fn changed_fields_names_what_changed() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("cf-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("T".to_string()),
        body: Some("B".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"a": 1}))),
        project_hash: None,
        session_id: None,
        relations: None,
    })
    .unwrap();

    let result = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("cf-1".to_string()),
            kind: None,
            status: None,
            title: Some("T2".to_string()),
            body: None,
            body_replace: None,
            metadata: Some(Some(serde_json::json!({"a": 2}))),
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

    assert!(result.changed_fields.contains(&"title".to_string()));
    assert!(result.changed_fields.contains(&"metadata".to_string()));
    assert!(!result.changed_fields.contains(&"body".to_string()));
    assert!(!result.changed_fields.contains(&"kind".to_string()));
    assert!(!result.changed_fields.contains(&"status".to_string()));
    assert!(!result.changed_fields.contains(&"relations".to_string()));
}

#[test]
fn explicit_null_clears_nullable_fields() {
    let db = test_db();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("null-1".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("T".to_string()),
        body: Some("B".to_string()),
        body_replace: None,
        metadata: Some(Some(serde_json::json!({"a": 1}))),
        project_hash: Some(Some("proj-a".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();

    let result = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("null-1".to_string()),
            kind: None,
            status: None,
            title: None,
            body: None,
            body_replace: None,
            metadata: Some(None),
            project_hash: Some(None),
            session_id: None,
            relations: None,
        })
        .unwrap();

    assert!(!result.created);
    assert_eq!(result.record.metadata, None);
    assert_eq!(result.record.project_hash, None);
    assert_eq!(result.record.title, "T");
    assert!(result.changed_fields.contains(&"metadata".to_string()));
    assert!(result.changed_fields.contains(&"project_hash".to_string()));
}

#[test]
fn body_replace_real_case_hooks_decision() {
    // The real case: one stale line inside a body of several thousand
    // characters, corrected without touching anything else.
    let db = test_db();
    let filler = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(100);
    let stale = "fuera de esta versión: hooks internos";
    let body = format!("{filler}\n{stale}\n{filler}");
    assert!(body.len() > 6000);
    assert_eq!(body.matches(stale).count(), 1);
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("hooks-decision".to_string()),
        kind: Some("decision".to_string()),
        status: None,
        title: Some("Scope".to_string()),
        body: Some(body.clone()),
        body_replace: None,
        metadata: None,
        project_hash: None,
        session_id: None,
        relations: None,
    })
    .unwrap();

    let result = db
        .upsert_intelligence_node(IntelligenceNodeInput {
            id: Some("hooks-decision".to_string()),
            kind: None,
            status: None,
            title: None,
            body: None,
            body_replace: Some(BodyReplace {
                fragment: stale.to_string(),
                replacement: "dentro de esta versión: hooks internos".to_string(),
            }),
            metadata: None,
            project_hash: None,
            session_id: None,
            relations: None,
        })
        .unwrap();

    let expected = body.replacen(stale, "dentro de esta versión: hooks internos", 1);
    assert_eq!(result.record.body, expected);
    // Everything before and after the fragment is byte-identical.
    let at = body.find(stale).unwrap();
    assert_eq!(&result.record.body[..at], &body[..at]);
    assert_eq!(
        &result.record.body[at + "dentro de esta versión: hooks internos".len()..],
        &body[at + stale.len()..]
    );
    assert_eq!(
        result.changed_fields,
        vec!["body".to_string(), "updated_at".to_string()]
    );
}
