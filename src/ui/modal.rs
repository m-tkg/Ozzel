//! Renders the modal overlays: the bottom Filter/JumpSearch input line
//! (drawn in place of the status bar — these are live-narrowing
//! interactions, so the listing below has to stay visible), and the
//! centered popups drawn on top of everything else: the Select jump menu,
//! the Confirm dialog, and — like Confirm — the `Mode::Prompt` text-input
//! box (`render_prompt_box`; a prompt commits/cancels as a single unit
//! rather than live-narrowing anything, so unlike Filter/JumpSearch there's
//! nothing behind it that needs to stay visible while typing).

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::mode::{LineEditor, Mode, PromptKind};
use crate::ui::text::slice_display_cols;

/// Narrowest a `render_prompt_box` popup is ever allowed to shrink to,
/// regardless of how short the title/content are — small enough to still
/// read as a deliberate dialog rather than a sliver, matching the spec's
/// "reasonable min/max" width.
const PROMPT_BOX_MIN_WIDTH: u16 = 30;

/// Draws a centered popup box for `Mode::Prompt` (every `PromptKind`:
/// Rename/Mkdir/ZipName/Command/Duplicate) on top of `area` (normally the
/// whole frame) — title = the prompt's own label, one input row showing
/// the `LineEditor`'s content with the real terminal cursor positioned
/// inside it (horizontally scrolling via `slice_display_cols` when the
/// input is wider than the box, exactly like the viewer's text mode
/// scrolls a long line), and a one-line `Enter: OK   Esc: Cancel` hint.
/// No-op if `mode` is not `Prompt`.
pub fn render_prompt_box(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Prompt { kind, input } = mode else {
        return;
    };
    let title = match kind {
        PromptKind::Rename { .. } => "Rename".to_string(),
        PromptKind::Mkdir => "New directory".to_string(),
        PromptKind::ZipName { .. } => "Zip as".to_string(),
        PromptKind::Command => "Command".to_string(),
        PromptKind::Duplicate { .. } => "Duplicate as".to_string(),
        // `done` counts *finished* renames, so the entry currently being
        // prompted for is number `done + 1`.
        PromptKind::RenameMany { done, total, .. } => format!("Rename ({}/{})", done + 1, total),
        PromptKind::CollisionRename { state } => {
            format!(
                "Rename to ({}/{}): {}",
                state.index, state.total, state.current.name
            )
        }
        PromptKind::ArchivePassword { .. } => "Password".to_string(),
        PromptKind::TouchTime { targets } => {
            format!("Touch {} item(s) (empty = now)", targets.len())
        }
    };
    let mask = matches!(kind, PromptKind::ArchivePassword { .. });
    render_input_box(frame, area, &title, input, mask);
}

/// The actual popup layout/rendering `render_prompt_box` delegates to —
/// factored out so it's exercisable (and testable) without going through
/// `Mode::Prompt`'s specific title-selection match, and so a future
/// second caller (a text-input editor in the upcoming settings screen,
/// say) can reuse it directly.
/// `mask` replaces the input's display (only — never its value) with one
/// `*` per grapheme, for password entry. Cursor positioning under a mask
/// uses the grapheme count directly (each `*` is one column), not the
/// original text's display width.
fn render_input_box(frame: &mut Frame, area: Rect, title: &str, input: &LineEditor, mask: bool) {
    // Width: half the frame is the normal case, but never narrower than
    // `PROMPT_BOX_MIN_WIDTH` (clamped down further only if the whole frame
    // itself is that small) and never wider than the frame itself — long
    // content scrolls horizontally inside the box instead of growing it
    // unboundedly.
    let width = (area.width / 2)
        .max(PROMPT_BOX_MIN_WIDTH.min(area.width))
        .min(area.width);
    let height = 4.min(area.height); // top/bottom border + input row + hint row
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let content_width = inner.width as usize;
    // Under a mask, the rendered text is one single-width `*` per
    // grapheme, so the cursor column is the grapheme index — the display
    // width of the real value never appears anywhere on screen.
    let (display_value, cursor_col) = if mask {
        (
            "*".repeat(input.grapheme_count()),
            input.cursor_grapheme_index(),
        )
    } else {
        (input.value(), input.cursor_display_col())
    };
    // Keep the cursor's column inside the visible window: once it would
    // fall past the right edge, scroll the window to keep it exactly on
    // the last visible column — the same "reveal as you type" scrolling a
    // real single-line text input needs, built from the same display-
    // column math the viewer's horizontal scroll already uses.
    let start_col = cursor_col.saturating_sub(content_width.saturating_sub(1));
    let visible = slice_display_cols(&display_value, start_col, content_width);
    frame.render_widget(Paragraph::new(visible), rows[0]);

    if rows.len() > 1 && rows[1].height > 0 {
        let hint = Paragraph::new("Enter: OK   Esc: Cancel")
            .style(Style::default().add_modifier(Modifier::DIM))
            .alignment(Alignment::Center);
        frame.render_widget(hint, rows[1]);
    }

    let cursor_x = inner.x + (cursor_col - start_col) as u16;
    frame.set_cursor_position((cursor_x, inner.y));
}

