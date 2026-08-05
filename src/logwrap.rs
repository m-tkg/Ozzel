//! Timestamp-prefixing and width-wrapping for log lines — split out from
//! `ui::log_view` (which still owns the actual *rendering* of the wrapped
//! rows) so that `App::log_wrapped`'s cache-rebuild call and `LogLine::new`
//! (both `app.rs`) don't need to depend on the `ui` layer just to reuse this
//! pure text logic. That upward `app -> ui` dependency, on top of the
//! pre-existing `ui -> app` one (every `ui::*_view::render` takes `&App`),
//! was a real import cycle between the two layers — this module is the cut:
//! it depends on neither `app` nor `ui` (not even `ui::text`, which is why
//! `wrap_to_width` — used only by the two functions below — moved here
//! along with them rather than staying put and being called across the
//! layer boundary), and each of those two now only ever depends *down* into
//! this one.
//!
//! [`LoggableLine`] is the seam that makes that possible: rather than this
//! module importing `app::LogLine` directly (which would just relocate the
//! same `app` dependency here instead of removing it), it depends only on
//! this small trait, and `app.rs` implements it for its own `LogLine` type —
//! keeping the dependency arrow pointing the one direction that doesn't
//! cycle back.

use std::collections::VecDeque;

use chrono::{DateTime, Local};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// The minimal view of one loggable line that [`wrap_log_lines`]/
/// [`wrap_log_lines_tail`] need. Implemented by `app::LogLine` — see this
/// module's doc comment for why the dependency runs that direction instead
/// of this module importing `LogLine` directly.
pub trait LoggableLine {
    fn message(&self) -> &str;
    fn is_error(&self) -> bool;
    /// The line's `format_timestamp_prefix(timestamp)` output, computed
    /// once at append time (see `app::LogLine::formatted_timestamp`'s own
    /// doc comment for why: re-running `chrono`'s `format` on every
    /// in-memory line every frame the log view (or its search) draws was
    /// real, measurable per-frame cost).
    fn formatted_timestamp(&self) -> &str;
}

/// `strftime`-style format for a log line's timestamp prefix: 4-digit
/// year, 2-digit month/day/hour/minute/second, fixed punctuation, always
/// exactly `TIMESTAMP_PREFIX_WIDTH` display columns wide (ASCII-only, so
/// char count and display width are the same).
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S ";
/// Display width of `TIMESTAMP_FORMAT`'s output (e.g.
/// `"2026-08-05 14:03:22 "`) — doubles as the hang-indent width for a
/// wrapped line's continuation rows, so the message column lines up under
/// the first row's.
pub const TIMESTAMP_PREFIX_WIDTH: usize = 20;

/// Formats `timestamp` as the fixed-width prefix every log row gets (see
/// `TIMESTAMP_FORMAT`). Takes the timestamp as a plain value rather than
/// calling `Local::now()` itself, so it stays a pure, directly-testable
/// function — the actual "when" is captured once, at append time, by
/// `app::LogLine::new`.
pub fn format_timestamp_prefix(timestamp: DateTime<Local>) -> String {
    timestamp.format(TIMESTAMP_FORMAT).to_string()
}

/// Hard-wraps `s` into chunks of at most `width` display columns, breaking
/// only on grapheme-cluster boundaries (never mid-character, even for wide
/// Japanese graphemes) — this is a plain width-based wrap, not word-wrap,
/// since log messages are often paths with no spaces to break on. An empty
/// string still yields one (empty) row, matching the pre-wrap one-row-per-
/// line baseline for a blank message. Unlike `ui::text::take_display_prefix`,
/// an over-width single grapheme is still force-placed alone on its own row
/// (via `.max(1)` treating every grapheme as at least 1 column) rather than
/// leaving that row empty, so this always makes forward progress across the
/// whole string — required for a loop that must consume all of `s`, not
/// just take one prefix. Originally lived alongside `take_display_prefix` in
/// `ui::text` (this module's doc comment explains why it moved).
fn wrap_to_width(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    for g in s.graphemes(true) {
        let w = UnicodeWidthStr::width(g).max(1);
        if current_width + w > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push_str(g);
        current_width += w;
    }
    rows.push(current);
    rows
}

