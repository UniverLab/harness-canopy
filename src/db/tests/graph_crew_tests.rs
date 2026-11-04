// ── CM50: graph_crew ───────────────────────────────────────────

/// CM50: `graph_crew` — every graph's harness/model assignments, in the
/// shape the MCP tool and `canopy graph crew` both print.
use super::*;
use crate::daemon::graph_cli::format::format_graph_crew_table;
use crate::db::graph_crew::GraphCrewDbParams;
use crate::db::roster_entries::{NewRosterEntry, RosterState};
use crate::domain::crews::CrewMember;
use crate::domain::graphs::{
    ArchiveGraphOutcome, CrewRowKind, Ensemble, EnsembleKind, EnsembleMember, GraphCompletionHook,
    GraphCrewGroup, GraphCrewRow, GraphHookEvent,
};
use std::collections::BTreeMap;

/// Prompt text planted in the fixture: it must never appear in any
/// serialized `graph_crew` output.
const LEAKED_HOOK_PROMPT: &str = "SECRETS FROM THE HOOK PROMPT";
const LEAKED_NODE_PROMPT: &str = "SECRETS FROM THE NODE PROMPT_TEMPLATE";

/// Two graphs: `Crew live` (literal two-member ensemble, a lone agent
/// node, an agent hook) and `Crew archived` (one agent node, archived).
struct CrewFixture {
    db: Database,
    live_id: String,
    archived_id: String,
    ensemble_id: String,
}

fn agent_node(
    spec_id: &str,
    id: &str,
    name: &str,
    position: i64,
    config: serde_json::Value,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: Utc::now(),
    }
}

fn static_node(spec_id: &str, id: &str, name: &str, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: name.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "true", "success_condition": "exit_code_0" }),
        position,
        created_at: Utc::now(),
    }
}

