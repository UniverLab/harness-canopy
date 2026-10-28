pub const CANOPY_AGENT_ID_ENV: &str = "CANOPY_AGENT_ID";
pub const CANOPY_SESSION_NAME_ENV: &str = "CANOPY_SESSION_NAME";
pub const CANOPY_WORKDIR_ENV: &str = "CANOPY_WORKDIR";
pub const CANOPY_CLIENT_NAME_ENV: &str = "CANOPY_CLIENT_NAME";
/// Optional env var for seed-bound sessions.
pub const CANOPY_SEED_ID_ENV: &str = "CANOPY_SEED_ID";
/// Optional env var selecting the MCP tool profile for a session.
pub const CANOPY_MCP_PROFILE_ENV: &str = "CANOPY_MCP_PROFILE";
/// Optional env var carrying the harness's own conversation identifier
/// (Claude Code session / transcript id, opencode session id, muse resume id).
/// A later registration with the same value is the same conversation resumed.
pub const CANOPY_HARNESS_SESSION_ID_ENV: &str = "CANOPY_HARNESS_SESSION_ID";
/// Env var carrying the graph id of the node run an agent process executes.
/// The engine sets all three identity vars on every agent process it spawns
/// so a verdict never depends on the model reproducing UUIDs from memory.
pub const CANOPY_GRAPH_ID_ENV: &str = "CANOPY_GRAPH_ID";
/// Env var carrying the node id of the node run an agent process executes.
pub const CANOPY_NODE_ID_ENV: &str = "CANOPY_NODE_ID";
/// Env var carrying the run id of the node run an agent process executes.
pub const CANOPY_NODE_RUN_ID_ENV: &str = "CANOPY_NODE_RUN_ID";

pub const CANOPY_AGENT_ID_HEADER: &str = "x-canopy-agent-id";
/// Static header injected by the MCP client config with the harness name (e.g. "copilot", "opencode").
pub const CANOPY_CLIENT_NAME_HEADER: &str = "x-canopy-client-name";
/// Optional header for seed-bound sessions.
pub const CANOPY_SEED_ID_HEADER: &str = "x-canopy-seed-id";
/// HTTP header forwarding the MCP tool profile for a session.
pub const CANOPY_MCP_PROFILE_HEADER: &str = "x-canopy-mcp-profile";
/// Optional header carrying the harness's own conversation identifier, the
/// header twin of [`CANOPY_HARNESS_SESSION_ID_ENV`].
pub const CANOPY_HARNESS_SESSION_ID_HEADER: &str = "x-canopy-harness-session-id";
/// Header twin of [`CANOPY_GRAPH_ID_ENV`]: the bridge forwards the env var
/// to the daemon on every proxied request.
pub const CANOPY_GRAPH_ID_HEADER: &str = "x-canopy-graph-id";
/// Header twin of [`CANOPY_NODE_ID_ENV`].
pub const CANOPY_NODE_ID_HEADER: &str = "x-canopy-node-id";
/// Header twin of [`CANOPY_NODE_RUN_ID_ENV`].
pub const CANOPY_NODE_RUN_ID_HEADER: &str = "x-canopy-node-run-id";

/// The graph/node/run identity a node agent process carries: the run the
/// engine spawned it for. Resolved by [`graph_identity`] from the request
/// headers first, then the process environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphIdentity {
    /// Full graph id (`CANOPY_GRAPH_ID`).
    pub graph_id: String,
    /// Full node id (`CANOPY_NODE_ID`).
    pub node_id: String,
    /// Full node run id (`CANOPY_NODE_RUN_ID`).
    pub run_id: String,
}

/// Resolve the calling agent's [`GraphIdentity`]: the three `x-canopy-*`
/// headers when all three are present, else the three `CANOPY_*` env vars
/// when all three are set and non-empty after trimming, else `None`. Each
/// source only counts as a complete triple — values are never mixed across
/// the two sources.
pub(crate) fn graph_identity(parts: Option<&axum::http::request::Parts>) -> Option<GraphIdentity> {
    if let Some(parts) = parts {
        if let (Some(graph_id), Some(node_id), Some(run_id)) = (
            header_str(parts, CANOPY_GRAPH_ID_HEADER),
            header_str(parts, CANOPY_NODE_ID_HEADER),
            header_str(parts, CANOPY_NODE_RUN_ID_HEADER),
        ) {
            return Some(GraphIdentity {
                graph_id: graph_id.to_string(),
                node_id: node_id.to_string(),
                run_id: run_id.to_string(),
            });
        }
    }
    let (Some(graph_id), Some(node_id), Some(run_id)) = (
        non_empty_env(CANOPY_GRAPH_ID_ENV),
        non_empty_env(CANOPY_NODE_ID_ENV),
        non_empty_env(CANOPY_NODE_RUN_ID_ENV),
    ) else {
        return None;
    };
    Some(GraphIdentity {
        graph_id,
        node_id,
        run_id,
    })
}

