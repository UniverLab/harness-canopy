//! Headless liveness probing for configured platforms.
//!
//! `agent_models`/`doctor` answer "does the catalog list this platform" and
//! "is its binary present and its config well-formed" — neither one ever
//! actually asks the harness to speak. A platform can be installed, listed,
//! and configured while every real invocation answers with its own error
//! text and exits 0 (the 2026-08 `mimocode`/`mimo-auto` incident: `Error:
//! Unsupported model mimo-auto` on stdout, `Error: Invalid API Key` on the
//! named ones, exit code 0 both times). This module is the missing "actually
//! run it" step: build the platform's real headless command, spawn it, and
//! judge the *response*, never the exit code alone.
//!
//! ## What counts as "a usable response"
//!
//! Non-empty stdout is the obvious rule and is too weak — `Error: Invalid
//! API Key` is non-empty stdout that exits 0. Instead every probe asks the
//! harness to echo a unique, per-probe token ([`probe_token`]) and the
//! verdict is whether that exact token shows up anywhere in the captured
//! stdout/stderr ([`probe_target`]). Containment rather than an exact match
//! deliberately tolerates a harness that wraps or decorates its output
//! (banners, ANSI, a leading "Assistant:" prefix) — the token is chosen
//! random enough (`CANOPY-PROBE-<uuidv4>`) that a harness's own error text
//! reflecting it back by coincidence is not a realistic failure mode, and an
//! error message that doesn't echo the prompt (`Unsupported model ...`,
//! `Invalid API Key`) correctly fails the check.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::daemon::handler::redact_secrets;
use crate::daemon::process::{terminate_process_group_async, KILL_GRACE};
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::cli_strategy::CliStrategy;
use crate::domain::graphs::{GraphDetails, GraphNodeKind};

/// Default bound on how long a single probe waits for a response — this is a
/// liveness check, not a capability benchmark, so it stays short.
pub(crate) const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 30;
pub(crate) const MIN_PROBE_TIMEOUT_SECS: u64 = 5;
pub(crate) const MAX_PROBE_TIMEOUT_SECS: u64 = 120;

/// One platform+model pair to probe. `model: None` means "whatever this
/// platform's CLI defaults to when no model flag is passed" — not "any
/// model", so a probe against `None` says nothing about a specific model a
/// node might request explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProbeTarget {
    pub platform: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// What actually happened when a target was invoked. Distinct from a bare
/// `bool` because the whole point of this module is that a caller needs to
/// know *why* a platform is unreachable (missing API key vs. wrong model
/// name vs. it simply never answered) — see [`ProbeReport::error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeOutcome {
    /// The harness echoed the probe token back — a real response came from
    /// the model, not just a process that exited cleanly.
    Reachable,
    /// The process ran to completion (any exit code) but the response never
    /// contained the probe token — the defect class this module exists to
    /// catch, including "exit 0 with an error and nothing else."
    Broken,
    /// The process ran to completion and its response carries the CLI's own
    /// argument-parser usage/error text — canopy called the platform wrong
    /// (flag order, a renamed/removed flag, an argument shape the parser
    /// rejects), not a credentials/network/quota problem the platform
    /// itself has (CB61's devin case: `-p` before `--model` made the
    /// prompt land as a positional `PATH`, and devin's own clap parser
    /// refused with "the argument '--print [<PROMPT>]' cannot be used with
    /// '[PATH]...'"). Distinct from `Broken` so the fix is understood to be
    /// the registry's `invocation_template`/`headless_mode`, never a retry
    /// or a permuted command line (see `error` for the exact parser line,
    /// `argv` for the exact command canopy ran).
    Misconfigured,
    /// The resolved binary does not identify as this platform (CB44): the
    /// registry's `identity_check` ran and its output lacked the expected
    /// substring. Distinct from `Broken`/`SpawnFailed` so the caller knows
    /// it is not a quota, model, or network problem — the wrong program
    /// answered to the bare CLI name.
    WrongBinary,
    /// The process was still running when the timeout elapsed; its process
    /// group has been killed. Deliberately distinct from `Broken`: the
    /// harness never got a chance to answer at all.
    TimedOut,
    /// `platform` names a CLI that isn't in `~/.canopy/config.toml` — no
    /// process was ever spawned.
    NotConfigured,
    /// The command could not even be built/spawned/waited on (binary not
    /// resolvable, permission denied, a `wait()` syscall failure) — distinct
    /// from `Broken` because the harness's own error-reporting was never
    /// reached.
    SpawnFailed,
    /// A `model` was requested but this platform's CLI has no way to select
    /// one explicitly (no `model_flag` configured) — the 2026-08-13
    /// `mistral` case, whose CLI addresses named agents, not models, so
    /// `agent_models` output is never valid input for it. No process was
    /// spawned: probing "whatever the bare CLI defaults to" would validate
    /// a different pair than the one asked about and silently pass, which
    /// is the exact defect this variant exists to prevent. Never
    /// `reachable` — a caller that treated this as passing would repeat
    /// the 2026-08-13 incident.
    Unknown,
    /// The harness accepted the request and answered, but its own response
    /// carries a "requested model not recognized, using a different one"
    /// warning (see [`detect_model_substitution`]) — the `gpt-5.6` case:
    /// the probe token can still show up because the *substituted* model
    /// answered it, not the one that was actually requested. Reporting
    /// this `Reachable` would validate a pair a real node never actually
    /// runs.
    Substituted,
}

impl ProbeOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Broken => "broken",
            Self::Misconfigured => "misconfigured",
            Self::WrongBinary => "wrong_binary",
            Self::TimedOut => "timed_out",
            Self::NotConfigured => "not_configured",
            Self::SpawnFailed => "spawn_failed",
            Self::Unknown => "unknown",
            Self::Substituted => "substituted",
        }
    }

    pub fn reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }
}

/// Result of probing one [`ProbeTarget`].
#[derive(Debug, Clone)]
pub(crate) struct ProbeReport {
    pub platform: String,
    pub model: Option<String>,
    pub outcome: ProbeOutcome,
    pub duration_ms: u128,
    /// The harness's own error text, verbatim after secret redaction —
    /// `None` exactly when `outcome` is `Reachable`. A boolean here would
    /// tell a caller nothing about whether the fix is an API key, a model
    /// name, or a missing binary.
    pub error: Option<String>,
    /// The exact argv canopy invoked for this probe (program + args,
    /// space-joined, secrets redacted) — CB61. `Some` on every outcome from
    /// `SpawnFailed` onward where a `Command` was actually built (spawn
    /// failure, timeout, a wait() error, `Substituted`, `Broken`,
    /// `Misconfigured`); `None` when no command was ever built
    /// (`NotConfigured`, `Unknown`, `WrongBinary`, a `build_command` error,
    /// and `Reachable` — paired with `error` being `None` there too, since
    /// nothing needs reproducing on success).
    pub argv: Option<String>,
}

impl ProbeReport {
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "platform": self.platform,
            "model": self.model,
            "reachable": self.outcome.reachable(),
            "outcome": self.outcome.as_str(),
            "duration_ms": self.duration_ms,
            "error": self.error,
            "argv": self.argv,
        })
    }
}

/// A fresh, hard-to-guess token this probe run asks the harness to echo
/// back. Random per call so two concurrent probes (or a probe re-run) can
/// never be confused by a stale token surviving in some cache/log the
/// harness reads.
fn probe_token() -> String {
    format!("CANOPY-PROBE-{}", uuid::Uuid::new_v4().simple())
}

