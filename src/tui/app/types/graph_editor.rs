#[derive(Clone)]
pub(crate) enum GraphEditorMode {
    AgentPrompt,
    NodeConfig,
    /// A [`crate::domain::graphs::GraphNodeKind::Router`] node's structured
    /// routes/fallback/wiring editor — replaces the raw JSON buffer used by
    /// [`GraphEditorMode::NodeConfig`] with the `router_*` fields below, since
    /// wiring an edge per route isn't expressible as node config alone.
    RouterRoutes,
    /// A node's outgoing `pass`/`fail`/`always` edges — lets an ordinary
    /// edge be retargeted or deleted, through the same validated path
    /// (`daemon::handler::retarget_graph_edge`/`delete_graph_edge_checked`)
    /// the `graph_update_edge`/`graph_delete_edge` MCP tools use, rather than
    /// only a router's route edges (see [`GraphEditorMode::RouterRoutes`]).
    Edges,
}

/// Which sub-field of the currently-focused route row
/// [`GraphEditorDialog::router_field`] points at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RouterField {
    Label,
    Description,
    Target,
}

/// One route being edited in the router routes dialog: the declared
/// label/description (persisted into the node's `config`) plus the target
/// node its `route` edge should point at (persisted as a separate
/// [`crate::domain::graphs::GraphEdge`] on save — `None` means not wired yet).
#[derive(Clone, Default)]
pub(crate) struct RouterRouteDraft {
    pub label: String,
    pub description: String,
    pub target_node_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct GraphEditorDialog {
    pub node_id: String,
    pub node_name: String,
    pub title: String,
    pub help: String,
    pub buffer: String,
    pub cursor: usize,
    pub mode: GraphEditorMode,
    pub parse_error: Option<String>,
    /// `RouterRoutes` mode only, below — unused (empty) otherwise.
    pub router_routes: Vec<RouterRouteDraft>,
    pub router_fallback: String,
    pub router_route_index: usize,
    pub router_field: RouterField,
    /// Candidate `(node_id, node_name)` targets a route can wire to: every
    /// other node in the router's graph.
    pub router_targets: Vec<(String, String)>,
    /// `Edges` mode only, below — unused (empty) otherwise. This node's
    /// outgoing `pass`/`fail`/`always` edges (route edges are managed via
    /// `RouterRoutes` instead).
    pub edge_rows: Vec<crate::domain::graphs::GraphEdge>,
    pub edge_row_index: usize,
    /// Candidate `(node_id, node_name)` retarget destinations: every other
    /// node in the edge's graph — same set as `router_targets`.
    pub edge_targets: Vec<(String, String)>,
}

impl GraphEditorDialog {
    pub fn new(
        node_id: String,
        node_name: String,
        title: String,
        help: String,
        buffer: String,
        mode: GraphEditorMode,
    ) -> Self {
        let cursor = buffer.chars().count();
        Self {
            node_id,
            node_name,
            title,
            help,
            buffer,
            cursor,
            mode,
            parse_error: None,
            router_routes: Vec::new(),
            router_fallback: String::new(),
            router_route_index: 0,
            router_field: RouterField::Label,
            router_targets: Vec::new(),
            edge_rows: Vec::new(),
            edge_row_index: 0,
            edge_targets: Vec::new(),
        }
    }

    pub fn new_edges(
        node_id: String,
        node_name: String,
        title: String,
        help: String,
        edges: Vec<crate::domain::graphs::GraphEdge>,
        targets: Vec<(String, String)>,
    ) -> Self {
        Self {
            node_id,
            node_name,
            title,
            help,
            buffer: String::new(),
            cursor: 0,
            mode: GraphEditorMode::Edges,
            parse_error: None,
            router_routes: Vec::new(),
            router_fallback: String::new(),
            router_route_index: 0,
            router_field: RouterField::Label,
            router_targets: Vec::new(),
            edge_rows: edges,
            edge_row_index: 0,
            edge_targets: targets,
        }
    }

    /// Move the `Edges` mode row focus among this node's outgoing edges,
    /// wrapping. A no-op with zero or one row.
    pub fn edge_move_row(&mut self, forward: bool) {
        if self.edge_rows.is_empty() {
            return;
        }
        self.edge_row_index =
            crate::tui::selection::move_index(self.edge_row_index, self.edge_rows.len(), forward);
    }

    /// The edge currently focused in `Edges` mode's row list.
    pub fn focused_edge(&self) -> Option<&crate::domain::graphs::GraphEdge> {
        self.edge_rows.get(self.edge_row_index)
    }

