//! Shared client for `POST /admin/remote` — the token-authenticated route
//! behind the daemon's bearer-token gate that serves the four privileged
//! `remote` actions (pair, set_enabled, revoke, trust_add).
//!
//! Both the CLI and the TUI call [`admin_remote_post`]: it builds the
//! request, sends it over a plain blocking TCP connection (the same shape
//! as the TUI's MCP client, but a single unauthenticated-session POST —
//! the daemon token in `Authorization` is the only auth), and applies the
//! route's response contract. The token value is never logged.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::sessions::auth::{daemon_auth_header, token_path};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The daemon is not reachable at all: the CLI prints this and exits
/// non-zero, with no local fallback.
pub const DAEMON_DOWN_MESSAGE: &str =
    "the daemon is not running: start it with 'canopy daemon start'";

/// The daemon token when its file already exists, read the way the TUI's
/// MCP client reads it: best-effort and read-only, re-read on every
/// request so a rotated token takes effect. Never logged.
fn daemon_token() -> Option<String> {
    let data_dir = dirs::home_dir().map(|home| home.join(".canopy"))?;
    let token = std::fs::read_to_string(token_path(&data_dir))
        .ok()?
        .trim()
        .to_string();
    (!token.is_empty()).then_some(token)
}

/// POST `body` to `/admin/remote` on `port` and apply the route's response
/// contract: `200` returns the unwrapped JSON value; any other status
/// returns the `error` field as the message; a refused connection reports
/// the daemon is down.
pub fn admin_remote_post(
    port: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::to_string(body)
        .map_err(|error| format!("cannot encode admin request: {error}"))?;
    let (status, response_body) = post_admin(port, &payload, daemon_token().as_deref())?;
    if status == 200 {
        return serde_json::from_str(&response_body)
            .map_err(|_| "daemon returned an unreadable admin response".to_string());
    }
    Err(admin_error(status, &response_body))
}

/// POST to `/admin/remote` with an explicit token: the seam the tests drive
/// without touching `$HOME`. Production callers go through
/// [`admin_remote_post`], which supplies [`daemon_token`]. A `None` token
/// sends no header — best-effort, never a fabricated credential.
fn post_admin(port: &str, payload: &str, token: Option<&str>) -> Result<(u16, String), String> {
    let auth_header = token
        .map(|token| {
            let (name, value) = daemon_auth_header(token);
            format!("{name}: {}\r\n", value.to_str().unwrap_or("Bearer invalid"))
        })
        .unwrap_or_default();
    let request = format!(
        "POST /admin/remote HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         {auth_header}Content-Length: {}\r\n\
         \r\n\
         {payload}",
        payload.len()
    );
    let addr = format!("127.0.0.1:{port}");
    let mut stream = TcpStream::connect_timeout(
        &addr.parse().map_err(|_| DAEMON_DOWN_MESSAGE.to_string())?,
        CONNECT_TIMEOUT,
    )
    .map_err(|_| DAEMON_DOWN_MESSAGE.to_string())?;
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| format!("admin request failed: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("admin request failed: {error}"))?;
    let mut raw = Vec::new();
    // `Connection: close` is sent with every request, so EOF marks the end
    // of the response. A timeout mid-read still leaves a parseable prefix.
    let _ = stream.read_to_end(&mut raw);
    parse_response(&raw)
}

/// Split one raw HTTP response into its status and body.
fn parse_response(raw: &[u8]) -> Result<(u16, String), String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    Ok((
        status,
        String::from_utf8_lossy(&raw[split + 4..]).into_owned(),
    ))
}

/// The message for a non-`200` status: the route's `error` field when the
/// body carries one, otherwise the status and the raw body.
fn admin_error(status: u16, body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim()) {
        if let Some(error) = value.get("error").and_then(|error| error.as_str()) {
            return error.to_string();
        }
    }
    format!("admin request failed with HTTP {status}: {}", body.trim())
}

