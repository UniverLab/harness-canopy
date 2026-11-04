//! `CONNECT` tunneling for the relay socket behind a corporate proxy.
//!
//! The relay client dials direct when no proxy is set. Otherwise the socket
//! is opened through the proxy with an HTTP `CONNECT` tunnel: `ws://`
//! destinations use `HTTP_PROXY`, `wss://` use `HTTPS_PROXY`, `ALL_PROXY`
//! is the fallback for both, and `NO_PROXY` bypasses matching hosts.
//! Errors name the proxy host only: credentials are never logged or
//! returned.

use std::net::IpAddr;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::relay::{ConnectError, WsStream};

/// Proxy URL for `dest` from `env`: `ws://` reads `HTTP_PROXY` first,
/// `wss://` (and anything else) reads `HTTPS_PROXY` first, uppercase
/// before lowercase, then `ALL_PROXY` as the fallback; an empty value
/// counts as unset. Destinations matching `NO_PROXY` bypass the proxy.
/// Production passes `std::env::var`; tests inject a map.
pub fn proxy_url_for(dest: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let (host, _) = ws_host_port(dest);
    let no_proxy = ["NO_PROXY", "no_proxy"]
        .into_iter()
        .filter_map(env)
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty());
    if let Some(raw) = no_proxy {
        if no_proxy_bypassed(&host, &raw) {
            return None;
        }
    }
    let scheme_keys: [&str; 2] = if dest.starts_with("ws://") {
        ["HTTP_PROXY", "http_proxy"]
    } else {
        ["HTTPS_PROXY", "https_proxy"]
    };
    scheme_keys
        .into_iter()
        .chain(["ALL_PROXY", "all_proxy"])
        .filter_map(env)
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Production wrapper: `proxy_url_for` reading the process environment.
/// Honors `HTTP(S)_PROXY`/`ALL_PROXY` plus `NO_PROXY` for `dest`.
pub fn proxy_url_for_dest(dest: &str) -> Option<String> {
    proxy_url_for(dest, &|key| std::env::var(key).ok())
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

/// Whether `host` matches any entry of a raw `NO_PROXY` list: entries are
/// comma-separated, surrounding whitespace is ignored, and empty entries
/// match nothing.
fn no_proxy_bypassed(host: &str, raw: &str) -> bool {
    raw.split(',').any(|entry| {
        let entry = entry.trim();
        !entry.is_empty() && entry_matches(host, entry)
    })
}

/// One `NO_PROXY` entry against `host`: `*` bypasses all; otherwise an
/// exact hostname, a domain suffix (a leading dot is optional), an IP
/// literal, or a CIDR range. Hostnames match case-insensitively; ports
/// and IPv6 brackets on either side are ignored.
fn entry_matches(host: &str, entry: &str) -> bool {
    let entry = entry.trim();
    if entry == "*" {
        return true;
    }
    let host = strip_brackets(host.trim());
    if let Some((network, prefix)) = parse_cidr(entry) {
        return host
            .parse::<IpAddr>()
            .is_ok_and(|ip| cidr_contains(network, prefix, ip));
    }
    if let Some(entry_ip) = parse_ip_entry(entry) {
        return host.parse::<IpAddr>().is_ok_and(|ip| ip == entry_ip);
    }
    let want = strip_entry_port(entry).to_lowercase();
    let want = want.strip_prefix('.').unwrap_or(&want);
    if want.is_empty() {
        return false;
    }
    let host = host.to_lowercase();
    host == want || host.ends_with(&format!(".{want}"))
}

/// Strip one `[...]` IPv6 bracket pair; anything else passes through.
fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// An entry's host part without any `:port`: bracketed literals cut at
/// the closing bracket, bare IP literals keep their colons, and anything
/// else drops a trailing numeric `:port`.
fn strip_entry_port(entry: &str) -> &str {
    if let Some(rest) = entry.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    if entry.parse::<IpAddr>().is_ok() {
        return entry;
    }
    match entry.rsplit_once(':') {
        Some((bare, port))
            if !bare.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            bare
        }
        _ => entry,
    }
}

/// An entry as an IP literal, ignoring brackets and any port.
fn parse_ip_entry(entry: &str) -> Option<IpAddr> {
    strip_entry_port(entry).parse().ok()
}

