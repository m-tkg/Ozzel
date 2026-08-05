//! Renders `Mode::Viewer`: a full-frame, borderless scrollable view — plain
//! text or an `xxd`-style hex dump, toggled with Tab — with a 1-line
//! reverse-video footer showing the mode tag and the visible range (e.g.
//! `path  [text]  [12-45/230]  q:close`). Takes over the entire frame —
//! panes, log, and status bar are all replaced while a file is open, the
//! same way a real pager would. `less`-style `/`/`?` search (see
//! `crate::mode::ViewerSearch`) temporarily replaces this footer with a
//! search input line while typing, and — once a search is active —
//! highlights every match on the visible lines and appends match-count/
//! wrap info to the footer instead.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::mode::{LineEditor, Mode, SearchDirection, ViewMode, ViewerSearch};
use crate::viewer::{self, HEX_BYTES_PER_LINE, Matcher};

/// Style applied to the byte range of every search match on a visible
/// line — reverse video, same visual language the footer and the other
/// modal input lines (`Filter`/`JumpSearch`/`Prompt`) already use for
/// "this is drawing your attention," just without their red tint (that's
/// reserved for warnings/errors, not matches).
fn match_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub fn render(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Viewer {
        path,
        lines,
        bytes,
        view_mode,
        scroll,
        h_scroll,
        truncated,
        search,
    } = mode
    else {
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let viewport_height = rows[0].height as usize;
    let viewport_width = rows[0].width as usize;

    // While actively typing a search pattern, the matcher doesn't exist
    // yet (nothing has been searched for) — only an `Active` search has
    // anything to highlight.
    let matcher = match search {
        ViewerSearch::Active { pattern, .. } => Some(Matcher::build(pattern)),
        _ => None,
    };

    let (mode_tag, range_text) = match view_mode {
        ViewMode::Text => {
            let visible: Vec<Line> = lines
                .iter()
                .skip(*scroll)
                .take(viewport_height)
                .map(|line| styled_line(line, *h_scroll, viewport_width, matcher.as_ref()))
                .collect();
            frame.render_widget(Paragraph::new(visible), rows[0]);

            let total = lines.len();
            let bottom = (*scroll + viewport_height).min(total);
            let range_text = if total == 0 {
                "0/0".to_string()
            } else {
                format!("{}-{}/{} lines", *scroll + 1, bottom, total)
            };
            ("text", range_text)
        }
        ViewMode::Hex => {
            let total_rows = bytes.len().div_ceil(HEX_BYTES_PER_LINE).max(1);
            let visible: Vec<Line> = bytes
                .chunks(HEX_BYTES_PER_LINE)
                .enumerate()
                .skip(*scroll)
                .take(viewport_height)
                .map(|(row_idx, chunk)| {
                    let line = viewer::format_hex_line(chunk, row_idx * HEX_BYTES_PER_LINE);
                    styled_line(&line, 0, usize::MAX, matcher.as_ref())
                })
                .collect();
            frame.render_widget(Paragraph::new(visible), rows[0]);

            let bottom_row = (*scroll + viewport_height).min(total_rows);
            let start_byte = (*scroll * HEX_BYTES_PER_LINE).min(bytes.len());
            let end_byte = (bottom_row * HEX_BYTES_PER_LINE).min(bytes.len());
            let range_text = if bytes.is_empty() {
                "0/0 bytes".to_string()
            } else {
                format!("{start_byte}-{end_byte}/{} bytes", bytes.len())
            };
            ("hex", range_text)
        }
    };

    // A search input in progress takes over the footer row entirely, same
    // as `Filter`/`JumpSearch` do to the status bar — the content above is
    // still the ordinary viewer, just with the bottom line temporarily
    // repurposed for typing.
    if let ViewerSearch::Editing {
        input, direction, ..
    } = search
    {
        render_search_input_line(frame, rows[1], *direction, input);
        return;
    }

    let truncated_note = if *truncated { "  [truncated]" } else { "" };
    let search_note = match search {
        ViewerSearch::Active {
            pattern,
            direction,
            matches,
            current,
            wrapped,
        } => {
            let prefix = match direction {
                SearchDirection::Forward => '/',
                SearchDirection::Backward => '?',
            };
            let wrap_note = if *wrapped { "  (search wrapped)" } else { "" };
            format!(
                "  {prefix}{pattern}  {}/{}{wrap_note}",
                current + 1,
                matches.len()
            )
        }
        _ => String::new(),
    };
    let footer = format!(
        " {}  [{mode_tag}]  [{range_text}]{truncated_note}{search_note}  Tab:hex/text  /:search  q:close",
        path.display()
    );
    let footer_style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(footer).style(footer_style), rows[1]);
}

