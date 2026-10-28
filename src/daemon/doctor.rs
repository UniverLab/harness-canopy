use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::application::ports::{AgentRepository, StateRepository};
use crate::daemon::process::{
    diagnose_daemon, is_process_running, read_pid, resolve_port_pid, service_manager_facts,
    DaemonState,
};
use crate::db::Database;
use crate::domain::db_health::DbHealthStatus;
use crate::domain::db_paths::database_path;
use crate::domain::graphs::{
    EnsembleDetails, GraphCompletionHook, GraphHookEvent, GraphNode, GraphStatus,
};
use crate::domain::validation::{find_stale_2x_names, StaleNameHit};

mod service_unit;
use service_unit::*;
mod path_copies;
use path_copies::*;
mod rag_report;
use rag_report::*;

/// CB68 FR3: aggregated result of scanning every non-archived graph's
/// node/ensemble prompts and hooks, and every scheduled agent's prompt, for a
/// stale 2.x `loop_<name>` tool name or the gone `canopy loop` CLI. Grouped
/// one entry per graph (not one per node) so doctor prints a readable line per
/// graph. Pure over an already-open `Database` — no printing — so tests can
/// assert on a fixture DB directly (stdout-fd capture is unreliable in CI; see
/// the `#[ignore]` notes on the `run_doctor_*` fixture tests below).
pub(crate) struct StaleNameReport {
    pub graphs: Vec<StaleGraphNames>,
    pub agents: Vec<StaleAgentNames>,
}

/// Distinct stale names found in one graph, with the total reference count
/// (hits, not distinct pairs) and the graph's current top-level status.
pub(crate) struct StaleGraphNames {
    pub graph_name: String,
    pub status: GraphStatus,
    pub ref_count: usize,
    pub names: Vec<StaleNameHit>,
}

/// Distinct stale names found in one scheduled agent's prompt.
pub(crate) struct StaleAgentNames {
    pub agent_id: String,
    pub ref_count: usize,
    pub names: Vec<StaleNameHit>,
}

/// Unique old→new pairs, preserving first-seen order. O(n²) but n < ~10.
fn distinct_hits(hits: Vec<StaleNameHit>) -> Vec<StaleNameHit> {
    let mut distinct: Vec<StaleNameHit> = Vec::new();
    for hit in hits {
        if !distinct.contains(&hit) {
            distinct.push(hit);
        }
    }
    distinct
}

pub(crate) fn find_stale_2x_name_hits(db: &Database) -> anyhow::Result<StaleNameReport> {
    let mut graphs = Vec::new();
    for graph in db.list_graphs(None, false)? {
        let Some(details) = db.get_graph_details(&graph.id)? else {
            continue;
        };
        let mut hits: Vec<StaleNameHit> = Vec::new();
        hits.extend(graph_node_hits(
            details
                .graph_nodes
                .iter()
                .chain(details.specs.iter().flat_map(|s| s.nodes.iter())),
        ));
        hits.extend(graph_hook_hits(&details.lp.hooks));
        let mut ensembles = db.list_ensembles_for_graph(&graph.id)?;
        for spec in &details.specs {
            ensembles.extend(db.list_ensembles_for_spec(&spec.spec.id)?);
        }
        hits.extend(graph_ensemble_hits(&ensembles));
        if !hits.is_empty() {
            graphs.push(StaleGraphNames {
                graph_name: graph.name,
                status: details.lp.status,
                ref_count: hits.len(),
                names: distinct_hits(hits),
            });
        }
    }

    let mut agents = Vec::new();
    for agent in db.list_agents()? {
        let hits = find_stale_2x_names(&agent.prompt);
        if !hits.is_empty() {
            agents.push(StaleAgentNames {
                agent_id: agent.id,
                ref_count: hits.len(),
                names: hits,
            });
        }
    }

    Ok(StaleNameReport { graphs, agents })
}

