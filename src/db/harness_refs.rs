//! Who still names a harness: crews, graph nodes and scheduled agents.
//!
//! A removed harness stays configured while something references it —
//! pruning it would strand the reference. This module collects those
//! references so prune and doctor can tell "safe to drop" apart from
//! "still named somewhere".

use std::collections::BTreeMap;

use crate::db::Database;

/// One live reference to a harness name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessReference {
    /// The harness name as stored (`crew_members.platform`, node
    /// `config["platform"]`/`config["cli"]`, agent `cli`).
    pub harness: String,
    /// Human label, e.g. `crew lab-general member 3`,
    /// `graph 'g' node 'n'`, `scheduled agent abc123`.
    pub label: String,
}

fn config_platform(config: &serde_json::Value) -> Option<String> {
    ["platform", "cli"]
        .iter()
        .filter_map(|key| config.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

impl Database {
    /// Every harness name still referenced by a crew member, a graph node
    /// config, or a scheduled agent — deduplicated, sorted by
    /// `(harness, label)`. Never fails the caller: an unreadable table
    /// yields no references rather than a broken prune/doctor run.
    pub fn harness_references(&self) -> Vec<HarnessReference> {
        let mut refs: BTreeMap<(String, String), ()> = BTreeMap::new();
        let mut insert = |harness: String, label: String| {
            if !harness.is_empty() {
                refs.insert((harness, label), ());
            }
        };

        collect_crew_references(self, &mut insert);
        collect_node_references(self, &mut insert);
        collect_agent_references(self, &mut insert);

        refs.into_keys()
            .map(|(harness, label)| HarnessReference { harness, label })
            .collect()
    }
}

/// Crew members: `crew {name} member {position}` (1-based for humans).
fn collect_crew_references(db: &Database, insert: &mut impl FnMut(String, String)) {
    let Ok(crews) = db.crew_list() else {
        return;
    };
    for (crew, members) in &crews {
        for member in members {
            let platform = member.platform.trim();
            if platform.is_empty() {
                continue;
            }
            insert(
                platform.to_string(),
                format!("crew {} member {}", crew.name, member.position + 1),
            );
        }
    }
}

/// Graph node configs (`platform`, accepting the legacy `cli` alias):
/// `graph '{graph}' node '{node}'` — both spec nodes and top-level nodes.
fn collect_node_references(db: &Database, insert: &mut impl FnMut(String, String)) {
    let Ok(specs) = db.list_all_specs_for_conversion() else {
        return;
    };
    for spec in &specs {
        let Ok(Some(details)) = db.get_graph_spec_details(&spec.id) else {
            continue;
        };
        let graph_name = spec
            .graph_id
            .as_deref()
            .and_then(|id| db.get_graph(id).ok().flatten())
            .map(|graph| graph.name)
            .unwrap_or_else(|| spec.name.clone());
        collect_nodes(&details.nodes, &graph_name, insert);
    }
    let Ok(graphs) = db.list_graphs(None, true) else {
        return;
    };
    for graph in &graphs {
        let Ok(Some(details)) = db.get_graph_details(&graph.id) else {
            continue;
        };
        collect_nodes(&details.graph_nodes, &graph.name, insert);
        collect_hooks(&details.lp, insert);
    }
}

/// Agent hooks with an explicit platform (`graph '{graph}' {event} hook`):
/// they name a harness independently of any node config, so a harness only
/// a hook fires would otherwise be pruned and strand the hook. Crew-bound
/// hooks resolve their platform from the crew at fire time; crews are
/// already covered by [`collect_crew_references`], so only literal
/// platforms are collected here.
fn collect_hooks(graph: &crate::domain::graphs::Graph, insert: &mut impl FnMut(String, String)) {
    for (event, hooks) in &graph.hooks {
        for hook in hooks {
            let Some(platform) = hook.platform.as_deref().map(str::trim) else {
                continue;
            };
            if platform.is_empty() {
                continue;
            }
            insert(
                platform.to_string(),
                format!("graph '{}' {} hook", graph.name, event.as_str()),
            );
        }
    }
}

/// One `graph '{graph}' node '{node}'` line per node naming a harness.
fn collect_nodes(
    nodes: &[crate::domain::graphs::GraphNode],
    graph_name: &str,
    insert: &mut impl FnMut(String, String),
) {
    for node in nodes {
        let Some(harness) = config_platform(&node.config) else {
            continue;
        };
        insert(
            harness,
            format!("graph '{graph_name}' node '{}'", node.name),
        );
    }
}

/// Scheduled agents: `scheduled agent {id}`.
fn collect_agent_references(db: &Database, insert: &mut impl FnMut(String, String)) {
    let Ok(agents) = crate::application::ports::AgentRepository::list_agents(db) else {
        return;
    };
    for agent in &agents {
        insert(
            agent.cli.as_str().to_string(),
            format!("scheduled agent {}", agent.id),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::AgentRepository;
    use crate::domain::crews::CrewMember;
    use crate::domain::graphs::{
        Graph, GraphHookEvent, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus,
    };
    use crate::domain::models::{Agent, AgentAction, Cli};
    use tempfile::NamedTempFile;

    fn test_db() -> Database {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn crew_member(platform: &str) -> CrewMember {
        CrewMember {
            position: 0,
            platform: platform.to_string(),
            model: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn sample_agent(id: &str, cli: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "do things".to_string(),
            trigger: None,
            cli: Cli::new(cli),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/harness-refs-test.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: AgentAction::Headless,
        }
    }

    fn sample_graph(id: &str) -> Graph {
        Graph {
            id: id.to_string(),
            name: "refs-graph".to_string(),
            description: None,
            workdir: "/tmp/project".to_string(),
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
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }
    }

    fn sample_spec(id: &str, graph_id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: Some(graph_id.to_string()),
            name: "refs-spec".to_string(),
            description: None,
            position: 0,
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
        }
    }

    fn agent_node(spec_id: &str, platform: &str) -> GraphNode {
        GraphNode {
            id: format!("node-{platform}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("node-{platform}"),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": platform}),
            position: 0,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn graph_hook_platform_is_referenced_without_a_node() {
        let db = test_db();
        let mut graph = sample_graph("g1");
        graph.hooks.insert(
            GraphHookEvent::OnCompleted,
            vec![crate::domain::graphs::GraphCompletionHook {
                platform: Some("qwen".to_string()),
                crew: None,
                spec_ids: None,
                model: None,
                effort: None,
                prompt: Some("done".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        db.insert_graph(&graph).unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].harness, "qwen");
        assert_eq!(refs[0].label, "graph 'refs-graph' on_completed hook");
    }

    #[test]
    fn empty_database_has_no_references() {
        let db = test_db();
        assert!(db.harness_references().is_empty());
    }

    #[test]
    fn crew_member_platform_is_referenced_with_position() {
        let db = test_db();
        db.crew_create("lab-general", None, &[crew_member("qwen")])
            .unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].harness, "qwen");
        assert_eq!(refs[0].label, "crew lab-general member 1");
    }

    #[test]
    fn crew_member_platform_is_trimmed_before_matching() {
        let db = test_db();
        db.crew_create("lab-general", None, &[crew_member("  qwen  ")])
            .unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].harness, "qwen");
    }

    #[test]
    fn graph_node_platform_is_referenced_with_graph_and_node() {
        let db = test_db();
        db.insert_graph(&sample_graph("g1")).unwrap();
        db.insert_graph_spec(&sample_spec("s1", "g1")).unwrap();
        db.insert_graph_node(&agent_node("s1", "qoder")).unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].harness, "qoder");
        assert_eq!(refs[0].label, "graph 'refs-graph' node 'node-qoder'");
    }

    #[test]
    fn scheduled_agent_cli_is_referenced_by_id() {
        let db = test_db();
        db.upsert_agent(&sample_agent("nightly", "continue"))
            .unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].harness, "continue");
        assert_eq!(refs[0].label, "scheduled agent nightly");
    }

    #[test]
    fn same_harness_twice_yields_one_ref_per_position() {
        let db = test_db();
        db.crew_create(
            "lab-general",
            None,
            &[crew_member("qwen"), crew_member("qwen")],
        )
        .unwrap();
        let refs = db.harness_references();
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(|r| r.harness == "qwen"));
        assert_eq!(refs[0].label, "crew lab-general member 1");
        assert_eq!(refs[1].label, "crew lab-general member 2");
    }
}
