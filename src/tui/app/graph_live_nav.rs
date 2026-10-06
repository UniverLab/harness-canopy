//! Graph-live navigation and selection methods moved from `mod.rs`
//! (B5 decomposition).

use std::collections::{HashMap, HashSet};

use super::graph_live_state::{self, EnsembleLiveInfo, GraphLiveState};
use super::types::{App, GraphLiveFocus};
use crate::domain::graphs::GraphEdgeCondition;

impl App {
    /// member-id -> ensemble for every ensemble member.
    pub(super) fn nav_by_member(state: &GraphLiveState) -> HashMap<&str, &EnsembleLiveInfo> {
        state
            .ensembles
            .iter()
            .flat_map(|e| e.members.iter().map(move |m| (m.node_id.as_str(), e)))
            .collect()
    }

    /// join-node-id -> ensemble for every ensemble.
    pub(super) fn nav_by_join(state: &GraphLiveState) -> HashMap<&str, &EnsembleLiveInfo> {
        state
            .ensembles
            .iter()
            .map(|e| (e.join_node_id.as_str(), e))
            .collect()
    }

    /// member-id -> ensemble-id for every ensemble member.
    pub(super) fn nav_member_to_ensemble(state: &GraphLiveState) -> HashMap<String, String> {
        let mut member_to_ensemble: HashMap<String, String> = HashMap::new();
        for ens in &state.ensembles {
            for m in &ens.members {
                member_to_ensemble.insert(m.node_id.clone(), ens.ensemble_id.clone());
            }
        }
        member_to_ensemble
    }

    /// Resolve a raw node id to the collapsed key navigation uses.
    pub(super) fn nav_collapse_key(
        raw: &str,
        by_join: &HashMap<&str, &EnsembleLiveInfo>,
        member_to_ensemble: &HashMap<String, String>,
    ) -> String {
        if let Some(ens) = by_join.get(raw) {
            return ens.ensemble_id.clone();
        }
        if let Some(ek) = member_to_ensemble.get(raw) {
            return ek.clone();
        }
        raw.to_string()
    }

    /// Build the navigation adjacency: collapsed edges with member->join edges
    /// removed and duplicates dropped (unsorted — the caller orders what it uses).
    pub(super) fn nav_collapsed_edges(
        state: &GraphLiveState,
        by_join: &HashMap<&str, &EnsembleLiveInfo>,
        by_member: &HashMap<&str, &EnsembleLiveInfo>,
        member_to_ensemble: &HashMap<String, String>,
    ) -> HashMap<String, Vec<(String, GraphEdgeCondition)>> {
        let join_ids: HashSet<&str> = state
            .ensembles
            .iter()
            .map(|e| e.join_node_id.as_str())
            .collect();
        let mut collapsed_edges: HashMap<String, Vec<(String, GraphEdgeCondition)>> =
            HashMap::new();
        for edge in &state.effective_edges {
            let from_raw = edge.from_node.as_str();
            let to_raw = edge.to_node.as_str();
            if by_member
                .get(from_raw)
                .is_some_and(|e| e.join_node_id.as_str() == to_raw)
            {
                continue;
            }
            let from_key = if let Some(ens) = by_join.get(from_raw) {
                Some(ens.ensemble_id.clone())
            } else if let Some(ek) = member_to_ensemble.get(from_raw) {
                Some(ek.clone())
            } else if state.effective_nodes.iter().any(|n| n.id == from_raw)
                && !join_ids.contains(from_raw)
            {
                Some(from_raw.to_string())
            } else {
                None
            };
            let to_key = if let Some(ens) = by_join.get(to_raw) {
                Some(ens.ensemble_id.clone())
            } else if let Some(ek) = member_to_ensemble.get(to_raw) {
                Some(ek.clone())
            } else if state.effective_nodes.iter().any(|n| n.id == to_raw)
                && !join_ids.contains(to_raw)
            {
                Some(to_raw.to_string())
            } else {
                None
            };
            let (Some(fk), Some(tk)) = (from_key, to_key) else {
                continue;
            };
            let e = collapsed_edges.entry(fk).or_default();
            if !e.iter().any(|(ek, ec)| ek == &tk && ec == &edge.condition) {
                e.push((tk, edge.condition.clone()));
            }
        }
        collapsed_edges
    }

