//! Dynamic CLI execution strategy.
//!
//! All CLI definitions come from the registry (platforms.json).
//! Commands are built dynamically based on the saved configuration.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use anyhow::{Context, Result};

mod binary_identity;
mod identity;
mod template;

use template::TemplateArgs;

#[cfg(test)]
use identity::{truncate_identity_output, IDENTITY_CHECK_OUTPUT_LIMIT};
// the standalone RAG search example re-includes `src/domain` via `#[path]` as a private
// module, so these re-exports look unused in that crate even though the daemon
// (probe/doctor) uses them. Same pattern as `domain::graphs`' re-export.
#[allow(unused_imports)]
pub use identity::{verify_identity, verify_identity_async, WrongBinaryError};
// Same `#[allow(unused_imports)]` pattern as `identity` above: the standalone RAG search example
// re-includes `src/domain` via `#[path]` as a private module, so these re-exports
// look unused in that crate even though probe/dispatch/doctor use them.
#[allow(unused_imports)]
pub use binary_identity::{
    ensure_binary_identity, ensure_binary_identity_blocking, ensure_cli_installed,
    ensure_cli_installed_in, BinaryIdentity, NotInstalled,
};

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
    /// Flag that sets the session title at spawn. See
    /// [`CliConfig::session_title_flag`].
    ///
    /// [`CliConfig::session_title_flag`]: super::cli_config::CliConfig::session_title_flag
    pub session_title_flag: Option<String>,
    /// Args to list sessions as JSON. See
    /// [`CliConfig::session_list_args`].
    ///
    /// [`CliConfig::session_list_args`]: super::cli_config::CliConfig::session_list_args
    pub session_list_args: Option<Vec<String>>,
    /// Args to end a session by id (with `{session_id}` substitution). See
    /// [`CliConfig::session_end_args`].
    ///
    /// [`CliConfig::session_end_args`]: super::cli_config::CliConfig::session_end_args
    pub session_end_args: Option<Vec<String>>,
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
    /// Extra headless flags for decision-model invocation (CB88). See
    /// [`CliConfig::decision_args`].
    ///
    /// [`CliConfig::decision_args`]: super::cli_config::CliConfig::decision_args
    pub decision_args: Option<String>,
    /// CB85: identity declaration enforced on dispatch. `None` for every
    /// platform that does not declare both `identity_args` and
    /// `identity_pattern` — those keep today's behaviour with no per-call
    /// tax. Mirrored here (not `identity_check`, see the note on
    /// [`Self::from_cli_config`]) so the one shared spawn path can check it.
    pub identity: Option<binary_identity::BinaryIdentity>,
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
    /// CB85 reverses that decision for its own pair: `identity_args`/
    /// `identity_pattern` ARE mirrored and ARE enforced on dispatch,
    /// because a platform whose binary is a different program must never be
    /// invoked as if it were — the cached check makes the per-spawn cost a
    /// hashmap hit.
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
            session_title_flag: cli_config.session_title_flag.clone(),
            session_list_args: cli_config.session_list_args.clone(),
            session_end_args: cli_config.session_end_args.clone(),
            session_resume_cmd: cli_config.session_resume_cmd.clone(),
            trust_flag: cli_config.trust_flag.clone(),
            invocation_template: cli_config.invocation_template.clone(),
            effort_declaration: cli_config.effort_declaration.clone(),
            decision_args: cli_config.decision_args.clone(),
            identity: binary_identity::BinaryIdentity::from_cli_config(cli_config),
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

    /// CB84: the `(flag, canopy-<run_id>)` title pair to stamp on a headless
    /// spawn, or `None` when the platform sets no `session_title_flag` or the
    /// caller has no run id (probes, conflict-resolution agents, MCP
    /// subagents keep today's behaviour).
    fn session_title_for(&self, run_id: Option<&str>) -> Option<(&str, String)> {
        let flag = self.session_title_flag.as_deref()?;
        let run_id = run_id?;
        Some((flag, format!("canopy-{run_id}")))
    }

    /// Build the command to end a session by id, replacing `{session_id}` in
    /// `session_end_args`. Returns `None` when the platform has no
    /// `session_end_args` (or its binary cannot be resolved) — the caller
    /// keeps today's behaviour.
    pub fn build_session_end_command(
        &self,
        session_id: &str,
        workdir: &str,
    ) -> Option<tokio::process::Command> {
        let end_args = self.session_end_args.as_ref()?;
        let resolved = resolve_binary(&self.binary).ok()?;
        let mut cmd = tokio::process::Command::new(resolved);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);
        for (key, value) in &self.env_vars {
            cmd.env(key, value);
        }
        for arg in end_args {
            cmd.arg(arg.replace("{session_id}", session_id));
        }
        cmd.current_dir(workdir);
        cmd.stdin(std::process::Stdio::null());
        Some(cmd)
    }

    /// Build the command to list sessions as JSON. Returns `None` when the
    /// platform has no `session_list_args` (or its binary cannot be
    /// resolved). The caller runs it with the run's workdir as cwd — several
    /// CLIs scope their session list to the current project — and finds the
    /// entry whose `title` equals `canopy-<run_id>`.
    pub fn build_session_list_command_by_title(
        &self,
        workdir: &str,
    ) -> Option<tokio::process::Command> {
        let list_args = self.session_list_args.as_ref()?;
        let resolved = resolve_binary(&self.binary).ok()?;
        let mut cmd = tokio::process::Command::new(resolved);
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);
        for (key, value) in &self.env_vars {
            cmd.env(key, value);
        }
        for arg in list_args {
            cmd.arg(arg);
        }
        cmd.current_dir(workdir);
        cmd.stdin(std::process::Stdio::null());
        Some(cmd)
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
        run_id: Option<&str>,
    ) -> Result<Command> {
        self.build_command_with_session(prompt, model, working_dir, None, None, run_id)
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
        run_id: Option<&str>,
    ) -> Result<Command> {
        // Cold start: inject the set-at-spawn flag + id only when both the
        // registry flag and a caller-minted id exist.
        let session_arg = match (self.session_id_set_flag.as_deref(), session_id) {
            (Some(flag), Some(id)) => Some((flag, id)),
            _ => None,
        };
        self.build_headless_command(
            prompt,
            model,
            working_dir,
            session_arg,
            None,
            effort,
            run_id,
        )
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
        run_id: Option<&str>,
    ) -> Result<Command> {
        self.build_headless_command(
            prompt,
            model,
            working_dir,
            None,
            mcp_config_path,
            effort,
            run_id,
        )
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
        run_id: Option<&str>,
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
            run_id,
        )
    }

    /// Whether this platform can resume a specific session by id in headless
    /// mode (RS2) — i.e. the registry gives it a `session_resume_cmd`.
    pub fn supports_resume_by_id(&self) -> bool {
        self.session_resume_cmd.is_some()
    }

    /// Shared core for every headless spawn (cold or resume). `session_arg`,
    /// when `Some((flag, id))`, injects that flag + id immediately before the
    /// positional prompt so the id can never be mistaken for the prompt. The
    /// cold path passes the set-at-spawn flag; the resume path passes the
    /// resume flag; a plain `build_command` passes `None`. Keeping this one
    /// function means the cold layout is byte-identical whichever caller runs.
    /// `run_id`, when `Some` and the platform sets `session_title_flag`,
    /// appends `<flag> canopy-<run_id>` right after the headless flags so the
    /// session can later be found by title.
    #[expect(clippy::too_many_arguments)]
    fn build_headless_command(
        &self,
        prompt: &str,
        model: Option<&str>,
        working_dir: Option<&str>,
        session_arg: Option<(&str, &str)>,
        mcp_config: Option<&str>,
        effort: Option<&str>,
        run_id: Option<&str>,
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

        // The spawned process must RUN in `working_dir` (cwd), regardless of
        // how the directory also reaches its argv: some CLIs resolve relative
        // paths from their cwd instead of the `--dir`-style flag, and a CLI
        // with no working-dir flag at all has no other way to be pointed at
        // `working_dir`. Set here — once, before the template/legacy split —
        // so every platform shares the same behaviour with no special-casing
        // at the call sites. `None` leaves the command inheriting the
        // caller's cwd.
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        if let Some(ref template) = self.invocation_template {
            // Template-driven argv assembly (CB2).
            let argv = self.build_argv_from_template(
                template,
                prompt,
                TemplateArgs {
                    model,
                    working_dir,
                    session_arg,
                    effort,
                    mcp_config,
                },
            );

            // Headless flags are always prepended (not part of template).
            for arg in shell_words::split(&self.headless_mode).unwrap_or_default() {
                cmd.arg(arg);
            }

            // CB84: title flag so the session can be found by title later.
            if let Some((flag, value)) = self.session_title_for(run_id) {
                cmd.arg(flag).arg(value);
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

            // CB84: title flag so the session can be found by title later.
            if let Some((flag, value)) = self.session_title_for(run_id) {
                cmd.arg(flag).arg(value);
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

            // Add working directory if supported. `supports_working_dir`
            // is a plain bool field, so filtering the optional dir on it is
            // exactly the original nested-if chain one level shorter.
            if let Some(dir) = working_dir.filter(|_| self.supports_working_dir) {
                if let Some(ref flag) = self.working_dir_flag {
                    cmd.arg(flag).arg(dir);
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
mod tests;
