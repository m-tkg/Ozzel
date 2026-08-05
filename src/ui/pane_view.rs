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
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::PaneLayout;
use crate::entry::EntryKind;
#[cfg(test)]
use crate::entry::FsEntry;
use crate::pane::{Pane, VisibleItem};
use crate::virtual_dir;

const MARK_COL_WIDTH: usize = 1;
const SIZE_COL_WIDTH: usize = 9;
const MTIME_COL_WIDTH: usize = 14;
/// `drwxr-xr-x` — type char + 3x(rwx).
#[cfg(unix)]
const PERMS_COL_WIDTH: usize = 10;
/// The Windows fallback (`rw`/`ro` + a dir marker — see
/// `windows_permission_string`).
#[cfg(not(unix))]
const PERMS_COL_WIDTH: usize = 3;
/// Below this pane width, the permissions column is dropped entirely
/// (before the name column starts getting mangled) even when
/// `show_permissions` is on — see `format_row`'s caller in `render`.
const MIN_WIDTH_FOR_PERMS_COL: usize =
    MARK_COL_WIDTH + SIZE_COL_WIDTH + MTIME_COL_WIDTH + PERMS_COL_WIDTH + 3 + 4;

/// Color/dim settings for rendering a pane, derived from `config::ColorsConfig`
/// by `ui/mod.rs`. Kept as its own small `Copy` struct (rather than passing
/// `&ColorsConfig` straight through) so this module stays independent of
/// `config.rs`.
#[derive(Debug, Clone, Copy)]
pub struct PaneColors {
    pub cursor: Color,
    pub cursor_inactive: Color,
    pub dim_inactive: bool,
    pub directory: Color,
    pub hidden: Color,
    pub executable: Color,
}

/// Renders `pane` into `area` and reports back the geometry mouse
/// hit-testing needs (`ui::mod::draw` stores this into `App::pane_layout`
/// every frame) — the *only* place that geometry is computed, so
/// `App::hit_test_row` and this function can never disagree about where a
/// row actually is on screen.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane,
    active: bool,
    colors: PaneColors,
    show_permissions: bool,
) -> PaneLayout {
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

    // The header (cwd + `[flt: ...]` tag) normally rides the border's own
    // title line for free — that's the `header_lines.len() == 1` case, and
    // costs the entry list nothing. Only when it doesn't fit in one line
    // does a second, real content row get carved out of `inner` for it
    // (see `wrap_header_lines`'s doc comment for the full policy,
    // including the last-resort left-truncation case).
    let title_budget = (area.width.saturating_sub(2) as usize).max(1); // account for the two border corners
    let mut title_source = match &pane.virtual_dir {
        Some(vd) => virtual_dir::header_label(vd),
        None => pane.cwd.display().to_string(),
    };
    if let Some(filter) = &pane.filter {
        title_source.push_str(&format!(" [flt: {}]", filter.raw));
    }
    let header_lines = wrap_header_lines(&title_source, title_budget);

    let block = Block::default()
        .title(header_lines[0].clone())
        .borders(Borders::ALL)
        .border_style(border_style);
    let mut inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(second_line) = header_lines.get(1)
        && inner.height > 0
    {
        let header_row = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        let style = if dim {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(second_line.clone()).style(style), header_row);
        inner = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height - 1,
        };
    }

    let items = pane.visible_entries();
    if items.is_empty() || inner.height == 0 {
        return PaneLayout {
            area,
            rows_area: inner,
            start: 0,
        };
    }

    let show_perms_col = show_permissions && inner.width as usize >= MIN_WIDTH_FOR_PERMS_COL;
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
            let text = format_row(item, inner.width as usize, marked, show_perms_col);
            let type_color = match item {
                VisibleItem::Parent => None,
                VisibleItem::Entry(e) => entry_type_color(
                    e.kind == EntryKind::Dir,
                    e.is_hidden,
                    e.is_executable,
                    colors,
                ),
            };
            let style = row_style(idx == cursor, marked, dim, cursor_color, type_color);
            ListItem::new(Line::styled(text, style))
        })
        .collect();

    frame.render_widget(List::new(rows), inner);

    PaneLayout {
        area,
        rows_area: inner,
        start,
    }
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
/// `type_color` (directory/hidden/executable — see `entry_type_color`) is
/// only ever applied to a row that's neither the cursor row nor marked:
/// the cursor row keeps its fixed bg+black fg regardless (a colored fg
/// would be invisible against its own bg half the time anyway), and a
/// marked row keeps its existing yellow — `executable`'s default color is
/// also yellow, and marked-ness is the more important thing to see at a
/// glance, so it wins the collision rather than the two competing. When
/// dimmed, the type color still applies (just with `DIM` layered on top —
/// "type colors + DIM combine" per the plan) rather than being dropped.
fn row_style(
    is_cursor: bool,
    marked: bool,
    dim: bool,
    cursor_color: Color,
    type_color: Option<Color>,
) -> Style {
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
    } else {
        let base = match type_color {
            Some(c) => Style::default().fg(c),
            None => Style::default(),
        };
        if dim {
            base.add_modifier(Modifier::DIM)
        } else {
            base
        }
    }
}

