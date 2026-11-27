//! Spawn command resolution for daemon hub sessions: which binary and args a
//! `SpawnRequest` turns into.
//!
//! Kept beside [`super::hub`] rather than inside it so the hub stays under the
//! file-size cap; the behaviour is the hub's — the TUI builds the same command
//! through [`super::build_agent_cmd`].

use portable_pty::CommandBuilder;

use crate::domain::cli_config::CliConfig;
use crate::domain::models::Cli;

use super::hub::{SessionKind, SpawnRequest, EXIT_DETAIL_MAX_BYTES};
use super::{
    agent_args_string, build_agent_cmd, build_terminal_cmd, parse_accent_rgb, with_yolo,
    yolo_flag_for, AgentSpawnSpec, TerminalSpawnSpec,
};

/// Build the spawn command and the args string recorded in the DB row.
/// Explicit launch fields from the New dialog win; when absent the hub falls
/// back to the registry, so older clients keep working. `yolo` appends the
/// harness's registry `yolo_flag` to the resolved interactive args — it must
/// only be set for an agent whose harness defines one (the frame handlers
/// check that before the hub sees the request).
pub(crate) fn build_spawn_command(
    req: &SpawnRequest,
    id: &str,
    name: &str,
) -> (CommandBuilder, Option<String>, Option<String>) {
    match req.kind {
        SessionKind::Agent => build_agent(req, id, name),
        SessionKind::Terminal => {
            let cmd = build_terminal_cmd(&TerminalSpawnSpec {
                shell: &req.cli,
                working_dir: &req.workdir,
                id,
                name,
            });
            (cmd, None, None)
        }
    }
}

/// The agent arm: registry fallbacks, the model flag, yolo, and the args
/// string the DB row keeps in the same order the child receives them.
fn build_agent(
    req: &SpawnRequest,
    id: &str,
    name: &str,
) -> (CommandBuilder, Option<String>, Option<String>) {
    let cli = Cli::new(req.cli.as_str());
    let registry = cli.registry_config();
    build_agent_with_config(req, id, name, registry.as_ref())
}

/// Testable agent command builder taking an explicit registry [`CliConfig`]
/// so unit tests never touch `~/.canopy`. Returns `(cmd, persisted_args,
/// generated_harness_conversation_id)`.
pub(crate) fn build_agent_with_config(
    req: &SpawnRequest,
    id: &str,
    name: &str,
    registry: Option<&CliConfig>,
) -> (CommandBuilder, Option<String>, Option<String>) {
    let cli = Cli::new(req.cli.as_str());
    let command = cli.command_name();
    let interactive_args = req
        .interactive_args
        .as_deref()
        .or_else(|| registry.and_then(|c| c.interactive_args.as_deref()));
    let fallback_args = req
        .fallback_args
        .as_deref()
        .or_else(|| registry.and_then(|c| c.fallback_interactive_args.as_deref()));
    let model_flag = req
        .model_flag
        .as_deref()
        .or_else(|| registry.and_then(|c| c.model_flag.as_deref()));
    let yolo_args = req.yolo.then(|| {
        let flag = registry
            .and_then(|c| c.yolo_flag.clone())
            .or_else(|| yolo_flag_for(req.cli.as_str()))
            .expect("validated");
        with_yolo(interactive_args, &flag)
    });
    let interactive_args = yolo_args.as_deref().or(interactive_args);
    let args = agent_args_string(
        interactive_args,
        fallback_args,
        req.model.as_deref(),
        model_flag,
    );
    let mut cmd = build_agent_cmd(&AgentSpawnSpec {
        command: &command,
        working_dir: &req.workdir,
        interactive_args,
        fallback_args,
        model: req.model.as_deref(),
        model_flag,
        accent_rgb: parse_accent_rgb(req.accent.as_deref()),
        id,
        name,
    });
    let generated_cid =
        maybe_inject_session_id(&mut cmd, req, cli.as_str(), interactive_args, registry);
    (cmd, args, generated_cid)
}

fn maybe_inject_session_id(
    cmd: &mut CommandBuilder,
    req: &SpawnRequest,
    cli_name: &str,
    interactive_args: Option<&str>,
    registry: Option<&CliConfig>,
) -> Option<String> {
    if req.kind != SessionKind::Agent
        || req.preserve_args
        || req
            .harness_conversation_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        || cli_name == "muse"
    {
        return None;
    }
    let reg = registry?;
    let set_flag = reg
        .session_id_set_flag
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let resume_cmd = reg
        .session_resume_cmd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if let Some(ia) = interactive_args {
        let has_resume_args = reg
            .resume_args
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some_and(|r| args_contain_sequence(ia, r));
        if args_contain_sequence(ia, set_flag)
            || has_resume_args
            || args_contain_sequence(ia, resume_cmd)
        {
            return None;
        }
    }
    let uuid = uuid::Uuid::new_v4().to_string();
    for token in set_flag.split_whitespace() {
        cmd.arg(token);
    }
    cmd.arg(&uuid);
    Some(uuid)
}

fn args_contain_sequence(args: &str, sequence: &str) -> bool {
    let args_tokens: Vec<_> = args.split_whitespace().collect();
    let sequence_tokens: Vec<_> = sequence.split_whitespace().collect();
    !sequence_tokens.is_empty()
        && args_tokens
            .windows(sequence_tokens.len())
            .any(|window| window == sequence_tokens.as_slice())
}

