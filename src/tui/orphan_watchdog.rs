//! Orphan watchdog: end the TUI once its terminal is gone.
//!
//! When the terminal window closes, the main thread can be stuck inside
//! crossterm's `poll`/`read` on a hung-up tty, spinning at 100% CPU and never
//! reaching the per-iteration `SIGHUP_RECEIVED` check. This thread watches for
//! the three signs that nobody is on the other end — a `SIGHUP`, the parent
//! process having died (we were reparented), or stdin no longer being a
//! terminal — and, when one shows, first asks the event graph to stop on its
//! own, then, if it has not after a short grace period, restores what it can
//! and exits.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::tui::agent::pty::SIGHUP_RECEIVED;

/// How often the watchdog looks. Three syscalls per tick: cheap.
const CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// How long the event graph gets to shut down through `app.cleanup()` after
/// the watchdog asked it to, before the process is ended from here.
const GRACE: Duration = Duration::from_secs(2);
/// How long the best-effort terminal restore may take before it is abandoned:
/// writes to a dead tty can block on a full output buffer.
const RESTORE_TIMEOUT: Duration = Duration::from_millis(200);
/// Exit status when the watchdog ends the process: 128 + `SIGHUP`, what a
/// shell reports for a process the hangup killed.
const EXIT_CODE: i32 = 129;

/// What the watchdog saw on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Observation {
    /// A `SIGHUP` reached the process.
    pub sighup: bool,
    /// The parent process died and we were reparented (to init or a
    /// subreaper).
    pub parent_gone: bool,
    /// Stdin was a terminal when the TUI started and is not any more (the
    /// tty was hung up or closed).
    pub tty_lost: bool,
}

/// Whether the TUI is orphaned and must shut down.
pub(crate) fn should_shut_down(obs: Observation) -> bool {
    obs.sighup || obs.parent_gone || obs.tty_lost
}

/// Whether the process may be ended from the watchdog: it asked the event
/// graph to stop `since` ago and that is longer than the grace period.
pub(crate) fn grace_expired(asked_at: Option<Instant>, now: Instant) -> bool {
    asked_at.is_some_and(|at| now.saturating_duration_since(at) >= GRACE)
}

fn stdin_is_tty() -> bool {
    // SAFETY: `isatty` only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 }
}

fn parent_pid() -> libc::pid_t {
    // SAFETY: `getppid` cannot fail.
    unsafe { libc::getppid() }
}

/// Start the watchdog. Call once, after the terminal is set up. The baseline
/// (parent pid, whether stdin is a tty) is taken here, so a TUI started with
/// stdin redirected, or directly under init, is not mistaken for an orphan.
pub(crate) fn spawn() {
    let initial_ppid = parent_pid();
    let had_tty = stdin_is_tty();
    let _ = std::thread::Builder::new()
        .name("canopy-tui-watchdog".into())
        .spawn(move || run(initial_ppid, had_tty));
}

fn run(initial_ppid: libc::pid_t, had_tty: bool) {
    let mut asked_at: Option<Instant> = None;
    loop {
        std::thread::sleep(CHECK_INTERVAL);
        let obs = Observation {
            sighup: SIGHUP_RECEIVED.load(Ordering::Relaxed),
            parent_gone: parent_pid() != initial_ppid,
            tty_lost: had_tty && !stdin_is_tty(),
        };
        if !should_shut_down(obs) {
            continue;
        }
        if asked_at.is_none() {
            // The event graph checks this flag every iteration: if it is not
            // stuck it quits through the normal cleanup path.
            SIGHUP_RECEIVED.store(true, Ordering::Relaxed);
            asked_at = Some(Instant::now());
            continue;
        }
        if grace_expired(asked_at, Instant::now()) {
            force_exit();
        }
    }
}

/// Restore the terminal on a helper thread (it may block on a dead tty, or on
/// the stdout lock the stuck main thread holds), wait briefly, then end the
/// process. `_exit` rather than `exit`: atexit handlers and static destructors
/// of native libraries can deadlock against the threads still running.
fn force_exit() -> ! {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let _ = std::thread::Builder::new()
        .name("canopy-tui-restore".into())
        .spawn(move || {
            super::panic_hook::restore_terminal_best_effort();
            let _ = tx.send(());
        });
    let _ = rx.recv_timeout(RESTORE_TIMEOUT);
    // SAFETY: `_exit` ends the process immediately; nothing after it runs.
    unsafe { libc::_exit(EXIT_CODE) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(sighup: bool, parent_gone: bool, tty_lost: bool) -> Observation {
        Observation {
            sighup,
            parent_gone,
            tty_lost,
        }
    }

    #[test]
    fn healthy_tui_keeps_running() {
        assert!(!should_shut_down(obs(false, false, false)));
    }

    #[test]
    fn any_orphan_sign_shuts_down() {
        assert!(should_shut_down(obs(true, false, false)));
        assert!(should_shut_down(obs(false, true, false)));
        assert!(should_shut_down(obs(false, false, true)));
        assert!(should_shut_down(obs(true, true, true)));
    }

    #[test]
    fn grace_period_gates_the_forced_exit() {
        let now = Instant::now();
        assert!(!grace_expired(None, now));
        assert!(!grace_expired(Some(now), now));
        assert!(!grace_expired(Some(now), now + GRACE / 2));
        assert!(grace_expired(Some(now), now + GRACE));
    }
}
