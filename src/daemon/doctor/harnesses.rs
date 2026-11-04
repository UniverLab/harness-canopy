//! Doctor's harness section (B40): per-harness inspection, the installed
//! summary line, and the per-CLI resolution report.
//!
//! A harness whose binary is gone but that something still references (crew
//! member, graph node, scheduled agent) is reported once as an actionable
//! warning naming the references — never with a PATH dump. An uninstalled,
//! unreferenced harness does not appear at all: pruning removes it on its
//! own, so doctor has nothing to say about it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::daemon::doctor::service_unit::{
    binary_is_executable, binary_version, diagnose_binary_identity, diagnose_cli_identity,
    doctor_cli_label,
};
use crate::domain::cli_strategy::ResolutionStep;

/// One configured harness, inspected.
pub(super) struct HarnessEntry {
    /// Registry slug, e.g. `qwen`.
    pub name: String,
    /// Display label (product name + slug when they differ).
    pub label: String,
    pub state: HarnessState,
    /// Live reference labels (`crew lab-general member 3`, ...). Empty
    /// when nothing references this harness.
    pub references: Vec<String>,
}

/// The verdict of inspecting one harness's binary.
pub(super) enum HarnessState {
    /// The binary resolves and is executable. `version` is the best-effort
    /// `<binary> --version` output, if it ran.
    Installed {
        resolved: PathBuf,
        step: ResolutionStep,
        version: Option<String>,
    },
    /// The bare `binary` name does not resolve on `PATH`.
    MissingOnPath {
        /// The searched PATH, colon-separated — only ever printed under
        /// `--verbose`.
        searched: String,
    },
    /// The absolute `binary` path resolves as a string but names no
    /// executable file.
    MissingFile { resolved: PathBuf },
}

/// Inspect every configured harness: installed ones carry their resolved
/// path and version; uninstalled, unreferenced ones are dropped (pruning
/// removes them on its own); uninstalled but referenced ones are kept so
/// the report can warn once.
///
/// `reference_labels` maps a harness name to its live reference labels
/// (see [`crate::db::Database::harness_references`]); callers without an
/// open database pass an empty map, which keeps every uninstalled harness
/// silent.
pub(super) fn inspect_harnesses(
    config: &crate::domain::canopy_config::CanopyConfig,
    reference_labels: &HashMap<String, Vec<String>>,
) -> Vec<HarnessEntry> {
    let mut entries = Vec::new();
    for cli in &config.clis {
        let label = doctor_cli_label(cli);
        let references = reference_labels
            .get(cli.name.as_str())
            .cloned()
            .unwrap_or_default();
        match cli.resolve() {
            Ok((resolved, step)) if binary_is_executable(&resolved) => {
                entries.push(HarnessEntry {
                    name: cli.name.clone(),
                    label,
                    state: HarnessState::Installed {
                        version: binary_version(&resolved),
                        resolved,
                        step,
                    },
                    references,
                });
            }
            Ok((resolved, _)) if resolved.is_absolute() => {
                if references.is_empty() {
                    continue;
                }
                entries.push(HarnessEntry {
                    name: cli.name.clone(),
                    label,
                    state: HarnessState::MissingFile { resolved },
                    references,
                });
            }
            Ok(_) => {
                if references.is_empty() {
                    continue;
                }
                entries.push(HarnessEntry {
                    name: cli.name.clone(),
                    label,
                    state: HarnessState::MissingOnPath {
                        searched: std::env::var("PATH").unwrap_or_default(),
                    },
                    references,
                });
            }
            Err(_) => {
                if references.is_empty() {
                    continue;
                }
                entries.push(HarnessEntry {
                    name: cli.name.clone(),
                    label,
                    state: HarnessState::MissingOnPath {
                        searched: std::env::var("PATH").unwrap_or_default(),
                    },
                    references,
                });
            }
        }
    }
    entries
}

/// Group [`crate::db::Database::harness_references`] by harness name for
/// [`inspect_harnesses`].
pub(super) fn reference_labels_by_harness(
    db: &crate::db::Database,
) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for reference in db.harness_references() {
        map.entry(reference.harness)
            .or_default()
            .push(reference.label);
    }
    for labels in map.values_mut() {
        labels.sort();
        labels.dedup();
    }
    map
}