/// Stale-name hits in every node config of one graph: the top-level graph
/// nodes and each spec's nodes, `prompt_template` then `command`.
fn graph_node_hits<'a>(nodes: impl Iterator<Item = &'a GraphNode>) -> Vec<StaleNameHit> {
    let mut hits = Vec::new();
    for node in nodes {
        for field in ["prompt_template", "command"] {
            if let Some(text) = node.config.get(field).and_then(|v| v.as_str()) {
                hits.extend(find_stale_2x_names(text));
            }
        }
    }
    hits
}

/// Stale-name hits in one graph's hooks, each hook's prompt then its command.
fn graph_hook_hits(
    hooks: &BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
) -> Vec<StaleNameHit> {
    let mut hits = Vec::new();
    for hook_list in hooks.values() {
        for hook in hook_list {
            for text in [hook.prompt.as_deref(), hook.command.as_deref()]
                .into_iter()
                .flatten()
            {
                hits.extend(find_stale_2x_names(text));
            }
        }
    }
    hits
}

/// Stale-name hits in one graph's ensembles: the shared prompt first, then
/// each member's prompt override.
fn graph_ensemble_hits(ensembles: &[EnsembleDetails]) -> Vec<StaleNameHit> {
    let mut hits = Vec::new();
    for ens in ensembles {
        hits.extend(find_stale_2x_names(&ens.ensemble.prompt_template));
        for member in &ens.members {
            let Some(prompt_override) = &member.prompt_override else {
                continue;
            };
            hits.extend(find_stale_2x_names(prompt_override));
        }
    }
    hits
}

/// Print doctor's "verified" line — the ✓ glyph reserved for a check that
/// actually exercised the capability it reports on (opened the database,
/// opened the vector store, confirmed a resolved binary is executable,
/// ...), never one that merely read a declared value or stat'd a path.
/// This is the only place in this module allowed to embed the raw glyph
/// escape sequence: `success_glyph_only_printed_by_shared_helper` (in
/// `tests`) greps this file's own source and fails if that sequence shows
/// up anywhere else, so a future check can't print a false-positive tick
/// just by typing it inline the way the old vector-store check did.
fn success(message: impl std::fmt::Display) {
    println!(" \x1b[32m✓\x1b[0m {message}");
}

/// [`success`], indented for a line nested under a parent check (e.g. the
/// service unit's binary detail line).
fn success_nested(message: impl std::fmt::Display) {
    println!("     \x1b[32m✓\x1b[0m {message}");
}