    /// First collapsed key with an edge into `current_key`.
    pub(super) fn nav_find_incoming(
        edges: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
        current_key: &str,
    ) -> Option<String> {
        for (fk, tos) in edges {
            if tos.iter().any(|(tk, _)| tk == current_key) {
                return Some(fk.clone());
            }
        }
        None
    }

    /// The first member's node id of the ensemble named `key`, if any.
    pub(super) fn nav_ensemble_first_member(state: &GraphLiveState, key: &str) -> Option<String> {
        state
            .ensembles
            .iter()
            .find(|e| e.ensemble_id == key)
            .and_then(|e| e.members.first().map(|m| m.node_id.clone()))
    }

    /// DFS traversal order of the effective graph (collapsed ensembles as one
    /// node, cycles via visited set, `pass > fail > always > route(alpha) > error`).
    /// This is the visual order the renderer draws, and what Up/Down navigate.
    pub fn dfs_order(&self) -> Vec<String> {
        let Some(state) = self.graph_live_state.as_ref() else {
            return Vec::new();
        };
        if state.effective_nodes.is_empty() {
            return Vec::new();
        }
        // Build collapsed graph identical to graph_live::graph_lines
        let collapsed = Self::collapse_nodes(state);
        if collapsed.is_empty() {
            return Vec::new();
        }
        let collapsed_pos: HashMap<String, i64> = collapsed
            .iter()
            .map(|(key, (_, position))| (key.clone(), *position))
            .collect();
        let ensemble_by_join: HashMap<&str, &EnsembleLiveInfo> = state
            .ensembles
            .iter()
            .map(|e| (e.join_node_id.as_str(), e))
            .collect();
        let member_to_ensemble = Self::member_to_ensemble(state);
        let collapsed_edges =
            Self::dfs_collapsed_edges(state, &collapsed, &ensemble_by_join, &member_to_ensemble);
        let entry_key = Self::dfs_entry_key(&collapsed, &collapsed_pos, &collapsed_edges);
        let mut visited: HashSet<String> = HashSet::new();
        let mut order: Vec<String> = Vec::new();
        Self::dfs_rec(&entry_key, &collapsed_edges, &mut visited, &mut order);
        let mut remaining: Vec<String> = collapsed
            .keys()
            .filter(|k| !visited.contains(k.as_str()))
            .cloned()
            .collect();
        remaining.sort_by_key(|k| collapsed_pos.get(k.as_str()).copied().unwrap_or(i64::MAX));
        for rk in remaining {
            Self::dfs_rec(&rk, &collapsed_edges, &mut visited, &mut order);
        }
        Self::dfs_concrete_ids(order, state)
    }

    /// Move to the next/previous node in DFS visual order (Up/Down).
    pub fn graph_live_navigate_sibling(&mut self, forward: bool) {
        let ids = self.dfs_order();
        if ids.is_empty() {
            return;
        }
        let current = self.graph_live_highlighted_node_id().map(str::to_string);
        // Map current concrete id to its collapsed representation for index lookup
        // dfs_order already returns concrete ids (ensemble first member), so direct lookup works
        let idx = current
            .as_deref()
            .and_then(|id| ids.iter().position(|n| n == id))
            .unwrap_or(0);
        let next_idx = crate::tui::selection::move_index(idx, ids.len(), forward);
        self.graph_live_selected_node = Some(ids[next_idx].clone());
        self.graph_live_follow = false;
    }

    /// Keep the old name as an alias for backward compatibility (tests, older key handlers).
    #[allow(dead_code)]
    pub fn graph_live_move_highlight(&mut self, forward: bool) {
        self.graph_live_navigate_sibling(forward);
    }