fn edge(spec_id: &str, from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: format!("{from}->{to}"),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

fn member(
    ensemble_id: &str,
    node_id: &str,
    position: i64,
    platform: &str,
    model: Option<&str>,
    timeout_minutes: Option<i64>,
) -> EnsembleMember {
    EnsembleMember {
        ensemble_id: ensemble_id.to_string(),
        node_id: node_id.to_string(),
        position,
        platform: platform.to_string(),
        model: model.map(str::to_string),
        prompt_override: None,
        timeout_minutes,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// One crew member (CM49) for the fixture's crew creation.
fn crew_member(position: i64, platform: &str, model: Option<&str>) -> CrewMember {
    CrewMember {
        position,
        platform: platform.to_string(),
        model: model.map(str::to_string),
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// Insert a two-member ensemble wired `entry -> members -> join ->
/// exit`, with the two static endpoints this spec's graphs use.
/// `crew` binds the ensemble to a crew (CM49) as `(crew id, crew name,
/// version applied)` — the id must exist in `crews`, so the caller
/// creates it first — instead of leaving its members literal.
fn insert_ensemble(
    db: &Database,
    spec_id: &str,
    ensemble_id: &str,
    name: &str,
    crew: Option<(&str, &str, i64)>,
) {
    db.insert_graph_node(&static_node(spec_id, "node-entry", "Entry", 0))
        .unwrap();
    db.insert_graph_node(&static_node(spec_id, "node-exit", "Exit", 4))
        .unwrap();

    let ensemble = Ensemble {
        mutex: Vec::new(),
        id: ensemble_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: name.to_string(),
        prompt_template: "review it: {{spec_content}}".to_string(),
        join_node_id: "node-join".to_string(),
        entry_from_node: "node-entry".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "node-exit".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        commit_rights: false,
        created_at: Utc::now(),
        crew_id: crew.map(|(id, _, _)| id.to_string()),
        crew_name: crew.map(|(_, name, _)| name.to_string()),
        crew_version_applied: crew.map(|(_, _, version)| version),
    };
    let members = vec![
        member(ensemble_id, "node-m1", 0, "claude", Some("opus"), Some(10)),
        member(
            ensemble_id,
            "node-m2",
            1,
            "opencode-go",
            Some("gpt-5"),
            None,
        ),
    ];
    let member_nodes = vec![
        agent_node(
            spec_id,
            "node-m1",
            "Member one",
            1,
            serde_json::json!({ "platform": "claude", "model": "opus" }),
        ),
        agent_node(
            spec_id,
            "node-m2",
            "Member two",
            2,
            serde_json::json!({ "platform": "opencode-go", "model": "gpt-5" }),
        ),
    ];
    let join = GraphNode {
        id: "node-join".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": ensemble_id }),
        position: 3,
        created_at: Utc::now(),
    };
    let edges = vec![
        edge(spec_id, "node-entry", "node-m1", GraphEdgeCondition::Always),
        edge(spec_id, "node-entry", "node-m2", GraphEdgeCondition::Always),
        edge(spec_id, "node-m1", "node-join", GraphEdgeCondition::Always),
        edge(spec_id, "node-m2", "node-join", GraphEdgeCondition::Always),
        edge(spec_id, "node-join", "node-exit", GraphEdgeCondition::Pass),
    ];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join, &edges)
        .unwrap();
}

fn agent_hook(platform: &str, model: &str, prompt: &str) -> GraphCompletionHook {
    GraphCompletionHook {
        platform: Some(platform.to_string()),
        crew: None,
        model: Some(model.to_string()),
        effort: None,
        prompt: Some(prompt.to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(5),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    }
}

/// The spec's fixture database: two graphs, exactly one of them
/// archived.
fn crew_fixture() -> CrewFixture {
    let db = test_db();

    let mut live = sample_graph("graph-crew-live");
    live.name = "Crew live".to_string();
    db.insert_graph(&live).unwrap();
    let spec = sample_graph_spec(&live.id, "spec-crew-live", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_ensemble(&db, &spec.id, "ens-crew", "Reviewers", None);
    // The lone agent node: outside every ensemble, and carrying a
    // prompt the result must never echo.
    db.insert_graph_node(&agent_node(
        &spec.id,
        "node-lone",
        "Lone agent",
        5,
        serde_json::json!({
            "platform": "claude",
            "model": "opus",
            "timeout_minutes": 10,
            "prompt_template": LEAKED_NODE_PROMPT,
        }),
    ))
    .unwrap();
    let mut hooks = BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![agent_hook("claude", "sonnet", LEAKED_HOOK_PROMPT)],
    );
    db.update_graph_hooks(&live.id, &hooks).unwrap();

    let mut archived = sample_graph("graph-crew-archived");
    archived.name = "Crew archived".to_string();
    // Strictly newer than the live graph, so `list_graphs`' created_at
    // ordering is deterministic when both appear (`include_archived`).
    archived.created_at = live.created_at + chrono::Duration::seconds(60);
    db.insert_graph(&archived).unwrap();
    let archived_spec = sample_graph_spec(&archived.id, "spec-crew-archived", 1);
    db.insert_graph_spec(&archived_spec).unwrap();
    db.insert_graph_node(&agent_node(
        &archived_spec.id,
        "node-archived",
        "Archived agent",
        0,
        serde_json::json!({ "platform": "opencode-go", "model": "gpt-5" }),
    ))
    .unwrap();
    assert!(matches!(
        db.archive_graph(&archived.id).unwrap(),
        ArchiveGraphOutcome::Archived
    ));

    CrewFixture {
        db,
        live_id: live.id,
        archived_id: archived.id,
        ensemble_id: "ens-crew".to_string(),
    }
}

/// Every row of every group, in order — the flat view the row-level
/// assertions work on.
fn flat(groups: &[GraphCrewGroup]) -> Vec<&GraphCrewRow> {
    groups.iter().flat_map(|g| g.rows.iter()).collect()
}

fn group<'a>(groups: &'a [GraphCrewGroup], id: &str) -> &'a GraphCrewGroup {
    groups
        .iter()
        .find(|g| g.id == id)
        .unwrap_or_else(|| panic!("no group for graph {id}"))
}

/// FR2/FR5: one fixture, all three row kinds, with the ensemble's
/// kind/min_pass/commit_rights on its members — and the static
/// `Entry`/`Quorum` nodes never producing rows of their own.
#[test]
fn graph_crew_fixture_reports_every_row_kind() {
    let fixture = crew_fixture();
    let groups = fixture.db.graph_crew(GraphCrewDbParams::default()).unwrap();

    assert_eq!(groups.len(), 1, "the archived graph stays out by default");
    let live = &groups[0];
    assert_eq!(live.id, fixture.live_id);
    assert_eq!(live.name, "Crew live");
    assert_eq!(live.status, "draft");
    assert_eq!(live.workdir, "/tmp/project");
    assert_eq!(live.rows.len(), 4, "lone node + 2 members + 1 hook");

    let node = live
        .rows
        .iter()
        .find(|r| r.kind == CrewRowKind::Node)
        .expect("the lone agent node is a row");
    assert_eq!(node.node_id.as_deref(), Some("node-lone"));
    assert_eq!(node.name, "Lone agent");
    assert_eq!(node.platform, "claude");
    assert_eq!(node.model, "opus");
    assert_eq!(node.timeout_minutes, Some(10));
    assert_eq!(node.ensemble_id, None);

    let members: Vec<&GraphCrewRow> = live
        .rows
        .iter()
        .filter(|r| r.kind == CrewRowKind::EnsembleMember)
        .collect();
    assert_eq!(members.len(), 2);
    let first = members
        .iter()
        .find(|r| r.position == Some(0))
        .expect("member at position 0");
    assert_eq!(
        first.ensemble_id.as_deref(),
        Some(fixture.ensemble_id.as_str())
    );
    assert_eq!(first.node_id.as_deref(), Some("node-m1"));
    assert_eq!(first.name, "Reviewers");
    assert_eq!(first.platform, "claude");
    assert_eq!(first.model, "opus");
    assert_eq!(first.timeout_minutes, Some(10));
    assert_eq!(first.ensemble_kind.as_deref(), Some("parallel"));
    assert_eq!(first.ensemble_min_pass, Some(2));
    assert_eq!(first.ensemble_commit_rights, Some(false));
    let second = members
        .iter()
        .find(|r| r.position == Some(1))
        .expect("member at position 1");
    assert_eq!(second.platform, "opencode-go");
    assert_eq!(second.model, "gpt-5");
    // No member timeout of its own: the ensemble's shared timeout is
    // what actually applies.
    assert_eq!(second.timeout_minutes, Some(30));

    let hook = live
        .rows
        .iter()
        .find(|r| r.kind == CrewRowKind::Hook)
        .expect("the agent hook is a row");
    assert_eq!(hook.name, "on_completed");
    assert_eq!(hook.platform, "claude");
    assert_eq!(hook.model, "sonnet");
    assert_eq!(hook.timeout_minutes, Some(5));

    assert!(
        live.rows
            .iter()
            .all(|r| r.name != "Entry" && r.name != "Quorum"),
        "static and join nodes are not crew assignments"
    );
}

/// FR1: `platform` filters by exact match and leaves only matching
/// rows; graphs with none are omitted.
#[test]
fn graph_crew_filters_by_platform() {
    let fixture = crew_fixture();
    let groups = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            platform: Some("opencode-go"),
            ..GraphCrewDbParams::default()
        })
        .unwrap();

    assert_eq!(groups.len(), 1, "only the graph still holding a row");
    assert_eq!(groups[0].id, fixture.live_id);
    let rows = flat(&groups);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, CrewRowKind::EnsembleMember);
    assert_eq!(rows[0].platform, "opencode-go");
    // No partial match: exact equality, not a substring.
    assert!(flat(&groups).iter().all(|r| r.platform == "opencode-go"));
}