pub(crate) async fn run_doctor() -> Result<()> {
    use crate::shared::banner;

    banner::print_banner_with_gradient("canopy doctor");
    println!();

    let home = dirs::home_dir().context("No home directory")?;
    let canopy_dir = home.join(".canopy");
    let db_path = database_path(&canopy_dir);

    let mut issues: Vec<String> = Vec::new();

    report_data_directory(&canopy_dir, &mut issues);
    report_database(&db_path, &canopy_dir, &mut issues);

    let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    report_configuration(&canopy_dir, &config);

    let daemon = resolve_daemon_facts(&canopy_dir, &db_path);
    report_daemon_status(&daemon, &mut issues);

    // ── Legacy/new data layout split ────────────────────────────────
    // `usage.toml` and `cache/` migrations defer deleting their legacy
    // counterpart while a daemon may still be using it (see
    // `usage_stats::migrate_legacy_json` / `models_db::migrate_legacy_caches`),
    // so a lingering split is expected during that window — but it's the
    // one thing an operator can actually observe from outside, and
    // otherwise has no way to explain.
    report_layout_split(&canopy_dir, daemon.state_pid, &mut issues);

    // ── Service Unit ──────────────────────────────────────────────
    // A unit that exists but points at a deleted/stale binary makes
    // systemd/launchd retry-graph the daemon forever with nothing on the
    // port — from here that's indistinguishable from "never started" unless
    // doctor reads the unit itself and says so.
    //
    // CB73: the BROWSER check judges the *effective* environment (merged
    // drop-ins via `systemctl show`), plus the environ of an orphaned port
    // holder when that process isn't the unit's MainPID (CB72) — the
    // running daemon's environment is the one harnesses actually inherit.
    report_service_unit_section(&home, &daemon, &mut issues);

    // ── Orphaned joins (CB52) ─────────────────────────────────────
    // Databases damaged before the entry/exit FKs became RESTRICT hold join
    // nodes with no ensemble row. Never auto-repaired (C1) — listed here
    // per graph so the operator can delete or rewire them by hand.
    report_orphan_joins(&canopy_dir, &mut issues);

    // ── Duplicate Binaries (C17) ────────────────────────────────────
    // A tool installed by both the install script (~/.local/bin) and
    // `cargo install` (~/.cargo/bin) leaves two binaries on PATH — updating
    // one and running the other produces old behaviour with nothing to
    // explain it. Walk PATH the way the shell does and say which copy
    // actually runs.
    report_canopy_path_copies(&mut issues).await;

    report_setup_completion(&config, &mut issues);
    report_cli_resolution(&config, &mut issues);

    // ── RAG Health ──────────────────────────────────────────────
    // Re-read config.toml here: the RAG checks judge the file as it is now,
    // not as it was when the sections above read it.
    let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);

    report_embeddings(&config, &db_path, &canopy_dir, &mut issues);
    report_rag_size_limit(&config, &mut issues);
    report_rag_directories(&canopy_dir, &config, &mut issues);
    report_ragignore(&canopy_dir);
    report_vector_store(&config, &mut issues).await;
    report_rag_queue(&db_path, &canopy_dir);
    report_rag_store_health(&db_path, &canopy_dir, &config, &mut issues);
    print_doctor_suggestions(&issues);

    Ok(())
}

/// Doctor's data-directory stat. Capability: `exists()` is a live stat, not
/// a cached/declared value — the fact asserted ("this directory is on disk
/// right now") is exactly what's checked.
fn report_data_directory(canopy_dir: &Path, issues: &mut Vec<String>) {
    if canopy_dir.exists() {
        success(format!("Data directory: {}", canopy_dir.display()));
    } else {
        println!(
            " \x1b[31m✗\x1b[0m Data directory not found: {}",
            canopy_dir.display()
        );
        issues.push("Run 'canopy setup' to initialize".to_string());
    }
}

/// Doctor's database section: open, agent counts, `quick_check`, the daily
/// health routine's last recorded outcome, and the stale-2.x-name scan.
///
/// Capability: the tick requires the database to actually open, not just
/// for its file to exist — a corrupt/unreadable db file used to print the
/// same green line as a healthy one because the old code printed the tick
/// before attempting `Database::new`. Opening stays passive: a missing
/// database is reported, never created.
fn report_database(db_path: &PathBuf, canopy_dir: &Path, issues: &mut Vec<String>) {
    if !db_path.exists() {
        println!(" \x1b[33m⚠\x1b[0m Database not found (will be created on setup)");
        return;
    }
    match Database::new_safe(db_path, canopy_dir) {
        Ok(db) => report_open_database(db_path, &db, issues),
        Err(e) => {
            println!(" \x1b[31m✗\x1b[0m Database exists but could not be opened: {e}");
            issues.push(format!(
                "Database at {} could not be opened ({e}) — it may be corrupt.",
                db_path.display()
            ));
        }
    }
}

