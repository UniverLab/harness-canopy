//! Centralized color palette for the TUI. Every renderer (header, footer,
//! sidebar, panels, system dashboard, dialogs) consumes this as of T2-T4;
//! no renderer reads a hardcoded color constant directly anymore.

use super::brand::{self, rgb};
use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub border_color: Color,
    pub panel_bg: Color,
    pub sidebar_bg: Color,
    pub selected_bg: Color,
    pub header_color: Color,
    pub dim_text: Color,
    pub show_borders: bool,
    /// Dialog/modal chrome background (C4 audit: `Color::Rgb(15, 25, 15)`
    /// was hardcoded identically across every file in `ui/dialogs/`).
    pub dialog_bg: Color,
    /// Default readable body/value text (C4 audit: bare `Color::White` at
    /// dozens of call sites across the sidebar and dialogs).
    pub text_primary: Color,
    /// Foreground for text drawn on top of an accent/header-colored
    /// background — e.g. a focused, selected row (C4 audit: `Color::Black`).
    pub accent_fg: Color,
    /// Muted secondary text distinct from `dim_text` — empty-state hints
    /// and similar (C4 audit: `Color::DarkGray`).
    pub muted_text: Color,
    /// Pending/in-progress/caution state (C4 audit: `Color::Yellow`).
    pub warning: Color,
    /// Failed/unavailable state (C4 audit: `Color::Red`).
    pub error: Color,
    /// Active/success indicator distinct from the status-role constants in
    /// `ui/mod.rs` (C4 audit: a bare green literal).
    pub success: Color,
    /// Input-field background, unfocused (C4 audit: `Color::Rgb(30, 30, 30)`
    /// in the prompt builder's section fields).
    pub field_bg: Color,
    /// Input-field background, focused — the theme-driven pair for
    /// `field_bg` (C4 audit: `Color::Rgb(40, 40, 40)`).
    pub field_bg_focused: Color,
    /// Graph status palette — routed through Theme so the drawing never
    /// ships with fixed colors (CT2).
    pub status_running: Color,
    pub status_ok: Color,
    pub status_fail: Color,
    pub status_disabled: Color,
    pub status_interrupted: Color,
    pub kind_router: Color,
    /// Meaning-tinted grounds from the web look kit: info, ok, warning and
    /// error blocks, plus the neutral stage big views stand on.
    pub info_bg: Color,
    pub ok_bg: Color,
    pub warn_bg: Color,
    pub error_bg: Color,
    pub stage_bg: Color,
}

impl Theme {
    /// The web's Canopy `tui` surface: the teal brand palette from
    /// `univerlab/src/styles/global.css`, with `#00a9a0` as the one essence.
    pub fn classic() -> Self {
        Self {
            border_color: rgb(brand::LINE),
            panel_bg: rgb(brand::BG),
            sidebar_bg: rgb(brand::BG_DEEP),
            selected_bg: rgb(brand::BG_RAISE),
            header_color: rgb(brand::ESSENCE),
            dim_text: rgb(brand::INK_DIM),
            show_borders: true,
            dialog_bg: rgb(brand::BG_DEEP),
            text_primary: rgb(brand::INK),
            accent_fg: rgb(brand::BG_DEEP),
            muted_text: rgb(brand::INK_FAINT),
            warning: Color::Yellow,
            error: Color::Red,
            success: rgb(brand::ESSENCE),
            field_bg: Color::Rgb(2, 46, 43),
            field_bg_focused: rgb(brand::BG_RAISE),
            status_running: rgb(brand::ESSENCE),
            status_ok: Color::Rgb(66, 165, 245),
            status_fail: Color::Rgb(229, 57, 53),
            status_disabled: Color::Rgb(120, 120, 120),
            status_interrupted: Color::Rgb(255, 179, 0),
            kind_router: Color::Rgb(171, 71, 188),
            info_bg: rgb(brand::TINT_INFO),
            ok_bg: rgb(brand::TINT_OK),
            warn_bg: rgb(brand::TINT_WARN),
            error_bg: rgb(brand::TINT_ERROR),
            stage_bg: rgb(brand::STAGE),
        }
    }

