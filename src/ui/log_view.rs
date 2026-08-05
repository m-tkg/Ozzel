//! Renders the log area: the tail of the log that still fits after
//! reserving one row per currently-running background task for a progress
//! gauge (`desc [####----] 43% current_file`).
//!
//! Every log line is prefixed with the local timestamp it was appended at
//! (`YYYY-MM-dd HH:MM:SS `, captured once by `App::log_push` — see
//! `format_timestamp_prefix`; the year was added so a log spanning
//! midnight on New Year's Eve, or one simply read back much later, is
//! never ambiguous), and long lines wrap across multiple rows
//! (width-aware, grapheme-safe — see `wrap_to_width`) rather than getting
//! clipped at the right edge. A wrapped line's continuation rows hang-
//! indent by the timestamp prefix's width instead of repeating it, so the
//! message column stays aligned under the first row's. The *newest*
//! wrapped rows stay bottom-anchored even when that means an older line's
//! wrapped continuation scrolls off the top first.

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LogLine};
use crate::tasks::RunningTask;

/// `strftime`-style format for a log line's timestamp prefix: 4-digit
/// year, 2-digit month/day/hour/minute/second, fixed punctuation, always
/// exactly `TIMESTAMP_PREFIX_WIDTH` display columns wide (ASCII-only, so
/// char count and display width are the same).
const TIMESTAMP_FORMAT: &str = "%Y-%m-%d %H:%M:%S ";
/// Display width of `TIMESTAMP_FORMAT`'s output (e.g.
/// `"2026-08-05 14:03:22 "`) — doubles as the hang-indent width for a
/// wrapped line's continuation rows, so the message column lines up under
/// the first row's.
const TIMESTAMP_PREFIX_WIDTH: usize = 20;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().title("Log").borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let running: Vec<&RunningTask> = app.tasks.running.values().collect();
    let gauge_rows = running.len().min(inner.height as usize);
    let log_rows = inner.height - gauge_rows as u16;

    let mut constraints = Vec::with_capacity(gauge_rows + 1);
    if log_rows > 0 {
        constraints.push(Constraint::Length(log_rows));
    }
    constraints.extend(std::iter::repeat_n(Constraint::Length(1), gauge_rows));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut row = 0;
    if log_rows > 0 {
        render_log_lines(frame, rows[row], app, log_rows as usize);
        row += 1;
    }
    for task in running.into_iter().take(gauge_rows) {
        render_gauge(frame, rows[row], task);
        row += 1;
    }
}