/// Everything printed from an already-opened database handle, in order:
/// agent counts, `quick_check`, the daily health verdict, stale 2.x names.
fn report_open_database(db_path: &Path, db: &Database, issues: &mut Vec<String>) {
    success(format!("Database: {}", db_path.display()));
    if let Ok(agents) = db.list_agents() {
        let cron_count = agents.iter().filter(|a| a.is_cron()).count();
        let watch_count = agents.iter().filter(|a| a.is_watch()).count();
        println!(
            " Agents: {} (cron: {}, watch: {})",
            agents.len(),
            cron_count,
            watch_count
        );
    }

    // capability: `quick_check` is run here directly (same cheap
    // PRAGMA `canopy clean` gates its reclaim on), distinct from
    // the daemon's own daily `integrity_check` reported just
    // below — doctor needs an answer in an interactive
    // round-trip, so it can't afford the full scan.
    match db.quick_check() {
        Ok(verdict) if verdict == "ok" => {
            success("Database quick_check: ok".to_string());
        }
        Ok(verdict) => {
            println!(" \x1b[31m✗\x1b[0m Database quick_check reported a problem: {verdict}");
            issues.push(format!(
                "Database quick_check found a problem: {verdict}. Back up what you can and investigate."
            ));
        }
        Err(e) => {
            println!(" \x1b[33m⚠\x1b[0m Could not run quick_check: {e}");
        }
    }

    // declaration: this reports the daemon's daily health
    // routine's *last recorded* outcome, not a check run here —
    // "never run" must read distinctly from "ran and passed",
    // which is exactly what `DbHealthStatus::last_run_at` being
    // `None` vs `Some` distinguishes.
    let health_status = crate::daemon::health_routine::load_status(db);
    report_health_routine(&health_status, issues);

    report_stale_names(db, issues);
}

/// The "Daily health routine" line and its follow-ups. Reports the daemon's
/// last *recorded* outcome (never a check run here). CB93: a routine that has
/// never completed but keeps being deferred (busy daemon) reads distinctly
/// from a fresh install's "never run", and a completed run shows its backup's
/// age and any deferrals since.
fn report_health_routine(health_status: &DbHealthStatus, issues: &mut Vec<String>) {
    let now = Utc::now();
    match health_status.last_run_at {
        None => {
            if health_status.deferred_count > 0 {
                let first = format_ts(health_status.first_deferred_at);
                let last = format_ts(health_status.last_skip_at);
                let busy = health_status
                    .last_skip_reason
                    .as_deref()
                    .unwrap_or("unknown");
                println!(
                    " \x1b[33m⚠\x1b[0m Daily health routine: never completed — deferred {} times \
                     since {}, last {} (busy: {}); it runs anyway after 72h of deferral",
                    health_status.deferred_count, first, last, busy
                );
                issues.push(format!(
                    "Daily database health routine has never completed — deferred {} times since \
                     {} (busy: {}); run 'canopy daemon health-check' to run it now.",
                    health_status.deferred_count, first, busy
                ));
            } else {
                println!(" \x1b[33m⚠\x1b[0m Daily health routine: never run");
            }
        }
        Some(when) => {
            report_completed_health_routine(health_status, when, now, issues);
        }
    }
}

/// The rendering for a routine that has completed at least once: the outcome
/// line, the backup path and its age (when a backup exists on disk), and the
/// deferrals counted since that run.
fn report_completed_health_routine(
    health_status: &DbHealthStatus,
    when: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
    issues: &mut Vec<String>,
) {
    match health_status.outcome {
        Some(crate::domain::db_health::DbHealthOutcome::Passed) => {
            success(format!(
                "Daily health routine: passed (last run {})",
                when.to_rfc3339()
            ));
        }
        Some(outcome) => {
            println!(
                " \x1b[31m✗\x1b[0m Daily health routine: {outcome:?} (last run {})",
                when.to_rfc3339()
            );
            if let Some(result) = &health_status.integrity_result {
                println!("     integrity_check: {result}");
            }
            issues.push(format!(
                "The daily database health routine last reported {outcome:?} at {} — run 'canopy daemon health-check' for details.",
                when.to_rfc3339()
            ));
        }
        None => {
            println!(
                " \x1b[33m⚠\x1b[0m Daily health routine: last run {} (outcome unknown)",
                when.to_rfc3339()
            );
        }
    }
    if let Some(age) = backup_age(health_status, now) {
        println!("     backup: {age}");
    }
    if health_status.deferred_count > 0 {
        println!(
            "     deferred {} times since the last run",
            health_status.deferred_count
        );
    }
}