/// Wraps every log line to `width` display columns — the *message* portion
/// wraps against `width` minus the timestamp prefix's width (see
/// `wrap_to_width` above), and the result is flattened into
/// `(display_row_text, is_error)` pairs in original order. The first
/// wrapped row of each line gets the real timestamp prefix; continuation
/// rows get a blank hang-indent of the same width instead, so the message
/// column stays aligned without repeating the timestamp. `is_error` is
/// carried onto every wrapped row of an error line, so a wrapped error
/// message stays red top to bottom. `pub` so `App::run_log_search`/
/// `log_search_step` (`app.rs`) can rewrap the log at `App::log_view_width`
/// — the same width `ui::log_view::render_full` last actually rendered at
/// — to search against exactly the rows the screen shows, `ui::log_view`
/// itself can render it, and `benches/`'s criterion bench, an external
/// target, can measure it directly.
pub fn wrap_log_lines<'a, L: LoggableLine + 'a>(
    log: impl Iterator<Item = &'a L>,
    width: usize,
) -> Vec<(String, bool)> {
    if width == 0 {
        return Vec::new();
    }
    let message_width = width.saturating_sub(TIMESTAMP_PREFIX_WIDTH);
    let hang_indent = " ".repeat(TIMESTAMP_PREFIX_WIDTH);

    log.flat_map(move |line| {
        let prefix = line.formatted_timestamp().to_string();
        let is_error = line.is_error();
        let hang_indent = hang_indent.clone();
        wrap_to_width(line.message(), message_width)
            .into_iter()
            .enumerate()
            .map(move |(i, row)| {
                let indent = if i == 0 { &prefix } else { &hang_indent };
                (format!("{indent}{row}"), is_error)
            })
    })
    .collect()
}

