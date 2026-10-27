//! Sessions owned by the daemon: PTY spawning, the session hub, and the
//! WebSocket protocol that serves hub sessions to clients.
//!
//! The TUI still spawns its own PTYs in this spec; only the shared spawn and
//! parser helpers live here (see [`spawn`]), so both sides build identical
//! children and handle their parsers identically.

pub mod auth;
pub mod client;
pub mod history;
pub mod hub;
pub mod protocol;
pub mod resume;
pub mod spawn;
pub mod ws;

pub use history::save_terminal_histories;

pub use hub::{
    HubEvent, HubSession, OutputReceiver, SessionHub, SessionKind, SessionStatus, SpawnRequest,
};
pub use spawn::{
    agent_args_string, build_agent_cmd, build_terminal_cmd, format_accent_rgb, parse_accent_rgb,
    preserve_rows_on_shrink, spawn_identity, write_submitted_prompt, AgentSpawnSpec,
    TerminalSpawnSpec, VT_SCROLLBACK_LINES,
};

#[cfg(test)]
mod tests;
