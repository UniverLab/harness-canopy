//! Where one registry refresh reads from: a commit-pinned
//! `raw.githubusercontent.com` base URL instead of the CDN-cached `/main/`
//! branch URL.
//!
//! Right after a registry merge the branch URL can serve a stale copy for a
//! few minutes, so a refresh could report "nothing deleted" while the merge
//! deleted platforms. A URL naming a commit sha is immutable and never stale:
//! [`resolve_registry_base`] asks the GitHub commits API for main's current
//! sha once per refresh, and every file of that refresh is fetched under it.

use std::time::Duration;

/// How long the single commit-sha lookup may take. Nothing is retried: on any
/// failure the caller falls back to the branch URL.
const COMMIT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// One refresh's read position: every registry file URL is `base_url` plus a
/// relative path, and `source` is the label the refresh report carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryBase {
    pub base_url: String,
    pub source: String,
}

/// Ask `api_url` (production:
/// `https://api.github.com/repos/UniverLab/canopy-registry/commits/main` with
/// `Accept: application/vnd.github.sha`) for main's current commit sha. A 200
/// whose trimmed body is a 40-char hex sha pins `base_url` to
/// `{raw_root}/{sha}/`; anything else falls back to `{raw_root}/main/` with a
/// `source` label saying so. Exactly one API call, never retried.
pub(crate) fn resolve_registry_base(
    client: &reqwest::blocking::Client,
    api_url: &str,
    raw_root: &str,
) -> RegistryBase {
    match fetch_commit_sha(client, api_url) {
        Ok(sha) => RegistryBase {
            base_url: format!("{raw_root}/{sha}/"),
            source: format!("commit {}", &sha[..7]),
        },
        Err(reason) => {
            tracing::warn!(
                "registry commit lookup failed ({reason}); using branch main, \
                 which may lag a few minutes behind a merge"
            );
            fallback_base(raw_root, &reason)
        }
    }
}

/// The branch-URL fallback for `reason`, with the label the report prints.
fn fallback_base(raw_root: &str, reason: &str) -> RegistryBase {
    RegistryBase {
        base_url: format!("{raw_root}/main/"),
        source: format!(
            "branch main (commit lookup failed: {reason}); it may lag a few minutes behind a merge"
        ),
    }
}

