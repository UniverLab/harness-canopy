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
        let host = &authority[..=end + 1];
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

/// The proxy host as a dial target: `ToSocketAddrs` takes a bare IPv6
/// literal, so brackets come off here. The `CONNECT` line keeps them.
fn dial_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
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
    let mut stream = TcpStream::connect((dial_host(&target.host), target.port))
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
/// line. The reply carries no body, so the tunnel is byte-transparent from
/// the blank line: the handshake goes out only after this returns.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_host_port_defaults_port_by_scheme() {
        assert_eq!(
            ws_host_port("wss://relay.example.com/ws/host"),
            ("relay.example.com".to_string(), 443)
        );
        assert_eq!(
            ws_host_port("ws://relay.example.com/ws/host"),
            ("relay.example.com".to_string(), 80)
        );
    }

    #[test]
    fn ws_host_port_keeps_explicit_port_and_strips_userinfo() {
        assert_eq!(
            ws_host_port("ws://user:pass@127.0.0.1:9000/ws/host"),
            ("127.0.0.1".to_string(), 9000)
        );
    }

    #[test]
    fn ws_host_port_keeps_ipv6_brackets() {
        assert_eq!(
            ws_host_port("wss://[2001:db8::1]/ws/host"),
            ("[2001:db8::1]".to_string(), 443)
        );
        assert_eq!(
            ws_host_port("wss://[2001:db8::1]:8443/ws/host"),
            ("[2001:db8::1]".to_string(), 8443)
        );
    }

    #[test]
    fn parse_proxy_url_extracts_userinfo_and_defaults_port() {
        let target = parse_proxy_url("http://user:pass@proxy.example.com").expect("proxy");
        assert_eq!(target.host, "proxy.example.com");
        assert_eq!(target.port, 8080);
        assert_eq!(target.userinfo.as_deref(), Some("user:pass"));
    }

    #[test]
    fn parse_proxy_url_accepts_bare_host_and_rejects_empty() {
        let target = parse_proxy_url("proxy.example.com:3128").expect("proxy");
        assert_eq!(target.host, "proxy.example.com");
        assert_eq!(target.port, 3128);
        assert!(target.userinfo.is_none());
        assert!(parse_proxy_url("http://").is_none());
        assert!(parse_proxy_url("").is_none());
    }

    #[test]
    fn redact_strips_userinfo_but_keeps_host() {
        assert_eq!(
            redact("http://user:pass@proxy.example.com:8080"),
            "...@proxy.example.com:8080"
        );
        assert_eq!(
            redact("http://proxy.example.com:8080"),
            "http://proxy.example.com:8080"
        );
    }

    #[test]
    fn dial_host_strips_ipv6_brackets() {
        assert_eq!(dial_host("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(dial_host("proxy.example.com"), "proxy.example.com");
    }
}