    /// Move to the first outgoing edge's target (Right): pass > fail > always > route(alpha) > error.
    pub fn graph_live_navigate_child(&mut self) {
        let Some(state) = self.graph_live_state.as_ref() else {
            return;
        };
        let Some(current_id) = self.graph_live_highlighted_node_id().map(str::to_string) else {
            let order = self.dfs_order();
            if let Some(first) = order.first() {
                self.graph_live_selected_node = Some(first.clone());
                self.graph_live_follow = false;
            } else {
                self.graph_live_follow = false;
            }
            return;
        };
        let by_member = Self::nav_by_member(state);
        let by_join = Self::nav_by_join(state);
        let member_to_ensemble = Self::nav_member_to_ensemble(state);
        let current_key = Self::nav_collapse_key(&current_id, &by_join, &member_to_ensemble);
        let mut edges = Self::nav_collapsed_edges(state, &by_join, &by_member, &member_to_ensemble);
        let Some(targets) = edges.get_mut(&current_key) else {
            return;
        };
        targets.sort_by(|a, b| {
            let (pa, la) = Self::edge_priority(&a.1);
            let (pb, lb) = Self::edge_priority(&b.1);
            pa.cmp(&pb).then_with(|| la.cmp(&lb))
        });
        let Some(target_key) = targets.first().map(|(k, _)| k.clone()) else {
            return;
        };
        // Map collapsed target back to a concrete node id.
        let target_id = Self::nav_ensemble_first_member(state, &target_key)
            .unwrap_or_else(|| target_key.clone());
        self.graph_live_selected_node = Some(target_id);
        self.graph_live_follow = false;
    }

    /// Move to the incoming edge's source (Left).
    pub fn graph_live_navigate_parent(&mut self) {
        let Some(state) = self.graph_live_state.as_ref() else {
            return;
        };
        let Some(current_id) = self.graph_live_highlighted_node_id().map(str::to_string) else {
            return;
        };
        let by_member = Self::nav_by_member(state);
        let by_join = Self::nav_by_join(state);
        let member_to_ensemble = Self::nav_member_to_ensemble(state);
        let current_key = Self::nav_collapse_key(&current_id, &by_join, &member_to_ensemble);
        let edges = Self::nav_collapsed_edges(state, &by_join, &by_member, &member_to_ensemble);
        let Some(parent_key) = Self::nav_find_incoming(&edges, &current_key) else {
            return;
        };
        let parent_id = Self::nav_ensemble_first_member(state, &parent_key)
            .unwrap_or_else(|| parent_key.clone());
        self.graph_live_selected_node = Some(parent_id);
        self.graph_live_follow = false;
    }

    /// Enter the live graph view's manual navigation (CT23): the only way to
    /// reach manual mode — functional requirement 4 forbids a separate
    /// toggle. Seeds the manual selection, in order (CT24): (a) the engine's
    /// current node while the graph is running; (b) on a finished graph, the
    /// most recent run's node of the strip-selected spec (else the last spec
    /// in the queue), when that node still exists in the effective graph;
    /// (c) otherwise the graph's entry node (first in DFS order). Entering
    /// never jumps the highlight on a running graph; on a finished graph it
    /// lands somewhere navigable instead of pointing at `None`. A no-op if
    /// already in manual mode (`Enter` always means "be in manual mode"), and
    /// a no-op on a graph with no effective nodes (nothing to navigate — stay
    /// in auto-follow rather than entering manual pointing at nothing).
    pub fn graph_live_enter(&mut self) {
        if !self.graph_live_follow {
            return;
        }
        // (a) running graph: current node.
        if let Some(node) = self.running_graph_current_node() {
            self.select_manual_graph_node(Some(node));
            return;
        }
        // (b) finished graph: most recent run of strip-selected spec, else
        // last spec in queue; (c) otherwise the graph's entry node (first in
        // DFS order, which itself falls back to the lowest-position node).
        let seed = self
            .finished_graph_seed_node()
            .or_else(|| self.dfs_order().first().cloned());
        self.select_manual_graph_node(seed);
        // No effective nodes: stay in follow (nothing to navigate). Do NOT
        // set follow=false with a None selection.
    }