/// The prompt every probe sends: minimal in, minimal out (a few tokens each
/// way) — this is a liveness check, not a capability benchmark.
fn probe_prompt(token: &str) -> String {
    format!("Reply with exactly this text and nothing else: {token}")
}

/// Marker substring of the harness's own "requested model not recognized,
/// substituting a different one" warning — the 2026-08-13 incident's
/// `Model metadata for 'gpt-5.6' not found. Defaulting to fallback
/// metadata`. Matched case-insensitively and on the "defaulting to
/// fallback metadata" half only, since that half is the actual signal (the
/// model name/quoting around "not found" is free-form harness text not
/// worth pinning exactly).
const MODEL_SUBSTITUTION_MARKER: &str = "defaulting to fallback metadata";

/// Find the line (if any) in the captured response that shows the harness
/// silently answered with a different model than the one requested. Checked
/// against the *raw* stdout/stderr, independent of whether the probe token
/// is present — the whole defect this catches is a response that both
/// contains the token (a real, usable-looking answer) and this warning (the
/// answer came from a model nobody asked for).
fn detect_model_substitution(stdout: &str, stderr: &str) -> Option<String> {
    for text in [stderr, stdout] {
        if let Some(line) = text
            .lines()
            .find(|line| line.to_lowercase().contains(MODEL_SUBSTITUTION_MARKER))
        {
            return Some(line.to_string());
        }
    }
    None
}

/// Substrings that indicate the requested model was rejected outright — the
/// 2026-08-13 `gpt-5.6` case where Codex returned `400 invalid_request_error
/// — "The 'gpt-5.6' model is not supported when using Codex with a ChatGPT
/// account."` Matched case-insensitively and per-line so the returned error
/// is the exact harness line, like [`detect_model_substitution`]. Only checked
/// when a specific `model` was requested; otherwise a platform mention of
/// "model" in help text would false-positive.
fn detect_model_rejection(stdout: &str, stderr: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "model is not supported",
        "not supported when using",
        "model not available",
    ];
    for text in [stderr, stdout] {
        for line in text.lines() {
            let lower = line.to_lowercase();
            if MARKERS.iter().any(|m| lower.contains(m)) {
                return Some(line.to_string());
            }
            // `invalid_request_error` + `model` in the same line covers the raw
            // JSON body variant: `{"error":{"type":"invalid_request_error","message":"The 'gpt-5.6' model ..."}}`
            // without pinning the full JSON shape. Anchored on `invalid_request`
            // rather than a bare `invalid` so an unrelated line that merely
            // mentions "invalid" and "model" on a healthy probe can't be
            // misread as a rejection.
            if lower.contains("invalid_request") && lower.contains("model") {
                return Some(line.to_string());
            }
        }
    }
    None
}

