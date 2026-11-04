//! Part 02 of `tests` (split from `src/domain/cli_strategy.rs`, pure move).
use super::*;

#[test]
fn verify_identity_passes_when_output_contains_expected_substring() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "bb", "echo 'Blackbox CLI v1.2.3'\n");
    let cli = identity_cli(
        &script.to_string_lossy(),
        Some("--version"),
        Some("Blackbox"),
    );
    assert!(verify_identity(&cli, &script).is_ok());
}

#[test]
fn verify_identity_fails_when_output_missing_substring_reports_wrong_binary_with_path() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(
        &dir,
        "blackbox",
        "echo \"blackbox: another window manager is already running on display ':0'\"\n",
    );
    let cli = identity_cli(
        &script.to_string_lossy(),
        Some("--version"),
        Some("Blackbox CLI"),
    );
    let err = verify_identity(&cli, &script).unwrap_err();
    assert_eq!(err.resolved, script);
    assert_eq!(err.expected, "Blackbox CLI");
    assert!(
        err.output.contains("another window manager"),
        "error must carry the wrong program's output as evidence: {}",
        err.output
    );
    let report = err.report(&cli.binary, "--version");
    assert!(
        report.contains(&script.to_string_lossy().to_string()),
        "report must name the resolved absolute path: {report}"
    );
    assert!(report.contains("does not identify as the blackbox CLI"));
}

#[test]
fn verify_identity_skipped_when_check_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(
        &dir,
        "whatever",
        "echo 'another window manager is already running'\n",
    );
    let cli = identity_cli(&script.to_string_lossy(), None, None);
    assert!(verify_identity(&cli, &script).is_ok());
}

#[test]
fn verify_identity_uses_absolute_path_directly_without_path_search() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "real-bb", "echo 'Blackbox CLI'\n");
    let cli = identity_cli(
        &script.to_string_lossy(),
        Some("--version"),
        Some("blackbox"),
    );
    // Absolute binary is used as-is; no PATH lookup happens.
    assert!(verify_identity(&cli, &script).is_ok());
}

#[test]
fn verify_identity_matches_case_insensitively() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_identity_script(&dir, "bb", "echo 'BLACKBOX cli'\n");
    let cli = identity_cli(
        &script.to_string_lossy(),
        Some("--version"),
        Some("blackbox"),
    );
    assert!(verify_identity(&cli, &script).is_ok());
}

#[test]
fn verify_identity_truncates_long_output_but_keeps_evidence() {
    let long = "x".repeat(IDENTITY_CHECK_OUTPUT_LIMIT + 1000);
    let truncated = truncate_identity_output(&long);
    assert!(truncated.len() <= IDENTITY_CHECK_OUTPUT_LIMIT + 3);
    assert!(truncated.starts_with("xxx"));
}

/// CB44 constraint 3: the identity check is diagnosis-only
/// (probe/doctor), never a per-dispatch tax. Dispatch builds every
/// command through `CliStrategy::from_cli_config`, which deliberately
/// drops `identity_check` — and neither the executor nor the graph
/// engine may reference the check or its helper directly. Greps their
/// production code the way doctor's glyph test greps its own.
#[test]
fn agent_dispatch_never_runs_identity_check() {
    for (name, source) in [
        ("executor", include_str!("../../../executor/mod.rs")),
        ("graph_engine", include_str!("../../../graph_engine/mod.rs")),
    ] {
        let production_code = source.split("mod tests {").next().unwrap_or(source);
        for marker in ["verify_identity", "identity_check"] {
            assert!(
                !production_code.contains(marker),
                "CB44: `{marker}` must not appear in {name} production code — \
                     identity verification is diagnosis-only (probe/doctor)"
            );
        }
    }
}

#[tokio::test]
async fn verify_identity_async_passes_and_fails_like_sync() {
    let dir = tempfile::tempdir().unwrap();
    let good = write_identity_script(&dir, "good", "echo 'Blackbox CLI'\n");
    let bad = write_identity_script(
        &dir,
        "bad",
        "echo 'another window manager is already running'\n",
    );
    let good_cli = identity_cli(&good.to_string_lossy(), Some("--version"), Some("Blackbox"));
    let bad_cli = identity_cli(
        &bad.to_string_lossy(),
        Some("--version"),
        Some("Blackbox CLI"),
    );
    assert!(verify_identity_async(&good_cli, &good).await.is_ok());
    let err = verify_identity_async(&bad_cli, &bad).await.unwrap_err();
    assert_eq!(err.resolved, bad);
    assert!(err.output.contains("another window manager"));
}
