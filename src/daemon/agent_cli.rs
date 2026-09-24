//! CLI handlers for `canopy agent` subcommands.
//!
//! `show` is read-only: it opens the local database directly, exactly like
//! `canopy graph info` (see `graph_cli.rs`), and never requires the daemon.

use anyhow::Result;
use clap::Subcommand;

use crate::application::ports::AgentRepository;
use crate::daemon::handler_formatting::agent_detail_json;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::domain::models::Agent;

#[derive(Subcommand, Debug)]
pub(crate) enum AgentAction {
    /// Show one agent's full stored definition, prompt last and untruncated.
    Show {
        /// Agent ID (exact, as listed by agent_list or the TUI).
        id: String,
        /// Print the definition as JSON — byte-identical to the `agent_get`
        /// MCP tool's output.
        #[arg(long)]
        json: bool,
    },
}

pub(crate) async fn handle_agent_action(action: AgentAction) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = Database::new_safe(&database_path(&data_dir), &data_dir)?;
    match action {
        AgentAction::Show { id, json } => handle_agent_show(&db, &id, json),
    }
}

fn handle_agent_show(db: &Database, id: &str, json: bool) -> Result<()> {
    let agent = db
        .get_agent(id)?
        .ok_or_else(|| anyhow::anyhow!("agent '{id}' not found"))?;
    let notify = db.agent_notify_on_success(id)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_detail_json(&agent, notify))?
        );
        return Ok(());
    }
    print_agent_human(&agent, notify);
    Ok(())
}

/// Human-readable layout: every stored field, prompt last and untruncated.
/// Optional fields print `-` when unset; datetimes print RFC 3339.
fn print_agent_human(a: &Agent, notify_on_success: bool) {
    let opt = |o: &Option<String>| o.as_deref().unwrap_or("-").to_string();
    let opt_dt = |o: &Option<chrono::DateTime<chrono::Utc>>| {
        o.map(|t| t.to_rfc3339()).unwrap_or_else(|| "-".to_string())
    };
    let opt_bool = |o: Option<bool>| o.map(|b| b.to_string()).unwrap_or_else(|| "-".to_string());
    let trigger_config = match &a.trigger {
        Some(t) => serde_json::to_string(t).unwrap_or_else(|_| "-".to_string()),
        None => "-".to_string(),
    };

    println!("id:                {}", a.id);
    println!("enabled:           {}", a.enabled);
    println!("trigger_type:      {}", a.trigger_type_label());
    println!("trigger_config:    {trigger_config}");
    println!("cli:               {}", a.cli);
    println!("model:             {}", opt(&a.model));
    println!("effort:            {}", opt(&a.effort));
    println!("working_dir:       {}", opt(&a.working_dir));
    println!("timeout_minutes:   {}", a.timeout_minutes);
    println!("expires_at:        {}", opt_dt(&a.expires_at));
    println!("enable_at:         {}", opt_dt(&a.enable_at));
    println!("notify_on_success: {notify_on_success}");
    println!("log_path:          {}", a.log_path);
    println!("created_at:        {}", a.created_at.to_rfc3339());
    println!("last_run_at:       {}", opt_dt(&a.last_run_at));
    println!("last_run_ok:       {}", opt_bool(a.last_run_ok));
    println!("last_triggered_at: {}", opt_dt(&a.last_triggered_at));
    println!("trigger_count:     {}", a.trigger_count);
    println!("prompt:");
    println!("{}", a.prompt);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: AgentAction,
    }

    // Spec guideline (c): clap parse test for `canopy agent show loop-watchdog --json`.
    #[test]
    fn agent_show_parses_id_and_json_flag() {
        let cli = TestCli::try_parse_from(["test", "show", "loop-watchdog", "--json"])
            .expect("show --json should parse");
        match cli.action {
            AgentAction::Show { id, json } => {
                assert_eq!(id, "loop-watchdog");
                assert!(json);
            }
        }

        let cli = TestCli::try_parse_from(["test", "show", "loop-watchdog"])
            .expect("show without --json should parse");
        match cli.action {
            AgentAction::Show { id, json } => {
                assert_eq!(id, "loop-watchdog");
                assert!(!json);
            }
        }

        assert!(TestCli::try_parse_from(["test", "show"]).is_err());
    }
}
