#![allow(clippy::doc_markdown)]
//! canopy — MCP server for AI agent background_agent scheduling and file watching.
//!
//! Binary modes:
//! - `daemon start` — start the MCP server as a persistent background process
//! - `daemon stop` — stop the running daemon
//! - `daemon status` — check daemon health
//! - `stdio` — run in stdio MCP transport mode (legacy/fallback)
//! - (no args) — start in foreground with Streamable HTTP transport

mod application;
mod autoupdate;
mod config;
mod daemon;
mod db;
#[cfg(test)]
#[path = "docs_first_graph_tests.rs"]
mod docs_first_graph_tests;
mod domain;
mod dynamic_skills;
mod executor;
mod graph_engine;
mod mcp_wizard_module;
mod rag;
mod remote;
mod scheduler;
mod sessions;
mod setup_module;
mod shared;
mod skills_module;
mod state_dir;
#[cfg(test)]
#[path = "state_dir_tests.rs"]
mod state_dir_tests;
mod system;
#[cfg(test)]
mod test_support;
mod tui;
mod watchers;

use anyhow::Result;
use clap::{Parser, Subcommand};
use daemon::agent_cli::{handle_agent_action, AgentAction};
use daemon::bridge::run_bridge;
use daemon::clean_cli::handle_clean_action;
use daemon::cli::{handle_daemon_action, DaemonAction};
use daemon::crew_cli::{handle_crew_action, CrewAction};
use daemon::doctor::run_doctor;
use daemon::graph_cli::{handle_graph_action, GraphAction};
use daemon::harness_metrics_cli::handle_harnesses_action;
use daemon::missions_cli::handle_missions_action;
use daemon::models_cli::{handle_models_action, ModelsAction};
use daemon::mutex_cli::{handle_mutex_action, MutexAction};
use daemon::node_cli::{handle_node_action, NodeAction};
use daemon::project_cli::{handle_project_action, ProjectAction};
use daemon::prompts_cli::{handle_prompts_action, PromptsAction};
use daemon::queue_cli::{handle_queue_action, QueueAction};
use daemon::rag_cli::{handle_rag_action, RagAction};
use daemon::roster_cli::{handle_roster_action, RosterAction};
use daemon::sandbox_cli::{handle_sandbox_action, SandboxAction};
use daemon::server::{run_http_server, run_stdio_server};
use daemon::settings_cli::{handle_settings_action, SettingsAction};
use daemon::shelf_cli::{handle_shelf_action, ShelfAction};
use daemon::spec_cli::{handle_spec_action, SpecAction};
use daemon::subagent_cli::{handle_subagent_action, SubagentAction};
use remote::cli::{handle_remote_action, RemoteAction};
use setup_module::registry_cli::{handle_registry_action, RegistryAction};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "canopy", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(long, short, global = true)]
    port: Option<u16>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start, stop, or manage the background daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Check for a newer stable release and install it (asks first; refuses cargo installs).
    Update {
        /// Only print whether an update exists; exit 0 up to date, 1 update available, 2 check failed. Changes nothing.
        #[arg(long)]
        check: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Run a health check diagnosing common issues.
    Doctor {
        /// Show the searched PATH for missing harnesses.
        #[arg(long)]
        verbose: bool,
        /// Restore the verified backup over a damaged database, then verify
        /// the restored file. Refuses while the daemon is running.
        #[arg(long)]
        restore: bool,
        /// Restore from this file instead of the daily backup.
        #[arg(long, value_name = "FILE", requires = "restore")]
        from: Option<PathBuf>,
    },
    /// Run the MCP server over stdio transport.
    Stdio,
    /// First-run setup wizard to configure agents and directories.
    Setup {
        /// Use a local registry directory instead of fetching from GitHub.
        /// Useful for development and testing registry changes before publishing.
        #[arg(long = "local-registry", value_name = "PATH")]
        local_registry: Option<PathBuf>,
        /// Also remove legacy skill copies that were edited locally when
        /// cleaning up copies earlier versions installed. Default: edited
        /// copies are kept with a notice.
        #[arg(long = "force-skills")]
        force_skills: bool,
        /// List every registry harness with its install status and dated
        /// free-tier fact instead of running the interactive wizard.
        #[arg(long = "list-harnesses")]
        list_harnesses: bool,
        /// With --list-harnesses, print JSON (keys: name, installed,
        /// binary_path, free_tier, free_tier_checked_at, free_tier_source,
        /// docs_url) instead of plain text rows.
        #[arg(long)]
        json: bool,
    },
    /// Apply the platform registry to config.toml without the TUI.
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
    /// Remove canopy and optionally delete all local data.
    Uninstall {
        /// Preview what would be removed without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Also delete ~/.canopy (database, models, config). Without this flag,
        /// only the tool footprint is removed; data is preserved.
        #[arg(long)]
        purge: bool,
        /// Skip the interactive confirmation prompt for --purge.
        #[arg(long)]
        yes: bool,
        /// Only remove canopy's own `canopy` server entry from each platform config;
        /// leave shared registry servers (`fetch`, `filesystem`) in place.
        #[arg(long = "keep-shared-servers")]
        keep_shared_servers: bool,
    },
    /// Interactive wizard to configure MCP in your AI client.
    Mcp {
        /// Use a local registry directory instead of fetching from GitHub.
        /// Useful for development and testing registry changes before publishing.
        #[arg(long = "local-registry", value_name = "PATH")]
        local_registry: Option<PathBuf>,
    },
    /// RAG indexing management.
    Rag {
        #[command(subcommand)]
        action: RagAction,
    },
    /// Inspect and control graph state (list/info are read-only;
    /// run/pause/continue/reset/autorun delegate to the daemon).
    Graph {
        #[command(subcommand)]
        action: GraphAction,
    },
    /// Install, list, and remove shareable node documents (a single node or
    /// an ensemble) and add them to graphs by name.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
    /// Read skills from the configured skills registry (list/get print to
    /// stdout) and write the embedded `canopy` skill into every harness
    /// skills directory (install).
    Skill {
        #[command(subcommand)]
        action: Option<dynamic_skills::cli::SkillAction>,
    },
    /// Inspect registered agents (read-only, served from the local database).
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Manage standalone specs.
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
    /// Declare platform/model availability for graph dispatch.
    Roster {
        #[command(subcommand)]
        action: RosterAction,
    },
    /// Manage crews — named, ordered member lists ensembles and nodes bind to.
    Crew {
        #[command(subcommand)]
        action: CrewAction,
    },
    /// Inspect named mutexes nodes hold while they run.
    Mutex {
        #[command(subcommand)]
        action: MutexAction,
    },
    /// Inspect and change settings in config.toml.
    Config {
        #[command(subcommand)]
        action: SettingsAction,
    },
    /// Manage spec queues.
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    /// Inspect or refresh the `harness models` catalog cache.
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// List gamification missions with their unlock state (read-only, served
    /// from the local database — no daemon required).
    Missions {
        /// Print the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Manage the project registry (path-derived identity).
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Remove safely-removable stale data (soft cleanup, default mode).
    Clean {
        /// Preview what would be removed without deleting or modifying anything.
        #[arg(long)]
        dry_run: bool,
        /// Override the configured retention window, in days.
        #[arg(long = "older-than", value_name = "DAYS")]
        older_than: Option<u64>,
        /// Also delete orphaned projects (workdir missing) and their
        /// dependent rows. Requires interactive confirmation, unless
        /// `--yes` is set; `--dry-run` skips the prompt and deletes
        /// nothing. Soft cleanup runs first either way.
        #[arg(long)]
        hard: bool,
        /// Skip the interactive yes/no prompt that `--hard` would
        /// otherwise require. Intended for scripting; a typo here can
        /// delete a real cascade.
        #[arg(long)]
        yes: bool,
        /// Skip reclaiming freed database space (VACUUM + WAL checkpoint)
        /// even when this run deleted enough rows to warrant it. Use for a
        /// fast run — reclamation takes an exclusive lock and rewrites the
        /// whole database file.
        #[arg(long = "no-reclaim")]
        no_reclaim: bool,
        /// Consent to stop the running daemon for the duration of a
        /// warranted reclaim (refuse-if-busy, stop, quick_check, cleanup,
        /// VACUUM + WAL checkpoint, restart), then restart it exactly as it
        /// was running. Without this, an interactive terminal is prompted;
        /// a non-interactive run skips reclamation while the daemon is up.
        #[arg(long = "stop-daemon")]
        stop_daemon: bool,
    },
    /// Discover file-backed prompt presets (~/.canopy/prompts/).
    Prompts {
        #[command(subcommand)]
        action: PromptsAction,
    },
    /// Launch and collect ephemeral subagents.
    Subagent {
        #[command(subcommand)]
        action: SubagentAction,
    },
    /// List, land, or discard canopy sandbox worktrees left by sandboxed graph runs.
    Sandbox {
        #[command(subcommand)]
        action: SandboxAction,
    },
    /// List or open session-shelf files through the daemon.
    Shelf {
        #[command(subcommand)]
        action: ShelfAction,
    },
    /// Owner-only remote control: sign in, pair browsers, manage the relay.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },
    /// Read host usage history (no daemon required).
    Metrics {
        #[command(subcommand)]
        action: MetricsAction,
    },
    /// Run a stdio sidecar proxy that injects canopy identity headers.
    Bridge {
        /// Agent session ID to bind this bridge process.
        #[arg(long = "id")]
        agent_id: Option<String>,
        /// Explicit daemon port override.
        #[arg(long)]
        port: Option<u16>,
        /// Working directory forwarded to the daemon.
        #[arg(long)]
        workdir: Option<PathBuf>,
    },
    /// Extract text content from a PDF file (internal use).
    #[command(hide = true)]
    InternalPdfExtract { path: PathBuf },
    /// Start the HTTP API server (used by the daemon).
    #[command(hide = true)]
    Serve,
}

/// Read-only views over locally stored history.
#[derive(Subcommand)]
enum MetricsAction {
    /// Host usage history: peaks, culprits and what the machine was doing.
    Host {
        /// Window start: RFC 3339 instant or duration before `--until`
        /// (`30m`, `6h`, `1d`). Defaults to `1h`.
        #[arg(long, default_value = "1h")]
        since: String,
        /// Window end: RFC 3339 instant. Defaults to now.
        #[arg(long)]
        until: Option<String>,
        /// Samples within 60 seconds of this RFC 3339 instant, each with its
        /// `top_procs`. Overrides `--since`/`--until`.
        #[arg(long)]
        at: Option<String>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// With `--json`: omit the `samples` array.
        #[arg(long = "no-samples")]
        no_samples: bool,
    },
    /// Graph run metrics: runs, pass/fail, and durations by graph, node, or model.
    Graphs {
        /// Window start: RFC 3339 instant or duration like `7d`, `30m`. Defaults to `7d`.
        #[arg(long, default_value = "7d")]
        since: String,
        /// Narrow to one graph (name or id).
        #[arg(long)]
        graph: Option<String>,
        /// Group by: graph, node, or model.
        #[arg(long, default_value = "graph")]
        by: String,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Harness launch counts with last use, plus mission totals.
    Harnesses {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    restore_default_sigpipe(&cli.command);

    match cli.command {
        Some(Commands::Daemon { action }) => handle_daemon_action(action, cli.port).await,
        Some(Commands::Update { check, yes }) => {
            let code = autoupdate::run_update(check, yes)?;
            std::process::exit(code);
        }
        Some(Commands::Doctor {
            verbose,
            restore,
            from,
        }) => handle_doctor(verbose, restore, from).await,
        Some(Commands::Stdio) => run_stdio_server().await,
        Some(Commands::Serve) => run_http_server(cli.port).await,
        Some(Commands::Setup {
            local_registry,
            force_skills,
            list_harnesses,
            json,
        }) => handle_setup_action(local_registry, force_skills, list_harnesses, json),
        Some(Commands::Mcp { local_registry }) => {
            if let Some(path) = local_registry {
                setup_module::registry_fetch::set_local_registry(path);
            }
            tokio::task::block_in_place(mcp_wizard_module::run_mcp_wizard)?;
            Ok(())
        }
        Some(Commands::Registry { action }) => {
            tokio::task::block_in_place(|| handle_registry_action(action))?;
            Ok(())
        }
        Some(Commands::Uninstall {
            dry_run,
            purge,
            yes,
            keep_shared_servers,
        }) => {
            tokio::task::block_in_place(|| {
                handle_uninstall(dry_run, purge, yes, keep_shared_servers)
            })?;
            Ok(())
        }
        Some(Commands::Rag { action }) => handle_rag_action(action).await,
        Some(Commands::Graph { action }) => handle_graph_action(action, cli.port).await,
        Some(Commands::Node { action }) => handle_node_action(action),
        Some(Commands::Skill { action }) => dynamic_skills::cli::handle_skill_action(action),
        Some(Commands::Agent { action }) => handle_agent_action(action).await,
        Some(Commands::Spec { action }) => handle_spec_action(action, cli.port).await,
        Some(Commands::Roster { action }) => handle_roster_action(action, cli.port).await,
        Some(Commands::Crew { action }) => handle_crew_action(action, cli.port).await,
        Some(Commands::Mutex { action }) => handle_mutex_action(action).await,
        Some(Commands::Config { action }) => handle_settings_action(action, cli.port).await,
        Some(Commands::Queue { action }) => handle_queue_action(action, cli.port).await,
        Some(Commands::Models { action }) => handle_models_action(action).await,
        Some(Commands::Missions { json }) => handle_missions_action(json).await,
        Some(Commands::Project { action }) => handle_project_action(action).await,
        Some(Commands::Clean {
            dry_run,
            older_than,
            hard,
            yes,
            no_reclaim,
            stop_daemon,
        }) => handle_clean_action(dry_run, older_than, hard, yes, no_reclaim, stop_daemon).await,
        Some(Commands::Prompts { action }) => handle_prompts_action(action).await,
        Some(Commands::Subagent { action }) => handle_subagent_action(action, cli.port).await,
        Some(Commands::Sandbox { action }) => handle_sandbox_action(action).await,
        Some(Commands::Shelf { action }) => handle_shelf_action(action, cli.port).await,
        Some(Commands::Remote { action }) => handle_remote_action(action).await,
        Some(Commands::Metrics { action }) => handle_metrics_action(action, cli.port).await,
        Some(Commands::Bridge {
            agent_id,
            port,
            workdir,
        }) => run_bridge(agent_id, port.or(cli.port), workdir).await,
        Some(Commands::InternalPdfExtract { path }) => {
            rag::ingestion::run_internal_pdf_extract(&path)
        }
        None => {
            tokio::task::block_in_place(|| {
                if setup_module::needs_setup() {
                    setup_module::run_setup(false)?;
                }
                setup_module::maybe_refresh_registry();
                autoupdate::maybe_spawn_update_notice();
                tui::run_tui()
            })?;
            Ok(())
        }
    }
}

/// Whether this invocation should keep SIGPIPE ignored.
///
/// Long-lived servers write to sockets and must observe the write error
/// instead of dying; short-lived CLI commands should end silently when their
/// stdout reader closes, as any Unix tool does.
#[cfg_attr(windows, allow(dead_code))]
fn keeps_sigpipe_ignored(cmd: &Option<Commands>) -> bool {
    match cmd {
        None => true,
        Some(Commands::Daemon { .. }) => true,
        Some(Commands::Serve) => true,
        Some(Commands::Bridge { .. }) => true,
        Some(Commands::Update { .. })
        | Some(Commands::Doctor { .. })
        | Some(Commands::Stdio)
        | Some(Commands::Setup { .. })
        | Some(Commands::Registry { .. })
        | Some(Commands::Uninstall { .. })
        | Some(Commands::Mcp { .. })
        | Some(Commands::Rag { .. })
        | Some(Commands::Graph { .. })
        | Some(Commands::Node { .. })
        | Some(Commands::Skill { .. })
        | Some(Commands::Agent { .. })
        | Some(Commands::Spec { .. })
        | Some(Commands::Roster { .. })
        | Some(Commands::Crew { .. })
        | Some(Commands::Mutex { .. })
        | Some(Commands::Config { .. })
        | Some(Commands::Queue { .. })
        | Some(Commands::Models { .. })
        | Some(Commands::Missions { .. })
        | Some(Commands::Project { .. })
        | Some(Commands::Clean { .. })
        | Some(Commands::Prompts { .. })
        | Some(Commands::Subagent { .. })
        | Some(Commands::Sandbox { .. })
        | Some(Commands::Shelf { .. })
        | Some(Commands::Metrics { .. })
        | Some(Commands::Remote { .. })
        | Some(Commands::InternalPdfExtract { .. }) => false,
    }
}

/// Restore the default SIGPIPE disposition for short-lived CLI commands.
fn restore_default_sigpipe(cmd: &Option<Commands>) {
    #[cfg(unix)]
    {
        if !keeps_sigpipe_ignored(cmd) {
            // SAFETY: setting the disposition to the default only. The process
            // inherits no SIGPIPE handler that needs to survive this call.
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = cmd;
    }
}

/// `canopy setup`: non-interactive harness list with `--list-harnesses`
/// (JSON with `--json`), else the interactive wizard.
fn handle_setup_action(
    local_registry: Option<PathBuf>,
    force_skills: bool,
    list_harnesses: bool,
    json: bool,
) -> Result<()> {
    if let Some(path) = local_registry {
        setup_module::registry_fetch::set_local_registry(path);
    }
    if list_harnesses {
        setup_module::run_list_harnesses(json)
    } else {
        tokio::task::block_in_place(|| setup_module::run_setup(force_skills))
    }
}

/// `canopy doctor`: health report by default, verified-backup restore with
/// `--restore` (refuses while the daemon runs, verifies the restored file).
async fn handle_doctor(verbose: bool, restore: bool, from: Option<PathBuf>) -> Result<()> {
    if restore {
        let data_dir = crate::ensure_data_dir()?;
        let port = crate::daemon::cli::configured_port(&data_dir);
        return crate::daemon::db_recovery::run_restore(&data_dir, from.as_deref(), &|| {
            crate::daemon::process::other_instance_may_be_running(&data_dir)
                || crate::daemon::process::resolve_port_pid(port).is_some()
        });
    }
    if verbose {
        crate::daemon::doctor::run_doctor_verbose().await
    } else {
        run_doctor().await
    }
}

async fn handle_metrics_action(action: MetricsAction, port: Option<u16>) -> Result<()> {
    match action {
        MetricsAction::Host {
            since,
            until,
            at,
            json,
            no_samples,
        } => {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
            crate::system::host_metrics_reader::run_host(
                &home.join(".canopy").join("metrics"),
                &crate::system::host_metrics_reader::HostOptions {
                    since,
                    until,
                    at,
                    json,
                    no_samples,
                },
            )
        }
        MetricsAction::Graphs {
            since,
            graph,
            by,
            json,
        } => {
            let mut args = serde_json::json!({
                "action": "metrics",
                "since": since,
                "by": by,
            });
            if let Some(graph) = graph {
                args["graph_id"] = serde_json::Value::String(graph);
            }
            let text = crate::daemon::cli_daemon::call_tool(port, "graph", &args)?;
            if json {
                println!("{text}");
            } else {
                print_graph_metrics_table(&text);
            }
            Ok(())
        }
        MetricsAction::Harnesses { json } => handle_harnesses_action(json).await,
    }
}

fn print_graph_metrics_table(text: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        println!("{text}");
        return;
    };
    let groups = v
        .get("groups")
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default();
    let by = v.get("by").and_then(|b| b.as_str()).unwrap_or("graph");
    let since = v
        .get("window")
        .and_then(|w| w.get("since"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let until = v
        .get("window")
        .and_then(|w| w.get("until"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    println!("Graph metrics by {by}  ({since} .. {until})");
    if groups.is_empty() {
        println!("No runs in window.");
        return;
    }
    println!(
        "{:<40} {:>5} {:>6} {:>6} {:>6} {:>7} {:>6} {:>7} {:>8} {:>8} {:>8} {:>8}",
        "KEY",
        "RUNS",
        "PASS",
        "FAIL",
        "INT",
        "WAIT",
        "ERR",
        "RUN",
        "PASS%",
        "AVG_S",
        "P95_S",
        "MAX_S"
    );
    for g in &groups {
        let key = g.get("key").and_then(|x| x.as_str()).unwrap_or("");
        let runs = g.get("runs").and_then(|x| x.as_u64()).unwrap_or(0);
        let passed = g.get("passed").and_then(|x| x.as_u64()).unwrap_or(0);
        let failed = g.get("failed").and_then(|x| x.as_u64()).unwrap_or(0);
        let interrupted = g.get("interrupted").and_then(|x| x.as_u64()).unwrap_or(0);
        let waiting = g.get("waiting").and_then(|x| x.as_u64()).unwrap_or(0);
        let error = g.get("error").and_then(|x| x.as_u64()).unwrap_or(0);
        let running = g.get("running").and_then(|x| x.as_u64()).unwrap_or(0);
        let pass_rate = g.get("pass_rate");
        let pass_pct = match pass_rate {
            Some(v) if v.is_number() => format!("{:.1}%", v.as_f64().unwrap_or(0.0) * 100.0),
            _ => "-".to_string(),
        };
        let dur = g.get("duration_s").cloned().unwrap_or_default();
        let avg = dur.get("avg").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let p95 = dur.get("p95").and_then(|x| x.as_i64()).unwrap_or(0);
        let max = dur.get("max").and_then(|x| x.as_i64()).unwrap_or(0);
        let key_display = if key.chars().count() > 40 {
            let truncated: String = key.chars().take(37).collect();
            format!("{truncated}...")
        } else {
            key.to_string()
        };
        println!(
            "{:<40} {:>5} {:>6} {:>6} {:>6} {:>7} {:>6} {:>7} {:>8} {:>8.1} {:>8} {:>8}",
            key_display,
            runs,
            passed,
            failed,
            interrupted,
            waiting,
            error,
            running,
            pass_pct,
            avg,
            p95,
            max
        );
    }
}

fn handle_uninstall(
    dry_run: bool,
    purge: bool,
    yes: bool,
    keep_shared_servers: bool,
) -> Result<()> {
    let plan = setup_module::uninstall::build_uninstall_plan(keep_shared_servers)?;

    if dry_run {
        setup_module::uninstall::print_dry_run(&plan);
        return Ok(());
    }

    if purge && !yes {
        println!("  This will DELETE ~/.canopy (database, models, config, logs).");
        println!("  Type 'yes' to confirm:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != "yes" {
            println!("  Aborted.");
            return Ok(());
        }
    }

    setup_module::uninstall::execute_uninstall(&plan, purge)?;
    println!();
    println!("  canopy has been uninstalled.");
    Ok(())
}

pub(crate) fn resolve_port(port_override: Option<u16>) -> u16 {
    port_override
        .or_else(|| {
            std::env::var("CANOPY_PORT")
                .ok()
                .and_then(|p| p.parse::<u16>().ok())
        })
        .unwrap_or(7755)
}

pub(crate) fn ensure_data_dir() -> Result<std::path::PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let data_dir = home.join(".canopy");
    state_dir::ensure_private_dir(&data_dir)?;
    state_dir::ensure_private_dir(&data_dir.join("logs"))?;
    // Migrate a pre-existing flat/JSON layout to the current one (TOML for
    // hand-inspectable files, a named `cache/` dir for program-managed
    // caches). Idempotent and cheap once migrated, so it's safe to run on
    // every call rather than gating it behind a first-run flag.
    //
    // Both migrations defer deleting a legacy path while another canopy
    // process may still be using it: this source tree gets rebuilt and
    // re-run while the previously installed binary's daemon (and TUI) are
    // still live, so "an old reader of the legacy path still exists" is the
    // default assumption here, not an edge case.
    let other_instance_may_be_running = daemon::process::other_instance_may_be_running(&data_dir);
    domain::usage_stats::migrate_legacy_json(&data_dir, other_instance_may_be_running);
    domain::models_db::migrate_legacy_caches(&data_dir, other_instance_may_be_running);
    let _ = domain::config_pin::pin_legacy_defaults(&data_dir);
    Ok(data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    fn assert_all_subcommands_have_about(cmd: &clap::Command, prefix: &str) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            let full = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix} {name}")
            };
            assert!(
                sub.get_about()
                    .is_some_and(|a| !a.to_string().trim().is_empty()),
                "Subcommand '{full}' has no doc comment (about is empty). Add a `///` doc comment."
            );
            assert_all_subcommands_have_about(sub, &full);
        }
    }

    #[test]
    fn all_subcommands_have_help_text() {
        let cmd = <Cli as CommandFactory>::command();
        assert_all_subcommands_have_about(&cmd, "");
    }

    #[test]
    fn mcp_subcommand_accepts_local_registry_flag() {
        // CB65: `canopy mcp` must accept the same --local-registry flag as setup.
        let cli = Cli::try_parse_from(["canopy", "mcp", "--local-registry", "/tmp/r"])
            .expect("`canopy mcp --local-registry /tmp/r` must parse");
        match cli.command {
            Some(Commands::Mcp { local_registry }) => {
                assert_eq!(local_registry, Some(PathBuf::from("/tmp/r")));
            }
            _ => panic!("expected Commands::Mcp with local_registry"),
        }
    }

    #[test]
    fn uninstall_accepts_keep_shared_servers_flag() {
        let cli = Cli::try_parse_from(["canopy", "uninstall", "--keep-shared-servers"])
            .expect("`canopy uninstall --keep-shared-servers` must parse");
        match cli.command {
            Some(Commands::Uninstall {
                keep_shared_servers,
                ..
            }) => assert!(keep_shared_servers),
            _ => panic!("expected Commands::Uninstall with keep_shared_servers"),
        }

        let cli = Cli::try_parse_from(["canopy", "uninstall"])
            .expect("`canopy uninstall` must parse without the shared-server flag");
        match cli.command {
            Some(Commands::Uninstall {
                keep_shared_servers,
                ..
            }) => assert!(!keep_shared_servers),
            _ => panic!("expected Commands::Uninstall with keep_shared_servers"),
        }
    }

    fn command_line(args: &[&str]) -> Option<Commands> {
        let mut argv = vec!["canopy"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
            .unwrap_or_else(|err| panic!("`canopy {}` must parse: {err}", args.join(" ")))
            .command
    }

    #[test]
    fn sigpipe_kept_ignored_for_long_lived_modes() {
        for args in [
            &[][..],
            &["daemon", "status"][..],
            &["serve"][..],
            &["bridge"][..],
        ] {
            assert!(
                keeps_sigpipe_ignored(&command_line(args)),
                "`canopy {}` must keep SIGPIPE ignored",
                args.join(" ")
            );
        }
    }

    #[test]
    fn sigpipe_restored_for_short_lived_commands() {
        for args in [&["graph", "list"][..], &["clean", "--dry-run"][..]] {
            assert!(
                !keeps_sigpipe_ignored(&command_line(args)),
                "`canopy {}` must restore the default SIGPIPE disposition",
                args.join(" ")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn sigpipe_restore_actually_changes_disposition() {
        // Guards the real side effect: if `restore_default_sigpipe` stops
        // calling `libc::signal`, this fails while the pure-function tests
        // above still pass.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        restore_default_sigpipe(&command_line(&["graph", "list"]));
        let prev = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        assert_eq!(
            prev,
            libc::SIG_DFL,
            "`graph list` must leave SIGPIPE at SIG_DFL"
        );

        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        restore_default_sigpipe(&command_line(&[]));
        let prev = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        assert_eq!(prev, libc::SIG_IGN, "bare TUI must keep SIGPIPE ignored");
    }
}