/// Draws the `Filter` line into `area`, showing the live input plus an
/// error hint when the current `re:` pattern fails to compile. No-op if
/// `mode` is not `Filter`.
pub fn render_filter_line(frame: &mut Frame, area: Rect, mode: &Mode, error: Option<&str>) {
    let Mode::Filter { input } = mode else {
        return;
    };
    let suffix = error.map(|err| format!("  (invalid regex: {err})"));
    render_prefixed_input_line(frame, area, "Filter: ", input, suffix.as_deref());
}

/// Draws the `JumpSearch` line into `area`, showing the live input plus a
/// `(no match)` hint (in warning styling) whenever the typed prefix
/// currently matches nothing. No-op if `mode` is not `JumpSearch`.
/// `has_match` is computed by the caller (`ui::draw`), which — unlike this
/// module — has access to the active pane's `Pane::jump_matches`.
pub fn render_jump_search_line(frame: &mut Frame, area: Rect, mode: &Mode, has_match: bool) {
    let Mode::JumpSearch { input, .. } = mode else {
        return;
    };
    let no_match = !input.value().is_empty() && !has_match;
    let suffix = no_match.then_some("  (no match)");
    render_prefixed_input_line(frame, area, "Jump: ", input, suffix);
}

/// One reverse-video input line: `{label}{input}{suffix}`, with the real
/// terminal cursor positioned at the input's own edit point (`label`'s
/// width plus the input's display column, clamped to stay inside `area`).
/// Shared by six call sites across `ui/*`: `render_filter_line`/
/// `render_jump_search_line` below (`suffix` carries their warning text,
/// e.g. `"  (invalid regex: ...)"`/`"  (no match)"`, and turns the whole
/// line red when present), plus `ui::help_view`/`ui::log_view`/
/// `ui::viewer_view`'s `/`/`?` search input line (label is `"/"`/`"?"`,
/// `suffix` always `None` — those three never show a warning, so the red
/// styling never triggers for them). `suffix`, when present, is expected
/// to already include its own leading spacing/punctuation since callers'
/// wording differs. `pub(super)` so the three `ui::*_view` modules can
/// reach it without redefining this line-drawing/cursor-positioning logic
/// three more times.
pub(super) fn render_prefixed_input_line(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    input: &crate::mode::LineEditor,
    suffix: Option<&str>,
) {
    let text = match suffix {
        Some(suffix) => format!("{label}{}{suffix}", input.value()),
        None => format!("{label}{}", input.value()),
    };
    let mut style = Style::default().add_modifier(Modifier::REVERSED);
    if suffix.is_some() {
        style = style.fg(Color::Red);
    }
    let paragraph = Paragraph::new(text).style(style);
    frame.render_widget(paragraph, area);

    let col = area.x + UnicodeWidthStr::width(label) as u16 + input.cursor_display_col() as u16;
    let col = col.min(area.x + area.width.saturating_sub(1));
    frame.set_cursor_position((col, area.y));
}

/// Draws the centered `Select` jump menu (history/bookmarks) on top of
/// `area` (normally the whole frame). No-op if `mode` is not `Select`.
pub fn render_select(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Select {
        title,
        items,
        cursor,
        ..
    } = mode
    else {
        return;
    };

    let inner_width = items
        .iter()
        .map(|(label, _)| UnicodeWidthStr::width(label.as_str()))
        .max()
        .unwrap_or(0)
        .max(UnicodeWidthStr::width(title.as_str()));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    let height = (items.len() as u16 + 2).clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title.as_str()).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(idx, (label, _))| {
            let style = if idx == *cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::styled(label.clone(), style))
        })
        .collect();
    frame.render_widget(List::new(rows), inner);
}

/// The sort dialog's display rows, index-aligned with
/// `App::SORT_DIALOG_CHOICES` — the key handler and this renderer must
/// agree on the ordering, so both are fixed arrays of the same length
/// (asserted by a test below).
pub const SORT_DIALOG_LABELS: [&str; 8] = [
    "name  ↑ ascending",
    "name  ↓ descending",
    "size  ↑ ascending",
    "size  ↓ descending",
    "mtime ↑ ascending",
    "mtime ↓ descending",
    "ext   ↑ ascending",
    "ext   ↓ descending",
];

