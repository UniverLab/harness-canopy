//! Dirty tracking for the TUI event loop: a tick redraws only when something
//! visible changed (FR1) — input, PTY output, a slow-refresh data change, or
//! an animation frame that is actually on screen.
//!
//! The loop calls [`tick`] once per poll cycle; it runs the fast refresh,
//! the throttled slow refresh ([`SlowFingerprint`] gates the draw), and the
//! animation checks, and reports whether the frame must be drawn.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::tui::app::system_monitor::system_settling;
use crate::tui::app::types::{App, Focus};
use crate::tui::app::SlowFingerprint;
use crate::tui::ui::{animations_enabled, atmosphere_active, header_frame_id, header_millis};

// ── Perf counters ───────────────────────────────────────────────────

/// What one [`tick`] did, for the `tui_perf` counters.
#[derive(Clone, Copy)]
pub(crate) struct PerfKind {
    pub drawn: bool,
    pub slow: bool,
}

static PERF_TICKS: AtomicU64 = AtomicU64::new(0);
static PERF_DRAWS: AtomicU64 = AtomicU64::new(0);
static PERF_SLOWS: AtomicU64 = AtomicU64::new(0);

/// Count one tick (fast work always runs; draws and slow refreshes are
/// gated) and log the totals behind the `tui_perf` tracing target every 60
/// ticks so an idle TUI's cost can be profiled without guessing.
pub(crate) fn perf_tick(kind: PerfKind) {
    let ticks = PERF_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    if kind.drawn {
        PERF_DRAWS.fetch_add(1, Ordering::Relaxed);
    }
    if kind.slow {
        PERF_SLOWS.fetch_add(1, Ordering::Relaxed);
    }
    if ticks.is_multiple_of(60) {
        tracing::debug!(
            target: "tui_perf",
            ticks,
            draws = PERF_DRAWS.load(Ordering::Relaxed),
            slows = PERF_SLOWS.load(Ordering::Relaxed),
            "tui tick counters",
        );
    }
}

// ── Per-loop dirty state ────────────────────────────────────────────

/// Frame the header's whimsg line showed on the last tick it was sampled.
#[derive(PartialEq, Eq)]
struct WhimSnapshot {
    title_visible: usize,
    kaomoji: String,
    text: String,
    text_visible: usize,
}

/// Caches [`tick`] carries across loop iterations: per-session PTY output
/// sequences, the last whimsg frame, and the last header animation frame.
#[derive(Default)]
pub(crate) struct TickState {
    pty_seqs: HashMap<String, u64>,
    last_whim: Option<WhimSnapshot>,
    last_header: Option<crate::tui::ui::HeaderFrameId>,
}

/// Run one loop iteration's refresh work and report `(dirty, slow_ran)`:
/// whether anything visible changed (the frame must be drawn) and whether
/// the slow refresh ran (for [`perf_tick`]).
///
/// `input` is true when the poll cycle delivered a crossterm event (key,
/// mouse, resize, paste): input always dirties the tick.
pub(crate) fn tick(app: &mut App, state: &mut TickState, input: bool) -> Result<(bool, bool)> {
    let mut dirty = input;
    dirty |= app.refresh_fast();
    dirty |= pty_seq_changed(app, &mut state.pty_seqs);
    let mut slow_ran = false;
    if app.slow_refresh_due() {
        let before = SlowFingerprint::capture(app);
        app.refresh_slow()?;
        slow_ran = true;
        dirty |= before != SlowFingerprint::capture(app);
    }
    dirty |= animation_on_screen(app, state);
    Ok((dirty, slow_ran))
}

// ── PTY output ──────────────────────────────────────────────────────

