//! Search the graphs catalogue (`GET /api/search`): discovery only.
//!
//! Agents search here before building their own graphs; installing stays
//! with the existing `graph install` / `node install` paths. One blocking
//! GET with a `canopy/<version>` User-Agent, a 15 s timeout, and the body
//! capped at 1 MiB (mirroring `fetch.rs`). Network and server failures are
//! errors — never empty results.

use serde::Deserialize;
use serde_json::Value;

/// Largest catalogue search response accepted: 1 MiB (mirrors `fetch.rs`).
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Request timeout, in seconds (mirrors `fetch.rs`).
const SEARCH_TIMEOUT_SECS: u64 = 15;

/// One catalogue search response: `{items, q, sort, page, total,
/// truncated}`. `items` stays [`Value`]: the site owns the item shape —
/// including the FEAT-24 model verdict — so a typed item would reject
/// fields the site adds.
#[derive(Debug, Deserialize)]
pub(crate) struct SearchResponse {
    pub items: Vec<Value>,
    pub q: String,
    pub sort: String,
    pub page: u32,
    pub total: u64,
    pub truncated: bool,
}

/// Build the catalogue search URL: `{base}/api/search` with the `q`,
/// `sort`, and `page` query parameters. `sort` is `top` (default) or
/// `new`; `page` starts at 1 (default 1). Anything else is refused with
/// the accepted values in the message.
pub(crate) fn search_url(
    base: &str,
    q: &str,
    sort: Option<&str>,
    page: Option<u32>,
) -> Result<reqwest::Url, String> {
    let sort = match sort {
        None => "top",
        Some("top") => "top",
        Some("new") => "new",
        Some(other) => {
            return Err(format!("Invalid sort '{other}': use 'top' or 'new'."));
        }
    };
    let page = match page {
        None => 1,
        Some(0) => {
            return Err("Invalid page '0': use a page number starting at 1.".to_string());
        }
        Some(number) => number,
    };
    let base = base.trim_end_matches('/');
    let without_query = format!("{base}/api/search");
    let mut url = reqwest::Url::parse(&without_query)
        .map_err(|error| format!("Invalid catalogue URL '{base}': {error}."))?;
    url.query_pairs_mut()
        .append_pair("q", q)
        .append_pair("sort", sort)
        .append_pair("page", &page.to_string());
    Ok(url)
}

/// Resolve the client-side page cap: default 10, clamped to 1..=50.
/// The API has no page-size parameter, so the caller truncates the
/// returned page to this many items; deeper access uses `page`.
pub(crate) fn resolve_limit(limit: Option<u32>) -> usize {
    limit.unwrap_or(10).clamp(1, 50) as usize
}

/// Blocking GET of one catalogue search page. Network failures, non-2xx
/// statuses, oversize bodies, and unparsable responses are all errors
/// naming the attempted URL — a failed search never returns empty results.
pub(crate) fn search_catalogue(
    base: &str,
    q: &str,
    sort: Option<&str>,
    page: Option<u32>,
) -> Result<SearchResponse, String> {
    let url = search_url(base, q, sort, page)?;
    let attempted = url.to_string();
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("canopy/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(SEARCH_TIMEOUT_SECS))
        .build()
        .map_err(|error| format!("Could not build the catalogue HTTP client: {error}."))?;
    let response = client
        .get(url)
        .send()
        .map_err(|error| format!("Could not search the catalogue (GET {attempted}): {error}."))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "Could not search the catalogue (GET {attempted} returned {status})."
        ));
    }
    let mut body = Vec::new();
    use std::io::Read;
    response
        .take((MAX_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| {
            format!("Could not read the catalogue search response (GET {attempted}): {error}.")
        })?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "The catalogue search response exceeds the 1 MiB limit (GET {attempted})."
        ));
    }
    let body = String::from_utf8(body).map_err(|_| {
        format!("The catalogue search response is not valid UTF-8 (GET {attempted}).")
    })?;
    serde_json::from_str(&body).map_err(|error| {
        format!("Could not parse the catalogue search response (GET {attempted}): {error}.")
    })
}

#[cfg(test)]
mod tests {
    use super::super::stub::{StubCatalogue, StubRoute};
    use super::*;
    use serde_json::json;

    fn routes(pairs: Vec<(&str, StubRoute)>) -> std::collections::HashMap<String, StubRoute> {
        pairs
            .into_iter()
            .map(|(path, route)| (path.to_string(), route))
            .collect()
    }

    fn page_body(items: &serde_json::Value, q: &str, sort: &str, page: u32) -> String {
        json!({
            "items": items,
            "q": q,
            "sort": sort,
            "page": page,
            "total": 42,
            "truncated": false,
        })
        .to_string()
    }

    /// One catalogue item in the shape the live API returns (measured
    /// 2026-10-31): `repo` is the combined `owner/repo`, `official` picks
    /// the install-ref form (bare slug when true, `repo/slug` otherwise),
    /// and there is no literal install-ref field.
    fn item(slug: &str) -> serde_json::Value {
        json!({
            "kind": "graph",
            "slug": slug,
            "name": slug,
            "description": format!("{slug} does things"),
            "repo": "acme/widgets",
            "official": false,
            "stars": 7,
            "sourceRef": "main",
        })
    }

    #[test]
    fn search_url_defaults_to_top_and_page_one() {
        let url = search_url("http://catalogue.test", "graphs", None, None).unwrap();
        assert_eq!(
            url.as_str(),
            "http://catalogue.test/api/search?q=graphs&sort=top&page=1"
        );
    }

    #[test]
    fn search_url_honours_sort_page_and_a_trailing_slash() {
        let url = search_url("http://catalogue.test/", "my graph", Some("new"), Some(3)).unwrap();
        assert_eq!(
            url.as_str(),
            "http://catalogue.test/api/search?q=my+graph&sort=new&page=3"
        );
    }