/// "path (age: 2h)" for the routine's recorded backup, when that file still
/// exists on disk and its mtime is readable. `None` otherwise — a stale path
/// must not be printed as if a backup were there.
fn backup_age(health_status: &DbHealthStatus, now: chrono::DateTime<Utc>) -> Option<String> {
    let path = std::path::Path::new(health_status.backup_path.as_deref()?);
    let mtime = path.metadata().ok()?.modified().ok()?;
    let age = now
        .signed_duration_since(chrono::DateTime::<Utc>::from(mtime))
        .to_std()
        .unwrap_or_default();
    let age = crate::daemon::handler_formatting::format_duration_short(age);
    Some(format!("{} (age: {})", path.display(), age))
}

fn format_ts(ts: Option<chrono::DateTime<Utc>>) -> String {
    ts.map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The stale-2.x-name section: one line per affected graph (not one per node),
/// one line per affected scheduled agent, then the remediation suggestion.
fn report_stale_names(db: &Database, issues: &mut Vec<String>) {
    let report = match find_stale_2x_name_hits(db) {
        Ok(r) => r,
        Err(e) => {
            println!(" \x1b[33m⚠\x1b[0m Could not scan for stale 2.x names: {e}");
            return;
        }
    };
    let graph_refs: usize = report.graphs.iter().map(|g| g.ref_count).sum();
    let agent_refs: usize = report.agents.iter().map(|a| a.ref_count).sum();
    let total = graph_refs + agent_refs;
    if total == 0 {
        success("Stale 2.x names: none found");
        return;
    }
    if report.graphs.is_empty() {
        // Only scheduled-agent prompts are stale (every affected graph is
        // already archived): a "0 graph(s)" header would read as a bug.
        println!(
            " \x1b[31m✗\x1b[0m Stale 2.x names ({} found in {} scheduled agent(s)):",
            total,
            report.agents.len()
        );
    } else if report.agents.is_empty() {
        println!(
            " \x1b[31m✗\x1b[0m Stale 2.x names ({} found in {} graph(s)):",
            total,
            report.graphs.len()
        );
    } else {
        println!(
            " \x1b[31m✗\x1b[0m Stale 2.x names ({} found in {} graph(s), {} in scheduled agent(s)):",
            total,
            report.graphs.len(),
            report.agents.len()
        );
    }
    for g in &report.graphs {
        println!(
            "     • {} ({}, {} refs): {}",
            g.graph_name,
            g.status.as_str(),
            g.ref_count,
            format_hits(&g.names)
        );
    }
    for a in &report.agents {
        println!(
            "     • scheduled agent '{}' ({} refs): {}",
            a.agent_id,
            a.ref_count,
            format_hits(&a.names)
        );
    }
    issues.push(stale_names_suggestion(&report, total));
}

fn format_hits(names: &[StaleNameHit]) -> String {
    names
        .iter()
        .map(|n| format!("{} → {}", n.old, n.new))
        .collect::<Vec<_>>()
        .join(", ")
}

/// CB93 FR4: when every affected graph is completed/failed, archiving is the
/// clean remediation (`graph_edit archive`, reversible with `graph_edit restore`); the
/// scan already skips archived graphs, so archiving actually clears the
/// report. Any graph in another status (or any scheduled agent) keeps the
/// edit-to-3.x-names suggestion instead — archiving would not be enough.
fn stale_names_suggestion(report: &StaleNameReport, total: usize) -> String {
    let all_graphs_terminal = !report.graphs.is_empty()
        && report
            .graphs
            .iter()
            .all(|g| matches!(g.status, GraphStatus::Completed | GraphStatus::Failed));
    if all_graphs_terminal && report.agents.is_empty() {
        format!(
            "{total} stale 2.x name(s) found in completed/failed graphs (listed above) — those \
             graphs can be archived with `graph_edit archive` (reversible with `graph_edit restore`); the \
             scan already skips archived graphs."
        )
    } else {
        format!(
            "{total} stale 2.x loop_*/canopy-loop reference(s) found in stored prompts, hooks, or \
             scheduled agent prompts (listed above) — not rewritten automatically, edit them to \
             the 3.x names shown."
        )
    }
}

/// Doctor's config.toml section: presence, harness list, legacy fallback.
///
/// declaration: `is_configured()` only checks a persisted marker
/// (`configured_at.is_some()`) — the claim made here is exactly that
/// marker's presence, nothing about setup's outcome, so no further
/// verification applies. A config string existing is not a capability,
/// so this stays informational (no tick) rather than routing through
/// `success`.
fn report_configuration(canopy_dir: &Path, config: &crate::domain::canopy_config::CanopyConfig) {
    if config.is_configured() {
        println!(" \x1b[90m–\x1b[0m Config: config.toml");
        if config.clis.is_empty() {
            println!(" Harnesses: (none configured)");
        } else {
            println!(" Harnesses: {}", config.cli_names().join(", "));
        }
        report_settings_problems(canopy_dir);
        return;
    }

    let cli_config_path = canopy_dir.join("cli_config.json");
    let configured_marker = canopy_dir.join(".configured");
    if cli_config_path.exists() || configured_marker.exists() {
        println!(
            " \x1b[33m⚠\x1b[0m Legacy config files found (run setup to migrate to config.toml)"
        );
    } else {
        println!(" \x1b[33m⚠\x1b[0m Config not found (run setup)");
    }
    report_settings_problems(canopy_dir);
}

/// CM51: surface every setting the file gets wrong — an invalid value (still
/// running on its previous effective value) and a key nothing declares.
fn report_settings_problems(canopy_dir: &Path) {
    for row in crate::domain::settings::inspect::list(canopy_dir, None) {
        match row.status {
            crate::domain::settings::inspect::Status::Invalid(reason) => {
                println!(" \x1b[33m⚠\x1b[0m Config: {} invalid: {reason}", row.key);
            }
            crate::domain::settings::inspect::Status::Unknown => {
                println!(
                    " \x1b[33m⚠\x1b[0m Config: unknown key {} (kept, not used)",
                    row.key
                );
            }
            _ => {}
        }
    }
}

/// The daemon facts doctor reports on, gathered in one place so the status
/// and service-unit sections judge the same state.
struct DoctorDaemonFacts {
    /// The PID the PID file names, before any liveness check.
    raw_pid: Option<u32>,
    /// The PID file's PID only if that process is actually alive.
    state_pid: Option<u32>,
    /// The process holding doctor's resolved port, if any.
    port_pid: Option<u32>,
    /// The service manager (if any) that claims to own the unit.
    manager: Option<crate::daemon::process::ServiceManagerFacts>,
}

/// Gather the daemon facts in the order they've always been gathered: PID
/// file → liveness → port (reading the persisted port state, falling back
/// to `resolve_port`) → port holder → service manager.
///
/// Same fact-check `canopy daemon status` runs (kept in one place per the
/// service-unit spec): a PID file naming a live process isn't enough to
/// call the daemon healthy if that process isn't actually the one holding
/// the port, or isn't the one a service manager owns.
fn resolve_daemon_facts(canopy_dir: &Path, db_path: &PathBuf) -> DoctorDaemonFacts {
    let raw_pid = read_pid(canopy_dir);
    let state_pid = raw_pid.filter(|&p| is_process_running(p));
    let port: u16 = db_path
        .exists()
        .then(|| Database::new_safe(db_path, canopy_dir).ok())
        .flatten()
        .and_then(|db| db.get_state("port").ok().flatten())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| crate::resolve_port(None));
    let port_pid = resolve_port_pid(port);
    let manager = service_manager_facts();

    DoctorDaemonFacts {
        raw_pid,
        state_pid,
        port_pid,
        manager,
    }
}

