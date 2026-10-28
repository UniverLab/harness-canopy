//! Shared PTY spawn helpers: identical command building, environment and
//! parser handling for TUI-owned sessions and daemon-hub sessions.
//!
//! Moved out of `tui::agent` so the hub reuses the exact same behavior; the
//! TUI spawn paths call these functions.

use std::io::Write;
use std::time::Duration;

use portable_pty::CommandBuilder;

use crate::domain::cli_config::PasteSubmitSpec;
use crate::shared::sync_identity::{
    CANOPY_AGENT_ID_ENV, CANOPY_SEED_ID_ENV, CANOPY_SESSION_NAME_ENV, CANOPY_WORKDIR_ENV,
};

/// Scrollback rows for every session parser, TUI or hub.
pub const VT_SCROLLBACK_LINES: usize = 5_000;

/// Before shrinking a vt100 screen from `old_rows` to `new_rows`, move the rows
/// that would fall off the bottom into the scrollback.
///
/// vt100's `set_size` shrink path ends in `Vec::resize`, which truncates the
/// tail rows — the newest output — and never hands them to the scrollback. A
/// Scroll Up first routes them through `scroll_up`, which does push evicted rows
/// onto the scrollback deque; the matching Cursor Up keeps the shell's prompt on
/// its logical line so it does not appear to jump. A grow (or an unchanged
/// height, e.g. a cols-only resize) is a no-op.
///
/// The scroll is anchored at the cursor (what xterm does): only the overflow
/// of the cursor past the new bottom is scrolled, so a shrink of a
/// mostly-blank screen — cursor near the top, blanks below — scrolls nothing
/// and keeps the newest output (the just-finished command's) on screen. A
/// blind `old_rows - new_rows` scroll anchored at the top would discard the
/// oldest rows first and push the newest content off into scrollback or drop
/// it (CT15); when the screen is full the cursor sits at the bottom and both
/// computations agree (CT9).
pub fn preserve_rows_on_shrink<C: vt100::Callbacks>(
    parser: &mut vt100::Parser<C>,
    old_rows: u16,
    new_rows: u16,
) {
    if new_rows >= old_rows {
        return;
    }
    let cursor_row = parser.screen().cursor_position().0;
    let needed = (cursor_row + 1).saturating_sub(new_rows);
    if needed == 0 {
        return;
    }
    parser.process(format!("\x1b[{needed}S\x1b[{needed}A").as_bytes());
}

/// The session id and display name a spawned agent runs under: a resumed
/// harness conversation passes both, so it keeps its canopy session id and
/// name (hooks and scheduled sends addressed to them keep resolving);
/// otherwise the id is a fresh uuid and the name a random one that avoids
/// `existing_ids`.
pub fn spawn_identity(
    id: Option<&str>,
    name: Option<&str>,
    existing_ids: &[&str],
) -> (String, String) {
    let id = id
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let name = name
        .map(str::to_owned)
        .unwrap_or_else(|| crate::tui::agent::naming::pick_random_name(existing_ids));
    (id, name)
}

pub fn apply_canopy_session_env(
    cmd: &mut CommandBuilder,
    agent_id: &str,
    session_name: &str,
    working_dir: &str,
    seed_id: Option<&str>,
) {
    cmd.env(CANOPY_AGENT_ID_ENV, agent_id);
    cmd.env(CANOPY_SESSION_NAME_ENV, session_name);
    cmd.env(CANOPY_WORKDIR_ENV, working_dir);
    if let Some(sid) = seed_id {
        cmd.env(CANOPY_SEED_ID_ENV, sid);
    }
}

/// Everything [`build_agent_cmd`] needs to build an agent child command.
pub struct AgentSpawnSpec<'a> {
    /// Binary to run (the registry-resolved command name).
    pub command: &'a str,
    pub working_dir: &'a str,
    /// Registry-driven interactive args (e.g. `--tui`, `-c`).
    pub interactive_args: Option<&'a str>,
    /// Fallback args tried when the primary args fail (e.g. kiro `chat`).
    pub fallback_args: Option<&'a str>,
    pub model: Option<&'a str>,
    pub model_flag: Option<&'a str>,
    /// Canopy UX accent as RGB, advertised to the child via `CANOPY_ACCENT_*`.
    pub accent_rgb: Option<(u8, u8, u8)>,
    pub id: &'a str,
    pub name: &'a str,
    pub seed_id: Option<&'a str>,
}