    /// Borderless, background-contrast look (T5): panels are separated by
    /// differing background colors instead of box-drawing borders.
    pub fn modern() -> Self {
        let panel_bg = Color::Rgb(30, 30, 42);
        Self {
            border_color: Color::Rgb(42, 42, 60),
            panel_bg,
            sidebar_bg: Color::Rgb(14, 14, 22),
            selected_bg: Color::Rgb(44, 44, 62),
            header_color: rgb(brand::ESSENCE),
            dim_text: Color::Rgb(140, 140, 155),
            show_borders: false,
            dialog_bg: Color::Rgb(14, 14, 22),
            text_primary: Color::Rgb(225, 225, 230),
            accent_fg: Color::Rgb(18, 18, 24),
            muted_text: Color::Rgb(130, 130, 145),
            warning: Color::Rgb(215, 180, 90),
            error: Color::Rgb(195, 95, 95),
            success: rgb(brand::ESSENCE),
            field_bg: Color::Rgb(38, 38, 50),
            field_bg_focused: Color::Rgb(48, 48, 64),
            status_running: rgb(brand::ESSENCE),
            status_ok: Color::Rgb(66, 165, 245),
            status_fail: Color::Rgb(229, 57, 53),
            status_disabled: Color::Rgb(120, 120, 120),
            status_interrupted: Color::Rgb(255, 179, 0),
            kind_router: Color::Rgb(171, 71, 188),
            info_bg: Color::Rgb(16, 20, 34),
            ok_bg: Color::Rgb(14, 28, 24),
            warn_bg: Color::Rgb(30, 18, 28),
            error_bg: Color::Rgb(30, 16, 20),
            stage_bg: Color::Rgb(10, 10, 16),
        }
    }