/// Doctor's daemon-status section: stopped, running, or inconsistent.
fn report_daemon_status(facts: &DoctorDaemonFacts, issues: &mut Vec<String>) {
    match diagnose_daemon(facts.state_pid, facts.port_pid, facts.manager.as_ref()) {
        DaemonState::Stopped => {
            if let Some(stale) = facts.raw_pid {
                println!(
                    " \x1b[31m✗\x1b[0m Daemon not running (stale PID: {})",
                    stale
                );
                issues.push("Stale PID file — run 'canopy daemon start'".to_string());
            } else {
                println!(" \x1b[33m⚠\x1b[0m Daemon not running");
            }
        }
        // capability: `diagnose_daemon` cross-checks the PID file against
        // who actually holds the port and what the service manager thinks —
        // a live PID alone is not enough to reach this branch.
        DaemonState::Running { pid } => {
            success(format!("Daemon running (PID: {pid})"));
        }
        DaemonState::Discrepancy(d) => {
            println!(" \x1b[31m✗\x1b[0m Daemon status is inconsistent:");
            for line in d.describe() {
                println!("     {line}");
            }
            if d.is_orphan {
                issues.push(format!(
                    "An orphaned canopy process (PID {}) holds the port but isn't managed by {} — run 'canopy daemon stop' to clear it.",
                    d.port_pid.expect("is_orphan implies port_pid is Some"),
                    d.manager_name.unwrap_or("the service manager")
                ));
            } else {
                issues.push(
                    "Daemon status is inconsistent — see 'canopy daemon status' for details"
                        .to_string(),
                );
            }
        }
    }
}

