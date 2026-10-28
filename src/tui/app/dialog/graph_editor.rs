//! Graph-editor dialog methods moved from `mod.rs` (B5 decomposition).

use anyhow::Result;

use super::super::types::{App, Focus};
use crate::domain::graphs::GraphNodeKind;

impl App {
    pub fn open_graph_editor_dialog(&mut self) -> Result<()> {
        let Some(node) = self.selected_graph_node() else {
            return Ok(());
        };
        let dialog = self.build_editor_dialog_content(node);
        self.graph_editor_dialog = Some(dialog);
        self.focus = Focus::GraphEditorDialog;
        Ok(())
    }

    /// Open the highlighted node's `Edges` dialog: its outgoing
    /// `pass`/`fail`/`always` edges, retargetable/deletable in place. A
    /// router's `route` edges stay under its `RouterRoutes` dialog instead
    /// (see [`Self::build_edges_dialog`]'s filter).
    pub fn open_graph_edges_dialog(&mut self) -> Result<()> {
        let Some(node) = self.selected_graph_node() else {
            return Ok(());
        };
        let dialog = self.build_edges_dialog(node);
        self.graph_editor_dialog = Some(dialog);
        self.focus = Focus::GraphEditorDialog;
        Ok(())
    }

    fn build_edges_dialog(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> crate::tui::app::types::GraphEditorDialog {
        let edges: Vec<crate::domain::graphs::GraphEdge> = self
            .router_existing_edges(node)
            .into_iter()
            .filter(|edge| edge.condition.route_label().is_none())
            .collect();
        let targets = self.router_candidate_targets(node);
        crate::tui::app::types::GraphEditorDialog::new_edges(
            node.id.clone(),
            node.name.clone(),
            format!(" Edges · {} ", node.name),
            "↑↓ edge · ←→ retarget · Ctrl+D delete · Esc close".to_string(),
            edges,
            targets,
        )
    }

    /// Retarget the `Edges` dialog's focused edge to the next/previous
    /// candidate node — applied immediately through the same validated path
    /// as the `graph_edge` MCP tool
    /// ([`crate::daemon::handler::retarget_graph_edge`]), so a running graph
    /// or a cross-graph target is rejected the same way it would be over
    /// MCP, with the rejection shown as the dialog's error line.
    pub fn retarget_focused_graph_edge(&mut self, forward: bool) -> Result<()> {
        let Some(dialog) = self.graph_editor_dialog.as_ref() else {
            return Ok(());
        };
        let Some(edge) = dialog.focused_edge() else {
            return Ok(());
        };
        let edge_id = edge.id.clone();
        let current_target = edge.to_node.clone();
        let Some(next_target) = dialog.next_edge_target_candidate(forward) else {
            return Ok(());
        };
        if next_target == current_target {
            return Ok(());
        }
        match crate::daemon::handler::retarget_graph_edge(&self.db, &edge_id, &next_target) {
            Ok(updated) => {
                if let Some(dialog) = self.graph_editor_dialog.as_mut() {
                    dialog.parse_error = None;
                    if let Some(row) = dialog.edge_rows.get_mut(dialog.edge_row_index) {
                        *row = updated;
                    }
                }
                self.refresh_graphs()?;
            }
            Err(message) => {
                if let Some(dialog) = self.graph_editor_dialog.as_mut() {
                    dialog.parse_error = Some(message);
                }
            }
        }
        Ok(())
    }

    /// Delete the `Edges` dialog's focused edge — through the same
    /// validated path as the `graph_edge` MCP tool
    /// ([`crate::daemon::handler::delete_graph_edge_checked`]).
    pub fn delete_focused_graph_edge(&mut self) -> Result<()> {
        let Some(dialog) = self.graph_editor_dialog.as_ref() else {
            return Ok(());
        };
        let Some(edge) = dialog.focused_edge() else {
            return Ok(());
        };
        let edge_id = edge.id.clone();
        match crate::daemon::handler::delete_graph_edge_checked(&self.db, &edge_id) {
            Ok(_) => {
                if let Some(dialog) = self.graph_editor_dialog.as_mut() {
                    dialog.parse_error = None;
                    dialog.edge_rows.retain(|edge| edge.id != edge_id);
                    if dialog.edge_row_index >= dialog.edge_rows.len() {
                        dialog.edge_row_index = dialog.edge_rows.len().saturating_sub(1);
                    }
                }
                self.refresh_graphs()?;
            }
            Err(message) => {
                if let Some(dialog) = self.graph_editor_dialog.as_mut() {
                    dialog.parse_error = Some(message);
                }
            }
        }
        Ok(())
    }

    /// U10: duplicate the highlighted graph node — a fresh, unwired copy of its
    /// config into the same graph — then open the editor on the copy so its
    /// prompt/config can be tweaked (the closest thing the TUI has to a
    /// creation flow to pre-fill). Ensemble member/join nodes are
    /// engine-managed, so duplicating a whole ensemble is left to the
    /// `graph_ensemble` MCP tool and this is a no-op for those.
    pub fn duplicate_selected_graph_node(&mut self) -> Result<()> {
        let Some(node) = self.selected_graph_node() else {
            return Ok(());
        };
        let node = node.clone();

        // Ensemble-owned (member or join) nodes can't be copied as plain
        // nodes — that would break the "no nested ensembles" invariant.
        if node.kind == GraphNodeKind::Join
            || self.db.get_ensemble_by_member_node(&node.id)?.is_some()
            || self.db.get_ensemble_by_join_node(&node.id)?.is_some()
        {
            return Ok(());
        }

        let siblings = match (&node.spec_id, &node.graph_id) {
            (Some(spec_id), _) => self.db.list_graph_nodes(spec_id)?,
            (None, Some(graph_id)) => self.db.list_graph_nodes_for_graph(graph_id)?,
            (None, None) => return Ok(()),
        };
        let next_position = siblings.last().map(|n| n.position + 1).unwrap_or(1);

        let copy = crate::domain::graphs::GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: node.spec_id.clone(),
            graph_id: node.graph_id.clone(),
            name: format!("{} (copy)", node.name),
            kind: node.kind,
            config: node.config,
            position: next_position,
            created_at: chrono::Utc::now(),
        };
        self.db.insert_graph_node(&copy)?;
        self.refresh_graphs()?;

        // Pre-fill the editor with the copy's config (identical to the
        // source's) so the user can immediately adjust it.
        let dialog = self.build_editor_dialog_content(&copy);
        self.graph_editor_dialog = Some(dialog);
        self.focus = Focus::GraphEditorDialog;
        Ok(())
    }