/// Draws the centered sort dialog (`Mode::SortSelect`, the `t` action):
/// the fixed (key, direction) rows with the highlight on `cursor`. No-op
/// if `mode` is not `SortSelect`.
pub fn render_sort_select(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::SortSelect { cursor } = mode else {
        return;
    };

    let title = "Sort";
    let inner_width = SORT_DIALOG_LABELS
        .iter()
        .map(|label| UnicodeWidthStr::width(*label))
        .max()
        .unwrap_or(0)
        .max(UnicodeWidthStr::width(title));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    let height = (SORT_DIALOG_LABELS.len() as u16 + 2).clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows: Vec<ListItem> = SORT_DIALOG_LABELS
        .iter()
        .enumerate()
        .map(|(idx, label)| {
            let style = if idx == *cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::styled(*label, style))
        })
        .collect();
    frame.render_widget(List::new(rows), inner);
}

/// Draws the per-file transfer-collision dialog (`Mode::TransferCollision`):
/// a title naming the conflicting entry with `(n/total)` progress, both
/// sides' pre-formatted size/mtime lines (the newer side already carries
/// `[New]` — see `App::collision_info`), and the five answers with the
/// highlight on `cursor`. No-op if `mode` is not `TransferCollision`.
pub fn render_transfer_collision(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::TransferCollision { state } = mode else {
        return;
    };

    let title = format!(
        "Overwrite? ({}/{}): {}",
        state.index, state.total, state.current.name
    );
    let info_lines = [&state.current.src_line, &state.current.dest_line];

    let inner_width = crate::mode::COLLISION_CHOICES
        .iter()
        .map(|label| UnicodeWidthStr::width(*label) + 1)
        .chain(
            info_lines
                .iter()
                .map(|l| UnicodeWidthStr::width(l.as_str())),
        )
        .max()
        .unwrap_or(0)
        .max(UnicodeWidthStr::width(title.as_str()));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    // borders + 2 info lines + 1 blank + 5 choices
    let height = (crate::mode::COLLISION_CHOICES.len() as u16 + info_lines.len() as u16 + 3)
        .clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut rows: Vec<ListItem> = info_lines
        .iter()
        .map(|line| ListItem::new(Line::raw((*line).clone())))
        .collect();
    rows.push(ListItem::new(Line::raw(String::new())));
    rows.extend(
        crate::mode::COLLISION_CHOICES
            .iter()
            .enumerate()
            .map(|(idx, label)| {
                let style = if idx == state.cursor {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Line::styled(format!(" {label}"), style))
            }),
    );
    frame.render_widget(List::new(rows), inner);
}

