//! `canopy metrics`: read-only views over locally stored history.
//!
//! Split out of the `canopy` binary root (`main.rs`): the `MetricsAction`
//! subcommand plus its handlers. Tool names, flags, and output unchanged.

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::harness_metrics_cli::handle_harnesses_action;

/// Read-only views over locally stored history.
#[derive(Subcommand)]
pub(crate) enum MetricsAction {
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

pub(crate) async fn handle_metrics_action(action: MetricsAction, port: Option<u16>) -> Result<()> {
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
