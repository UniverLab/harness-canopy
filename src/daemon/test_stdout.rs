//! Process-wide stdout discipline for tests.
//!
//! Several test modules capture real stdout by swapping file descriptor 1
//! for the whole process (`StdoutCapture`). That swap is global: two such
//! tests overlapping in time — or one uncaptured `println!` landing in
//! another test's pipe — corrupts exact-output assertions. Worse, a panic
//! between `start` and `stop` leaks a pipe write end, so some other test's
//! `stop` blocks on its read forever and the whole test binary hangs at 0%
//! CPU. Every test that writes to real stdout, captured or not, must hold
//! [`lock_test_stdout`] for the whole window.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Acquire the process-wide test-stdout lock. Hold the guard for the entire
/// window in which the test touches real stdout — from before capture (or
/// the first print) until after `stop` (or the last print). Poison-tolerant:
/// a sibling test that panicked mid-capture must fail its own assertions,
/// not hand every later test a poisoned lock.
///
/// Sync tests only: a `std` guard cannot be held across `.await` (clippy
/// `await_holding_lock`), so async capture helpers do not take it — their
/// tests are `#[ignore]`d out of the gate for the same capture fragility.
pub(crate) fn lock_test_stdout() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