/// `available_rows` is a display-row budget (post gauge-row subtraction),
/// not a `LogLine` count — a single long log line can expand into several
/// wrapped rows. Every log line is timestamp-prefixed and wrapped to
/// `area.width` first (see `wrap_log_lines`), then only the last
/// `available_rows` wrapped rows are kept, so the newest content always
/// stays visible even if that cuts an older line's wrapped continuation
/// off the top.
fn render_log_lines(frame: &mut Frame, area: Rect, app: &App, available_rows: usize) {
    let wrapped = wrap_log_lines(app.log.iter(), area.width as usize);
    let start = wrapped.len().saturating_sub(available_rows);

    let lines: Vec<Line> = wrapped[start..]
        .iter()
        .map(|(text, is_error)| {
            let style = if *is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::styled(text.clone(), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// Formats `timestamp` as the fixed-width prefix every log row gets (see
/// `TIMESTAMP_FORMAT`). Takes the timestamp as a plain value rather than
/// calling `Local::now()` itself, so it stays a pure, directly-testable
/// function — the actual "when" is captured once, at append time, by
/// `App::log_push`.
fn format_timestamp_prefix(timestamp: DateTime<Local>) -> String {
    timestamp.format(TIMESTAMP_FORMAT).to_string()
}

/// Wraps every log line to `width` display columns — the *message* portion
/// wraps against `width` minus the timestamp prefix's width (see
/// `wrap_to_width`), and the result is flattened into
/// `(display_row_text, is_error)` pairs in original order. The first
/// wrapped row of each line gets the real timestamp prefix; continuation
/// rows get a blank hang-indent of the same width instead, so the message
/// column stays aligned without repeating the timestamp. `is_error` is
/// carried onto every wrapped row of an error line, so a wrapped error
/// message stays red top to bottom. `pub(crate)` (rather than private) so
/// `App::run_log_search`/`log_search_step` can rewrap the log at
/// `App::log_view_width` — the same width this was last rendered at — to
/// search against exactly the rows the screen shows. `pub` (rather than
/// `pub(crate)`) additionally so `benches/`'s criterion bench, an external
/// target, can measure it directly.
pub fn wrap_log_lines<'a>(
    log: impl Iterator<Item = &'a LogLine>,
    width: usize,
) -> Vec<(String, bool)> {
    if width == 0 {
        return Vec::new();
    }
    let message_width = width.saturating_sub(TIMESTAMP_PREFIX_WIDTH);
    let hang_indent = " ".repeat(TIMESTAMP_PREFIX_WIDTH);

    log.flat_map(move |line| {
        let prefix = format_timestamp_prefix(line.timestamp);
        let is_error = line.is_error;
        let hang_indent = hang_indent.clone();
        wrap_to_width(&line.message, message_width)
            .into_iter()
            .enumerate()
            .map(move |(i, row)| {
                let indent = if i == 0 { &prefix } else { &hang_indent };
                (format!("{indent}{row}"), is_error)
            })
    })
    .collect()
}

/// Hard-wraps `s` into chunks of at most `width` display columns, breaking
/// only on grapheme-cluster boundaries (never mid-character, even for wide
/// Japanese graphemes) — this is a plain width-based wrap, not word-wrap,
/// since log messages are often paths with no spaces to break on. An empty
/// string still yields one (empty) row, matching the pre-wrap one-row-per-
/// line baseline for a blank message.
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

/// `L`/`S-l`: renders the *entire* in-memory log (all `LOG_CAPACITY` lines,
/// not just the tail that fits in the status area's fixed 4 rows) as a
/// full-frame takeover, scrolled per `scroll_from_bottom` — see
/// `Mode::Log`'s doc comment for why that's stored as "rows scrolled up
/// from the newest content" rather than an absolute index: only this
/// function actually knows the terminal width (and therefore the real
/// wrapped row count) needed to turn it into a concrete start offset (see
/// `log_view_start`).
///
/// Deliberately borderless — same layout as `viewer_view`/`help_view`
/// (content row(s) + a 1-line reverse-video footer, no `Block`/`Borders`
/// anywhere) — so that with mouse capture dynamically disabled while this
/// mode is up (see `App::wants_mouse_capture`), a native terminal
/// click-drag text selection only ever grabs log content, never a border
/// glyph or the title.
pub fn render_full(frame: &mut Frame, area: Rect, app: &mut App, scroll_from_bottom: usize) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    // Cached so `App::run_log_search`/`log_search_step` (a keystroke, not a
    // frame — `App` otherwise has no way to know the terminal width at
    // all) can rewrap the log into exactly these same rows to search
    // against. Same "stale until the first frame draws, harmless" idiom
    // `App::pane_layout` already relies on for mouse hit-testing.
    app.log_view_width = rows[0].width;

    let viewport = rows[0].height as usize;
    let wrapped = wrap_log_lines(app.log.iter(), rows[0].width as usize);
    let start = log_view_start(wrapped.len(), viewport, scroll_from_bottom);
    let end = (start + viewport).min(wrapped.len());

    // While actively typing a search pattern, the matcher doesn't exist
    // yet — only an `Active` search (see `super::viewer_view`'s identical
    // reasoning) has anything to highlight.
    let matcher = match &app.mode {
        crate::mode::Mode::Log {
            search: crate::mode::ViewerSearch::Active { pattern, .. },
            ..
        } => Some(crate::viewer::Matcher::build(pattern)),
        _ => None,
    };

    let lines: Vec<Line> = wrapped[start..end]
        .iter()
        .map(|(text, is_error)| {
            let base = super::viewer_view::styled_line(text, 0, usize::MAX, matcher.as_ref());
            if *is_error {
                base.patch_style(Style::default().fg(Color::Red))
            } else {
                base
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows[0]);

    let total = wrapped.len();
    let range_text = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}-{}/{total}", start + 1, end)
    };

    if let crate::mode::Mode::Log {
        search: crate::mode::ViewerSearch::Editing {
            input, direction, ..
        },
        ..
    } = &app.mode
    {
        render_search_input_line(frame, rows[1], *direction, input);
        return;
    }

    let search_note = match &app.mode {
        crate::mode::Mode::Log {
            search:
                crate::mode::ViewerSearch::Active {
                    pattern,
                    direction,
                    matches,
                    current,
                    wrapped: search_wrapped,
                },
            ..
        } => {
            let prefix = match direction {
                crate::mode::SearchDirection::Forward => '/',
                crate::mode::SearchDirection::Backward => '?',
            };
            let wrap_note = if *search_wrapped {
                "  (search wrapped)"
            } else {
                ""
            };
            format!(
                "  {prefix}{pattern}  {}/{}{wrap_note}",
                current + 1,
                matches.len()
            )
        }
        _ => String::new(),
    };

    let footer = format!(" Log  [{range_text}]{search_note}  /,?:search  q/Esc:close");
    let footer_style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(footer).style(footer_style), rows[1]);
}

/// Draws the `/`/`?` search input line into `area` — mirrors
/// `ui::viewer_view`'s identical helper (kept as a small, private
/// duplicate rather than a shared export: `log_view`'s footer row has no
/// other reason to depend on `viewer_view` beyond `styled_line`/
/// `match_style`, which are already reused above and below).
fn render_search_input_line(
    frame: &mut Frame,
    area: Rect,
    direction: crate::mode::SearchDirection,
    input: &crate::mode::LineEditor,
) {
    let label = match direction {
        crate::mode::SearchDirection::Forward => "/",
        crate::mode::SearchDirection::Backward => "?",
    };
    let text = format!("{label}{}", input.value());
    let style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(text).style(style), area);

    let col = area.x + UnicodeWidthStr::width(label) as u16 + input.cursor_display_col() as u16;
    let col = col.min(area.x + area.width.saturating_sub(1));
    frame.set_cursor_position((col, area.y));
}

