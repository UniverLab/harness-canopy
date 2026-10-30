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
/// sequences and activity pulses, the last whimsg frame, and the last header
/// animation frame.
#[derive(Default)]
pub(crate) struct TickState {
    pty_seqs: HashMap<String, u64>,
    activity: HashMap<String, bool>,
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
    dirty |= activity_changed(app, &mut state.activity);
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

/// True when a session's activity pulse flipped since the last tick: the
/// sidebar holds the working-green pulse while output is recent and goes
/// solid 12 s after the last bytes. Time-based, so it must be diffed across
/// ticks like the PTY sequences — the fast poll cannot see it change.
fn activity_changed(app: &App, cache: &mut HashMap<String, bool>) -> bool {
    let mut changed = false;
    let mut live = HashSet::new();
    for agent in app
        .interactive_agents
        .iter()
        .chain(app.terminal_agents.iter())
    {
        live.insert(agent.id.as_str());
        let active = agent.has_recent_activity();
        let same = cache.get(&agent.id).is_some_and(|cached| *cached == active);
        if !same {
            cache.insert(agent.id.clone(), active);
            changed = true;
        }
    }
    let gone: Vec<String> = cache
        .keys()
        .filter(|id| !live.contains(id.as_str()))
        .cloned()
        .collect();
    for id in gone {
        cache.remove(&id);
    }
    changed
}

// ── Animation ───────────────────────────────────────────────────────

/// True while a Brian's Brain animation is on screen and allowed to step:
/// the home banner's wave on `Focus::Home` (it moves every tick while either
/// animation toggle is on, even before the automaton exists on a tiny panel),
/// or the sidebar brain while the sidebar is visible and the automaton
/// exists (the draw records its rect and then returns early on `None`, so
/// the rect alone does not prove anything was drawn).
fn brain_visible(app: &App) -> bool {
    if app.focus == Focus::Home && animations_enabled(app) {
        return true;
    }
    if !app.tui_settings.brians_brain {
        return false;
    }
    app.sidebar_visible && app.sidebar_brain.is_some() && app.sidebar_brain_rect.is_some()
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
    // `whimsg_frame_changed` advances the whimsg phases as a side effect and
    // `header_animating` refreshes its frame cache, so both run
    // unconditionally (while enabled) to keep their caches fresh even when an
    // earlier disjunct already dirtied the tick — otherwise the skipped cache
    // would prime a tick late and dirty on its own.
    let whim = whimsg_frame_changed(app, state);
    let header = header_animating(app, state);
    atmosphere_active(app)
        || brain_visible(app)
        || whim
        || system_settling(&app.system_info, &app.system_info_target)
        || header
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

        // A pushed change arms an immediate slow refresh through `slow_force`.
        app.slow_force = true;
        assert!(app.slow_refresh_due());
        app.slow_force = false;
        // Let the one-second cadence lapse instead of pre-arming `slow_force`:
        // in production it never survives a tick, so a test that pre-arms it
        // would dirty through the hub poll rather than the slow path.
        app.last_slow_refresh = std::time::Instant::now() - std::time::Duration::from_secs(2);
        let (dirty, slow_ran) = tick(&mut app, &mut state, false).unwrap();
        assert!(slow_ran, "due slow refresh must run");
        assert_eq!(app.daemon_version, "9.9.9-test");
        assert!(dirty, "a slow data change must draw");

        // A slow run with settled data draws nothing (FR5).
        app.last_slow_refresh = std::time::Instant::now() - std::time::Duration::from_secs(2);
        let (dirty, slow_ran) = tick(&mut app, &mut state, false).unwrap();
        assert!(slow_ran, "due slow refresh must run again");
        assert!(!dirty, "a slow run with no change must not draw");
    }

    /// Input always dirties the tick: keys, mouse, resize and paste must
    /// render within one frame.
    #[test]
    fn input_dirties() {
        let (mut app, _dir) = test_app();
        disable_animations(&mut app);
        let mut state = TickState::default();
        let (dirty, _) = tick(&mut app, &mut state, true).unwrap();
        assert!(dirty, "input must draw");
    }