/// FR1: `model` matches a case-insensitive substring.
#[test]
fn graph_crew_filters_by_model_substring() {
    let fixture = crew_fixture();
    let groups = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            model: Some("OPUS"),
            ..GraphCrewDbParams::default()
        })
        .unwrap();

    let rows = flat(&groups);
    assert_eq!(rows.len(), 2, "the lone node and member m1");
    assert!(rows.iter().all(|r| r.model.to_lowercase().contains("opus")));
}

/// FR1: archived graphs are excluded by default and included on request.
#[test]
fn graph_crew_include_archived() {
    let fixture = crew_fixture();

    let without = fixture.db.graph_crew(GraphCrewDbParams::default()).unwrap();
    assert_eq!(without.len(), 1);
    assert_eq!(without[0].id, fixture.live_id);

    let with = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            include_archived: true,
            ..GraphCrewDbParams::default()
        })
        .unwrap();
    assert_eq!(with.len(), 2);
    let archived = group(&with, &fixture.archived_id);
    assert_eq!(archived.name, "Crew archived");
    assert_eq!(archived.rows.len(), 1);
    assert_eq!(archived.rows[0].name, "Archived agent");
}

/// FR1: `graph_id` narrows to that graph alone — and, like every other
/// id-addressed read surface, it still reaches an archived graph.
#[test]
fn graph_crew_graph_id_narrows_to_one_graph() {
    let fixture = crew_fixture();
    let groups = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            graph_id: Some(fixture.archived_id.as_str()),
            ..GraphCrewDbParams::default()
        })
        .unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, fixture.archived_id);
    assert_eq!(groups[0].rows.len(), 1);

    let unknown = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            graph_id: Some("graph-does-not-exist"),
            ..GraphCrewDbParams::default()
        })
        .unwrap_err()
        .to_string();
    assert!(unknown.contains("graph-does-not-exist"), "{unknown}");
}

