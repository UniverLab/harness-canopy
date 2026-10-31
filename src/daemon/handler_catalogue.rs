//! The `catalogue` `search` action: discovery over the community graphs
//! catalogue.
//!
//! Agents search here before building their own graphs; installing stays
//! with the existing `graph install` / `node install` paths.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    pub(crate) async fn catalogue_search_impl(
        &self,
        params: CatalogueSearchParams,
    ) -> Result<CallToolResult, McpError> {
        // The API has no page-size parameter, so `limit` truncates the
        // returned page client-side; deeper access uses `page`.
        let limit = crate::daemon::graph_install::resolve_limit(params.limit);
        let q = params.q.clone();
        let sort = params.sort.clone();
        let page = params.page;
        // The fetch is blocking, so it runs off the async runtime — the
        // same `spawn_blocking` shape `node_install_impl` uses.
        let searched = tokio::task::spawn_blocking(move || {
            crate::daemon::graph_install::search_catalogue(
                &crate::daemon::graph_install::catalogue_url(),
                &q,
                sort.as_deref(),
                page,
            )
        })
        .await
        .map_err(|error| internal_error(error.to_string()))?;
        let mut searched = match searched {
            Ok(searched) => searched,
            Err(message) => return Ok(error_result(&message)),
        };
        searched.items.truncate(limit);
        Ok(build_json_result(&serde_json::json!({
            "items": searched.items,
            "q": searched.q,
            "sort": searched.sort,
            "page": searched.page,
            "total": searched.total,
            "truncated": searched.truncated,
        })))
    }
}