    /// The next/previous candidate destination for the focused edge,
    /// cycling through `edge_targets` (every other node in the graph).
    /// Unlike [`Self::cycle_router_target`], an ordinary edge always names a
    /// concrete `to_node`, so there is no `(none)` state to cycle through.
    pub fn next_edge_target_candidate(&self, forward: bool) -> Option<String> {
        let edge = self.focused_edge()?;
        if self.edge_targets.is_empty() {
            return None;
        }
        let current = self
            .edge_targets
            .iter()
            .position(|(id, _)| id == &edge.to_node);
        let len = self.edge_targets.len();
        let next_index = match current {
            Some(index) => crate::tui::selection::move_index(index, len, forward),
            None => 0,
        };
        Some(self.edge_targets[next_index].0.clone())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_router_routes(
        node_id: String,
        node_name: String,
        title: String,
        help: String,
        routes: Vec<RouterRouteDraft>,
        fallback: String,
        targets: Vec<(String, String)>,
    ) -> Self {
        Self {
            node_id,
            node_name,
            title,
            help,
            buffer: String::new(),
            cursor: 0,
            mode: GraphEditorMode::RouterRoutes,
            parse_error: None,
            router_routes: routes,
            router_fallback: fallback,
            router_route_index: 0,
            router_field: RouterField::Label,
            router_targets: targets,
            edge_rows: Vec::new(),
            edge_row_index: 0,
            edge_targets: Vec::new(),
        }
    }

    /// The label/description text behind the focused route's focused field,
    /// if the focus is on a text field (not the target picker).
    pub fn router_focused_text_mut(&mut self) -> Option<&mut String> {
        let field = self.router_field;
        let route = self.router_routes.get_mut(self.router_route_index)?;
        match field {
            RouterField::Label => Some(&mut route.label),
            RouterField::Description => Some(&mut route.description),
            RouterField::Target => None,
        }
    }

    /// Append to the focused route's focused text field. No-op on the
    /// target field (see [`Self::cycle_router_target`] instead) — fields are
    /// short single-line labels, so editing is append/pop-at-end only,
    /// unlike the free-cursor `buffer` used by the other editor modes.
    pub fn router_push_char(&mut self, value: char) {
        if let Some(text) = self.router_focused_text_mut() {
            text.push(value);
        }
    }

    /// Drop the last character of the focused route's focused text field.
    pub fn router_pop_char(&mut self) {
        if let Some(text) = self.router_focused_text_mut() {
            text.pop();
        }
    }

    /// Unwire the focused route's target (Backspace while it's focused).
    pub fn router_clear_target(&mut self) {
        if let Some(route) = self.router_routes.get_mut(self.router_route_index) {
            route.target_node_id = None;
        }
    }

    /// Cycle the focused route's target through `(none) -> targets... ->
    /// (none)`, in `router_targets` order.
    pub fn cycle_router_target(&mut self, forward: bool) {
        let Some(route) = self.router_routes.get_mut(self.router_route_index) else {
            return;
        };
        if self.router_targets.is_empty() {
            route.target_node_id = None;
            return;
        }
        let current = route
            .target_node_id
            .as_deref()
            .and_then(|id| self.router_targets.iter().position(|(tid, _)| tid == id));
        // Index space is `[None, targets[0], targets[1], ...]`.
        let len = self.router_targets.len() + 1;
        let current_index = current.map(|i| i + 1).unwrap_or(0);
        let next_index = crate::tui::selection::move_index(current_index, len, forward);
        route.target_node_id = if next_index == 0 {
            None
        } else {
            Some(self.router_targets[next_index - 1].0.clone())
        };
    }

    /// Move route/field focus to the next sub-field, wrapping to the next
    /// route's first field at the end.
    pub fn router_next_field(&mut self) {
        self.router_field = match self.router_field {
            RouterField::Label => RouterField::Description,
            RouterField::Description => RouterField::Target,
            RouterField::Target => {
                if !self.router_routes.is_empty() {
                    self.router_route_index = crate::tui::selection::move_index(
                        self.router_route_index,
                        self.router_routes.len(),
                        true,
                    );
                }
                RouterField::Label
            }
        };
    }

    pub fn router_prev_field(&mut self) {
        self.router_field = match self.router_field {
            RouterField::Target => RouterField::Description,
            RouterField::Description => RouterField::Label,
            RouterField::Label => {
                if !self.router_routes.is_empty() {
                    self.router_route_index = crate::tui::selection::move_index(
                        self.router_route_index,
                        self.router_routes.len(),
                        false,
                    );
                }
                RouterField::Target
            }
        };
    }

    /// Move the route selection itself (Up/Down), keeping the focused
    /// sub-field.
    pub fn router_move_route(&mut self, forward: bool) {
        if self.router_routes.is_empty() {
            return;
        }
        self.router_route_index = crate::tui::selection::move_index(
            self.router_route_index,
            self.router_routes.len(),
            forward,
        );
    }

    /// Append a fresh, unwired route and focus it (Ctrl+N).
    pub fn router_add_route(&mut self) {
        self.router_routes.push(RouterRouteDraft::default());
        self.router_route_index = self.router_routes.len() - 1;
        self.router_field = RouterField::Label;
    }

    /// Drop the focused route (Ctrl+D). If it was the fallback, the fallback
    /// is cleared — [`crate::domain::graphs::validate_router_routes`] will
    /// catch an empty/dangling fallback on save.
    pub fn router_remove_route(&mut self) {
        if self.router_routes.is_empty() {
            return;
        }
        let removed = self.router_routes.remove(self.router_route_index);
        if self.router_fallback == removed.label {
            self.router_fallback.clear();
        }
        if self.router_route_index >= self.router_routes.len() {
            self.router_route_index = self.router_routes.len().saturating_sub(1);
        }
    }

    /// Set the focused route as the fallback (Ctrl+F).
    pub fn router_set_fallback(&mut self) {
        if let Some(route) = self.router_routes.get(self.router_route_index) {
            self.router_fallback = route.label.clone();
        }
    }

    pub fn char_len(&self) -> usize {
        self.buffer.chars().count()
    }

    pub fn insert_char(&mut self, value: char) {
        self.insert_str(&value.to_string());
    }

    pub fn insert_str(&mut self, value: &str) {
        let byte_index = char_to_byte_index(&self.buffer, self.cursor);
        self.buffer.insert_str(byte_index, value);
        self.cursor += value.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte_index(&self.buffer, self.cursor - 1);
        let end = char_to_byte_index(&self.buffer, self.cursor);
        self.buffer.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.char_len();
    }
}

fn char_to_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .map(|(index, _)| index)
        .nth(char_index)
        .unwrap_or(text.len())
}
