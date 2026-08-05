//! Thin wrapper around `ratatui::style::Color`'s built-in `FromStr`, which
//! already handles named colors case-insensitively (including
//! `light_green`/`lightgreen`/`bright green`-style variants, since it
//! strips separators and normalizes `bright`→`light` internally) and
//! `#RRGGBB` hex — this just gives config parsing a clearer, input-echoing
//! error message than the library's generic "Failed to parse Colors".

use std::str::FromStr;

use ratatui::style::Color;

pub fn parse_color(input: &str) -> Result<Color, String> {
    Color::from_str(input.trim()).map_err(|_| format!("invalid color: \"{input}\""))
}

/// Approximate RGB for every named `Color` variant — the standard
/// ANSI/xterm palette values, close enough for "scale this toward black"
/// purposes (see `dim_color`). A real terminal's actual displayed RGB for
/// a named color varies by theme, which can't be known at draw time
/// regardless, so this is necessarily an approximation, not a lookup of
/// the truth. `Color::Rgb` passes its exact channels through unchanged;
/// `Color::Indexed`/`Color::Reset` have no reasonable universal RGB
/// (an indexed color's meaning depends on the terminal's 256-color
/// palette, which isn't known here either) and return `None`.
fn approx_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((205, 0, 0)),
        Color::Green => Some((0, 205, 0)),
        Color::Yellow => Some((205, 205, 0)),
        Color::Blue => Some((0, 0, 238)),
        Color::Magenta => Some((205, 0, 205)),
        Color::Cyan => Some((0, 205, 205)),
        Color::Gray => Some((229, 229, 229)),
        Color::DarkGray => Some((127, 127, 127)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((92, 92, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(_) | Color::Reset => None,
    }
}

/// Scales `color` toward black by `factor` (`0.0` = pure black, `1.0` =
/// unchanged), returning an RGB color — used to make the inactive pane's
/// cursor row genuinely darker in *color*, not by relying on the
/// terminal's SGR "dim"/faint attribute (which most terminals barely, if
/// at all, apply to a *background* color — see
/// `ui::pane_view::row_style`'s cursor-row branch, which is why this
/// exists instead of just adding `Modifier::DIM` there like every other
/// dimmed row). `factor` is clamped to `0.0..=1.0` first, so an
/// out-of-range value can't produce a brightened or negative-channel
/// result. Passes `color` through completely unchanged (graceful, honest
/// degradation rather than a wrong-looking guess) for `Indexed`/`Reset`,
/// which `approx_rgb` has no RGB approximation for.
pub fn dim_color(color: Color, factor: f32) -> Color {
    let Some((r, g, b)) = approx_rgb(color) else {
        return color;
    };
    let factor = factor.clamp(0.0, 1.0);
    let scale = |channel: u8| (channel as f32 * factor).round() as u8;
    Color::Rgb(scale(r), scale(g), scale(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_colors_case_insensitively() {
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("RED").unwrap(), Color::Red);
        assert_eq!(parse_color("Red").unwrap(), Color::Red);
    }

    #[test]
    fn parses_light_green_in_both_spellings() {
        assert_eq!(parse_color("light_green").unwrap(), Color::LightGreen);
        assert_eq!(parse_color("lightgreen").unwrap(), Color::LightGreen);
        assert_eq!(parse_color("LightGreen").unwrap(), Color::LightGreen);
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(
            parse_color("#90EE90").unwrap(),
            Color::Rgb(0x90, 0xEE, 0x90)
        );
        assert_eq!(parse_color("#000000").unwrap(), Color::Rgb(0, 0, 0));
        assert_eq!(
            parse_color("#ffffff").unwrap(),
            Color::Rgb(0xff, 0xff, 0xff)
        );
    }

    #[test]
    fn rejects_invalid_input() {
        assert!(parse_color("not-a-color").is_err());
        assert!(parse_color("#zzzzzz").is_err());
        assert!(
            parse_color("#fff").is_err(),
            "must be 6 hex digits, not shorthand"
        );
        assert!(parse_color("").is_err());
    }

    // --- dim_color ---------------------------------------------------------

    #[test]
    fn dim_color_scales_rgb_channels_by_the_factor() {
        assert_eq!(
            dim_color(Color::Rgb(200, 100, 40), 0.5),
            Color::Rgb(100, 50, 20)
        );
    }

    #[test]
    fn dim_color_one_is_unchanged_rgb_and_zero_is_black() {
        assert_eq!(
            dim_color(Color::Rgb(200, 100, 40), 1.0),
            Color::Rgb(200, 100, 40)
        );
        assert_eq!(
            dim_color(Color::Rgb(200, 100, 40), 0.0),
            Color::Rgb(0, 0, 0)
        );
    }

    #[test]
    fn dim_color_clamps_an_out_of_range_factor() {
        // A factor above 1.0 must not brighten past the original color.
        assert_eq!(
            dim_color(Color::Rgb(200, 100, 40), 2.0),
            Color::Rgb(200, 100, 40)
        );
        // A negative factor must not go below black / wrap around.
        assert_eq!(
            dim_color(Color::Rgb(200, 100, 40), -1.0),
            Color::Rgb(0, 0, 0)
        );
    }

    #[test]
    fn dim_color_maps_named_colors_to_an_approximate_rgb_before_scaling() {
        // White -> (255,255,255) approximated, halved.
        assert_eq!(dim_color(Color::White, 0.5), Color::Rgb(128, 128, 128));
        // Black -> (0,0,0) approximated; halving black is still black.
        assert_eq!(dim_color(Color::Black, 0.5), Color::Rgb(0, 0, 0));
    }

    #[test]
    fn dim_color_passes_through_indexed_and_reset_unchanged() {
        // No universal RGB approximation exists for either — graceful
        // passthrough rather than a wrong-looking guess.
        assert_eq!(dim_color(Color::Indexed(42), 0.5), Color::Indexed(42));
        assert_eq!(dim_color(Color::Reset, 0.5), Color::Reset);
    }
}
