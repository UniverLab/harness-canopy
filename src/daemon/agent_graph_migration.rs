//! Daemon-start migration: one spec-less backing graph per background agent.
//!
//! Each agent row gets a graph with the agent's id as its id and name, the
//! agent's trigger/enabled/enable_at/expires_at copied, and a single worker
//! node: an agent node for `Headless` agents, a deliver node for
//! `SendToSession` agents. The row is then marked `migrated_to_graph` and
//! never scheduled by the legacy path again; the row itself is never
//! deleted (rollback safety). The migration is idempotent: already-marked
//! rows are skipped, and a graph that is already the agent's own backing
//! graph (a previous run created it but did not reach the mark) is adopted
//! instead of colliding.

use std::collections::BTreeMap;

use anyhow::Result;
use chrono::Utc;

use crate::db::Database;
use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphStatus};
use crate::domain::models::{Agent, AgentAction};
use crate::graph_engine::{
    first_disallowed_marker, legacy_agent_id, LEGACY_AGENT_ID_KEY, LEGACY_WATCH_APPEND_KEY,
};

/// Outcome of one daemon-start migration pass over the agents table.
#[derive(Debug, Default)]
pub struct AgentMigrationReport {
    /// Agent ids that now own a backing graph (newly created or adopted).
    pub migrated: Vec<String>,
    /// `(agent id, reason)` for agents left unmigrated: a disallowed
    /// marker, a name collision, or a database error.
    pub failed: Vec<(String, String)>,
    /// Corrupt rows quarantined (disabled) by this pass. Already-disabled
    /// corrupt rows are left alone so the warning fires exactly once.
    pub corrupt: Vec<String>,
}

/// Migrate every unmigrated agent row to its backing graph: idempotent,
/// one transaction per agent (see [`Database::insert_agent_backing_graph`]).
pub fn migrate_all_agents_to_graphs(db: &Database) -> Result<AgentMigrationReport> {
    let mut report = AgentMigrationReport::default();
    quarantine_corrupt_agents(db, &mut report)?;
    for agent in db.list_unmigrated_agents()? {
        migrate_one_agent(db, &agent, &mut report)?;
    }
    if !report.migrated.is_empty() {
        tracing::info!(
            "Migrated {} background agent(s) to spec-less backing graphs: {}",
            report.migrated.len(),
            report.migrated.join(", ")
        );
    }
    for (id, reason) in &report.failed {
        tracing::warn!("Agent '{id}' not migrated to a backing graph: {reason}");
    }
    Ok(report)
}

/// Quarantine corrupt rows exactly like the legacy scheduler path: disable
/// and warn once. A row disabled by an earlier pass is already quarantined,
/// so it is reported (and warned) only by the pass that disables it.
fn quarantine_corrupt_agents(db: &Database, report: &mut AgentMigrationReport) -> Result<()> {
    use crate::application::ports::AgentRepository;
    for corrupt in db.list_corrupt_agents()? {
        if !corrupt.enabled {
            continue;
        }
        tracing::warn!(
            "Agent '{}' has a corrupt trigger_config and cannot be scheduled ({}); \
             quarantining (disabling) it",
            corrupt.id,
            corrupt.error
        );
        db.update_agent_enabled(&corrupt.id, false)?;
        report.corrupt.push(corrupt.id);
    }
    Ok(())
}

fn migrate_one_agent(
    db: &Database,
    agent: &Agent,
    report: &mut AgentMigrationReport,
) -> Result<()> {
    if let Some(marker) = first_disallowed_marker(&agent.prompt) {
        report.failed.push((
            agent.id.clone(),
            format!(
                "agent '{}' prompt uses marker '{{{marker}}}' which a migrated backing graph \
                 cannot bind (allowed: TIMESTAMP, TASK_ID, LOG_PATH, FILE_PATH, EVENT_TYPE, \
                 spec_name, spec_content, previous_output)",
                agent.id
            ),
        ));
        return Ok(());
    }
    if let Some(existing) = db.get_graph(&agent.id)? {
        if is_own_backing_graph(db, &agent.id)? {
            db.set_migrated_to_graph(&agent.id)?;
            report.migrated.push(agent.id.clone());
        } else {
            report.failed.push((
                agent.id.clone(),
                format!(
                    "agent '{}' collides with existing graph '{}' which is not its backing \
                     graph; rename one of them before migrating",
                    agent.id, existing.id
                ),
            ));
        }
        return Ok(());
    }
    let graph = backing_graph(agent);
    let node = backing_node(agent);
    if let Err(error) = db.insert_agent_backing_graph(&graph, &node) {
        report.failed.push((
            agent.id.clone(),
            format!(
                "agent '{}' backing graph insert failed: {error:#}",
                agent.id
            ),
        ));
        return Ok(());
    }
    report.migrated.push(agent.id.clone());
    Ok(())
}

