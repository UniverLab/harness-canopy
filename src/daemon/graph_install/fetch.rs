//! Fetch a graph export document from the catalogue: one GET with a
//! `canopy/<version>` User-Agent, following redirects (max 3, https only),
//! with a 15 s timeout and the body capped at 1 MiB.

use super::GraphRef;

/// Largest catalogue response body accepted: 1 MiB.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Most redirects followed before the fetch is refused.
const MAX_REDIRECTS: usize = 3;

/// Request timeout, in seconds.
const FETCH_TIMEOUT_SECS: u64 = 15;

/// A fetched catalogue document: the raw body plus the URL the body was
/// finally read from (after any redirects).
#[derive(Debug)]
pub(crate) struct FetchedDocument {
    pub body: String,
    pub source_url: String,
}

/// GET the document for `reference` from the catalogue at `base`:
/// `<base>/i/<kind>/<slug>` for an official ref,
/// `<base>/i/<owner>/<repo>/<kind>/<slug>` for a community one, with the
/// `@<ref>` suffix (if any) as the `ref` query parameter. `kind` is
/// `"graph"` or `"node"` — the catalogue's two shareable document kinds.
pub(crate) fn fetch_document(
    base: &str,
    reference: &GraphRef,
    kind: &str,
) -> Result<FetchedDocument, String> {
    let url = request_url(base, reference, kind)?;
    let attempted = url.to_string();
    let display = reference.display();
    let allow_http = url.scheme() == "http";
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("canopy/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::custom(
            move |attempt| match redirect_decision(
                attempt.previous().len(),
                attempt.url(),
                allow_http,
            ) {
                RedirectDecision::Follow => attempt.follow(),
                RedirectDecision::Refuse(message) => attempt.error(message),
            },
        ))
        .build()
        .map_err(|error| format!("Could not build the catalogue HTTP client: {error}."))?;
    let response = client.get(url).send().map_err(|error| {
        format!("Could not fetch '{display}' from the catalogue (GET {attempted}): {error}.")
    })?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(format!(
            "Graph '{display}' was not found in the catalogue (GET {attempted} returned 404)."
        ));
    }
    if !status.is_success() {
        return Err(format!(
            "Could not fetch '{display}' from the catalogue (GET {attempted} returned {status})."
        ));
    }
    let source_url = response.url().to_string();
    let mut body = Vec::new();
    use std::io::Read;
    response
        .take((MAX_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| {
            format!(
                "Could not read the catalogue response for '{display}' (GET {attempted}): {error}."
            )
        })?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!(
            "The catalogue response for '{display}' exceeds the 1 MiB limit (GET {attempted})."
        ));
    }
    let body = String::from_utf8(body).map_err(|_| {
        format!("The catalogue response for '{display}' is not valid UTF-8 (GET {attempted}).")
    })?;
    Ok(FetchedDocument { body, source_url })
}

/// The catalogue URL for a ref: the install-counting endpoint, not the raw
/// document host.
fn request_url(base: &str, reference: &GraphRef, kind: &str) -> Result<reqwest::Url, String> {
    let base = base.trim_end_matches('/');
    let without_query = match &reference.scope {
        None => format!("{base}/i/{kind}/{}", reference.slug),
        Some((owner, repo)) => format!("{base}/i/{owner}/{repo}/{kind}/{}", reference.slug),
    };
    let mut url = reqwest::Url::parse(&without_query)
        .map_err(|error| format!("Invalid catalogue URL '{base}': {error}."))?;
    if let Some(git_ref) = &reference.git_ref {
        url.query_pairs_mut().append_pair("ref", git_ref);
    }
    Ok(url)
}

/// One redirect verdict: follow it, or refuse with a message.
enum RedirectDecision {
    Follow,
    Refuse(String),
}

/// One redirect decision: at most [`MAX_REDIRECTS`] followed, and only to
/// `https` — plus plain `http` when the catalogue base itself is `http` (the
/// local test stub; a production `https` base can never be downgraded, and a
/// redirect to any other scheme is refused rather than left to the client).
/// `previous` counts every URL visited so far, including the original
/// request — so 3 redirects have been followed when it reaches 4.
fn redirect_decision(previous: usize, target: &reqwest::Url, allow_http: bool) -> RedirectDecision {
    if previous.saturating_sub(1) >= MAX_REDIRECTS {
        return RedirectDecision::Refuse("too many redirects (max 3)".to_string());
    }
    let allowed = target.scheme() == "https" || (allow_http && target.scheme() == "http");
    if !allowed {
        return RedirectDecision::Refuse(format!("refusing redirect to non-https URL: {target}"));
    }
    RedirectDecision::Follow
}

#[cfg(test)]
mod tests {
    use super::super::stub::{StubCatalogue, StubRoute};
    use super::*;

    fn routes(pairs: Vec<(&str, StubRoute)>) -> std::collections::HashMap<String, StubRoute> {
        pairs
            .into_iter()
            .map(|(path, route)| (path.to_string(), route))
            .collect()
    }