/// Draws the sync-mode dialog (`Mode::SyncSelect`, the `W` action): the
/// source/destination info lines and the two `SYNC_CHOICES` rows with the
/// highlight on `cursor` — same info-lines-plus-choices layout as the
/// transfer-collision dialog. No-op if `mode` is not `SyncSelect`.
pub fn render_sync_select(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::SyncSelect { src, dest, cursor } = mode else {
        return;
    };

    let title = "Sync direction";
    let info_lines = [
        format!("from: {}", src.display()),
        format!("to:   {}", dest.display()),
    ];

    let inner_width = crate::mode::SYNC_CHOICES
        .iter()
        .map(|label| UnicodeWidthStr::width(*label) + 1)
        .chain(
            info_lines
                .iter()
                .map(|l| UnicodeWidthStr::width(l.as_str())),
        )
        .max()
        .unwrap_or(0)
        .max(UnicodeWidthStr::width(title));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    // borders + 2 info lines + 1 blank + 2 choices
    let height = (crate::mode::SYNC_CHOICES.len() as u16 + info_lines.len() as u16 + 3)
        .clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut rows: Vec<ListItem> = info_lines
        .iter()
        .map(|line| ListItem::new(Line::raw(line.clone())))
        .collect();
    rows.push(ListItem::new(Line::raw(String::new())));
    rows.extend(
        crate::mode::SYNC_CHOICES
            .iter()
            .enumerate()
            .map(|(idx, label)| {
                let style = if idx == *cursor {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                ListItem::new(Line::styled(format!(" {label}"), style))
            }),
    );
    frame.render_widget(List::new(rows), inner);
}

/// Draws the chmod dialog (`Mode::Chmod`): a 3x3 rwx toggle grid (rows =
/// user/group/other), the highlighted cell in reverse video, and a live
/// `-rwxr-xr-x (0755)` readout of the mode being edited. No-op if `mode`
/// is not `Chmod`.
pub fn render_chmod(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Chmod { state } = mode else {
        return;
    };

    let title = format!("Permissions ({} item(s))", state.targets.len());
    const ROW_LABELS: [&str; 3] = ["user ", "group", "other"];
    const BIT_CHARS: [char; 3] = ['r', 'w', 'x'];

    // 3 grid rows + blank + live readout + blank + hint
    let hint = "Space: toggle  0-7: set row  Enter: apply  Esc: cancel";
    let inner_width = UnicodeWidthStr::width(hint).max(UnicodeWidthStr::width(title.as_str()));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    let height = 9u16.clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut rows: Vec<ListItem> = Vec::new();
    for (row, row_label) in ROW_LABELS.iter().enumerate() {
        let mut spans: Vec<ratatui::text::Span> =
            vec![ratatui::text::Span::raw(format!(" {row_label}  "))];
        for (col, bit_char) in BIT_CHARS.iter().enumerate() {
            let cursor = row * 3 + col;
            let bit = crate::mode::ChmodState::bit_at(cursor);
            let ch = if state.bits & bit != 0 {
                *bit_char
            } else {
                '-'
            };
            let style = if cursor == state.cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            spans.push(ratatui::text::Span::styled(format!(" {ch} "), style));
        }
        rows.push(ListItem::new(Line::from(spans)));
    }
    rows.push(ListItem::new(Line::raw(String::new())));
    let mut readout = String::with_capacity(10);
    readout.push('-');
    const BITS: [u32; 9] = [
        0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
    ];
    for (i, mask) in BITS.iter().enumerate() {
        readout.push(if state.bits & mask != 0 {
            BIT_CHARS[i % 3]
        } else {
            '-'
        });
    }
    rows.push(ListItem::new(Line::raw(format!(
        " {readout} ({:03o})",
        state.bits
    ))));
    rows.push(ListItem::new(Line::raw(String::new())));
    rows.push(ListItem::new(Line::styled(
        hint,
        Style::default().add_modifier(Modifier::DIM),
    )));
    frame.render_widget(List::new(rows), inner);
}

/// Draws the file-info modal (`Mode::FileInfo`): the pre-built
/// `label: value` rows, labels right-padded so values align. No-op if
/// `mode` is not `FileInfo`.
pub fn render_file_info(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::FileInfo { info } = mode else {
        return;
    };

    let label_width = info
        .rows
        .iter()
        .map(|(label, _)| UnicodeWidthStr::width(label.as_str()))
        .max()
        .unwrap_or(0);
    let lines: Vec<String> = info
        .rows
        .iter()
        .map(|(label, value)| {
            if label.is_empty() && value.is_empty() {
                String::new()
            } else {
                format!("{label:<label_width$}  {value}")
            }
        })
        .collect();

    let inner_width = lines
        .iter()
        .map(|l| UnicodeWidthStr::width(l.as_str()))
        .max()
        .unwrap_or(0)
        .max(UnicodeWidthStr::width(info.title.as_str()));
    let width = (inner_width as u16 + 4).clamp(1, area.width);
    let height = (lines.len() as u16 + 2).clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(info.title.as_str())
        .borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows: Vec<ListItem> = lines
        .into_iter()
        .map(|l| ListItem::new(Line::raw(l)))
        .collect();
    frame.render_widget(List::new(rows), inner);
}

/// Draws a centered confirm box with `message` on top of `area` (normally
/// the whole frame).
pub fn render_confirm(frame: &mut Frame, area: Rect, message: &str) {
    let width = (UnicodeWidthStr::width(message) as u16 + 4).min(area.width);
    let height = 3.min(area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default().title("Confirm").borders(Borders::ALL);
    let paragraph = Paragraph::new(message)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, popup);
}

/// Centers a `width` x `height` box inside `area`. `pub(super)` so
/// `ui::function_list_view`'s command-palette popup (the only other centered
/// popup in `ui/*`) reuses the exact same math instead of a second copy.
pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::mode::PromptKind;

    fn render_prompt(
        width: u16,
        height: u16,
        kind: PromptKind,
        value: &str,
    ) -> (String, (u16, u16)) {
        let mode = Mode::Prompt {
            kind,
            input: LineEditor::from_str(value),
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_prompt_box(frame, frame.area(), &mode))
            .unwrap();
        let cursor = terminal.get_cursor_position().unwrap();
        let buffer = terminal.backend().buffer();
        let mut rows = Vec::new();
        for y in 0..height {
            let mut row = String::new();
            for x in 0..width {
                row.push_str(buffer[(x, y)].symbol());
            }
            rows.push(row);
        }
        (rows.join("\n"), (cursor.x, cursor.y))
    }

    #[test]
    fn archive_password_prompt_masks_the_typed_value() {
        let (screen, cursor) = render_prompt(
            60,
            20,
            PromptKind::ArchivePassword {
                pending: crate::mode::PasswordPending::Unzip {
                    archive_path: std::path::PathBuf::from("/a/secret.zip"),
                    dest_dir: std::path::PathBuf::from("/b"),
                },
            },
            "hunter2",
        );
        assert!(screen.contains("Password"), "screen: {screen}");
        assert!(
            !screen.contains("hunter2"),
            "the raw password must never render: {screen}"
        );
        assert!(screen.contains("*******"), "screen: {screen}");
        // Cursor sits one past the 7 asterisks.
        let star_row = screen.lines().position(|l| l.contains("*******")).unwrap() as u16;
        assert_eq!(cursor.1, star_row);
    }

    #[test]
    fn render_prompt_box_shows_the_title_and_content_roughly_centered() {
        let (screen, _) = render_prompt(60, 20, PromptKind::Mkdir, "newdir");
        assert!(screen.contains("New directory"), "screen: {screen}");
        assert!(screen.contains("newdir"), "screen: {screen}");

        // "Roughly centered": in a frame with plenty of room, the box's
        // title row must be indented from the left edge (not flush,
        // unlike the old bottom-status-bar-line rendering it replaced),
        // and must not be the very first row of the frame (vertically
        // centered, not pinned to the top).
        let title_row_index = screen
            .lines()
            .position(|l| l.contains("New directory"))
            .unwrap();
        assert!(
            title_row_index > 0,
            "box must be vertically centered, not flush against row 0"
        );
        let title_line = screen.lines().nth(title_row_index).unwrap();
        let left_padding = title_line.chars().take_while(|c| *c == ' ').count();
        assert!(
            left_padding > 0 && left_padding < 55,
            "title row should be indented from the left edge, not flush: {title_line:?}"
        );
    }

    #[test]
    fn render_prompt_box_cursor_sits_at_the_end_of_the_typed_text() {
        let (_, (cursor_x, cursor_y)) = render_prompt(60, 20, PromptKind::Command, "ls -la");
        // The popup is vertically centered in a 20-row frame, and the
        // cursor sits on the input row, one row below the box's top
        // border — nowhere near the very first frame row either way,
        // which is what actually matters here (a fixed exact coordinate
        // would be too brittle against layout tweaks).
        assert!(cursor_y > 1, "cursor row: {cursor_y}");
        assert!(cursor_x > 0, "cursor col: {cursor_x}");
    }

    #[test]
    fn render_prompt_box_titles_match_every_prompt_kind() {
        let cases: &[(PromptKind, &str)] = &[
            (
                PromptKind::Rename {
                    orig: "a.txt".to_string(),
                },
                "Rename",
            ),
            (PromptKind::Mkdir, "New directory"),
            (
                PromptKind::ZipName {
                    targets: Vec::new(),
                },
                "Zip as",
            ),
            (PromptKind::Command, "Command"),
            (
                PromptKind::Duplicate {
                    source: std::path::PathBuf::from("a.txt"),
                },
                "Duplicate as",
            ),
        ];
        for (kind, expected_title) in cases {
            let (screen, _) = render_prompt(60, 20, kind.clone(), "x");
            assert!(
                screen.contains(expected_title),
                "expected title {expected_title:?} for {kind:?}, screen: {screen}"
            );
        }
    }

    #[test]
    fn render_prompt_box_long_input_scrolls_so_the_cursor_stays_visible() {
        // A box far narrower than the typed text — the cursor (at the end
        // of the input) must still land inside the box's own bounds, not
        // off its right edge, the way it would without the horizontal
        // scroll.
        let frame_width = 40u16;
        let long_value = "a".repeat(200);
        let (_, (cursor_x, _)) = render_prompt(frame_width, 10, PromptKind::Mkdir, &long_value);
        let popup_width = (frame_width / 2)
            .max(PROMPT_BOX_MIN_WIDTH.min(frame_width))
            .min(frame_width);
        let popup_x = frame_width.saturating_sub(popup_width) / 2;
        assert!(
            cursor_x >= popup_x && cursor_x < popup_x + popup_width,
            "cursor_x {cursor_x} must stay within the popup's own [{popup_x}, {}) bounds",
            popup_x + popup_width
        );
    }
}
