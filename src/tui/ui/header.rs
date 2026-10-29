//! Header bar rendering — animated title + daemon status indicator.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme::Theme;
use super::ERROR_COLOR;
use crate::shared::banner::BANNER_GRADIENT;
use crate::tui::app::types::App;
use crate::tui::whimsg::TITLE;

/// Return the first `n` chars of `s`, respecting char boundaries.
fn first_n_chars(s: &str, n: usize) -> &str {
    let end = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

const SPINNER: [&str; 8] = ["⣷", "⣯", "⣟", "⡿", "⢿", "⣻", "⣽", "⣾"];

/// Milliseconds between sweep-head steps: the sweep advances one character
/// every `SWEEP_STEP_MS`, so a slower value means a slower, calmer motion.
const SWEEP_STEP_MS: u128 = 140;
/// Milliseconds the title rests (fully idle) between sweeps.
const SWEEP_REST_MS: u128 = 2000;

// THEME-EXEMPT: the banner gradient is its own authored animation palette
// (`BANNER_GRADIENT`), independent of classic/modern — not a theme role.
fn sweep_color(char_idx: usize, len: usize, millis: u128) -> Color {
    let gradient_idx = match sweep_head(millis, len) {
        Some(step) => {
            // Head runs from -2 to len+1 so the falloff enters and leaves off-name.
            let head = -2i128 + step as i128;
            match (char_idx as i128 - head).abs() {
                0 => 4,
                1 => 3,
                2 => 2,
                _ => 1,
            }
        }
        None => 1,
    };
    let (r, g, b) = BANNER_GRADIENT[gradient_idx];
    // THEME-EXEMPT: see function doc above.
    Color::Rgb(r, g, b)
}

/// Sweep-head step for `millis`: `Some(step)` while the sweep travels,
/// `None` during the rest phase (fully idle).
pub(crate) fn sweep_head(millis: u128, len: usize) -> Option<u64> {
    let sweep = (len as u128 + 4) * SWEEP_STEP_MS;
    let t = millis % (sweep + SWEEP_REST_MS);
    if t < sweep {
        Some((t / SWEEP_STEP_MS) as u64)
    } else {
        None
    }
}

/// Spinner frame index for `millis` (the daemon-running indicator).
pub(crate) fn spinner_index(millis: u128) -> usize {
    ((millis / 125) % 8) as usize
}

/// Whether the stopped-daemon block shows lit (vs. dim) at `millis`.
pub(crate) fn blink_on(millis: u128) -> bool {
    (millis / 500).is_multiple_of(2)
}

/// Discrete animation frame the header renders: spinner index, sweep-head
/// step (`None` while resting) and blink phase. The event loop compares it
/// across ticks so an idle header doesn't redraw every tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct HeaderFrameId {
    pub spinner: u8,
    pub sweep: Option<u64>,
    pub blink: bool,
}

/// The header frame `millis` renders. The sweep period uses the full title
/// length: exact while the title shows (the idle case); during whimsg
/// messages the whimsg check dirties the tick anyway.
pub(crate) fn header_frame_id(millis: u128, daemon_running: bool) -> HeaderFrameId {
    HeaderFrameId {
        // The spinner only renders while the daemon runs; the blink only
        // while it is stopped. Gating each on its visible side keeps a
        // hidden animation from dirtying the tick.
        spinner: if daemon_running {
            u8::try_from(spinner_index(millis)).unwrap_or(0)
        } else {
            0
        },
        sweep: sweep_head(millis, TITLE.chars().count()),
        blink: !daemon_running && blink_on(millis),
    }
}

fn push_animated_gradient_text(spans: &mut Vec<Span>, visible: &str, millis: u128) {
    let chars: Vec<char> = visible.chars().collect();
    let len = chars.len();
    for (i, ch) in chars.into_iter().enumerate() {
        spans.push(Span::styled(
            ch.to_string(),
            Style::default()
                .fg(sweep_color(i, len, millis))
                .add_modifier(Modifier::BOLD),
        ));
    }
}

