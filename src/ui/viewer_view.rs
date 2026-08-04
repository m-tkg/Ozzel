//! Renders `Mode::Viewer`: a full-frame, borderless scrollable text view
//! with a 1-line reverse-video footer (`path  [12-45/230]  q:close`). Takes
//! over the entire frame — panes, log, and status bar are all replaced
//! while a file is open, the same way a real pager would.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::mode::Mode;

pub fn render(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Viewer {
        path,
        lines,
        scroll,
        h_scroll,
        truncated,
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

    let visible: Vec<Line> = lines
        .iter()
        .skip(*scroll)
        .take(viewport_height)
        .map(|line| Line::raw(slice_display_cols(line, *h_scroll, viewport_width)))
        .collect();
    frame.render_widget(Paragraph::new(visible), rows[0]);

    let total = lines.len();
    let bottom = (*scroll + viewport_height).min(total);
    let range_text = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}-{}/{}", *scroll + 1, bottom, total)
    };
    let truncated_note = if *truncated { "  [truncated]" } else { "" };
    let footer = format!(
        " {}  [{range_text}]{truncated_note}  q:close",
        path.display()
    );
    let footer_style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(footer).style(footer_style), rows[1]);
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
}