/// Build the child command for an interactive agent session: registry
/// interactive/fallback args, model flag, working directory, canopy session
/// environment, and truecolor capability advertisement.
pub fn build_agent_cmd(spec: &AgentSpawnSpec<'_>) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(spec.command);
    // Apply registry-driven interactive args (e.g. "--tui", "-c", etc.)
    // If primary args fail and fallback is available, try that instead.
    let args_to_use = spec.interactive_args.or(spec.fallback_args);
    if let Some(args) = args_to_use {
        for arg in args.split_whitespace().filter(|a| !a.is_empty()) {
            cmd.arg(arg);
        }
    }
    if let (Some(flag), Some(m)) = (spec.model_flag, spec.model) {
        if !m.is_empty() {
            cmd.arg(flag);
            cmd.arg(m);
        }
    }
    cmd.cwd(spec.working_dir);
    apply_canopy_session_env(&mut cmd, spec.id, spec.name, spec.working_dir, spec.seed_id);

    // Advertise truecolor capability so child CLIs (Kiro, etc.) use
    // 24-bit RGB color sequences for their accent colors instead of
    // limited 16-color ANSI codes that get mapped to wrong hues.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    // Pass Canopy's UX accent color to child CLIs so they respect
    // the original color scheme instead of using their own ANSI color map.
    if let Some((r, g, b)) = spec.accent_rgb {
        cmd.env("CANOPY_ACCENT_R", r.to_string());
        cmd.env("CANOPY_ACCENT_G", g.to_string());
        cmd.env("CANOPY_ACCENT_B", b.to_string());
    }
    cmd
}

/// Everything [`build_terminal_cmd`] needs to build a terminal child command.
pub struct TerminalSpawnSpec<'a> {
    /// Shell binary (e.g. `"bash"`, `"zsh"`).
    pub shell: &'a str,
    pub working_dir: &'a str,
    pub id: &'a str,
    pub name: &'a str,
}

/// Build the child command for a raw terminal session: the shell with a
/// compact prompt, canopy session environment, and truecolor advertisement.
pub fn build_terminal_cmd(spec: &TerminalSpawnSpec<'_>) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(spec.shell);
    cmd.cwd(spec.working_dir);
    apply_canopy_session_env(&mut cmd, spec.id, spec.name, spec.working_dir, None);
    // Compact prompt since warp mode shows its own prompt line
    cmd.env("PS1", "$ ");
    cmd.env("PROMPT_COMMAND", "");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd
}

/// Parse a `"r,g,b"` accent into RGB; `None` when absent or malformed.
pub fn parse_accent_rgb(raw: Option<&str>) -> Option<(u8, u8, u8)> {
    let raw = raw?;
    let mut parts = raw.split(',');
    let r: u8 = parts.next()?.trim().parse().ok()?;
    let g: u8 = parts.next()?.trim().parse().ok()?;
    let b: u8 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((r, g, b))
}

/// Format RGB as the `"r,g,b"` accent a spawn frame carries.
pub fn format_accent_rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("{r},{g},{b}")
}

/// The registry's autonomous-mode flag for `cli`, or `None` when the harness
/// has no `yolo_flag` — the flag a `spawn{yolo:true}` appends.
pub fn yolo_flag_for(cli: &str) -> Option<String> {
    crate::domain::models::Cli::new(cli)
        .registry_config()
        .and_then(|config| config.yolo_flag)
}