/// The `Harnesses:` summary line: installed harnesses with their version,
/// or `(none installed)`. Uninstalled harnesses never appear here.
pub(super) fn installed_summary(entries: &[HarnessEntry]) -> String {
    let installed: Vec<String> = entries
        .iter()
        .filter_map(|entry| match &entry.state {
            HarnessState::Installed { version, .. } => Some(match version {
                Some(version) => format!("{} ({version})", entry.name),
                None => entry.name.clone(),
            }),
            HarnessState::MissingOnPath { .. } | HarnessState::MissingFile { .. } => None,
        })
        .collect();
    if installed.is_empty() {
        "Harnesses: (none installed)".to_string()
    } else {
        format!("Harnesses: {}", installed.join(", "))
    }
}

/// The warning for a removed harness something still references: the
/// console line and the `Suggestions` entry. Pure over the entry so tests
/// assert the wording without capturing stdout. `None` when the harness is
/// unreferenced (nothing to warn about) or still installed.
///
/// The console line carries a `⚠`, never a `✗`; neither line ever carries
/// the searched PATH unless `verbose` (`--verbose`) is set.
fn missing_harness_report(entry: &HarnessEntry, verbose: bool) -> Option<(String, String)> {
    match &entry.state {
        HarnessState::Installed { .. }
        | HarnessState::MissingOnPath { .. }
        | HarnessState::MissingFile { .. }
            if entry.references.is_empty() =>
        {
            None
        }
        HarnessState::Installed { .. } => None,
        HarnessState::MissingOnPath { .. } | HarnessState::MissingFile { .. } => {
            let detail = if verbose {
                match &entry.state {
                    HarnessState::Installed { .. } => String::new(),
                    HarnessState::MissingOnPath { searched } => {
                        format!(" (searched PATH: {searched})")
                    }
                    HarnessState::MissingFile { resolved } => {
                        format!(" (resolved to {})", resolved.display())
                    }
                }
            } else {
                String::new()
            };
            Some((
                format!(
                    " \x1b[33m⚠\x1b[0m {}: not on PATH, used by {}{detail}",
                    entry.label,
                    entry.references.join(", "),
                ),
                format!(
                    "'{}' is not on PATH but is still used by {}; reinstall it or remove the reference.",
                    entry.name,
                    entry.references.join(", "),
                ),
            ))
        }
    }
}

/// Doctor's CLI-resolution section (B40): per-CLI report of resolved or
/// not, by which step, absolute path. A removed harness that something
/// still references is warned about once, naming the references, without
/// a PATH dump; an uninstalled, unreferenced harness never reaches this
/// function (see [`inspect_harnesses`]).
///
/// `verbose` gates the searched-PATH detail: without `--verbose` the
/// warning carries no PATH at all.
///
/// `issues` collects the `Suggestions` entries. `warned` collects the
/// harness names already warned about, so a second call in the same run
/// (e.g. the RAG re-read of config) does not duplicate the warning.
pub(super) fn report_cli_resolution(
    config: &crate::domain::canopy_config::CanopyConfig,
    entries: &[HarnessEntry],
    verbose: bool,
    warned: &mut HashSet<String>,
    issues: &mut Vec<String>,
) {
    let daemon_path = crate::domain::cli_strategy::daemon_path();

    if config.clis.is_empty() {
        println!(" \x1b[33m⚠\x1b[0m No harnesses configured (run 'canopy setup')");
        issues.push("Run 'canopy setup' to detect and configure harnesses".to_string());
        return;
    }
    if entries.is_empty() {
        println!(" \x1b[33m⚠\x1b[0m No harnesses installed (run 'canopy setup')");
        issues.push("Run 'canopy setup' to detect and configure harnesses".to_string());
        return;
    }

    let by_name: HashMap<&str, &HarnessEntry> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    for cli_config in &config.clis {
        // Uninstalled and unreferenced: pruned on its own, nothing to say.
        let Some(entry) = by_name.get(cli_config.name.as_str()) else {
            continue;
        };
        report_one_harness(entry, cli_config, &daemon_path, verbose, warned, issues);
    }
}

