//! Renders a single [`Pane`]: a bordered block titled with the (left-
//! truncated) cwd, and rows of name / size / mtime with grapheme-safe,
//! width-aware truncation so Japanese and other wide-glyph filenames align
//! correctly.

use std::time::SystemTime;

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::entry::EntryKind;
use crate::pane::{Pane, VisibleItem};

const MARK_COL_WIDTH: usize = 1;
const SIZE_COL_WIDTH: usize = 9;
const MTIME_COL_WIDTH: usize = 14;

/// Color/dim settings for rendering a pane, derived from `config::ColorsConfig`
/// by `ui/mod.rs`. Kept as its own small `Copy` struct (rather than passing
/// `&ColorsConfig` straight through) so this module stays independent of
/// `config.rs`.
#[derive(Debug, Clone, Copy)]
pub struct PaneColors {
    pub cursor: Color,
    pub cursor_inactive: Color,
    pub dim_inactive: bool,
}

pub fn render(frame: &mut Frame, area: Rect, pane: &Pane, active: bool, colors: PaneColors) {
    // The inactive pane dims when configured to (default on); its cursor
    // row dims along with everything else (see the cursor-row style below)
    // — it's still locatable since it's the only row with a background
    // fill at all, just a dimmed one.
    let dim = !active && colors.dim_inactive;
    let cursor_color = if active {
        colors.cursor
    } else {
        colors.cursor_inactive
    };

    let border_style = if dim {
        Style::default().add_modifier(Modifier::DIM)
    } else if active {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let title_budget = area.width.saturating_sub(2) as usize; // account for the two border corners
    let mut title_source = pane.cwd.display().to_string();
    if let Some(filter) = &pane.filter {
        title_source.push_str(&format!(" [flt: {}]", filter.raw));
    }
    let title = truncate_left(&title_source, title_budget);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items = pane.visible_entries();
    if items.is_empty() || inner.height == 0 {
        return;
    }

    let viewport_height = inner.height as usize;
    let cursor = pane.cursor.min(items.len().saturating_sub(1));
    let start = scroll_offset(cursor, items.len(), viewport_height);

    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .skip(start)
        .take(viewport_height)
        .map(|(idx, item)| {
            let marked = matches!(item, VisibleItem::Entry(e) if pane.marks.contains(&e.path));
            let text = format_row(item, inner.width as usize, marked);
            let style = row_style(idx == cursor, marked, dim, cursor_color);
            ListItem::new(Line::styled(text, style))
        })
        .collect();

    frame.render_widget(List::new(rows), inner);
}

/// The style for one row, given whether it's the cursor row, whether it's
/// marked, and whether the whole pane is currently dimmed. The cursor
/// row's bg/fg takes priority over the marked row's yellow fg when both
/// apply — yellow-on-light-green reads poorly, and the row's own `*`
/// prefix already marks "marked" unambiguously regardless of color.
/// Unlike an earlier round, the cursor row *does* dim along with the rest
/// of an inactive dimmed pane now (user feedback: an undimmed cursor stood
/// out too much against a dimmed pane) — it's still locatable since it's
/// the only row keeping a background fill at all, dimmed or not.
///
/// The cursor row drops `BOLD` when dimmed rather than combining it with
/// `DIM`: in the ANSI/VT100 model both attributes share the same terminal
/// "intensity" slot (SGR 1 = bold, SGR 2 = faint, mutually exclusive, not
/// independently stackable bits the way ratatui's `Modifier` bitflags
/// suggest) — asking for both at once is at the mercy of which one a given
/// terminal happens to apply last, which is exactly the ambiguity a
/// deliberate "dim" request shouldn't be subject to.
fn row_style(is_cursor: bool, marked: bool, dim: bool, cursor_color: Color) -> Style {
    if is_cursor {
        let base = Style::default().bg(cursor_color).fg(Color::Black);
        if dim {
            base.add_modifier(Modifier::DIM)
        } else {
            base.add_modifier(Modifier::BOLD)
        }
    } else if marked {
        let base = Style::default().fg(Color::Yellow);
        if dim {
            base.add_modifier(Modifier::DIM)
        } else {
            base
        }
    } else if dim {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default()
    }
}

/// Where the viewport should start so that `cursor` is always visible.
fn scroll_offset(cursor: usize, len: usize, viewport_height: usize) -> usize {
    if viewport_height == 0 || len <= viewport_height {
        return 0;
    }
    let max_start = len - viewport_height;
    cursor
        .saturating_sub(viewport_height.saturating_sub(1))
        .min(max_start)
}

fn format_row(item: &VisibleItem<'_>, width: usize, marked: bool) -> String {
    let (name, size_text, mtime_text) = match item {
        VisibleItem::Parent => ("..".to_string(), String::new(), String::new()),
        VisibleItem::Entry(e) => {
            let size_text = if e.kind == EntryKind::Dir {
                "<DIR>".to_string()
            } else {
                human_size(e.size)
            };
            let mtime_text = e.mtime.map(format_mtime).unwrap_or_default();
            (e.name.clone(), size_text, mtime_text)
        }
    };

    let mark_col = if marked { "*" } else { " " };
    let reserved = MARK_COL_WIDTH + SIZE_COL_WIDTH + MTIME_COL_WIDTH + 2; // two single-space separators
    let name_width = width.saturating_sub(reserved).max(3);

    let name_col = pad_right_display(&truncate_right(&name, name_width), name_width);
    let size_col = pad_left_display(&size_text, SIZE_COL_WIDTH);
    let mtime_col = pad_left_display(&mtime_text, MTIME_COL_WIDTH);

    format!("{mark_col}{name_col} {size_col} {mtime_col}")
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1}{}", UNITS[unit])
}