    fn build_editor_dialog_content(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> crate::tui::app::types::GraphEditorDialog {
        match node.kind {
            GraphNodeKind::Agent => self.build_agent_prompt_dialog(node),
            GraphNodeKind::Router => self.build_router_routes_dialog(node),
            _ => self.build_node_config_dialog(node),
        }
    }

    fn build_agent_prompt_dialog(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> crate::tui::app::types::GraphEditorDialog {
        let prompt = node
            .config
            .get("prompt_template")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        crate::tui::app::types::GraphEditorDialog::new(
            node.id.clone(),
            node.name.clone(),
            format!(" Graph Prompt · {} ", node.name),
            "Ctrl+S save  ·  Enter newline  ·  Esc cancel".to_string(),
            prompt.to_string(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        )
    }

    fn build_node_config_dialog(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> crate::tui::app::types::GraphEditorDialog {
        crate::tui::app::types::GraphEditorDialog::new(
            node.id.clone(),
            node.name.clone(),
            format!(" Graph Config · {} ", node.name),
            "Ctrl+S save JSON  ·  Enter newline  ·  Esc cancel".to_string(),
            serde_json::to_string_pretty(&node.config).unwrap_or_default(),
            crate::tui::app::types::GraphEditorMode::NodeConfig,
        )
    }

    /// Every other node in `node`'s graph — candidate targets a router
    /// route can wire an edge to.
    fn router_candidate_targets(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> Vec<(String, String)> {
        let siblings = match (&node.spec_id, &node.graph_id) {
            (Some(spec_id), _) => self.db.list_graph_nodes(spec_id).unwrap_or_default(),
            (None, Some(graph_id)) => self
                .db
                .list_graph_nodes_for_graph(graph_id)
                .unwrap_or_default(),
            (None, None) => Vec::new(),
        };
        siblings
            .into_iter()
            .filter(|sibling| sibling.id != node.id)
            .map(|sibling| (sibling.id, sibling.name))
            .collect()
    }

    /// This router node's currently-persisted `route`-conditioned outgoing
    /// edges, keyed by nothing in particular — callers match by route label
    /// via [`crate::domain::graphs::GraphEdgeCondition::route_label`].
    fn router_existing_edges(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> Vec<crate::domain::graphs::GraphEdge> {
        let edges = match (&node.spec_id, &node.graph_id) {
            (Some(spec_id), _) => self.db.list_graph_edges(spec_id).unwrap_or_default(),
            (None, Some(graph_id)) => self
                .db
                .list_graph_edges_for_graph(graph_id)
                .unwrap_or_default(),
            (None, None) => Vec::new(),
        };
        edges
            .into_iter()
            .filter(|edge| edge.from_node == node.id)
            .collect()
    }

    /// Best-effort extraction of a router node's declared routes + fallback
    /// out of its raw `config` — used only to pre-fill the dialog. Shape
    /// correctness is enforced on save by
    /// [`crate::domain::graphs::validate_router_routes`], not here.
    fn parse_router_config(
        config: &serde_json::Value,
    ) -> (Vec<crate::domain::graphs::RouterRoute>, String) {
        let map = config.as_object();
        let routes = map
            .and_then(|m| m.get("routes"))
            .and_then(serde_json::Value::as_array)
            .map(|routes| {
                routes
                    .iter()
                    .filter_map(serde_json::Value::as_object)
                    .map(|obj| crate::domain::graphs::RouterRoute {
                        label: obj
                            .get("label")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        description: obj
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fallback = map
            .and_then(|m| m.get("fallback"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        (routes, fallback)
    }

    fn build_router_routes_dialog(
        &self,
        node: &crate::domain::graphs::GraphNode,
    ) -> crate::tui::app::types::GraphEditorDialog {
        let (parsed_routes, fallback) = Self::parse_router_config(&node.config);
        let existing_edges = self.router_existing_edges(node);
        let mut routes: Vec<crate::tui::app::types::RouterRouteDraft> = parsed_routes
            .into_iter()
            .map(|route| {
                let target = existing_edges
                    .iter()
                    .find(|edge| edge.condition.route_label() == Some(route.label.as_str()))
                    .map(|edge| edge.to_node.clone());
                crate::tui::app::types::RouterRouteDraft {
                    label: route.label,
                    description: route.description,
                    target_node_id: target,
                }
            })
            .collect();
        // A brand new router (empty `routes`) starts with the domain's
        // minimum so the form is immediately shaped like a valid one.
        while routes.len() < crate::domain::graphs::ROUTER_MIN_ROUTES {
            routes.push(crate::tui::app::types::RouterRouteDraft::default());
        }
        let targets = self.router_candidate_targets(node);
        crate::tui::app::types::GraphEditorDialog::new_router_routes(
            node.id.clone(),
            node.name.clone(),
            format!(" Router Routes · {} ", node.name),
            "Tab field · ↑↓ route · ←→ target · Ctrl+N add · Ctrl+D delete · Ctrl+F fallback · Ctrl+S save · Esc cancel"
                .to_string(),
            routes,
            fallback,
            targets,
        )
    }

    pub fn cancel_graph_editor_dialog(&mut self) {
        self.graph_editor_dialog = None;
        self.focus = Focus::Preview;
    }

    pub fn save_graph_editor_dialog(&mut self) -> Result<()> {
        let Some(dialog) = self.graph_editor_dialog.take() else {
            return Ok(());
        };
        let Some(node) = self.db.get_graph_node(&dialog.node_id)? else {
            self.focus = Focus::Preview;
            return Ok(());
        };

        if matches!(
            dialog.mode,
            crate::tui::app::types::GraphEditorMode::RouterRoutes
        ) {
            return self.save_router_routes_dialog(dialog, &node);
        }

        let updated_config = self.compute_updated_node_config(&dialog, &node)?;
        // Same allowlist `graph_node`/`graph_node` enforce (see
        // `daemon::handler::validate_node_config`) — a raw NodeConfig-mode
        // edit is the one TUI path that can hand-write a config the engine
        // will silently ignore (e.g. `prompt` instead of `prompt_template`),
        // so it must be rejected here too, not just from the MCP tools.
        if let Err(message) =
            crate::daemon::handler::validate_node_config(node.kind, &updated_config)
        {
            let mut d = dialog.clone();
            d.parse_error = Some(message);
            self.graph_editor_dialog = Some(d);
            self.focus = Focus::GraphEditorDialog;
            return Err(anyhow::anyhow!("Invalid node config"));
        }
        self.db.update_graph_node_details(
            &dialog.node_id,
            None,
            None,
            Some(&updated_config),
            None,
        )?;
        self.focus = Focus::Preview;
        self.refresh_graphs()?;
        Ok(())
    }

    /// Validate then persist a router's routes dialog: the declared
    /// routes/fallback shape (into `node.config`) and each route's edge
    /// wiring (as separate `route`-conditioned [`crate::domain::graphs::GraphEdge`]s).
    /// Parse the dialog's route drafts into validated `RouterRoute`s.
    /// `validate_router_routes` runs here so a rejected save reopens the
    /// dialog with the domain error before anything is written.
    fn collect_router_routes(
        dialog: &crate::tui::app::types::GraphEditorDialog,
    ) -> Result<Vec<crate::domain::graphs::RouterRoute>, String> {
        let routes: Vec<crate::domain::graphs::RouterRoute> = dialog
            .router_routes
            .iter()
            .map(|draft| crate::domain::graphs::RouterRoute {
                label: draft.label.trim().to_string(),
                description: draft.description.trim().to_string(),
            })
            .collect();
        let fallback = dialog.router_fallback.trim().to_string();
        crate::domain::graphs::validate_router_routes(&routes, &fallback).map(|()| routes)
    }

    /// Upsert the dialog's route edges and drop edges for routes the form no
    /// longer declares — the write half of a router save, after both domain
    /// checks have passed against the intended state.
    fn sync_router_edges(
        &mut self,
        node: &crate::domain::graphs::GraphNode,
        dialog: &crate::tui::app::types::GraphEditorDialog,
        routes: &[crate::domain::graphs::RouterRoute],
        existing_edges: &[crate::domain::graphs::GraphEdge],
    ) -> Result<()> {
        for draft in &dialog.router_routes {
            let label = draft.label.trim();
            let existing = existing_edges
                .iter()
                .find(|edge| edge.condition.route_label() == Some(label));
            match (&draft.target_node_id, existing) {
                (Some(target), Some(edge)) if &edge.to_node != target => {
                    crate::daemon::handler::retarget_graph_edge(&self.db, &edge.id, target)
                        .map_err(anyhow::Error::msg)?;
                }
                (Some(target), None) => {
                    self.db
                        .insert_graph_edge(&crate::domain::graphs::GraphEdge {
                            id: uuid::Uuid::new_v4().to_string(),
                            spec_id: node.spec_id.clone(),
                            graph_id: node.graph_id.clone(),
                            from_node: node.id.clone(),
                            to_node: target.clone(),
                            condition: crate::domain::graphs::GraphEdgeCondition::Route(
                                label.to_string(),
                            ),
                        })?;
                }
                _ => {}
            }
        }
        // A route dropped from the form (Ctrl+D) leaves its old edge naming
        // a route the node no longer declares — drop the edge too so
        // `validate_router_edges_declared` never has to catch it later.
        for edge in existing_edges {
            let Some(label) = edge.condition.route_label() else {
                continue;
            };
            if !routes.iter().any(|route| route.label == label) {
                crate::daemon::handler::delete_graph_edge_checked(&self.db, &edge.id)
                    .map_err(anyhow::Error::msg)?;
            }
        }
        Ok(())
    }

    /// Both domain checks run against the *intended* state before anything
    /// is written, so a rejected save never leaves a half-wired router.
    fn save_router_routes_dialog(
        &mut self,
        dialog: crate::tui::app::types::GraphEditorDialog,
        node: &crate::domain::graphs::GraphNode,
    ) -> Result<()> {
        let routes = match Self::collect_router_routes(&dialog) {
            Ok(routes) => routes,
            Err(message) => return self.reopen_router_dialog_with_error(dialog, message),
        };
        let fallback = dialog.router_fallback.trim().to_string();

        let intended_edges: Vec<crate::domain::graphs::GraphEdge> = dialog
            .router_routes
            .iter()
            .filter_map(|draft| {
                let target = draft.target_node_id.clone()?;
                Some(crate::domain::graphs::GraphEdge {
                    id: String::new(),
                    spec_id: node.spec_id.clone(),
                    graph_id: node.graph_id.clone(),
                    from_node: node.id.clone(),
                    to_node: target,
                    condition: crate::domain::graphs::GraphEdgeCondition::Route(
                        draft.label.trim().to_string(),
                    ),
                })
            })
            .collect();
        if let Err(message) = crate::domain::graphs::validate_router_route_coverage(
            &routes,
            &node.id,
            &intended_edges,
        ) {
            return self.reopen_router_dialog_with_error(dialog, message);
        }

        let config = serde_json::json!({
            "routes": routes
                .iter()
                .map(|route| serde_json::json!({
                    "label": route.label,
                    "description": route.description,
                }))
                .collect::<Vec<_>>(),
            "fallback": fallback,
        });
        self.db
            .update_graph_node_details(&dialog.node_id, None, None, Some(&config), None)?;

        let existing_edges = self.router_existing_edges(node);
        self.sync_router_edges(node, &dialog, &routes, &existing_edges)?;

        self.focus = Focus::Preview;
        self.refresh_graphs()?;
        Ok(())
    }

    fn reopen_router_dialog_with_error(
        &mut self,
        mut dialog: crate::tui::app::types::GraphEditorDialog,
        message: String,
    ) -> Result<()> {
        dialog.parse_error = Some(message);
        self.graph_editor_dialog = Some(dialog);
        self.focus = Focus::GraphEditorDialog;
        Err(anyhow::anyhow!("Invalid router routes"))
    }

    fn compute_updated_node_config(
        &mut self,
        dialog: &crate::tui::app::types::GraphEditorDialog,
        node: &crate::domain::graphs::GraphNode,
    ) -> Result<serde_json::Value> {
        match dialog.mode {
            crate::tui::app::types::GraphEditorMode::AgentPrompt => {
                Ok(Self::update_prompt_config(&node.config, &dialog.buffer))
            }
            crate::tui::app::types::GraphEditorMode::NodeConfig => {
                match serde_json::from_str::<serde_json::Value>(&dialog.buffer) {
                    Ok(v) => Ok(v),
                    Err(e) => {
                        let mut d = dialog.clone();
                        d.parse_error = Some(format!("JSON error: {e}"));
                        self.graph_editor_dialog = Some(d);
                        self.focus = Focus::GraphEditorDialog;
                        Err(anyhow::anyhow!("Invalid JSON"))
                    }
                }
            }
            // Router routes are saved through `save_router_routes_dialog`
            // before this function is ever reached — see
            // `save_graph_editor_dialog`'s mode check.
            crate::tui::app::types::GraphEditorMode::RouterRoutes => unreachable!(
                "RouterRoutes is handled by save_router_routes_dialog before this call"
            ),
            // Edges mode has no Ctrl+S save step — every retarget/delete
            // applies immediately (see `handle_edges_key`), so this is
            // never reached.
            crate::tui::app::types::GraphEditorMode::Edges => {
                unreachable!("Edges mode has no save step; mutations apply immediately")
            }
        }
    }

    pub(crate) fn update_prompt_config(
        config: &serde_json::Value,
        prompt: &str,
    ) -> serde_json::Value {
        if let Some(object) = config.as_object() {
            let mut updated = object.clone();
            updated.insert(
                "prompt_template".to_string(),
                serde_json::Value::String(prompt.to_string()),
            );
            serde_json::Value::Object(updated)
        } else {
            serde_json::json!({ "prompt_template": prompt })
        }
    }
}
