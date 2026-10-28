//! Render text as a terminal QR code using Unicode half blocks.

use anyhow::Result;
use qrcode::{Color, QrCode};

/// Modules of quiet zone around the symbol.
const QUIET: usize = 2;

/// Render `text` as a QR code with `▀`, `▄` and `█` (two module rows per
/// terminal line), at medium error correction.
pub fn render(text: &str) -> Result<String> {
    render_level(text, qrcode::EcLevel::M)
}

/// Render `text` as a QR code at one error-correction level. The Remote
/// panel uses `L`: for a pairing link that is 33×17 terminal cells, while
/// `M` is 37×19 and no longer fits the 80×24 overlay.
pub fn render_level(text: &str, level: qrcode::EcLevel) -> Result<String> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), level)?;
    let width = code.width();
    let side = width + QUIET * 2;
    let mut grid = vec![vec![false; side]; side];
    for (y, row) in code.to_colors().chunks(width).enumerate() {
        for (x, color) in row.iter().enumerate() {
            grid[y + QUIET][x + QUIET] = *color == Color::Dark;
        }
    }
    let mut out = String::new();
    let mut y = 0;
    while y < side {
        for (x, top) in grid[y].iter().enumerate() {
            let bottom = grid
                .get(y + 1)
                .and_then(|row| row.get(x))
                .copied()
                .unwrap_or(false);
            out.push(match (*top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
        y += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_square_symbol_with_half_blocks_and_quiet_zone() {
        let rendered = render("https://remote.example/pair#7K3QH-M9P2X").expect("qr");
        let lines: Vec<&str> = rendered.lines().collect();
        assert!(!lines.is_empty());
        assert!(lines
            .iter()
            .all(|line| line.chars().count() == lines[0].chars().count()));
        assert!(lines
            .iter()
            .any(|line| line.contains('▀') || line.contains('▄')));
        // The quiet zone: first and last lines are blank.
        assert!(lines[0].trim().is_empty());
        assert!(lines.last().expect("last line").trim().is_empty());
    }

    #[test]
    fn level_l_fits_the_pairing_overlay_while_m_does_not() {
        let link = "https://remote.univerlab.org/pair#7K3QH-M9P2X";
        let low = render_level(link, qrcode::EcLevel::L).expect("level L");
        let low_lines: Vec<&str> = low.lines().collect();
        assert_eq!(low_lines.len(), 17, "17 rows: {low}");
        assert!(
            low_lines.iter().all(|line| line.chars().count() == 33),
            "33 wide: {low}"
        );

        let medium = render(link).expect("level M");
        let medium_lines: Vec<&str> = medium.lines().collect();
        assert_eq!(medium_lines.len(), 19, "M is one version up: {medium}");
        assert!(
            medium_lines.iter().all(|line| line.chars().count() == 37),
            "37 wide: {medium}"
        );
    }
}