/// How many head lines a long failure detail keeps before the omission line.
const EXIT_DETAIL_HEAD_LINES: usize = 10;

/// How many tail lines a long failure detail keeps after the omission line.
const EXIT_DETAIL_TAIL_LINES: usize = 20;

/// The failure detail a nonzero agent exit records: the first
/// [`EXIT_DETAIL_HEAD_LINES`] and the last [`EXIT_DETAIL_TAIL_LINES`] lines
/// of the session's output, with a `[... N lines omitted ...]` line between
/// them when the output is longer than head plus tail. Shorter output is kept
/// whole. The detail stays capped at `EXIT_DETAIL_MAX_BYTES` bytes by
/// shrinking the tail lines first, then dropping the tail, so the head — the
/// lines that name the error — survives the cap.
pub(crate) fn format_exit_detail(full: &str) -> String {
    const WINDOW: usize = EXIT_DETAIL_HEAD_LINES + EXIT_DETAIL_TAIL_LINES;
    let trimmed = full.trim_end();
    debug_assert!(
        !trimmed.trim().is_empty(),
        "empty output never reaches the formatter"
    );
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() <= WINDOW {
        return head_chars(trimmed, EXIT_DETAIL_MAX_BYTES);
    }
    let omitted = lines.len() - WINDOW;
    let head = lines[..EXIT_DETAIL_HEAD_LINES].join("\n");
    let omission = format!("[... {omitted} lines omitted ...]");
    let mut tail: Vec<&str> = lines[lines.len() - EXIT_DETAIL_TAIL_LINES..].to_vec();
    loop {
        let candidate = format!("{head}\n{omission}\n{}", tail.join("\n"));
        if candidate.len() <= EXIT_DETAIL_MAX_BYTES {
            return candidate;
        }
        if tail.is_empty() {
            break;
        }
        tail.remove(0);
    }
    let head_only = format!("{head}\n{omission}");
    if head_only.len() <= EXIT_DETAIL_MAX_BYTES {
        return head_only;
    }
    head_chars(&head, EXIT_DETAIL_MAX_BYTES)
}

/// The first `max_bytes` of `text`, cut on a char boundary. Capping keeps
/// the head — the error lines — and drops the tail, never the reverse.
fn head_chars(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_like_config() -> CliConfig {
        serde_json::from_str(
            r#"{
                "name": "claude",
                "binary": "claude",
                "session_id_set_flag": "--session-id",
                "session_resume_cmd": "--resume",
                "resume_args": "--continue"
            }"#,
        )
        .expect("claude-like CliConfig")
    }

    fn base_req(cli: &str) -> SpawnRequest {
        SpawnRequest {
            kind: SessionKind::Agent,
            cli: cli.to_string(),
            model: None,
            workdir: "/tmp".to_string(),
            name: Some("alpha".to_string()),
            prompt: None,
            interactive_args: None,
            fallback_args: None,
            model_flag: None,
            id: None,
            harness_conversation_id: None,
            accent: None,
            yolo: false,
            preserve_args: false,
        }
    }

    fn argv_strings(cmd: &CommandBuilder) -> Vec<String> {
        cmd.get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn fresh_session_id_set_flag_injects_uuid_into_cmd_only() {
        let cfg = claude_like_config();
        let req = base_req("claude");
        let (cmd, args, generated_cid) =
            build_agent_with_config(&req, "sess-1", "alpha", Some(&cfg));

        let cid = generated_cid.expect("generated conversation id");
        assert!(
            uuid::Uuid::parse_str(&cid).is_ok(),
            "generated id '{cid}' must be a valid UUID"
        );
        let argv = argv_strings(&cmd);
        assert!(
            argv.windows(2)
                .any(|pair| pair[0] == "--session-id" && pair[1] == cid),
            "child argv {argv:?} must contain --session-id {cid}"
        );
        assert!(
            !args
                .as_deref()
                .is_some_and(|a| a.contains("--session-id") || a.contains(&cid)),
            "persisted args {args:?} must not contain --session-id or uuid"
        );
    }

    #[test]
    fn session_id_injection_skips_muse_preserve_args_existing_id_and_conflicts() {
        let cfg = claude_like_config();

        let muse_req = base_req("muse");
        let (cmd, _, cid) = build_agent_with_config(&muse_req, "s1", "n1", Some(&cfg));
        assert_eq!(cid, None);
        assert!(!argv_strings(&cmd).contains(&"--session-id".to_string()));

        let mut preserve_req = base_req("claude");
        preserve_req.preserve_args = true;
        let (cmd, _, cid) = build_agent_with_config(&preserve_req, "s2", "n2", Some(&cfg));
        assert_eq!(cid, None);
        assert!(!argv_strings(&cmd).contains(&"--session-id".to_string()));

        let mut existing_req = base_req("claude");
        existing_req.harness_conversation_id = Some("existing-uuid".to_string());
        let (cmd, _, cid) = build_agent_with_config(&existing_req, "s3", "n3", Some(&cfg));
        assert_eq!(cid, None);
        assert!(!argv_strings(&cmd).contains(&"--session-id".to_string()));

        for conflict in ["--session-id custom", "--continue", "--resume abc"] {
            let mut conflict_req = base_req("claude");
            conflict_req.interactive_args = Some(conflict.to_string());
            let (_, _, cid) = build_agent_with_config(&conflict_req, "s4", "n4", Some(&cfg));
            assert_eq!(
                cid, None,
                "must not inject --session-id when interactive_args has '{conflict}'"
            );
        }
    }
}