/// A fake `POST /admin/remote` server for the client and CLI tests: it
/// records every request (line, `Authorization` header, body) and answers
/// every one with the canned status and body.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// One recorded admin request.
    #[derive(Debug, Clone)]
    pub(crate) struct RecordedAdminRequest {
        /// For example `POST /admin/remote HTTP/1.1`.
        pub request_line: String,
        /// The `Authorization` header, if the caller sent one.
        pub authorization: Option<String>,
        /// The raw request body.
        pub body: String,
    }

    pub(crate) struct FakeAdmin {
        pub port: String,
        pub requests: Arc<Mutex<Vec<RecordedAdminRequest>>>,
    }

    impl FakeAdmin {
        pub(crate) fn recorded(&self) -> Vec<RecordedAdminRequest> {
            self.requests.lock().expect("requests").clone()
        }
    }

    /// A localhost port with nothing listening on it.
    pub(crate) fn unused_port() -> String {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind probe")
            .local_addr()
            .expect("port")
            .port()
            .to_string()
    }

    /// Answer every admin POST with `status` and `body`.
    pub(crate) fn spawn_fake_admin(status: u16, body: &str) -> FakeAdmin {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake admin");
        let port = listener.local_addr().expect("port").port().to_string();
        let requests: Arc<Mutex<Vec<RecordedAdminRequest>>> = Arc::default();
        let canned = body.to_string();
        let recorded = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                handle_one(stream, &recorded, status, &canned);
            }
        });
        FakeAdmin { port, requests }
    }

    fn handle_one(
        mut stream: TcpStream,
        recorded: &Arc<Mutex<Vec<RecordedAdminRequest>>>,
        status: u16,
        body: &str,
    ) {
        let mut raw = Vec::new();
        let mut buf = [0u8; 1024];
        let (request_line, authorization, request_body) = loop {
            if let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&raw[..split]).into_owned();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if raw.len() >= split + 4 + content_length {
                    let request_body =
                        String::from_utf8_lossy(&raw[split + 4..split + 4 + content_length])
                            .into_owned();
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    let authorization = head.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("authorization")
                            .then(|| value.trim().to_string())
                    });
                    break (request_line, authorization, request_body);
                }
            }
            let Ok(n) = stream.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            raw.extend_from_slice(&buf[..n]);
        };
        recorded
            .lock()
            .expect("requests")
            .push(RecordedAdminRequest {
                request_line,
                authorization,
                body: request_body,
            });
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            422 => "Unprocessable Entity",
            _ => "Error",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{spawn_fake_admin, unused_port};
    use super::{admin_remote_post, post_admin, DAEMON_DOWN_MESSAGE};

    #[test]
    fn ok_returns_the_unwrapped_value() {
        let fake = spawn_fake_admin(200, r#"{"revoked":"bfcba2b742a807bb"}"#);
        let value = admin_remote_post(&fake.port, &serde_json::json!({ "confirm": true }))
            .expect("200 returns the value");
        assert_eq!(value, serde_json::json!({ "revoked": "bfcba2b742a807bb" }));
    }

    #[test]
    fn unprocessable_returns_the_error_text() {
        let fake = spawn_fake_admin(422, r#"{"error":"no paired device with id x"}"#);
        let error = admin_remote_post(&fake.port, &serde_json::json!({})).expect_err("422 errors");
        assert_eq!(error, "no paired device with id x");
    }

    #[test]
    fn bad_request_returns_the_error_text() {
        let fake = spawn_fake_admin(400, r#"{"error":"confirm required"}"#);
        let error = admin_remote_post(&fake.port, &serde_json::json!({})).expect_err("400 errors");
        assert_eq!(error, "confirm required");
    }

    #[test]
    fn unauthorized_surfaces_the_gate_error() {
        let fake = spawn_fake_admin(401, r#"{"error":"missing or invalid daemon token"}"#);
        let error = admin_remote_post(&fake.port, &serde_json::json!({})).expect_err("401 errors");
        assert_eq!(error, "missing or invalid daemon token");
    }

    #[test]
    fn refused_connection_reports_the_daemon_is_down() {
        let error =
            admin_remote_post(&unused_port(), &serde_json::json!({})).expect_err("refused errors");
        assert_eq!(error, DAEMON_DOWN_MESSAGE);
    }

    /// The CLI-shaped request carries the daemon `Authorization` header and
    /// the body's `confirm: true` verbatim.
    #[test]
    fn request_carries_the_auth_header_and_confirm() {
        let fake = spawn_fake_admin(200, r#"{"enabled":true}"#);
        let payload =
            serde_json::json!({ "action": "set_enabled", "enabled": true, "confirm": true });
        let (status, _) = post_admin(&fake.port, &payload.to_string(), Some("client-test-token"))
            .expect("post against the fake admin");
        assert_eq!(status, 200);
        let requests = fake.recorded();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(requests[0].request_line, "POST /admin/remote HTTP/1.1");
        assert!(
            requests[0]
                .authorization
                .as_deref()
                .is_some_and(|header| header.starts_with("Bearer ") && header.len() > 7),
            "the admin POST carries the daemon bearer header: {requests:?}"
        );
        let sent: serde_json::Value =
            serde_json::from_str(&requests[0].body).expect("the body is JSON");
        assert_eq!(sent, payload, "the body arrives verbatim, confirm included");
    }

    #[test]
    fn no_token_sends_no_authorization_header() {
        let fake = spawn_fake_admin(200, r#"{}"#);
        post_admin(&fake.port, r#"{"confirm":true}"#, None).expect("post without a token");
        let requests = fake.recorded();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert_eq!(requests[0].authorization, None);
    }
}
