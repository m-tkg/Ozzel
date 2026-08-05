//! Renders the log area: the tail of the log that still fits after
//! reserving one row per currently-running background task for a progress
//! gauge (`desc [####----] 43% current_file`).
//!
//! Long lines wrap across multiple rows (width-aware, grapheme-safe —
//! see `wrap_to_width`) rather than getting clipped at the right edge, and
//! the *newest* wrapped rows stay bottom-anchored even when that means an
//! older line's wrapped continuation scrolls off the top first.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LogLine};
use crate::tasks::RunningTask;

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
/// wrapped rows. Every log line is wrapped to `area.width` first (see
/// `wrap_log_lines`), then only the last `available_rows` wrapped rows are
/// kept, so the newest content always stays visible even if that cuts an
/// older line's wrapped continuation off the top.
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

/// Wraps every log line to `width` display columns (see `wrap_to_width`),
/// flattening the result into `(display_row_text, is_error)` pairs in
/// original order — `is_error` is carried onto every wrapped row of an
/// error line, so a wrapped error message stays red top to bottom.
fn wrap_log_lines<'a>(log: impl Iterator<Item = &'a LogLine>, width: usize) -> Vec<(String, bool)> {
    if width == 0 {
        return Vec::new();
    }
    log.flat_map(|line| {
        wrap_to_width(&line.message, width)
            .into_iter()
            .map(|row| (row, line.is_error))
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

    fn log_line(message: &str, is_error: bool) -> LogLine {
        LogLine {
            message: message.to_string(),
            is_error,
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
    fn wrap_log_lines_flattens_in_order_and_carries_is_error_per_row() {
        let lines = [
            log_line("short one", false),
            log_line("this is a longer error line", true),
        ];
        let wrapped = wrap_log_lines(lines.iter(), 10);

        // "short one" (9 cols) fits in one row; the error line (28 chars)
        // must wrap into multiple rows, every one still flagged is_error.
        assert_eq!(wrapped[0], ("short one".to_string(), false));
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
        assert_eq!(tail, vec!["line 4", "line 5"], "must keep the newest rows");
    }

    #[test]
    fn bottom_anchoring_with_more_rows_available_than_content_keeps_everything() {
        let lines: Vec<LogLine> = vec![log_line("only line", false)];
        let wrapped = wrap_log_lines(lines.iter(), 80);
        let available_rows = 10;
        let start = wrapped.len().saturating_sub(available_rows);
        assert_eq!(start, 0, "must not skip content when there's room to spare");
    }
}
