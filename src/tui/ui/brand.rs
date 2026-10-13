//! Canopy brand palette, ported from the web's `tui` surface
//! (`univerlab/src/styles/global.css`, the `[data-surface='tui']` blocks), so
//! the TUI reads like Canopy's page on univerlab.org.

use ratatui::style::Color;

/// Canopy's tonal scale, dark → light. The middle stop is the decided
/// essence `#00a9a0` (Intelligence `944ada9e`); every stop is the teal
/// family the web builds from that essence.
pub const CANOPY_SCALE: [(u8, u8, u8); 6] = [
    (0, 51, 48),    // #003330  darkest stop — background tone only
    (0, 102, 97),   // #006661
    (0, 169, 160),  // #00a9a0  essence
    (0, 224, 212),  // #00e0d4
    (31, 255, 243), // #1ffff3
    (82, 255, 246), // #52fff6  lightest stop
];

/// The essence: Canopy has exactly one teal.
pub const ESSENCE: (u8, u8, u8) = CANOPY_SCALE[2];

/// Page background (`#012624`).
pub const BG: (u8, u8, u8) = (1, 38, 36);
/// Deepest background (`#011d1c`) — sidebar and inverse-text base.
pub const BG_DEEP: (u8, u8, u8) = (1, 29, 28);
/// Raised surface (`#003330`) — dialogs, selected rows, focused fields.
pub const BG_RAISE: (u8, u8, u8) = CANOPY_SCALE[0];
/// Primary ink (`#edfffe`).
pub const INK: (u8, u8, u8) = (237, 255, 254);
/// Dimmed ink (`#bbc7c6`).
pub const INK_DIM: (u8, u8, u8) = (188, 199, 198);
/// Faintest ink (`#9aa8a7`).
pub const INK_FAINT: (u8, u8, u8) = (154, 168, 167);
/// Hairline (`#1a3c3a`) — `rgba(255,255,255,.10)` composited over [`BG`].
pub const LINE: (u8, u8, u8) = (26, 60, 58);
/// Phosphor highlight (`#fde9ff`). Part of the published palette; kept so the
/// brand tokens stay 1:1 with the web's CSS even before a renderer consumes it.
#[allow(dead_code)]
pub const PHOSPHOR: (u8, u8, u8) = (253, 233, 255);

/// Wrap a palette tuple as a ratatui [`Color`].
pub const fn rgb(token: (u8, u8, u8)) -> Color {
    Color::Rgb(token.0, token.1, token.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_stops_are_the_published_tone() {
        assert_eq!(
            CANOPY_SCALE,
            [
                (0, 51, 48),
                (0, 102, 97),
                (0, 169, 160),
                (0, 224, 212),
                (31, 255, 243),
                (82, 255, 246),
            ]
        );
        assert_eq!(ESSENCE, CANOPY_SCALE[2]);
        assert_eq!(BG_RAISE, CANOPY_SCALE[0]);
    }

    #[test]
    fn named_tokens_keep_their_hex_values() {
        assert_eq!(BG, (1, 38, 36));
        assert_eq!(BG_DEEP, (1, 29, 28));
        assert_eq!(INK, (237, 255, 254));
        assert_eq!(INK_DIM, (188, 199, 198));
        assert_eq!(INK_FAINT, (154, 168, 167));
        assert_eq!(LINE, (26, 60, 58));
        assert_eq!(PHOSPHOR, (253, 233, 255));
    }

    #[test]
    fn rgb_wraps_a_token() {
        assert_eq!(rgb(ESSENCE), Color::Rgb(0, 169, 160));
    }
}