    #[test]
    fn fetch_official_ref_requests_install_path_with_canopy_user_agent() {
        let stub = StubCatalogue::start(routes(vec![(
            "/i/graph/my-graph",
            StubRoute::ok("{\"ok\":true}"),
        )]));
        let reference = super::super::parse_ref("my-graph").unwrap();
        let fetched = fetch_document(&stub.base_url(), &reference, "graph").unwrap();
        assert_eq!(fetched.body, "{\"ok\":true}");
        assert_eq!(
            fetched.source_url,
            format!("{}/i/graph/my-graph", stub.base_url())
        );
        let hits = stub.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "/i/graph/my-graph");
        let agent = hits[0].user_agent.clone().expect("User-Agent is sent");
        assert!(agent.starts_with("canopy/"), "{agent}");
    }

    #[test]
    fn fetch_community_ref_with_version_requests_scoped_path_and_ref_query() {
        let stub = StubCatalogue::start(routes(vec![(
            "/i/acme/widgets/graph/my-graph",
            StubRoute::ok("{}"),
        )]));
        let reference = super::super::parse_ref("acme/widgets/my-graph@v1.2").unwrap();
        fetch_document(&stub.base_url(), &reference, "graph").unwrap();
        let hits = stub.hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target, "/i/acme/widgets/graph/my-graph?ref=v1.2");
    }

    #[test]
    fn fetch_tolerates_a_trailing_slash_on_the_base() {
        let stub = StubCatalogue::start(routes(vec![("/i/graph/my-graph", StubRoute::ok("{}"))]));
        let reference = super::super::parse_ref("my-graph").unwrap();
        fetch_document(&format!("{}/", stub.base_url()), &reference, "graph").unwrap();
        assert_eq!(stub.hits()[0].target, "/i/graph/my-graph");
    }

    #[test]
    fn fetch_follows_a_redirect_then_reads_the_document() {
        let stub = StubCatalogue::start(routes(vec![
            ("/i/graph/my-graph", StubRoute::redirect_to("/doc")),
            ("/doc", StubRoute::ok("{\"ok\":true}")),
        ]));
        let reference = super::super::parse_ref("my-graph").unwrap();
        let fetched = fetch_document(&stub.base_url(), &reference, "graph").unwrap();
        assert_eq!(fetched.body, "{\"ok\":true}");
        assert_eq!(
            fetched.source_url,
            format!("{}/doc", stub.base_url()),
            "the report names the URL the body was read from, not the counting endpoint"
        );
        assert_eq!(stub.hits().len(), 2);
    }

    #[test]
    fn fetch_refuses_more_than_three_redirects_end_to_end() {
        // A redirect chain longer than the 3-follow budget must fail through
        // the real client, proving the custom policy is wired in (reqwest
        // follows up to 10 redirects by default, so unwiring it would pass
        // a single redirect through and only this test would catch it).
        let stub = StubCatalogue::start(routes(vec![
            ("/i/graph/my-graph", StubRoute::redirect_to("/r1")),
            ("/r1", StubRoute::redirect_to("/r2")),
            ("/r2", StubRoute::redirect_to("/r3")),
            ("/r3", StubRoute::redirect_to("/r4")),
            ("/r4", StubRoute::ok("{\"ok\":true}")),
        ]));
        let reference = super::super::parse_ref("my-graph").unwrap();
        let error = fetch_document(&stub.base_url(), &reference, "graph")
            .expect_err("redirect chain must fail");
        assert!(error.contains("redirect"), "{error}");
        assert!(error.contains("/i/graph/my-graph"), "{error}");
        assert_eq!(
            stub.hits().len(),
            4,
            "three redirects followed, then refused"
        );
    }

    #[test]
    fn fetch_reports_404_as_not_found_with_status_and_url() {
        let stub = StubCatalogue::start(routes(vec![]));
        let reference = super::super::parse_ref("missing").unwrap();
        let error =
            fetch_document(&stub.base_url(), &reference, "graph").expect_err("404 must fail");
        assert!(error.contains("not found in the catalogue"), "{error}");
        assert!(error.contains("404"), "{error}");
        assert!(error.contains(&stub.base_url()), "{error}");
    }

    #[test]
    fn fetch_refuses_a_body_over_1_mib() {
        let big = vec![b'x'; MAX_BODY_BYTES + 1];
        let stub = StubCatalogue::start(routes(vec![(
            "/i/graph/my-graph",
            StubRoute {
                status: 200,
                headers: Vec::new(),
                body: big,
            },
        )]));
        let reference = super::super::parse_ref("my-graph").unwrap();
        let error =
            fetch_document(&stub.base_url(), &reference, "graph").expect_err("oversize must fail");
        assert!(error.contains("1 MiB"), "{error}");
    }

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    /// The downgrade half of the policy is unit-tested: the stub speaks plain
    /// `http`, so its base can never be `https`, and `allow_http` is true for
    /// every request it serves. The wiring of the custom policy itself is
    /// proven end to end by the redirect-cap test below.
    #[test]
    fn redirect_to_a_non_https_target_is_refused_unless_the_base_is_http() {
        assert!(matches!(
            redirect_decision(1, &url("http://x/graph"), false),
            RedirectDecision::Refuse(_)
        ));
        let RedirectDecision::Refuse(message) = redirect_decision(1, &url("http://x/graph"), false)
        else {
            panic!("http redirect must be refused");
        };
        assert!(message.contains("non-https"), "{message}");
        assert!(matches!(
            redirect_decision(1, &url("http://x/graph"), true),
            RedirectDecision::Follow
        ));
        assert!(matches!(
            redirect_decision(1, &url("https://x/graph"), false),
            RedirectDecision::Follow
        ));
        // Any scheme that is not https is refused, not passed to the client.
        for target in ["ftp://x/graph", "file:///etc/passwd", "data:text/plain,x"] {
            assert!(
                matches!(
                    redirect_decision(1, &url(target), false),
                    RedirectDecision::Refuse(_)
                ),
                "{target} must be refused"
            );
        }
    }

    #[test]
    fn redirect_past_three_is_refused() {
        // `previous` includes the original request: 3 followed at 4.
        assert!(matches!(
            redirect_decision(3, &url("https://x/3"), false),
            RedirectDecision::Follow
        ));
        assert!(matches!(
            redirect_decision(4, &url("https://x/4"), false),
            RedirectDecision::Refuse(_)
        ));
    }
}