/// Render a built command's argv as one space-joined, secret-redacted,
/// shell-quoted string — CB61. Quoted (via `shell_words::quote`, the same
/// crate the registry side already uses to split `headless_mode`) because
/// the probe prompt itself is a single argv element containing spaces
/// ("Reply with exactly this text and nothing else: <token>") — an
/// unquoted join would let a shell re-split it into several words and
/// change what gets reproduced, defeating FR1's "paste it into a shell
/// without reconstructing it." Reuses `redact_secrets` (the same
/// redaction node-run output already goes through) rather than a second
/// implementation. Built from `Command::as_std()`
/// (`get_program`/`get_args`), the same argv the OS will actually exec —
/// not a re-derivation from the registry template, so it can never drift
/// from what was really run.
fn command_argv(command: &tokio::process::Command) -> String {
    let std_cmd = command.as_std();
    let mut parts = vec![std_cmd.get_program().to_string_lossy().into_owned()];
    parts.extend(std_cmd.get_args().map(|a| a.to_string_lossy().into_owned()));
    let quoted = parts
        .iter()
        .map(|p| shell_words::quote(p).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    redact_secrets(&quoted)
}

/// Substrings of a CLI argument-parser's own usage/error output that mean
/// canopy's invocation was malformed, not that the platform is down —
/// CB61. Matched case-insensitively and per-line, like
/// `detect_model_rejection` above, but unconditionally (not gated on
/// `target.model` — CB61's devin case has no model requested at all).
/// This is a fixed, closed list (functional requirement #2's four shapes);
/// it is not a general clap/argparse-error detector.
const MISCONFIGURED_MARKERS: &[&str] = &[
    "cannot be used with",
    "unrecognized flag",
    "unexpected argument",
    "unknown option",
];

fn detect_misconfigured_invocation(stdout: &str, stderr: &str) -> Option<String> {
    for text in [stderr, stdout] {
        for line in text.lines() {
            let lower = line.to_lowercase();
            if MISCONFIGURED_MARKERS.iter().any(|m| lower.contains(m)) {
                return Some(line.to_string());
            }
        }
    }
    None
}

/// Probe one platform+model pair by actually invoking it: build the
/// platform's real headless command from its registry config
/// (`headless_mode`/`model_flag`/etc, via [`CliStrategy::from_cli_config`] —
/// the same path a real graph node uses), spawn it, and judge the captured
/// response rather than the exit code.
///
/// `workdir` is passed straight to [`CliStrategy::build_command`]; pass the
/// graph's workdir when probing on a graph's behalf, `None` for a standalone
/// probe with no project context.
pub(crate) async fn probe_target(
    config: &CanopyConfig,
    target: &ProbeTarget,
    workdir: Option<&str>,
    timeout: Duration,
) -> ProbeReport {
    let start = Instant::now();

    let Some(cli_config) = config.get_cli(&target.platform) else {
        return ProbeReport {
            platform: target.platform.clone(),
            model: target.model.clone(),
            outcome: ProbeOutcome::NotConfigured,
            duration_ms: start.elapsed().as_millis(),
            argv: None,
            error: Some(format!(
                "Platform '{}' is not configured in canopy (~/.canopy/config.toml).",
                target.platform
            )),
        };
    };

    let strategy = CliStrategy::from_cli_config(cli_config);

    // A model was requested but this platform's CLI has no usable model flag
    // (absent or blank) — `CliStrategy::build_command` would silently drop it
    // and probe the bare default instead, which is exactly how the 2026-08-13
    // `mistral` pair was wrongly reported `reachable`: the probe validated
    // a different pair than the one a real node run would use. Report
    // `Unknown` without spawning rather than pretend the specific model was
    // exercised.
    if let Some(model) = target.model.as_deref() {
        if !crate::domain::cli_config::model_flag_selects_model(strategy.model_flag.as_deref()) {
            return ProbeReport {
                platform: target.platform.clone(),
                model: target.model.clone(),
                outcome: ProbeOutcome::Unknown,
                duration_ms: start.elapsed().as_millis(),
                argv: None,
                error: Some(format!(
                    "Platform '{}' has no configured way to select a model explicitly \
                     (no usable model_flag) — cannot validate model '{}' end-to-end. Omit \
                     `model` to probe the platform's own default instead.",
                    target.platform, model
                )),
            };
        }
    }

    // CB44: identity check before the probe prompt. When the resolved
    // binary is a different program answering to the same bare name (e.g.
    // the Blackbox window manager), report WrongBinary with the resolved
    // absolute path — never a quota, model, or network problem — and do not
    // run the probe prompt at all. Platforms with no declared check behave
    // exactly as today. Dispatch never runs this (probe/doctor only).
    if cli_config.identity_check.is_some() {
        let resolved_path = match cli_config.resolve() {
            Ok((p, _step)) => p,
            Err(error) => {
                return ProbeReport {
                    platform: target.platform.clone(),
                    model: target.model.clone(),
                    outcome: ProbeOutcome::SpawnFailed,
                    duration_ms: start.elapsed().as_millis(),
                    argv: None,
                    error: Some(redact_secrets(&error.to_string())),
                };
            }
        };
        if let Err(wb) =
            crate::domain::cli_strategy::verify_identity_async(cli_config, &resolved_path).await
        {
            let check_cmd = cli_config
                .identity_check
                .as_ref()
                .map(|c| c.cmd.as_str())
                .unwrap_or("");
            return ProbeReport {
                platform: target.platform.clone(),
                model: target.model.clone(),
                outcome: ProbeOutcome::WrongBinary,
                duration_ms: start.elapsed().as_millis(),
                argv: None,
                error: Some(redact_secrets(&wb.report(&cli_config.binary, check_cmd))),
            };
        }
    }

    let token = probe_token();
    let prompt = probe_prompt(&token);

    let mut command = match strategy.build_command(&prompt, target.model.as_deref(), workdir) {
        Ok(command) => command,
        Err(error) => {
            return ProbeReport {
                platform: target.platform.clone(),
                model: target.model.clone(),
                outcome: ProbeOutcome::SpawnFailed,
                duration_ms: start.elapsed().as_millis(),
                argv: None,
                error: Some(redact_secrets(&error.to_string())),
            }
        }
    };
    let argv = command_argv(&command);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ProbeReport {
                platform: target.platform.clone(),
                model: target.model.clone(),
                outcome: ProbeOutcome::SpawnFailed,
                duration_ms: start.elapsed().as_millis(),
                argv: Some(argv.clone()),
                error: Some(redact_secrets(&error.to_string())),
            }
        }
    };
    // Captured before `wait_with_output` below takes ownership of `child`.
    let pid = child.id();

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_elapsed) => {
            // Same B12 process-group kill every other detached spawn in the
            // engine uses on timeout — a probe must not leak a running child
            // any more than a real node run does.
            if let Some(pid) = pid {
                terminate_process_group_async(pid as i64, KILL_GRACE);
            }
            ProbeReport {
                platform: target.platform.clone(),
                model: target.model.clone(),
                outcome: ProbeOutcome::TimedOut,
                duration_ms: start.elapsed().as_millis(),
                argv: Some(argv.clone()),
                error: None,
            }
        }
        Ok(Err(error)) => ProbeReport {
            platform: target.platform.clone(),
            model: target.model.clone(),
            outcome: ProbeOutcome::SpawnFailed,
            duration_ms: start.elapsed().as_millis(),
            argv: Some(argv.clone()),
            error: Some(redact_secrets(&error.to_string())),
        },
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let duration_ms = start.elapsed().as_millis();

            // Checked before the token match: a substituted-model response
            // can still contain the probe token (the fallback model
            // answered it), which would otherwise be reported `Reachable`
            // for a model nobody actually validated — the 2026-08-13
            // `gpt-5.6` case.
            if target.model.is_some() {
                if let Some(warning) = detect_model_substitution(&stdout, &stderr) {
                    return ProbeReport {
                        platform: target.platform.clone(),
                        model: target.model.clone(),
                        outcome: ProbeOutcome::Substituted,
                        duration_ms,
                        argv: Some(argv.clone()),
                        error: Some(redact_secrets(&warning)),
                    };
                }
                if let Some(rejection) = detect_model_rejection(&stdout, &stderr) {
                    return ProbeReport {
                        platform: target.platform.clone(),
                        model: target.model.clone(),
                        outcome: ProbeOutcome::Broken,
                        duration_ms,
                        argv: Some(argv.clone()),
                        error: Some(redact_secrets(&rejection)),
                    };
                }
            }

            if stdout.contains(&token) || stderr.contains(&token) {
                ProbeReport {
                    platform: target.platform.clone(),
                    model: target.model.clone(),
                    outcome: ProbeOutcome::Reachable,
                    duration_ms,
                    argv: None,
                    error: None,
                }
            } else {
                // CB61: the platform answered with its own argument-parser
                // usage/error text — canopy called it wrong, not a
                // credentials/network/quota failure on the platform's side.
                // Checked before `raw_error` below moves stdout/stderr.
                let outcome = if detect_misconfigured_invocation(&stdout, &stderr).is_some() {
                    ProbeOutcome::Misconfigured
                } else {
                    ProbeOutcome::Broken
                };
                // Prefer stderr (where an error is conventionally printed)
                // but fall back to stdout — the mimocode incident printed
                // its error to stdout with an empty stderr, exit code 0.
                let raw_error = if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    format!(
                        "exited {} with no output and no probe token in the response",
                        output.status.code().unwrap_or(-1)
                    )
                };
                ProbeReport {
                    platform: target.platform.clone(),
                    model: target.model.clone(),
                    outcome,
                    duration_ms,
                    argv: Some(argv.clone()),
                    error: Some(redact_secrets(&raw_error)),
                }
            }
        }
    }
}

/// Count of reports that are *known* to fail a real run — every outcome
/// except `Reachable` and `Unknown`. Deliberately excludes `Unknown`: a pair
/// whose validity could not be determined is not a confirmed failure, and
/// folding it in here would make `would_fail` claim more certainty than the
/// probe actually has. Shared by `agent_probe` and `graph_preflight` so the
/// two tools can never disagree about what counts as "would fail".
pub(crate) fn would_fail_count(reports: &[ProbeReport]) -> usize {
    reports
        .iter()
        .filter(|r| !r.outcome.reachable() && r.outcome != ProbeOutcome::Unknown)
        .count()
}

/// Count of reports whose validity could not be determined at all (see
/// [`ProbeOutcome::Unknown`]). Reported alongside `would_fail` rather than
/// folded into it — an unknown pair is not a confirmed pass (excluded from
/// `would_fail`'s complement) and not a confirmed failure (excluded from
/// `would_fail` itself), so it needs its own count to stay honest about
/// which pairs were actually validated.
pub(crate) fn unknown_count(reports: &[ProbeReport]) -> usize {
    reports
        .iter()
        .filter(|r| r.outcome == ProbeOutcome::Unknown)
        .count()
}

/// Probe every target concurrently — a single slow/hanging harness must not
/// serialize the rest of the sweep. Results are returned in the same order
/// as `targets`.
pub(crate) async fn probe_targets(
    config: &CanopyConfig,
    targets: &[ProbeTarget],
    workdir: Option<&str>,
    timeout: Duration,
) -> Vec<ProbeReport> {
    let futures = targets
        .iter()
        .map(|target| probe_target(config, target, workdir, timeout));
    futures::future::join_all(futures).await
}

/// One distinct platform+model pair referenced somewhere in a graph, plus the
/// human-readable list of nodes/hooks that reference it — so a caller who
/// sees a pair fail knows which node(s) that affects, without probing the
/// same pair twice.
#[derive(Debug, Clone)]
pub(crate) struct GraphProbeTarget {
    pub target: ProbeTarget,
    pub used_by: Vec<String>,
}