/// Draws the `/`/`?` search input line into `area`, mirroring
/// `ui::modal::render_input_line`'s look (reverse video, real terminal
/// cursor positioned at the edit point) without depending on that private
/// helper — the viewer is a full-frame takeover with its own layout, not
/// the shared pane/status-bar frame `modal.rs`'s callers draw into.
fn render_search_input_line(
    frame: &mut Frame,
    area: Rect,
    direction: SearchDirection,
    input: &LineEditor,
) {
    let label = match direction {
        SearchDirection::Forward => "/",
        SearchDirection::Backward => "?",
    };
    let text = format!("{label}{}", input.value());
    let style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(text).style(style), area);

    let col = area.x + UnicodeWidthStr::width(label) as u16 + input.cursor_display_col() as u16;
    let col = col.min(area.x + area.width.saturating_sub(1));
    frame.set_cursor_position((col, area.y));
}

/// Builds one rendered `Line` for `line`'s visible slice
/// `[start_col, start_col + width)` (see `slice_display_cols`), split into
/// unstyled/highlighted spans wherever `matcher` (`None` when there's no
/// active search) matches — grapheme-safe, so a highlighted run never
/// splits a multi-byte character. `width = usize::MAX` (hex mode, which
/// never horizontally scrolls) behaves as "no clipping."
fn styled_line(
    line: &str,
    start_col: usize,
    width: usize,
    matcher: Option<&Matcher>,
) -> Line<'static> {
    let Some(matcher) = matcher else {
        return Line::raw(slice_display_cols(line, start_col, width));
    };
    let match_ranges = matcher.find_ranges(line);
    if match_ranges.is_empty() {
        return Line::raw(slice_display_cols(line, start_col, width));
    }
    let spans: Vec<Span> = highlighted_segments(line, start_col, width, &match_ranges)
        .into_iter()
        .map(|(text, is_match)| {
            if is_match {
                Span::styled(text, match_style())
            } else {
                Span::raw(text)
            }
        })
        .collect();
    Line::from(spans)
}