/// Probe what the service unit knows (effective `systemctl show`
/// environment, the orphan's own `/proc/<pid>/environ`) and hand the result
/// to the existing service-unit reporter.
fn report_service_unit_section(home: &Path, facts: &DoctorDaemonFacts, issues: &mut Vec<String>) {
    let manager_pid = facts.manager.as_ref().and_then(|m| m.pid);
    let show_out = if facts.manager.is_some_and(|m| m.name == "systemd") {
        read_systemctl_show_environment()
    } else {
        None
    };
    let oom_policy_out = if facts.manager.is_some_and(|m| m.name == "systemd") {
        read_systemctl_show_oom_policy()
    } else {
        None
    };
    let is_orphan =
        facts.port_pid.is_some() && manager_pid != facts.port_pid && facts.manager.is_some();
    let orphan_env: Option<Vec<u8>> = if is_orphan {
        facts.port_pid.and_then(read_proc_environ)
    } else {
        None
    };
    report_service_unit(
        home,
        facts.port_pid,
        manager_pid,
        show_out.as_deref(),
        orphan_env.as_deref(),
        oom_policy_out.as_deref(),
        issues,
    );
}

/// Doctor's setup-completion line.
///
/// declaration: same marker as the "Config: config.toml" check above —
/// kept as a separate line for readability, not a separate fact. Same
/// reasoning applies: informational, not a tick.
fn report_setup_completion(
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    if config.is_configured() {
        println!(" \x1b[90m–\x1b[0m Setup completed");
    } else {
        println!(" \x1b[33m⚠\x1b[0m Setup not completed");
        issues.push("Run 'canopy setup'".to_string());
    }
}

/// Doctor's footer: the accumulated suggestions, or the all-clear line,
/// always followed by the closing blank line.
fn print_doctor_suggestions(issues: &[String]) {
    if !issues.is_empty() {
        println!("\n \x1b[1;33m⚠ Suggestions:\x1b[0m");
        for issue in issues {
            println!(" • {}", issue);
        }
    } else {
        println!("\n \x1b[32m✅ All checks passed!\x1b[0m");
    }
    println!();
}

#[cfg(test)]
mod tests;
