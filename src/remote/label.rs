//! The host label the daemon sends on the relay handshake.
//!
//! The label rides in an HTTP header, so it must be ASCII: anything outside
//! the unreserved set is percent-encoded, and the worker decodes it back
//! (raw labels still read for old workers). The decoded label is truncated
//! to [`MAX_LABEL_CHARS`] characters before sending, so length alone can
//! never earn a `bad_label` rejection.

use crate::domain::canopy_config::CanopyConfig;

/// Longest decoded label the daemon sends, in characters.
pub const MAX_LABEL_CHARS: usize = 64;

/// The decoded label for this host: the configured `host_label`, or the
/// hostname when it is empty, truncated to [`MAX_LABEL_CHARS`] characters.
pub fn host_label(config: &CanopyConfig) -> String {
    if !config.remote.host_label.trim().is_empty() {
        return truncate_label(config.remote.host_label.trim());
    }
    truncate_label(&hostname())
}

/// The machine's hostname, from `$HOSTNAME` or `/etc/hostname`.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|name| name.trim().to_string())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "canopy-host".to_string())
}

/// Truncate a decoded label to [`MAX_LABEL_CHARS`] characters.
pub fn truncate_label(label: &str) -> String {
    label.chars().take(MAX_LABEL_CHARS).collect()
}

/// Percent-encode a label for the handshake header. Unreserved characters
/// (`A-Z a-z 0-9 - _ . ~`) pass through; every other UTF-8 byte becomes an
/// uppercase `%XX` sequence.
pub fn encode_label(label: &str) -> String {
    let mut encoded = String::with_capacity(label.len());
    for byte in label.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreserved_characters_pass_through_untouched() {
        let label = "abcXYZ019-_.~host";
        assert_eq!(encode_label(label), label);
    }

    #[test]
    fn non_ascii_bytes_become_uppercase_percent_sequences() {
        assert_eq!(encode_label("caf\u{e9}-\u{df}"), "caf%C3%A9-%C3%9F");
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_label(&"a".repeat(100)).chars().count(), 64);
        assert_eq!(truncate_label(&"\u{e9}".repeat(100)).chars().count(), 64);
        assert_eq!(truncate_label("short"), "short");
    }
}
