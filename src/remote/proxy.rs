//! `CONNECT` tunneling for the relay socket behind a corporate proxy.
//!
//! The relay client dials direct when no proxy is set. When `HTTPS_PROXY`
//! (or `ALL_PROXY` as a fallback) is set, the socket is opened through the
//! proxy with an HTTP `CONNECT` tunnel, authenticating with the proxy URL's
//! userinfo when present. Errors name the proxy host only: credentials are
//! never logged or returned.

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::relay::{ConnectError, WsStream};

/// Proxy URL from the environment: `HTTPS_PROXY` wins over `ALL_PROXY`,
/// uppercase before lowercase; an empty value counts as unset.
pub fn proxy_url() -> Option<String> {
    ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Split a `ws(s)` URL into its `(host, port)`: `ws` defaults to 80,
/// `wss` to 443.
pub fn ws_host_port(url: &str) -> (String, u16) {
    let (rest, default_port) = if let Some(rest) = url.strip_prefix("wss://") {
        (rest, 443)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (rest, 80)
    } else {
        (url, 80)
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    split_host_port(host_port, default_port)
}

/// Split one `host[:port]` authority, keeping `IPv6` brackets on the host.
fn split_host_port(authority: &str, default_port: u16) -> (String, u16) {
    if let Some(inner) = authority.strip_prefix('[') {
        let Some(end) = inner.find(']') else {
            return (authority.to_string(), default_port);
        };
        let host = &authority[..=end];
        let port = inner[end + 1..]
            .strip_prefix(':')
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default_port);
        return (host.to_string(), port);
    }
    match authority.rsplit_once(':') {
        Some((host, raw)) if !host.is_empty() => match raw.parse() {
            Ok(port) => (host.to_string(), port),
            Err(_) => (authority.to_string(), default_port),
        },
        _ => (authority.to_string(), default_port),
    }
}

/// A parsed proxy URL: `http://[user:pass@]host[:port]`.
struct ProxyTarget {
    /// Proxy host without credentials or port.
    host: String,
    /// Proxy port, 8080 when the URL carries none.
    port: u16,
    /// Raw `user:pass` userinfo, when the URL carries one.
    userinfo: Option<String>,
}

/// Parse `http://[user:pass@]host[:port]`; `None` when there is no host.
fn parse_proxy_url(proxy: &str) -> Option<ProxyTarget> {
    let without_scheme = proxy
        .strip_prefix("http://")
        .or_else(|| proxy.strip_prefix("https://"))
        .unwrap_or(proxy);
    let authority = without_scheme.split('/').next()?;
    if authority.is_empty() {
        return None;
    }
    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((credentials, host_port)) => (Some(credentials.to_string()), host_port),
        None => (None, authority),
    };
    if host_port.is_empty() {
        return None;
    }
    let (host, port) = split_host_port(host_port, 8080);
    if host.is_empty() {
        return None;
    }
    Some(ProxyTarget {
        host,
        port,
        userinfo,
    })
}

/// The proxy URL with any userinfo stripped, for error messages.
fn redact(proxy: &str) -> String {
    match proxy.rsplit_once('@') {
        Some((_, host)) => format!("...@{host}"),
        None => proxy.to_string(),
    }
}

/// Open a `CONNECT host:port` tunnel through `proxy`, authenticating with
/// the proxy URL's userinfo when present. Every error names the proxy host
/// only, never credentials.
pub async fn tunnel_stream(proxy: &str, host: &str, port: u16) -> Result<TcpStream, String> {
    let Some(target) = parse_proxy_url(proxy) else {
        return Err(format!("unparseable proxy URL {}", redact(proxy)));
    };
    let mut stream = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|error| format!("proxy {} unreachable: {error}", target.host))?;
    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some(userinfo) = &target.userinfo {
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            userinfo.as_bytes(),
        );
        request.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("proxy {} write failed: {error}", target.host))?;
    let status = read_connect_status(&mut stream, &target.host).await?;
    if !status.contains(" 200 ") && !status.ends_with(" 200") {
        return Err(format!(
            "proxy {} refused CONNECT to {host}:{port}: {status}",
            target.host
        ));
    }
    Ok(stream)
}

/// Dial through a `CONNECT` tunnel, then run the same handshake over it.
pub async fn connect_via_proxy(
    request: tokio_tungstenite::tungstenite::handshake::client::Request,
    url: &str,
    proxy: &str,
) -> Result<WsStream, ConnectError> {
    let (host, port) = ws_host_port(url);
    let stream = tunnel_stream(proxy, &host, port)
        .await
        .map_err(ConnectError::Transient)?;
    match tokio_tungstenite::client_async_tls(request, stream).await {
        Ok((stream, _response)) => Ok(stream),
        Err(error) => Err(classify_handshake_error(error)),
    }
}

/// A 401 stops the token and a 400 parks the label; anything else may work
/// on a later attempt. Shared by the direct and the proxied dial.
pub fn classify_handshake_error(error: tokio_tungstenite::tungstenite::Error) -> ConnectError {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401 => {
            ConnectError::Unauthorized
        }
        tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 400 => {
            ConnectError::BadRequest(rejection_message(&response))
        }
        error => ConnectError::Transient(error.to_string()),
    }
}

/// One-line handshake rejection for the status surface: the HTTP 400 body,
/// lossy and capped at 120 characters, so a `bad_label` reason arrives
/// intact without flooding `remote_error`.
fn rejection_message(
    response: &tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> String {
    let body = response.body().as_deref().unwrap_or_default();
    let text: String = String::from_utf8_lossy(body).chars().take(120).collect();
    format!("relay rejected the handshake: HTTP 400 {text}")
}

/// Read the `CONNECT` reply head up to the blank line, returning its status
/// line. Nothing past the head is consumed: the tunnel is byte-transparent
/// from there, so no handshake bytes may be swallowed here.
async fn read_connect_status(stream: &mut TcpStream, proxy_host: &str) -> Result<String, String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if head.len() > 8192 {
            return Err(format!(
                "proxy {proxy_host} sent an oversized CONNECT reply"
            ));
        }
        let size = stream.read(&mut chunk).await.map_err(|error| {
            if head.is_empty() {
                format!("proxy {proxy_host} closed the CONNECT: {error}")
            } else {
                format!("proxy {proxy_host} CONNECT reply truncated: {error}")
            }
        })?;
        if size == 0 {
            return Err(format!("proxy {proxy_host} closed the CONNECT"));
        }
        head.extend_from_slice(&chunk[..size]);
        if head.windows(4).any(|word| word == b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string())
}
