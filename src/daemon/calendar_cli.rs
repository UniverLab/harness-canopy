//! CLI handlers for `canopy calendar` subcommands.
//!
//! Every subcommand delegates to the `calendar` MCP tool via
//! `daemon::cli_daemon::call_tool`, so CLI and MCP share one code path.

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;

#[derive(Subcommand, Debug)]
pub(crate) enum CalendarAction {
    /// List personal calendar events.
    List {
        /// Optional lower bound (RFC 3339).
        #[arg(long)]
        from: Option<String>,
        /// Optional upper bound (RFC 3339).
        #[arg(long)]
        to: Option<String>,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add a personal calendar event.
    Add {
        /// Event title.
        title: String,
        /// Event timestamp (RFC 3339).
        #[arg(long)]
        at: String,
        /// Optional notes.
        #[arg(long)]
        notes: Option<String>,
        /// Recurrence cadence: none, daily, weekly, or monthly.
        #[arg(long)]
        recurrence: Option<String>,
        /// Lead-time alert in minutes before `at`.
        #[arg(long)]
        lead: Option<i64>,
    },
    /// Remove a personal calendar event by id.
    Remove {
        /// Event id.
        id: String,
    },
    /// List per-project roadmap milestones.
    Milestones {
        /// Optional project path filter.
        #[arg(long)]
        project: Option<String>,
        /// Optional status filter: planned, active, or done.
        #[arg(long)]
        status: Option<String>,
        /// Output raw JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage per-project roadmap milestones.
    Milestone {
        #[command(subcommand)]
        action: CalendarMilestoneAction,
    },
    /// Export calendar events (and optionally milestones) as RFC 5545 `.ics`.
    Export {
        /// Write `.ics` output to this file path (defaults to stdout).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Include milestones that have `due_at` set.
        #[arg(long)]
        include_milestones: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum CalendarMilestoneAction {
    /// Add a project milestone.
    Add {
        /// Milestone title.
        title: String,
        /// Project directory path.
        #[arg(long)]
        project: String,
        /// Optional due timestamp (RFC 3339).
        #[arg(long)]
        due: Option<String>,
        /// Optional notes.
        #[arg(long)]
        notes: Option<String>,
        /// Initial status: planned, active, or done.
        #[arg(long)]
        status: Option<String>,
    },
    /// Update a milestone's status and optional due date.
    Set {
        /// Milestone id.
        id: String,
        /// Target status: planned, active, or done.
        #[arg(long)]
        status: String,
        /// Optional due timestamp (RFC 3339).
        #[arg(long)]
        due: Option<String>,
    },
    /// Remove a project milestone by id.
    Remove {
        /// Milestone id.
        id: String,
    },
}

pub(crate) async fn handle_calendar_action(
    action: CalendarAction,
    port_override: Option<u16>,
) -> Result<()> {
    match action {
        CalendarAction::List { from, to, json } => {
            handle_list_events(port_override, from, to, json)
        }
        CalendarAction::Add {
            title,
            at,
            notes,
            recurrence,
            lead,
        } => {
            let mut args = serde_json::json!({
                "action": "create_event",
                "title": title,
                "at": at,
            });
            insert_opt_str(&mut args, "notes", notes);
            insert_opt_str(&mut args, "recurrence", recurrence);
            if let Some(minutes) = lead {
                args["lead_minutes"] = serde_json::json!(minutes);
            }
            print_tool(port_override, &args)
        }
        CalendarAction::Remove { id } => print_tool(
            port_override,
            &serde_json::json!({ "action": "delete_event", "id": id }),
        ),
        CalendarAction::Milestones {
            project,
            status,
            json,
        } => handle_list_milestones(port_override, project, status, json),
        CalendarAction::Milestone { action } => handle_milestone_action(action, port_override),
        CalendarAction::Export {
            out,
            include_milestones,
        } => handle_export_ics(port_override, out, include_milestones),
    }
}

fn handle_milestone_action(
    action: CalendarMilestoneAction,
    port_override: Option<u16>,
) -> Result<()> {
    match action {
        CalendarMilestoneAction::Add {
            title,
            project,
            due,
            notes,
            status,
        } => {
            let mut args = serde_json::json!({
                "action": "create_milestone",
                "title": title,
                "project_path": project,
            });
            insert_opt_str(&mut args, "due_at", due);
            insert_opt_str(&mut args, "notes", notes);
            insert_opt_str(&mut args, "status", status);
            print_tool(port_override, &args)
        }
        CalendarMilestoneAction::Set { id, status, due } => {
            let mut args = serde_json::json!({
                "action": "update_milestone",
                "id": id,
                "status": status,
            });
            insert_opt_str(&mut args, "due_at", due);
            print_tool(port_override, &args)
        }
        CalendarMilestoneAction::Remove { id } => print_tool(
            port_override,
            &serde_json::json!({ "action": "delete_milestone", "id": id }),
        ),
    }
}

fn handle_list_events(
    port_override: Option<u16>,
    from: Option<String>,
    to: Option<String>,
    json: bool,
) -> Result<()> {
    let mut args = serde_json::json!({ "action": "list_events" });
    insert_opt_str(&mut args, "from", from);
    insert_opt_str(&mut args, "to", to);
    let text = call_tool(port_override, "calendar", &args)?;
    if json {
        println!("{text}");
        return Ok(());
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(events) = parsed.get("events").and_then(|v| v.as_array()) {
            if events.is_empty() {
                println!("No calendar events.");
                return Ok(());
            }
            for ev in events {
                let id = ev["id"].as_str().unwrap_or("");
                let at = ev["at"].as_str().unwrap_or("");
                let title = ev["title"].as_str().unwrap_or("");
                let rec = ev["recurrence"].as_str().unwrap_or("none");
                println!("{at}  [{rec}]  {title}  ({id})");
            }
            return Ok(());
        }
    }
    println!("{text}");
    Ok(())
}

fn handle_list_milestones(
    port_override: Option<u16>,
    project: Option<String>,
    status: Option<String>,
    json: bool,
) -> Result<()> {
    let mut args = serde_json::json!({ "action": "list_milestones" });
    insert_opt_str(&mut args, "project_path", project);
    insert_opt_str(&mut args, "status", status);
    let text = call_tool(port_override, "calendar", &args)?;
    if json {
        println!("{text}");
        return Ok(());
    }
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(milestones) = parsed.get("milestones").and_then(|v| v.as_array()) {
            if milestones.is_empty() {
                println!("No calendar milestones.");
                return Ok(());
            }
            for ms in milestones {
                let id = ms["id"].as_str().unwrap_or("");
                let status = ms["status"].as_str().unwrap_or("");
                let title = ms["title"].as_str().unwrap_or("");
                let due = ms["due_at"].as_str().unwrap_or("unscheduled");
                let project_path = ms["project_path"].as_str().unwrap_or("");
                println!("[{status}]  {due}  {title}  ({project_path})  ({id})");
            }
            return Ok(());
        }
    }
    println!("{text}");
    Ok(())
}

fn handle_export_ics(
    port_override: Option<u16>,
    out: Option<std::path::PathBuf>,
    include_milestones: bool,
) -> Result<()> {
    let args = serde_json::json!({
        "action": "export_ics",
        "include_milestones": include_milestones,
    });
    let text = call_tool(port_override, "calendar", &args)?;
    let parsed: serde_json::Value = serde_json::from_str(&text)?;
    let ics = parsed
        .get("ics")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Unexpected export_ics response: {text}"))?;
    if let Some(path) = out {
        std::fs::write(&path, ics)?;
        println!("Exported calendar to {}", path.display());
    } else {
        print!("{ics}");
    }
    Ok(())
}

fn insert_opt_str(obj: &mut serde_json::Value, key: &str, value: Option<String>) {
    if let Some(val) = value {
        obj[key] = serde_json::json!(val);
    }
}

fn print_tool(port_override: Option<u16>, args: &serde_json::Value) -> Result<()> {
    let text = call_tool(port_override, "calendar", args)?;
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::mcp_client::test_support::spawn_fake_daemon;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: CalendarAction,
    }

    #[test]
    fn cli_parses_all_calendar_subcommands() {
        let list = TestCli::try_parse_from([
            "test",
            "list",
            "--from",
            "2026-11-01T00:00:00Z",
            "--to",
            "2026-12-01T00:00:00Z",
            "--json",
        ])
        .expect("parse list");
        assert!(matches!(
            list.action,
            CalendarAction::List { json: true, .. }
        ));

        let add = TestCli::try_parse_from([
            "test",
            "add",
            "Demo",
            "--at",
            "2026-11-27T15:00:00Z",
            "--notes",
            "Bring slides",
            "--recurrence",
            "weekly",
            "--lead",
            "15",
        ])
        .expect("parse add");
        match add.action {
            CalendarAction::Add {
                title,
                at,
                notes,
                recurrence,
                lead,
            } => {
                assert_eq!(title, "Demo");
                assert_eq!(at, "2026-11-27T15:00:00Z");
                assert_eq!(notes.as_deref(), Some("Bring slides"));
                assert_eq!(recurrence.as_deref(), Some("weekly"));
                assert_eq!(lead, Some(15));
            }
            _ => panic!("expected Add"),
        }

        let rm = TestCli::try_parse_from(["test", "remove", "ev-1"]).expect("parse remove");
        assert!(matches!(rm.action, CalendarAction::Remove { id } if id == "ev-1"));

        let ms = TestCli::try_parse_from([
            "test",
            "milestones",
            "--project",
            "/tmp/p",
            "--status",
            "active",
            "--json",
        ])
        .expect("parse milestones");
        assert!(matches!(
            ms.action,
            CalendarAction::Milestones { json: true, .. }
        ));

        let ms_add = TestCli::try_parse_from([
            "test",
            "milestone",
            "add",
            "v1.0",
            "--project",
            "/tmp/p",
            "--due",
            "2026-12-15T00:00:00Z",
            "--notes",
            "Ship it",
            "--status",
            "planned",
        ])
        .expect("parse milestone add");
        assert!(matches!(
            ms_add.action,
            CalendarAction::Milestone {
                action: CalendarMilestoneAction::Add { .. }
            }
        ));

        let ms_set = TestCli::try_parse_from([
            "test",
            "milestone",
            "set",
            "ms-1",
            "--status",
            "done",
            "--due",
            "2026-12-20T00:00:00Z",
        ])
        .expect("parse milestone set");
        assert!(matches!(
            ms_set.action,
            CalendarAction::Milestone {
                action: CalendarMilestoneAction::Set { .. }
            }
        ));

        let ms_rm = TestCli::try_parse_from(["test", "milestone", "remove", "ms-1"])
            .expect("parse milestone remove");
        assert!(matches!(
            ms_rm.action,
            CalendarAction::Milestone {
                action: CalendarMilestoneAction::Remove { id }
            } if id == "ms-1"
        ));
    }

    #[tokio::test]
    async fn cli_delegates_to_calendar_mcp_tool() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{\"events\":[]}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_calendar_action(
            CalendarAction::Add {
                title: "Standup".to_string(),
                at: "2026-11-27T09:00:00Z".to_string(),
                notes: None,
                recurrence: Some("daily".to_string()),
                lead: Some(5),
            },
            Some(port),
        )
        .await
        .unwrap();

        handle_calendar_action(
            CalendarAction::Milestone {
                action: CalendarMilestoneAction::Set {
                    id: "ms-1".to_string(),
                    status: "done".to_string(),
                    due: None,
                },
            },
            Some(port),
        )
        .await
        .unwrap();

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["name"], "calendar");
        assert_eq!(calls[0]["arguments"]["action"], "create_event");
        assert_eq!(calls[0]["arguments"]["lead_minutes"], 5);
        assert_eq!(calls[1]["name"], "calendar");
        assert_eq!(calls[1]["arguments"]["action"], "update_milestone");
        assert_eq!(calls[1]["arguments"]["status"], "done");
    }
}
