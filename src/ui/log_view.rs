//! Renders the log area: the tail of the log that still fits after
//! reserving one row per currently-running background task for a progress
//! gauge (`desc [####----] 43% current_file`).
//!
//! Every log line is prefixed with the local timestamp it was appended at
//! (`YYYY-MM-dd HH:MM:SS `, captured once by `App::log_push` — see
//! `crate::logwrap::format_timestamp_prefix`; the year was added so a log
//! spanning midnight on New Year's Eve, or one simply read back much
//! later, is never ambiguous), and long lines wrap across multiple rows
//! (width-aware, grapheme-safe — see `crate::logwrap`'s `wrap_to_width`)
//! rather than getting clipped at the right edge. A wrapped line's
//! continuation rows hang-indent by the timestamp prefix's width instead
//! of repeating it, so the message column stays aligned under the first
//! row's. The *newest* wrapped rows stay bottom-anchored even when that
//! means an older line's wrapped continuation scrolls off the top first.
//!
//! The wrapping/timestamp-formatting itself lives in `crate::logwrap`
//! rather than here, so `app.rs` (which needs the same wrapping for
//! `App::log_wrapped`'s search support, and the same timestamp formatting
//! for every `LogLine` it appends) can reuse it without depending on `ui`
//! — see that module's doc comment for the full rationale.

use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::app::App;
use crate::logwrap::wrap_log_lines_tail;
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
/// wrapped rows. Every log line is timestamp-prefixed and wrapped to
/// `area.width` first (see `wrap_log_lines`), then only the last
/// `available_rows` wrapped rows are kept, so the newest content always
/// stays visible even if that cuts an older line's wrapped continuation
/// off the top.
fn render_log_lines(frame: &mut Frame, area: Rect, app: &App, available_rows: usize) {
    let wrapped = wrap_log_lines_tail(app.log.iter().rev(), area.width as usize, available_rows);

    let lines: Vec<Line> = wrapped
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
///
/// Returns this frame's content width (`rows[0].width`) rather than writing
/// it directly into `App::log_view_width` the way an earlier round did —
/// `ui::draw` (this function's only caller) folds it into the
/// `LayoutFeedback` it returns instead, so a render function never mutates
/// `App`'s externally-visible state as a side effect of drawing; see
/// `ui::LayoutFeedback`'s doc comment for the full reasoning. The caller
/// (`main.rs`'s loop, via `App::apply_layout_feedback`) is what actually
/// writes `App::log_view_width`, on the same "stale until the first frame
/// draws, harmless" idiom `App::pane_layout` already relies on for mouse
/// hit-testing.
pub fn render_full(frame: &mut Frame, area: Rect, app: &mut App, scroll_from_bottom: usize) -> u16 {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    let content_width = rows[0].width;

    // Pulled out as an owned value *before* `app.log_wrapped` below, which
    // needs `app` mutably (it may rebuild and cache the wrap) — `search` is
    // small to clone (its `matcher`, if any, is an `Rc` bump) and this way
    // nothing here needs to juggle two live borrows of `app` at once. Like
    // the `matcher` lookup this replaces, defaults to "no active search"
    // rather than assuming the mode (callers only ever invoke this while
    // `app.mode` is `Mode::Log`, but nothing here actually depends on that).
    let search = match &app.mode {
        crate::mode::Mode::Log { search, .. } => search.clone(),
        _ => crate::mode::ViewerSearch::Idle,
    };

    let viewport = rows[0].height as usize;
    let wrapped = app.log_wrapped(rows[0].width);
    let start = log_view_start(wrapped.len(), viewport, scroll_from_bottom);
    let end = (start + viewport).min(wrapped.len());

    // While actively typing a search pattern, the matcher doesn't exist
    // yet — only an `Active` search (see `super::viewer_view`'s identical
    // reasoning) has anything to highlight. The matcher itself was compiled
    // once by `crate::search::run`, not rebuilt here every frame.
    let matcher = match &search {
        crate::mode::ViewerSearch::Active { matcher, .. } => Some(Rc::clone(matcher)),
        _ => None,
    };

    let lines: Vec<Line> = wrapped[start..end]
        .iter()
        .map(|(text, is_error)| {
            let base = super::viewer_view::styled_line(text, 0, usize::MAX, matcher.as_deref());
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

    // `wrapped`'s last use was just above (`total`/the slice for `lines`),
    // so `app` is no longer borrowed from here on — everything below reads
    // `search` (already an owned copy) instead of `app.mode` again.
    if let crate::mode::ViewerSearch::Editing {
        input, direction, ..
    } = &search
    {
        crate::ui::modal::render_prefixed_input_line(
            frame,
            rows[1],
            direction.label(),
            input,
            None,
        );
        return content_width;
    }

    let search_note = match &search {
        crate::mode::ViewerSearch::Active {
            pattern,
            direction,
            matches,
            current,
            wrapped: search_wrapped,
            ..
        } => {
            let prefix = direction.label();
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

    content_width
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
    use chrono::{DateTime, Local, TimeZone};

    use super::*;
    use crate::app::LogLine;
    use crate::logwrap::{format_timestamp_prefix, wrap_log_lines};

    /// A fixed local timestamp for every test `LogLine` below — the exact
    /// value doesn't matter to most of these tests (they're about
    /// bottom-anchoring, not the clock), so using one constant keeps
    /// expected strings easy to build via `format_timestamp_prefix` itself.
    /// The wrapping/timestamp-formatting tests themselves now live in
    /// `crate::logwrap`'s own test module — see that module's doc comment
    /// for why the logic moved there.
    fn test_timestamp() -> DateTime<Local> {
        Local.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap()
    }

    fn log_line(message: &str, is_error: bool) -> LogLine {
        LogLine::new(message.to_string(), is_error, test_timestamp())
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
            .draw(|frame| {
                render_full(frame, frame.area(), &mut app, 0);
            })
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