/// Which `[colors]` fg to use for a non-cursor, non-marked row, given its
/// type. Precedence when an entry is more than one of these at once (a
/// hidden, executable directory would qualify for all three): `hidden` >
/// `executable` > `directory` — hidden is the most likely thing a user is
/// scanning for, so it should never get masked by the other two. A plain
/// regular file returns `None` (keep the terminal's default fg).
fn entry_type_color(
    is_dir: bool,
    is_hidden: bool,
    is_executable: bool,
    colors: PaneColors,
) -> Option<Color> {
    if is_hidden {
        Some(colors.hidden)
    } else if is_executable {
        Some(colors.executable)
    } else if is_dir {
        Some(colors.directory)
    } else {
        None
    }
}

/// Formats the permissions column: unix `drwxr-xr-x` style from the raw
/// mode bits when available, otherwise a compact 3-char Windows fallback
/// (`rw`/`ro` + a `d`/`-` dir marker) built from `readonly` — dispatched
/// purely on whether `unix_mode` is `Some` (i.e. this data came from a
/// unix `read_dir_entries` call), not on `cfg(unix)`, so both branches stay
/// unit-testable from any host platform.
fn format_permissions(kind: EntryKind, unix_mode: Option<u32>, readonly: bool) -> String {
    match unix_mode {
        Some(mode) => unix_permission_string(kind, mode),
        None => windows_permission_string(kind, readonly),
    }
}

fn unix_permission_string(kind: EntryKind, mode: u32) -> String {
    let type_ch = match kind {
        EntryKind::Dir => 'd',
        EntryKind::Symlink => 'l',
        EntryKind::File => '-',
    };
    const BITS: [(u32, char); 9] = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    let mut s = String::with_capacity(10);
    s.push(type_ch);
    for (mask, ch) in BITS {
        s.push(if mode & mask != 0 { ch } else { '-' });
    }
    s
}

fn windows_permission_string(kind: EntryKind, readonly: bool) -> String {
    let rw = if readonly { "ro" } else { "rw" };
    let marker = if kind == EntryKind::Dir { 'd' } else { '-' };
    format!("{rw}{marker}")
}

/// Wraps a pane header (`cwd` + optional `[flt: ...]` tag) into at most 2
/// display lines that fit within `width` columns each: fits in one line ->
/// that one line (the common case — rendered on the border's title, no
/// extra row spent); doesn't fit but 2 plain-wrapped lines would hold the
/// whole thing -> exactly that, greedily filling line 1 first, no
/// characters lost; still doesn't fit even across 2 lines -> falls back to
/// `truncate_left` (keep the meaningful tail, ellipsis at the very front)
/// on the *whole* 2-line budget, then re-wraps that already-fitting result
/// across the same 2 lines. Grapheme/width-safe throughout (never splits a
/// wide character).
fn wrap_header_lines(source: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    if UnicodeWidthStr::width(source) <= width {
        return vec![source.to_string()];
    }
    let (line1, rest) = greedy_wrap_one_line(source, width);
    if UnicodeWidthStr::width(rest.as_str()) <= width {
        return vec![line1, rest];
    }
    // Even 2 rows of plain wrapping can't hold it: fall back to truncating
    // the whole thing from the left (keeping the tail) to fit exactly a
    // 2-row budget, then re-wrap that shorter, now-fitting string.
    let truncated = truncate_left(source, width * 2);
    let (line1, line2) = greedy_wrap_one_line(&truncated, width);
    vec![line1, line2]
}

