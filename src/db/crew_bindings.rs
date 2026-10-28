//! CM49: every binding of every crew — which ensembles, agent nodes and
//! agent hooks name which crew. Split out of `db::crews` to keep that file
//! within the size gate; the crews/crew_members tables themselves live in
//! `crate::db::crews`.
//!
//! One code path (`all_crew_bindings`) answers all three questions:
//! `crew_bindings` (every binding of one crew, for `crew get`/`crew delete`)
//! and `crew_binding_counts` (per-kind counts per crew, for `crew list`).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::db::Database;

/// What kind of thing a crew is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewBindingKind {
    Ensemble,
    Node,
    Hook,
}

impl CrewBindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CrewBindingKind::Ensemble => "ensemble",
            CrewBindingKind::Node => "node",
            CrewBindingKind::Hook => "hook",
        }
    }
}

/// One binding of a crew: the graph it lives in, what kind of binding, and the
/// binding's own name (ensemble name, node name, or hook event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrewBinding {
    pub graph: String,
    pub kind: CrewBindingKind,
    pub name: String,
}

/// Binding counts per crew id: (ensembles, nodes, hooks).
#[derive(Debug, Clone, Copy, Default)]
pub struct CrewBindingCounts {
    pub ensembles: usize,
    pub nodes: usize,
    pub hooks: usize,
}

/// Append every `crew`-bound hook in one graph's `hooks` JSON blob.
fn append_hook_bindings(out: &mut Vec<(String, CrewBinding)>, graph_name: &str, hooks_raw: &str) {
    let hooks: serde_json::Value =
        serde_json::from_str(hooks_raw).unwrap_or(serde_json::Value::Null);
    let Some(events) = hooks.as_object() else {
        return;
    };
    let bindings = events.iter().flat_map(|(event, list)| {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(move |hook| {
                let crew_name = hook.get("crew").and_then(|v| v.as_str())?;
                Some((
                    crew_name.to_string(),
                    CrewBinding {
                        graph: graph_name.to_string(),
                        kind: CrewBindingKind::Hook,
                        name: event.clone(),
                    },
                ))
            })
    });
    out.extend(bindings);
}

impl Database {
    /// Every binding of the named crew.
    pub fn crew_bindings(&self, name: &str) -> Result<Vec<CrewBinding>> {
        let all = self.all_crew_bindings()?;
        Ok(all
            .into_iter()
            .filter(|(crew_name, _)| crew_name == name.trim())
            .map(|(_, binding)| binding)
            .collect())
    }

    /// Binding counts keyed by crew id (ensembles, nodes, hooks).
    pub fn crew_binding_counts(&self) -> Result<HashMap<String, CrewBindingCounts>> {
        let name_to_id: HashMap<String, String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare("SELECT id, name FROM crews")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(0)?))
            })?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
        };
        let mut counts: HashMap<String, CrewBindingCounts> = HashMap::new();
        for (crew_name, binding) in self.all_crew_bindings()? {
            let Some(id) = name_to_id.get(&crew_name) else {
                continue;
            };
            let entry = counts.entry(id.clone()).or_default();
            match binding.kind {
                CrewBindingKind::Ensemble => entry.ensembles += 1,
                CrewBindingKind::Node => entry.nodes += 1,
                CrewBindingKind::Hook => entry.hooks += 1,
            }
        }
        Ok(counts)
    }

    /// Every binding of every crew, as `(crew_name, binding)`. One code path
    /// shared by `crew_bindings` and `crew_binding_counts`.
    fn all_crew_bindings(&self) -> Result<Vec<(String, CrewBinding)>> {
        let mut out = self.ensemble_crew_bindings()?;
        out.extend(self.node_crew_bindings()?);
        out.extend(self.hook_crew_bindings()?);
        Ok(out)
    }

    /// Every ensemble-bound crew.
    fn ensemble_crew_bindings(&self) -> Result<Vec<(String, CrewBinding)>> {
        let ensembles: Vec<(Option<String>, Option<String>, String, String)> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT e.graph_id, e.spec_id, e.name, COALESCE(c.name, e.crew_name)
                 FROM ensembles e LEFT JOIN crews c ON c.id = e.crew_id
                 WHERE e.crew_id IS NOT NULL OR e.crew_name IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut out = Vec::new();
        for (graph_id, spec_id, ensemble_name, crew_name) in ensembles {
            out.push((
                crew_name,
                CrewBinding {
                    graph: self.graph_name_for_scope(graph_id.as_deref(), spec_id.as_deref())?,
                    kind: CrewBindingKind::Ensemble,
                    name: ensemble_name,
                },
            ));
        }
        Ok(out)
    }

    /// Every node-bound crew.
    fn node_crew_bindings(&self) -> Result<Vec<(String, CrewBinding)>> {
        let nodes: Vec<(String, Option<String>, String, String)> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt =
                conn.prepare("SELECT id, graph_id, spec_id, name, config FROM graph_nodes")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            rows.filter_map(|row| row.ok())
                .filter_map(|(id, graph_id, name, config)| {
                    let value: serde_json::Value = serde_json::from_str(&config).ok()?;
                    let crew = value.get("crew").and_then(|v| v.as_str())?.to_string();
                    Some((id, graph_id, name, crew))
                })
                .collect()
        };
        let mut out = Vec::new();
        for (_id, graph_id, node_name, crew_name) in nodes {
            out.push((
                crew_name,
                CrewBinding {
                    graph: self.graph_name_for_scope(graph_id.as_deref(), None)?,
                    kind: CrewBindingKind::Node,
                    name: node_name,
                },
            ));
        }
        Ok(out)
    }

    /// Every hook-bound crew.
    fn hook_crew_bindings(&self) -> Result<Vec<(String, CrewBinding)>> {
        let hook_rows: Vec<(String, String, String)> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt =
                conn.prepare("SELECT id, name, hooks FROM graphs WHERE hooks IS NOT NULL")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.filter_map(|row| row.ok()).collect()
        };
        let mut out = Vec::new();
        for (_id, graph_name, hooks_raw) in hook_rows {
            append_hook_bindings(&mut out, &graph_name, &hooks_raw);
        }
        Ok(out)
    }

    fn graph_name_for_scope(
        &self,
        graph_id: Option<&str>,
        spec_id: Option<&str>,
    ) -> Result<String> {
        if let Some(graph_id) = graph_id {
            if let Some(graph) = self.get_graph(graph_id)? {
                return Ok(graph.name);
            }
            return Ok(graph_id.to_string());
        }
        let Some(spec_id) = spec_id else {
            return Ok("(unknown)".to_string());
        };
        if let Some(spec) = self.get_graph_spec(spec_id)? {
            if let Some(graph_id) = spec.graph_id.as_deref() {
                if let Some(graph) = self.get_graph(graph_id)? {
                    return Ok(graph.name);
                }
                return Ok(graph_id.to_string());
            }
        }
        Ok("(unknown)".to_string())
    }
}
