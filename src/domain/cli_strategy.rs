//! Dynamic CLI execution strategy.
//!
//! All CLI definitions come from the registry (platforms.json).
//! Commands are built dynamically based on the saved configuration.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use anyhow::{Context, Result};

/// Strategy for building CLI commands from registry config.
#[derive(Clone)]
pub struct CliStrategy {
    pub binary: String,
    pub headless_mode: String,
    pub model_flag: Option<String>,
    pub supports_working_dir: bool,
    pub working_dir_flag: Option<String>,
    pub env_vars: HashMap<String, String>,
    /// When true, the prompt is delivered via stdin (backed by an anonymous
    /// temp file) instead of argv. See [`CliConfig::prompt_via_stdin`] for
    /// why this must stay opt-in per CLI.
    ///
    /// [`CliConfig::prompt_via_stdin`]: super::cli_config::CliConfig::prompt_via_stdin
    pub prompt_via_stdin: bool,
    /// Flag that sets the session id when spawning a new headless session
    /// (RS1). See [`CliConfig::session_id_set_flag`].
    ///
    /// [`CliConfig::session_id_set_flag`]: super::cli_config::CliConfig::session_id_set_flag
    pub session_id_set_flag: Option<String>,
    /// Subcommand/args to list this platform's sessions, e.g. `"session
    /// list"` or `"ls"`. Drives list-after-run session id capture (RS1
    /// phase 2). See [`CliConfig::session_list_cmd`].
    ///
    /// [`CliConfig::session_list_cmd`]: super::cli_config::CliConfig::session_list_cmd
    pub session_list_cmd: Option<String>,
    /// Extra args that make the session list machine-readable (e.g.
    /// `"--format json"`). See [`CliConfig::session_list_format_args`].
    ///
    /// [`CliConfig::session_list_format_args`]: super::cli_config::CliConfig::session_list_format_args
    pub session_list_format_args: Option<String>,
    /// Regex extracting session ids from the list output. See
    /// [`CliConfig::session_id_pattern`].
    ///
    /// [`CliConfig::session_id_pattern`]: super::cli_config::CliConfig::session_id_pattern
    pub session_id_pattern: Option<String>,
    /// Headless flag that resumes a specific session by id (RS2), e.g.
    /// `"--session"` (opencode/mimo/kilo), `"--resume"` (qwen/claude), or
    /// `"--fork"` (cn). The id is appended as the next argument, before the
    /// prompt. When absent, the platform has no verified by-id headless
    /// resume and every run cold-starts. See [`CliConfig::session_resume_cmd`].
    ///
    /// [`CliConfig::session_resume_cmd`]: super::cli_config::CliConfig::session_resume_cmd
    pub session_resume_cmd: Option<String>,
    /// Flag that non-interactively trusts the run's working directory for
    /// this invocation, e.g. mistral's `--trust`. See
    /// [`CliConfig::trust_flag`]. Only applied by a caller that has opted in
    /// (the graph engine, per `node.config["trust_workdir"]`) — never appended
    /// unconditionally by this struct's own command builders.
    ///
    /// [`CliConfig::trust_flag`]: super::cli_config::CliConfig::trust_flag
    pub trust_flag: Option<String>,
    /// Declarative argv template. See [`CliConfig::invocation_template`].
    pub invocation_template: Option<String>,
    pub effort_declaration: Option<super::cli_config::EffortDeclaration>,
}

/// A CLI's configured `binary` could not be resolved to an executable.
///
/// Typed (rather than a bare `anyhow!` string) so callers can tell this
/// apart from every other command-build failure without matching on the
/// rendered message: a missing binary is *permanent*, so the graph engine's
/// infra-crash retry must not spend attempts and backoff waiting for it to
/// appear (B39).
#[derive(Debug, thiserror::Error)]
#[error("CLI binary '{binary}' not found on PATH (searched: {path})")]
pub struct BinaryResolutionError {
    pub binary: String,
    pub path: String,
}

/// Which step of the resolution order produced a match. Setup/doctor
/// detection (B40) reports this alongside the resolved path so it can never
/// disagree with the spawner about *how* a CLI was found, not just whether
/// it was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionStep {
    /// `binary` was already an absolute path; used as-is, no PATH search.
    AbsolutePath,
    /// Found by searching PATH.
    Path,
}

impl ResolutionStep {
    pub fn label(&self) -> &'static str {
        match self {
            ResolutionStep::AbsolutePath => "absolute path",
            ResolutionStep::Path => "PATH",
        }
    }
}

/// Resolve the executable path for a CLI's configured `binary`.
///
/// - Absolute paths are returned as-is (the `binary` override in
///   `~/.canopy/config.toml` escape-hatch).
/// - Bare names are resolved against PATH via `which::which`.
///
/// Resolution is PATH-only: no install-directory guesses, no
/// `~/.<binary>/bin/<binary>` fallback. If the daemon's PATH matches the
/// user's PATH (set at `canopy daemon install` time), this is all that is
/// needed.
pub fn resolve_binary(binary: &str) -> Result<PathBuf> {
    let path_value = std::env::var("PATH").unwrap_or_default();
    resolve_binary_with_path(binary, &path_value)
}

/// Read the `Environment=PATH=` value from the systemd user unit file.
///
/// Returns `None` when the unit file doesn't exist or doesn't declare a PATH
/// (e.g. macOS launchd, or a manual install). The returned string is the
/// raw value — callers split on `:` themselves.
pub fn daemon_path() -> Option<String> {
    daemon_path_at(&dirs::home_dir()?)
}

/// [`daemon_path`], reading the unit file from under an explicit home
/// directory instead of `dirs::home_dir()`. Tests inject a temp dir so they
/// never depend on the developer's real `~/.config/systemd/user`.
fn daemon_path_at(home: &Path) -> Option<String> {
    let unit_path = home.join(".config/systemd/user").join("canopy.service");
    let content = std::fs::read_to_string(unit_path).ok()?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("Environment=PATH="))
        .map(|v| v.to_string())
}

/// [`resolve_binary`], reporting which step of the resolution order matched.
///
/// This is the single resolution primitive shared by the spawner
/// (`resolve_binary`, via [`resolve_binary_with_path`]) and setup/doctor
/// detection, so the two can never disagree about whether a CLI is usable —
/// there is one resolution path in the codebase, not two. `path` is an
/// explicit, injectable PATH string (colon-separated) rather than always
/// reading the current process's environment, so callers can ask "would
/// this resolve under *this* PATH" — e.g. doctor comparing the interactive
/// shell's PATH against the daemon's captured PATH (B40).
pub fn resolve_binary_in(
    binary: &str,
    path: &str,
) -> std::result::Result<(PathBuf, ResolutionStep), BinaryResolutionError> {
    let b = Path::new(binary);
    if b.is_absolute() {
        return Ok((b.to_path_buf(), ResolutionStep::AbsolutePath));
    }

    if let Ok(resolved) = which::which_in(binary, Some(path), ".") {
        return Ok((resolved, ResolutionStep::Path));
    }

    Err(BinaryResolutionError {
        binary: binary.to_string(),
        path: path.to_string(),
    })
}

/// Resolve a binary against an explicit PATH string (colon-separated).
///
/// Tests inject a controlled PATH so they never depend on the developer's
/// real environment.
fn resolve_binary_with_path(binary: &str, path: &str) -> Result<PathBuf> {
    resolve_binary_in(binary, path)
        .map(|(resolved, _step)| resolved)
        .map_err(Into::into)
}

/// How long an identity check may run before it is treated as a failure.
/// Diagnosis-only (probe/doctor, CB44) — never on dispatch.
pub const IDENTITY_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Maximum characters of captured output kept in a [`WrongBinaryError`].
pub const IDENTITY_CHECK_OUTPUT_LIMIT: usize = 500;

/// The resolved binary does not identify as the platform it was resolved
/// for (CB44): the registry's `identity_check` ran against the resolved
/// path and its output did not contain the expected substring.
///
/// The captured output is evidence for the report and nothing else — callers
/// must not scrape it for quota/model meaning.
#[derive(Debug, thiserror::Error)]
#[error("Resolved '{resolved}' which does not identify as the {platform} CLI (expected '{expected}' in output; saw: {output}")]
pub struct WrongBinaryError {
    pub platform: String,
    pub resolved: PathBuf,
    pub expected: String,
    pub output: String,
}

impl WrongBinaryError {
    /// The full failure text: absolute path, platform, expected substring,
    /// invoked command, and truncated evidence. This wording (not "platform
    /// unreachable") is the whole value of CB44 — it ends the months-long
    /// "blackbox is broken" misdiagnosis by naming what was actually found.
    pub fn report(&self, binary: &str, cmd: &str) -> String {
        format!(
            "Resolved '{}', which does not identify as the {} CLI (expected '{}' in output of '{} {}'; saw: {})",
            self.resolved.display(),
            self.platform,
            self.expected,
            binary,
            cmd,
            self.output,
        )
    }
}