/// Whether the graph with the agent's id is already that agent's own
/// backing graph from an earlier (possibly partial) migration pass: one of
/// its top-level nodes carries this agent's id as `legacy_agent_id`.
fn is_own_backing_graph(db: &Database, agent_id: &str) -> Result<bool> {
    Ok(db
        .list_graph_nodes_for_graph(agent_id)?
        .iter()
        .any(|node| legacy_agent_id(node).as_deref() == Some(agent_id)))
}

/// The spec-less backing graph: the agent's id as id and name, its trigger
/// and enable/expiry fields copied, and the agent prompt as `trigger_prompt`
/// so every tick/watch fire launches with an idea (a spec-less graph cannot
/// launch on an empty idea).
fn backing_graph(agent: &Agent) -> Graph {
    let now = Utc::now();
    let workdir = agent
        .working_dir
        .clone()
        .filter(|dir| !dir.trim().is_empty())
        .unwrap_or_else(fallback_workdir);
    Graph {
        id: agent.id.clone(),
        name: agent.id.clone(),
        description: None,
        workdir,
        status: GraphStatus::Draft,
        trigger: agent.trigger.clone(),
        trigger_prompt: Some(agent.prompt.clone()),
        created_at: now,
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: BTreeMap::new(),
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
        enabled: agent.enabled,
        enable_at: agent.enable_at,
        expires_at: agent.expires_at,
    }
}

/// The backing graph's single worker node: an agent node carrying the
/// headless execution config (the run workdir comes from the graph, exactly
/// like every other agent node), or a deliver node carrying the
/// send-to-session target verbatim. Both keep the agent prompt verbatim as
/// `prompt_template` and carry the legacy keys the two-pass render reads.
fn backing_node(agent: &Agent) -> GraphNode {
    let now = Utc::now();
    let watch_append = agent.is_watch();
    match &agent.action {
        AgentAction::Headless => GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(agent.id.clone()),
            name: "agent".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": agent.cli.as_str(),
                "model": agent.model,
                "effort": agent.effort,
                "timeout_minutes": agent.timeout_minutes,
                "prompt_template": agent.prompt,
                LEGACY_AGENT_ID_KEY: agent.id,
                LEGACY_WATCH_APPEND_KEY: watch_append,
            }),
            position: 0,
            created_at: now,
        },
        AgentAction::SendToSession {
            target_session_name,
            target_session_id,
        } => GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(agent.id.clone()),
            name: "deliver".to_string(),
            kind: GraphNodeKind::Deliver,
            config: serde_json::json!({
                "target_session_name": target_session_name,
                "target_session_id": target_session_id,
                "prompt_template": agent.prompt,
                LEGACY_AGENT_ID_KEY: agent.id,
                LEGACY_WATCH_APPEND_KEY: watch_append,
            }),
            position: 0,
            created_at: now,
        },
    }
}

fn fallback_workdir() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

