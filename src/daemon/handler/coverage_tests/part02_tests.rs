//! Part 02 of `coverage_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[test]
fn graph_node_run_summary_json_compact_mode_keeps_id_omits_internal_ids() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
    insert_test_node(&db, "node-1", "spec-1");
    let run = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);

    let compact = graph_node_run_summary_json(&db, &run, true);
    assert_eq!(compact["id"], "run-1", "compact must include id");
    assert!(
        compact.get("spec_id").is_none(),
        "compact must omit spec_id"
    );
    assert!(
        compact.get("node_id").is_none(),
        "compact must omit node_id"
    );
    assert!(
        compact.get("session_id").is_none(),
        "compact must omit session_id"
    );
    assert_eq!(compact["spec_name"], "spec-1");
    assert_eq!(compact["node_name"], "node-1");
    assert_eq!(compact["status"], "pass");
    assert_eq!(compact["iteration"], 1);
    assert!(compact.get("input").is_none(), "compact must omit input");
    assert!(compact.get("output").is_none(), "compact must omit output");

    let full = graph_node_run_summary_json(&db, &run, false);
    assert_eq!(full["id"], "run-1");
    assert_eq!(full["spec_id"], "spec-1");
    assert_eq!(full["node_id"], "node-1");
    assert!(
        full.get("spec_name").is_none(),
        "full mode does not carry spec_name"
    );
}

/// T3 (CB43): the recorded pair appears in both list modes; pre-migration
/// rows (NULL) omit the keys rather than emitting nulls.
#[test]
fn cb43_run_pair_appears_in_both_list_modes() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
    insert_test_node(&db, "node-1", "spec-1");
    let mut run = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);
    run.executed_platform = Some("opencode".to_string());
    run.executed_model = Some("opencode/big-pickle".to_string());

    for compact in [true, false] {
        let json = graph_node_run_summary_json(&db, &run, compact);
        assert_eq!(json["platform"], "opencode");
        assert_eq!(json["model"], "opencode/big-pickle");
    }

    let detail = graph_node_run_detail_json(&run, Some("node-1"), None);
    assert_eq!(detail["platform"], "opencode");
    assert_eq!(detail["model"], "opencode/big-pickle");

    // Legacy row: keys omitted, not null.
    let legacy = graph_run_row("run-2", "graph-1", "spec-1", GraphRunStatus::Pass);
    for compact in [true, false] {
        let json = graph_node_run_summary_json(&db, &legacy, compact);
        assert!(json.get("platform").is_none());
        assert!(json.get("model").is_none());
    }
    let detail = graph_node_run_detail_json(&legacy, Some("node-1"), None);
    assert!(detail.get("platform").is_none());
    assert!(detail.get("model").is_none());
}

/// CM26: `find_resumed_from_run_id` recovers "was this run a resume, and
/// of what" purely from the session id a resumed run's own row carries
/// verbatim — no dedicated column. Two rows on the same node share
/// `session_id`; the later one (by insertion order) resumed the earlier
/// one, never the reverse, and `graph`'s JSON surfaces it.
#[test]
fn graph_node_run_get_reports_resumed_from_run_id_for_ensemble_member() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
    insert_test_node(&db, "node-1", "spec-1");

    let mut first = graph_run_row("run-1", "graph-1", "spec-1", GraphRunStatus::Pass);
    first.session_id = Some("ses-1".to_string());
    db.insert_graph_run(&first).unwrap();

    let mut second = graph_run_row("run-2", "graph-1", "spec-1", GraphRunStatus::Pass);
    second.session_id = Some("ses-1".to_string());
    db.insert_graph_run(&second).unwrap();

    assert_eq!(
        db.find_resumed_from_run_id(&second.id, &second.node_id, "ses-1")
            .unwrap(),
        Some(first.id.clone()),
        "the later row (by insertion order) resumed the earlier one"
    );
    assert_eq!(
        db.find_resumed_from_run_id(&first.id, &first.node_id, "ses-1")
            .unwrap(),
        None,
        "nothing precedes the first row with that session id"
    );

    let resumed_from = db
        .find_resumed_from_run_id(&second.id, &second.node_id, "ses-1")
        .unwrap();
    let detail = graph_node_run_detail_json(&second, Some("node-1"), resumed_from.as_deref());
    assert_eq!(detail["resumed_from_run_id"], serde_json::json!(first.id));

    let detail_first = graph_node_run_detail_json(&first, Some("node-1"), None);
    assert_eq!(detail_first["resumed_from_run_id"], serde_json::Value::Null);
}