/// Truncate captured output to [`IDENTITY_CHECK_OUTPUT_LIMIT`] chars for
/// the report, keeping the head (where `--version`-shaped output lives).
pub fn truncate_identity_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= IDENTITY_CHECK_OUTPUT_LIMIT {
        return trimmed.to_string();
    }
    format!("{}…", &trimmed[..IDENTITY_CHECK_OUTPUT_LIMIT])
}

/// Case-insensitive substring check of combined stdout+stderr against the
/// registry's expected token. Pure so unit tests can pin the matching rule
/// without spawning a process.
fn identity_output_matches(combined: &str, expected: &str) -> bool {
    combined.to_lowercase().contains(&expected.to_lowercase())
}

fn wrong_binary_error(
    cli: &super::cli_config::CliConfig,
    resolved: &Path,
    expected: &str,
    output: &str,
) -> WrongBinaryError {
    WrongBinaryError {
        platform: cli.name.clone(),
        resolved: resolved.to_path_buf(),
        expected: expected.to_string(),
        output: truncate_identity_output(output),
    }
}

/// Synchronous identity check for sync contexts (doctor). Runs
/// `<resolved> <check.cmd>` with stdin nulled, no network, no credentials,
/// and judges combined stdout+stderr against `check.contains`.
///
/// - `Ok(())` when the platform declares no check (backward compatible) or
///   the output contains the expected substring.
/// - Never searches for another candidate binary: reports what was found
///   and lets a human point at the right one via the absolute-path override.
/// - Never called on dispatch (constraint 3) — probe/doctor only.
pub fn verify_identity(
    cli: &super::cli_config::CliConfig,
    resolved: &Path,
) -> std::result::Result<(), WrongBinaryError> {
    let Some(check) = cli.identity_check.as_ref() else {
        return Ok(());
    };
    let args = match shell_words::split(&check.cmd) {
        Ok(args) => args,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to parse identity check command: {e}"),
            ));
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let resolved_owned = resolved.to_path_buf();
    std::thread::spawn(move || {
        let out = std::process::Command::new(&resolved_owned)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output();
        let _ = tx.send(out);
    });
    let output = match rx.recv_timeout(IDENTITY_CHECK_TIMEOUT) {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
        Err(_) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                "identity check timed out",
            ));
        }
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if identity_output_matches(&combined, &check.contains) {
        Ok(())
    } else {
        Err(wrong_binary_error(
            cli,
            resolved,
            &check.contains,
            &combined,
        ))
    }
}

/// Async identity check for async contexts (probe). Same rule as
/// [`verify_identity`]: `<resolved> <check.cmd>` judged against
/// `check.contains`, case-insensitively, on combined stdout+stderr.
pub async fn verify_identity_async(
    cli: &super::cli_config::CliConfig,
    resolved: &Path,
) -> std::result::Result<(), WrongBinaryError> {
    let Some(check) = cli.identity_check.as_ref() else {
        return Ok(());
    };
    let args = match shell_words::split(&check.cmd) {
        Ok(args) => args,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to parse identity check command: {e}"),
            ));
        }
    };
    let mut cmd = tokio::process::Command::new(resolved);
    cmd.args(&args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
    };
    let output = match tokio::time::timeout(IDENTITY_CHECK_TIMEOUT, child.wait_with_output()).await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                &format!("failed to run identity check: {e}"),
            ));
        }
        Err(_) => {
            return Err(wrong_binary_error(
                cli,
                resolved,
                &check.contains,
                "identity check timed out",
            ));
        }
    };
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if identity_output_matches(&combined, &check.contains) {
        Ok(())
    } else {
        Err(wrong_binary_error(
            cli,
            resolved,
            &check.contains,
            &combined,
        ))
    }
}

