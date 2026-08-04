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
}
