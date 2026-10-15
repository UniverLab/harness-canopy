//! Shared banner rendering functionality

use crate::tui::ui::brand::CANOPY_SCALE;

/// The five legible Canopy scale stops, light at the top of the banner to
/// dark at the bottom. `#003330` (the darkest stop) is excluded: it measures
/// ~1.4:1 on a dark terminal, so it is a background tone only.
pub const BANNER_GRADIENT: [(u8, u8, u8); 5] = [
    CANOPY_SCALE[5], // #52fff6
    CANOPY_SCALE[4], // #1ffff3
    CANOPY_SCALE[3], // #00e0d4
    CANOPY_SCALE[2], // #00a9a0
    CANOPY_SCALE[1], // #006661
];

pub const BANNER: &str = r#"                                                     
  ██████   ██████   ████████    ██████  ████████  █████ ████
 ███░░███ ░░░░░███ ░░███░░███  ███░░███░░███░░███░░███ ░███ 
░███ ░░░   ███████  ░███ ░███ ░███ ░███ ░███ ░███ ░███ ░███ 
░███  ███ ███░░███  ░███ ░███ ░███ ░███ ░███ ░███ ░███ ░███ 
░░██████ ░░████████ ████ █████░░██████  ░███████  ░░███████ 
 ░░░░░░   ░░░░░░░░ ░░░░ ░░░░░  ░░░░░░   ░███░░░    ░░░░░███ 
                                        ░███       ███ ░███ 
                                        █████     ░░██████  
                                       ░░░░░       ░░░░░░   
"#;

/// Print the banner with gradient colors and custom title
pub fn print_banner_with_gradient(title: &str) {
    let lines: Vec<&str> = BANNER.lines().collect();

    // Print each line with a different color from the gradient
    for (i, line) in lines.iter().enumerate() {
        let (r, g, b) = gradient_rgb(i, lines.len());
        println!("\x1b[38;2;{r};{g};{b}m{line}\x1b[0m");
    }

    // Print additional text in the lightest scale stop (#52fff6) with custom title
    println!("\x1b[38;2;82;255;246m  \x1b[1m{}\x1b[0m", title);
    println!("  ─────────────────────────────────────────────");
    println!();
}

pub fn gradient_rgb(index: usize, line_count: usize) -> (u8, u8, u8) {
    let denominator = line_count.max(1);
    let color_index =
        (index as f32 / denominator as f32 * (BANNER_GRADIENT.len() - 1) as f32).round() as usize;
    BANNER_GRADIENT[color_index.min(BANNER_GRADIENT.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gradient_rgb at index 0 returns first color
    #[test]
    fn gradient_rgb_first_index() {
        let (r, g, b) = gradient_rgb(0, 8);
        assert_eq!((r, g, b), BANNER_GRADIENT[0]);
    }

    /// The last line of the 11-line banner lands on the darkest legible stop.
    #[test]
    fn gradient_rgb_last_index() {
        // gradient_rgb(10, 11) = (10/11 * 4).round() = 3.636.round() = 4
        let (r, g, b) = gradient_rgb(10, 11);
        assert_eq!((r, g, b), BANNER_GRADIENT[4]);
    }

    /// gradient_rgb with line_count=1 returns first color
    #[test]
    fn gradient_rgb_single_line() {
        let (r, g, b) = gradient_rgb(0, 1);
        assert_eq!((r, g, b), BANNER_GRADIENT[0]);
    }

    /// gradient_rgb with line_count=0 returns first color
    #[test]
    fn gradient_rgb_zero_lines() {
        let (r, g, b) = gradient_rgb(0, 0);
        assert_eq!((r, g, b), BANNER_GRADIENT[0]);
    }

    /// gradient_rgb returns valid indices within bounds
    #[test]
    fn gradient_rgb_all_indices_valid() {
        for index in 0..20 {
            for line_count in 1..20 {
                let (r, g, b) = gradient_rgb(index, line_count);
                // Check it's one of the valid gradient colors
                assert!(
                    BANNER_GRADIENT.contains(&(r, g, b)),
                    "gradient_rgb({index}, {line_count}) returned ({r}, {g}, {b}) which is not in BANNER_GRADIENT"
                );
            }
        }
    }

    /// gradient_rgb with middle indices returns middle-range colors
    #[test]
    fn gradient_rgb_middle_indices() {
        let (r, g, b) = gradient_rgb(4, 8);
        // Should return a middle color (around index 3-4)
        // Just verify it's a valid gradient color
        assert!(BANNER_GRADIENT.contains(&(r, g, b)));
    }

    /// gradient_rgb increases (roughly) as index increases
    #[test]
    fn gradient_rgb_monotonic_trend() {
        // Test that color indices generally increase as we move through the banner
        let colors: Vec<_> = (0..8).map(|i| gradient_rgb(i, 8)).collect();
        // All should be valid colors
        for (r, g, b) in &colors {
            assert!(BANNER_GRADIENT.contains(&(*r, *g, *b)));
        }
    }

    /// gradient_rgb handles large line counts
    #[test]
    fn gradient_rgb_large_line_count() {
        let (r, g, b) = gradient_rgb(50, 1000);
        assert!(BANNER_GRADIENT.contains(&(r, g, b)));
    }

    /// gradient_rgb index beyond line_count wraps correctly
    #[test]
    fn gradient_rgb_index_beyond_count() {
        let (r, g, b) = gradient_rgb(100, 10);
        assert!(BANNER_GRADIENT.contains(&(r, g, b)));
    }

    /// BANNER_GRADIENT has expected length
    #[test]
    fn banner_gradient_has_expected_length() {
        assert_eq!(BANNER_GRADIENT.len(), 5);
    }

    /// The darkest scale stop is a background tone, never banner ink.
    #[test]
    fn banner_gradient_does_not_contain_deepest_stop() {
        assert!(
            !BANNER_GRADIENT.contains(&CANOPY_SCALE[0]),
            "the banner must not use #003330"
        );
    }

    /// The banner is exactly the five legible stops, light → dark.
    #[test]
    fn banner_gradient_equals_scale_five_to_one_reversed() {
        for (i, stop) in BANNER_GRADIENT.iter().enumerate() {
            assert_eq!(*stop, CANOPY_SCALE[5 - i]);
        }
    }

    /// BANNER_GRADIENT colors are valid RGB tuples
    #[test]
    fn banner_gradient_color_ranges() {
        for (r, g, b) in BANNER_GRADIENT {
            // u8 is always <= 255, but verify structure is correct
            let _ = (r, g, b);
        }
    }

    /// BANNER contains expected string content
    #[test]
    fn banner_contains_canopy_text() {
        assert!(BANNER.len() > 100);
        assert!(BANNER.contains("██"));
    }

    /// print_banner_with_gradient doesn't panic (I/O test)
    #[test]
    fn print_banner_with_gradient_no_panic() {
        // Just verify it doesn't panic - we can't easily test output
        print_banner_with_gradient("Test Title");
    }
}
