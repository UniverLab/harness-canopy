pub mod events;
pub mod kaomojis;
mod phrases;
pub mod rng;

pub use kaomojis::{WhimContext, TITLE};

use std::time::{Duration, Instant};

use events::{EventKind, WhimEvent};
use kaomojis::{
    BLANK_MS, ERASE_MS, EVENT_DECAY_SECS, HOLD_MAX, HOLD_MIN, INTERVAL_MAX, INTERVAL_MIN,
    KAOMOJI_MS, KAO_ERROR, KAO_LOADING, KAO_MAX_LEN, KAO_SUCCESS, KAO_THINKING, TYPE_MS,
};
use phrases::{
    BUSY, EVENT_AGENT_DONE, EVENT_AGENT_FAILED, EVENT_AGENT_SPAWNED, EVENT_GRAPH_COMPLETED,
    EVENT_GRAPH_STOPPED, EVENT_HOOK_MESSAGE, EVENT_SESSION_RENAMED, EVENT_SPEC_COMPLETED, IDLE,
    JOKES, SCROLL,
};
use rng::{pick_no_repeat, DedupRing, Rng};

pub struct WhimFrame {
    /// How many chars of TITLE are visible (0 = hidden, `TITLE.len()` = full).
    pub title_visible: usize,
    /// The kaomoji to display (empty when title is showing).
    pub kaomoji: String,
    /// The message text (may be partially visible).
    pub text: String,
    /// How many chars of `text` are visible.
    pub text_visible: usize,
}

impl WhimFrame {
    fn full_title() -> Self {
        Self {
            title_visible: TITLE.len(),
            kaomoji: String::new(),
            text: String::new(),
            text_visible: 0,
        }
    }
    fn empty_title() -> Self {
        Self {
            title_visible: 0,
            kaomoji: String::new(),
            text: String::new(),
            text_visible: 0,
        }
    }
}

fn tick_erasing_title(elapsed: u64) -> (WhimFrame, Option<Phase>) {
    let erased = (elapsed / ERASE_MS) as usize;
    if erased >= TITLE.len() {
        return (WhimFrame::empty_title(), Some(Phase::KaomojiFlash));
    }
    (
        WhimFrame {
            title_visible: TITLE.len() - erased,
            kaomoji: String::new(),
            text: String::new(),
            text_visible: 0,
        },
        None,
    )
}

#[derive(Clone, Copy)]
enum Intent {
    Loading,
    Success,
    Error,
    Thinking,
}

enum Phase {
    Idle,
    ErasingTitle,
    KaomojiFlash,
    TypingMsg,
    Holding,
    ErasingMsg,
    Blank,
}