/// Bring one agent row's backing graph up to date after an `agent_edit`
/// mutation (or create it for an agent added while the daemon runs, after
/// the startup migration). Mirrors [`migrate_one_agent`] but updates an
/// existing own-backing graph in place — trigger, trigger prompt, lifecycle
/// fields, workdir and the worker node — instead of only creating it, so
/// run history on the backing graph survives edits.
///
/// Fails (leaving the agent row untouched — the caller keeps its own
/// result) when the prompt holds a disallowed marker or the graph id
/// collides with a graph that is not this agent's backing graph.
pub fn sync_agent_backing_graph(db: &Database, agent: &Agent) -> Result<()> {
    if let Some(marker) = first_disallowed_marker(&agent.prompt) {
        anyhow::bail!(
            "agent '{}' prompt uses marker '{{{marker}}}' which a migrated backing graph \
             cannot bind (allowed: TIMESTAMP, TASK_ID, LOG_PATH, FILE_PATH, EVENT_TYPE, \
             spec_name, spec_content, previous_output)",
            agent.id
        );
    }
    let Some(existing) = db.get_graph(&agent.id)? else {
        let graph = backing_graph(agent);
        let node = backing_node(agent);
        db.insert_agent_backing_graph(&graph, &node)?;
        return Ok(());
    };
    if !is_own_backing_graph(db, &agent.id)? {
        anyhow::bail!(
            "agent '{}' collides with existing graph '{}' which is not its backing \
             graph; rename one of them before migrating",
            agent.id,
            existing.id
        );
    }
    db.update_graph_trigger(&agent.id, agent.trigger.as_ref())?;
    db.update_graph_trigger_prompt(&agent.id, Some(&agent.prompt))?;
    db.update_graph_lifecycle(
        &agent.id,
        Some(agent.enabled),
        Some(agent.enable_at),
        Some(agent.expires_at),
    )?;
    if let Some(workdir) = agent
        .working_dir
        .as_deref()
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
    {
        db.update_graph_details(&agent.id, None, None, Some(workdir))?;
    }
    sync_backing_node(db, agent)?;
    db.set_migrated_to_graph(&agent.id)?;
    Ok(())
}

/// Rewrite the own-backing graph's worker node from the agent row: the node
/// kind and config follow the action (headless/deliver), so a retargeted
/// send agent or a headless/send switch takes effect without losing the
/// graph's run history. A backing graph with no legacy node yet (an
/// adopted empty graph) gets a fresh worker node.
fn sync_backing_node(db: &Database, agent: &Agent) -> Result<()> {
    let fresh = backing_node(agent);
    let legacy: Vec<_> = db
        .list_graph_nodes_for_graph(&agent.id)?
        .into_iter()
        .filter(|node| legacy_agent_id(node).as_deref() == Some(agent.id.as_str()))
        .collect();
    if legacy.is_empty() {
        db.insert_graph_node(&fresh)?;
        return Ok(());
    }
    for node in &legacy {
        db.update_graph_node_details(
            &node.id,
            Some(&fresh.name),
            Some(fresh.kind),
            Some(&fresh.config),
            Some(fresh.position),
        )?;
    }
    Ok(())
}

/// Best-effort backing-graph sync after an `agent_edit` mutation: the
/// agent-facing result and error texts never change — a sync problem is a
/// warning, and the startup migration (or the next mutation) repairs it.
pub fn sync_agent_backing(db: &Database, agent: &Agent) {
    if let Err(e) = sync_agent_backing_graph(db, agent) {
        tracing::warn!("Agent '{}' backing graph sync skipped: {e:#}", agent.id);
    }
}

/// Post-`update` sync: a rename orphans the old id's backing graph (removed
/// first so it can never fire under the stale id), then the row's current
/// state syncs onto the new id's backing graph.
pub fn sync_backing_after_update(db: &Database, old_id: &str, new_id: Option<&str>, agent: &Agent) {
    let renamed = new_id.is_some_and(|new_id| new_id != old_id);
    if renamed {
        if let Err(e) = remove_agent_backing_graph(db, old_id) {
            tracing::warn!("Agent '{old_id}' old backing graph removal skipped: {e:#}");
        }
    }
    sync_agent_backing(db, agent);
}

/// Delete the agent's own backing graph (on `agent_edit remove`): the run
/// history goes with it, exactly like the legacy `runs` rows the row delete
/// already cascades. A foreign graph with the same id is never touched.
/// Returns whether a backing graph was deleted.
pub fn remove_agent_backing_graph(db: &Database, agent_id: &str) -> Result<bool> {
    if !is_own_backing_graph(db, agent_id)? {
        return Ok(false);
    }
    db.delete_graph(agent_id)?;
    Ok(true)
}