/// The pure scroll math behind `render_full`: `scroll_from_bottom` rows
/// scrolled up from the bottom-anchored position (`total_rows.saturating_sub(viewport)`),
/// saturating at `0` (the very top) rather than panicking or wrapping when
/// `scroll_from_bottom` overshoots — which is exactly how `Home`
/// (`scroll_from_bottom = usize::MAX`, see `App::handle_log_view_key`) is
/// able to mean "scroll all the way to the top" without `App` itself ever
/// knowing how many wrapped rows that actually is.
fn log_view_start(total_rows: usize, viewport: usize, scroll_from_bottom: usize) -> usize {
    let max_start = total_rows.saturating_sub(viewport);
    max_start.saturating_sub(scroll_from_bottom)
}

fn render_gauge(frame: &mut Frame, area: Rect, task: &RunningTask) {
    let (done, total) = task.progress;
    let ratio = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };
    let percent = (ratio * 100.0).round() as u32;
    let label = if task.detail.is_empty() {
        format!("{} {percent}%", task.desc)
    } else {
        format!("{} {percent}% {}", task.desc, task.detail)
    };
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .gauge_style(Style::default().fg(Color::Cyan));
    frame.render_widget(gauge, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A fixed local timestamp for every test `LogLine` below — the exact
    /// value doesn't matter to most of these tests (they're about wrapping
    /// and bottom-anchoring, not the clock), so using one constant keeps
    /// expected strings easy to build via `format_timestamp_prefix` itself.
    fn test_timestamp() -> DateTime<Local> {
        Local.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap()
    }

    fn log_line(message: &str, is_error: bool) -> LogLine {
        LogLine {
            message: message.to_string(),
            is_error,
            timestamp: test_timestamp(),
        }
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
        // Each of "日本語ファイル" is a width-2 grapheme; width=5 must wrap
        // after 2 graphemes (4 cols), not 2.5.
        let rows = wrap_to_width("日本語ファイル", 5);
        for row in &rows {
            assert!(
                UnicodeWidthStr::width(row.as_str()) <= 5,
                "row {row:?} exceeds width 5"
            );
        }
        // Every grapheme in the source must survive intact in some row.
        let source_graphemes: Vec<&str> = "日本語ファイル".graphemes(true).collect();
        let rebuilt: String = rows.concat();
        let rebuilt_graphemes: Vec<&str> = rebuilt.graphemes(true).collect();
        assert_eq!(source_graphemes, rebuilt_graphemes);
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
    fn format_timestamp_prefix_is_a_fixed_width_yyyy_mm_dd_hh_mm_ss() {
        let prefix = format_timestamp_prefix(test_timestamp());
        assert_eq!(prefix, "2024-01-02 03:04:05 ");
        assert_eq!(
            UnicodeWidthStr::width(prefix.as_str()),
            TIMESTAMP_PREFIX_WIDTH
        );
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

    #[test]
    fn bottom_anchoring_keeps_the_newest_wrapped_rows() {
        // Mirrors exactly what `render_log_lines` does with its
        // `wrapped.len().saturating_sub(available_rows)` slice, using
        // fixed-width single-row messages so row count == line count and
        // the tail is easy to reason about.
        let lines: Vec<LogLine> = (1..=5)
            .map(|n| log_line(&format!("line {n}"), false))
            .collect();
        let wrapped = wrap_log_lines(lines.iter(), 80);
        assert_eq!(wrapped.len(), 5);

        let available_rows = 2;
        let start = wrapped.len().saturating_sub(available_rows);
        let tail: Vec<&str> = wrapped[start..].iter().map(|(t, _)| t.as_str()).collect();
        let prefix = format_timestamp_prefix(test_timestamp());
        assert_eq!(
            tail,
            vec![format!("{prefix}line 4"), format!("{prefix}line 5")],
            "must keep the newest rows"
        );
    }

    #[test]
    fn log_view_start_at_zero_scroll_is_bottom_anchored() {
        assert_eq!(log_view_start(100, 20, 0), 80);
    }

    #[test]
    fn log_view_start_scrolls_up_by_the_requested_amount() {
        assert_eq!(log_view_start(100, 20, 30), 50);
    }

    #[test]
    fn log_view_start_saturates_at_zero_rather_than_underflowing() {
        assert_eq!(log_view_start(100, 20, 1000), 0);
    }

    #[test]
    fn log_view_start_home_sentinel_scrolls_all_the_way_to_the_top() {
        assert_eq!(log_view_start(100, 20, usize::MAX), 0);
    }

    #[test]
    fn log_view_start_when_content_fits_the_viewport_is_always_zero() {
        assert_eq!(log_view_start(5, 20, 0), 0);
        assert_eq!(log_view_start(5, 20, 3), 0);
    }

    #[test]
    fn bottom_anchoring_with_more_rows_available_than_content_keeps_everything() {
        let lines: Vec<LogLine> = vec![log_line("only line", false)];
        let wrapped = wrap_log_lines(lines.iter(), 80);
        let available_rows = 10;
        let start = wrapped.len().saturating_sub(available_rows);
        assert_eq!(start, 0, "must not skip content when there's room to spare");
    }

    /// Every box-drawing glyph `Borders::ALL` might have drawn — used by
    /// `render_full`'s (and its siblings') "no border characters anywhere"
    /// regression tests: with mouse capture dynamically disabled while
    /// these full-frame text modes are up (see `App::wants_mouse_capture`),
    /// a native terminal click-drag selection must only ever be able to
    /// grab real content, never a border glyph or a title.
    const BORDER_GLYPHS: [char; 11] = ['─', '│', '┌', '┐', '└', '┘', '┬', '┴', '├', '┤', '┼'];

    #[test]
    fn render_full_draws_no_border_characters() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let dir = tempfile::tempdir().unwrap();
        let mut app = crate::app::App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            crate::config::Config::default(),
        )
        .unwrap();
        // A message long enough to wrap, so a continuation row is
        // exercised too, not just single-line content.
        app.log.push_back(log_line(
            "a reasonably long log message that should wrap across more than one row",
            false,
        ));

        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_full(frame, frame.area(), &mut app, 0))
            .unwrap();

        for cell in terminal.backend().buffer().content() {
            let symbol = cell.symbol();
            assert!(
                !BORDER_GLYPHS.iter().any(|g| symbol.contains(*g)),
                "unexpected border glyph {symbol:?} in the rendered log view"
            );
        }
    }
}
