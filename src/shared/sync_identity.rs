pub const CANOPY_AGENT_ID_ENV: &str = "CANOPY_AGENT_ID";
pub const CANOPY_SESSION_NAME_ENV: &str = "CANOPY_SESSION_NAME";
pub const CANOPY_WORKDIR_ENV: &str = "CANOPY_WORKDIR";
/// Optional env var for seed-bound sessions.
pub const CANOPY_SEED_ID_ENV: &str = "CANOPY_SEED_ID";
/// Optional env var selecting the MCP tool profile for a session.
pub const CANOPY_MCP_PROFILE_ENV: &str = "CANOPY_MCP_PROFILE";
/// Optional env var carrying the harness's own conversation identifier
/// (Claude Code session / transcript id, opencode session id, muse resume id).
/// A later registration with the same value is the same conversation resumed.
pub const CANOPY_HARNESS_SESSION_ID_ENV: &str = "CANOPY_HARNESS_SESSION_ID";

pub const CANOPY_AGENT_ID_HEADER: &str = "x-canopy-agent-id";
/// Optional header for seed-bound sessions.
pub const CANOPY_SEED_ID_HEADER: &str = "x-canopy-seed-id";
/// HTTP header forwarding the MCP tool profile for a session.
pub const CANOPY_MCP_PROFILE_HEADER: &str = "x-canopy-mcp-profile";
/// Optional header carrying the harness's own conversation identifier, the
/// header twin of [`CANOPY_HARNESS_SESSION_ID_ENV`].
pub const CANOPY_HARNESS_SESSION_ID_HEADER: &str = "x-canopy-harness-session-id";

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
        assert!(!CANOPY_SEED_ID_ENV.is_empty());
    }

    #[test]
    fn header_constants_are_nonempty() {
        assert!(!CANOPY_AGENT_ID_HEADER.is_empty());
        assert!(!CANOPY_SEED_ID_HEADER.is_empty());
    }

    #[test]
    fn env_constant_values_match_expected() {
        assert_eq!(CANOPY_AGENT_ID_ENV, "CANOPY_AGENT_ID");
        assert_eq!(CANOPY_SESSION_NAME_ENV, "CANOPY_SESSION_NAME");
        assert_eq!(CANOPY_WORKDIR_ENV, "CANOPY_WORKDIR");
        assert_eq!(CANOPY_SEED_ID_ENV, "CANOPY_SEED_ID");
    }

    #[test]
    fn header_constant_values_match_expected() {
        assert_eq!(CANOPY_AGENT_ID_HEADER, "x-canopy-agent-id");
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
}