    /// Resolve a `Theme` from the persisted `CanopyConfig::theme` string
    /// (T6): `"modern"` -> [`Theme::modern`], anything else -> classic.
    /// An unrecognized value (a config from a newer binary, a typo, etc.)
    /// falls back to classic with a warning instead of failing to start.
    pub fn resolve(config_value: &str) -> Self {
        match config_value {
            "classic" => Self::classic(),
            "modern" => Self::modern(),
            other => {
                tracing::warn!(theme = other, "unknown theme in config, using classic");
                Self::classic()
            }
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::classic()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_matches_default() {
        assert_eq!(Theme::classic(), Theme::default());
    }

    #[test]
    fn classic_reproduces_current_constants() {
        let theme = Theme::classic();
        assert_eq!(theme.border_color, Color::Rgb(26, 60, 58)); // LINE
        assert_eq!(theme.panel_bg, Color::Rgb(1, 38, 36)); // BG
        assert_eq!(theme.sidebar_bg, Color::Rgb(1, 29, 28)); // BG_DEEP
        assert_eq!(theme.selected_bg, Color::Rgb(0, 51, 48)); // BG_RAISE
        assert_eq!(theme.header_color, rgb(brand::ESSENCE));
        assert_eq!(theme.dim_text, Color::Rgb(188, 199, 198)); // INK_DIM
        assert!(theme.show_borders);
        assert_eq!(theme.dialog_bg, rgb(brand::BG_DEEP)); // sidebar_bg
        assert_eq!(theme.text_primary, Color::Rgb(237, 255, 254)); // INK
        assert_eq!(theme.accent_fg, rgb(brand::BG_DEEP));
        assert_eq!(theme.muted_text, Color::Rgb(154, 168, 167)); // INK_FAINT
        assert_eq!(theme.warning, Color::Yellow);
        assert_eq!(theme.error, Color::Red);
        assert_eq!(theme.success, rgb(brand::ESSENCE));
        assert_eq!(theme.field_bg, Color::Rgb(2, 46, 43)); // #022e2b
        assert_eq!(theme.field_bg_focused, rgb(brand::BG_RAISE));
        assert_eq!(theme.status_running, rgb(brand::ESSENCE));
        assert_eq!(theme.status_ok, Color::Rgb(66, 165, 245));
        assert_eq!(theme.status_fail, Color::Rgb(229, 57, 53));
        assert_eq!(theme.status_disabled, Color::Rgb(120, 120, 120));
        assert_eq!(theme.status_interrupted, Color::Rgb(255, 179, 0));
        assert_eq!(theme.kind_router, Color::Rgb(171, 71, 188));
        assert_eq!(theme.info_bg, rgb(brand::TINT_INFO));
        assert_eq!(theme.ok_bg, rgb(brand::TINT_OK));
        assert_eq!(theme.warn_bg, rgb(brand::TINT_WARN));
        assert_eq!(theme.error_bg, rgb(brand::TINT_ERROR));
        assert_eq!(theme.stage_bg, rgb(brand::STAGE));
    }

    #[test]
    fn dialog_bg_matches_sidebar_bg() {
        let classic = Theme::classic();
        assert_eq!(classic.dialog_bg, classic.sidebar_bg);
        let modern = Theme::modern();
        assert_eq!(modern.dialog_bg, modern.sidebar_bg);
    }

    #[test]
    fn dialog_bg_contrasts_from_selected_and_focused() {
        for theme in [Theme::classic(), Theme::modern()] {
            assert_eq!(theme.dialog_bg, theme.sidebar_bg);
            assert_ne!(
                theme.dialog_bg, theme.selected_bg,
                "selected rows must stand out on the dialog background"
            );
            assert_ne!(
                theme.dialog_bg, theme.field_bg_focused,
                "focused fields must stand out on the dialog background"
            );
        }
    }

    #[test]
    fn modern_is_borderless() {
        assert!(!Theme::modern().show_borders);
    }

    #[test]
    fn modern_reproduces_spec_values() {
        let theme = Theme::modern();
        assert_eq!(theme.panel_bg, Color::Rgb(30, 30, 42));
        assert_eq!(theme.border_color, Color::Rgb(42, 42, 60));
        assert_eq!(theme.sidebar_bg, Color::Rgb(14, 14, 22));
        assert_eq!(theme.selected_bg, Color::Rgb(44, 44, 62));
        assert_eq!(theme.header_color, rgb(brand::ESSENCE));
        assert_eq!(theme.dim_text, Color::Rgb(140, 140, 155));
        assert_eq!(theme.dialog_bg, Color::Rgb(14, 14, 22)); // == sidebar_bg
        assert_eq!(theme.muted_text, Color::Rgb(130, 130, 145));
        assert_eq!(theme.field_bg, Color::Rgb(38, 38, 50));
        assert_eq!(theme.field_bg_focused, Color::Rgb(48, 48, 64));
        assert_eq!(theme.info_bg, Color::Rgb(16, 20, 34));
        assert_eq!(theme.ok_bg, Color::Rgb(14, 28, 24));
        assert_eq!(theme.warn_bg, Color::Rgb(30, 18, 28));
        assert_eq!(theme.error_bg, Color::Rgb(30, 16, 20));
        assert_eq!(theme.stage_bg, Color::Rgb(10, 10, 16));
    }

    fn linear(channel: u8) -> f64 {
        let value = channel as f64 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn rel_luminance(color: Color) -> f64 {
        match color {
            Color::Rgb(red, green, blue) => {
                0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
            }
            Color::White => rel_luminance(Color::Rgb(255, 255, 255)),
            Color::Black => 0.0,
            other => panic!("unsupported test color: {other:?}"),
        }
    }

    fn contrast_ratio(foreground: Color, background: Color) -> f64 {
        let foreground_luminance = rel_luminance(foreground);
        let background_luminance = rel_luminance(background);
        (foreground_luminance.max(background_luminance) + 0.05)
            / (foreground_luminance.min(background_luminance) + 0.05)
    }

    #[test]
    fn sidebar_text_contrast() {
        // The sidebar paints `sidebar_bg` behind its body, cards and the
        // Live / Automation / Knowledge tab strip, so every sidebar text
        // token must clear its bar on that surface in every theme.
        for theme in [Theme::classic(), Theme::modern()] {
            for (foreground, threshold) in [
                (theme.text_primary, 4.5),
                (theme.dim_text, 4.5),
                (theme.muted_text, 4.5),
                (theme.header_color, 3.0),
            ] {
                assert!(
                    contrast_ratio(foreground, theme.sidebar_bg) >= threshold,
                    "{foreground:?} fails {threshold}:1 contrast against sidebar"
                );
            }
            assert!(contrast_ratio(theme.text_primary, theme.panel_bg) >= 4.5);
            assert!(contrast_ratio(theme.text_primary, theme.field_bg) >= 4.5);
        }
    }

    #[test]
    fn classic_text_contrast_on_surfaces() {
        let theme = Theme::classic();
        assert!(
            contrast_ratio(theme.text_primary, theme.panel_bg) >= 4.5,
            "INK on BG must clear 4.5:1"
        );
        assert!(
            contrast_ratio(theme.text_primary, theme.dialog_bg) >= 4.5,
            "INK on dialog_bg must clear 4.5:1"
        );
    }

    #[test]
    fn text_stays_readable_on_the_new_tinted_grounds() {
        // The builder draws body text, dimmed hints/previews and muted
        // secondary text directly on the new grounds, so all three must
        // clear 4.5:1 (same bar as `sidebar_text_contrast`).
        for theme in [Theme::classic(), Theme::modern()] {
            for background in [theme.info_bg, theme.stage_bg] {
                for (name, foreground) in [
                    ("text_primary", theme.text_primary),
                    ("dim_text", theme.dim_text),
                    ("muted_text", theme.muted_text),
                ] {
                    assert!(
                        contrast_ratio(foreground, background) >= 4.5,
                        "{name} {foreground:?} on {background:?} must clear 4.5:1",
                    );
                }
            }
        }
    }

    #[test]
    fn status_palette_stays_legible_on_the_graph_canvas() {
        for theme in [Theme::classic(), Theme::modern()] {
            for (name, foreground) in [
                ("running", theme.status_running),
                ("ok", theme.status_ok),
                ("fail", theme.status_fail),
                ("disabled", theme.status_disabled),
                ("interrupted", theme.status_interrupted),
                ("kind_router", theme.kind_router),
            ] {
                assert!(
                    contrast_ratio(foreground, theme.info_bg) >= 3.0,
                    "{name} {foreground:?} on info_bg {:?} must clear 3:1",
                    theme.info_bg
                );
            }
        }
    }

    #[test]
    fn modern_sidebar_panel_step_is_perceptible() {
        let theme = Theme::modern();
        let sidebar_luminance = rel_luminance(theme.sidebar_bg);
        let panel_luminance = rel_luminance(theme.panel_bg);
        assert_ne!(sidebar_luminance, panel_luminance);
        assert!(
            contrast_ratio(theme.sidebar_bg, theme.panel_bg) >= 1.12
                || (panel_luminance - sidebar_luminance).abs() >= 0.007
        );
    }

    /// The hub/prompt tab bar's active tab is the pill pair (`accent_fg` on
    /// `header_color`) in every theme. It must never regress onto a dark
    /// raise (`selected_bg` is only ~1.2:1 against `accent_fg` in modern)
    /// and `accent_fg` must stay the dark ink it is meant to be.
    #[test]
    fn active_tab_contrast() {
        for theme in [Theme::classic(), Theme::modern()] {
            assert!(
                contrast_ratio(theme.accent_fg, theme.header_color) >= 4.5,
                "active-tab pill {:?} on {:?} must clear 4.5:1",
                theme.accent_fg,
                theme.header_color
            );
        }
    }

    /// Every colour role `Theme` declares must have a distinct value between
    /// `classic` and `modern` — otherwise a role was added and never given
    /// its own modern treatment, silently falling back to the classic look
    /// (the exact defect this spec fixes, reintroduced one field at a time).
    /// Semantic status colors (and router tag) are intentionally identical
    /// across themes — they encode pass/fail/running, not chrome — so they
    /// are excluded from this distinctness check (see CT2).
    #[test]
    fn classic_and_modern_differ_in_every_color_field() {
        let classic = Theme::classic();
        let modern = Theme::modern();
        assert_ne!(classic.border_color, modern.border_color);
        assert_ne!(classic.panel_bg, modern.panel_bg);
        assert_ne!(classic.sidebar_bg, modern.sidebar_bg);
        assert_ne!(classic.selected_bg, modern.selected_bg);
        assert_ne!(classic.dim_text, modern.dim_text);
        assert_ne!(classic.dialog_bg, modern.dialog_bg);
        assert_ne!(classic.text_primary, modern.text_primary);
        assert_ne!(classic.accent_fg, modern.accent_fg);
        assert_ne!(classic.muted_text, modern.muted_text);
        assert_ne!(classic.warning, modern.warning);
        assert_ne!(classic.error, modern.error);
        assert_ne!(classic.field_bg, modern.field_bg);
        assert_ne!(classic.field_bg_focused, modern.field_bg_focused);
        assert_ne!(classic.info_bg, modern.info_bg);
        assert_ne!(classic.ok_bg, modern.ok_bg);
        assert_ne!(classic.warn_bg, modern.warn_bg);
        assert_ne!(classic.error_bg, modern.error_bg);
        assert_ne!(classic.stage_bg, modern.stage_bg);
    }

    #[test]
    fn classic_has_status_colors() {
        let theme = Theme::classic();
        assert_eq!(theme.status_running, rgb(brand::ESSENCE));
        assert_eq!(theme.status_ok, Color::Rgb(66, 165, 245));
        assert_eq!(theme.status_fail, Color::Rgb(229, 57, 53));
        assert_eq!(theme.status_disabled, Color::Rgb(120, 120, 120));
        assert_eq!(theme.status_interrupted, Color::Rgb(255, 179, 0));
        assert_eq!(theme.kind_router, Color::Rgb(171, 71, 188));
        // Modern intentionally preserves the same semantic values.
        let modern = Theme::modern();
        assert_eq!(modern.status_running, rgb(brand::ESSENCE));
        assert_eq!(modern.status_ok, Color::Rgb(66, 165, 245));
        assert_eq!(modern.status_fail, Color::Rgb(229, 57, 53));
        assert_eq!(modern.status_disabled, Color::Rgb(120, 120, 120));
        assert_eq!(modern.status_interrupted, Color::Rgb(255, 179, 0));
        assert_eq!(modern.kind_router, Color::Rgb(171, 71, 188));
    }

    #[test]
    fn resolve_classic_returns_classic() {
        assert_eq!(Theme::resolve("classic"), Theme::classic());
    }

    #[test]
    fn resolve_modern_returns_modern() {
        assert_eq!(Theme::resolve("modern"), Theme::modern());
    }

    #[test]
    fn resolve_unknown_value_falls_back_to_classic_without_panicking() {
        assert_eq!(Theme::resolve("bogus"), Theme::classic());
        assert_eq!(Theme::resolve(""), Theme::classic());
    }

    #[test]
    fn borders_for_classic_draws_all_sides() {
        use ratatui::widgets::Borders;
        let borders = crate::tui::ui::borders_for(&Theme::classic());
        assert_eq!(borders, Borders::ALL);
        assert!(!borders.is_empty());
    }

    #[test]
    fn borders_for_modern_draws_no_sides() {
        use ratatui::widgets::Borders;
        let borders = crate::tui::ui::borders_for(&Theme::modern());
        assert_eq!(borders, Borders::NONE);
        assert!(borders.is_empty());
    }

    #[test]
    fn test_backend_classic_draws_box_glyphs_modern_does_not() {
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Block, Borders};
        use ratatui::Terminal;

        let render = |borders: Borders| -> String {
            let backend = TestBackend::new(10, 5);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    let block = Block::default()
                        .borders(borders)
                        .border_style(ratatui::style::Style::default().fg(Color::White));
                    frame.render_widget(block, frame.area());
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            let mut text = String::new();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    text.push_str(buffer[(x, y)].symbol());
                }
                text.push('\n');
            }
            text
        };

        let classic = render(Borders::ALL);
        let modern = render(Borders::NONE);

        // Borders::ALL paints corners on the top-left of a 10-wide frame.
        assert!(
            classic.starts_with('┌'),
            "Borders::ALL should paint a top-left corner glyph\n{classic}"
        );
        // Borders::NONE paints no glyphs at all in the frame interior.
        for line in modern.lines() {
            assert!(
                !line.contains('─')
                    && !line.contains('│')
                    && !line.contains('┌')
                    && !line.contains('┐')
                    && !line.contains('└')
                    && !line.contains('┘'),
                "Borders::NONE must not contain any box-drawing glyphs\nline: {line:?}\nfull:\n{modern}"
            );
        }
    }