/// Read an env var trimmed, `None` when unset, empty, or whitespace-only.
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The harness conversation identifier a request reports, header first then
/// env — the same precedence the agent-id/client-name resolution uses. `None`
/// when the caller carries neither, which keeps today's behaviour (a new
/// session id and name on registration).
pub(crate) fn harness_conversation_id(
    parts: Option<&axum::http::request::Parts>,
) -> Option<String> {
    if let Some(parts) = parts {
        if let Some(harness_id) = header_str(parts, CANOPY_HARNESS_SESSION_ID_HEADER) {
            return Some(harness_id.to_string());
        }
    }
    std::env::var(CANOPY_HARNESS_SESSION_ID_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Read a header value from an HTTP request, trimmed and non-empty.
pub(crate) fn header_str<'a>(parts: &'a axum::http::request::Parts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Request};

    #[test]
    fn env_constants_are_nonempty() {
        assert!(!CANOPY_AGENT_ID_ENV.is_empty());
        assert!(!CANOPY_SESSION_NAME_ENV.is_empty());
        assert!(!CANOPY_WORKDIR_ENV.is_empty());
        assert!(!CANOPY_CLIENT_NAME_ENV.is_empty());
        assert!(!CANOPY_SEED_ID_ENV.is_empty());
    }

    #[test]
    fn header_constants_are_nonempty() {
        assert!(!CANOPY_AGENT_ID_HEADER.is_empty());
        assert!(!CANOPY_CLIENT_NAME_HEADER.is_empty());
        assert!(!CANOPY_SEED_ID_HEADER.is_empty());
    }

    #[test]
    fn env_constant_values_match_expected() {
        assert_eq!(CANOPY_AGENT_ID_ENV, "CANOPY_AGENT_ID");
        assert_eq!(CANOPY_SESSION_NAME_ENV, "CANOPY_SESSION_NAME");
        assert_eq!(CANOPY_WORKDIR_ENV, "CANOPY_WORKDIR");
        assert_eq!(CANOPY_CLIENT_NAME_ENV, "CANOPY_CLIENT_NAME");
        assert_eq!(CANOPY_SEED_ID_ENV, "CANOPY_SEED_ID");
    }

    #[test]
    fn header_constant_values_match_expected() {
        assert_eq!(CANOPY_AGENT_ID_HEADER, "x-canopy-agent-id");
        assert_eq!(CANOPY_CLIENT_NAME_HEADER, "x-canopy-client-name");
        assert_eq!(CANOPY_SEED_ID_HEADER, "x-canopy-seed-id");
    }

    fn make_request_with_header(name: &str, value: &str) -> axum::http::request::Parts {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::try_from(name).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        let (parts, _) = Request::builder().body(()).unwrap().into_parts();
        let mut parts = parts;
        parts.headers = headers;
        parts
    }

    #[test]
    fn header_str_returns_value_when_present() {
        let parts = make_request_with_header("x-canopy-agent-id", "agent-123");
        assert_eq!(header_str(&parts, "x-canopy-agent-id"), Some("agent-123"));
    }

    #[test]
    fn header_str_trims_whitespace() {
        let parts = make_request_with_header("x-canopy-agent-id", "  agent-123  ");
        assert_eq!(header_str(&parts, "x-canopy-agent-id"), Some("agent-123"));
    }

    #[test]
    fn header_str_returns_none_for_missing_header() {
        let parts = make_request_with_header("other-header", "value");
        assert_eq!(header_str(&parts, "x-canopy-agent-id"), None);
    }

    #[test]
    fn header_str_returns_none_for_empty_value() {
        let parts = make_request_with_header("x-canopy-agent-id", "");
        assert_eq!(header_str(&parts, "x-canopy-agent-id"), None);
    }

    #[test]
    fn header_str_returns_none_for_whitespace_only_value() {
        let parts = make_request_with_header("x-canopy-agent-id", "   ");
        assert_eq!(header_str(&parts, "x-canopy-agent-id"), None);
    }

    #[test]
    fn harness_conversation_id_is_read_from_its_header() {
        let parts = make_request_with_header("x-canopy-harness-session-id", " conv-42 ");
        assert_eq!(
            harness_conversation_id(Some(&parts)),
            Some("conv-42".to_string())
        );
    }

    #[test]
    fn harness_conversation_id_constant_values_match_expected() {
        assert_eq!(
            CANOPY_HARNESS_SESSION_ID_HEADER,
            "x-canopy-harness-session-id"
        );
        assert_eq!(CANOPY_HARNESS_SESSION_ID_ENV, "CANOPY_HARNESS_SESSION_ID");
    }

    #[test]
    fn graph_identity_constant_values_match_expected() {
        assert_eq!(CANOPY_GRAPH_ID_ENV, "CANOPY_GRAPH_ID");
        assert_eq!(CANOPY_NODE_ID_ENV, "CANOPY_NODE_ID");
        assert_eq!(CANOPY_NODE_RUN_ID_ENV, "CANOPY_NODE_RUN_ID");
        assert_eq!(CANOPY_GRAPH_ID_HEADER, "x-canopy-graph-id");
        assert_eq!(CANOPY_NODE_ID_HEADER, "x-canopy-node-id");
        assert_eq!(CANOPY_NODE_RUN_ID_HEADER, "x-canopy-node-run-id");
    }

    /// RAII guard pinning the three identity env vars (or clearing them),
    /// restoring the previous values on drop. Safe under `cargo nextest`
    /// (one process per test) but would race under plain `cargo test`.
    struct IdentityVar {
        prev: [(String, Option<std::ffi::OsString>); 3],
    }

    impl IdentityVar {
        fn set(graph_id: &str, node_id: &str, run_id: &str) -> Self {
            let prev = [
                (
                    CANOPY_GRAPH_ID_ENV.to_string(),
                    std::env::var_os(CANOPY_GRAPH_ID_ENV),
                ),
                (
                    CANOPY_NODE_ID_ENV.to_string(),
                    std::env::var_os(CANOPY_NODE_ID_ENV),
                ),
                (
                    CANOPY_NODE_RUN_ID_ENV.to_string(),
                    std::env::var_os(CANOPY_NODE_RUN_ID_ENV),
                ),
            ];
            unsafe {
                std::env::set_var(CANOPY_GRAPH_ID_ENV, graph_id);
                std::env::set_var(CANOPY_NODE_ID_ENV, node_id);
                std::env::set_var(CANOPY_NODE_RUN_ID_ENV, run_id);
            }
            IdentityVar { prev }
        }

        fn clear() -> Self {
            let this = Self::set("", "", "");
            unsafe {
                std::env::remove_var(CANOPY_GRAPH_ID_ENV);
                std::env::remove_var(CANOPY_NODE_ID_ENV);
                std::env::remove_var(CANOPY_NODE_RUN_ID_ENV);
            }
            this
        }
    }

    impl Drop for IdentityVar {
        fn drop(&mut self) {
            for (name, prev) in &self.prev {
                match prev {
                    Some(v) => unsafe { std::env::set_var(name, v) },
                    None => unsafe { std::env::remove_var(name) },
                }
            }
        }
    }

    fn make_parts(headers: &[(&str, &str)]) -> axum::http::request::Parts {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                HeaderName::try_from(*name).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        let (mut parts, _) = Request::builder().body(()).unwrap().into_parts();
        parts.headers = map;
        parts
    }

    #[test]
    fn graph_identity_prefers_headers_over_env() {
        let _guard = IdentityVar::set("env-graph", "env-node", "env-run");
        let parts = make_parts(&[
            ("x-canopy-graph-id", "hdr-graph"),
            ("x-canopy-node-id", "hdr-node"),
            ("x-canopy-node-run-id", "hdr-run"),
        ]);
        assert_eq!(
            graph_identity(Some(&parts)),
            Some(GraphIdentity {
                graph_id: "hdr-graph".to_string(),
                node_id: "hdr-node".to_string(),
                run_id: "hdr-run".to_string(),
            })
        );
    }

    #[test]
    fn graph_identity_falls_back_to_trimmed_env() {
        let _guard = IdentityVar::set("  env-graph ", "env-node", "env-run");
        let parts = make_parts(&[]);
        assert_eq!(
            graph_identity(Some(&parts)),
            Some(GraphIdentity {
                graph_id: "env-graph".to_string(),
                node_id: "env-node".to_string(),
                run_id: "env-run".to_string(),
            })
        );
        assert_eq!(
            graph_identity(None),
            Some(GraphIdentity {
                graph_id: "env-graph".to_string(),
                node_id: "env-node".to_string(),
                run_id: "env-run".to_string(),
            })
        );
    }

    #[test]
    fn graph_identity_rejects_a_partial_triple() {
        // Two of three headers and a full env triple: the headers don't
        // form a triple, so the env triple applies whole — never mixed
        // with the header values.
        let _guard = IdentityVar::set("env-graph", "env-node", "env-run");
        let parts = make_parts(&[
            ("x-canopy-graph-id", "hdr-graph"),
            ("x-canopy-node-id", "hdr-node"),
        ]);
        assert_eq!(
            graph_identity(Some(&parts)),
            Some(GraphIdentity {
                graph_id: "env-graph".to_string(),
                node_id: "env-node".to_string(),
                run_id: "env-run".to_string(),
            })
        );

        // No headers and one blank env var: no identity.
        let _guard = IdentityVar::set("env-graph", "  ", "env-run");
        assert_eq!(graph_identity(None), None);

        // Nothing anywhere.
        let _cleared = IdentityVar::clear();
        assert_eq!(graph_identity(None), None);
    }
}