/// `interactive_args` with the harness's yolo flag appended (the New dialog's
/// skip-approvals switch). Unconditional: an explicit arg string keeps its
/// own flags and gains this one after them.
pub fn with_yolo(interactive_args: Option<&str>, yolo_flag: &str) -> String {
    let base = interactive_args.unwrap_or_default().trim();
    let flag = yolo_flag.trim();
    match (base.is_empty(), flag.is_empty()) {
        (true, true) => String::new(),
        (true, false) => flag.to_string(),
        (false, true) => base.to_string(),
        (false, false) => format!("{base} {flag}"),
    }
}

/// Expand and validate a `spawn` frame's `workdir`: a leading `~/` (or a bare
/// `~`) becomes the host user's home, the result must be absolute, and it
/// must name an existing directory. `Err` carries the `bad_request` message
/// naming the offending path.
pub fn resolve_spawn_workdir(raw: &str) -> Result<String, String> {
    let expanded = expand_home(raw);
    let path = std::path::Path::new(&expanded);
    if !path.is_absolute() {
        return Err("spawn: field 'workdir' must be an absolute path".to_string());
    }
    if !path.is_dir() {
        return Err(format!(
            "spawn: workdir '{expanded}' does not exist or is not a directory"
        ));
    }
    Ok(expanded)
}

/// `~` and `~/x` become the host user's home and its child; any other path is
/// returned unchanged (and a `~user` form is left for the absolute check to
/// refuse).
fn expand_home(raw: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return raw.to_string();
    };
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    // `~`, `~/` and `~/x` are the home, its trailing-slash form and a child;
    // `~user` is not expanded (the absolute check refuses it).
    match rest.strip_prefix('/').or(rest.is_empty().then_some("")) {
        Some("") => home.to_string_lossy().into_owned(),
        Some(tail) => {
            // A `~/` prefix with extra slashes (`~//x`) must stay under the
            // home: `Path::join` discards the base for an absolute tail.
            let tail = tail.trim_start_matches('/');
            if tail.is_empty() {
                home.to_string_lossy().into_owned()
            } else {
                home.join(tail).to_string_lossy().into_owned()
            }
        }
        None => raw.to_string(),
    }
}

/// The launch-args string recorded in the session's database row: the
/// interactive (or fallback) args plus the model flag and model, in the same
/// order [`build_agent_cmd`] passes them to the child.
pub fn agent_args_string(
    interactive_args: Option<&str>,
    fallback_args: Option<&str>,
    model: Option<&str>,
    model_flag: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(args) = interactive_args.or(fallback_args) {
        parts.extend(args.split_whitespace().filter(|a| !a.is_empty()));
    }
    if let (Some(flag), Some(m)) = (model_flag, model) {
        if !m.is_empty() {
            parts.push(flag);
            parts.push(m);
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Write `prompt` to `writer` as a SUBMITTED message: the paste lands as one
/// block (bracketed only when the target actually enabled bracketed-paste
/// mode — see
/// [`InteractiveAgent::bracketed_paste_enabled`](crate::tui::agent::InteractiveAgent::bracketed_paste_enabled)),
/// then the submit keystroke is written as a SEPARATE event after
/// `spec.settle`.
///
/// This separation matters: writing the submit key immediately after the
/// paste (zero delay, same logical write) lets the two coalesce into one
/// chunk at the pty/kernel level. A target CLI with bracketed-paste
/// handling that reads a `\r` inside (or immediately trailing, same-chunk)
/// the paste event can fold it into the pasted text as a literal newline
/// instead of parsing it as a distinct Enter keypress — the prompt lands in
/// the input box but is never submitted.
pub fn write_submitted_prompt(
    writer: &mut impl Write,
    prompt: &str,
    bracketed: bool,
    spec: PasteSubmitSpec,
) -> std::io::Result<()> {
    if bracketed {
        writer.write_all(format!("\x1b[200~{prompt}\x1b[201~").as_bytes())?;
    } else {
        writer.write_all(prompt.as_bytes())?;
    }
    writer.flush()?;

    for _ in 0..spec.presses.max(1) {
        if spec.settle > Duration::ZERO {
            std::thread::sleep(spec.settle);
        }
        writer.write_all(spec.submit_key)?;
        writer.flush()?;
    }
    Ok(())
}