/// Returns the substring of `line` covering display columns
/// `[start_col, start_col + width)`, breaking only on grapheme-cluster
/// boundaries. A grapheme whose *start* column falls inside the window is
/// included in full; one that starts before it is dropped in full — cheap
/// and correct for the common case, at the cost of not clipping a wide
/// grapheme that straddles the window edge mid-glyph.
fn slice_display_cols(line: &str, start_col: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
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

/// Like `slice_display_cols`, but splits the visible window into runs of
/// `(text, is_match)` instead of a single plain string — the run boundary
/// bookkeeping `styled_line` needs to turn a line into alternating
/// unstyled/highlighted spans. A grapheme counts as a match if its byte
/// range intersects *any* range in `match_byte_ranges` (byte offsets into
/// the original, un-sliced `line`, as `viewer::Matcher::find_ranges`
/// returns) even partially, so a multi-byte grapheme is never split
/// visually across a match boundary. Consecutive graphemes with the same
/// match flag are coalesced into one run, so a multi-character match
/// becomes a single `Span` rather than one per grapheme.
fn highlighted_segments(
    line: &str,
    start_col: usize,
    width: usize,
    match_byte_ranges: &[(usize, usize)],
) -> Vec<(String, bool)> {
    if width == 0 {
        return Vec::new();
    }
    let end_col = start_col.saturating_add(width);
    let mut segments: Vec<(String, bool)> = Vec::new();
    let mut col = 0usize;
    let mut byte_pos = 0usize;
    for g in line.graphemes(true) {
        let g_start_byte = byte_pos;
        let g_end_byte = g_start_byte + g.len();
        byte_pos = g_end_byte;

        if col >= end_col {
            break;
        }
        let w = UnicodeWidthStr::width(g).max(1);
        if col >= start_col {
            let is_match = match_byte_ranges
                .iter()
                .any(|&(s, e)| s < g_end_byte && g_start_byte < e);
            match segments.last_mut() {
                Some((text, last_is_match)) if *last_is_match == is_match => text.push_str(g),
                _ => segments.push((g.to_string(), is_match)),
            }
        }
        col += w;
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // "日本語" is 3 graphemes, each width 2 (total width 6).
        assert_eq!(slice_display_cols("日本語", 0, 2), "日");
        assert_eq!(slice_display_cols("日本語", 2, 2), "本");
        assert_eq!(slice_display_cols("日本語", 4, 2), "語");
    }

    #[test]
    fn slice_display_cols_offset_past_end_is_empty() {
        assert_eq!(slice_display_cols("short", 100, 20), "");
    }

    // --- `highlighted_segments`: match-span math --------------------------

    #[test]
    fn highlighted_segments_with_no_matches_is_one_unmatched_run() {
        let segments = highlighted_segments("hello world", 0, 80, &[]);
        assert_eq!(segments, vec![("hello world".to_string(), false)]);
    }

    #[test]
    fn highlighted_segments_splits_into_unmatched_matched_unmatched_runs() {
        // "world" is bytes 6..11 of "hello world".
        let segments = highlighted_segments("hello world", 0, 80, &[(6, 11)]);
        assert_eq!(
            segments,
            vec![("hello ".to_string(), false), ("world".to_string(), true),]
        );
    }

    #[test]
    fn highlighted_segments_coalesces_adjacent_matches_into_one_run() {
        // Two back-to-back match ranges covering "ab" (0..1) and "b" (1..2)
        // must still produce a single merged highlighted run "ab", not two
        // separate one-character spans.
        let segments = highlighted_segments("abc", 0, 80, &[(0, 1), (1, 2)]);
        assert_eq!(
            segments,
            vec![("ab".to_string(), true), ("c".to_string(), false)]
        );
    }

    #[test]
    fn highlighted_segments_on_japanese_text_never_splits_a_grapheme() {
        // "本語" inside "日本語のテキスト" is the 3-byte-per-character
        // range starting right after "日" (bytes 3..9, covering "本語").
        // Every byte offset here lands mid-line, not at char/grapheme 0 —
        // exactly the case that would break a naive byte-slicing approach.
        let line = "日本語のテキスト";
        let start = line.find('本').unwrap();
        let end = start + "本語".len();
        let segments = highlighted_segments(line, 0, 80, &[(start, end)]);
        assert_eq!(
            segments,
            vec![
                ("日".to_string(), false),
                ("本語".to_string(), true),
                ("のテキスト".to_string(), false),
            ]
        );
    }

    #[test]
    fn highlighted_segments_respects_horizontal_scroll_and_width() {
        // "world" (display cols 6..11) partially highlighted, but the
        // window only shows cols 6..9 ("wor") — the match still applies to
        // whatever of it is visible.
        let segments = highlighted_segments("hello world", 6, 3, &[(6, 11)]);
        assert_eq!(segments, vec![("wor".to_string(), true)]);
    }

    #[test]
    fn highlighted_segments_zero_width_is_empty() {
        assert_eq!(highlighted_segments("hello", 0, 0, &[(0, 5)]), Vec::new());
    }

    /// With mouse capture dynamically disabled while the viewer is up
    /// (see `App::wants_mouse_capture`), a native terminal click-drag
    /// selection must only ever be able to grab real file content, never
    /// a border glyph or the title — this renders a real frame and checks
    /// for any box-drawing character `Borders::ALL` might have produced.
    #[test]
    fn render_draws_no_border_characters() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        const BORDER_GLYPHS: [char; 11] = ['─', '│', '┌', '┐', '└', '┘', '┬', '┴', '├', '┤', '┼'];

        let mode = Mode::Viewer {
            path: std::path::PathBuf::from("example.txt"),
            lines: vec!["hello world".to_string(), "a second line".to_string()],
            bytes: b"hello world\na second line".to_vec(),
            view_mode: ViewMode::Text,
            scroll: 0,
            h_scroll: 0,
            truncated: false,
            search: ViewerSearch::Idle,
        };

        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mode))
            .unwrap();

        for cell in terminal.backend().buffer().content() {
            let symbol = cell.symbol();
            assert!(
                !BORDER_GLYPHS.iter().any(|g| symbol.contains(*g)),
                "unexpected border glyph {symbol:?} in the rendered viewer"
            );
        }
    }

    /// Renders `mode` into a `width`x`height` `TestBackend` and returns
    /// every row's plain text (symbols concatenated, no styling) — enough
    /// to assert on *what* was drawn; `render_marks_search_matches_in_reverse_video`
    /// below inspects styling directly instead of going through this.
    fn render_to_rows(mode: &Mode, width: u16, height: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), mode))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    fn text_viewer_mode(lines: Vec<&str>, search: ViewerSearch) -> Mode {
        let bytes = lines.join("\n").into_bytes();
        Mode::Viewer {
            path: std::path::PathBuf::from("example.txt"),
            lines: lines.into_iter().map(str::to_string).collect(),
            bytes,
            view_mode: ViewMode::Text,
            scroll: 0,
            h_scroll: 0,
            truncated: false,
            search,
        }
    }

    #[test]
    fn render_editing_search_shows_the_prompt_and_typed_pattern_in_the_footer_row() {
        let mode = text_viewer_mode(
            vec!["hello world"],
            ViewerSearch::Editing {
                input: LineEditor::from_str("needle"),
                direction: SearchDirection::Forward,
                previous: Box::new(ViewerSearch::Idle),
            },
        );
        let rows = render_to_rows(&mode, 40, 4);
        assert!(
            rows.last().unwrap().starts_with("/needle"),
            "footer row: {:?}",
            rows.last()
        );
    }

    #[test]
    fn render_backward_editing_search_shows_a_question_mark_prompt() {
        let mode = text_viewer_mode(
            vec!["hello world"],
            ViewerSearch::Editing {
                input: LineEditor::new(),
                direction: SearchDirection::Backward,
                previous: Box::new(ViewerSearch::Idle),
            },
        );
        let rows = render_to_rows(&mode, 40, 4);
        assert!(rows.last().unwrap().starts_with('?'));
    }

    #[test]
    fn render_active_search_shows_pattern_and_match_count_in_the_footer() {
        let mode = text_viewer_mode(
            vec!["one", "needle here", "three", "needle again"],
            ViewerSearch::Active {
                pattern: "needle".to_string(),
                direction: SearchDirection::Forward,
                matches: vec![1, 3],
                current: 0,
                wrapped: false,
            },
        );
        let rows = render_to_rows(&mode, 60, 6);
        let footer = rows.last().unwrap();
        assert!(footer.contains("/needle"), "footer: {footer:?}");
        assert!(footer.contains("1/2"), "footer: {footer:?}");
        assert!(
            !footer.contains("wrapped"),
            "must not show the wrap notice when it didn't wrap: {footer:?}"
        );
    }

    #[test]
    fn render_active_search_that_wrapped_shows_the_wrap_notice_in_the_footer() {
        let mode = text_viewer_mode(
            vec!["needle one", "needle two"],
            ViewerSearch::Active {
                pattern: "needle".to_string(),
                direction: SearchDirection::Forward,
                matches: vec![0, 1],
                current: 0,
                wrapped: true,
            },
        );
        let rows = render_to_rows(&mode, 120, 4);
        assert!(rows.last().unwrap().contains("wrapped"));
    }

    #[test]
    fn render_marks_search_matches_in_reverse_video() {
        let mode = text_viewer_mode(
            vec!["hello needle world"],
            ViewerSearch::Active {
                pattern: "needle".to_string(),
                direction: SearchDirection::Forward,
                matches: vec![0],
                current: 0,
                wrapped: false,
            },
        );
        let backend = ratatui::backend::TestBackend::new(40, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mode))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        // "needle" starts at column 6 of the content row (row 0).
        let match_cell = &buffer[(6, 0)];
        assert!(
            match_cell.modifier.contains(Modifier::REVERSED),
            "the matched span must be reverse-video styled"
        );
        let before_cell = &buffer[(0, 0)];
        assert!(
            !before_cell.modifier.contains(Modifier::REVERSED),
            "text outside the match must stay unstyled"
        );
    }
}
