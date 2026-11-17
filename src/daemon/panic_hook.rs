//! Daemon panic hook: remember where the last panic happened.
//!
//! [`crate::graph_engine::GraphEngine`]'s `run_guarded` turns a panicked graph
//! task into an `Err`, but a [`tokio::task::JoinError`] does not carry the
//! panic's source location. This hook wraps the previously installed hook (so
//! the journal still prints the default message) and stashes `file:line` for
//! the next [`take_last_panic_location`] caller to append to its error.
//!
//! Separate from `src/tui/panic_hook.rs`: that one restores the terminal and
//! writes a log for the TUI process, and never records a location.

use std::sync::Mutex;

/// `file:line` of the most recent panic, if any.
static LAST_PANIC_LOCATION: Mutex<Option<String>> = Mutex::new(None);

/// Install the hook: record the panic location, then run the previously
/// installed hook so the default panic message still reaches the journal.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(location) = info.location() {
            let mut slot = lock_location();
            *slot = Some(format!("{}:{}", location.file(), location.line()));
        }
        previous(info);
    }));
}

/// Read and clear the recorded location — one panic is attributed once.
pub(crate) fn take_last_panic_location() -> Option<String> {
    lock_location().take()
}

/// The location slot, recovering from a poisoned mutex: the hook runs inside
/// a panic and must never lose the location just because another panic
/// poisoned it.
fn lock_location() -> std::sync::MutexGuard<'static, Option<String>> {
    LAST_PANIC_LOCATION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
