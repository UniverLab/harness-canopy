//! Off-thread node-tail resolver: the tail pane's per-tick database reads
//! (`list_graph_runs_for_spec`, `get_graph_run_tail`, `get_graph_run`) run on
//! a worker thread that owns its own [`Database`] connection, following the
//! same worker-plus-poll pattern as the snapshot worker. The UI kicks a
//! request with the highlighted node plus the live-state inputs the resolver
//! needs (no DB handle), and applies the latest finished response without
//! blocking. Without a worker (tests, fallback) the dialog resolves inline.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::domain::graphs::{GraphNodeRun, GraphRunStatus};

use super::node_tail::NODE_TAIL_MAX_LINES;

/// Live-state inputs the resolver needs, captured from the UI thread.
#[derive(Debug)]
pub(crate) struct NodeTailRequest {
    pub spec_id: String,
    pub highlighted: String,
    pub member_cursor: usize,
    pub ensembles: Vec<NodeTailEnsemble>,
    pub node_names: HashMap<String, String>,
    pub spec_name: String,
    pub spec_code: Option<String>,
}

/// One ensemble's join node plus its member node ids.
#[derive(Debug)]
pub(crate) struct NodeTailEnsemble {
    pub join_node_id: String,
    pub member_ids: Vec<String>,
}

/// Owned resolver output, mirroring the dialog's `PollData` resolve cases.
/// (No `None`: without highlight/state/spec the UI clears inline and never
/// kicks a resolve.)
pub(crate) enum NodeTailResponse {
    NoRun {
        node_name: String,
        spec_name: String,
        spec_code: Option<String>,
    },
    Show(NodeTailShow),
}

/// Owned display data for one run, mirroring the dialog's `ShowData`.
pub(crate) struct NodeTailShow {
    pub run_id: String,
    pub executed_platform: Option<String>,
    pub executed_model: Option<String>,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
    pub status: Option<GraphRunStatus>,
    pub started_at: Option<DateTime<Utc>>,
    pub timed_out: bool,
    pub node_name: String,
    pub member_pos: Option<(usize, usize)>,
}

pub(crate) struct NodeTailWorker {
    req_tx: mpsc::Sender<NodeTailRequest>,
    resp_rx: mpsc::Receiver<NodeTailResponse>,
    in_flight: bool,
}

impl NodeTailWorker {
    pub(crate) fn spawn(db_path: &Path, data_dir: &Path) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<NodeTailRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<NodeTailResponse>();
        let db_path = db_path.to_path_buf();
        let data_dir = data_dir.to_path_buf();
        std::thread::spawn(move || {
            worker_loop(&db_path, &data_dir, req_rx, &resp_tx);
        });
        Self {
            req_tx,
            resp_rx,
            in_flight: false,
        }
    }

    /// Send a resolve request when no resolve is in flight.
    pub(crate) fn kick(&mut self, request: NodeTailRequest) {
        if self.in_flight {
            return;
        }
        if self.req_tx.send(request).is_ok() {
            self.in_flight = true;
        }
    }

    /// Non-blocking drain: the latest finished response, if any. True when
    /// the worker is gone and the UI should fall back to inline resolves.
    pub(crate) fn poll(&mut self) -> (Option<NodeTailResponse>, bool) {
        let mut latest = None;
        loop {
            match self.resp_rx.try_recv() {
                Ok(response) => {
                    latest = Some(response);
                    self.in_flight = false;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return (latest, true),
            }
        }
        (latest, false)
    }
}

fn worker_loop(
    db_path: &PathBuf,
    data_dir: &Path,
    req_rx: mpsc::Receiver<NodeTailRequest>,
    resp_tx: &mpsc::Sender<NodeTailResponse>,
) {
    let db = match Database::new_safe(db_path, data_dir) {
        Ok(db) => db,
        Err(error) => {
            tracing::warn!("Node-tail worker could not open the database: {error:#}");
            return;
        }
    };
    for request in req_rx {
        let response = resolve(&db, &request);
        if resp_tx.send(response).is_err() {
            return;
        }
    }
}

fn resolve(db: &Database, request: &NodeTailRequest) -> NodeTailResponse {
    let node_name_of = |node_id: &str| -> String {
        request
            .node_names
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| node_id.to_string())
    };
    let ensemble = request.ensembles.iter().find(|e| {
        e.join_node_id == request.highlighted
            || e.member_ids.iter().any(|m| m == &request.highlighted)
    });
    match ensemble {
        Some(ens) => {
            let mut runs: Vec<GraphNodeRun> = db
                .list_graph_runs_for_spec(&request.spec_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| ens.member_ids.iter().any(|m| m == &r.node_id))
                .collect();
            runs.sort_by(|a, b| match (a.status, b.status) {
                (GraphRunStatus::Running, GraphRunStatus::Running) => {
                    a.started_at.cmp(&b.started_at)
                }
                (GraphRunStatus::Running, _) => std::cmp::Ordering::Less,
                (_, GraphRunStatus::Running) => std::cmp::Ordering::Greater,
                _ => b.started_at.cmp(&a.started_at),
            });
            if runs.is_empty() {
                return NodeTailResponse::NoRun {
                    node_name: node_name_of(&request.highlighted),
                    spec_name: request.spec_name.clone(),
                    spec_code: request.spec_code.clone(),
                };
            }
            let cursor = request.member_cursor.min(runs.len().saturating_sub(1));
            let run = &runs[cursor];
            make_show(
                db,
                run,
                node_name_of(&run.node_id),
                Some((cursor + 1, runs.len())),
            )
        }
        None => {
            let runs = db
                .list_graph_runs_for_spec(&request.spec_id)
                .unwrap_or_default();
            match runs.iter().rfind(|r| r.node_id == request.highlighted) {
                Some(run) => make_show(db, run, node_name_of(&request.highlighted), None),
                None => NodeTailResponse::NoRun {
                    node_name: node_name_of(&request.highlighted),
                    spec_name: request.spec_name.clone(),
                    spec_code: request.spec_code.clone(),
                },
            }
        }
    }
}

fn make_show(
    db: &Database,
    run: &GraphNodeRun,
    node_name: String,
    member_pos: Option<(usize, usize)>,
) -> NodeTailResponse {
    let (stdout_lines, stderr_lines) = db
        .get_graph_run_tail(&run.id, NODE_TAIL_MAX_LINES)
        .ok()
        .map(|(out, err)| {
            (
                out.lines().map(str::to_string).collect(),
                err.lines().map(str::to_string).collect(),
            )
        })
        .unwrap_or_default();
    let latest = db.get_graph_run(&run.id).ok().flatten();
    NodeTailResponse::Show(NodeTailShow {
        run_id: run.id.clone(),
        executed_platform: run.executed_platform.clone(),
        executed_model: run.executed_model.clone(),
        stdout_lines,
        stderr_lines,
        status: latest.as_ref().map(|r| r.status),
        started_at: latest.as_ref().map(|r| r.started_at),
        timed_out: latest.as_ref().is_some_and(|r| {
            r.output
                .as_ref()
                .and_then(|v| v.get("error"))
                .and_then(|v| v.as_str())
                == Some("timed out")
        }),
        node_name,
        member_pos,
    })
}