/// Walk a graph's agent nodes (top-level graph and every spec's graph —
/// ensemble members are themselves ordinary [`GraphNodeKind::Agent`] rows, so
/// no separate ensemble query is needed) plus every hook in every event, and
/// return the distinct platform+model pairs they reference. A platform used
/// by five nodes appears once, with all five names attached.
pub(crate) fn distinct_targets_for_graph(details: &GraphDetails) -> Vec<GraphProbeTarget> {
    let mut by_key: HashMap<(String, Option<String>, Option<String>), usize> = HashMap::new();
    let mut result: Vec<GraphProbeTarget> = Vec::new();

    let mut record =
        |platform: Option<&str>, model: Option<&str>, effort: Option<&str>, label: String| {
            let Some(platform) = platform else { return };
            let key = (
                platform.to_string(),
                model.map(str::to_string),
                effort.map(str::to_string),
            );
            if let Some(&idx) = by_key.get(&key) {
                result[idx].used_by.push(label);
            } else {
                by_key.insert(key.clone(), result.len());
                result.push(GraphProbeTarget {
                    target: ProbeTarget {
                        platform: key.0,
                        model: key.1,
                        effort: key.2,
                    },
                    used_by: vec![label],
                });
            }
        };

    for (event, hooks) in &details.lp.hooks {
        for (idx, hook) in hooks.iter().enumerate() {
            // Command hooks have no platform — they are not agent targets.
            record(
                hook.platform.as_deref(),
                hook.model.as_deref(),
                hook.effort.as_deref(),
                format!("{} hook {}", event.as_str(), idx),
            );
        }
    }

    let all_nodes = details
        .graph_nodes
        .iter()
        .chain(details.specs.iter().flat_map(|spec| spec.nodes.iter()));
    for node in all_nodes {
        if node.kind != GraphNodeKind::Agent {
            continue;
        }
        let platform = node
            .config
            .get("platform")
            .or_else(|| node.config.get("cli"))
            .and_then(Value::as_str);
        let model = node.config.get("model").and_then(Value::as_str);
        let effort = node.config.get("effort").and_then(Value::as_str);
        record(platform, model, effort, format!("node: {}", node.name));
    }

    result
}

#[derive(Debug, Clone)]
pub(crate) struct RecentProbeTarget {
    pub target: ProbeTarget,
    pub used_by: Vec<String>,
    pub last_run: chrono::DateTime<chrono::Utc>,
    pub run_count: i64,
    pub last_outcome: String,
}

fn recent_source_label(source: &crate::db::graphs::RecentPairSourceKind) -> String {
    use crate::db::graphs::RecentPairSourceKind::*;
    match source {
        GraphNode {
            graph_id,
            graph_name,
            node_id,
            node_name,
        } => format!("graph '{graph_name}' ({graph_id}) — node '{node_name}' ({node_id})"),
        CompletionHook {
            graph_id,
            graph_name,
            event,
            hook_index,
        } => format!("graph '{graph_name}' ({graph_id}) — {event} hook {hook_index}"),
        BackgroundAgent { agent_id, prompt } => {
            let preview: String = prompt.chars().take(80).collect();
            let suffix = if preview.chars().count() < prompt.chars().count() {
                "..."
            } else {
                ""
            };
            format!("agent {agent_id} (prompt: \"{preview}{suffix}\")")
        }
        SubagentSpawn { workdir } => format!("subagent spawn in workdir '{workdir}'"),
    }
}