pub(super) fn draw_header(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    // Both animation toggles off freezes the header: spinner[0], sweep t=0,
    // solid block (see `header_millis`).
    let millis = super::header_millis(app);

    let (status_char, status_color) = if app.daemon_running {
        // Smooth spinner in green, no blink
        let frame_idx = spinner_index(millis);
        (SPINNER[frame_idx], theme.header_color)
    } else {
        // Blinking █ in red when stopped.
        let lit = blink_on(millis);
        let color = if lit {
            ERROR_COLOR
        } else {
            // THEME-EXEMPT: single-use dim variant of the error red — not a recurring role.
            Color::Rgb(120, 60, 60)
        };
        ("█", color)
    };

    let wf = app.whimsg.tick();
    let mut spans: Vec<Span> = Vec::new();
    // Leading padding so the title/whimsg block isn't flush against the left border
    spans.push(Span::raw(" "));

    if wf.title_visible > 0 {
        // Title partially or fully visible - animated gradient per letter.
        let visible = first_n_chars(TITLE, wf.title_visible);
        push_animated_gradient_text(&mut spans, visible, millis);
        spans.push(Span::raw(" "));
    } else if !wf.kaomoji.is_empty() && wf.text_visible == 0 && wf.text.is_empty() {
        // Kaomoji flash with the same animated gradient style as the title.
        push_animated_gradient_text(&mut spans, &wf.kaomoji, millis);
        spans.push(Span::raw(" "));
    } else if !wf.kaomoji.is_empty() {
        // Kaomoji with animated gradient + message in gray without background.
        let visible_text = first_n_chars(&wf.text, wf.text_visible);
        push_animated_gradient_text(&mut spans, &wf.kaomoji, millis);
        spans.push(Span::raw(" "));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{} ", visible_text),
            Style::default()
                // THEME-EXEMPT: single-use whimsy-message gray — not a recurring role.
                .fg(Color::Rgb(140, 140, 140))
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        // Blank phase — leading space already present
    }

    let left = Paragraph::new(Line::from(spans));
    frame.render_widget(left, area);

    // Daemon status spinner on the right edge
    if area.width > 3 {
        let status = Paragraph::new(Line::from(Span::styled(
            status_char,
            Style::default().fg(status_color),
        )));
        let status_area = Rect::new(area.x + area.width - 2, area.y, 1, 1);
        frame.render_widget(status, status_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn darkest() -> Color {
        let (r, g, b) = BANNER_GRADIENT[4];
        Color::Rgb(r, g, b)
    }

    fn idle() -> Color {
        let (r, g, b) = BANNER_GRADIENT[1];
        Color::Rgb(r, g, b)
    }

    /// Cycle length in ms for a name of `len` characters, per the spec.
    fn cycle_ms(len: usize) -> u128 {
        (len as u128 + 4) * SWEEP_STEP_MS + SWEEP_REST_MS
    }

    /// Sweep head position in ms where the sweep completes for a `len`-char name.
    fn sweep_ms(len: usize) -> u128 {
        (len as u128 + 4) * SWEEP_STEP_MS
    }

    /// At most one character wears the darkest tone at any instant.
    #[test]
    fn sweep_at_most_one_darkest() {
        let len = 14;
        for ms in (0..cycle_ms(len)).step_by(10) {
            let count = (0..len)
                .filter(|&i| sweep_color(i, len, ms) == darkest())
                .count();
            assert!(
                count <= 1,
                "at {ms} ms {count} chars were darkest (expected at most 1)"
            );
        }
    }

    /// During the rest window every character is idle.
    #[test]
    fn sweep_rest_all_idle() {
        let len = 14;
        for ms in (sweep_ms(len)..cycle_ms(len)).step_by(10) {
            let count = (0..len)
                .filter(|&i| sweep_color(i, len, ms) == darkest())
                .count();
            assert_eq!(count, 0, "at {ms} ms a char was darkest during rest");
            for i in 0..len {
                assert_eq!(
                    sweep_color(i, len, ms),
                    idle(),
                    "at {ms} ms char {i} was not idle during rest"
                );
            }
        }
    }

    /// The head lands exactly where the `-2 + t/140` formula says it should.
    #[test]
    fn sweep_head_positions() {
        assert_eq!(sweep_color(0, 14, 280), darkest());
        assert_ne!(sweep_color(0, 14, 420), darkest());
        assert_eq!(sweep_color(1, 14, 420), darkest());
    }

    /// Degenerate lengths must not panic or divide by zero.
    #[test]
    fn sweep_short_len_no_panic() {
        let _ = sweep_color(0, 1, 0);
        let _ = sweep_color(0, 0, 0);
        for len in 0..=1 {
            for ms in (0..6000).step_by(140) {
                let _ = sweep_color(0, len, ms);
            }
        }
    }
}