/// True when a hub session is new, gone, or produced output since the last
/// tick: each session's last applied output sequence is already published on
/// its agent ([`crate::tui::agent::InteractiveAgent::output_seq`]), so the
/// loop diffs those instead of adding a wake channel through the
/// open/attach/reattach paths (FR3: output is still visible within one
/// frame, since the tick cadence is unchanged).
pub(crate) fn pty_seq_changed(app: &App, cache: &mut HashMap<String, u64>) -> bool {
    let mut changed = false;
    let mut live = HashSet::new();
    for agent in app
        .interactive_agents
        .iter()
        .chain(app.terminal_agents.iter())
    {
        live.insert(agent.id.as_str());
        let seq = agent.output_seq();
        let same = cache.get(&agent.id).is_some_and(|cached| *cached == seq);
        if !same {
            cache.insert(agent.id.clone(), seq);
            changed = true;
        }
    }
    let gone: Vec<String> = cache
        .keys()
        .filter(|id| !live.contains(id.as_str()))
        .cloned()
        .collect();
    if !gone.is_empty() {
        for id in gone {
            cache.remove(&id);
        }
        changed = true;
    }
    changed
}

// ── Animation ───────────────────────────────────────────────────────

/// True while a Brian's Brain automaton is on screen and allowed to step:
/// the home banner's brain on `Focus::Home`, or the sidebar brain while the
/// sidebar is visible (its draw rect proves it was drawn).
fn brain_visible(app: &App) -> bool {
    if !app.tui_settings.brians_brain {
        return false;
    }
    (app.focus == Focus::Home && app.home_brain.is_some())
        || (app.sidebar_visible && app.sidebar_brain_rect.is_some())
}

/// Sample the whimsg line and report whether its frame differs from the last
/// sample. Sampling advances the time-based phases exactly as the header's
/// own `tick()` call during draw would, so calling it here is behavior-free.
fn whimsg_frame_changed(app: &mut App, state: &mut TickState) -> bool {
    let frame = app.whimsg.tick();
    let snapshot = WhimSnapshot {
        title_visible: frame.title_visible,
        kaomoji: frame.kaomoji,
        text: frame.text,
        text_visible: frame.text_visible,
    };
    let changed = state.last_whim.as_ref() != Some(&snapshot);
    state.last_whim = Some(snapshot);
    changed
}

/// True when the header's discrete animation frame (spinner index, sweep
/// step, blink phase) moved since the last tick. Both toggles off freezes
/// the header (`header_millis` returns 0), so the frame can only move while
/// either toggle is on.
fn header_animating(app: &App, state: &mut TickState) -> bool {
    if !animations_enabled(app) {
        return false;
    }
    let id = header_frame_id(header_millis(app), app.daemon_running);
    let changed = state.last_header != Some(id);
    state.last_header = Some(id);
    changed
}

/// True when an animation frame is actually on screen: the atmosphere, a
/// visible brain, a changed whimsg frame, settling system numbers, or a
/// moved header frame. Both animation toggles off is the master switch
/// (FR5): every source below is frozen then, so an idle tick never draws.
pub(crate) fn animation_on_screen(app: &mut App, state: &mut TickState) -> bool {
    if !animations_enabled(app) {
        return false;
    }
    // `whimsg_frame_changed` advances the whimsg phases as a side effect, so
    // it runs unconditionally (while enabled) to keep its cache fresh even
    // when an earlier disjunct already dirtied the tick.
    let whim = whimsg_frame_changed(app, state);
    atmosphere_active(app)
        || brain_visible(app)
        || whim
        || system_settling(&app.system_info, &app.system_info_target)
        || header_animating(app, state)
}