    /// The prioritised C4 surfaces: every colour they draw must be resolved
    /// through `&Theme`, never re-derived from a hardcoded literal. A
    /// deliberately-kept exception (single-use, not a recurring role —
    /// see decision #2 in the C4 spec) must carry a `THEME-EXEMPT` comment
    /// on its own line or the line above, so this scan can tell "still a
    /// bug" apart from "documented and intentional."
    const SCANNED_SURFACES: &[&str] = &[
        "src/tui/ui/header.rs",
        "src/tui/ui/sidebar.rs",
        "src/tui/app/dialog/prompt.rs",
        "src/tui/ui/dialogs/simple_prompt.rs",
        "src/tui/ui/dialogs/mod.rs",
    ];

    /// Production code only — `#[cfg(test)]` fixtures routinely pass an
    /// arbitrary `Color` literal into a generic helper (e.g. `Color::Black`
    /// as an accent in a unit test) and that is not the defect this spec
    /// fixes. Every file here puts its whole test module after a single
    /// `#[cfg(test)]` line, so truncating there is exact, not a heuristic
    /// that could silently swallow production code.
    fn strip_test_module(source: &str) -> &str {
        match source.find("#[cfg(test)]") {
            Some(idx) => &source[..idx],
            None => source,
        }
    }

    #[test]
    fn prioritised_surfaces_have_no_undocumented_color_literals() {
        let literal = regex::Regex::new(
            r"Color::(Rgb|Indexed|Yellow|Cyan|Green|Red|Blue|Magenta|White|Black|Gray|DarkGray|LightRed|LightGreen|LightYellow|LightBlue|LightMagenta|LightCyan)",
        )
        .unwrap();

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let mut violations = Vec::new();

        for rel_path in SCANNED_SURFACES {
            let full_path = std::path::Path::new(manifest_dir).join(rel_path);
            let source = std::fs::read_to_string(&full_path)
                .unwrap_or_else(|e| panic!("failed to read {rel_path}: {e}"));
            let production = strip_test_module(&source);

            let lines: Vec<&str> = production.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if !literal.is_match(line) {
                    continue;
                }
                let exempt_here = line.contains("THEME-EXEMPT");
                let exempt_above = i > 0 && lines[i - 1].contains("THEME-EXEMPT");
                if !exempt_here && !exempt_above {
                    violations.push(format!("{rel_path}:{} — {}", i + 1, line.trim()));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "undocumented hardcoded colour literal(s) in prioritised C4 surfaces \
             (route through &Theme, or mark a genuine single-use exception with \
             a THEME-EXEMPT comment):\n{}",
            violations.join("\n")
        );
    }
}