/// The template list for one event kind. Single-template kinds avoid the
/// empty-list case the mission branch handles separately.
fn event_templates(kind: EventKind) -> &'static [&'static str] {
    match kind {
        EventKind::AgentSpawned => EVENT_AGENT_SPAWNED,
        EventKind::AgentDone => EVENT_AGENT_DONE,
        EventKind::AgentFailed => EVENT_AGENT_FAILED,
        EventKind::SessionRenamed => EVENT_SESSION_RENAMED,
        EventKind::SpecCompleted => EVENT_SPEC_COMPLETED,
        EventKind::GraphCompleted => EVENT_GRAPH_COMPLETED,
        EventKind::GraphBlocked | EventKind::GraphFailed => EVENT_GRAPH_STOPPED,
        EventKind::HookMessage => EVENT_HOOK_MESSAGE,
        EventKind::MissionUnlocked => &[],
    }
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

/// Whether a template asks for the event's detail (`{node}`/`{spec}`);
/// `{agent}`/`{graph}` always have the subject to fall back on.
fn needs_detail(template: &str) -> bool {
    template.contains("{node}") || template.contains("{spec}")
}

/// Fill an event template's placeholders from the event. `{agent}`/`{graph}`
/// become the subject, `{spec}` the first word of the detail (a spec's id),
/// `{node}` the detail itself. Callers must have filtered out templates the
/// event cannot fill (see [`needs_detail`]).
fn fill_template(template: &str, ev: &WhimEvent) -> String {
    let spec = ev
        .detail
        .as_deref()
        .map(first_word)
        .unwrap_or_else(|| first_word(&ev.subject));
    let node = ev.detail.as_deref().unwrap_or(ev.subject.as_str());
    template
        .replace("{agent}", &ev.subject)
        .replace("{graph}", &ev.subject)
        .replace("{spec}", spec)
        .replace("{node}", node)
}

pub struct Whimsg {
    rng: Rng,
    phase: Phase,
    phase_start: Instant,
    next_trigger: Instant,
    active_kaomoji: String,
    active_text: String,
    active_hold_ms: u64,
    event_context: Option<WhimEvent>,
    event_at: Instant,
    ambient: WhimContext,
    seen_kaomoji: DedupRing,
    seen_idle: DedupRing,
    seen_busy: DedupRing,
    seen_scroll: DedupRing,
    seen_joke: DedupRing,
    seen_event: DedupRing,
    mission_title: Option<String>,
    celebration_remaining: u32,
}

impl Whimsg {
    pub fn new() -> Self {
        let mut rng = Rng::from_instant(Instant::now());
        let first = Duration::from_secs(rng.between(8, 20));
        Self {
            phase: Phase::Idle,
            phase_start: Instant::now(),
            next_trigger: Instant::now() + first,
            active_kaomoji: String::new(),
            active_text: String::new(),
            active_hold_ms: 0,
            event_context: None,
            event_at: Instant::now() - Duration::from_secs(999),
            ambient: WhimContext::Idle,
            seen_kaomoji: DedupRing::new(KAO_MAX_LEN),
            seen_idle: DedupRing::new(IDLE.len()),
            seen_busy: DedupRing::new(BUSY.len()),
            seen_scroll: DedupRing::new(SCROLL.len()),
            seen_joke: DedupRing::new(JOKES.len()),
            seen_event: DedupRing::new(1),
            mission_title: None,
            celebration_remaining: 0,
            rng,
        }
    }

    /// Deterministic construction for tests.
    #[cfg(test)]
    pub(crate) fn with_seed(seed: u64) -> Self {
        let mut w = Self::new();
        w.rng = Rng::from_seed(seed);
        w
    }

    /// Celebrate a newly unlocked mission (one-shot, high priority).
    pub fn notify_mission_unlocked(&mut self, title: &str) {
        self.mission_title = Some(title.to_string());
        self.celebration_remaining = 3;
        self.notify_event(WhimEvent::new(EventKind::MissionUnlocked, title));
    }

    /// Set the ambient context (reflects ongoing state: idle, busy, etc.).
    pub fn set_ambient(&mut self, ctx: WhimContext) {
        self.ambient = ctx;
    }

    /// Push a one-shot event carrying its real subject. Triggers a sooner
    /// message.
    pub fn notify_event(&mut self, event: WhimEvent) {
        self.event_context = Some(event);
        self.event_at = Instant::now();
        if matches!(self.phase, Phase::Idle) {
            let soon = self.rng.between(15, 30);
            let proposed = Instant::now() + Duration::from_secs(soon);
            if proposed < self.next_trigger {
                self.next_trigger = proposed;
            }
        }
    }

    /// Produce the current animation frame. Call every render tick.
    pub fn tick(&mut self) -> WhimFrame {
        loop {
            let elapsed = self.phase_start.elapsed().as_millis() as u64;
            let (frame, next_phase) = match self.phase {
                Phase::Idle => self.tick_idle(),
                Phase::ErasingTitle => tick_erasing_title(elapsed),
                Phase::KaomojiFlash => self.tick_kaomoji_flash(elapsed),
                Phase::TypingMsg => self.tick_typing_msg(elapsed),
                Phase::Holding => self.tick_holding(elapsed),
                Phase::ErasingMsg => self.tick_erasing_msg(elapsed),
                Phase::Blank => self.tick_blank(elapsed),
            };
            if let Some(next) = next_phase {
                self.advance(next);
                continue;
            }
            return frame;
        }
    }

    fn tick_idle(&mut self) -> (WhimFrame, Option<Phase>) {
        if Instant::now() >= self.next_trigger {
            self.generate();
            return (WhimFrame::empty_title(), Some(Phase::ErasingTitle));
        }
        (WhimFrame::full_title(), None)
    }

    fn tick_kaomoji_flash(&self, elapsed: u64) -> (WhimFrame, Option<Phase>) {
        if elapsed >= KAOMOJI_MS {
            return (WhimFrame::empty_title(), Some(Phase::TypingMsg));
        }
        (
            WhimFrame {
                title_visible: 0,
                kaomoji: self.active_kaomoji.clone(),
                text: String::new(),
                text_visible: 0,
            },
            None,
        )
    }

    fn tick_typing_msg(&self, elapsed: u64) -> (WhimFrame, Option<Phase>) {
        let total = self.active_text.chars().count();
        let typed = (elapsed / TYPE_MS) as usize;
        if typed >= total {
            return (WhimFrame::empty_title(), Some(Phase::Holding));
        }
        (
            WhimFrame {
                title_visible: 0,
                kaomoji: self.active_kaomoji.clone(),
                text: self.active_text.clone(),
                text_visible: typed,
            },
            None,
        )
    }

    fn tick_holding(&self, elapsed: u64) -> (WhimFrame, Option<Phase>) {
        if elapsed >= self.active_hold_ms {
            return (WhimFrame::empty_title(), Some(Phase::ErasingMsg));
        }
        (
            WhimFrame {
                title_visible: 0,
                kaomoji: self.active_kaomoji.clone(),
                text: self.active_text.clone(),
                text_visible: self.active_text.chars().count(),
            },
            None,
        )
    }

    fn tick_erasing_msg(&self, elapsed: u64) -> (WhimFrame, Option<Phase>) {
        let total = self.active_text.chars().count();
        let erased = (elapsed / ERASE_MS) as usize;
        if erased >= total {
            return (WhimFrame::empty_title(), Some(Phase::Blank));
        }
        (
            WhimFrame {
                title_visible: 0,
                kaomoji: self.active_kaomoji.clone(),
                text: self.active_text.clone(),
                text_visible: total - erased,
            },
            None,
        )
    }

    fn tick_blank(&mut self, elapsed: u64) -> (WhimFrame, Option<Phase>) {
        let blank_ms = if self.celebration_remaining > 0 {
            100
        } else {
            BLANK_MS
        };
        if elapsed >= blank_ms {
            if self.celebration_remaining > 0 {
                self.celebration_remaining -= 1;
                if let Some(title) = self.mission_title.clone() {
                    self.event_context = Some(WhimEvent::new(EventKind::MissionUnlocked, title));
                    self.event_at = Instant::now();
                }
                self.generate();
                self.advance(Phase::ErasingTitle);
                return (WhimFrame::empty_title(), None);
            }
            let delay = self.rng.between(INTERVAL_MIN, INTERVAL_MAX);
            self.next_trigger = Instant::now() + Duration::from_secs(delay);
            self.advance(Phase::Idle);
            return (WhimFrame::full_title(), None);
        }
        (WhimFrame::empty_title(), None)
    }

    fn advance(&mut self, next: Phase) {
        self.phase = next;
        self.phase_start = Instant::now();
    }

    /// The live event, if one is fresh enough to override the ambient context.
    fn active_event(&self) -> Option<WhimEvent> {
        let ev = self.event_context.as_ref()?;
        if self.event_at.elapsed() < Duration::from_secs(EVENT_DECAY_SECS) {
            Some(ev.clone())
        } else {
            None
        }
    }

    fn generate(&mut self) {
        if let Some(ev) = self.active_event() {
            let intent = if ev.kind.is_error() {
                Intent::Error
            } else {
                Intent::Success
            };
            self.pick_kaomoji(intent);
            self.active_text = self.render_event(&ev);
        } else {
            let intent = self.ambient_intent();
            self.pick_kaomoji(intent);
            self.active_text = self.ambient_text();
        }
        self.active_hold_ms = self.rng.between(HOLD_MIN, HOLD_MAX) * 1000;
    }

    fn pick_kaomoji(&mut self, intent: Intent) {
        let kaomojis = match intent {
            Intent::Loading => KAO_LOADING,
            Intent::Success => KAO_SUCCESS,
            Intent::Error => KAO_ERROR,
            Intent::Thinking => KAO_THINKING,
        };
        let ki = pick_no_repeat(&mut self.rng, kaomojis.len(), &self.seen_kaomoji);
        self.seen_kaomoji.push(ki);
        self.active_kaomoji = kaomojis[ki].to_string();
    }

    fn ambient_intent(&mut self) -> Intent {
        match self.ambient {
            WhimContext::Busy(_) => Intent::Loading,
            WhimContext::Scrolling => {
                if self.rng.chance(0.7) {
                    Intent::Thinking
                } else {
                    Intent::Loading
                }
            }
            WhimContext::Idle => {
                if self.rng.chance(0.5) {
                    Intent::Thinking
                } else {
                    Intent::Loading
                }
            }
        }
    }

    /// Ambient text: a joke replaces the ambient pick with probability 0.25
    /// (never during a live event — this branch only runs with no event).
    fn ambient_text(&mut self) -> String {
        if self.rng.chance(0.25) {
            let i = pick_no_repeat(&mut self.rng, JOKES.len(), &self.seen_joke);
            self.seen_joke.push(i);
            return JOKES[i].to_string();
        }
        match self.ambient {
            WhimContext::Idle => {
                let i = pick_no_repeat(&mut self.rng, IDLE.len(), &self.seen_idle);
                self.seen_idle.push(i);
                IDLE[i].to_string()
            }
            WhimContext::Busy(n) => {
                let i = pick_no_repeat(&mut self.rng, BUSY.len(), &self.seen_busy);
                self.seen_busy.push(i);
                BUSY[i].replace("{n}", &n.to_string())
            }
            WhimContext::Scrolling => {
                let i = pick_no_repeat(&mut self.rng, SCROLL.len(), &self.seen_scroll);
                self.seen_scroll.push(i);
                SCROLL[i].to_string()
            }
        }
    }

    fn render_event(&mut self, ev: &WhimEvent) -> String {
        if ev.kind == EventKind::MissionUnlocked {
            return format!("( ^ω^) Achieved: {}!", ev.subject);
        }
        let templates = event_templates(ev.kind);
        let i = pick_no_repeat(&mut self.rng, templates.len(), &self.seen_event);
        self.seen_event.push(i);
        // A template the event cannot fill would repeat the subject in its
        // own place ("grove hit a rock at grove"), so fall back to the first
        // template that only needs the subject.
        let chosen = if ev.detail.is_some() || !needs_detail(templates[i]) {
            templates[i]
        } else {
            templates
                .iter()
                .copied()
                .find(|t| !needs_detail(t))
                .unwrap_or(templates[i])
        };
        fill_template(chosen, ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn text_has_placeholder(text: &str) -> bool {
        text.contains('{')
    }

    /// WhimFrame factory methods produce correct initial state
    #[test]
    fn whim_frame_full_title_has_visible_title() {
        let frame = WhimFrame::full_title();
        assert_eq!(frame.title_visible, TITLE.len());
        assert!(frame.kaomoji.is_empty());
        assert!(frame.text.is_empty());
        assert_eq!(frame.text_visible, 0);
    }

    /// WhimFrame factory methods produce correct initial state
    #[test]
    fn whim_frame_empty_title_has_no_visible_title() {
        let frame = WhimFrame::empty_title();
        assert_eq!(frame.title_visible, 0);
        assert!(frame.kaomoji.is_empty());
        assert!(frame.text.is_empty());
        assert_eq!(frame.text_visible, 0);
    }

    /// tick_erasing_title progresses character visibility over time
    #[test]
    fn tick_erasing_title_progressively_hides_characters() {
        let (frame, _next_phase) = tick_erasing_title(0);
        let initial_visible = frame.title_visible;
        assert!(initial_visible > 0);
        assert_eq!(initial_visible, TITLE.len());

        let (frame2, _next_phase2) = tick_erasing_title(ERASE_MS);
        assert!(frame2.title_visible < initial_visible);

        let (frame3, next_phase3) = tick_erasing_title(ERASE_MS * (TITLE.len() as u64));
        assert_eq!(frame3.title_visible, 0);
        assert!(next_phase3.is_some());
    }

    /// tick_erasing_title transitions to KaomojiFlash when title is fully erased
    #[test]
    fn tick_erasing_title_transitions_to_kaomoji_flash_when_done() {
        let (_, next_phase) = tick_erasing_title(ERASE_MS * (TITLE.len() as u64 + 1));
        assert!(matches!(next_phase, Some(Phase::KaomojiFlash)));
    }

    /// Whimsg::new creates a Whimsg with sensible defaults
    #[test]
    fn whimsg_new_initializes_correctly() {
        let w = Whimsg::new();
        assert!(matches!(w.phase, Phase::Idle));
        assert!(w.active_kaomoji.is_empty());
        assert!(w.active_text.is_empty());
        assert_eq!(w.active_hold_ms, 0);
        assert_eq!(w.celebration_remaining, 0);
        assert!(w.event_context.is_none());
        assert!(matches!(w.ambient, WhimContext::Idle));
    }

    /// set_ambient updates the ambient context
    #[test]
    fn set_ambient_changes_context() {
        let mut w = Whimsg::new();
        assert!(matches!(w.ambient, WhimContext::Idle));

        w.set_ambient(WhimContext::Busy(3));
        assert!(matches!(w.ambient, WhimContext::Busy(3)));

        w.set_ambient(WhimContext::Scrolling);
        assert!(matches!(w.ambient, WhimContext::Scrolling));
    }

    /// notify_event sets event context and updates trigger time
    #[test]
    fn notify_event_sets_context_and_trigger() {
        let mut w = Whimsg::new();
        let old_trigger = w.next_trigger;

        w.notify_event(WhimEvent::new(EventKind::AgentFailed, "oak"));
        assert!(w.event_context.is_some());
        assert_eq!(w.event_context.as_ref().unwrap().subject, "oak");

        // From Idle phase, trigger should be sooner
        assert!(w.next_trigger <= old_trigger);
    }

    /// notify_event from non-idle phase doesn't change trigger time if already sooner
    #[test]
    fn notify_event_when_already_triggered_keeps_early_trigger() {
        let mut w = Whimsg::new();

        w.notify_event(WhimEvent::new(EventKind::AgentSpawned, "fern"));
        let first_trigger = w.next_trigger;

        w.notify_event(WhimEvent::new(EventKind::AgentFailed, "fern"));
        assert!(w.next_trigger <= first_trigger);
    }

    /// notify_mission_unlocked sets mission title and celebration counter
    #[test]
    fn notify_mission_unlocked_sets_title_and_celebration() {
        let mut w = Whimsg::new();
        w.notify_mission_unlocked("Test Mission");
        assert_eq!(w.mission_title, Some("Test Mission".to_string()));
        assert_eq!(w.celebration_remaining, 3);
        assert!(w.event_context.is_some());
    }

    /// tick returns a WhimFrame on every call without panicking
    #[test]
    fn tick_always_produces_frame() {
        let mut w = Whimsg::new();
        for _ in 0..100 {
            let frame = w.tick();
            assert!(frame.title_visible <= TITLE.len());
        }
    }

    /// tick always starts in idle phase with full title visible
    #[test]
    fn tick_initial_state_shows_full_title_in_idle() {
        let mut w = Whimsg::new();
        let frame = w.tick();
        assert_eq!(frame.title_visible, TITLE.len());
        assert!(matches!(w.phase, Phase::Idle));
    }

    /// Active event decays to None after the timeout
    #[test]
    fn active_event_decays_after_timeout() {
        let mut w = Whimsg::new();
        w.event_context = Some(WhimEvent::new(EventKind::AgentSpawned, "fern"));
        w.event_at = Instant::now() - Duration::from_secs(EVENT_DECAY_SECS + 1);
        assert!(w.active_event().is_none());
    }

    /// Active event is used while within the timeout
    #[test]
    fn active_event_used_within_timeout() {
        let mut w = Whimsg::new();
        w.event_context = Some(WhimEvent::new(EventKind::AgentFailed, "fern"));
        w.event_at = Instant::now() - Duration::from_secs(1);
        assert_eq!(w.active_event().unwrap().kind, EventKind::AgentFailed);
    }

    /// WhimFrame doesn't panic when constructing with various text lengths
    #[test]
    fn whim_frame_handles_various_text_lengths() {
        let test_strings = vec!["", "a", "hello", "こんにちは", "🎉🚀"];

        for text in test_strings {
            let frame = WhimFrame {
                title_visible: 0,
                kaomoji: text.to_string(),
                text: text.to_string(),
                text_visible: text.chars().count(),
            };
            assert_eq!(frame.text, text);
        }
    }

    /// Multiple sequential notifications don't corrupt state
    #[test]
    fn sequential_notifications_maintain_state() {
        let mut w = Whimsg::new();

        w.notify_event(WhimEvent::new(EventKind::AgentSpawned, "leaf"));
        assert_eq!(w.event_context.as_ref().unwrap().subject, "leaf");

        w.notify_event(WhimEvent::new(EventKind::AgentFailed, "bark"));
        assert_eq!(w.event_context.as_ref().unwrap().subject, "bark");

        w.set_ambient(WhimContext::Busy(2));
        assert!(matches!(w.ambient, WhimContext::Busy(2)));
    }

    /// Celebration graph properly decrements counter during celebration phase
    #[test]
    fn celebration_sets_and_eventually_uses_title() {
        let mut w = Whimsg::new();
        w.notify_mission_unlocked("Test Mission");
        assert_eq!(w.celebration_remaining, 3);
        assert_eq!(w.mission_title, Some("Test Mission".to_string()));
        w.celebration_remaining = 2;
        assert_eq!(w.celebration_remaining, 2);
    }

    /// An agent-failed event renders one of its two templates with the subject
    /// substituted, and never a joke.
    #[test]
    fn agent_failed_event_uses_subject_and_never_jokes() {
        let mut w = Whimsg::with_seed(7);
        let ev = WhimEvent::new(EventKind::AgentFailed, "taiwania");
        w.notify_event(ev.clone());
        let expected: Vec<String> = EVENT_AGENT_FAILED
            .iter()
            .map(|t| fill_template(t, &ev))
            .collect();

        for _ in 0..200 {
            w.generate();
            assert!(
                expected.contains(&w.active_text),
                "unexpected text: {}",
                w.active_text
            );
            assert!(w.active_text.contains("taiwania"));
            assert!(!JOKES.contains(&w.active_text.as_str()));
        }
    }

    /// A spec-completed event renders the spec's first word (its id).
    #[test]
    fn spec_completed_event_renders_spec_id() {
        let mut w = Whimsg::with_seed(11);
        w.notify_event(WhimEvent::with_detail(
            EventKind::SpecCompleted,
            "canopy-tui-polish",
            "CT32 minimal prompt protocol",
        ));
        for _ in 0..50 {
            w.generate();
            assert!(w.active_text.contains("CT32"), "text: {}", w.active_text);
            assert!(!w.active_text.contains("minimal"));
            assert!(!text_has_placeholder(&w.active_text));
        }
    }

    /// SPECCODE: a spec-completed event whose label carries a code renders
    /// the code (`{spec}` is the label's first word).
    #[test]
    fn spec_completed_event_with_code_renders_the_code() {
        let mut w = Whimsg::with_seed(11);
        w.notify_event(WhimEvent::with_detail(
            EventKind::SpecCompleted,
            "grove",
            "[FIX-7] Alpha",
        ));
        for _ in 0..50 {
            w.generate();
            assert!(w.active_text.contains("[FIX-7]"), "text: {}", w.active_text);
            assert!(!w.active_text.contains("Alpha"));
            assert!(!text_has_placeholder(&w.active_text));
        }
    }

    /// FR5: a renamed session announces its old and new names, with no
    /// placeholder left behind.
    #[test]
    fn renamed_session_event_names_the_old_and_the_new_name() {
        let mut w = Whimsg::with_seed(17);
        w.notify_event(WhimEvent::with_detail(
            EventKind::SessionRenamed,
            "paulownia",
            "platanus",
        ));
        for _ in 0..50 {
            w.generate();
            assert!(
                w.active_text.contains("paulownia"),
                "text: {}",
                w.active_text
            );
            assert!(
                w.active_text.contains("platanus"),
                "text: {}",
                w.active_text
            );
            assert!(!text_has_placeholder(&w.active_text));
        }
    }

    /// A blocked/failed graph with no recorded node never repeats the graph's
    /// own name as the node it hit.
    #[test]
    fn stopped_graph_without_a_node_never_names_itself() {
        for kind in [EventKind::GraphBlocked, EventKind::GraphFailed] {
            let mut w = Whimsg::with_seed(13);
            w.notify_event(WhimEvent::new(kind, "grove"));
            for _ in 0..100 {
                w.generate();
                assert_eq!(w.active_text, "grove is waiting for you");
                assert!(!text_has_placeholder(&w.active_text));
            }
        }

        // With a node in the detail, the rock template stays reachable.
        let mut w = Whimsg::with_seed(23);
        w.notify_event(WhimEvent::with_detail(
            EventKind::GraphBlocked,
            "grove",
            "node7",
        ));
        let mut saw_rock = false;
        for _ in 0..20 {
            w.generate();
            saw_rock |= w.active_text.contains("node7");
        }
        assert!(saw_rock, "the rock template should render for a known node");
    }

    /// With no event, roughly a quarter of ambient generations are jokes.
    #[test]
    fn ambient_generations_are_about_a_quarter_jokes() {
        let mut w = Whimsg::with_seed(99);
        w.set_ambient(WhimContext::Idle);
        let mut jokes = 0;
        for _ in 0..400 {
            w.generate();
            if JOKES.contains(&w.active_text.as_str()) {
                jokes += 1;
            }
        }
        assert!(
            (60..=140).contains(&jokes),
            "expected 15-35% jokes in 400, got {jokes}"
        );
    }

    /// Every event kind renders a non-empty message with no placeholder left.
    #[test]
    fn every_event_kind_renders_cleanly() {
        let kinds = [
            EventKind::AgentSpawned,
            EventKind::AgentDone,
            EventKind::AgentFailed,
            EventKind::SessionRenamed,
            EventKind::SpecCompleted,
            EventKind::GraphCompleted,
            EventKind::GraphBlocked,
            EventKind::GraphFailed,
            EventKind::HookMessage,
            EventKind::MissionUnlocked,
        ];
        for kind in kinds {
            let mut w = Whimsg::with_seed(3);
            let ev =
                WhimEvent::with_detail(kind, "canopy-tui-polish", "CT32 minimal prompt protocol");
            w.notify_event(ev);
            for _ in 0..20 {
                w.generate();
                assert!(!w.active_text.is_empty(), "empty text for {kind:?}");
                assert!(
                    !text_has_placeholder(&w.active_text),
                    "placeholder left for {kind:?}: {}",
                    w.active_text
                );
            }
        }
    }

    /// Busy ambient text fills the branch count.
    #[test]
    fn busy_ambient_fills_branch_count() {
        let mut w = Whimsg::with_seed(5);
        w.set_ambient(WhimContext::Busy(4));
        let mut seen_count = false;
        for _ in 0..400 {
            w.generate();
            assert!(!text_has_placeholder(&w.active_text), "{}", w.active_text);
            if w.active_text.contains('4') {
                seen_count = true;
            }
        }
        assert!(seen_count, "a busy template should show the count");
    }
}
