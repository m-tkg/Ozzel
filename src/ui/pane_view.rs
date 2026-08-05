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
use unicode_width::UnicodeWidthStr;

use crate::app::PaneLayout;
use crate::color::dim_color;
use crate::entry::EntryKind;
#[cfg(test)]
use crate::entry::FsEntry;
use crate::pane::{Pane, VisibleItem};
use crate::ui::text;
use crate::virtual_dir;
#[cfg(test)]
use unicode_segmentation::UnicodeSegmentation;

/// How much the inactive pane's cursor row background darkens toward
/// black when the pane is dimmed (`dim_inactive`, default on) — see
/// `dim_color`. `0.5` came out of eyeballing it in a real terminal
/// (tmux): perceptibly, unambiguously darker than the active pane's
/// cursor at every named/hex color tried, without going so dark the row
/// stops reading as "this is still the cursor."
const INACTIVE_CURSOR_DIM_FACTOR: f32 = 0.5;

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
    // When the pane is actually dimmed, the cursor row's background is
    // pre-darkened *here* (an approximate-RGB scale toward black — see
    // `dim_color`) rather than left at `cursor_inactive`'s full
    // brightness and handed to the terminal's SGR "dim" attribute the way
    // every other dimmed row is (`row_style`'s `Modifier::DIM`, still
    // used for everything else): most terminals barely apply "dim" to a
    // *background* color at all, which is exactly why the previous
    // approach left the inactive cursor looking just as bright as the
    // active one.
    let cursor_color = if active {
        colors.cursor
    } else if dim {
        dim_color(colors.cursor_inactive, INACTIVE_CURSOR_DIM_FACTOR)
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
                VisibleItem::Entry(e) => {
                    entry_type_color(e.is_dir_like(), e.is_hidden, e.is_executable, colors)
                }
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
/// The cursor row's own darkening is carried entirely by `cursor_color`
/// (already pre-darkened by the caller, `render`, via `dim_color` when
/// `dim` is true) rather than by adding `Modifier::DIM` here the way
/// every other dimmed row does: SGR "dim"/faint barely affects — often
/// doesn't affect at all — a terminal's *background* color, which is
/// exactly what made an "inactive but dimmed" cursor look just as bright
/// as the active one before. `BOLD` (the cursor row's undimmed weight)
/// drops out when dimmed rather than combining with the (no longer used
/// here) `DIM`: in the ANSI/VT100 model both share the same terminal
/// "intensity" slot (SGR 1 = bold, SGR 2 = faint, mutually exclusive, not
/// independently stackable bits the way ratatui's `Modifier` bitflags
/// suggest) — asking for both at once is at the mercy of which one a given
/// terminal happens to apply last, an ambiguity worth avoiding regardless
/// of which one is actually driving the dimming now.
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
        // `cursor_color` already carries the darkening (see `render`) —
        // no `Modifier::DIM` here, just drop the undimmed weight.
        let base = Style::default().bg(cursor_color).fg(Color::Black);
        if dim {
            base
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
    let (line1, rest) = text::take_display_prefix(source, width);
    if UnicodeWidthStr::width(rest.as_str()) <= width {
        return vec![line1, rest];
    }
    // Even 2 rows of plain wrapping can't hold it: fall back to truncating
    // the whole thing from the left (keeping the tail) to fit exactly a
    // 2-row budget, then re-wrap that shorter, now-fitting string.
    let truncated = text::truncate_left(source, width * 2);
    let (line1, line2) = text::take_display_prefix(&truncated, width);
    vec![line1, line2]
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
            let size_text = if e.is_dir_like() {
                "<DIR>".to_string()
            } else {
                human_size(e.size)
            };
            let mtime_text = e.mtime.map(format_mtime).unwrap_or_default();
            let perms_text = format_permissions(e.kind, e.unix_mode, e.readonly);
            // `ls -F`'s convention: a trailing `@` marks a symlink,
            // regardless of what it resolves to — the size column/color
            // (both driven by `is_dir_like`) already say "this behaves
            // like a directory"; this says "...but it's actually a link"
            // (dangling or not). Appended only for display — `e.name`
            // itself (sorting, filtering, prefix-jump, rename prefill,
            // marks-by-path) never sees this suffix.
            let name = if e.kind == EntryKind::Symlink {
                format!("{}@", e.name)
            } else {
                e.name.clone()
            };
            (name, size_text, mtime_text, perms_text)
        }
    };

    let mark_col = if marked { "*" } else { " " };
    let mut reserved = MARK_COL_WIDTH + SIZE_COL_WIDTH + MTIME_COL_WIDTH + 2; // two single-space separators
    if show_perms_col {
        reserved += PERMS_COL_WIDTH + 1; // one more separator
    }
    let name_width = width.saturating_sub(reserved).max(3);

    let name_col = text::pad_right(&text::truncate_right(&name, name_width), name_width);
    let size_col = text::pad_left(&size_text, SIZE_COL_WIDTH);
    let mtime_col = text::pad_left(&mtime_text, MTIME_COL_WIDTH);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_row_style_dims_when_pane_is_dimmed() {
        // The bug-fix under test: the inactive pane's cursor row must dim
        // along with the rest of the pane, not stay exempt. Unlike every
        // other row, the darkening isn't `Modifier::DIM` here — SGR
        // "dim"/faint barely affects a terminal's *background* color, so
        // the row would look just as bright otherwise. `row_style` trusts
        // `cursor_color` to already be pre-darkened by the caller (see
        // `render`'s `dim_color` call and
        // `cursor_row_style_background_is_actually_darker_not_just_dim_flagged`
        // below for that half of the fix) — its own job here is just to
        // drop `Modifier::DIM`/`BOLD` for this row, since applying `DIM`
        // on top would be redundant with (and fight) the already-darker
        // color.
        let style = row_style(true, false, true, Color::Rgb(128, 128, 128), None);
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert_eq!(style.bg, Some(Color::Rgb(128, 128, 128)));
        assert_eq!(style.fg, Some(Color::Black));
        // BOLD and DIM share the same ANSI "intensity" slot on a real
        // terminal (SGR 1 vs SGR 2, mutually exclusive) — asking for both
        // is unreliable, so the dimmed cursor row must drop BOLD too.
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn cursor_row_style_background_is_actually_darker_not_just_dim_flagged() {
        // The other half of the fix, at the `render`-level call site
        // rather than `row_style` itself: when the pane is dimmed, the
        // color `render` computes and passes in as `cursor_color` must
        // already be `dim_color(cursor_inactive, ...)`, genuinely darker
        // RGB — not the configured `cursor_inactive` at full brightness
        // relying on a `DIM` modifier that terminals mostly ignore for
        // backgrounds. This directly exercises `dim_color` the same way
        // `render` does, rather than re-deriving the expected value.
        let cursor_inactive = Color::White;
        let dimmed = dim_color(cursor_inactive, INACTIVE_CURSOR_DIM_FACTOR);
        assert_ne!(
            dimmed, cursor_inactive,
            "the dimmed cursor color must differ from the configured one"
        );
        assert_eq!(dimmed, Color::Rgb(128, 128, 128));
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
        //
        // `format_row`'s column-width math reserves exactly
        // `PERMS_COL_WIDTH` for the permissions text, which is only
        // accurate when `unix_mode`'s presence agrees with which of
        // `format_permissions`'s two branches — and therefore which
        // width — actually fires (see `entry.rs`, which only ever
        // populates `unix_mode` on unix). Build the entry the same way so
        // this stays true on every platform, and derive the expected
        // permissions text from `format_permissions` itself rather than a
        // hard-coded, platform-specific string.
        use std::path::PathBuf;
        use std::time::SystemTime;
        #[cfg(unix)]
        let (unix_mode, readonly) = (Some(0o755), false);
        #[cfg(not(unix))]
        let (unix_mode, readonly) = (None, false);
        let entry = FsEntry {
            name: "run.sh".to_string(),
            name_lower: "run.sh".to_string(),
            ext_lower: "sh".to_string(),
            path: PathBuf::from("run.sh"),
            kind: EntryKind::File,
            size: 10,
            mtime: Some(SystemTime::UNIX_EPOCH),
            is_hidden: false,
            unix_mode,
            readonly,
            is_executable: true,
            symlink_target: None,
        };
        let item = VisibleItem::Entry(&entry);
        let expected_perms = format_permissions(EntryKind::File, unix_mode, readonly);

        let with = format_row(&item, 60, false, true);
        let without = format_row(&item, 60, false, false);
        assert!(with.contains(&expected_perms));
        assert!(!without.contains(&expected_perms));
        assert_eq!(UnicodeWidthStr::width(with.as_str()), 60);
        assert_eq!(UnicodeWidthStr::width(without.as_str()), 60);
    }

    fn symlink_entry(name: &str, target: crate::entry::SymlinkTarget) -> FsEntry {
        use std::path::PathBuf;
        let (name_lower, ext_lower) = crate::entry::lower_keys(name);
        FsEntry {
            name: name.to_string(),
            name_lower,
            ext_lower,
            path: PathBuf::from(name),
            kind: EntryKind::Symlink,
            size: 11,
            mtime: None,
            is_hidden: false,
            unix_mode: None,
            readonly: false,
            is_executable: false,
            symlink_target: Some(target),
        }
    }

    #[test]
    fn format_row_shows_dir_size_for_a_directory_symlink_and_appends_the_link_marker() {
        let entry = symlink_entry("mylink", crate::entry::SymlinkTarget::Dir);
        let row = format_row(&VisibleItem::Entry(&entry), 60, false, false);
        assert!(row.contains("<DIR>"), "row: {row:?}");
        assert!(row.contains("mylink@"), "row: {row:?}");
    }

    #[test]
    fn format_row_shows_human_size_for_a_file_symlink_and_still_appends_the_link_marker() {
        let entry = symlink_entry("mylink", crate::entry::SymlinkTarget::File);
        let row = format_row(&VisibleItem::Entry(&entry), 60, false, false);
        assert!(!row.contains("<DIR>"), "row: {row:?}");
        assert!(row.contains("mylink@"), "row: {row:?}");
    }

    #[test]
    fn format_row_never_appends_the_link_marker_to_a_real_directory_or_file() {
        let dir = entry_for_test("adir", EntryKind::Dir);
        let file = entry_for_test("afile.txt", EntryKind::File);
        assert!(!format_row(&VisibleItem::Entry(&dir), 60, false, false).contains('@'));
        assert!(!format_row(&VisibleItem::Entry(&file), 60, false, false).contains('@'));
    }

    fn entry_for_test(name: &str, kind: EntryKind) -> FsEntry {
        use std::path::PathBuf;
        let (name_lower, ext_lower) = crate::entry::lower_keys(name);
        FsEntry {
            name: name.to_string(),
            name_lower,
            ext_lower,
            path: PathBuf::from(name),
            kind,
            size: 0,
            mtime: None,
            is_hidden: false,
            unix_mode: None,
            readonly: false,
            is_executable: false,
            symlink_target: None,
        }
    }

    #[test]
    fn entry_type_color_treats_a_directory_symlink_like_a_directory() {
        let colors = PaneColors {
            cursor: Color::White,
            cursor_inactive: Color::Gray,
            dim_inactive: false,
            directory: Color::Blue,
            hidden: Color::DarkGray,
            executable: Color::Green,
        };
        let link = symlink_entry("mylink", crate::entry::SymlinkTarget::Dir);
        assert_eq!(
            entry_type_color(
                link.is_dir_like(),
                link.is_hidden,
                link.is_executable,
                colors
            ),
            Some(Color::Blue)
        );
    }

    #[test]
    fn render_inactive_dimmed_cursor_row_background_is_darker_than_active() {
        // End-to-end through the real `render` call site (not just the
        // pure `row_style`/`dim_color` units above): the actual rendered
        // cell's background color for the inactive pane's cursor row must
        // come out darker than the active pane's, given the same
        // `cursor`/`cursor_inactive` configured color.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        pane.cursor = 0; // the ".." row — always present, simplest to target

        let colors = PaneColors {
            cursor: Color::White,
            cursor_inactive: Color::White,
            dim_inactive: true,
            directory: Color::Cyan,
            hidden: Color::Red,
            executable: Color::Yellow,
        };

        let render_cursor_bg = |active: bool| {
            // Wide enough that the header (cwd path) never needs the
            // second-line wrap (see `wrap_header_lines`) regardless of how
            // long the tempdir's own path happens to be on this machine —
            // otherwise the cursor row wouldn't reliably be at a fixed
            // row index.
            let backend = TestBackend::new(200, 6);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &pane, active, colors, false);
                })
                .unwrap();
            // Row 0 is the top border; the cursor (".." ) row is the first
            // content row, row 1. Column 1 is just inside the left border.
            terminal.backend().buffer()[(1, 1)].bg
        };

        let active_bg = render_cursor_bg(true);
        let inactive_bg = render_cursor_bg(false);
        assert_eq!(
            active_bg,
            Color::White,
            "active pane's cursor keeps the configured color at full brightness"
        );
        assert_eq!(
            inactive_bg,
            dim_color(Color::White, INACTIVE_CURSOR_DIM_FACTOR),
            "inactive+dimmed pane's cursor background must be the darkened color"
        );
        assert_ne!(
            active_bg, inactive_bg,
            "the inactive cursor row must render genuinely darker, not identically bright"
        );
    }
}