pub(crate) fn group_recent_usage(
    rows: &[crate::db::graphs::RecentPairSource],
) -> Vec<RecentProbeTarget> {
    let mut indexes: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut grouped = Vec::new();
    for row in rows {
        let key = (row.platform.clone(), row.model.clone());
        let index = match indexes.get(&key) {
            Some(index) => *index,
            None => {
                let index = grouped.len();
                indexes.insert(key.clone(), index);
                grouped.push(RecentProbeTarget {
                    target: ProbeTarget {
                        platform: key.0,
                        model: key.1,
                        effort: None,
                    },
                    used_by: Vec::new(),
                    last_run: row.started_at,
                    run_count: 0,
                    last_outcome: row.status.clone(),
                });
                index
            }
        };
        let target = &mut grouped[index];
        target.run_count += 1;
        if row.started_at > target.last_run {
            target.last_run = row.started_at;
            target.last_outcome = row.status.clone();
        }
        let label = recent_source_label(&row.source);
        if !target.used_by.contains(&label) {
            target.used_by.push(label);
        }
    }
    grouped.sort_by_key(|target| std::cmp::Reverse(target.last_run));
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cli_config::CliConfig;

    /// A `CanopyConfig` with one CLI entry pointing at `script_path`,
    /// invoked with no headless flags and no model flag — the fixture
    /// scripts below just read argv[1] (the prompt) and decide what to
    /// print from it, so no real flags are needed to exercise the verdict
    /// logic.
    fn config_with_cli(name: &str, script_path: &std::path::Path) -> CanopyConfig {
        CanopyConfig {
            clis: vec![CliConfig {
                name: name.to_string(),
                binary: script_path.to_string_lossy().to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// Same as [`config_with_cli`] but with `--model` configured as the
    /// model flag — for exercising the `platform`+`model` path the way a
    /// real graph node (e.g. codex) does, as opposed to platforms like
    /// `mistral` that have no model flag at all.
    fn config_with_cli_and_model_flag(name: &str, script_path: &std::path::Path) -> CanopyConfig {
        CanopyConfig {
            clis: vec![CliConfig {
                name: name.to_string(),
                binary: script_path.to_string_lossy().to_string(),
                model_flag: Some("--model".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn write_script(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[tokio::test]
    async fn probe_target_not_configured_for_unknown_platform() {
        let config = CanopyConfig::default();
        let target = ProbeTarget {
            platform: "ghost-cli".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::NotConfigured);
        assert!(report.error.unwrap().contains("ghost-cli"));
    }

    #[tokio::test]
    async fn agent_probe_rejects_display_string_as_platform() {
        // FR3: the display string is never accepted as input. A platform
        // configured under its slug must NOT match its own display name.
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "vibe", "echo ok\n");
        let mut config = config_with_cli("mistral", &script);
        config.clis[0].provider = Some("Mistral AI".to_string());
        config.clis[0].tool_name = Some("Vibe".to_string());
        let target = ProbeTarget {
            platform: "Mistral AI · Vibe".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::NotConfigured);
        assert_eq!(report.platform, "Mistral AI · Vibe");
    }

    /// Real failure shape #1: exit 0, the error goes to stderr, stdout is
    /// empty. Must be reported broken — this is the "Invalid API Key"
    /// shape from the mimocode incident when nothing reaches stdout at all.
    #[tokio::test]
    async fn probe_target_reports_broken_for_exit0_stderr_error_empty_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "broken-stderr-cli",
            "echo 'Error: Invalid API Key' 1>&2\nexit 0\n",
        );
        let config = config_with_cli("broken-stderr", &script);
        let target = ProbeTarget {
            platform: "broken-stderr".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert!(!report.outcome.reachable());
        assert_eq!(report.error.as_deref(), Some("Error: Invalid API Key"));
    }

    /// Real failure shape #2: exit 0, the error is printed to stdout instead
    /// of stderr (the mimocode "Unsupported model mimo-auto" shape). Must
    /// also be reported broken — a naive "non-empty stdout" check would
    /// wrongly pass this.
    #[tokio::test]
    async fn probe_target_reports_broken_for_exit0_stdout_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "broken-stdout-cli",
            "echo 'Error: Unsupported model mimo-auto'\nexit 0\n",
        );
        let config = config_with_cli("broken-stdout", &script);
        let target = ProbeTarget {
            platform: "broken-stdout".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert_eq!(
            report.error.as_deref(),
            Some("Error: Unsupported model mimo-auto")
        );
    }

    /// Real success shape: the harness echoes the probe token back on
    /// stdout. Must be reported reachable regardless of decoration around
    /// the token.
    #[tokio::test]
    async fn probe_target_reports_reachable_for_healthy_response() {
        let dir = tempfile::tempdir().unwrap();
        // Echoes argv[1] (the prompt, which embeds the token) back wrapped
        // in some decoration, simulating a harness that doesn't print the
        // token verbatim and alone.
        let script = write_script(
            &dir,
            "healthy-cli",
            "echo \"Assistant: sure, here you go -> $1\"\n",
        );
        let config = config_with_cli("healthy", &script);
        let target = ProbeTarget {
            platform: "healthy".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
        assert!(report.outcome.reachable());
        assert!(report.error.is_none());
    }

    /// No-regression check for the healthy path with a *specific* model
    /// requested against a platform that actually has a model flag
    /// configured (unlike the `mistral` case below): a clean answer
    /// containing the probe token must still be `Reachable`.
    #[tokio::test]
    async fn probe_target_reports_reachable_for_healthy_response_with_model() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "healthy-model-cli", "echo \"ok: $1\"\n");
        let config = config_with_cli_and_model_flag("healthy-model", &script);
        let target = ProbeTarget {
            platform: "healthy-model".to_string(),
            model: Some("some-model".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
        assert!(report.outcome.reachable());
        assert!(report.error.is_none());
    }

    /// The 2026-08-13 incident, reproduced as a fixture: the real 400 body
    /// Codex returned for `gpt-5.6` on a ChatGPT account. It never echoes
    /// the probe token, so it must be reported `Broken` (not `Reachable`) —
    /// this was already true before this fix; this test pins it against
    /// regressions now that the substitution check runs in the same branch.
    #[tokio::test]
    async fn probe_target_reports_broken_for_real_unsupported_model_400_body() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account."}}"#;
        let script = write_script(&dir, "codex-400-cli", &format!("echo '{body}'\nexit 0\n"));
        let config = config_with_cli_and_model_flag("codex-400", &script);
        let target = ProbeTarget {
            platform: "codex-400".to_string(),
            model: Some("gpt-5.6".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert!(!report.outcome.reachable());
        assert!(report.error.unwrap().contains("not supported"));
    }

    /// The gap the `detect_model_rejection` fix closes: the harness echoes the
    /// prompt (which contains the probe token) inside its error output *and*
    /// also carries a "model is not supported" rejection. A naive token check
    /// would report `Reachable` because the token is present; rejection must
    /// override token presence and report `Broken`.
    #[tokio::test]
    async fn probe_target_reports_broken_for_model_rejection_even_with_token_present() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "reject-with-token-cli",
            "echo \"The 'gpt-5.6' model is not supported when using Codex with a ChatGPT account.\"\necho \"$1\"\n",
        );
        let config = config_with_cli_and_model_flag("reject-with-token", &script);
        let target = ProbeTarget {
            platform: "reject-with-token".to_string(),
            model: Some("gpt-5.6".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert!(!report.outcome.reachable());
        assert!(report
            .error
            .unwrap()
            .to_lowercase()
            .contains("not supported"));
    }

    /// The `invalid_request` + `model` branch of `detect_model_rejection`:
    /// a rejection whose wording ("Unknown model") none of the fixed
    /// markers match, but which the CLI still tags as an
    /// `invalid_request_error`. Token is echoed too, so this also pins that
    /// the rejection wins over token presence.
    #[tokio::test]
    async fn probe_target_reports_broken_for_invalid_request_model_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"error":{"type":"invalid_request_error","message":"Unknown model gpt-5.6 requested"}}"#;
        let script = write_script(
            &dir,
            "invalid-request-cli",
            &format!("echo '{body}'\necho \"$1\"\n"),
        );
        let config = config_with_cli_and_model_flag("invalid-request", &script);
        let target = ProbeTarget {
            platform: "invalid-request".to_string(),
            model: Some("gpt-5.6".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert!(!report.outcome.reachable());
        assert!(report.error.unwrap().contains("invalid_request_error"));
    }

    /// A healthy probe whose output happens to contain both "invalid" and
    /// "model" on one line (but not `invalid_request`) must NOT be misread
    /// as a rejection — guards the anchoring of the catch-all branch.
    #[tokio::test]
    async fn probe_target_reachable_despite_incidental_invalid_and_model_words() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "noisy-healthy-cli",
            "echo 'note: pruned invalid cache entries for model gpt-5'\necho \"$1\"\n",
        );
        let config = config_with_cli_and_model_flag("noisy-healthy", &script);
        let target = ProbeTarget {
            platform: "noisy-healthy".to_string(),
            model: Some("gpt-5".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
        assert!(report.error.is_none());
    }

    /// The other half of the 2026-08-13 incident: the harness answers with
    /// the probe token present (so a naive check would call it healthy) but
    /// its own output also carries the "requested model not recognized,
    /// substituting a different one" warning. Must be `Substituted`, never
    /// `Reachable` — the answer did not come from the model that was
    /// actually requested.
    #[tokio::test]
    async fn probe_target_reports_substituted_for_model_metadata_fallback_warning() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "fallback-cli",
            "echo \"warning: Model metadata for 'gpt-5.6' not found. Defaulting to fallback metadata\" 1>&2\necho \"$1\"\n",
        );
        let config = config_with_cli_and_model_flag("fallback", &script);
        let target = ProbeTarget {
            platform: "fallback".to_string(),
            model: Some("gpt-5.6".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Substituted);
        assert!(!report.outcome.reachable());
        assert!(report
            .error
            .unwrap()
            .to_lowercase()
            .contains("fallback metadata"));
    }

    /// A response with no substitution warning and no probe token at all
    /// must stay `Broken`, not `Substituted` — the substitution check must
    /// not fire on unrelated broken output.
    #[tokio::test]
    async fn probe_target_does_not_report_substituted_without_the_warning() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "plain-broken-cli",
            "echo 'Error: something else went wrong' 1>&2\nexit 0\n",
        );
        let config = config_with_cli_and_model_flag("plain-broken", &script);
        let target = ProbeTarget {
            platform: "plain-broken".to_string(),
            model: Some("some-model".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
    }

    /// The `mistral` case: a `model` is requested but this platform's CLI
    /// has no `model_flag` configured at all — it addresses named agents,
    /// not models, so the pair can never be exercised end to end. Must be
    /// `Unknown` without ever spawning the (misleading) bare-default
    /// process, and never `Reachable`.
    #[tokio::test]
    async fn probe_target_reports_unknown_when_platform_has_no_model_flag() {
        let dir = tempfile::tempdir().unwrap();
        // If this script ran and its output were consulted, the probe would
        // wrongly look healthy — proving the `Unknown` verdict came from
        // the pre-spawn check, not from judging a response.
        let script = write_script(&dir, "no-model-flag-cli", "echo \"$1\"\n");
        let config = config_with_cli("no-model-flag", &script);
        let target = ProbeTarget {
            platform: "no-model-flag".to_string(),
            model: Some("mistral-medium-latest".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Unknown);
        assert!(!report.outcome.reachable());
        assert!(report.error.unwrap().contains("no configured way"));
    }

    /// Omitting `model` must keep today's behaviour exactly: even on a
    /// platform with no `model_flag`, probing the bare default is fine and
    /// must not trip the `Unknown` check (that check only applies when a
    /// *specific* model was requested and can't be honored).
    #[tokio::test]
    async fn probe_target_without_model_ignores_missing_model_flag() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "no-model-flag-default-cli", "echo \"$1\"\n");
        let config = config_with_cli("no-model-flag-default", &script);
        let target = ProbeTarget {
            platform: "no-model-flag-default".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
    }

    /// CB34: antigravity's `model_flag = ""` — a blank flag is not a flag. A
    /// specific `model` was requested and cannot be honoured, so the pair is
    /// `Unknown` ("cannot select a model"), never `Broken` and never `Reachable`.
    /// The stub script would look healthy if it ran, proving the verdict is
    /// pre-spawn.
    #[tokio::test]
    async fn probe_target_reports_unknown_when_model_flag_is_blank() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "blank-model-flag-cli", "echo \"$1\"\n");
        let mut config = config_with_cli("blank-model-flag", &script);
        config.clis[0].model_flag = Some(String::new());
        let target = ProbeTarget {
            platform: "blank-model-flag".to_string(),
            model: Some("claude-opus-4-8".to_string()),
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Unknown);
        assert!(!report.outcome.reachable());
    }

    /// A harness that hangs past the timeout must report `TimedOut`,
    /// distinct from `Broken` — the process never got a chance to answer.
    #[tokio::test]
    async fn probe_target_reports_timed_out_for_a_hanging_harness() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "hang-cli", "sleep 30\n");
        let config = config_with_cli("hang", &script);
        let target = ProbeTarget {
            platform: "hang".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_millis(200)).await;
        assert_eq!(report.outcome, ProbeOutcome::TimedOut);
        assert!(report.error.is_none());
    }

    #[tokio::test]
    async fn probe_target_reports_spawn_failed_for_a_missing_binary() {
        let config = config_with_cli("missing", std::path::Path::new("/no/such/binary-xyz"));
        let target = ProbeTarget {
            platform: "missing".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::SpawnFailed);
        assert!(report.error.is_some());
    }

    /// CB44: the resolved binary answers to the platform's name but is a
    /// different program (the Blackbox window-manager shape). The identity
    /// check fails, so the probe reports `WrongBinary` — naming the resolved
    /// absolute path and what the check saw — never quota/model/network.
    #[tokio::test]
    async fn probe_target_reports_wrong_binary_when_identity_check_fails() {
        use crate::domain::cli_config::IdentityCheck;
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "blackbox",
            "if [ \"$1\" = \"--version\" ]; then echo \"blackbox: another window manager is already running on display ':0'\"; else echo \"$1\"; fi\n",
        );
        let mut config = config_with_cli("blackbox", &script);
        config.clis[0].identity_check = Some(IdentityCheck {
            cmd: "--version".to_string(),
            contains: "Blackbox CLI".to_string(),
        });
        let target = ProbeTarget {
            platform: "blackbox".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::WrongBinary);
        assert!(!report.outcome.reachable());
        let error = report.error.unwrap();
        assert!(
            error.contains(&script.to_string_lossy().to_string()),
            "must name the resolved absolute path: {error}"
        );
        assert!(
            error.contains("does not identify as the blackbox CLI"),
            "must use the wrong-binary wording, not quota/model: {error}"
        );
        assert!(error.contains("another window manager"));
        assert_eq!(report.outcome.as_str(), "wrong_binary");
    }

    /// CB44: once the identity check fails, the probe prompt is never built
    /// or spawned — the wrong program's output is evidence only, never
    /// scraped for meaning. The fixture would echo the token back (looking
    /// healthy) if the probe prompt ever ran; `WrongBinary` proves it didn't.
    #[tokio::test]
    async fn probe_target_does_not_run_probe_prompt_when_identity_fails() {
        use crate::domain::cli_config::IdentityCheck;
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "blackbox",
            "if [ \"$1\" = \"--version\" ]; then echo 'wrong program'; else echo \"$1\"; fi\n",
        );
        let mut config = config_with_cli("blackbox-noprompt", &script);
        config.clis[0].identity_check = Some(IdentityCheck {
            cmd: "--version".to_string(),
            contains: "expected-token-xyz".to_string(),
        });
        let target = ProbeTarget {
            platform: "blackbox-noprompt".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::WrongBinary);
        assert!(!report.outcome.reachable());
    }

    /// CB44: a binary that passes the identity check proceeds to the normal
    /// probe prompt and is `Reachable` when it echoes the token.
    #[tokio::test]
    async fn probe_target_reachable_when_identity_check_passes_then_token_echoed() {
        use crate::domain::cli_config::IdentityCheck;
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "real-bb",
            "if [ \"$1\" = \"--version\" ]; then echo 'Blackbox CLI v1.2.3'; else echo \"$1\"; fi\n",
        );
        let mut config = config_with_cli("real-bb", &script);
        config.clis[0].identity_check = Some(IdentityCheck {
            cmd: "--version".to_string(),
            contains: "Blackbox".to_string(),
        });
        let target = ProbeTarget {
            platform: "real-bb".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
        assert!(report.error.is_none());
    }

    /// CB44 requirement 5: a platform with no declared check behaves exactly
    /// as today — the window-manager output is `Broken`, never `WrongBinary`.
    #[tokio::test]
    async fn probe_target_with_no_identity_check_still_reports_broken_for_window_manager_output() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "blackbox-unchecked",
            "echo \"blackbox: another window manager is already running on display ':0'\"\nexit 0\n",
        );
        let config = config_with_cli("blackbox-unchecked", &script);
        let target = ProbeTarget {
            platform: "blackbox-unchecked".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
    }

    #[tokio::test]
    async fn probe_target_redacts_secrets_in_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "leaky-cli",
            "echo 'Error: bad key sk-abcdefghijklmnopqrstuvwx' 1>&2\nexit 0\n",
        );
        let config = config_with_cli("leaky", &script);
        let target = ProbeTarget {
            platform: "leaky".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        let error = report.error.unwrap();
        assert!(!error.contains("sk-abcdefghijklmnopqrstuvwx"));
        assert!(error.contains("[REDACTED]"));
    }

    /// CB61 checked-by #1: a fake CLI that exits non-zero printing
    /// `error: unexpected argument` is reported `misconfigured`, and the
    /// argv canopy ran is present in the report.
    #[tokio::test]
    async fn probe_target_reports_misconfigured_for_unexpected_argument() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "misconfigured-cli",
            "echo \"error: unexpected argument '--foo' found\" 1>&2\nexit 1\n",
        );
        let config = config_with_cli("misconfigured", &script);
        let target = ProbeTarget {
            platform: "misconfigured".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Misconfigured);
        assert!(!report.outcome.reachable());
        assert_eq!(report.outcome.as_str(), "misconfigured");
        let argv = report.argv.expect("misconfigured report must carry argv");
        assert!(argv.contains(&script.to_string_lossy().to_string()));
    }

    /// FR1: "a person can paste it into a shell without reconstructing it."
    /// The probe prompt is one argv element containing spaces ("Reply with
    /// exactly this text and nothing else: <token>") — a naive space-join
    /// would let a shell re-split it into several words, changing what
    /// gets reproduced. Assert the rendered argv round-trips through
    /// `shell_words::split` back into the exact argv canopy ran, with the
    /// prompt intact as a single word.
    #[tokio::test]
    async fn probe_target_argv_is_shell_pasteable_despite_multi_word_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "misconfigured-argv-cli",
            "echo \"error: unexpected argument '--foo' found\" 1>&2\nexit 1\n",
        );
        let config = config_with_cli("misconfigured-argv", &script);
        let target = ProbeTarget {
            platform: "misconfigured-argv".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        let argv = report.argv.expect("misconfigured report must carry argv");
        let words = shell_words::split(&argv).expect("argv must be valid shell syntax");
        assert!(
            words
                .iter()
                .any(|w| w.starts_with("Reply with exactly this text and nothing else:")),
            "prompt must survive as a single shell word, got: {words:?}"
        );
    }

    /// CB61 checked-by #2: a fake CLI that exits non-zero printing a network
    /// error (no usage-string marker) stays `broken`, not `misconfigured`.
    #[tokio::test]
    async fn probe_target_stays_broken_for_non_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "network-broken-cli",
            "echo 'Error: connection reset by peer' 1>&2\nexit 1\n",
        );
        let config = config_with_cli("network-broken", &script);
        let target = ProbeTarget {
            platform: "network-broken".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Broken);
        assert!(report.argv.is_some());
    }

    /// CB61 checked-by #3: a fake CLI that answers normally stays
    /// `reachable`, with no argv attached (nothing to reproduce on success,
    /// matching `error`'s existing convention).
    #[tokio::test]
    async fn probe_target_reachable_has_no_argv() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "reachable-cli", "echo \"ok: $1\"\n");
        let config = config_with_cli("reachable-argv", &script);
        let target = ProbeTarget {
            platform: "reachable-argv".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Reachable);
        assert!(report.argv.is_none());
    }

    /// CB61 checked-by #4: the argv in the output contains no API key or
    /// token. `headless_mode` is the one field the legacy (non-template)
    /// build path always splits into argv verbatim (see
    /// `build_headless_command`), so it is the simplest way to put a fixed
    /// secret string on the command line for this test.
    #[tokio::test]
    async fn probe_target_redacts_secrets_in_argv() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "leaky-argv-cli",
            "echo \"error: unrecognized flag: --foo\" 1>&2\nexit 1\n",
        );
        let config = CanopyConfig {
            clis: vec![CliConfig {
                name: "leaky-argv".to_string(),
                binary: script.to_string_lossy().to_string(),
                headless_mode: "--api-key sk-ant-1234567890123456".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let target = ProbeTarget {
            platform: "leaky-argv".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Misconfigured);
        let argv = report.argv.unwrap();
        assert!(!argv.contains("sk-ant-1234567890123456"));
        assert!(argv.contains("[REDACTED]"));
    }

    /// CB61 corpus check: devin's real registry error text (the argument-
    /// order defect this spec exists to catch) classifies as
    /// `misconfigured`. Literal string copied from
    /// `canopy-registry/platforms/devin.toml`'s comment.
    #[tokio::test]
    async fn probe_target_classifies_devin_real_error_as_misconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "devin-like-cli",
            "echo \"error: the argument '--print [<PROMPT>]' cannot be used with '[PATH]...'\" 1>&2\nexit 2\n",
        );
        let config = config_with_cli("devin-like", &script);
        let target = ProbeTarget {
            platform: "devin-like".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Misconfigured);
    }

    /// CB61 corpus check: opencode's real registry error text (a v2-removed
    /// flag) classifies as `misconfigured`. Literal string copied from
    /// `canopy-registry/platforms/opencode.toml`'s comment.
    #[tokio::test]
    async fn probe_target_classifies_opencode_real_error_as_misconfigured() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            &dir,
            "opencode-like-cli",
            "echo \"Unrecognized flag: --dir in command opencode run\" 1>&2\nexit 1\n",
        );
        let config = config_with_cli("opencode-like", &script);
        let target = ProbeTarget {
            platform: "opencode-like".to_string(),
            model: None,
            effort: None,
        };
        let report = probe_target(&config, &target, None, Duration::from_secs(5)).await;
        assert_eq!(report.outcome, ProbeOutcome::Misconfigured);
    }

    #[tokio::test]
    async fn probe_targets_runs_concurrently_not_serially() {
        // Two hanging probes with a timeout well under 2x itself: if they
        // ran serially the second probe's deadline would already have
        // elapsed before it even started, so both still reporting
        // `TimedOut` at all (rather than the harness never having been
        // spawned) plus a wall-clock bound below 2x the timeout is the
        // signal that they were polled concurrently.
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(&dir, "hang-cli", "sleep 30\n");
        let mut config = CanopyConfig::default();
        config.clis.push(CliConfig {
            name: "hang-a".to_string(),
            binary: script.to_string_lossy().to_string(),
            ..Default::default()
        });
        config.clis.push(CliConfig {
            name: "hang-b".to_string(),
            binary: script.to_string_lossy().to_string(),
            ..Default::default()
        });
        let targets = vec![
            ProbeTarget {
                platform: "hang-a".to_string(),
                model: None,
                effort: None,
            },
            ProbeTarget {
                platform: "hang-b".to_string(),
                model: None,
                effort: None,
            },
        ];
        let timeout = Duration::from_millis(300);
        let start = Instant::now();
        let reports = probe_targets(&config, &targets, None, timeout).await;
        let elapsed = start.elapsed();
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|r| r.outcome == ProbeOutcome::TimedOut));
        assert!(
            elapsed < timeout * 2,
            "two probes took {elapsed:?}, expected well under 2x the {timeout:?} timeout \
             if they ran concurrently"
        );
    }

    fn agent_node(name: &str, config: Value) -> crate::domain::graphs::GraphNode {
        crate::domain::graphs::GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config,
            position: 0,
            created_at: chrono::Utc::now(),
        }
    }

    fn sample_graph(
        on_completed: Option<crate::domain::graphs::GraphCompletionHook>,
    ) -> crate::domain::graphs::Graph {
        let mut hooks = std::collections::BTreeMap::new();
        if let Some(hook) = on_completed {
            hooks.insert(
                crate::domain::graphs::GraphHookEvent::OnCompleted,
                vec![hook],
            );
        }
        crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "graph-1".to_string(),
            name: "sample".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks,
        }
    }

    #[test]
    fn distinct_targets_for_graph_dedupes_same_pair_across_nodes() {
        let details = GraphDetails {
            lp: sample_graph(None),
            graph_nodes: vec![
                agent_node(
                    "node-a",
                    serde_json::json!({"platform": "claude", "model": "claude-opus-4-8"}),
                ),
                agent_node(
                    "node-b",
                    serde_json::json!({"platform": "claude", "model": "claude-opus-4-8"}),
                ),
                agent_node(
                    "node-c",
                    serde_json::json!({"platform": "mimo", "model": "mimo-auto"}),
                ),
            ],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };

        let targets = distinct_targets_for_graph(&details);
        assert_eq!(targets.len(), 2);
        let claude = targets
            .iter()
            .find(|t| t.target.platform == "claude")
            .unwrap();
        assert_eq!(claude.target.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(claude.used_by, vec!["node: node-a", "node: node-b"]);
        let mimo = targets
            .iter()
            .find(|t| t.target.platform == "mimo")
            .unwrap();
        assert_eq!(mimo.used_by, vec!["node: node-c"]);
    }

    #[test]
    fn distinct_targets_for_graph_includes_on_completed_hook() {
        let hook = crate::domain::graphs::GraphCompletionHook {
            platform: Some("mimo".to_string()),
            model: None,
            effort: None,
            prompt: Some("done".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        };
        let details = GraphDetails {
            lp: sample_graph(Some(hook)),
            graph_nodes: vec![],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };

        let targets = distinct_targets_for_graph(&details);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target.platform, "mimo");
        assert_eq!(targets[0].used_by, vec!["on_completed hook 0"]);
    }

    #[test]
    fn distinct_targets_for_graph_includes_all_hook_events_and_dedups() {
        use crate::domain::graphs::{GraphCompletionHook, GraphHookEvent};
        use std::collections::BTreeMap;
        let mut hooks: BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>> = BTreeMap::new();
        hooks.insert(
            GraphHookEvent::OnCompleted,
            vec![GraphCompletionHook {
                platform: Some("mimo".to_string()),
                model: None,
                effort: None,
                prompt: Some("done".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        hooks.insert(
            GraphHookEvent::OnFailed,
            vec![GraphCompletionHook {
                platform: Some("mimo".to_string()),
                model: None,
                effort: None,
                prompt: Some("failed {{blocker}}".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        hooks.insert(
            GraphHookEvent::OnSpecCompleted,
            vec![GraphCompletionHook {
                platform: Some("other-cli".to_string()),
                model: None,
                effort: None,
                prompt: Some("spec {{spec_name}}".to_string()),
                command: None,
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: None,
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
            }],
        );
        let mut lp = sample_graph(None);
        lp.hooks = hooks;
        let details = GraphDetails {
            lp,
            graph_nodes: vec![],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };

        let targets = distinct_targets_for_graph(&details);
        assert_eq!(targets.len(), 2);
        let mimo = targets
            .iter()
            .find(|t| t.target.platform == "mimo")
            .unwrap();
        assert_eq!(
            mimo.used_by,
            vec!["on_completed hook 0", "on_failed hook 0"]
        );
        let other = targets
            .iter()
            .find(|t| t.target.platform == "other-cli")
            .unwrap();
        assert_eq!(other.used_by, vec!["on_spec_completed hook 0"]);
    }

    #[test]
    fn distinct_targets_for_graph_skips_non_agent_nodes() {
        let details = GraphDetails {
            lp: sample_graph(None),
            graph_nodes: vec![{
                let mut node = agent_node("gate-1", serde_json::json!({"platform": "claude"}));
                node.kind = GraphNodeKind::Gate;
                node
            }],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };

        assert!(distinct_targets_for_graph(&details).is_empty());
    }

    fn report(outcome: ProbeOutcome) -> ProbeReport {
        ProbeReport {
            platform: "p".to_string(),
            model: None,
            outcome,
            duration_ms: 0,
            argv: None,
            error: None,
        }
    }

    /// CB44: `WrongBinary` is a confirmed failure (the binary is the wrong
    /// program, not an unvalidated pair), so `would_fail` counts it — and
    /// `unknown_count` does not.
    #[test]
    fn would_fail_count_counts_wrong_binary_as_confirmed_failure() {
        let reports = vec![report(ProbeOutcome::WrongBinary)];
        assert_eq!(would_fail_count(&reports), 1);
        assert_eq!(unknown_count(&reports), 0);
    }

    /// `would_fail` must count every confirmed-failure outcome — not just
    /// `Broken` — and must never count `Reachable`.
    #[test]
    fn would_fail_count_counts_every_confirmed_failure_outcome() {
        let reports = vec![
            report(ProbeOutcome::Reachable),
            report(ProbeOutcome::Broken),
            report(ProbeOutcome::TimedOut),
            report(ProbeOutcome::NotConfigured),
            report(ProbeOutcome::SpawnFailed),
            report(ProbeOutcome::Substituted),
            report(ProbeOutcome::Misconfigured),
        ];
        assert_eq!(would_fail_count(&reports), 6);
    }

    /// The core honesty requirement: a pair whose validity is `Unknown`
    /// must not be counted as a confirmed failure by `would_fail`, and must
    /// not vanish into a `would_fail` of zero as if it had passed either —
    /// it has its own count via `unknown_count`.
    #[test]
    fn would_fail_count_does_not_count_unknown_as_a_failure_or_a_pass() {
        let reports = vec![report(ProbeOutcome::Unknown), report(ProbeOutcome::Unknown)];
        assert_eq!(would_fail_count(&reports), 0);
        assert_eq!(unknown_count(&reports), 2);
    }

    #[test]
    fn unknown_count_ignores_every_other_outcome() {
        let reports = vec![
            report(ProbeOutcome::Reachable),
            report(ProbeOutcome::Broken),
            report(ProbeOutcome::TimedOut),
            report(ProbeOutcome::NotConfigured),
            report(ProbeOutcome::SpawnFailed),
            report(ProbeOutcome::Substituted),
            report(ProbeOutcome::Misconfigured),
        ];
        assert_eq!(unknown_count(&reports), 0);
    }

    /// `agent_probe` and `graph_preflight` (handler.rs) both build their JSON
    /// response exclusively from `ProbeReport::to_json()` and this module's
    /// `would_fail_count`/`unknown_count` — there is no second copy of the
    /// outcome vocabulary for either tool to drift from. This test pins the
    /// vocabulary itself so a future new variant can't silently ship a word
    /// one caller knows about and the other doesn't.
    #[test]
    fn probe_outcome_vocabulary_is_stable_and_shared() {
        let all = [
            ProbeOutcome::Reachable,
            ProbeOutcome::Broken,
            ProbeOutcome::TimedOut,
            ProbeOutcome::NotConfigured,
            ProbeOutcome::SpawnFailed,
            ProbeOutcome::Unknown,
            ProbeOutcome::Substituted,
            ProbeOutcome::Misconfigured,
        ];
        let words: Vec<&str> = all.iter().map(ProbeOutcome::as_str).collect();
        assert_eq!(
            words,
            vec![
                "reachable",
                "broken",
                "timed_out",
                "not_configured",
                "spawn_failed",
                "unknown",
                "substituted",
                "misconfigured",
            ]
        );
        // Only `Reachable` is ever a pass.
        assert_eq!(all.iter().filter(|o| o.reachable()).count(), 1);
    }

    fn recent_source(
        platform: &str,
        model: Option<&str>,
        started_at: chrono::DateTime<chrono::Utc>,
        source: crate::db::graphs::RecentPairSourceKind,
    ) -> crate::db::graphs::RecentPairSource {
        crate::db::graphs::RecentPairSource {
            platform: platform.to_string(),
            model: model.map(str::to_string),
            started_at,
            status: "pass".to_string(),
            source,
        }
    }

    #[test]
    fn recent_usage_groups_once_and_deduplicates_repeated_sources() {
        let now = chrono::Utc::now();
        let source = |id: &str| crate::db::graphs::RecentPairSourceKind::GraphNode {
            graph_id: "g1".into(),
            graph_name: "Graph".into(),
            node_id: id.into(),
            node_name: format!("Node {id}"),
        };
        let rows = vec![
            recent_source("claude", Some("opus"), now, source("one")),
            recent_source(
                "claude",
                Some("opus"),
                now - chrono::Duration::minutes(1),
                source("one"),
            ),
            recent_source(
                "claude",
                Some("opus"),
                now - chrono::Duration::minutes(2),
                source("two"),
            ),
        ];
        let grouped = group_recent_usage(&rows);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].run_count, 3);
        assert_eq!(grouped[0].used_by.len(), 2);
        assert_eq!(grouped[0].last_run, now);
    }

    #[test]
    fn recent_usage_orders_pairs_by_last_run() {
        let now = chrono::Utc::now();
        let source = crate::db::graphs::RecentPairSourceKind::SubagentSpawn {
            workdir: "/tmp".into(),
        };
        let rows = vec![
            recent_source(
                "old",
                None,
                now - chrono::Duration::hours(1),
                source.clone(),
            ),
            recent_source("new", None, now, source),
        ];
        let grouped = group_recent_usage(&rows);
        assert_eq!(grouped[0].target.platform, "new");
    }
}