    /// The engine's current node while the graph is running, if any.
    fn running_graph_current_node(&self) -> Option<String> {
        self.graph_live_state
            .as_ref()
            .and_then(|s| s.current_node_id.clone())
    }

    /// On a finished graph, the most recent run's node of the strip-selected
    /// spec (else the last spec in the queue), provided it still exists in
    /// the effective graph.
    fn finished_graph_seed_node(&self) -> Option<String> {
        let state = self.graph_live_state.as_ref()?;
        let spec_id = self
            .graph_spec_strip_selected
            .clone()
            .or_else(|| state.spec_queue.last().map(|e| e.spec_id.clone()))?;
        let node_id = self
            .db
            .list_graph_runs_for_spec(&spec_id)
            .unwrap_or_default()
            .last()
            .map(|r| r.node_id.clone())?;
        state
            .effective_nodes
            .iter()
            .any(|n| n.id == node_id)
            .then_some(node_id)
    }

    /// Enter manual navigation on `node_id`, if any: selects it and leaves
    /// auto-follow. A `None` selection touches nothing (stay in follow).
    fn select_manual_graph_node(&mut self, node_id: Option<String>) {
        if let Some(id) = node_id {
            self.graph_live_selected_node = Some(id);
            self.graph_live_follow = false;
        }
    }

    /// Return the live graph view to auto-follow, discarding any manual
    /// node-inspection selection.
    pub fn graph_live_reset_follow(&mut self) {
        self.graph_live_follow = true;
        self.graph_live_selected_node = None;
        self.graph_live_follow_anchor = None;
    }

    /// Step the live view's vertical scroll by `dir` lines (positive = down
    /// into content, negative = back toward top). Clamps to the valid range
    /// using the last-rendered total line count. No-op when no graph is
    /// selected.
    pub fn graph_live_view_scroll_step(&mut self, dir: i32) {
        let new = if dir > 0 {
            self.graph_live_view_scroll
                .saturating_add(dir.unsigned_abs() as u16)
        } else {
            self.graph_live_view_scroll
                .saturating_sub(dir.unsigned_abs() as u16)
        };
        self.graph_live_view_scroll = new.min(self.graph_live_view_max_scroll());
    }

    /// Maximum valid scroll value given the last-rendered total line count
    /// and panel height. Returns 0 when the content fits entirely.
    pub fn graph_live_view_max_scroll(&self) -> u16 {
        self.graph_live_view_total_lines
            .saturating_sub(self.last_panel_inner.1)
    }

    /// Step-and-clamp the right panel's Graph face scroll. Mirrors
    /// `graph_live_view_scroll_step`'s pattern but uses this face's own
    /// bookkeeping (`graph_face_total_lines`, `last_graph_face_rect`) —
    /// deliberately not shared with the main pane's live graph view, which
    /// can be showing something else entirely while this face is visible.
    pub fn graph_face_scroll_step(&mut self, dir: i32) {
        let new = if dir > 0 {
            self.graph_face_scroll
                .saturating_add(dir.unsigned_abs() as u16)
        } else {
            self.graph_face_scroll
                .saturating_sub(dir.unsigned_abs() as u16)
        };
        self.graph_face_scroll = new.min(self.graph_face_scroll_max());
    }

    /// Maximum valid scroll value given the last-rendered total line count
    /// and the Graph face's own rect height. Returns 0 when the content
    /// fits entirely or the face hasn't drawn this frame.
    pub fn graph_face_scroll_max(&self) -> u16 {
        let visible = self.last_graph_face_rect.map_or(0, |r| r.height);
        self.graph_face_total_lines.saturating_sub(visible)
    }

