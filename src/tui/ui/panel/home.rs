use crate::shared::banner::BANNER_GRADIENT;
use crate::tui::app::types::App;
use crate::tui::brians_brain::{BriansBrain, CellState};
use crate::tui::ui::brand::{rgb, ESSENCE};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Frame;

/// Fire colour blend over the area background — the terminal-legible
/// equivalent of the web's `0.26` canvas alpha (see `brain.ts`).
pub(crate) const BRAIN_FIRE_MIX: f64 = 0.50;
/// Dying colour blend over the area background — the terminal-legible
/// equivalent of the web's `0.07` canvas alpha.
pub(crate) const BRAIN_DYING_MIX: f64 = 0.15;

/// Sub-cell → Braille dot bit, indexed `[row 0..3][col 0..1]`. Same table as
/// the web's `brain.ts`.
const BRAILLE_DOT: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

pub(super) fn draw_canopy_banner_animation(frame: &mut Frame, area: Rect, app: &App) {
    let banner = crate::shared::banner::BANNER.trim_matches('\n');
    let banner_lines: Vec<&str> = banner.lines().collect();
    let total = banner_lines.len() as u16;

    let top_pad = if area.height > total {
        (area.height - total) / 2
    } else {
        0
    };

    let wave_offset = (app.animation_tick as f32 * 0.02) % 1.0;

    for (i, line) in banner_lines.iter().enumerate() {
        let row_pos = if total > 1 {
            i as f32 / (total - 1) as f32
        } else {
            0.0
        };
        let (r, g, b) = sample_mirrored_gradient((row_pos + wave_offset) % 1.0);
        let accent = Color::Rgb(r, g, b);
        let accent_dim = Color::Rgb(
            r.saturating_sub(40),
            g.saturating_sub(40),
            b.saturating_sub(40),
        );

        let y = area.y + top_pad + i as u16;
        if y >= area.y + area.height {
            break;
        }

        let line_start_x = area.x + area.width / 2 - (line.chars().count() as u16 / 2);

        for (col_idx, ch) in line.chars().enumerate() {
            let x = line_start_x + col_idx as u16;
            if x >= area.x + area.width {
                break;
            }
            if x < area.x {
                continue;
            }

            let buf_cell = &mut frame.buffer_mut()[(x, y)];
            let symbol = match ch {
                '█' => "█",
                '░' => "░",
                _ => continue,
            };
            buf_cell.set_symbol(symbol);
            buf_cell.set_style(Style::default().fg(if ch == '█' { accent } else { accent_dim }));
        }
    }
}