fn format_mtime(t: SystemTime) -> String {
    let dt: DateTime<Local> = t.into();
    dt.format("%y-%m-%d %H:%M").to_string()
}

/// Truncates `s` from the right (keeping the start) so its display width
/// fits within `max_width`, breaking only on grapheme-cluster boundaries and
/// appending an ellipsis when anything was cut.
fn truncate_right(s: &str, max_width: usize) -> String {
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
fn truncate_left(s: &str, max_width: usize) -> String {
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

fn pad_right_display(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

fn pad_left_display(s: &str, width: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{}{s}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_row_style_dims_when_pane_is_dimmed() {
        // The bug-fix under test: the inactive pane's cursor row must dim
        // along with the rest of the pane, not stay exempt.
        let style = row_style(true, false, true, Color::White);
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.bg, Some(Color::White));
        assert_eq!(style.fg, Some(Color::Black));
        // BOLD and DIM share the same ANSI "intensity" slot on a real
        // terminal (SGR 1 vs SGR 2, mutually exclusive) — asking for both
        // is unreliable, so the dimmed cursor row must drop BOLD rather
        // than combine it with DIM.
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn cursor_row_style_does_not_dim_when_pane_is_not_dimmed() {
        let style = row_style(true, false, false, Color::Rgb(0x90, 0xEE, 0x90));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.bg, Some(Color::Rgb(0x90, 0xEE, 0x90)));
    }

    #[test]
    fn marked_row_style_dims_when_pane_is_dimmed() {
        let style = row_style(false, true, true, Color::White);
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn plain_row_style_dims_when_pane_is_dimmed() {
        let style = row_style(false, false, true, Color::White);
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.bg, None);
        assert_eq!(style.fg, None);
    }

    #[test]
    fn plain_row_style_is_bare_when_not_dimmed() {
        let style = row_style(false, false, false, Color::White);
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style, Style::default());
    }

    #[test]
    fn human_size_formats_common_ranges() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(999), "999B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0G");
    }

    #[test]
    fn truncate_right_keeps_ascii_untouched_when_it_fits() {
        assert_eq!(truncate_right("short.txt", 20), "short.txt");
    }

    #[test]
    fn truncate_right_never_splits_a_japanese_grapheme() {
        let name = "日本語ファイル名.txt";
        // Each of 日/本/語/ファイル名 is width 2; force a truncation that
        // would land mid-character if done by byte count instead of width.
        let truncated = truncate_right(name, 7);
        assert!(truncated.ends_with('…'));
        // Must be valid UTF-8 (guaranteed by type) and must not exceed the
        // requested display width.
        assert!(UnicodeWidthStr::width(truncated.as_str()) <= 7);
        // Every grapheme in the result must also appear as a whole grapheme
        // in the source (no half-character survived).
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
    fn pad_right_display_accounts_for_wide_characters() {
        // "日本語" is 3 graphemes but 6 display columns wide.
        let padded = pad_right_display("日本語", 10);
        assert_eq!(UnicodeWidthStr::width(padded.as_str()), 10);
    }

    #[test]
    fn format_row_parent_row_has_no_size_or_mtime() {
        let row = format_row(&VisibleItem::Parent, 40, false);
        assert!(row.trim_start().starts_with(".."));
    }

    #[test]
    fn format_row_marks_prepend_asterisk() {
        let row = format_row(&VisibleItem::Parent, 40, true);
        assert!(row.starts_with('*'));
    }
}