/// FR1: a graph left with no matching row disappears from a filtered
/// answer, while an unfiltered answer still lists every graph — even
/// one that runs nothing.
#[test]
fn graph_crew_omits_graphs_with_no_matching_rows() {
    let fixture = crew_fixture();
    let only_claude = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            platform: Some("claude"),
            ..GraphCrewDbParams::default()
        })
        .unwrap();
    assert_eq!(only_claude.len(), 1, "the archived graph has no claude row");
    assert_eq!(only_claude[0].id, fixture.live_id);

    let nothing = fixture
        .db
        .graph_crew(GraphCrewDbParams {
            platform: Some("mimo"),
            ..GraphCrewDbParams::default()
        })
        .unwrap();
    assert!(nothing.is_empty());

    // A graph whose nodes are all static still shows up unfiltered…
    let db = test_db();
    let mut quiet = sample_graph("graph-crew-quiet");
    quiet.name = "Quiet".to_string();
    db.insert_graph(&quiet).unwrap();
    let spec = sample_graph_spec(&quiet.id, "spec-crew-quiet", 1);
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&static_node(&spec.id, "node-check", "Check", 0))
        .unwrap();
    let quiet_groups = db.graph_crew(GraphCrewDbParams::default()).unwrap();
    assert_eq!(quiet_groups.len(), 1);
    assert!(quiet_groups[0].rows.is_empty());
    // …and drops out of a filtered one.
    let filtered = db
        .graph_crew(GraphCrewDbParams {
            platform: Some("claude"),
            ..GraphCrewDbParams::default()
        })
        .unwrap();
    assert!(filtered.is_empty());
}