#[cfg(test)]
mod redraw_tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::domain::models::Cli;
    use crate::tui::agent::InteractiveAgent;
    use crate::tui::ui::{header_frame_id, header_millis, HeaderFrameId};
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    fn test_app() -> (App, tempfile::TempDir) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        (app, data_dir)
    }

    fn disable_animations(app: &mut App) {
        app.tui_settings.atmosphere = false;
        app.tui_settings.brians_brain = false;
    }

    /// FR1/FR5: a tick with no input, output or state change performs no draw.
    #[test]
    fn idle_tick_no_draw() {
        let (mut app, _dir) = test_app();
        disable_animations(&mut app);
        let mut state = TickState::default();
        for _ in 0..2 {
            let (dirty, slow_ran) = tick(&mut app, &mut state, false).unwrap();
            assert!(!slow_ran, "slow refresh must be throttled under 1s");
            assert!(!dirty, "idle tick must not draw");
        }
    }

    /// FR3: PTY output dirties the tick (visible within one frame), while a
    /// quiet session stays clean.
    #[test]
    fn pty_seq_bump_draws() {
        let (mut app, _dir) = test_app();
        disable_animations(&mut app);
        let agent = InteractiveAgent::spawn(
            Cli::new("cat"),
            env!("CARGO_MANIFEST_DIR"),
            80,
            24,
            None,
            None,
            Color::Reset,
            Some("redraw-test-agent"),
            None,
            &[],
            None,
            None,
            None,
            None,
        )
        .expect("spawn cat as a stand-in interactive child");
        app.interactive_agents.push(agent);
        let mut state = TickState::default();

        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "attaching a session must draw");

        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(!dirty, "quiet session must not draw");

        let seq = app.interactive_agents[0].output_seq();
        assert!(app.interactive_agents[0].apply_out_frame(seq + 1, b"hi"));
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "pty output must draw");
        app.interactive_agents[0].kill();
    }

    /// FR2: slow work runs on its own cadence (not every tick) and a slow
    /// data change still draws.
    #[test]
    fn slow_throttled_under_1s() {
        let (mut app, _dir) = test_app();
        disable_animations(&mut app);
        app.db.set_state("version", "9.9.9-test").unwrap();
        let mut state = TickState::default();

        let before = app.daemon_version.clone();
        let (dirty, slow_ran) = tick(&mut app, &mut state, false).unwrap();
        assert!(!slow_ran, "slow refresh must be throttled under 1s");
        assert_eq!(app.daemon_version, before, "throttled tick reads nothing");
        assert!(!dirty, "throttled tick with no change must not draw");

        app.slow_force = true;
        let (dirty, slow_ran) = tick(&mut app, &mut state, false).unwrap();
        assert!(slow_ran, "forced slow refresh must run");
        assert_eq!(app.daemon_version, "9.9.9-test");
        assert!(dirty, "a slow data change must draw");
    }

    /// FR5: both toggles off freezes the header (spinner[0], sweep t=0,
    /// solid block); either on animates it.
    #[test]
    fn header_frozen_off() {
        let (mut app, _dir) = test_app();
        disable_animations(&mut app);
        assert_eq!(header_millis(&app), 0, "frozen header reads t=0");
        assert_eq!(
            header_frame_id(0, false),
            HeaderFrameId {
                spinner: 0,
                sweep: Some(0),
                blink: true,
            },
            "t=0 renders spinner[0], sweep t=0, solid lit block",
        );

        // Either toggle on: the header frame advances with wall-clock time.
        app.tui_settings.atmosphere = true;
        app.daemon_running = true;
        let mut state = TickState::default();
        assert!(header_animating(&app, &mut state), "first sample primes");
        assert!(
            !header_animating(&app, &mut state),
            "same frame twice must not dirty"
        );
        // 150 ms always crosses a 125 ms spinner boundary.
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            header_animating(&app, &mut state),
            "advanced header frame must dirty"
        );
    }

    /// An animation frame on screen draws: the atmosphere by default, or a
    /// visible brain with only its toggle on.
    #[test]
    fn animation_on_draws() {
        let (mut app, _dir) = test_app();
        assert!(
            app.tui_settings.atmosphere && app.tui_settings.brians_brain,
            "test pins the default toggles on"
        );
        let mut state = TickState::default();
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "active atmosphere must draw");

        app.tui_settings.atmosphere = false;
        app.sidebar_visible = true;
        app.sidebar_brain_rect = Some(Rect::new(0, 0, 30, 10));
        assert!(brain_visible(&app), "sidebar brain is on screen");
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "visible brain must draw");

        app.tui_settings.brians_brain = false;
        assert!(!brain_visible(&app), "toggle off hides the brain");
    }
}
