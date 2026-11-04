use super::*;

// ── Catalogue tool parameter types ───────────────────────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CatalogueSearchParams {
    /// Free-text query matched against community graphs and nodes
    /// (`topic:canopy-graph` plus description and name).
    pub q: String,
    /// Result order: `top` (default) or `new`.
    pub sort: Option<String>,
    /// Page number, starting at 1. Defaults to 1.
    pub page: Option<u32>,
    /// Maximum items returned from this page, client-side. Defaults to 10,
    /// capped at 50; deeper access uses `page`.
    pub limit: Option<u32>,
}