/// FR4: `group_by: "pair"` counts each distinct platform/model pair's
/// graphs, ensembles, nodes and hooks, and names the graphs.
#[test]
fn graph_crew_summary_groups_by_pair() {
    let fixture = crew_fixture();
    let summary = fixture
        .db
        .graph_crew_summary(GraphCrewDbParams::default())
        .unwrap();

    assert_eq!(
        summary.len(),
        3,
        "claude/opus, claude/sonnet, opencode-go/gpt-5"
    );
    let opus = summary
        .iter()
        .find(|s| s.platform == "claude" && s.model == "opus")
        .expect("claude/opus pair");
    assert_eq!(opus.graph_count, 1);
    assert_eq!(opus.ensemble_count, 1, "member m1 runs on the pair");
    assert_eq!(opus.node_count, 1, "the lone node");
    assert_eq!(opus.hook_count, 0);
    assert_eq!(opus.graph_names, vec!["Crew live".to_string()]);

    let sonnet = summary
        .iter()
        .find(|s| s.model == "sonnet")
        .expect("claude/sonnet pair");
    assert_eq!(sonnet.hook_count, 1);
    assert_eq!(sonnet.node_count, 0);

    let gpt = summary
        .iter()
        .find(|s| s.platform == "opencode-go")
        .expect("opencode-go/gpt-5 pair");
    assert_eq!(gpt.graph_count, 1, "archived graphs stay out by default");
    assert_eq!(gpt.ensemble_count, 1);
    assert_eq!(gpt.node_count, 0);

    let with_archived = fixture
        .db
        .graph_crew_summary(GraphCrewDbParams {
            include_archived: true,
            ..GraphCrewDbParams::default()
        })
        .unwrap();
    let gpt = with_archived
        .iter()
        .find(|s| s.platform == "opencode-go")
        .expect("opencode-go/gpt-5 pair");
    assert_eq!(gpt.graph_count, 2, "live member + archived node");
    assert_eq!(gpt.node_count, 1);
    let mut names = gpt.graph_names.clone();
    names.sort();
    assert_eq!(
        names,
        vec!["Crew archived".to_string(), "Crew live".to_string()]
    );
}

/// Walk every serialized key of a `graph_crew` result: `prompt` and
/// `prompt_template` must never appear, at any depth.
fn assert_no_prompt_keys(value: &serde_json::Value, path: &str) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                assert_ne!(key.as_str(), "prompt", "prompt leaked at {path}.{key}");
                assert_ne!(
                    key.as_str(),
                    "prompt_template",
                    "prompt_template leaked at {path}.{key}"
                );
                assert_no_prompt_keys(child, &format!("{path}.{key}"));
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                assert_no_prompt_keys(child, &format!("{path}[{index}]"));
            }
        }
        _ => {}
    }
}

/// FR2/FR5: no prompt text, ever — neither the key nor the fixture's
/// own prompt content, in the grouped result or the pair summary.
#[test]
fn graph_crew_serialization_contains_no_prompt_fields() {
    let fixture = crew_fixture();
    let groups = fixture.db.graph_crew(GraphCrewDbParams::default()).unwrap();

    let json = serde_json::to_string(&groups).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_no_prompt_keys(&value, "$");
    assert!(!json.contains("prompt"), "no prompt key at all: {json}");
    assert!(!json.contains(LEAKED_HOOK_PROMPT), "{json}");
    assert!(!json.contains(LEAKED_NODE_PROMPT), "{json}");

    let summary = fixture
        .db
        .graph_crew_summary(GraphCrewDbParams::default())
        .unwrap();
    let summary_json = serde_json::to_string(&summary).unwrap();
    assert!(!summary_json.contains("prompt"), "{summary_json}");
    assert!(!summary_json.contains(LEAKED_HOOK_PROMPT), "{summary_json}");
}