/// Splits `s` into `(first `width`-columns-worth, remainder)` on grapheme
/// boundaries, never exceeding `width` display columns in the first part.
fn greedy_wrap_one_line(s: &str, width: usize) -> (String, String) {
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

/// `show_perms_col` is decided once per frame by the caller (`render`, from
/// the pane's actual width — see `MIN_WIDTH_FOR_PERMS_COL`) so every row in
/// a frame agrees on whether the column exists at all; when it does, `..`
/// gets a blank column of the same width rather than omitting it, so every
/// row's other columns stay aligned.
fn format_row(item: &VisibleItem<'_>, width: usize, marked: bool, show_perms_col: bool) -> String {
    let (name, size_text, mtime_text, perms_text) = match item {
        VisibleItem::Parent => (
            "..".to_string(),
            String::new(),
            String::new(),
            " ".repeat(PERMS_COL_WIDTH),
        ),
        VisibleItem::Entry(e) => {
            let size_text = if e.kind == EntryKind::Dir {
                "<DIR>".to_string()
            } else {
                human_size(e.size)
            };
            let mtime_text = e.mtime.map(format_mtime).unwrap_or_default();
            let perms_text = format_permissions(e.kind, e.unix_mode, e.readonly);
            (e.name.clone(), size_text, mtime_text, perms_text)
        }
    };

    let mark_col = if marked { "*" } else { " " };
    let mut reserved = MARK_COL_WIDTH + SIZE_COL_WIDTH + MTIME_COL_WIDTH + 2; // two single-space separators
    if show_perms_col {
        reserved += PERMS_COL_WIDTH + 1; // one more separator
    }
    let name_width = width.saturating_sub(reserved).max(3);

    let name_col = pad_right_display(&truncate_right(&name, name_width), name_width);
    let size_col = pad_left_display(&size_text, SIZE_COL_WIDTH);
    let mtime_col = pad_left_display(&mtime_text, MTIME_COL_WIDTH);

    if show_perms_col {
        format!("{mark_col}{name_col} {perms_text} {size_col} {mtime_col}")
    } else {
        format!("{mark_col}{name_col} {size_col} {mtime_col}")
    }
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
        let style = row_style(true, false, true, Color::White, None);
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
        let style = row_style(true, false, false, Color::Rgb(0x90, 0xEE, 0x90), None);
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(style.bg, Some(Color::Rgb(0x90, 0xEE, 0x90)));
    }

    #[test]
    fn cursor_row_style_ignores_type_color() {
        // Spec: "Cursor row keeps its bg+black fg (type color not applied
        // there)".
        let style = row_style(true, false, false, Color::White, Some(Color::Cyan));
        assert_eq!(style.fg, Some(Color::Black));
    }

    #[test]
    fn marked_row_style_dims_when_pane_is_dimmed() {
        let style = row_style(false, true, true, Color::White, None);
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn marked_row_style_wins_over_type_color() {
        // Spec: marked (yellow) keeps priority over a type color, even
        // executable's own default yellow — this is what actually resolves
        // that collision.
        let style = row_style(false, true, false, Color::White, Some(Color::Yellow));
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn plain_row_style_dims_when_pane_is_dimmed() {
        let style = row_style(false, false, true, Color::White, None);
        assert!(style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.bg, None);
        assert_eq!(style.fg, None);
    }

    #[test]
    fn plain_row_style_is_bare_when_not_dimmed() {
        let style = row_style(false, false, false, Color::White, None);
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style, Style::default());
    }

    #[test]
    fn plain_row_style_applies_type_color_and_combines_with_dim() {
        let style = row_style(false, false, true, Color::White, Some(Color::Cyan));
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn entry_type_color_precedence_is_hidden_then_executable_then_directory() {
        let colors = test_colors();
        assert_eq!(
            entry_type_color(true, true, true, colors),
            Some(colors.hidden)
        );
        assert_eq!(
            entry_type_color(false, false, true, colors),
            Some(colors.executable)
        );
        assert_eq!(
            entry_type_color(true, false, false, colors),
            Some(colors.directory)
        );
        assert_eq!(entry_type_color(false, false, false, colors), None);
    }

    fn test_colors() -> PaneColors {
        PaneColors {
            cursor: Color::Rgb(0x90, 0xEE, 0x90),
            cursor_inactive: Color::White,
            dim_inactive: true,
            directory: Color::Cyan,
            hidden: Color::Red,
            executable: Color::Yellow,
        }
    }

    #[test]
    fn unix_permission_string_formats_dir_and_bits() {
        assert_eq!(unix_permission_string(EntryKind::Dir, 0o755), "drwxr-xr-x");
        assert_eq!(unix_permission_string(EntryKind::File, 0o644), "-rw-r--r--");
        assert_eq!(
            unix_permission_string(EntryKind::Symlink, 0o777),
            "lrwxrwxrwx"
        );
        assert_eq!(unix_permission_string(EntryKind::File, 0o000), "----------");
    }

    #[test]
    fn windows_permission_string_is_compact() {
        assert_eq!(windows_permission_string(EntryKind::File, false), "rw-");
        assert_eq!(windows_permission_string(EntryKind::File, true), "ro-");
        assert_eq!(windows_permission_string(EntryKind::Dir, false), "rwd");
    }

    #[test]
    fn format_permissions_dispatches_on_unix_mode_presence() {
        assert_eq!(
            format_permissions(EntryKind::File, Some(0o644), false),
            "-rw-r--r--"
        );
        assert_eq!(format_permissions(EntryKind::File, None, true), "ro-");
    }

    #[test]
    fn wrap_header_lines_returns_one_line_when_it_fits() {
        assert_eq!(wrap_header_lines("/short/path", 40), vec!["/short/path"]);
    }

    #[test]
    fn wrap_header_lines_splits_into_two_when_it_fits_across_two() {
        let source = "a".repeat(30);
        let lines = wrap_header_lines(&source, 20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len() + lines[1].len(), 30);
        assert_eq!(lines[0], "a".repeat(20));
        assert_eq!(lines[1], "a".repeat(10));
    }

    #[test]
    fn wrap_header_lines_truncates_from_the_left_when_even_two_lines_are_not_enough() {
        let source = "a".repeat(100);
        let lines = wrap_header_lines(&source, 10);
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 10);
        }
        // The very first character of the whole 2-row budget must be the
        // ellipsis — "only truncating (left, with …) if even 2 rows can't
        // fit".
        assert!(lines[0].starts_with('…'));
    }

    #[test]
    fn wrap_header_lines_never_splits_a_japanese_grapheme() {
        let source = "日本語".repeat(10); // 30 graphemes, width 60
        let lines = wrap_header_lines(&source, 20);
        for line in &lines {
            assert!(UnicodeWidthStr::width(line.as_str()) <= 20);
        }
        let joined: String = lines.join("");
        for g in joined.trim_start_matches('…').graphemes(true) {
            assert!(source.contains(g));
        }
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
        let row = format_row(&VisibleItem::Parent, 40, false, false);
        assert!(row.trim_start().starts_with(".."));
    }

    #[test]
    fn format_row_marks_prepend_asterisk() {
        let row = format_row(&VisibleItem::Parent, 40, true, false);
        assert!(row.starts_with('*'));
    }

    #[test]
    fn format_row_drops_permissions_column_when_show_perms_col_is_false() {
        // Both rows fill exactly `width` columns either way (the name
        // column absorbs whatever the permissions column doesn't use), so
        // the meaningful check is *content*, not raw length: the
        // permissions text must appear only when the column is shown.
        use std::path::PathBuf;
        use std::time::SystemTime;
        let entry = FsEntry {
            name: "run.sh".to_string(),
            path: PathBuf::from("run.sh"),
            kind: EntryKind::File,
            size: 10,
            mtime: Some(SystemTime::UNIX_EPOCH),
            is_hidden: false,
            unix_mode: Some(0o755),
            readonly: false,
            is_executable: true,
        };
        let item = VisibleItem::Entry(&entry);

        let with = format_row(&item, 60, false, true);
        let without = format_row(&item, 60, false, false);
        assert!(with.contains("rwxr-xr-x"));
        assert!(!without.contains("rwxr-xr-x"));
        assert_eq!(UnicodeWidthStr::width(with.as_str()), 60);
        assert_eq!(UnicodeWidthStr::width(without.as_str()), 60);
    }
}