/// Tail-first sibling of `wrap_log_lines` for the bottom log panel, which
/// only ever shows a handful of rows: walks `log` **newest to oldest**
/// (pass `app.log.iter().rev()`) and stops as soon as `needed_rows` wrapped
/// rows have been collected, instead of wrapping (and timestamp-prefixing)
/// every one of the up-to-`LOG_CAPACITY` in-memory lines just to throw all
/// but the last few away. Produces exactly the same rows, in the same
/// order, that `wrap_log_lines(log, width)`'s last `needed_rows` entries
/// would (see the equivalence tests below) — a perf path, not a behavior
/// change. The full-frame log view (`render_full`) and its `/`/`?` search
/// still go through `wrap_log_lines` proper: they need the *entire* wrapped
/// log (to scroll/search through), so there's nothing to skip there.
pub fn wrap_log_lines_tail<'a, L: LoggableLine + 'a>(
    log_newest_first: impl Iterator<Item = &'a L>,
    width: usize,
    needed_rows: usize,
) -> Vec<(String, bool)> {
    if width == 0 || needed_rows == 0 {
        return Vec::new();
    }
    let message_width = width.saturating_sub(TIMESTAMP_PREFIX_WIDTH);
    let hang_indent = " ".repeat(TIMESTAMP_PREFIX_WIDTH);

    // Rows accumulate oldest-to-newest as lines are consumed newest-to-
    // oldest, so each line's rows are pushed to the *front* — a `VecDeque`
    // makes that O(1) instead of an `insert(0, ...)` shuffle on a `Vec`.
    let mut collected: VecDeque<(String, bool)> = VecDeque::new();
    for line in log_newest_first {
        let rows = wrap_to_width(line.message(), message_width);
        for (i, row) in rows.into_iter().enumerate().rev() {
            let indent = if i == 0 {
                line.formatted_timestamp()
            } else {
                hang_indent.as_str()
            };
            collected.push_front((format!("{indent}{row}"), line.is_error()));
        }
        if collected.len() >= needed_rows {
            break;
        }
    }

    let start = collected.len().saturating_sub(needed_rows);
    collected.into_iter().skip(start).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A minimal stand-in for `app::LogLine` so these tests don't need to
    /// depend on the `app` module (which itself depends on this one — see
    /// this module's doc comment) just to build a `LoggableLine`.
    struct TestLine {
        message: String,
        is_error: bool,
        formatted_timestamp: String,
    }

    impl LoggableLine for TestLine {
        fn message(&self) -> &str {
            &self.message
        }
        fn is_error(&self) -> bool {
            self.is_error
        }
        fn formatted_timestamp(&self) -> &str {
            &self.formatted_timestamp
        }
    }

    /// A fixed local timestamp for every test line below — the exact value
    /// doesn't matter to most of these tests (they're about wrapping and
    /// timestamp formatting, not the clock), so using one constant keeps
    /// expected strings easy to build via `format_timestamp_prefix` itself.
    fn test_timestamp() -> DateTime<Local> {
        Local.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap()
    }

    fn log_line(message: &str, is_error: bool) -> TestLine {
        TestLine {
            message: message.to_string(),
            is_error,
            formatted_timestamp: format_timestamp_prefix(test_timestamp()),
        }
    }

    #[test]
    fn format_timestamp_prefix_is_a_fixed_width_yyyy_mm_dd_hh_mm_ss() {
        let prefix = format_timestamp_prefix(test_timestamp());
        assert_eq!(prefix, "2024-01-02 03:04:05 ");
        assert_eq!(
            UnicodeWidthStr::width(prefix.as_str()),
            TIMESTAMP_PREFIX_WIDTH
        );
    }

    #[test]
    fn wrap_to_width_leaves_a_short_line_as_one_row() {
        assert_eq!(wrap_to_width("short", 80), vec!["short".to_string()]);
    }

    #[test]
    fn wrap_to_width_splits_a_long_ascii_line_at_the_width() {
        let rows = wrap_to_width("abcdefghij", 4);
        assert_eq!(rows, vec!["abcd", "efgh", "ij"]);
        for row in &rows {
            assert!(UnicodeWidthStr::width(row.as_str()) <= 4);
        }
    }

    #[test]
    fn wrap_to_width_never_splits_a_wide_japanese_grapheme() {
        let rows = wrap_to_width("日本語ファイル", 5);
        for row in &rows {
            assert!(UnicodeWidthStr::width(row.as_str()) <= 5);
        }
    }

    #[test]
    fn wrap_to_width_empty_string_is_one_empty_row() {
        assert_eq!(wrap_to_width("", 80), vec!["".to_string()]);
    }

    #[test]
    fn wrap_to_width_zero_width_is_one_empty_row_not_a_panic() {
        assert_eq!(wrap_to_width("anything", 0), vec!["".to_string()]);
    }

    #[test]
    fn wrap_log_lines_prefixes_the_first_row_with_the_timestamp() {
        let lines = [log_line("hello", false)];
        let wrapped = wrap_log_lines(lines.iter(), 40);
        assert_eq!(
            wrapped,
            vec![("2024-01-02 03:04:05 hello".to_string(), false)]
        );
    }

    #[test]
    fn wrap_log_lines_hang_indents_continuation_rows_instead_of_repeating_the_timestamp() {
        // message_width = 30 - 15 = 15; a message longer than that must
        // wrap, and only the first row may carry the real timestamp.
        let long_message = "abcdefghijklmnopqrstuvwxyz"; // 26 chars
        let lines = [log_line(long_message, false)];
        let wrapped = wrap_log_lines(lines.iter(), 30);

        assert!(wrapped.len() > 1, "must wrap into more than one row");
        let prefix = format_timestamp_prefix(test_timestamp());
        assert!(wrapped[0].0.starts_with(&prefix), "row 0: {:?}", wrapped[0]);
        let hang_indent = " ".repeat(TIMESTAMP_PREFIX_WIDTH);
        for (text, _) in &wrapped[1..] {
            assert!(
                text.starts_with(&hang_indent),
                "continuation row must hang-indent by the prefix width, not repeat the timestamp: {text:?}"
            );
            assert!(
                !text.starts_with(&prefix),
                "must not repeat the timestamp: {text:?}"
            );
        }
        // The message column (everything after each row's fixed-width
        // indent) must concatenate back to the original text exactly.
        let message_parts: String = wrapped
            .iter()
            .map(|(text, _)| &text[TIMESTAMP_PREFIX_WIDTH..])
            .collect();
        assert_eq!(message_parts, long_message);
    }

    #[test]
    fn wrap_log_lines_flattens_in_order_and_carries_is_error_per_row() {
        let lines = [
            log_line("short one", false),
            log_line("this is a longer error line", true),
        ];
        // message_width = 30 - 15 = 15.
        let wrapped = wrap_log_lines(lines.iter(), 30);

        // "short one" (9 cols) fits in one row; the error line (28 chars)
        // must wrap into multiple rows, every one still flagged is_error.
        assert_eq!(
            wrapped[0],
            ("2024-01-02 03:04:05 short one".to_string(), false)
        );
        assert!(wrapped.len() > 2, "the long line must wrap into >1 row");
        assert!(
            wrapped[1..].iter().all(|(_, is_error)| *is_error),
            "every wrapped row of an error line must stay flagged as an error: {wrapped:?}"
        );
    }

    #[test]
    fn wrap_log_lines_zero_width_is_empty() {
        let lines = [log_line("hello", false)];
        assert!(wrap_log_lines(lines.iter(), 0).is_empty());
    }

    /// `wrap_log_lines_tail(log.iter().rev(), width, needed_rows)` must
    /// produce exactly the same rows as taking `wrap_log_lines(log.iter(),
    /// width)`'s own last `needed_rows` entries — the whole point of the
    /// tail-first path is that it's a perf shortcut to the same answer, not
    /// a different one.
    fn assert_tail_matches_full_wrap(lines: &[TestLine], width: usize, needed_rows: usize) {
        let full = wrap_log_lines(lines.iter(), width);
        let expected_start = full.len().saturating_sub(needed_rows);
        let expected = &full[expected_start..];

        let tail = wrap_log_lines_tail(lines.iter().rev(), width, needed_rows);
        assert_eq!(
            tail, expected,
            "tail-first wrap must match the full wrap's own tail slice"
        );
    }

    #[test]
    fn wrap_log_lines_tail_matches_the_full_wrap_tail_when_lines_wrap_across_multiple_rows() {
        let lines: Vec<TestLine> = vec![
            log_line("short one", false),
            log_line(
                "this is a much longer line that will wrap across several rows",
                true,
            ),
            log_line("another short line", false),
            log_line("yet another line, also fairly short", false),
        ];
        // message_width small enough that at least one line wraps.
        assert_tail_matches_full_wrap(&lines, 30, 4);
    }

    #[test]
    fn wrap_log_lines_tail_stops_early_without_touching_lines_entirely_trimmed_away() {
        // Only the newest couple of (single-row) lines are ever needed —
        // this exercises the early-`break` path directly, not just its
        // output (a bug that read the whole log but still produced the
        // right *slice* would pass `assert_tail_matches_full_wrap` too).
        let lines: Vec<TestLine> = (1..=50)
            .map(|n| log_line(&format!("line {n}"), false))
            .collect();
        let tail = wrap_log_lines_tail(lines.iter().rev(), 80, 2);
        assert_eq!(
            tail,
            vec![
                ("2024-01-02 03:04:05 line 49".to_string(), false),
                ("2024-01-02 03:04:05 line 50".to_string(), false),
            ]
        );
    }

    #[test]
    fn wrap_log_lines_tail_requesting_more_rows_than_exist_returns_everything() {
        let lines: Vec<TestLine> = vec![log_line("only line", false)];
        assert_tail_matches_full_wrap(&lines, 80, 10);
    }

    #[test]
    fn wrap_log_lines_tail_zero_width_is_empty() {
        let lines = [log_line("hello", false)];
        assert!(wrap_log_lines_tail(lines.iter().rev(), 0, 5).is_empty());
    }

    #[test]
    fn wrap_log_lines_tail_zero_needed_rows_is_empty() {
        let lines = [log_line("hello", false)];
        assert!(wrap_log_lines_tail(lines.iter().rev(), 80, 0).is_empty());
    }

    #[test]
    fn wrap_log_lines_tail_empty_log_is_empty() {
        let lines: [TestLine; 0] = [];
        assert!(wrap_log_lines_tail(lines.iter().rev(), 80, 5).is_empty());
    }
}