/// The agent's execution stats, read through its backing graph when
/// migrated: `last_run_at`/`last_run_ok` from the newest graph run,
/// `trigger_count` from the graph run count. Unmigrated agents read the
/// legacy row fields unchanged, so the tool surface keeps its shape (and
/// values) before and after migration.
pub fn agent_run_stats(
    db: &Database,
    agent: &Agent,
) -> (Option<chrono::DateTime<Utc>>, Option<bool>, u64) {
    if db.is_agent_migrated(&agent.id).unwrap_or(false) {
        if let Ok(stats) = backing_run_stats(db, &agent.id) {
            return stats;
        }
    }
    (agent.last_run_at, agent.last_run_ok, agent.trigger_count)
}

/// Newest-first stats over the backing graph's runs.
fn backing_run_stats(
    db: &Database,
    graph_id: &str,
) -> Result<(Option<chrono::DateTime<Utc>>, Option<bool>, u64)> {
    let mut runs = db.list_graph_runs_for_graph(graph_id)?;
    runs.sort_by_key(|run| (run.started_at, run.iteration, run.id.clone()));
    let (last_at, last_ok) = runs.last().map_or((None, None), |run| {
        (
            Some(run.started_at),
            Some(matches!(
                run.status,
                crate::domain::graphs::GraphRunStatus::Pass
            )),
        )
    });
    Ok((last_at, last_ok, runs.len() as u64))
}

/// The live log path for an agent: the backing graph's log file once
/// migrated (graph runs append their blocks there, and the two-pass render
/// binds `{{LOG_PATH}}` to it), else the row's own log path. Best-effort:
/// any lookup failure keeps the row value so log reads never break.
pub fn backing_log_path(db: &Database, agent: &Agent) -> String {
    if db.is_agent_migrated(&agent.id).unwrap_or(false) {
        if let Ok(Some(graph)) = db.get_graph(&agent.id) {
            return graph.log_path();
        }
    }
    agent.log_path.clone()
}

