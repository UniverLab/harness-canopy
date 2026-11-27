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
use daemon::calendar_cli::{handle_calendar_action, CalendarAction};
use daemon::clean_cli::handle_clean_action;
use daemon::cli::{handle_daemon_action, DaemonAction};
use daemon::crew_cli::{handle_crew_action, CrewAction};
use daemon::doctor::run_doctor;
use daemon::graph_cli::{handle_graph_action, GraphAction};
use daemon::metrics_cli::{handle_metrics_action, MetricsAction};
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
        /// Install a missing harness using its vendor's official installer
        /// after showing the exact command and obtaining consent.
        #[arg(long, value_name = "PLATFORM")]
        install: Option<String>,
        /// With --install, skip the interactive [y/N] confirmation prompt
        /// (still prints the exact command and source URL before running).
        #[arg(long)]
        yes: bool,
        /// Run the vendor's login command for a harness in a local PTY.
        #[arg(long, value_name = "PLATFORM")]
        login: Option<String>,
        /// Check authentication status for every installed harness.
        #[arg(long = "check-logins")]
        check_logins: bool,
    },
    /// Apply the platform registry to config.toml without the TUI.
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
    /// Manage interactive sessions.
    Session {
        #[command(subcommand)]
        action: crate::daemon::session_cli::SessionAction,
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
    /// Manage personal calendar events and per-project roadmap milestones.
    Calendar {
        #[command(subcommand)]
        action: CalendarAction,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    restore_default_sigpipe(&cli.command);
    dispatch(cli).await
}

async fn dispatch(cli: Cli) -> Result<()> {
    let port = cli.port;
    match cli.command {
        Some(Commands::Daemon { action }) => handle_daemon_action(action, port).await,
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
        Some(Commands::Serve) => run_http_server(port).await,
        Some(Commands::Setup {
            local_registry,
            force_skills,
            list_harnesses,
            json,
            install,
            yes,
            login,
            check_logins,
        }) => handle_setup_action(SetupActionArgs {
            local_registry,
            force_skills,
            list_harnesses,
            json,
            install,
            yes,
            login,
            check_logins,
        }),
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
        other => dispatch_rest(other, port).await,
    }
}

async fn dispatch_rest(command: Option<Commands>, port: Option<u16>) -> Result<()> {
    match command {
        Some(Commands::Rag { action }) => handle_rag_action(action).await,
        Some(Commands::Graph { action }) => handle_graph_action(action, port).await,
        Some(Commands::Node { action }) => handle_node_action(action),
        Some(Commands::Session { action }) => {
            crate::daemon::session_cli::handle_session_action(action, port)
        }
        Some(Commands::Skill { action }) => dynamic_skills::cli::handle_skill_action(action),
        Some(Commands::Agent { action }) => handle_agent_action(action).await,
        Some(Commands::Spec { action }) => handle_spec_action(action, port).await,
        Some(Commands::Roster { action }) => handle_roster_action(action, port).await,
        Some(Commands::Crew { action }) => handle_crew_action(action, port).await,
        Some(Commands::Calendar { action }) => handle_calendar_action(action, port).await,
        Some(Commands::Mutex { action }) => handle_mutex_action(action).await,
        Some(Commands::Config { action }) => handle_settings_action(action, port).await,
        Some(Commands::Queue { action }) => handle_queue_action(action, port).await,
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
        Some(Commands::Subagent { action }) => handle_subagent_action(action, port).await,
        Some(Commands::Sandbox { action }) => handle_sandbox_action(action).await,
        Some(Commands::Shelf { action }) => handle_shelf_action(action, port).await,
        Some(Commands::Remote { action }) => handle_remote_action(action).await,
        Some(Commands::Metrics { action }) => handle_metrics_action(action, port).await,
        Some(Commands::Bridge {
            agent_id,
            port: bridge_port,
            workdir,
        }) => run_bridge(agent_id, bridge_port.or(port), workdir).await,
        Some(Commands::InternalPdfExtract { path }) => {
            rag::ingestion::run_internal_pdf_extract(&path)
        }
        None => {
            tokio::task::block_in_place(|| {
                if setup_module::needs_setup()
                    && setup_module::run_setup(false)? == setup_module::SetupOutcome::Done
                {
                    return Ok(());
                }
                setup_module::maybe_refresh_registry();
                autoupdate::maybe_spawn_update_notice();
                tui::run_tui()
            })?;
            Ok(())
        }
        Some(_) => unreachable!("handled in dispatch"),
    }
}

/// Whether this invocation should keep SIGPIPE ignored.
///
/// Long-lived servers write to sockets and must observe the write error
/// instead of dying; short-lived CLI commands should end silently when their
/// stdout reader closes, as any Unix tool does.
#[cfg_attr(windows, allow(dead_code))]
fn keeps_sigpipe_ignored(cmd: &Option<Commands>) -> bool {
    matches!(
        cmd,
        None | Some(Commands::Daemon { .. } | Commands::Serve | Commands::Bridge { .. })
    )
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

struct SetupActionArgs {
    local_registry: Option<PathBuf>,
    force_skills: bool,
    list_harnesses: bool,
    json: bool,
    install: Option<String>,
    yes: bool,
    login: Option<String>,
    check_logins: bool,
}

/// `canopy setup`: non-interactive harness list with `--list-harnesses`
/// (JSON with `--json`), else the interactive wizard.
fn handle_setup_action(args: SetupActionArgs) -> Result<()> {
    if let Some(path) = args.local_registry {
        setup_module::registry_fetch::set_local_registry(path);
    }
    if let Some(platform) = args.login {
        let code = tokio::task::block_in_place(|| setup_module::login::run_login_cmd(&platform));
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if args.check_logins {
        let code = tokio::task::block_in_place(setup_module::login::run_check_logins_cmd);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if let Some(platform) = args.install {
        let code = tokio::task::block_in_place(|| {
            setup_module::install::run_install_cmd(&platform, args.yes)
        });
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if args.list_harnesses {
        setup_module::run_list_harnesses(args.json)
    } else {
        tokio::task::block_in_place(|| match setup_module::run_setup(args.force_skills)? {
            setup_module::SetupOutcome::LaunchTui => tui::run_tui(),
            setup_module::SetupOutcome::Done => Ok(()),
        })
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