/// FR2 (CM48): availability comes from the roster — an unavailable
/// pair reports `unavailable` with its reason, everything else reads
/// `available`.
#[test]
fn graph_crew_reports_availability_from_the_roster() {
    let fixture = crew_fixture();
    fixture
        .db
        .roster_upsert(NewRosterEntry {
            platform: "claude",
            model_pattern: Some("opus"),
            state: RosterState::Unavailable,
            reason: Some("quota exhausted"),
            until: None,
        })
        .unwrap();

    let groups = fixture.db.graph_crew(GraphCrewDbParams::default()).unwrap();
    let opus_node = flat(&groups)
        .into_iter()
        .find(|r| r.kind == CrewRowKind::Node)
        .cloned()
        .expect("lone node row");
    assert_eq!(opus_node.availability.as_deref(), Some("unavailable"));
    assert_eq!(
        opus_node.availability_reason.as_deref(),
        Some("quota exhausted")
    );

    let hook = flat(&groups)
        .into_iter()
        .find(|r| r.kind == CrewRowKind::Hook)
        .cloned()
        .expect("hook row");
    assert_eq!(hook.availability.as_deref(), Some("available"));
    assert_eq!(hook.availability_reason, None);
}

/// FR2 (CM49): a crew-bound node or ensemble reports its crew and the
/// crew version applied — and, having no pair yet, no availability.
#[test]
fn graph_crew_reports_crew_bindings() {
    let db = test_db();
    let mut lp = sample_graph("graph-crew-bound");
    lp.name = "Bound".to_string();
    db.insert_graph(&lp).unwrap();
    let spec = sample_graph_spec(&lp.id, "spec-crew-bound", 1);
    db.insert_graph_spec(&spec).unwrap();

    // The crew must exist first: `ensembles.crew_id` is a foreign key
    // into `crews`. Two member-list writes after creation leave it at
    // version 3, so the reported version is the crew's own — not a
    // constant.
    let crew_members = [
        crew_member(0, "claude", Some("opus")),
        crew_member(1, "opencode-go", Some("gpt-5")),
    ];
    let crew = db.crew_create("reviewers", None, &crew_members).unwrap();
    for _ in 0..2 {
        db.crew_update("reviewers", Some(&crew_members), None, None)
            .unwrap();
    }
    insert_ensemble(
        &db,
        &spec.id,
        "ens-bound",
        "Bound reviewers",
        Some((crew.id.as_str(), crew.name.as_str(), 3)),
    );
    db.insert_graph_node(&agent_node(
        &spec.id,
        "node-crew",
        "Crew bound",
        5,
        serde_json::json!({ "crew": "implementers" }),
    ))
    .unwrap();

    let groups = db.graph_crew(GraphCrewDbParams::default()).unwrap();
    let rows = flat(&groups);
    let member = rows
        .iter()
        .find(|r| r.kind == CrewRowKind::EnsembleMember)
        .expect("ensemble member row");
    assert_eq!(member.crew.as_deref(), Some("reviewers"));
    assert_eq!(member.crew_version_applied, Some(3));

    let node = rows
        .iter()
        .find(|r| r.kind == CrewRowKind::Node)
        .expect("crew-bound node row");
    assert_eq!(node.crew.as_deref(), Some("implementers"));
    assert_eq!(node.crew_version_applied, None, "nodes track no version");
    assert_eq!(node.platform, "", "a crew-bound node has no pair yet");
    assert_eq!(node.availability, None);
}

/// FR3/FR5: the CLI table prints the fixture's name, kind, pair, crew
/// and availability columns — the same rows MCP returns.
#[test]
fn graph_crew_cli_table_format() {
    let fixture = crew_fixture();
    let groups = fixture.db.graph_crew(GraphCrewDbParams::default()).unwrap();
    let rows: Vec<GraphCrewRow> = flat(&groups).into_iter().cloned().collect();

    let table = format_graph_crew_table(&rows);
    assert!(table.contains("Kind"), "{table}");
    assert!(table.contains("Lone agent"), "{table}");
    assert!(table.contains("node"), "{table}");
    assert!(table.contains("ensemble_member"), "{table}");
    assert!(table.contains("hook"), "{table}");
    assert!(table.contains("claude"), "{table}");
    assert!(table.contains("opus"), "{table}");
    assert!(table.contains("on_completed"), "{table}");
    assert!(table.contains("available"), "{table}");
    assert!(!table.contains("prompt"), "{table}");

    assert_eq!(
        format_graph_crew_table(&[]),
        "No matching crew assignments found."
    );
}