/// One GET to the commits API; `Ok(sha)` only for a 200 with a sha body.
fn fetch_commit_sha(client: &reqwest::blocking::Client, api_url: &str) -> Result<String, String> {
    let response = client
        .get(api_url)
        .header("Accept", "application/vnd.github.sha")
        .header("User-Agent", "canopy")
        .timeout(COMMIT_LOOKUP_TIMEOUT)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let sha = response
        .text()
        .map_err(|e| format!("unreadable body: {e}"))?;
    let sha = sha.trim();
    if sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(sha.to_string())
    } else {
        Err("unexpected API body".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::registry_fetch::{fetch_registry_from, RegistryRefreshReport};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const API_PATH: &str = "/commits/main";
    const INDEX_BODY: &str = "version = 6\n[[platforms]]\nname = \"testcli\"\nbinary = \"ls\"\n";
    const PLATFORM_BODY: &str =
        "name = \"testcli\"\nconfig_path = \"testcli.marker\"\nmcp_servers_key = []\n";

    /// What the stub serves. The API answers one body per hit (the last entry
    /// repeats), so a test can tell whether the fetch called it more than once.
    struct StubBehavior {
        api_status: u16,
        api_bodies: Vec<String>,
    }

    impl StubBehavior {
        fn sha(status_body: &str) -> Self {
            Self {
                api_status: 200,
                api_bodies: vec![status_body.to_string()],
            }
        }
    }

    /// A local HTTP stub recording every raw request path and API hit count.
    struct Stub {
        addr: SocketAddr,
        raw_paths: Arc<Mutex<Vec<String>>>,
        api_hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Stub {
        fn api_url(&self) -> String {
            format!("http://{}{API_PATH}", self.addr)
        }

        fn raw_root(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn raw_paths(&self) -> Vec<String> {
            self.raw_paths.lock().unwrap().clone()
        }

        fn api_hits(&self) -> usize {
            self.api_hits.load(Ordering::SeqCst)
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Serve `behavior` on a loopback stub until [`Stub::stop`].
    fn start_stub(behavior: StubBehavior) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let behavior = Arc::new(behavior);
        let raw_paths = Arc::new(Mutex::new(Vec::new()));
        let api_hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let behavior = Arc::clone(&behavior);
            let raw_paths = Arc::clone(&raw_paths);
            let api_hits = Arc::clone(&api_hits);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let Ok((stream, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    };
                    serve_one(stream, &behavior, &raw_paths, &api_hits);
                }
            })
        };
        Stub {
            addr,
            raw_paths,
            api_hits,
            stop,
            thread: Some(thread),
        }
    }

    /// Route one connection: the API path answers the commit lookup (counted),
    /// every other path is a raw registry file (recorded).
    fn serve_one(
        mut stream: std::net::TcpStream,
        behavior: &StubBehavior,
        raw_paths: &Arc<Mutex<Vec<String>>>,
        api_hits: &Arc<AtomicUsize>,
    ) {
        let mut buf = [0u8; 8192];
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }
        let path = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        if path == API_PATH {
            let hit = api_hits.fetch_add(1, Ordering::SeqCst);
            let body = behavior
                .api_bodies
                .get(hit)
                .or_else(|| behavior.api_bodies.last())
                .cloned()
                .unwrap_or_default();
            respond(&mut stream, behavior.api_status, &body);
        } else {
            raw_paths.lock().unwrap().push(path.clone());
            if path.ends_with("servers.toml") || path.ends_with("platforms.json") {
                respond(&mut stream, 404, "");
            } else if path.contains("/platforms/") {
                respond(&mut stream, 200, PLATFORM_BODY);
            } else {
                respond(&mut stream, 200, INDEX_BODY);
            }
        }
    }

    /// Minimal HTTP/1.1 response; one connection carries one request.
    fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            403 => "Forbidden",
            404 => "Not Found",
            _ => "Error",
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    }

    #[test]
    fn commit_sha_pins_every_raw_request() {
        let stub = start_stub(StubBehavior::sha(SHA_A));
        let (registry, source) = fetch_registry_from(&stub.api_url(), &stub.raw_root()).unwrap();
        assert_eq!(registry.platforms.len(), 1);
        assert_eq!(source, format!("commit {}", &SHA_A[..7]));
        let paths = stub.raw_paths();
        assert!(!paths.is_empty(), "fetch made no raw requests");
        for path in &paths {
            assert!(
                path.starts_with(&format!("/{SHA_A}/")),
                "raw request escaped the pin: {path}"
            );
            assert!(!path.contains("/main/"), "raw request used /main/: {path}");
        }
        assert_eq!(stub.api_hits(), 1);
        stub.stop();
    }

    #[test]
    fn api_403_falls_back_to_branch_main() {
        let stub = start_stub(StubBehavior {
            api_status: 403,
            api_bodies: vec!["rate limited".to_string()],
        });
        assert_falls_back_to_main(stub);
    }

    #[test]
    fn api_500_falls_back_to_branch_main() {
        let stub = start_stub(StubBehavior {
            api_status: 500,
            api_bodies: vec!["boom".to_string()],
        });
        assert_falls_back_to_main(stub);
    }

    #[test]
    fn non_sha_body_falls_back_to_branch_main() {
        let stub = start_stub(StubBehavior::sha("not-a-sha"));
        assert_falls_back_to_main(stub);
    }

    #[test]
    fn uppercase_sha_body_falls_back_to_branch_main() {
        let stub = start_stub(StubBehavior::sha(&SHA_A.to_ascii_uppercase()));
        assert_falls_back_to_main(stub);
    }

    /// The shared fallback assertions: the fetch still succeeds from `/main/`
    /// paths and the source label says the lookup failed.
    fn assert_falls_back_to_main(stub: Stub) {
        let (registry, source) = fetch_registry_from(&stub.api_url(), &stub.raw_root()).unwrap();
        assert_eq!(registry.platforms.len(), 1);
        assert!(
            source.starts_with("branch main (commit lookup failed"),
            "{source}"
        );
        let paths = stub.raw_paths();
        assert!(!paths.is_empty(), "fetch made no raw requests");
        for path in &paths {
            assert!(
                path.starts_with("/main/"),
                "raw request did not use /main/: {path}"
            );
        }
        stub.stop();
    }

    #[test]
    fn refused_commit_lookup_falls_back_to_branch_main() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let stub = start_stub(StubBehavior::sha(SHA_A));
        let api_url = format!("http://127.0.0.1:{port}{API_PATH}");
        let (_, source) = fetch_registry_from(&api_url, &stub.raw_root()).unwrap();
        assert!(
            source.starts_with("branch main (commit lookup failed"),
            "{source}"
        );
        for path in stub.raw_paths() {
            assert!(
                path.starts_with("/main/"),
                "raw request did not use /main/: {path}"
            );
        }
        stub.stop();
    }

    #[test]
    fn one_refresh_resolves_the_commit_once() {
        let stub = start_stub(StubBehavior {
            api_status: 200,
            api_bodies: vec![SHA_A.to_string(), SHA_B.to_string()],
        });
        let (_, source) = fetch_registry_from(&stub.api_url(), &stub.raw_root()).unwrap();
        assert_eq!(stub.api_hits(), 1);
        assert_eq!(source, format!("commit {}", &SHA_A[..7]));
        for path in stub.raw_paths() {
            assert!(path.contains(SHA_A), "raw request lost the pin: {path}");
            assert!(
                !path.contains(SHA_B),
                "raw request used a second sha: {path}"
            );
        }
        stub.stop();
    }

    #[test]
    fn render_text_up_to_date_ends_with_source() {
        let report = RegistryRefreshReport {
            changed: BTreeMap::new(),
            kept: BTreeMap::new(),
            added: Vec::new(),
            updated: Vec::new(),
            deleted: Vec::new(),
            pending: Vec::new(),
            up_to_date: true,
            source: "commit abc1234".to_string(),
        };
        assert_eq!(
            report.render_text(),
            "registry already up to date\nsource: commit abc1234"
        );
    }

    #[test]
    fn render_text_changed_ends_with_source() {
        let report = RegistryRefreshReport {
            changed: BTreeMap::from([("testcli".to_string(), vec!["model_flag".to_string()])]),
            kept: BTreeMap::new(),
            added: Vec::new(),
            updated: Vec::new(),
            deleted: Vec::new(),
            pending: Vec::new(),
            up_to_date: false,
            source: "commit abc1234".to_string(),
        };
        let text = report.render_text();
        assert!(text.contains("testcli: changed model_flag"), "{text}");
        assert!(text.ends_with("\nsource: commit abc1234"), "{text}");
    }
}
