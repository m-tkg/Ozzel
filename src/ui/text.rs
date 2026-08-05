//! Grapheme-cluster-safe, display-width-aware string helpers shared across
//! `ui/*`: truncation, padding, and line-wrapping. Every function here
//! breaks only on `unicode_segmentation` grapheme boundaries and measures
//! with `unicode_width`, so a wide Japanese/emoji glyph is never split mid-
//! character and column math (padding, truncation budgets) is always in
//! *display* columns, not bytes or `char`s.
//!
//! Previously these were five near-duplicate implementations scattered
//! across `pane_view`, `settings_view`, `log_view`, and `viewer_view`; this
//! module is the merge point. `pad_label_min1` and `take_display_prefix`
//! are kept as distinct functions rather than forced into one shape each —
//! see their own doc comments for the behavioral differences that make full
//! unification wrong. `wrap_to_width` — originally here too — now lives in
//! `crate::logwrap` instead: its only caller is the log-line wrapping this
//! module must not depend on (see `logwrap`'s own doc comment for why).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Truncates `s` from the right (keeping the start) so its display width
/// fits within `max_width`, breaking only on grapheme-cluster boundaries and
/// appending an ellipsis when anything was cut. Used for the pane's name
/// column.
pub(crate) fn truncate_right(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width.saturating_sub(1); // reserve 1 col for the ellipsis
    let mut result = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = UnicodeWidthStr::width(g);
        if used + w > budget {
            break;
        }
        result.push_str(g);
        used += w;
    }
    result.push('…');
    result
}

/// Truncates `s` from the left (keeping the end) so its display width fits
/// within `max_width`; used for the pane header so a deeply nested path
/// still shows the directory you're actually in.
pub(crate) fn truncate_left(s: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let budget = max_width.saturating_sub(1);
    let graphemes: Vec<&str> = s.graphemes(true).collect();
    let mut used = 0;
    let mut start_idx = graphemes.len();
    for (i, g) in graphemes.iter().enumerate().rev() {
        let w = UnicodeWidthStr::width(*g);
        if used + w > budget {
            break;
        }
        used += w;
        start_idx = i;
    }
    format!("…{}", graphemes[start_idx..].concat())
}

/// Right-pads `s` with spaces until it's exactly `width` display columns
/// wide (no-op, not a truncation, when `s` is already `>= width`).
pub(crate) fn pad_right(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

/// Left-pads `s` with spaces until it's exactly `width` display columns
/// wide (no-op, not a truncation, when `s` is already `>= width`).
pub(crate) fn pad_left(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - w))
    }
}

/// Right-pads `label` to (at least) `width` display columns — but unlike
/// [`pad_right`], **never truncates** and **always** appends at least one
/// space, even when `label` is already `>= width` columns wide. This is
/// `settings_view`'s column-separator behavior (a label column followed
/// immediately by a value, needing only "some" separation, never a hard
/// column edge) and is a deliberate, different contract from `pad_right`'s
/// exact-width guarantee — not a bug to unify away.
pub(crate) fn pad_label_min1(label: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(label);
    let mut out = label.to_string();
    out.push_str(&" ".repeat(width.saturating_sub(current).max(1)));
    out
}

/// Splits `s` into `(first `width`-columns-worth, remainder)` on grapheme
/// boundaries, never exceeding `width` display columns in the first part.
/// Unlike `crate::logwrap`'s `wrap_to_width`, this never force-places an
/// over-width single grapheme into the first part (if the very first
/// grapheme alone already exceeds `width`, the first part comes back empty)
/// — fine for its one caller (`pane_view::wrap_header_lines`, which only
/// ever takes at most two slices off the front of a string), but not a
/// general-purpose wrapping loop; see that function for that.
pub(crate) fn take_display_prefix(s: &str, width: usize) -> (String, String) {
    let graphemes: Vec<&str> = s.graphemes(true).collect();
    let mut line = String::new();
    let mut used = 0;
    let mut i = 0;
    while i < graphemes.len() {
        let g = graphemes[i];
        let w = UnicodeWidthStr::width(g);
        if used + w > width {
            break;
        }
        line.push_str(g);
        used += w;
        i += 1;
    }
    (line, graphemes[i..].concat())
}

