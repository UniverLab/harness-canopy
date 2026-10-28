//! Render text as a terminal QR code using Unicode half blocks.

use anyhow::Result;
use qrcode::{Color, QrCode};

/// Modules of quiet zone around the symbol.
const QUIET: usize = 2;

/// Render `text` as a QR code with `▀`, `▄` and `█` (two module rows per
/// terminal line).
pub fn render(text: &str) -> Result<String> {
    let code = QrCode::new(text.as_bytes())?;
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
}
