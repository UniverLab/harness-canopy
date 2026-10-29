//! Per-session persistence of the "protocol sent" fact.
//!
//! `App::session_protocol_state` only lives as long as the process, so a
//! canopy restart (or a resume that mints a new agent id) used to forget
//! that a conversation had already received the session-start block. These
//! helpers mirror that one bit into the `daemon_state` key-value store so
//! the once-per-session rule survives restarts and resumes.

use crate::application::ports::StateRepository;
use crate::tui::app::types::App;

/// `daemon_state` key prefix for the persisted "the Canopy protocol block
/// was sent to this session" fact; the session id follows the colon
/// (`prompt_protocol_sent:<session id>`), the value is the Unix timestamp.
pub(crate) const PROMPT_PROTOCOL_KEY_PREFIX: &str = "prompt_protocol_sent:";

fn protocol_key(session_id: &str) -> String {
    format!("{PROMPT_PROTOCOL_KEY_PREFIX}{session_id}")
}

impl App {
    /// Whether the persisted store says the protocol block was already sent
    /// to `session_id`. A read failure logs and answers `false` (fail
    /// open: the block is idempotent, one repeat is worse than a log line
    /// but never worse than losing the conversation's context).
    pub(crate) fn protocol_persisted(&self, session_id: &str) -> bool {
        match self.db.get_state(&protocol_key(session_id)) {
            Ok(value) => value.is_some(),
            Err(e) => {
                tracing::warn!("Failed to read persisted protocol flag for '{session_id}': {e}");
                false
            }
        }
    }

    /// Record that a prompt carrying the protocol block was sent to
    /// `session_id` (value = Unix timestamp, per the spec). Best-effort: a
    /// failure to persist must never block the send itself.
    pub(crate) fn persist_protocol_sent(&self, session_id: &str) {
        let timestamp = chrono::Utc::now().timestamp();
        if let Err(e) = self
            .db
            .set_state(&protocol_key(session_id), &timestamp.to_string())
        {
            tracing::warn!("Failed to persist protocol flag for '{session_id}': {e}");
        }
    }

    /// Carry `source_session_id`'s persisted flag over to `new_session_id`.
    /// Test-only now: every resume relaunches the source session's own id,
    /// so the flag stays on the reactivated row and production never calls
    /// this. Only copies when the source has the flag and the target does
    /// not; an existing target flag always wins.
    #[cfg(test)]
    pub(crate) fn inherit_protocol_flag(&self, source_session_id: &str, new_session_id: &str) {
        if source_session_id == new_session_id || self.protocol_persisted(new_session_id) {
            return;
        }
        let Ok(Some(value)) = self.db.get_state(&protocol_key(source_session_id)) else {
            return;
        };
        if let Err(e) = self.db.set_state(&protocol_key(new_session_id), &value) {
            tracing::warn!(
                "Failed to inherit protocol flag from '{source_session_id}' to \
                 '{new_session_id}': {e}"
            );
        }
    }
}
