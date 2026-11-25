//! Tests for [`super::signing::verify_release`] and the signature gate in
//! [`super::download_and_extract_with`].
//!
//! The fixtures are signed with a throwaway test key that was generated
//! once and never leaves this file; production verifies against the
//! embedded [`super::signing::PUBLIC_KEY`], whose secret nobody here holds.

use super::signing;

const TEST_PUBLIC_KEY: &str = "RUR0ZXN0a2V5ISFS+NGbeR0kRTJC4V8uq2y3z/p7al7TAJeWDgaYgdsS";
const TAG: &str = "v3.3.0";
const ASSET: &str = "canopy-v3.3.0-x86_64-unknown-linux-gnu.tar.gz";
const ARCHIVE: &[u8] = b"fixture archive bytes for signature tests";
const SUMS: &str = "b6088025ab5e94a257ef3d759436759f5129a88b9d85df2f9d52cc8e1c2d739a  canopy-v3.3.0-x86_64-unknown-linux-gnu.tar.gz\n";
const MINISIG: &str = "untrusted comment: throwaway test key\nRUR0ZXN0a2V5IYeAlrlauw50ehXwZmMIqTQPeLgC5R9+V4fpagcxgtMronnHlUhbfvYUsSp29wSlggL1WG2QsyE0qbbdjqHC5gA=\ntrusted comment: canopy v3.3.0\nNgCPhFL7OLykvikd80TaN4R5yO4a8YJsLDEHlQgtkC6nzdEc34tRe8Gx4k01SZZU2gmIYnSrmZj2iUE3Nd24BQ==";
// Same checksums as `SUMS` but signed over the trusted comment
// `canopy v9.9.9` instead of `canopy v3.3.0`.
const MINISIG_WRONG_COMMENT: &str = "untrusted comment: throwaway test key\nRUR0ZXN0a2V5IYeAlrlauw50ehXwZmMIqTQPeLgC5R9+V4fpagcxgtMronnHlUhbfvYUsSp29wSlggL1WG2QsyE0qbbdjqHC5gA=\ntrusted comment: canopy v9.9.9\nL32JC16OmiSdp2brWhwtp3jOEWSvhq93zGXStvhN2wzhmQNr5p1O5ric3uZJDpsrjvNqZK/rLdMOiLbv8w39Cw==";
// Checksums without an entry for `ASSET`, signed for `canopy v3.3.0`.
const SUMS_NO_ENTRY: &str =
    "f41f3fa625ff120ddca7ef456bf66371ecea23c129f4e4c32367101edb516cf8  some-other-asset.tar.gz\n";
const MINISIG_NO_ENTRY: &str = "untrusted comment: throwaway test key\nRUR0ZXN0a2V5IbydjxpktNFouky8oXtNvpuJ9xRtTGS3BTXZ454dSW1ckPQvmrqEUohFL4OfLghORePC4/WnYv1Z/Rpo/yvHtQE=\ntrusted comment: canopy v3.3.0\nOmNzVTzZkma58D3Wobxbl4f8NCPsjeVvgRpKf3QOdTZUwN4RWKRASU/CsUnc5D+d9rhmv/b+3PNI/d6j+tepDQ==";

#[test]
fn valid_signature_passes() {
    signing::verify_release(TEST_PUBLIC_KEY, SUMS, MINISIG, ARCHIVE, ASSET, TAG).unwrap();
}

#[test]
fn tampered_sums_fails_closed() {
    let tampered = SUMS.replacen('b', "c", 1);
    let error = signing::verify_release(TEST_PUBLIC_KEY, &tampered, MINISIG, ARCHIVE, ASSET, TAG)
        .unwrap_err();
    assert!(
        error.to_string().starts_with("signature invalid:"),
        "tampered sums must fail the signature check: {error}"
    );
}

#[test]
fn tampered_archive_fails_closed() {
    let mut tampered = ARCHIVE.to_vec();
    tampered[0] ^= 0x01;
    let error =
        signing::verify_release(TEST_PUBLIC_KEY, SUMS, MINISIG, &tampered, ASSET, TAG).unwrap_err();
    assert!(
        error.to_string().starts_with("checksum mismatch:"),
        "tampered archive must fail the checksum check: {error}"
    );
}

#[test]
fn wrong_trusted_comment_fails_closed() {
    let error = signing::verify_release(
        TEST_PUBLIC_KEY,
        SUMS,
        MINISIG_WRONG_COMMENT,
        ARCHIVE,
        ASSET,
        TAG,
    )
    .unwrap_err();
    assert!(
        error.to_string().starts_with("trusted comment mismatch:"),
        "wrong trusted comment must fail the tag binding: {error}"
    );
}

#[test]
fn missing_checksum_entry_fails_closed() {
    let error = signing::verify_release(
        TEST_PUBLIC_KEY,
        SUMS_NO_ENTRY,
        MINISIG_NO_ENTRY,
        ARCHIVE,
        ASSET,
        TAG,
    )
    .unwrap_err();
    assert!(
        error.to_string().starts_with("checksum entry missing:"),
        "checksums without the asset must fail: {error}"
    );
}

#[test]
fn gate_below_signed_releases_warns_only() {
    assert!(!signing::is_signature_mandatory("v3.2.0"));
    assert!(!signing::is_signature_mandatory("v3.2.9"));
    assert!(signing::is_signature_mandatory("v3.3.0"));
    assert!(signing::is_signature_mandatory("v4.0.0"));
}

/// Downloader serving an archive but no signature files, like a release
/// that predates signing.
struct UnsignedDownloader {
    archive: Vec<u8>,
}

impl super::BinaryDownloader for UnsignedDownloader {
    fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        if url.ends_with(".minisig") || url.ends_with("SHA256SUMS.txt") {
            anyhow::bail!("404");
        }
        Ok(self.archive.clone())
    }
}

fn canopy_tar_bytes(contents: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "canopy", contents)
        .unwrap();
    let tar_bytes = archive.into_inner().unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &tar_bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn missing_signature_below_gate_installs_with_warning() {
    let downloader = UnsignedDownloader {
        archive: canopy_tar_bytes(b"new"),
    };
    let output = tempfile::NamedTempFile::new().unwrap();
    assert!(super::download_and_extract_with(
        &downloader,
        "v3.2.0",
        "x86_64-unknown-linux-gnu",
        output.path()
    )
    .unwrap());
    assert_eq!(std::fs::read(output.path()).unwrap(), b"new");
}

#[test]
fn missing_signature_at_gate_aborts() {
    let downloader = UnsignedDownloader {
        archive: canopy_tar_bytes(b"new"),
    };
    let output = tempfile::NamedTempFile::new().unwrap();
    let error = super::download_and_extract_with(
        &downloader,
        "v3.3.0",
        "x86_64-unknown-linux-gnu",
        output.path(),
    )
    .unwrap_err();
    assert!(
        error.to_string().starts_with("signature missing:"),
        "missing signature at the gate must abort: {error}"
    );
}
