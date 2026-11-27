//! Domain layer — core business entities and validation rules.
//!
//! This is the innermost layer of the architecture. It has no dependencies
//! on infrastructure, frameworks, or external crates beyond basic utilities.

pub mod activity;
pub mod animations_mode;
pub mod calendar;
pub mod canopy_config;
pub mod clean;
pub mod cli_config;
pub mod cli_strategy;
pub mod config_cache;
pub mod config_pin;
pub mod crews;
pub mod daemon_config;
pub mod db_health;
pub mod db_paths;
pub mod gamification;
pub mod graph_transfer;
pub mod graphs;
pub mod ics;
pub mod instruction_file;
pub mod models;
pub mod models_db;
pub mod nodes;
pub mod notification;
pub mod project;
pub mod prompt_catalog;
pub mod prompt_draft;
pub mod prompts;
pub mod queues;
pub mod quick_prompts;
pub mod quota_reset;
pub mod registry_baseline;
pub mod remote_config;
pub mod roster;
pub mod sandbox;
pub mod settings;
pub mod spec_review;
pub mod spec_type;
pub mod specs;

#[cfg(test)]
mod spec_type_tests;
pub mod subagent_mcp;
pub mod sync;
pub mod usage_stats;
pub mod validation;
pub mod validation_mutex;
pub mod validation_session;

#[cfg(test)]
mod domain_tests;
#[cfg(test)]
mod prompt_draft_tests;