/// Split `address/prefix` into its network and prefix length; `None` when
/// either half is unparseable or the prefix exceeds the address width.
fn parse_cidr(entry: &str) -> Option<(IpAddr, u8)> {
    let (address, prefix) = entry.split_once('/')?;
    let network: IpAddr = strip_entry_port(address.trim()).parse().ok()?;
    let prefix: u8 = prefix.trim().parse().ok()?;
    let width = match network {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    (prefix <= width).then_some((network, prefix))
}

/// Whether `host` falls inside `network/prefix`; mixed families match
/// nothing, and a zero prefix matches its whole family.
fn cidr_contains(network: IpAddr, prefix: u8, host: IpAddr) -> bool {
    match (network, host) {
        (IpAddr::V4(network), IpAddr::V4(host)) => {
            if prefix == 0 {
                return true;
            }
            let shift = 32 - prefix;
            u32::from(network) >> shift == u32::from(host) >> shift
        }
        (IpAddr::V6(network), IpAddr::V6(host)) => {
            if prefix == 0 {
                return true;
            }
            let shift = 128 - prefix;
            u128::from(network) >> shift == u128::from(host) >> shift
        }
        _ => false,
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

    fn test_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn no_proxy_exact_host_bypassed() {
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "Relay.Example.COM"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com:443/ws/host", &env),
            None,
            "exact host matches case-insensitively, dest port ignored"
        );
        let env = test_env(&[
            ("HTTP_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "127.0.0.1"),
        ]);
        assert_eq!(
            proxy_url_for("ws://127.0.0.1:8787/ws/host", &env),
            None,
            "IPv4 literal bypasses, dest port ignored"
        );
        let env = test_env(&[
            ("HTTP_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "2001:db8::1"),
        ]);
        assert_eq!(
            proxy_url_for(
                "ws://[2001:0db8:0000:0000:0000:0000:0000:0001]/ws/host",
                &env
            ),
            None,
            "IPv6 literal matches across spellings, brackets ignored"
        );
        let env = test_env(&[
            ("HTTP_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "127.0.0.1:8787"),
        ]);
        assert_eq!(
            proxy_url_for("ws://127.0.0.1:8787/ws/host", &env),
            None,
            "entry port ignored on IP literals"
        );
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "relay.example.com:443"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com:443/ws/host", &env),
            None,
            "entry port ignored on hostnames"
        );
        let env = test_env(&[
            ("HTTP_PROXY", "http://proxy:8080"),
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "127.0.0.1"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://proxy:8080".to_string()),
            "an IP entry never matches a hostname"
        );
        assert_eq!(
            proxy_url_for("ws://127.0.0.2/ws/host", &env),
            Some("http://proxy:8080".to_string()),
            "a different IP literal uses the proxy"
        );
    }

    #[test]
    fn no_proxy_suffix_bypassed() {
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("NO_PROXY", ".example.com"),
        ]);
        assert_eq!(
            proxy_url_for("wss://api.example.com/ws/host", &env),
            None,
            "leading-dot suffix matches subdomains"
        );
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("no_proxy", "Example.COM"),
        ]);
        assert_eq!(
            proxy_url_for("wss://api.example.com/ws/host", &env),
            None,
            "bare suffix matches subdomains case-insensitively"
        );
        assert_eq!(
            proxy_url_for("wss://notexample.com/ws/host", &env),
            Some("http://proxy:8080".to_string()),
            "a non-dot-boundary host is not a subdomain"
        );
    }

    #[test]
    fn no_proxy_cidr_bypassed() {
        let env = test_env(&[
            ("HTTP_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "10.0.0.0/8, 2001:db8::/32"),
        ]);
        assert_eq!(
            proxy_url_for("ws://10.1.2.3/ws/host", &env),
            None,
            "IPv4 host inside the CIDR range bypasses"
        );
        assert_eq!(
            proxy_url_for("ws://[2001:db8::1]/ws/host", &env),
            None,
            "IPv6 host inside the CIDR range bypasses"
        );
        assert_eq!(
            proxy_url_for("ws://11.0.0.1/ws/host", &env),
            Some("http://proxy:8080".to_string()),
            "IPv4 host outside the CIDR range uses the proxy"
        );
    }

    #[test]
    fn no_proxy_star_bypasses_all() {
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("ALL_PROXY", "http://all:8080"),
            ("NO_PROXY", "*"),
        ]);
        assert_eq!(
            proxy_url_for("wss://anything.example.org/ws/host", &env),
            None
        );
    }

    #[test]
    fn no_proxy_mismatch_uses_proxy() {
        let env = test_env(&[
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("NO_PROXY", "other.com, 192.168.0.0/16"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://proxy:8080".to_string())
        );
    }

    #[test]
    fn ws_uses_http_proxy() {
        let env = test_env(&[
            ("HTTP_PROXY", "http://http-proxy:8080"),
            ("HTTPS_PROXY", "http://https-proxy:8080"),
        ]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            Some("http://http-proxy:8080".to_string()),
            "ws:// reads HTTP_PROXY even when HTTPS_PROXY is set"
        );
        let env = test_env(&[("HTTPS_PROXY", "http://https-proxy:8080")]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            None,
            "ws:// ignores HTTPS_PROXY"
        );
        let env = test_env(&[("http_proxy", "http://lower-proxy:8080")]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            Some("http://lower-proxy:8080".to_string()),
            "ws:// reads lowercase http_proxy"
        );
    }

    #[test]
    fn wss_uses_https_proxy() {
        let env = test_env(&[
            ("HTTP_PROXY", "http://http-proxy:8080"),
            ("HTTPS_PROXY", "http://https-proxy:8080"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://https-proxy:8080".to_string()),
            "wss:// reads HTTPS_PROXY even when HTTP_PROXY is set"
        );
        let env = test_env(&[("HTTP_PROXY", "http://http-proxy:8080")]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            None,
            "wss:// ignores HTTP_PROXY"
        );
    }

    #[test]
    fn all_proxy_fallback() {
        let env = test_env(&[("ALL_PROXY", "http://all:8080")]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            Some("http://all:8080".to_string())
        );
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://all:8080".to_string())
        );
        let env = test_env(&[
            ("HTTPS_PROXY", "http://https-proxy:8080"),
            ("ALL_PROXY", "http://all:8080"),
        ]);
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://https-proxy:8080".to_string()),
            "scheme proxy wins over ALL_PROXY"
        );
        let env = test_env(&[("all_proxy", "http://all-lower:8080")]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            Some("http://all-lower:8080".to_string())
        );
        assert_eq!(
            proxy_url_for("wss://relay.example.com/ws/host", &env),
            Some("http://all-lower:8080".to_string()),
            "lowercase all_proxy is the fallback for both schemes"
        );
        let env = test_env(&[
            ("ALL_PROXY", "http://all:8080"),
            ("all_proxy", "http://all-lower:8080"),
        ]);
        assert_eq!(
            proxy_url_for("ws://relay.example.com/ws/host", &env),
            Some("http://all:8080".to_string()),
            "uppercase ALL_PROXY wins over lowercase"
        );
    }
}