/// Returns the substring of `line` covering display columns
/// `[start_col, start_col + width)`, breaking only on grapheme-cluster
/// boundaries. A grapheme whose *start* column falls inside the window is
/// included in full; one that starts before it is dropped in full — cheap
/// and correct for the common case, at the cost of not clipping a wide
/// grapheme that straddles the window edge mid-glyph.
pub(crate) fn slice_display_cols(line: &str, start_col: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // Fast path: a pure-ASCII line (the common case — source code, logs,
    // paths) has exactly one display column per byte, so the visible slice
    // is a plain byte-range copy rather than a per-grapheme walk from
    // column 0. Without this, opening a huge single-line minified-JS-style
    // file and scrolling it horizontally re-walked the *entire* line from
    // the start on every rendered row, every frame, just to find where
    // `start_col` began. `.max(1)` on the grapheme path below means even an
    // ASCII control character counts as width 1, matching this exactly.
    if line.is_ascii() {
        let len = line.len();
        let start = start_col.min(len);
        let end = start_col.saturating_add(width).min(len);
        return line[start..end].to_string();
    }
    let end_col = start_col.saturating_add(width);
    let mut result = String::new();
    let mut col = 0usize;
    for g in line.graphemes(true) {
        if col >= end_col {
            break;
        }
        let w = UnicodeWidthStr::width(g).max(1);
        if col >= start_col {
            result.push_str(g);
        }
        col += w;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_right_keeps_ascii_untouched_when_it_fits() {
        assert_eq!(truncate_right("short.txt", 20), "short.txt");
    }

    #[test]
    fn truncate_right_never_splits_a_japanese_grapheme() {
        let name = "日本語ファイル名.txt";
        let truncated = truncate_right(name, 7);
        assert!(truncated.ends_with('…'));
        assert!(UnicodeWidthStr::width(truncated.as_str()) <= 7);
        let source_graphemes: Vec<&str> = name.graphemes(true).collect();
        for g in truncated.trim_end_matches('…').graphemes(true) {
            assert!(source_graphemes.contains(&g));
        }
    }

    #[test]
    fn truncate_left_keeps_the_tail_of_a_long_path() {
        let path = "/very/deeply/nested/日本語ディレクトリ/leaf";
        let truncated = truncate_left(path, 15);
        assert!(truncated.starts_with('…'));
        assert!(truncated.ends_with("leaf"));
        assert!(UnicodeWidthStr::width(truncated.as_str()) <= 15);
    }

    #[test]
    fn pad_right_accounts_for_wide_characters() {
        let padded = pad_right("日本語", 10);
        assert_eq!(UnicodeWidthStr::width(padded.as_str()), 10);
    }

    #[test]
    fn pad_right_is_noop_when_already_wide_enough() {
        assert_eq!(pad_right("abcdef", 3), "abcdef");
    }

    #[test]
    fn pad_left_pads_on_the_left() {
        assert_eq!(pad_left("42", 5), "   42");
    }

    #[test]
    fn pad_label_min1_always_adds_at_least_one_space() {
        // The behavioral difference from `pad_right`: even when the label
        // already meets/exceeds `width`, at least one trailing space is
        // still appended (never a hard truncation-free exact-width match).
        assert_eq!(pad_label_min1("abcdef", 3), "abcdef ");
        assert_eq!(pad_label_min1("ab", 6), "ab    ");
    }

    #[test]
    fn take_display_prefix_splits_on_grapheme_boundaries() {
        let (first, rest) = take_display_prefix("abcdef", 3);
        assert_eq!(first, "abc");
        assert_eq!(rest, "def");
    }

    #[test]
    fn take_display_prefix_leaves_first_part_empty_when_first_grapheme_overflows() {
        // Unlike `wrap_to_width`, an over-width leading grapheme is not
        // force-placed — the first part comes back empty.
        let (first, rest) = take_display_prefix("日本語", 1);
        assert_eq!(first, "");
        assert_eq!(rest, "日本語");
    }

    #[test]
    fn slice_display_cols_returns_whole_line_when_it_fits() {
        assert_eq!(slice_display_cols("hello", 0, 80), "hello");
    }

    #[test]
    fn slice_display_cols_clips_to_width() {
        assert_eq!(slice_display_cols("hello world", 0, 5), "hello");
    }

    #[test]
    fn slice_display_cols_applies_horizontal_offset() {
        assert_eq!(slice_display_cols("hello world", 6, 5), "world");
    }

    #[test]
    fn slice_display_cols_respects_wide_japanese_graphemes() {
        assert_eq!(slice_display_cols("日本語", 0, 2), "日");
        assert_eq!(slice_display_cols("日本語", 2, 2), "本");
        assert_eq!(slice_display_cols("日本語", 4, 2), "語");
    }

    #[test]
    fn slice_display_cols_offset_past_end_is_empty() {
        assert_eq!(slice_display_cols("short", 100, 20), "");
    }
}