    /// A session status flip (exit, disconnect) dirties the tick even though
    /// no PTY bytes arrived: the `exited` frame carries no `seq`. The exit
    /// lands as an atomic (as the stream reader records it) and the fast
    /// poll folds it into the status during the tick.
    #[test]
    fn session_status_change_draws() {
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
            Some("redraw-test-exit"),
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

        app.interactive_agents[0].note_exited(1);
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "a session status change must draw");
        app.interactive_agents[0].kill();
    }

    /// The sidebar's activity pulse holds while output is recent and goes
    /// solid 12 s after the last bytes; that expiry is a visible change and
    /// must draw even with animations off.
    #[test]
    fn activity_expiry_draws() {
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
            Some("redraw-test-activity"),
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

        let seq = app.interactive_agents[0].output_seq();
        assert!(app.interactive_agents[0].apply_out_frame(seq + 1, b"hi"));
        assert!(app.interactive_agents[0].has_recent_activity());
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "pty output must draw");

        // 20 s of quiet: the pulse expiry must draw on its own.
        *app.interactive_agents[0].last_output_at.lock().unwrap() =
            chrono::Utc::now() - chrono::Duration::seconds(20);
        assert!(!app.interactive_agents[0].has_recent_activity());
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "activity expiry must draw");
        app.interactive_agents[0].kill();
    }

    /// On Home with animations on, the banner wave moves every tick — even
    /// before the brain automaton exists (tiny panel) — so the tick draws.
    #[test]
    fn home_wave_draws_without_brain() {
        use crate::tui::app::types::Focus;

        let (mut app, _dir) = test_app();
        app.tui_settings.atmosphere = false;
        assert!(app.tui_settings.brians_brain);
        app.focus = Focus::Home;
        app.home_brain = None;
        assert!(
            brain_visible(&app),
            "Home with animations on always animates (banner wave)"
        );
        // Behavioral pin: prime the whim/header caches, then the wave alone
        // must keep the tick dirty.
        let mut state = TickState::default();
        let _ = tick(&mut app, &mut state, false).unwrap();
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(dirty, "the Home wave must draw while animations are on");
    }

    /// A sidebar brain rect with no automaton draws nothing (the draw
    /// returns early), so it must not dirty the tick on its own. The rect is
    /// below the automaton's minimum size, so the fast path's `ensure` leaves
    /// the brain unbuilt — a full-size rect would grow one on the first tick
    /// and correctly dirty from then on.
    #[test]
    fn sidebar_brain_none_no_draw() {
        use crate::tui::app::types::Focus;

        let (mut app, _dir) = test_app();
        app.tui_settings.atmosphere = false;
        assert!(app.tui_settings.brians_brain);
        app.focus = Focus::Agent;
        app.sidebar_visible = true;
        app.sidebar_brain_rect = Some(Rect::new(0, 0, 5, 2));
        app.sidebar_brain = None;
        // Detach the system sampler: a sample arriving mid-test would
        // legitimately dirty via settling, and freezing the shared
        // `system_monitor_active` flag cannot do that (the fast path re-arms
        // it from `sidebar_visible`). A terminated channel never delivers
        // another sample, which is what this test actually pins.
        let (_dead_tx, dead_rx) = std::sync::mpsc::channel();
        app.system_info_rx = dead_rx;
        app.system_info_target = app.system_info.clone();
        assert!(
            !brain_visible(&app),
            "no automaton on screen means no brain animation"
        );
        let mut state = TickState::default();
        let _ = tick(&mut app, &mut state, false).unwrap();
        assert!(
            app.sidebar_brain.is_none(),
            "a sub-minimum rect must not grow a brain"
        );
        let (dirty, _) = tick(&mut app, &mut state, false).unwrap();
        assert!(!dirty, "an undrawn brain must not draw");
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
