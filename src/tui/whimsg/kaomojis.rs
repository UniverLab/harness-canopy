//! Whimsical personality — animated kaomoji + contextual messages.
//!
//! Animation cycle:
//!   "harness-canopy" erases right-to-left → kaomoji flashes →
//!   message types left-to-right → holds 5-9s → erases right-to-left →
//!   brief blank → "harness-canopy" returns.

// ── Timing ────────────────────────────────────────────────────────

pub const TITLE: &str = "harness-canopy";
pub(crate) const ERASE_MS: u64 = 35;
pub(crate) const TYPE_MS: u64 = 45;
pub(crate) const KAOMOJI_MS: u64 = 400;
pub(crate) const BLANK_MS: u64 = 200;
pub(crate) const HOLD_MIN: u64 = 4;
pub(crate) const HOLD_MAX: u64 = 8;
pub(crate) const INTERVAL_MIN: u64 = 60;
pub(crate) const INTERVAL_MAX: u64 = 180;
pub(crate) const EVENT_DECAY_SECS: u64 = 15;

// ── Kaomojis ──────────────────────────────────────────────────────
//
// Every glyph here renders cleanly one `char` at a time: no combining marks
// (Unicode general category Mn/Me), which the header would otherwise stack on
// the preceding cell. Guarded by a test below.

pub(crate) const KAO_LOADING: &[&str] = &[
    "(Ծ‸ Ծ)",
    "( ≖.≖)",
    "(ㆆ_ㆆ)",
    "(◑_◑)",
    "(•_•)",
    "(o_o )",
    "(¬‿¬)",
];
pub(crate) const KAO_SUCCESS: &[&str] = &[
    "(*^‿^*)",
    "(◕‿◕)",
    "(っ▀¯▀)つ",
    "ヾ(´〇`)ﾉ♪♪♪",
    "٩(˘◡˘)۶",
    "(ᵔᵕᵔ)",
    "(๑˃ᴗ˂)ﻭ",
    "(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧",
    "(b ᵔ▽ᵔ)b",
    "(っ◕‿◕)っ",
    "(づ ◕‿◕ )づ",
];
pub(crate) const KAO_ERROR: &[&str] = &[
    "ಥ_ಥ",
    "(҂◡_◡)",
    "(Ծ‸ Ծ)",
    "¯\\_(ツ)_/¯",
    "(╥﹏╥)",
    "( ˘︹˘ )",
    "(ノಠ益ಠ)ノ彡┻━┻",
    "(ಥ﹏ಥ)",
    "(×_×)",
];
pub(crate) const KAO_THINKING: &[&str] = &[
    "(ʘ_ʘ)",
    "(º_º)",
    "(￢_￢)",
    "(._.)",
    "ఠ_ఠ",
    "(⊙_◎)",
    "(꜆꜄ * )꜆꜄",
    "(￣ω￣;)",
    "( ˘▽˘)っ旦",
];

/// The longest kaomoji list — the no-repeat ring is sized to it.
pub(crate) const KAO_MAX_LEN: usize = 11;

// ── Types ─────────────────────────────────────────────────────────

/// Ambient context — the ongoing state behind the header animation, used only
/// when no live event is fresh (see `Whimsg::active_event`). Events themselves
/// carry their subject as a `whimsg::events::WhimEvent`, not a context.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WhimContext {
    Idle,
    /// `n` live branches: running agents plus running graphs.
    Busy(usize),
    Scrolling,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True for a Unicode combining mark: the header types one `char` at a
    /// time, so a combining mark would merge into the previous cell.
    fn has_combining_mark(s: &str) -> bool {
        s.chars().any(|c| {
            matches!(
                c as u32,
                0x0300..=0x036F
                    | 0x1AB0..=0x1AFF
                    | 0x1DC0..=0x1DFF
                    | 0x20D0..=0x20FF
                    | 0xFE20..=0xFE2F
            )
        })
    }

    /// No kaomoji in any list may contain a combining mark.
    #[test]
    fn kaomojis_have_no_combining_marks() {
        for list in [KAO_LOADING, KAO_SUCCESS, KAO_ERROR, KAO_THINKING] {
            for kao in list {
                assert!(
                    !has_combining_mark(kao),
                    "kaomoji {kao:?} contains a combining mark"
                );
            }
        }
    }

    /// The kaomoji lists are exactly the curated ones, in order.
    #[test]
    fn kaomoji_lists_are_the_curated_ones() {
        assert_eq!(
            KAO_LOADING,
            &[
                "(Ծ‸ Ծ)",
                "( ≖.≖)",
                "(ㆆ_ㆆ)",
                "(◑_◑)",
                "(•_•)",
                "(o_o )",
                "(¬‿¬)"
            ]
        );
        assert_eq!(
            KAO_ERROR,
            &[
                "ಥ_ಥ",
                "(҂◡_◡)",
                "(Ծ‸ Ծ)",
                "¯\\_(ツ)_/¯",
                "(╥﹏╥)",
                "( ˘︹˘ )",
                "(ノಠ益ಠ)ノ彡┻━┻",
                "(ಥ﹏ಥ)",
                "(×_×)",
            ]
        );
        assert_eq!(
            KAO_SUCCESS,
            &[
                "(*^‿^*)",
                "(◕‿◕)",
                "(っ▀¯▀)つ",
                "ヾ(´〇`)ﾉ♪♪♪",
                "٩(˘◡˘)۶",
                "(ᵔᵕᵔ)",
                "(๑˃ᴗ˂)ﻭ",
                "(ﾉ◕ヮ◕)ﾉ*:･ﾟ✧",
                "(b ᵔ▽ᵔ)b",
                "(っ◕‿◕)っ",
                "(づ ◕‿◕ )づ",
            ]
        );
        assert_eq!(
            KAO_THINKING,
            &[
                "(ʘ_ʘ)",
                "(º_º)",
                "(￢_￢)",
                "(._.)",
                "ఠ_ఠ",
                "(⊙_◎)",
                "(꜆꜄ * )꜆꜄",
                "(￣ω￣;)",
                "( ˘▽˘)っ旦",
            ]
        );
    }

    /// The ring capacity covers the largest list.
    #[test]
    fn kao_max_len_covers_every_list() {
        for list in [KAO_LOADING, KAO_SUCCESS, KAO_ERROR, KAO_THINKING] {
            assert!(list.len() <= KAO_MAX_LEN);
        }
    }
}