/// One parsed piece of a template token: either literal text, or a
/// `{{marker}}` (required) / `{{marker?}}` (optional) reference.
enum TokenPart<'a> {
    Literal(&'a str),
    Marker { name: &'a str, optional: bool },
}

impl CliStrategy {
    /// Build a strategy straight from a registry [`CliConfig`] entry — the
    /// one place that lists every field this struct mirrors from it, so
    /// [`super::models::Cli::strategy`] and anything else that needs a
    /// strategy from a resolved config (e.g. the platform probe) can never
    /// drift apart by hand-copying the field list twice.
    ///
    /// CB44: `identity_check` is deliberately NOT mirrored here. Dispatch
    /// builds every command through this strategy, so leaving the check out
    /// keeps verification diagnosis-only (probe/doctor) with zero per-call
    /// tax — a user who never runs doctor or probe keeps today's behaviour.
    ///
    /// [`CliConfig`]: super::cli_config::CliConfig
    pub fn from_cli_config(cli_config: &super::cli_config::CliConfig) -> Self {
        Self {
            binary: cli_config.binary.clone(),
            headless_mode: cli_config.headless_mode.clone(),
            model_flag: cli_config.model_flag.clone(),
            supports_working_dir: cli_config.supports_working_dir,
            working_dir_flag: cli_config.working_dir_flag.clone(),
            env_vars: cli_config.env_vars.clone(),
            prompt_via_stdin: cli_config.prompt_via_stdin,
            session_id_set_flag: cli_config.session_id_set_flag.clone(),
            session_list_cmd: cli_config.session_list_cmd.clone(),
            session_list_format_args: cli_config.session_list_format_args.clone(),
            session_id_pattern: cli_config.session_id_pattern.clone(),
            session_resume_cmd: cli_config.session_resume_cmd.clone(),
            trust_flag: cli_config.trust_flag.clone(),
            invocation_template: cli_config.invocation_template.clone(),
            effort_declaration: cli_config.effort_declaration.clone(),
        }
    }

    /// The model-selection flag, but only when it names one. `None` when the
    /// platform has no `model_flag` OR it is blank (`model_flag = ""`) — in
    /// both cases a requested model must be omitted from argv entirely, never
    /// rendered as an empty word or a bare value.
    fn selectable_model_flag(&self) -> Option<&str> {
        let f = self.model_flag.as_deref()?;
        crate::domain::cli_config::model_flag_selects_model(Some(f)).then_some(f)
    }

    /// Return a copy of this strategy with `prompt_via_stdin` forced to
    /// `true`. Used by the graph engine when the composed prompt exceeds
    /// the OS argv size limit — delivering via stdin avoids E2BIG
    /// regardless of what the CLI's registered capability says.
    pub fn with_stdin_forced(&self) -> Self {
        Self {
            prompt_via_stdin: true,
            ..self.clone()
        }
    }

    /// Build a command using the registry-defined configuration.
    ///
    /// Resolves `self.binary` to an actual executable path first, so a
    /// missing CLI fails with a clear message instead of a bare
    /// `os error 2` once the process is spawned.
    pub fn build_command(
        &self,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
    ) -> Result<Command> {
        self.build_command_with_session(prompt, model, working_dir, None, None)
    }

    /// [`build_command`], additionally injecting a caller-chosen session id
    /// via the registry's `session_id_set_flag` (RS1 set-at-spawn capture).
    /// The id is silently dropped when the CLI has no such flag — callers
    /// decide whether to mint one by checking `session_id_set_flag` first.
    ///
    /// [`build_command`]: Self::build_command
    pub fn build_command_with_session(
        &self,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        session_id: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Command> {
        // Cold start: inject the set-at-spawn flag + id only when both the
        // registry flag and a caller-minted id exist.
        let session_arg = match (self.session_id_set_flag.as_deref(), session_id) {
            (Some(flag), Some(id)) => Some((flag, id)),
            _ => None,
        };
        self.build_headless_command(prompt, model, working_dir, session_arg, None, effort)
    }

    /// CM5: build a headless command that points the CLI at a canopy-synthesized
    /// MCP config file (via the `{{mcp_config}}` invocation-template marker)
    /// instead of the platform's global config, so an ephemeral subagent only
    /// sees the MCP surface it was granted. A platform with no
    /// `invocation_template` has no marker to inject: `mcp_config_path` is then
    /// inert and the CLI runs with its full global surface (the caller warns).
    pub fn build_command_with_mcp_config(
        &self,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        mcp_config_path: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Command> {
        self.build_headless_command(prompt, model, working_dir, None, mcp_config_path, effort)
    }

    /// Build a headless command that RESUMES an existing session by id (RS2).
    /// Identical argv layout to a cold start except the registry's
    /// `session_resume_cmd` flag + `session_id` are injected before the
    /// prompt, in place of the set-at-spawn flag. Errors (never silently cold
    /// starts) when the platform has no `session_resume_cmd` — callers must
    /// gate on [`Self::supports_resume_by_id`] first, so reaching here without
    /// it is a bug.
    pub fn build_resume_command(
        &self,
        session_id: &str,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
    ) -> Result<Command> {
        let flag = self.session_resume_cmd.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "CLI '{}' has no session_resume_cmd; cannot resume by id",
                self.binary
            )
        })?;
        self.build_headless_command(
            prompt,
            model,
            working_dir,
            Some((flag, session_id)),
            None,
            None,
        )
    }

    /// Whether this platform can resume a specific session by id in headless
    /// mode (RS2) — i.e. the registry gives it a `session_resume_cmd`.
    pub fn supports_resume_by_id(&self) -> bool {
        self.session_resume_cmd.is_some()
    }

    /// Build argv from the invocation template. Returns the substituted
    /// argv words (without headless flags) for the given marker values.
    /// Effort and mcp_config are declared but not yet wired (CB3/CB4).
    #[allow(clippy::too_many_arguments)]
    fn build_argv_from_template(
        &self,
        template: &str,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        session_arg: Option<(&str, &str)>,
        effort: Option<&str>,
        mcp_config: Option<&str>,
    ) -> Vec<String> {
        let mut markers: HashMap<&str, Option<&str>> = HashMap::new();
        // When prompt is delivered via stdin, treat {{prompt}} as unavailable
        // so it and any preceding flag are elided from argv.
        if self.prompt_via_stdin {
            markers.insert("prompt", None);
        } else {
            markers.insert("prompt", Some(prompt));
        }
        markers.insert("model", model);
        markers.insert("workdir", working_dir);
        markers.insert("effort", effort);
        markers.insert("mcp_config", mcp_config);
        let session_id_val = session_arg.map(|(_, id)| id);
        markers.insert("session_id", session_id_val);
        let session_flag_val = session_arg.map(|(flag, _)| flag);
        markers.insert("session_flag", session_flag_val);

        let tokens: Vec<&str> = template.split_whitespace().collect();
        let mut argv: Vec<String> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let token = tokens[i];
            let has_marker = token.contains("{{");
            if !has_marker {
                let next_dropped = if i + 1 < tokens.len() {
                    let next = tokens[i + 1];
                    next.contains("{{") && Self::resolve_token(next, &markers).is_none()
                } else {
                    false
                };
                if token.starts_with('-') && next_dropped {
                    i += 1;
                    continue;
                }
                argv.push(token.to_string());
                i += 1;
            } else {
                if let Some(rendered) = Self::resolve_token(token, &markers) {
                    argv.push(rendered);
                }
                i += 1;
            }
        }
        argv
    }

    /// FR4: a template's "model-bearing" token (the one carrying the `model`
    /// marker) must never be able to render empty — that would silently drop
    /// the whole model argument, the exact defect this spec exists to fix,
    /// just hidden behind the `?` spelling instead of the old
    /// drop-whole-token-on-any-missing-marker rule. A token where EVERY marker
    /// in it (including `model` itself, i.e. spelled `{{model?}}`) is optional
    /// can do exactly that when model is absent. `{{model}}` (required, the
    /// only spelling a model-bearing token should ever use) is always safe and
    /// never triggers this.
    ///
    /// `template` is the raw `invocation_template` string (no `None` case here
    /// — callers only invoke this when a template exists). Returns `Some(msg)`
    /// naming the platform and the offending token; `None` when the template
    /// has no unsafe model-bearing token.
    pub fn unsafe_optional_model_token(template: &str, platform: &str) -> Option<String> {
        for token in template.split_whitespace() {
            let parts = Self::parse_token_parts(token);
            let has_model_marker = parts
                .iter()
                .any(|p| matches!(p, TokenPart::Marker { name, .. } if *name == "model"));
            if !has_model_marker {
                continue;
            }
            let all_optional = parts.iter().all(|p| match p {
                TokenPart::Marker { optional, .. } => *optional,
                TokenPart::Literal(_) => true,
            });
            if all_optional {
                return Some(format!(
                    "platform '{platform}' invocation_template token '{token}' has only \
                     optional markers including {{{{model?}}}} — it would render empty \
                     (silently dropping the model argument) whenever model is absent. \
                     Spell the model marker as required: '{{{{model}}}}'."
                ));
            }
        }
        None
    }

    /// One parsed piece of a template token: either literal text, or a
    /// `{{marker}}` (required) / `{{marker?}}` (optional) reference.
    fn parse_token_parts(token: &str) -> Vec<TokenPart<'_>> {
        let mut parts = Vec::new();
        let mut rest = token;
        loop {
            let Some(start) = rest.find("{{") else {
                if !rest.is_empty() {
                    parts.push(TokenPart::Literal(rest));
                }
                break;
            };
            if start > 0 {
                parts.push(TokenPart::Literal(&rest[..start]));
            }
            let after_open = &rest[start + 2..];
            let Some(end) = after_open.find("}}") else {
                parts.push(TokenPart::Literal(&rest[start..]));
                break;
            };
            let raw_name = &after_open[..end];
            let (name, optional) = match raw_name.strip_suffix('?') {
                Some(n) => (n, true),
                None => (raw_name, false),
            };
            parts.push(TokenPart::Marker { name, optional });
            rest = &after_open[end + 2..];
        }
        parts
    }

    /// Resolve one template token against the marker values. `None` means the
    /// token is dropped entirely — either because a REQUIRED marker in it is
    /// unavailable (today's rule, unchanged: FR3), or because after removing
    /// every unavailable OPTIONAL marker and its bound literal glue (FR1/FR2)
    /// nothing is left to render (FR2: "a token that becomes empty is
    /// dropped"). `Some(rendered)` is the fully substituted token.
    fn resolve_token(token: &str, markers: &HashMap<&str, Option<&str>>) -> Option<String> {
        let parts = Self::parse_token_parts(token);

        // FR3: any unavailable REQUIRED marker drops the whole token, exactly
        // as `all_markers_available` did before — optional markers never
        // participate in this check.
        for part in &parts {
            if let TokenPart::Marker {
                name,
                optional: false,
            } = part
            {
                markers.get(name).copied().flatten()?;
            }
        }

        // FR1/FR2: render left to right. A literal immediately followed by an
        // unavailable OPTIONAL marker is the "glue bound to" that marker (FR2)
        // — both are dropped as a pair. Every other literal, and every
        // available marker's value, is emitted normally.
        let mut out = String::new();
        let mut i = 0;
        while i < parts.len() {
            match &parts[i] {
                TokenPart::Literal(s) => {
                    let glue_drops = matches!(
                        parts.get(i + 1),
                        Some(TokenPart::Marker { name, optional: true })
                            if markers.get(name).copied().flatten().is_none()
                    );
                    if glue_drops {
                        i += 2;
                    } else {
                        out.push_str(s);
                        i += 1;
                    }
                }
                TokenPart::Marker { name, .. } => {
                    if let Some(val) = markers.get(name).copied().flatten() {
                        out.push_str(val);
                    }
                    // Unavailable here can only be an optional marker (a
                    // required-unavailable one already returned above) — skip
                    // it silently, its own glue (if any) was already consumed
                    // in the Literal arm above, or there was none to consume.
                    i += 1;
                }
            }
        }

        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Shared core for every headless spawn (cold or resume). `session_arg`,
    /// when `Some((flag, id))`, injects that flag + id immediately before the
    /// positional prompt so the id can never be mistaken for the prompt. The
    /// cold path passes the set-at-spawn flag; the resume path passes the
    /// resume flag; a plain `build_command` passes `None`. Keeping this one
    /// function means the cold layout is byte-identical whichever caller runs.
    fn build_headless_command(
        &self,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        session_arg: Option<(&str, &str)>,
        mcp_config: Option<&str>,
        effort: Option<&str>,
    ) -> Result<Command> {
        let resolved = resolve_binary(&self.binary)?;
        let mut cmd = Command::new(resolved);

        // Make the child its own process-group leader so the engine can
        // `killpg` it (and any helpers it forks) as a unit on timeout/abnormal
        // end (B12), instead of leaving them to keep running past the daemon's
        // control. `kill_on_drop` is a cross-platform safety net for the
        // direct child alone, in case the `Command`/`Child` is ever dropped
        // without an explicit kill.
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);

        // Set environment variables
        for (key, value) in &self.env_vars {
            cmd.env(key, value);
        }

        if let Some(ref template) = self.invocation_template {
            // Template-driven argv assembly (CB2).
            let argv = self.build_argv_from_template(
                template,
                prompt,
                model,
                working_dir,
                session_arg,
                effort,
                mcp_config,
            );

            // Headless flags are always prepended (not part of template).
            for arg in shell_words::split(&self.headless_mode).unwrap_or_default() {
                cmd.arg(arg);
            }

            // Deliver prompt via stdin vs argv. When prompt_via_stdin is true,
            // {{prompt}} is already elided (markers inserted as None), but for
            // safety filter any argv word that equals the prompt text.
            for arg in &argv {
                if self.prompt_via_stdin && arg == prompt {
                    continue;
                }
                cmd.arg(arg);
            }

            if self.prompt_via_stdin {
                let mut file =
                    tempfile::tempfile().context("failed to create temp file for prompt")?;
                file.write_all(prompt.as_bytes())
                    .context("failed to write prompt to temp file")?;
                file.seek(SeekFrom::Start(0))
                    .context("failed to rewind prompt temp file")?;
                cmd.stdin(std::process::Stdio::from(file));
            } else {
                cmd.stdin(std::process::Stdio::null());
            }
        } else {
            // Legacy: fixed-order assembly (unchanged).
            // Add headless mode flags (before prompt)
            for arg in shell_words::split(&self.headless_mode).unwrap_or_default() {
                cmd.arg(arg);
            }

            // Inject the session flag + id (set-at-spawn for a cold start, or the
            // resume-by-id flag for a resume) before the positional prompt.
            if let Some((flag, id)) = session_arg {
                cmd.arg(flag).arg(id);
            }

            // Deliver the prompt via stdin (backed by an anonymous temp file) or
            // argv, per the CLI's registered capability. argv has an OS-level
            // per-argument/argv size cliff (Linux MAX_ARG_STRLEN, ARG_MAX) that a
            // large composed prompt (e.g. one embedding a prior node's full
            // output) can cross, crashing the spawn with E2BIG. Node outputs are
            // arbitrarily large, so any CLI that can read the prompt from stdin
            // instead should.
            if self.prompt_via_stdin {
                let mut file =
                    tempfile::tempfile().context("failed to create temp file for prompt")?;
                file.write_all(prompt.as_bytes())
                    .context("failed to write prompt to temp file")?;
                file.seek(SeekFrom::Start(0))
                    .context("failed to rewind prompt temp file")?;
                cmd.stdin(std::process::Stdio::from(file));
            } else {
                cmd.arg(prompt);
                cmd.stdin(std::process::Stdio::null());
            }

            // Add model if specified — but only when the platform actually
            // declares a way to select one. A blank `model_flag` (CB34,
            // antigravity's `model_flag = ""`) is NOT a flag: emitting it puts
            // a stray empty argv word on the command line that the CLI
            // rejects before the run starts. Treat blank like `None` and omit
            // the model entirely; the caller records a not-applied notice.
            if let Some(m) = model {
                if let Some(flag) = self.selectable_model_flag() {
                    cmd.arg(flag).arg(m);
                }
            }

            // Add working directory if supported
            if self.supports_working_dir {
                if let Some(dir) = working_dir {
                    if let Some(ref flag) = self.working_dir_flag {
                        cmd.arg(flag).arg(dir);
                    }
                }
            }
        }

        Ok(cmd)
    }

    /// Whether list-after-run session id capture (RS1 phase 2) applies to
    /// this platform: it exposes a session-list command AND an id-extraction
    /// pattern, and has NO set-at-spawn flag. Set-at-spawn takes strict
    /// precedence — when it exists the id is known before the process starts,
    /// so the engine must never fall back to diffing session lists.
    pub fn can_capture_session_after_run(&self) -> bool {
        self.session_id_set_flag.is_none()
            && self.session_list_cmd.is_some()
            && self.session_id_pattern.is_some()
    }

    /// Build the registry-defined session-list command, to be run with the
    /// node's workdir as cwd (several CLIs scope their session list to the
    /// current project). Returns `Ok(None)` when the platform has no
    /// `session_list_cmd`. Only listing args are added — never the prompt,
    /// model, headless, or session-id-set flags — so this can never start a
    /// real session or consume model quota.
    pub fn build_session_list_command(&self, working_dir: &str) -> Result<Option<Command>> {
        let Some(list_cmd) = self.session_list_cmd.as_deref() else {
            return Ok(None);
        };
        let resolved = resolve_binary(&self.binary)?;
        let mut cmd = Command::new(resolved);

        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);

        for (key, value) in &self.env_vars {
            cmd.env(key, value);
        }
        for arg in shell_words::split(list_cmd).unwrap_or_default() {
            cmd.arg(arg);
        }
        if let Some(fmt) = self.session_list_format_args.as_deref() {
            for arg in shell_words::split(fmt).unwrap_or_default() {
                cmd.arg(arg);
            }
        }
        cmd.current_dir(working_dir);
        cmd.stdin(std::process::Stdio::null());
        Ok(Some(cmd))
    }

    /// Extract the set of session ids from list-command output using the
    /// registry-configured `session_id_pattern`. Capture group 1 is the id
    /// when the pattern has one; otherwise the whole match. Returns an empty
    /// set when no pattern is configured or it fails to compile — capture is
    /// best-effort and never surfaces an error to the run.
    pub fn extract_session_ids(&self, output: &str) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        let Some(pattern) = self.session_id_pattern.as_deref() else {
            return ids;
        };
        let Ok(re) = regex::Regex::new(pattern) else {
            return ids;
        };
        for caps in re.captures_iter(output) {
            if let Some(m) = caps.get(1).or_else(|| caps.get(0)) {
                ids.insert(m.as_str().to_string());
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uses an absolute (non-existent) path for `binary` so tests don't
    /// depend on any real CLI being installed on the machine running them.
    fn sample_strategy() -> CliStrategy {
        let mut env_vars = HashMap::new();
        env_vars.insert("FOO".to_string(), "bar".to_string());

        CliStrategy {
            binary: "/usr/local/bin/test-cli".to_string(),
            headless_mode: "--headless --quiet".to_string(),
            model_flag: Some("--model".to_string()),
            supports_working_dir: true,
            working_dir_flag: Some("--workdir".to_string()),
            env_vars,
            prompt_via_stdin: false,
            session_id_set_flag: None,
            session_list_cmd: None,
            session_list_format_args: None,
            session_id_pattern: None,
            session_resume_cmd: None,
            trust_flag: None,
            invocation_template: None,
            effort_declaration: None,
        }
    }

    #[test]
    fn test_build_command_basic() {
        let strategy = sample_strategy();
        let cmd = strategy.build_command("test prompt", None, None).unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("test-cli"));
    }

    #[test]
    fn test_build_command_with_model() {
        let strategy = sample_strategy();
        let cmd = strategy
            .build_command("test prompt", Some("gpt-4"), None)
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--model"));
        assert!(cmd_str.contains("gpt-4"));
    }

    #[test]
    fn test_build_command_with_working_dir() {
        let strategy = sample_strategy();
        let cmd = strategy
            .build_command("test prompt", None, Some("/tmp/project"))
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--workdir"));
        assert!(cmd_str.contains("/tmp/project"));
    }

    #[test]
    fn test_build_command_no_working_dir_when_not_supported() {
        let mut strategy = sample_strategy();
        strategy.supports_working_dir = false;

        let cmd = strategy
            .build_command("test prompt", None, Some("/tmp/project"))
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("--workdir"));
    }

    #[test]
    fn test_build_command_no_model_flag() {
        let mut strategy = sample_strategy();
        strategy.model_flag = None;

        let cmd = strategy
            .build_command("test prompt", Some("gpt-4"), None)
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("--model"));
    }

    /// CB34: a blank `model_flag` (antigravity's `model_flag = ""`) must not put
    /// an empty argv word — or the bare model value — on the command line.
    #[test]
    fn build_command_blank_model_flag_omits_model_from_argv() {
        let mut strategy = sample_strategy();
        strategy.model_flag = Some(String::new());

        let cmd = strategy
            .build_command("test prompt", Some("gpt-4"), None)
            .unwrap();

        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(
            !args.iter().any(|a| a.is_empty()),
            "blank model_flag must not render an empty argv word: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "gpt-4"),
            "the model value must be omitted, not passed bare: {args:?}"
        );
    }

    /// A real `model_flag` is unaffected: flag + value still land in argv, adjacent.
    #[test]
    fn build_command_real_model_flag_still_passes_model_in_argv() {
        let strategy = sample_strategy(); // model_flag = Some("--model")

        let cmd = strategy
            .build_command("test prompt", Some("gpt-4"), None)
            .unwrap();

        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        let i = args
            .iter()
            .position(|a| a == "--model")
            .expect("--model must be present");
        assert_eq!(args.get(i + 1).map(String::as_str), Some("gpt-4"));
    }

    #[test]
    fn test_build_command_empty_headless_mode() {
        let mut strategy = sample_strategy();
        strategy.headless_mode = String::new();

        let cmd = strategy.build_command("test prompt", None, None).unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("test-cli"));
    }

    #[test]
    fn test_with_stdin_forced_overrides_flag() {
        let mut strategy = sample_strategy();
        strategy.prompt_via_stdin = false;
        let forced = strategy.with_stdin_forced();
        assert!(
            forced.prompt_via_stdin,
            "with_stdin_forced must set prompt_via_stdin to true"
        );
        assert!(!strategy.prompt_via_stdin, "original must be unchanged");
        assert_eq!(
            strategy.binary, forced.binary,
            "all other fields must be preserved"
        );
    }

    #[test]
    fn test_build_command_prompt_via_stdin_keeps_prompt_out_of_argv() {
        let mut strategy = sample_strategy();
        strategy.prompt_via_stdin = true;

        let cmd = strategy
            .build_command("this must not appear in argv", None, None)
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("this must not appear in argv"));
    }

    #[tokio::test]
    async fn test_build_command_prompt_via_stdin_delivers_huge_prompt() {
        // A multi-hundred-KB prompt would blow argv (Linux MAX_ARG_STRLEN is
        // 128KiB) if passed via `cmd.arg`. Piped via stdin it has no
        // input-size cliff: spawn `cat`, which just echoes stdin to stdout.
        let mut strategy = sample_strategy();
        strategy.binary = "/bin/cat".to_string();
        strategy.headless_mode = String::new();
        strategy.model_flag = None;
        strategy.supports_working_dir = false;
        strategy.prompt_via_stdin = true;

        let huge_prompt = "x".repeat(500 * 1024);
        let mut cmd = strategy.build_command(&huge_prompt, None, None).unwrap();
        cmd.stdout(std::process::Stdio::piped());

        let output = cmd.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), huge_prompt);
    }

    #[test]
    fn build_command_with_session_injects_set_flag_and_id() {
        let mut strategy = sample_strategy();
        strategy.session_id_set_flag = Some("--session-id".to_string());
        let cmd = strategy
            .build_command_with_session(
                "p",
                None,
                None,
                Some("11111111-2222-3333-4444-555555555555"),
                None,
            )
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--session-id"));
        assert!(cmd_str.contains("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn build_command_with_session_without_flag_drops_id() {
        // sample_strategy has no session_id_set_flag: the id must be
        // silently dropped, never passed as a stray argument.
        let strategy = sample_strategy();
        let cmd = strategy
            .build_command_with_session("p", None, None, Some("sid-123"), None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("sid-123"));
    }

    #[test]
    fn build_command_never_injects_session_flag_without_id() {
        let mut strategy = sample_strategy();
        strategy.session_id_set_flag = Some("--session-id".to_string());
        let cmd = strategy.build_command("p", None, None).unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("--session-id"));
    }

    #[test]
    fn test_build_command_all_options() {
        let strategy = sample_strategy();
        let cmd = strategy
            .build_command("my prompt", Some("claude-3"), Some("/home/project"))
            .unwrap();

        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("my prompt"));
        assert!(cmd_str.contains("--model"));
        assert!(cmd_str.contains("claude-3"));
        assert!(cmd_str.contains("--workdir"));
        assert!(cmd_str.contains("/home/project"));
    }

    #[test]
    fn can_capture_session_after_run_requires_list_and_pattern_without_set_flag() {
        let mut s = sample_strategy();
        assert!(!s.can_capture_session_after_run(), "nothing configured");

        s.session_list_cmd = Some("session list".to_string());
        assert!(!s.can_capture_session_after_run(), "pattern still missing");

        s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
        assert!(s.can_capture_session_after_run(), "list + pattern present");

        // Set-at-spawn takes precedence and disables list-after-run capture.
        s.session_id_set_flag = Some("--session-id".to_string());
        assert!(!s.can_capture_session_after_run(), "set-at-spawn wins");
    }

    #[test]
    fn extract_session_ids_pulls_id_key_from_opencode_family_json() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
        // Real opencode/mimo/kilo shape: a JSON array whose objects also carry
        // a `projectId` (which must NOT be mistaken for `id`).
        let output = r#"[
          {"id": "ses_AAA", "projectId": "hexhexhex", "directory": "/x"},
          {"id": "ses_BBB", "projectId": "hexhexhex", "directory": "/y"}
        ]"#;
        let ids = s.extract_session_ids(output);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("ses_AAA"));
        assert!(ids.contains("ses_BBB"));
    }

    #[test]
    fn extract_session_ids_pulls_id_key_from_cn_json() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some("\"id\"\\s*:\\s*\"([^\"]+)\"".to_string());
        // Real cn shape: an object wrapping a `sessions` array.
        let output = r#"{"sessions": [
          {"id": "8ae15a84-fec0-43b4-9cb8-47293662302e", "title": "x"},
          {"id": "633286f6-0820-46a5-8c8f-ed315faa5e49", "title": "y"}
        ]}"#;
        let ids = s.extract_session_ids(output);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("8ae15a84-fec0-43b4-9cb8-47293662302e"));
    }

    #[test]
    fn extract_session_ids_empty_without_pattern() {
        let s = sample_strategy();
        assert!(s.extract_session_ids(r#"[{"id":"ses_X"}]"#).is_empty());
    }

    #[test]
    fn build_session_list_command_none_without_list_cmd() {
        let s = sample_strategy();
        assert!(s.build_session_list_command("/tmp").unwrap().is_none());
    }

    #[test]
    fn build_session_list_command_appends_format_args_and_sets_cwd() {
        let mut s = sample_strategy();
        s.session_list_cmd = Some("session list".to_string());
        s.session_list_format_args = Some("--format json".to_string());
        let cmd = s
            .build_session_list_command("/tmp/project")
            .unwrap()
            .expect("list command must be built");
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("session"));
        assert!(cmd_str.contains("list"));
        assert!(cmd_str.contains("--format"));
        assert!(cmd_str.contains("json"));
        // The prompt/headless/model flags must never appear on a list command.
        assert!(!cmd_str.contains("--headless"));
        assert!(!cmd_str.contains("--model"));
    }

    #[test]
    fn supports_resume_by_id_follows_session_resume_cmd() {
        let mut s = sample_strategy();
        assert!(!s.supports_resume_by_id());
        s.session_resume_cmd = Some("--session".to_string());
        assert!(s.supports_resume_by_id());
    }

    #[test]
    fn build_resume_command_injects_resume_flag_and_id_before_prompt() {
        let mut s = sample_strategy();
        s.session_resume_cmd = Some("--session".to_string());
        let cmd = s
            .build_resume_command("ses_abc", "the prompt", None, None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--session"));
        assert!(cmd_str.contains("ses_abc"));
        // The resume flag+id must precede the positional prompt.
        let flag_at = cmd_str.find("--session").unwrap();
        let prompt_at = cmd_str.find("the prompt").unwrap();
        assert!(
            flag_at < prompt_at,
            "resume flag+id must come before prompt"
        );
    }

    #[test]
    fn build_resume_command_errors_without_session_resume_cmd() {
        let s = sample_strategy();
        let err = s
            .build_resume_command("ses_abc", "p", None, None)
            .unwrap_err();
        assert!(err.to_string().contains("session_resume_cmd"));
    }

    #[test]
    fn build_resume_command_never_uses_set_at_spawn_flag() {
        // A platform can have BOTH a set-at-spawn flag and a resume flag;
        // resume must use the resume flag, not mint via the set-at-spawn one.
        let mut s = sample_strategy();
        s.session_id_set_flag = Some("--session-id".to_string());
        s.session_resume_cmd = Some("--resume".to_string());
        let cmd = s.build_resume_command("ses_xyz", "p", None, None).unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--resume"));
        assert!(cmd_str.contains("ses_xyz"));
        assert!(!cmd_str.contains("--session-id"));
    }

    #[test]
    fn resolve_binary_absolute_path_used_as_is_without_touching_path() {
        // Deliberately a path that does not exist: absolute paths must be
        // returned verbatim, with no PATH lookup and no existence check.
        let resolved = resolve_binary_with_path("/nonexistent/somewhere/mimo", "").unwrap();
        assert_eq!(resolved, PathBuf::from("/nonexistent/somewhere/mimo"));
    }

    #[test]
    fn daemon_path_at_none_when_unit_file_missing() {
        let home = tempfile::tempdir().unwrap();
        assert!(daemon_path_at(home.path()).is_none());
    }

    #[test]
    fn daemon_path_at_none_when_unit_has_no_path_line() {
        let home = tempfile::tempdir().unwrap();
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        std::fs::write(
            unit_dir.join("canopy.service"),
            "[Service]\nExecStart=/usr/local/bin/canopy daemon run\n",
        )
        .unwrap();
        assert!(daemon_path_at(home.path()).is_none());
    }

    #[test]
    fn daemon_path_at_reads_environment_path_line() {
        let home = tempfile::tempdir().unwrap();
        let unit_dir = home.path().join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).unwrap();
        std::fs::write(
            unit_dir.join("canopy.service"),
            "[Service]\nEnvironment=PATH=/usr/bin:/bin:/home/user/.opencode/bin\nExecStart=/usr/local/bin/canopy daemon run\n",
        )
        .unwrap();
        assert_eq!(
            daemon_path_at(home.path()),
            Some("/usr/bin:/bin:/home/user/.opencode/bin".to_string())
        );
    }

    #[test]
    fn resolve_binary_not_found_names_binary_and_searched_path() {
        let err = resolve_binary_with_path("canopy-test-fixture-cli-missing", "/usr/bin:/bin")
            .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("canopy-test-fixture-cli-missing"));
        assert!(message.contains("/usr/bin:/bin"));
    }

    #[test]
    fn resolution_step_label_absolute_path() {
        assert_eq!(ResolutionStep::AbsolutePath.label(), "absolute path");
    }

    #[test]
    fn resolution_step_label_path() {
        assert_eq!(ResolutionStep::Path.label(), "PATH");
    }

    #[test]
    fn resolution_step_equality() {
        assert_eq!(ResolutionStep::AbsolutePath, ResolutionStep::AbsolutePath);
        assert_eq!(ResolutionStep::Path, ResolutionStep::Path);
        assert_ne!(ResolutionStep::AbsolutePath, ResolutionStep::Path);
    }

    #[test]
    fn extract_session_ids_invalid_regex_returns_empty() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some("[invalid".to_string());
        let ids = s.extract_session_ids(r#"[{"id":"ses_X"}]"#);
        assert!(ids.is_empty());
    }

    #[test]
    fn extract_session_ids_no_match_returns_empty() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some(r#""id"\s*:\s*"([^"]+)""#.to_string());
        let ids = s.extract_session_ids("no ids here at all");
        assert!(ids.is_empty());
    }

    #[test]
    fn extract_session_ids_uses_group_1_when_present() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some(r#"session_(\w+)"#.to_string());
        let ids = s.extract_session_ids("session_abc session_xyz");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("abc"));
        assert!(ids.contains("xyz"));
    }

    #[test]
    fn extract_session_ids_falls_back_to_full_match_without_group() {
        let mut s = sample_strategy();
        s.session_id_pattern = Some(r#""id"\s*:\s*"[^"]+""#.to_string());
        let ids = s.extract_session_ids(r#""id": "ses_full""#);
        assert_eq!(ids.len(), 1);
        // Without a capture group, the full match is used
        assert!(ids.contains(r#""id": "ses_full""#));
    }

    #[test]
    fn with_stdin_forced_preserves_all_other_fields() {
        let mut strategy = sample_strategy();
        strategy.session_id_set_flag = Some("--sid".to_string());
        strategy.session_list_cmd = Some("ls".to_string());
        strategy.session_resume_cmd = Some("--resume".to_string());

        let forced = strategy.with_stdin_forced();

        assert!(forced.prompt_via_stdin);
        assert_eq!(forced.session_id_set_flag.as_deref(), Some("--sid"));
        assert_eq!(forced.session_list_cmd.as_deref(), Some("ls"));
        assert_eq!(forced.session_resume_cmd.as_deref(), Some("--resume"));
        assert_eq!(forced.binary, strategy.binary);
        assert_eq!(forced.headless_mode, strategy.headless_mode);
    }

    #[test]
    fn binary_resolution_error_display() {
        let err = BinaryResolutionError {
            binary: "my-cli".to_string(),
            path: "/usr/bin:/bin".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("my-cli"));
        assert!(msg.contains("/usr/bin:/bin"));
    }

    #[test]
    fn template_prompt_positional_at_end() {
        // Case 1: prompt as positional at end (majority of current platforms).
        let mut s = sample_strategy();
        s.invocation_template =
            Some("--session-id {{session_id}} {{prompt}} --model {{model}}".to_string());
        s.session_id_set_flag = Some("--session-id".to_string());

        let cmd = s
            .build_command_with_session("the prompt", Some("gpt-4"), None, Some("ses_123"), None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);

        // Assert order: headless flags, then session-id, then prompt, then model.
        let headless_at = cmd_str.find("--headless").unwrap();
        let session_at = cmd_str.find("--session-id").unwrap();
        let prompt_at = cmd_str.find("the prompt").unwrap();
        let model_at = cmd_str.find("--model").unwrap();
        assert!(headless_at < session_at);
        assert!(session_at < prompt_at);
        assert!(prompt_at < model_at);
    }

    #[test]
    fn template_prompt_as_flag_value() {
        // Case 2: prompt as value of a flag (copilot -p). The flag that consumes
        // the next argv word must be immediately followed by the prompt, not by
        // --session-id (which would be misinterpreted as the prompt).
        let mut s = sample_strategy();
        s.invocation_template = Some("-p {{prompt}} --session-id {{session_id}}".to_string());
        s.session_id_set_flag = Some("--session-id".to_string());

        let cmd = s
            .build_command_with_session("the prompt", None, None, Some("ses_456"), None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);

        // Assert: -p immediately followed by prompt (no --session-id in between).
        let p_flag_at = cmd_str.find("-p").unwrap();
        let prompt_at = cmd_str.find("the prompt").unwrap();
        let session_at = cmd_str.find("--session-id").unwrap();
        assert!(p_flag_at < prompt_at);
        assert!(prompt_at < session_at);
        // Critical: --session-id must NOT appear between -p and the prompt.
        let between = &cmd_str[p_flag_at..prompt_at];
        assert!(!between.contains("--session-id"));
    }

    #[test]
    fn template_flag_equals_value_form() {
        // Case 3: --flag=value in a single word.
        let mut s = sample_strategy();
        s.invocation_template = Some("--model={{model}} {{prompt}}".to_string());

        let cmd = s.build_command("the prompt", Some("gpt-4"), None).unwrap();
        let cmd_str = format!("{:?}", cmd);

        // Assert: --model=gpt-4 is a single argv word (no space between flag and value).
        assert!(cmd_str.contains("--model=gpt-4"));
        // And it's distinct from --model gpt-4 (two words).
        assert!(!cmd_str.contains("--model \"gpt-4\""));
    }

    #[test]
    fn template_composite_marker_in_value() {
        // Case 4: marker composed within another marker's value (cursor effort).
        let s = sample_strategy();
        let argv = s.build_argv_from_template(
            "{{model}}[context=1m,effort={{effort}},fast=false] {{prompt}}",
            "the prompt",
            Some("claude-opus-4-8"),
            None,
            None,
            Some("high"),
            None,
        );

        assert_eq!(argv.len(), 2);
        assert_eq!(
            argv[0],
            "claude-opus-4-8[context=1m,effort=high,fast=false]"
        );
        assert_eq!(argv[1], "the prompt");
    }

    #[test]
    fn template_elides_unavailable_marker_and_companion_flag() {
        // If {{model}} is unavailable, --model is also elided (no orphan flag).
        let mut s = sample_strategy();
        s.invocation_template = Some("--model {{model}} {{prompt}}".to_string());

        let cmd = s.build_command("the prompt", None, None).unwrap();
        let cmd_str = format!("{:?}", cmd);

        assert!(cmd_str.contains("the prompt"));
        assert!(!cmd_str.contains("--model"));
    }

    #[test]
    fn template_elides_flag_equals_value_when_marker_unavailable() {
        // If {{model}} is unavailable, --model={{model}} is elided as a unit.
        let mut s = sample_strategy();
        s.invocation_template = Some("--model={{model}} {{prompt}}".to_string());

        let cmd = s.build_command("the prompt", None, None).unwrap();
        let cmd_str = format!("{:?}", cmd);

        assert!(cmd_str.contains("the prompt"));
        assert!(!cmd_str.contains("--model"));
    }

    #[test]
    fn template_elides_composite_when_any_marker_unavailable() {
        // If {{effort}} is unavailable, the whole composite token is elided.
        let s = sample_strategy();
        let argv = s.build_argv_from_template(
            "{{model}}[context=1m,effort={{effort}},fast=false] {{prompt}}",
            "the prompt",
            Some("claude-opus-4-8"),
            None,
            None,
            None, // effort unavailable
            None,
        );

        assert_eq!(argv.len(), 1);
        assert_eq!(argv[0], "the prompt");
    }

    #[test]
    fn template_none_falls_back_to_legacy_assembly() {
        // Backward compat: no template → legacy fixed-order behavior.
        let s = sample_strategy(); // invocation_template is None
        let cmd = s
            .build_command("the prompt", Some("gpt-4"), Some("/tmp"))
            .unwrap();
        let cmd_str = format!("{:?}", cmd);

        // Legacy order: headless, prompt, model, workdir.
        assert!(cmd_str.contains("--headless"));
        assert!(cmd_str.contains("the prompt"));
        assert!(cmd_str.contains("--model"));
        assert!(cmd_str.contains("gpt-4"));
        assert!(cmd_str.contains("--workdir"));
        assert!(cmd_str.contains("/tmp"));
    }

    #[test]
    fn template_resume_uses_correct_flag() {
        let mut s = sample_strategy();
        s.invocation_template = Some("{{session_flag}} {{session_id}} {{prompt}}".to_string());
        s.session_resume_cmd = Some("--resume".to_string());
        s.session_id_set_flag = Some("--session-id".to_string());

        let cmd = s
            .build_resume_command("ses_123", "the prompt", None, None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);

        assert!(cmd_str.contains("--resume"));
        assert!(cmd_str.contains("ses_123"));
        assert!(cmd_str.contains("the prompt"));
        // Critical: --session-id must NOT appear (resume uses --resume, not --session-id)
        let resume_at = cmd_str.find("--resume").unwrap();
        let ses_at = cmd_str.find("ses_123").unwrap();
        let between = &cmd_str[resume_at..ses_at];
        assert!(!between.contains("--session-id"));
    }

    #[test]
    fn template_stdin_forced_elides_prompt_marker() {
        let mut s = sample_strategy();
        s.invocation_template = Some("--model {{model}} {{prompt}}".to_string());
        s.prompt_via_stdin = true;

        let cmd = s.build_command("the prompt", Some("gpt-4"), None).unwrap();
        let cmd_str = format!("{:?}", cmd);

        assert!(cmd_str.contains("--model"));
        assert!(cmd_str.contains("gpt-4"));
        // Prompt is delivered via stdin, not argv
        assert!(!cmd_str.contains("the prompt"));
    }

    #[test]
    fn template_workdir_marker() {
        let mut s = sample_strategy();
        s.invocation_template = Some("--workdir {{workdir}} {{prompt}}".to_string());

        let cmd = s
            .build_command("the prompt", None, Some("/tmp/project"))
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--workdir"));
        assert!(cmd_str.contains("/tmp/project"));
        assert!(cmd_str.contains("the prompt"));

        // Without workdir, --workdir is elided
        let cmd2 = s.build_command("the prompt", None, None).unwrap();
        let cmd_str2 = format!("{:?}", cmd2);
        assert!(!cmd_str2.contains("--workdir"));
        assert!(cmd_str2.contains("the prompt"));
    }

    #[test]
    fn template_optional_marker_glue_renders_when_available() {
        let s = sample_strategy();
        let argv = s.build_argv_from_template(
            "-m {{model}}#{{effort?}} {{prompt}}",
            "implement",
            Some("opencode/big-pickle"),
            None,
            None,
            Some("high"),
            None,
        );
        assert_eq!(argv, vec!["-m", "opencode/big-pickle#high", "implement"]);
    }

    #[test]
    fn template_optional_marker_glue_elided_when_absent() {
        let s = sample_strategy();
        let argv = s.build_argv_from_template(
            "-m {{model}}#{{effort?}} {{prompt}}",
            "implement",
            Some("opencode/big-pickle"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(argv, vec!["-m", "opencode/big-pickle", "implement"]);
        assert!(argv.contains(&"opencode/big-pickle".to_string()));
        assert!(!argv.iter().any(|w| w.contains('#')));
    }

    #[test]
    fn template_optional_marker_hyphen_glue() {
        let s = sample_strategy();
        let template = "--model {{model}}-{{effort?}} {{prompt}}";
        let with_effort = s.build_argv_from_template(
            template,
            "go",
            Some("claude-sonnet-5"),
            None,
            None,
            Some("high"),
            None,
        );
        assert_eq!(with_effort, vec!["--model", "claude-sonnet-5-high", "go"]);
        let without_effort = s.build_argv_from_template(
            template,
            "go",
            Some("claude-sonnet-5"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(without_effort, vec!["--model", "claude-sonnet-5", "go"]);
    }

    #[test]
    fn template_required_marker_still_drops_token_and_flag() {
        let s = sample_strategy();
        let template = "--variant {{effort}} {{prompt}}";
        let argv = s.build_argv_from_template(template, "go", None, None, None, None, None);
        assert_eq!(argv, vec!["go"]);
        assert!(!argv.contains(&"--variant".to_string()));
    }

    #[test]
    fn template_optional_marker_alone_in_token_drops_with_flag() {
        let s = sample_strategy();
        let template = "--variant {{effort?}} {{prompt}}";
        let argv = s.build_argv_from_template(template, "go", None, None, None, None, None);
        assert_eq!(argv, vec!["go"]);
        let argv2 =
            s.build_argv_from_template(template, "go", None, None, None, Some("high"), None);
        assert_eq!(argv2, vec!["--variant", "high", "go"]);
    }

    #[test]
    fn unsafe_optional_model_token_rejects_all_optional_model_bearing_token() {
        let msg = CliStrategy::unsafe_optional_model_token(
            "-m {{model?}}#{{effort?}} {{prompt}}",
            "opencode",
        );
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.contains("opencode"));
        assert!(msg.contains("{{model?}}#{{effort?}}"));
    }

    #[test]
    fn unsafe_optional_model_token_allows_required_model_with_optional_effort() {
        assert!(CliStrategy::unsafe_optional_model_token(
            "-m {{model}}#{{effort?}} {{prompt}}",
            "opencode",
        )
        .is_none());
    }

    #[test]
    fn unsafe_optional_model_token_none_when_no_model_marker() {
        assert!(CliStrategy::unsafe_optional_model_token(
            "-p {{prompt}} --add-dir {{workdir}}",
            "antigravity",
        )
        .is_none());
    }

    // ── CB3: real registry templates as fixtures ─────────────────
    // All `invocation_template` strings below are copied verbatim from
    // `canopy-registry` commit `0f90d39` (branch `feat/invocation-template`),
    // one fixture per platform. Tests call `build_argv_from_template` directly
    // and assert the exact argv word list — no CLI is spawned, no network.
    fn strategy_with_template(template: &str) -> CliStrategy {
        let mut s = sample_strategy();
        s.headless_mode = String::new();
        s.invocation_template = Some(template.to_string());
        s
    }

    #[test]
    fn real_template_copilot_prompt_immediately_after_p_flag() {
        // Source: canopy-registry/platforms/copilot.toml @ 0f90d39
        // Template: "-p {{prompt}} {{session_flag}} {{session_id}} --model {{model}}"
        let template = "-p {{prompt}} {{session_flag}} {{session_id}} --model {{model}}";
        let s = strategy_with_template(template);

        // Without session: session markers elided, no orphan flags.
        let argv = s.build_argv_from_template(
            template,
            "fix the bug",
            Some("gpt-4"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(argv, vec!["-p", "fix the bug", "--model", "gpt-4"]);
        assert_eq!(argv[0], "-p");
        assert_eq!(argv[1], "fix the bug");

        // With session: -p still immediately followed by prompt, not by session flag.
        let argv2 = s.build_argv_from_template(
            template,
            "fix the bug",
            Some("gpt-4"),
            None,
            Some(("--session-id", "uuid-123")),
            None,
            None,
        );
        assert_eq!(
            argv2,
            vec![
                "-p",
                "fix the bug",
                "--session-id",
                "uuid-123",
                "--model",
                "gpt-4"
            ]
        );
        assert_eq!(argv2[0], "-p");
        assert_eq!(argv2[1], "fix the bug");
        // Guard against the RS1 regression: `copilot -p --session-id <uuid> <PROMPT>`
        assert_ne!(argv2[1], "--session-id");
    }

    #[test]
    fn real_template_claude_argv_with_effort() {
        // Source: canopy-registry/platforms/claude.toml @ 0f90d39
        // Template: "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}"
        let template =
            "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "do the thing",
            Some("claude-opus-4-8"),
            None,
            Some(("--session-id", "ses_abc")),
            Some("high"),
            None,
        );
        assert_eq!(
            argv,
            vec![
                "--effort",
                "high",
                "--model",
                "claude-opus-4-8",
                "--session-id",
                "ses_abc",
                "do the thing"
            ]
        );
    }

    #[test]
    fn real_template_claude_argv_without_effort_elides_flag() {
        // Source: canopy-registry/platforms/claude.toml @ 0f90d39
        let template =
            "--effort {{effort}} --model {{model}} {{session_flag}} {{session_id}} {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "do the thing",
            Some("claude-opus-4-8"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(argv, vec!["--model", "claude-opus-4-8", "do the thing"]);
        assert!(!argv.contains(&"--effort".to_string()));
        assert!(!argv.contains(&"--session-id".to_string()));
    }

    #[test]
    fn real_template_gemini_session_flag_before_prompt() {
        // Source: canopy-registry/platforms/gemini.toml @ 0f90d39
        // Template: "{{session_flag}} {{session_id}} -p {{prompt}} --model {{model}}"
        let template = "{{session_flag}} {{session_id}} -p {{prompt}} --model {{model}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "analyze this",
            Some("gemini-2.5-pro"),
            None,
            Some(("--session-id", "ses_gem")),
            None,
            None,
        );
        assert_eq!(
            argv,
            vec![
                "--session-id",
                "ses_gem",
                "-p",
                "analyze this",
                "--model",
                "gemini-2.5-pro"
            ]
        );
        // Without session: session markers elided, no orphan flags.
        let argv2 = s.build_argv_from_template(
            template,
            "analyze this",
            Some("gemini-2.5-pro"),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            argv2,
            vec!["-p", "analyze this", "--model", "gemini-2.5-pro"]
        );
    }

    #[test]
    fn real_template_codex_effort_as_single_argv_word() {
        // Source: canopy-registry/platforms/codex.toml @ 0f90d39
        // Template: "-m {{model}} -C {{workdir}} -c 'model_reasoning_effort=\"{{effort}}\"' {{prompt}}"
        let template =
            "-m {{model}} -C {{workdir}} -c 'model_reasoning_effort=\"{{effort}}\"' {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "refactor",
            Some("o3"),
            Some("/proj"),
            None,
            Some("high"),
            None,
        );
        assert_eq!(
            argv,
            vec![
                "-m",
                "o3",
                "-C",
                "/proj",
                "-c",
                "'model_reasoning_effort=\"high\"'",
                "refactor"
            ]
        );
        // Verify -c value is one single argv word containing the quotes.
        assert_eq!(argv[4], "-c");
        assert_eq!(argv[5], "'model_reasoning_effort=\"high\"'");
        // Without effort: flag and quoted token elided together.
        let argv2 = s.build_argv_from_template(
            template,
            "refactor",
            Some("o3"),
            Some("/proj"),
            None,
            None,
            None,
        );
        assert_eq!(argv2, vec!["-m", "o3", "-C", "/proj", "refactor"]);
        assert!(!argv2.contains(&"-c".to_string()));
    }

    #[test]
    fn real_template_cursor_effort_embedded_in_model() {
        // Source: canopy-registry/platforms/cursor.toml @ 0f90d39
        // Template: "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}"
        let template =
            "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "build ui",
            Some("claude-opus-4-8"),
            Some("/proj"),
            None,
            Some("high"),
            None,
        );
        assert_eq!(
            argv,
            vec![
                "claude-opus-4-8[context=1m,effort=high,fast=false]",
                "--workspace",
                "/proj",
                "build ui"
            ]
        );
        assert_eq!(
            argv[0],
            "claude-opus-4-8[context=1m,effort=high,fast=false]"
        );
    }

    #[test]
    fn real_template_cursor_effort_elided_when_unavailable() {
        // Source: canopy-registry/platforms/cursor.toml @ 0f90d39
        let template =
            "{{model}}[context=1m,effort={{effort}},fast=false] --workspace {{workdir}} {{prompt}}";
        let s = strategy_with_template(template);
        // Without effort, the whole composite token must be elided (both markers required).
        let argv = s.build_argv_from_template(
            template,
            "build ui",
            Some("claude-opus-4-8"),
            Some("/proj"),
            None,
            None,
            None,
        );
        assert_eq!(argv, vec!["--workspace", "/proj", "build ui"]);
        assert!(!argv.iter().any(|w| w.contains("effort=")));
    }

    #[test]
    fn real_template_cline_thinking_flag() {
        // Source: canopy-registry/platforms/cline.toml @ 0f90d39
        // Template: "--thinking {{effort}} -m {{model}} -c {{workdir}} {{prompt}}"
        let template = "--thinking {{effort}} -m {{model}} -c {{workdir}} {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "explain",
            Some("gpt-4"),
            Some("/proj"),
            None,
            Some("medium"),
            None,
        );
        assert_eq!(
            argv,
            vec![
                "--thinking",
                "medium",
                "-m",
                "gpt-4",
                "-c",
                "/proj",
                "explain"
            ]
        );
        // Without effort: no orphan --thinking flag.
        let argv2 = s.build_argv_from_template(
            template,
            "explain",
            Some("gpt-4"),
            Some("/proj"),
            None,
            None,
            None,
        );
        assert_eq!(argv2, vec!["-m", "gpt-4", "-c", "/proj", "explain"]);
        assert!(!argv2.contains(&"--thinking".to_string()));
    }

    #[test]
    fn real_template_opencode_v2_effort_in_model() {
        // Source: CM31 — opencode v2 removed `--variant`; reasoning level now
        // rides inside `-m provider/model#variant`. `{{effort?}}` optional so a
        // dispatch with no effort still gets `-m <model>` (not a dropped flag).
        let template = "-m {{model}}#{{effort?}} --dir {{workdir}} {{prompt}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "implement",
            Some("opencode/big-pickle"),
            Some("/proj"),
            None,
            Some("max"),
            None,
        );
        assert_eq!(
            argv,
            vec![
                "-m",
                "opencode/big-pickle#max",
                "--dir",
                "/proj",
                "implement"
            ]
        );
        // Without effort: `-m opencode/big-pickle` survives whole — this is
        // the CM31 fix; the old template lost `-m` and the model entirely here.
        let argv2 = s.build_argv_from_template(
            template,
            "implement",
            Some("opencode/big-pickle"),
            Some("/proj"),
            None,
            None,
            None,
        );
        assert_eq!(
            argv2,
            vec!["-m", "opencode/big-pickle", "--dir", "/proj", "implement"]
        );
        assert!(!argv2.iter().any(|w| w.contains('#')));
    }

    #[test]
    fn real_template_antigravity_no_effort_elides_cleanly() {
        // Source: canopy-registry/platforms/antigravity.toml @ 0f90d39
        // Template: "-p {{prompt}} --add-dir {{workdir}}"
        // effort_declaration = { form = "", values = [] } — declared as not supporting effort.
        let template = "-p {{prompt}} --add-dir {{workdir}}";
        let s = strategy_with_template(template);
        let argv =
            s.build_argv_from_template(template, "hello", None, Some("/proj"), None, None, None);
        assert_eq!(argv, vec!["-p", "hello", "--add-dir", "/proj"]);
        // Even if effort is passed, template has no {{effort}} marker so argv is unchanged.
        let argv2 = s.build_argv_from_template(
            template,
            "hello",
            None,
            Some("/proj"),
            None,
            Some("high"),
            None,
        );
        assert_eq!(argv2, vec!["-p", "hello", "--add-dir", "/proj"]);
        assert!(!argv2.iter().any(|w| w.contains("high")));
    }

    #[test]
    fn build_command_with_mcp_config_injects_path() {
        let template = "-p {{prompt}} --mcp-config {{mcp_config}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(
            template,
            "hello",
            None,
            None,
            None,
            None,
            Some("/tmp/mcp.json"),
        );
        assert!(argv.contains(&"/tmp/mcp.json".to_string()));
        assert!(argv.contains(&"--mcp-config".to_string()));
    }

    #[test]
    fn build_command_with_mcp_config_none_omits_marker() {
        let template = "-p {{prompt}} --mcp-config {{mcp_config}}";
        let s = strategy_with_template(template);
        let argv = s.build_argv_from_template(template, "hello", None, None, None, None, None);
        assert!(!argv.iter().any(|w| w.contains("mcp")));
        assert_eq!(argv, vec!["-p", "hello"]);
    }

    #[test]
    fn build_command_with_mcp_config_with_effort_reaches_argv() {
        let template = "-p {{prompt}} --mcp-config {{mcp_config}} --effort {{effort}}";
        let s = strategy_with_template(template);
        let cmd = s
            .build_command_with_mcp_config("hello", None, None, Some("/tmp/mcp.json"), Some("high"))
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(cmd_str.contains("--effort"));
        assert!(cmd_str.contains("high"));
    }

    #[test]
    fn build_command_with_mcp_config_without_effort_elides_flag() {
        let template = "-p {{prompt}} --mcp-config {{mcp_config}} --effort {{effort}}";
        let s = strategy_with_template(template);
        let cmd = s
            .build_command_with_mcp_config("hello", None, None, Some("/tmp/mcp.json"), None)
            .unwrap();
        let cmd_str = format!("{:?}", cmd);
        assert!(!cmd_str.contains("--effort"));
    }

    // ── CB44 identity check ──────────────────────────────────────

    fn identity_cli(
        binary: &str,
        cmd: Option<&str>,
        contains: Option<&str>,
    ) -> super::super::cli_config::CliConfig {
        super::super::cli_config::CliConfig {
            name: "blackbox".to_string(),
            binary: binary.to_string(),
            identity_check: match (cmd, contains) {
                (Some(c), Some(s)) => Some(super::super::cli_config::IdentityCheck {
                    cmd: c.to_string(),
                    contains: s.to_string(),
                }),
                _ => None,
            },
            ..Default::default()
        }
    }

    fn write_identity_script(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

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
            ("executor", include_str!("../executor/mod.rs")),
            ("graph_engine", include_str!("../graph_engine.rs")),
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
}