fn sample_mirrored_gradient(phase: f32) -> (u8, u8, u8) {
    let len = BANNER_GRADIENT.len();
    if len == 0 {
        return (255, 255, 255);
    }
    if len == 1 {
        return BANNER_GRADIENT[0];
    }

    // Mirror map 0..1 -> 0..1..0, so neighbors always follow the same gradient order.
    let mirrored = if phase < 0.5 {
        phase * 2.0
    } else {
        (1.0 - phase) * 2.0
    };

    let max_idx = (len - 1) as f32;
    let scaled = mirrored * max_idx;
    let i0 = scaled.floor() as usize;
    let i1 = (i0 + 1).min(len - 1);
    let t = (scaled - i0 as f32).clamp(0.0, 1.0);

    let (r0, g0, b0) = BANNER_GRADIENT[i0];
    let (r1, g1, b1) = BANNER_GRADIENT[i1];

    let lerp = |a: u8, b: u8| -> u8 { ((a as f32) + (b as f32 - a as f32) * t).round() as u8 };
    (lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}

/// Pack the 2×4 sub-cell block whose top-left sub-cell is `(bx*2, by*4)` into
/// `(fire_mask, dying_mask)`, using the web's Braille dot table.
fn braille_block_masks(brain: &BriansBrain, by: usize, bx: usize) -> (u8, u8) {
    let mut fire = 0u8;
    let mut dying = 0u8;
    for (r, dots) in BRAILLE_DOT.iter().enumerate() {
        for (col, dot) in dots.iter().enumerate() {
            let x = bx * 2 + col;
            let y = by * 4 + r;
            match brain.grid[y * brain.gw + x] {
                CellState::On => fire |= dot,
                CellState::Dying => dying |= dot,
                CellState::Off => {}
            }
        }
    }
    (fire, dying)
}

/// Blend a brand token over the area's background at strength `t`.
fn blend(foreground: (u8, u8, u8), background: Color, t: f64) -> Color {
    let Color::Rgb(br, bg, bb) = background else {
        return rgb(foreground);
    };
    let mix = |f: u8, b: u8| (f as f64 * t + b as f64 * (1.0 - t)).round() as u8;
    Color::Rgb(
        mix(foreground.0, br),
        mix(foreground.1, bg),
        mix(foreground.2, bb),
    )
}

/// Draw the automaton: one Braille glyph per character cell, each packing a
/// 2×4 block of sub-cells. A block with any firing dot draws its firing mask in
/// the fire colour; otherwise the dying mask draws in the dying colour.
pub(crate) fn draw_brians_brain(
    frame: &mut Frame,
    area: Rect,
    brain: &BriansBrain,
    area_bg: Color,
) {
    let fire_color = blend(ESSENCE, area_bg, BRAIN_FIRE_MIX);
    let dying_color = blend(ESSENCE, area_bg, BRAIN_DYING_MIX);
    let buf = frame.buffer_mut();

    for by in 0..brain.braille_rows() {
        if by as u16 >= area.height {
            break;
        }
        for bx in 0..brain.braille_cols() {
            if bx as u16 >= area.width {
                break;
            }
            let (fire, dying) = braille_block_masks(brain, by, bx);
            if fire == 0 && dying == 0 {
                continue;
            }
            let (mask, color) = if fire != 0 {
                (fire, fire_color)
            } else {
                (dying, dying_color)
            };
            let glyph = char::from_u32(0x2800 | mask as u32)
                .expect("Braille mask is always a valid code point");
            let buf_cell = &mut buf[(area.x + bx as u16, area.y + by as u16)];
            buf_cell.set_symbol(&glyph.to_string());
            buf_cell.set_style(Style::default().fg(color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A brain with every sub-cell explicitly Off.
    fn off_brain(rows: usize, cols: usize) -> BriansBrain {
        let mut brain = BriansBrain::new(rows, cols);
        for cell in brain.grid.iter_mut() {
            *cell = CellState::Off;
        }
        brain
    }

    fn render_brain(brain: &BriansBrain, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_brians_brain(frame, area, brain, Color::Rgb(0, 0, 0));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn full_2x4_firing_block_renders_braille_ff() {
        let mut brain = off_brain(2, 2);
        let gw = brain.gw;
        for r in 0..4 {
            for col in 0..2 {
                brain.grid[r * gw + col] = CellState::On;
            }
        }
        assert_eq!(braille_block_masks(&brain, 0, 0), (0xFF, 0x00));
        let text = render_brain(&brain, 4, 4);
        assert!(
            text.contains('\u{28ff}'),
            "a full firing block must render ⣿\n{text}"
        );
    }

    #[test]
    fn firing_mask_wins_over_dying_mask() {
        let mut brain = off_brain(2, 2);
        let gw = brain.gw;
        brain.grid[0] = CellState::On; // (sub 0,0) → fire dot 0x01
        brain.grid[gw] = CellState::Dying; // (sub 0,1) → dying dot 0x02
        assert_eq!(braille_block_masks(&brain, 0, 0), (0x01, 0x02));
        let text = render_brain(&brain, 4, 4);
        assert!(
            text.contains('\u{2801}'),
            "any firing dot draws the firing mask\n{text}"
        );
        assert!(
            !text.contains('\u{2803}'),
            "the dying mask must not be unioned into the firing glyph\n{text}"
        );
    }

    #[test]
    fn dying_only_block_renders_dying_mask() {
        let mut brain = off_brain(2, 2);
        let gw = brain.gw;
        brain.grid[gw] = CellState::Dying; // (sub 0,1) → dying dot 0x02
        assert_eq!(braille_block_masks(&brain, 0, 0), (0x00, 0x02));
        let text = render_brain(&brain, 4, 4);
        assert!(
            text.contains('\u{2802}'),
            "a block without firing dots draws the dying mask\n{text}"
        );
    }

    #[test]
    fn empty_block_renders_nothing() {
        let brain = off_brain(2, 2);
        assert_eq!(braille_block_masks(&brain, 0, 0), (0, 0));
        let text = render_brain(&brain, 4, 4);
        assert!(
            !text.contains('\u{2800}') && !text.contains('\u{28ff}'),
            "an empty block must draw no glyph\n{text}"
        );
        assert!(text.trim().is_empty(), "nothing should be drawn");
    }

    #[test]
    fn fire_color_uses_fire_mix() {
        assert_eq!(
            blend(ESSENCE, Color::Rgb(0, 0, 0), BRAIN_FIRE_MIX),
            Color::Rgb(0, 85, 80)
        );
    }

    #[test]
    fn dying_color_uses_dying_mix() {
        assert_eq!(
            blend(ESSENCE, Color::Rgb(0, 0, 0), BRAIN_DYING_MIX),
            Color::Rgb(0, 25, 24)
        );
    }
}