/// T4 (CB43): the recent section leads the output and carries last
/// outcome + frequency; an empty installation gets a one-line notice.
#[test]
fn cb43_recent_section_leads_with_last_outcome() {
    use crate::db::graphs::RecentModelUsage;

    let now = chrono::Utc::now();
    let rows = vec![
        RecentModelUsage {
            platform: "opencode".to_string(),
            model: Some("opencode/big-pickle".to_string()),
            last_run: now,
            count: 12,
            last_outcome: "pass".to_string(),
        },
        RecentModelUsage {
            platform: "claude".to_string(),
            model: None,
            last_run: now - chrono::Duration::hours(2),
            count: 40,
            last_outcome: "timeout".to_string(),
        },
    ];
    let section = format_recent_usage_section(&rows);
    assert!(
        section.starts_with("Recently used"),
        "section must lead so truncation drops the catalogue tail, not the signal"
    );
    assert!(section.contains("opencode/big-pickle"));
    assert!(section.contains("12 runs"));
    assert!(section.contains("last outcome: pass"));
    // A heavily-used model whose last run timed out must not read as a
    // recommendation: the outcome, not the count, is the honest signal.
    assert!(section.contains("claude/(default)"));
    assert!(section.contains("last outcome: timeout"));

    let empty = format_recent_usage_section(&[]);
    assert!(empty.starts_with("Recently used"));
    assert!(empty.contains("No models have run"));
}

/// T4/T11 (CB43): prepending puts the section first on success, leaves
/// errors alone, and stays inside budget.
#[test]
fn cb43_prepend_recent_usage_first_on_success_only() {
    let section = "Recently used on this machine (last 30 days):\n- a/b: x".to_string();

    let mut ok = CallToolResult::success(vec![ContentBlock::text("Source: catalogue".to_string())]);
    prepend_recent_usage(&mut ok, &section);
    let text = ok.content[0].as_text().unwrap().text.clone();
    assert!(text.starts_with("Recently used"));
    assert!(text.contains("Source: catalogue"));

    let mut err = error_result("No known model providers are mapped for platform 'x'.");
    let before = format!("{:?}", err.content);
    prepend_recent_usage(&mut err, &section);
    assert_eq!(format!("{:?}", err.content), before);

    let mut ok2 =
        CallToolResult::success(vec![ContentBlock::text("Source: catalogue".to_string())]);
    prepend_recent_usage(&mut ok2, "");
    assert_eq!(ok2.content[0].as_text().unwrap().text, "Source: catalogue");
}

/// T11 (CB43): a full fifteen-pair section is a couple of KB at most —
/// the header survives catalogue truncation.
#[test]
fn cb43_full_section_stays_inside_budget() {
    use crate::db::graphs::RecentModelUsage;

    let now = chrono::Utc::now();
    let rows: Vec<RecentModelUsage> = (0..15)
        .map(|i| RecentModelUsage {
            platform: "opencode".to_string(),
            model: Some(format!("org/model-{i:02}-with-a-long-name")),
            last_run: now - chrono::Duration::minutes(i),
            count: 100,
            last_outcome: "pass".to_string(),
        })
        .collect();
    let section = format_recent_usage_section(&rows);
    assert!(section.len() < 8192, "section was {} bytes", section.len());
    assert!(section.starts_with("Recently used"));
}