/// Report one inspection-verified harness entry: a removed harness that
/// something still references warns once (no PATH dump without `--verbose`);
/// anything else follows the installed path (identity checks, daemon
/// reachability, versioned success line).
fn report_one_harness(
    entry: &HarnessEntry,
    cli_config: &crate::domain::cli_config::CliConfig,
    daemon_path: &Option<String>,
    verbose: bool,
    warned: &mut HashSet<String>,
    issues: &mut Vec<String>,
) {
    if !matches!(entry.state, HarnessState::Installed { .. }) {
        if let Some((line, issue)) = missing_harness_report(entry, verbose) {
            if warned.insert(entry.name.clone()) {
                println!("{line}");
                issues.push(issue);
            }
        }
        return;
    }
    // Only inspection-verified installs reach here: report what was
    // inspected rather than re-resolving (which could only disagree).
    let HarnessState::Installed {
        resolved,
        step,
        version,
    } = &entry.state
    else {
        return;
    };
    report_installed_harness(
        entry,
        cli_config,
        resolved,
        *step,
        version,
        daemon_path,
        issues,
    );
}

/// The installed path: the race guard, the two identity checks, the
/// daemon-reachability check, and the versioned success line.
fn report_installed_harness(
    entry: &HarnessEntry,
    cli_config: &crate::domain::cli_config::CliConfig,
    resolved: &std::path::Path,
    step: crate::domain::cli_strategy::ResolutionStep,
    version: &Option<String>,
    daemon_path: &Option<String>,
    issues: &mut Vec<String>,
) {
    let label = doctor_cli_label(cli_config);
    // Race guard: a binary deleted between inspection and report.
    if !binary_is_executable(resolved) {
        println!(
            " \x1b[31m✗\x1b[0m {} → {} (via {} — file missing or not executable)",
            label,
            resolved.display(),
            step.label()
        );
        issues.push(format!(
            "'{}' resolved to {} but that file is missing or not executable.",
            cli_config.name,
            resolved.display()
        ));
        return;
    }
    if report_identity_mismatch(entry, cli_config, resolved, step, issues) {
        return;
    }
    report_daemon_reachability(
        entry,
        cli_config,
        resolved,
        step,
        version,
        daemon_path,
        issues,
    );
}

/// The two identity checks (registry `identity_check`, then
/// `identity_args`/`identity_pattern`): `true` when either failed and the
/// harness is already reported, `false` when both pass or are undeclared.
fn report_identity_mismatch(
    _entry: &HarnessEntry,
    cli_config: &crate::domain::cli_config::CliConfig,
    resolved: &std::path::Path,
    step: crate::domain::cli_strategy::ResolutionStep,
    issues: &mut Vec<String>,
) -> bool {
    let label = doctor_cli_label(cli_config);
    if let Some(wb) = diagnose_cli_identity(cli_config, resolved) {
        let check = cli_config
            .identity_check
            .as_ref()
            .map_or("", |c| c.contains.as_str());
        println!(
            " \x1b[31m✗\x1b[0m {} → {} (via {} — wrong binary: expected '{}' in output; saw: {})",
            label,
            resolved.display(),
            step.label(),
            check,
            wb.output,
        );
        let check_cmd = cli_config
            .identity_check
            .as_ref()
            .map_or("", |c| c.cmd.as_str());
        issues.push(wb.report(&cli_config.binary, check_cmd));
        return true;
    }
    if let Some(error) = diagnose_binary_identity(cli_config, resolved) {
        println!(
            " \x1b[31m✗\x1b[0m {} → {} (via {} — {})",
            label,
            resolved.display(),
            step.label(),
            error.message()
        );
        issues.push(error.message());
        return true;
    }
    false
}