    #[test]
    fn search_url_refuses_an_unknown_sort_naming_top_and_new() {
        let error = search_url("http://catalogue.test", "graphs", Some("pop"), None)
            .expect_err("unknown sort must fail");
        assert!(error.contains("'top'"), "{error}");
        assert!(error.contains("'new'"), "{error}");
    }

    #[test]
    fn search_url_refuses_page_zero() {
        let error = search_url("http://catalogue.test", "graphs", None, Some(0))
            .expect_err("page zero must fail");
        assert!(error.contains("starting at 1"), "{error}");
    }

    #[test]
    fn resolve_limit_defaults_to_10_and_clamps_to_1_through_50() {
        assert_eq!(resolve_limit(None), 10);
        assert_eq!(resolve_limit(Some(0)), 1);
        assert_eq!(resolve_limit(Some(1)), 1);
        assert_eq!(resolve_limit(Some(7)), 7);
        assert_eq!(resolve_limit(Some(50)), 50);
        assert_eq!(resolve_limit(Some(500)), 50);
    }

    #[test]
    fn search_sends_the_query_with_a_canopy_user_agent() {
        let body = page_body(&json!([item("a")]), "graphs", "top", 1);
        let stub = StubCatalogue::start(routes(vec![("/api/search", StubRoute::ok(&body))]));
        let found = search_catalogue(&stub.base_url(), "graphs", None, None).unwrap();
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.q, "graphs");
        assert_eq!(found.sort, "top");
        assert_eq!(found.page, 1);
        assert_eq!(found.total, 42);
        assert!(!found.truncated);
        let hits = stub.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "/api/search?q=graphs&sort=top&page=1");
        let agent = hits[0].user_agent.clone().expect("User-Agent is sent");
        assert!(agent.starts_with("canopy/"), "{agent}");
    }

    #[test]
    fn search_truncates_the_page_to_the_resolved_limit() {
        let items: Vec<serde_json::Value> = (0..60).map(|n| item(&format!("g{n:02}"))).collect();
        let body = page_body(&json!(items), "graphs", "top", 1);
        let stub = StubCatalogue::start(routes(vec![("/api/search", StubRoute::ok(&body))]));
        let mut found = search_catalogue(&stub.base_url(), "graphs", None, None).unwrap();
        assert_eq!(found.items.len(), 60);
        found.items.truncate(resolve_limit(Some(500)));
        assert_eq!(found.items.len(), 50, "limit clamps to 50");
        assert_eq!(found.items[0]["slug"], "g00");
        assert_eq!(found.items[49]["slug"], "g49");
        found.items.truncate(resolve_limit(None));
        assert_eq!(found.items.len(), 10, "limit defaults to 10");
        assert_eq!(found.items[0]["slug"], "g00");
    }

    #[test]
    fn search_passes_items_through_without_a_verdict() {
        let body = page_body(&json!([item("plain")]), "graphs", "top", 1);
        let stub = StubCatalogue::start(routes(vec![("/api/search", StubRoute::ok(&body))]));
        let found = search_catalogue(&stub.base_url(), "graphs", None, None).unwrap();
        let first = &found.items[0];
        assert_eq!(first["kind"], "graph");
        assert_eq!(first["slug"], "plain");
        assert_eq!(first["repo"], "acme/widgets");
        assert_eq!(first["official"], false);
        assert_eq!(first["stars"], 7);
        assert_eq!(first["description"], "plain does things");
        assert!(
            first.get("verdict").is_none(),
            "no verdict shipped yet: {first}"
        );
    }

    #[test]
    fn search_keeps_the_model_verdict_when_the_api_exposes_it() {
        let mut judged = item("judged");
        judged["verdict"] = json!({
            "label": "pass",
            "model": "canopy-reviewer-1",
            "scan_date": "2026-10-01",
        });
        let body = page_body(&json!([judged]), "graphs", "new", 2);
        let stub = StubCatalogue::start(routes(vec![("/api/search", StubRoute::ok(&body))]));
        let found = search_catalogue(&stub.base_url(), "graphs", Some("new"), Some(2)).unwrap();
        let verdict = &found.items[0]["verdict"];
        assert_eq!(verdict["label"], "pass");
        assert_eq!(verdict["model"], "canopy-reviewer-1");
        assert_eq!(verdict["scan_date"], "2026-10-01");
        assert_eq!(found.sort, "new");
        assert_eq!(found.page, 2);
        assert_eq!(
            stub.hits()[0].target,
            "/api/search?q=graphs&sort=new&page=2"
        );
    }

    #[test]
    fn search_reports_a_500_with_status_and_url() {
        let stub = StubCatalogue::start(routes(vec![(
            "/api/search",
            StubRoute {
                status: 500,
                headers: Vec::new(),
                body: b"boom".to_vec(),
            },
        )]));
        let error = search_catalogue(&stub.base_url(), "graphs", None, None)
            .expect_err("500 must fail, never empty-ok");
        assert!(error.contains("500"), "{error}");
        assert!(error.contains(&stub.base_url()), "{error}");
    }

    #[test]
    fn search_refuses_a_body_over_1_mib() {
        let big = vec![b'x'; MAX_BODY_BYTES + 1];
        let stub = StubCatalogue::start(routes(vec![(
            "/api/search",
            StubRoute {
                status: 200,
                headers: Vec::new(),
                body: big,
            },
        )]));
        let error = search_catalogue(&stub.base_url(), "graphs", None, None)
            .expect_err("oversize must fail");
        assert!(error.contains("1 MiB"), "{error}");
    }
}
