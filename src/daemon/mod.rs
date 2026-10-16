//! MCP Server handler implementing all canopy tools.
//!
//! Uses the `rmcp` SDK's `#[tool_router]` and `#[tool_handler]` macros
//! with `Parameters<T>` for proper MCP protocol compliance.

pub(crate) mod agent_cli;
pub(crate) mod announcements;
pub(crate) mod bridge;
pub(crate) mod clean_cli;
pub(crate) mod cli;
pub(crate) mod cli_daemon;
pub(crate) mod config_watch;
pub(crate) mod crew_cli;
pub(crate) mod daemon_start;
pub(crate) mod doctor;
pub(crate) mod graph_cli;
pub(crate) mod handler_agents;
pub(crate) mod handler_blueprints;
pub(crate) mod handler_copy;
pub(crate) mod handler_crew;
pub(crate) mod handler_edges;
pub(crate) mod handler_ensemble_core;
pub(crate) mod handler_ensemble_entry;
pub(crate) mod handler_ensemble_exit;
pub(crate) mod handler_ensemble_plan;
pub(crate) mod handler_ensemble_update;
pub(crate) mod handler_formatting;
pub(crate) mod handler_graph_crew;
pub(crate) mod handler_graphs;
pub(crate) mod handler_helpers;
pub(crate) mod handler_intel;
pub(crate) mod handler_meta;
pub(crate) mod handler_models_probes;
pub(crate) mod handler_nodes;
pub(crate) mod handler_notes;
pub(crate) mod handler_preflight;
pub(crate) mod handler_preflight_review;
pub(crate) mod handler_preflight_roster;
pub(crate) mod handler_queues;
pub(crate) mod handler_read;
pub(crate) mod handler_roster;
pub(crate) mod handler_run_exec;
pub(crate) mod handler_run_history;
pub(crate) mod handler_run_lifecycle;
pub(crate) mod handler_scheduled_sends;
pub(crate) mod handler_seeds_skills;
pub(crate) mod handler_send_session;
pub(crate) mod handler_settings;
pub(crate) mod handler_specs;
pub(crate) mod handler_sync;
pub(crate) mod handler_transfer;
pub(crate) mod handler_validate;
pub(crate) mod health_routine;
pub(crate) mod helpers;
pub(crate) mod missions_cli;
pub(crate) mod models_cli;
pub(crate) mod params;
pub(crate) mod params_extract;
pub(crate) mod probe;
pub(crate) mod process;
pub(crate) mod project_cli;
pub(crate) mod prompts_cli;
pub(crate) mod queue_cli;
pub(crate) mod rag_cli;

pub(crate) mod roster_cli;
pub(crate) mod sandbox_cli;
pub(crate) mod server;
pub(crate) mod service_install;
pub(crate) mod service_reconcile;
pub(crate) mod settings_cli;
pub(crate) mod spec_cli;
pub(crate) mod subagent;
pub(crate) mod subagent_cli;

pub mod handler;

#[cfg(test)]
mod test_missions;
#[cfg(test)]
mod tests;

pub use handler::TaskTriggerHandler;
