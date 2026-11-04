//! Spawn command resolution for daemon hub sessions: which binary and args a
//! `SpawnRequest` turns into.
//!
//! Kept beside [`super::hub`] rather than inside it so the hub stays under the
//! file-size cap; the behaviour is the hub's — the TUI builds the same command
//! through [`super::build_agent_cmd`].

use portable_pty::CommandBuilder;

use crate::domain::models::Cli;

use super::hub::{SessionKind, SpawnRequest};
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
) -> (CommandBuilder, Option<String>) {
    match req.kind {
        SessionKind::Agent => build_agent(req, id, name),
        SessionKind::Terminal => {
            let cmd = build_terminal_cmd(&TerminalSpawnSpec {
                shell: &req.cli,
                working_dir: &req.workdir,
                id,
                name,
            });
            (cmd, None)
        }
    }
}

/// The agent arm: registry fallbacks, the model flag, yolo, and the args
/// string the DB row keeps in the same order the child receives them.
fn build_agent(req: &SpawnRequest, id: &str, name: &str) -> (CommandBuilder, Option<String>) {
    let cli = Cli::new(req.cli.as_str());
    let command = cli.command_name();
    let registry = cli.registry_config();
    let interactive_args = req.interactive_args.as_deref().or_else(|| {
        registry
            .as_ref()
            .and_then(|c| c.interactive_args.as_deref())
    });
    let fallback_args = req.fallback_args.as_deref().or_else(|| {
        registry
            .as_ref()
            .and_then(|c| c.fallback_interactive_args.as_deref())
    });
    let model_flag = req
        .model_flag
        .as_deref()
        .or_else(|| registry.as_ref().and_then(|c| c.model_flag.as_deref()));
    let yolo_args = req.yolo.then(|| {
        let flag = yolo_flag_for(req.cli.as_str()).expect("validated");
        with_yolo(interactive_args, &flag)
    });
    let interactive_args = yolo_args.as_deref().or(interactive_args);
    let args = agent_args_string(
        interactive_args,
        fallback_args,
        req.model.as_deref(),
        model_flag,
    );
    let cmd = build_agent_cmd(&AgentSpawnSpec {
        command: &command,
        working_dir: &req.workdir,
        interactive_args,
        fallback_args,
        model: req.model.as_deref(),
        model_flag,
        accent_rgb: parse_accent_rgb(req.accent.as_deref()),
        id,
        name,
        seed_id: req.seed_id.as_deref(),
    });
    (cmd, args)
}