/// The agent with execution stats overlaid from its backing graph when
/// migrated (see [`agent_run_stats`]). Read paths (`agent list`/`get`)
/// format this instead of the bare row so run data stays live after the
/// legacy executor stops writing the row.
pub fn with_backing_run_stats(db: &Database, mut agent: Agent) -> Agent {
    let (last_run_at, last_run_ok, trigger_count) = agent_run_stats(db, &agent);
    agent.last_run_at = last_run_at;
    agent.last_run_ok = last_run_ok;
    agent.trigger_count = trigger_count;
    agent.log_path = backing_log_path(db, &agent);
    agent
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{AgentRepository, RunRepository};
    use crate::domain::models::{Cli, Trigger, WatchEvent};
    use crate::graph_engine::{render_legacy_migrated_prompt, FireCtx};

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn headless_cron(id: &str, prompt: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: prompt.to_string(),
            trigger: Some(Trigger::Cron {
                schedule_expr: "*/5 * * * *".to_string(),
            }),
            cli: Cli::new("opencode"),
            model: Some("m1".to_string()),
            effort: Some("high".to_string()),
            working_dir: Some("/tmp/work".to_string()),
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: format!("/tmp/{id}.log"),
            timeout_minutes: 20,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: AgentAction::Headless,
        }
    }

    fn watch_send(id: &str, prompt: &str) -> Agent {
        let mut agent = headless_cron(id, prompt);
        agent.trigger = Some(Trigger::Watch {
            path: "/tmp/watched".to_string(),
            events: vec![WatchEvent::Modify],
            debounce_seconds: 2,
            recursive: false,
        });
        agent.action = AgentAction::send_to_session(Some("orch".to_string()), None).unwrap();
        agent
    }

    fn worker_node(db: &Database, agent_id: &str) -> GraphNode {
        let nodes = db.list_graph_nodes_for_graph(agent_id).unwrap();
        assert_eq!(
            nodes.len(),
            1,
            "backing graph holds exactly one worker node"
        );
        nodes.into_iter().next().unwrap()
    }

    #[test]
    fn migrates_cron_headless_agent_to_backing_graph() {
        let db = test_db();
        let agent = headless_cron(
            "cron-1",
            "run {{TASK_ID}} at {{TIMESTAMP}} log {{LOG_PATH}}",
        );
        db.upsert_agent(&agent).unwrap();

        let report = migrate_all_agents_to_graphs(&db).unwrap();
        assert_eq!(report.migrated, vec!["cron-1".to_string()]);
        assert!(report.failed.is_empty());

        let graph = db
            .get_graph("cron-1")
            .unwrap()
            .expect("backing graph exists");
        assert_eq!(graph.name, "cron-1");
        assert_eq!(graph.workdir, "/tmp/work");
        assert!(graph.trigger.is_some());
        assert_eq!(graph.trigger_prompt.as_deref(), Some(agent.prompt.as_str()));
        assert!(graph.enabled);

        let node = worker_node(&db, "cron-1");
        assert_eq!(node.kind, GraphNodeKind::Agent);
        assert_eq!(node.config["platform"], serde_json::json!("opencode"));
        assert_eq!(node.config["model"], serde_json::json!("m1"));
        assert_eq!(node.config["timeout_minutes"], serde_json::json!(20));
        assert_eq!(
            node.config["prompt_template"],
            serde_json::json!(agent.prompt)
        );
        assert_eq!(
            node.config[LEGACY_AGENT_ID_KEY],
            serde_json::json!("cron-1")
        );

        // Legacy variables substitute identically through the two-pass render.
        let fire = FireCtx::default();
        let rendered =
            render_legacy_migrated_prompt(&agent.prompt, "cron-1", &graph.log_path(), &fire, false);
        assert!(rendered.starts_with("run cron-1 at "), "{rendered}");
        assert!(rendered.contains(&graph.log_path()), "{rendered}");
        assert!(!rendered.contains("{{"), "{rendered}");

        assert!(db.is_agent_migrated("cron-1").unwrap());
        assert!(db.list_cron_agents().unwrap().is_empty());
    }

    #[test]
    fn migrates_watch_agent_with_fire_context_and_block() {
        let db = test_db();
        let agent = watch_send(
            "watch-1",
            "check {{FILE_PATH}} {{EVENT_TYPE}} as {{spec_name}}",
        );
        db.upsert_agent(&agent).unwrap();

        let report = migrate_all_agents_to_graphs(&db).unwrap();
        assert_eq!(report.migrated, vec!["watch-1".to_string()]);

        let node = worker_node(&db, "watch-1");
        assert_eq!(node.kind, GraphNodeKind::Deliver);
        assert_eq!(
            node.config["target_session_name"],
            serde_json::json!("orch")
        );
        assert_eq!(
            node.config["prompt_template"],
            serde_json::json!(agent.prompt)
        );
        assert_eq!(
            node.config[LEGACY_WATCH_APPEND_KEY],
            serde_json::json!(true)
        );

        let graph = db.get_graph("watch-1").unwrap().unwrap();
        let fire = FireCtx {
            trigger: Some(crate::graph_engine::GraphRunTrigger::Watch),
            file_path: Some("/repo/a.rs".to_string()),
            event_type: Some("modify".to_string()),
        };
        let rendered =
            render_legacy_migrated_prompt(&agent.prompt, "watch-1", &graph.log_path(), &fire, true);
        assert!(
            rendered.contains("check /repo/a.rs modify as watch-1"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "---\nTriggered by canopy watch agent 'watch-1'.\nChanged path: /repo/a.rs\nEvent: modify"
            ),
            "{rendered}"
        );

        // Manual fires never append the block.
        let manual = render_legacy_migrated_prompt(
            &agent.prompt,
            "watch-1",
            &graph.log_path(),
            &FireCtx::default(),
            true,
        );
        assert!(!manual.contains("Triggered by canopy"), "{manual}");
    }

    #[test]
    fn migrates_send_to_session_agent_copying_target_verbatim() {
        let db = test_db();
        let mut agent = headless_cron("send-1", "ping {{TASK_ID}} for {{spec_name}}");
        agent.action = AgentAction::send_to_session(None, Some("sess-42".to_string())).unwrap();
        db.upsert_agent(&agent).unwrap();

        migrate_all_agents_to_graphs(&db).unwrap();
        let node = worker_node(&db, "send-1");
        assert_eq!(node.kind, GraphNodeKind::Deliver);
        assert_eq!(
            node.config["target_session_id"],
            serde_json::json!("sess-42")
        );
        assert!(
            node.config.get("target_session_name").is_none()
                || node.config["target_session_name"].is_null()
        );

        // Two-pass render: legacy variables substitute, then the router
        // markers bind to the agent (single pass, no recursion).
        let graph = db.get_graph("send-1").unwrap().unwrap();
        let rendered = render_legacy_migrated_prompt(
            &agent.prompt,
            "send-1",
            &graph.log_path(),
            &FireCtx::default(),
            false,
        );
        assert_eq!(rendered, "ping send-1 for send-1", "{rendered}");

        // Migrated sends carry deliver provenance with graph, node and run
        // ids — strictly more traceable than the legacy agent trigger.
        let provenance = crate::db::scheduled_prompts::ScheduledPromptProvenance::deliver(
            &graph.id, &node.id, "run-7",
        );
        assert_eq!(provenance.kind, "deliver");
        assert_eq!(provenance.graph_id, "send-1");
        assert_eq!(provenance.node_id.as_deref(), Some(node.id.as_str()));
        assert_eq!(provenance.run_id.as_deref(), Some("run-7"));
    }

    #[test]
    fn migrates_disabled_expired_agent_keeping_lifecycle_fields() {
        let db = test_db();
        let mut agent = headless_cron("old-1", "done");
        agent.enabled = false;
        agent.expires_at = Some(Utc::now() - chrono::Duration::hours(1));
        db.upsert_agent(&agent).unwrap();

        let report = migrate_all_agents_to_graphs(&db).unwrap();
        assert_eq!(report.migrated, vec!["old-1".to_string()]);

        let graph = db.get_graph("old-1").unwrap().unwrap();
        assert!(!graph.enabled);
        assert!(graph.is_expired());
    }

    #[test]
    fn disallowed_marker_fails_migration_naming_it() {
        let db = test_db();
        db.upsert_agent(&headless_cron("bad-1", "do {{previous_feedback}} now"))
            .unwrap();

        let report = migrate_all_agents_to_graphs(&db).unwrap();
        assert!(report.migrated.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "bad-1");
        assert!(
            report.failed[0].1.contains("previous_feedback"),
            "{}",
            report.failed[0].1
        );
        assert!(db.get_graph("bad-1").unwrap().is_none());
        assert!(!db.is_agent_migrated("bad-1").unwrap());
    }

    #[test]
    fn name_collision_fails_loudly_naming_both() {
        let db = test_db();
        db.upsert_agent(&headless_cron("clash", "run it")).unwrap();
        // A foreign graph already owns the agent's id.
        db.insert_graph(&foreign_graph("clash")).unwrap();

        let report = migrate_all_agents_to_graphs(&db).unwrap();
        assert!(report.migrated.is_empty());
        assert_eq!(report.failed.len(), 1);
        let message = &report.failed[0].1;
        assert!(message.contains("'clash'"), "{message}");
        assert!(message.contains("agent"), "{message}");
        assert!(message.contains("graph"), "{message}");
    }

    #[test]
    fn second_start_is_idempotent() {
        let db = test_db();
        db.upsert_agent(&headless_cron("idem-1", "run it")).unwrap();

        let first = migrate_all_agents_to_graphs(&db).unwrap();
        assert_eq!(first.migrated, vec!["idem-1".to_string()]);
        let second = migrate_all_agents_to_graphs(&db).unwrap();
        assert!(second.migrated.is_empty());
        assert!(second.failed.is_empty());
        assert_eq!(db.list_graph_nodes_for_graph("idem-1").unwrap().len(), 1);
    }

    #[test]
    fn corrupt_row_is_quarantined_and_reported_once() {
        let db = test_db();
        insert_corrupt_agent(&db, "corrupt-1", true);

        let first = migrate_all_agents_to_graphs(&db).unwrap();
        assert_eq!(first.corrupt, vec!["corrupt-1".to_string()]);
        // The row is quarantined (disabled) and still present for the
        // downgrade; a second pass reports nothing new.
        let quarantined = db.list_corrupt_agents().unwrap();
        assert_eq!(quarantined.len(), 1);
        assert!(!quarantined[0].enabled);

        let second = migrate_all_agents_to_graphs(&db).unwrap();
        assert!(second.corrupt.is_empty());
    }

    #[test]
    fn busy_backing_graph_records_missed_without_staleness() {
        use crate::domain::graphs::{GraphNodeRun, GraphRunStatus};
        let db = test_db();
        db.upsert_agent(&headless_cron("busy-1", "run it")).unwrap();
        migrate_all_agents_to_graphs(&db).unwrap();
        let spec = crate::graph_engine::no_spec_placeholder("busy-1");
        db.insert_graph_spec(&spec).unwrap();

        // Idle graph: no Missed.
        assert!(!db
            .record_agent_backing_missed("busy-1", crate::domain::models::TriggerType::Scheduled)
            .unwrap());
        assert!(db.list_runs("busy-1", 10).unwrap().is_empty());

        // Busy graph: exactly one Missed, no stale threshold involved.
        let now = Utc::now();
        let worker = db.list_graph_nodes_for_graph("busy-1").unwrap();
        db.insert_graph_run(&GraphNodeRun {
            id: "run-busy".to_string(),
            graph_id: "busy-1".to_string(),
            spec_id: spec.id,
            node_id: worker[0].id.clone(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: now,
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
        assert!(db
            .record_agent_backing_missed("busy-1", crate::domain::models::TriggerType::Scheduled)
            .unwrap());
        let runs = db.list_runs("busy-1", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, crate::domain::models::RunStatus::Missed);

        // Disabled graph: no Missed even when a stale row lingers.
        db.update_graph_lifecycle("busy-1", Some(false), None, None)
            .unwrap();
        assert!(!db
            .record_agent_backing_missed("busy-1", crate::domain::models::TriggerType::Scheduled)
            .unwrap());
        assert_eq!(db.list_runs("busy-1", 10).unwrap().len(), 1);
    }

    #[test]
    fn agent_json_shape_is_identical_before_and_after_migration() {
        use crate::daemon::handler_formatting::agent_detail_json;
        let db = test_db();
        let agent = headless_cron("shape-1", "run {{TASK_ID}}");
        db.upsert_agent(&agent).unwrap();

        let before = agent_detail_json(&agent, false);
        migrate_all_agents_to_graphs(&db).unwrap();
        let stored = db.get_agent("shape-1").unwrap().unwrap();
        let after = agent_detail_json(&with_backing_run_stats(&db, stored), false);

        let mut before_keys: Vec<_> = before.as_object().unwrap().keys().collect();
        let mut after_keys: Vec<_> = after.as_object().unwrap().keys().collect();
        before_keys.sort();
        after_keys.sort();
        assert_eq!(before_keys, after_keys);
        for key in ["id", "prompt", "cli", "trigger_type", "action"] {
            assert_eq!(
                before[key], after[key],
                "key '{key}' must survive migration"
            );
        }
    }

    #[test]
    fn migrated_agent_reads_live_log_path_from_backing_graph() {
        use crate::daemon::handler_formatting::resolve_log_path;
        let db = test_db();
        db.upsert_agent(&headless_cron("log-1", "run it")).unwrap();
        let row = db.get_agent("log-1").unwrap().unwrap();

        // Before migration the row's own path is live.
        assert_eq!(backing_log_path(&db, &row), row.log_path);
        assert_eq!(resolve_log_path(&db, "log-1").unwrap(), row.log_path);

        migrate_all_agents_to_graphs(&db).unwrap();
        let graph = db.get_graph("log-1").unwrap().unwrap();
        assert_ne!(
            graph.log_path(),
            row.log_path,
            "the backing graph appends run blocks to its own log file"
        );

        // After migration every read path follows the backing graph's file,
        // where graph runs (and `{{LOG_PATH}}`) actually land.
        let stored = db.get_agent("log-1").unwrap().unwrap();
        assert_eq!(backing_log_path(&db, &stored), graph.log_path());
        assert_eq!(
            with_backing_run_stats(&db, stored).log_path,
            graph.log_path()
        );
        assert_eq!(resolve_log_path(&db, "log-1").unwrap(), graph.log_path());
    }

    fn foreign_graph(id: &str) -> Graph {
        Graph {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            trigger_prompt: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }
    }

    fn insert_corrupt_agent(db: &Database, id: &str, enabled: bool) {
        crate::db::agent_backing::insert_corrupt_agent_row(db, id, enabled).unwrap();
    }
}