    /// Toggle plain-arrow-key ownership between the graph and the spec
    /// marker strip. The two are otherwise independent: switching focus
    /// never touches `graph_live_follow` or the strip's own selection.
    pub fn graph_live_toggle_focus(&mut self) {
        self.graph_live_focus = match self.graph_live_focus {
            GraphLiveFocus::Graph => GraphLiveFocus::SpecStrip,
            GraphLiveFocus::SpecStrip => GraphLiveFocus::Graph,
        };
    }

    /// Move the marker strip's selection to the next/previous spec in queue
    /// order, entering manual selection. Never touches `graph_live_follow` —
    /// selecting a spec by hand in the strip is independent of the graph's
    /// own follow/manual state. No-op when there's no live state or queue.
    pub fn graph_spec_strip_move_selection(&mut self, forward: bool) {
        let ids: Vec<String> = match self.graph_live_state.as_ref() {
            Some(state) if !state.spec_queue.is_empty() => {
                state.spec_queue.iter().map(|e| e.spec_id.clone()).collect()
            }
            _ => return,
        };

        let current = self.graph_spec_strip_selected.clone();
        let idx = current
            .as_deref()
            .and_then(|id| ids.iter().position(|n| n == id));
        let next_idx = match idx {
            // Nothing selected yet: land on the strip's first/last item
            // rather than skipping past it as if index 0 were already
            // selected (the graph's `graph_live_move_highlight` can assume
            // that, since auto-follow always has *some* node highlighted;
            // the strip starts with no selection at all).
            None => {
                if forward {
                    0
                } else {
                    ids.len() - 1
                }
            }
            Some(idx) => crate::tui::selection::move_index(idx, ids.len(), forward),
        };
        self.graph_spec_strip_selected = Some(ids[next_idx].clone());
        self.graph_spec_strip_scroll = crate::tui::selection::clamp_scroll(
            next_idx,
            self.graph_spec_strip_scroll,
            ids.len(),
            self.graph_spec_strip_capacity,
        );
    }

    /// Select a spec directly by id in the marker strip (mouse click path).
    /// Ignored if the id isn't in the current queue.
    pub fn graph_spec_strip_select(&mut self, spec_id: String) {
        let exists = self
            .graph_live_state
            .as_ref()
            .is_some_and(|state| state.spec_queue.iter().any(|e| e.spec_id == spec_id));
        if !exists {
            return;
        }
        self.graph_spec_strip_selected = Some(spec_id);
        self.graph_live_focus = GraphLiveFocus::SpecStrip;
    }

    /// The node id currently highlighted in the live graph view: the
    /// engine's current node while auto-following, else the manually
    /// selected node.
    pub fn graph_live_highlighted_node_id(&self) -> Option<&str> {
        if self.graph_live_follow {
            self.graph_live_state.as_ref()?.current_node_id.as_deref()
        } else {
            self.graph_live_selected_node.as_deref()
        }
    }

    /// Run info (status/started_at/iteration/output tail) for the live graph
    /// view's currently highlighted node — reuses the snapshot's own
    /// current-node fields when the highlight matches it (no query), else
    /// looks up the manually-highlighted node directly.
    pub fn graph_live_highlighted_node_run_info(&self) -> graph_live_state::NodeRunInfo {
        let Some(state) = self.graph_live_state.as_ref() else {
            return graph_live_state::NodeRunInfo::default();
        };
        let highlighted = self.graph_live_highlighted_node_id();
        if highlighted == state.current_node_id.as_deref() {
            return graph_live_state::NodeRunInfo {
                status: state.current_node_status,
                started_at: state.current_node_started_at,
                iteration: state.current_node_iteration,
                output_tail: state.current_node_output_tail.clone(),
                chosen_route: highlighted.and_then(|id| state.router_taken_routes.get(id).cloned()),
            };
        }
        let (Some(spec_id), Some(node_id)) = (state.current_spec_id.as_deref(), highlighted) else {
            return graph_live_state::NodeRunInfo::default();
        };
        self.graph_node_run_info(spec_id, node_id)
    }
}