/// The daemon-reachability check and the versioned success line: green only
/// when the binary also resolves under the daemon's captured PATH.
fn report_daemon_reachability(
    _entry: &HarnessEntry,
    cli_config: &crate::domain::cli_config::CliConfig,
    resolved: &std::path::Path,
    step: crate::domain::cli_strategy::ResolutionStep,
    version: &Option<String>,
    daemon_path: &Option<String>,
    issues: &mut Vec<String>,
) {
    let label = doctor_cli_label(cli_config);
    let daemon_reachable = match &daemon_path {
        Some(dp) => cli_config.resolve_against(dp).is_ok(),
        None => true,
    };
    if daemon_reachable {
        let version = version
            .as_deref()
            .map(|version| format!(" — {version}"))
            .unwrap_or_default();
        super::success(format!(
            "{} → {} (via {}){version}",
            label,
            resolved.display(),
            step.label()
        ));
    } else {
        println!(
            " \x1b[33m⚠\x1b[0m {} → {} (via {} — reachable now but NOT from the daemon)",
            label,
            resolved.display(),
            step.label()
        );
        issues.push(format!(
            "'{}' is on your interactive PATH but not the daemon's. \
             Re-run `canopy daemon install` to update the daemon's PATH, \
             or add the directory to the systemd unit's Environment=PATH=.",
            cli_config.name
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cli_config::CliConfig;

    fn missing_entry(name: &str, references: Vec<String>) -> HarnessEntry {
        HarnessEntry {
            name: name.to_string(),
            label: name.to_string(),
            state: HarnessState::MissingOnPath {
                searched: "/usr/bin:/bin".to_string(),
            },
            references,
        }
    }

    fn installed_entry(name: &str) -> HarnessEntry {
        HarnessEntry {
            name: name.to_string(),
            label: name.to_string(),
            state: HarnessState::Installed {
                resolved: PathBuf::from("/usr/bin/ls"),
                step: ResolutionStep::Path,
                version: Some("ls (GNU coreutils) 9.1".to_string()),
            },
            references: Vec::new(),
        }
    }

    #[test]
    fn unreferenced_missing_harness_is_silent() {
        let entry = missing_entry("qwen", Vec::new());
        assert!(missing_harness_report(&entry, false).is_none());
        assert!(missing_harness_report(&entry, true).is_none());

        let config = crate::domain::canopy_config::CanopyConfig {
            clis: vec![CliConfig {
                name: "qwen".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(inspect_harnesses(&config, &HashMap::new()).is_empty());
    }

    #[test]
    fn referenced_missing_harness_warns_once_without_path_dump() {
        let entry = missing_entry("qwen", vec!["crew lab-general member 3".to_string()]);
        let (line, issue) = missing_harness_report(&entry, false).expect("referenced warns");
        assert!(line.contains('⚠'), "warning, not an error: {line}");
        assert!(!line.contains('✗'), "never an error tick: {line}");
        assert!(line.contains("not on PATH, used by crew lab-general member 3"));
        assert!(!line.contains("/usr/bin:/bin"), "no PATH dump: {line}");
        assert!(
            !line.contains("searched"),
            "no searched-PATH wording: {line}"
        );
        assert!(
            !issue.contains("/usr/bin:/bin"),
            "no PATH dump in Suggestions: {issue}"
        );
        assert!(issue.contains("crew lab-general member 3"));
    }

    #[test]
    fn several_references_join_into_one_warning() {
        // One warning per harness, not one per reference: all live labels
        // join into the single console line and the single Suggestions entry.
        let entry = missing_entry(
            "qwen",
            vec![
                "crew lab-general member 1".to_string(),
                "scheduled agent nightly".to_string(),
            ],
        );
        let (line, issue) = missing_harness_report(&entry, false).expect("referenced warns");
        assert_eq!(line.matches('⚠').count(), 1, "{line}");
        assert!(line.contains("crew lab-general member 1"), "{line}");
        assert!(line.contains("scheduled agent nightly"), "{line}");
        assert!(issue.contains("crew lab-general member 1"), "{issue}");
        assert!(issue.contains("scheduled agent nightly"), "{issue}");
    }

    #[test]
    fn verbose_warning_names_the_searched_path() {
        let entry = missing_entry("qwen", vec!["scheduled agent abc123".to_string()]);
        let (line, issue) = missing_harness_report(&entry, true).expect("referenced warns");
        assert!(
            line.contains("/usr/bin:/bin"),
            "verbose names the PATH: {line}"
        );
        assert!(
            !issue.contains("/usr/bin:/bin"),
            "Suggestions stay clean: {issue}"
        );
    }

    #[test]
    fn installed_harness_summarises_with_version() {
        let entries = vec![
            installed_entry("opencode"),
            missing_entry("qwen", Vec::new()),
        ];
        let summary = installed_summary(&entries);
        assert!(
            summary.contains("opencode (ls (GNU coreutils) 9.1)"),
            "{summary}"
        );
        assert!(
            !summary.contains("qwen"),
            "uninstalled never listed: {summary}"
        );
        assert!(installed_summary(&[]).contains("(none installed)"));
    }

    #[test]
    fn installed_missing_file_harness_with_reference_warns() {
        let entry = HarnessEntry {
            name: "cn".to_string(),
            label: "cn".to_string(),
            state: HarnessState::MissingFile {
                resolved: PathBuf::from("/opt/cn/bin/cn"),
            },
            references: vec!["graph 'g' node 'n'".to_string()],
        };
        let (line, _) = missing_harness_report(&entry, false).expect("referenced warns");
        assert!(
            line.contains("not on PATH, used by graph 'g' node 'n'"),
            "{line}"
        );
        assert!(!line.contains('✗'), "{line}");
    }
}
