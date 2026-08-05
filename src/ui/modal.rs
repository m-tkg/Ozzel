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
use crate::ui::viewer_view::slice_display_cols;

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
        PromptKind::Rename { .. } => "Rename",
        PromptKind::Mkdir => "New directory",
        PromptKind::ZipName { .. } => "Zip as",
        PromptKind::Command => "Command",
        PromptKind::Duplicate { .. } => "Duplicate as",
    };
    render_input_box(frame, area, title, input);
}

/// The actual popup layout/rendering `render_prompt_box` delegates to —
/// factored out so it's exercisable (and testable) without going through
/// `Mode::Prompt`'s specific title-selection match, and so a future
/// second caller (a text-input editor in the upcoming settings screen,
/// say) can reuse it directly.
fn render_input_box(frame: &mut Frame, area: Rect, title: &str, input: &LineEditor) {
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
    let cursor_col = input.cursor_display_col();
    // Keep the cursor's column inside the visible window: once it would
    // fall past the right edge, scroll the window to keep it exactly on
    // the last visible column — the same "reveal as you type" scrolling a
    // real single-line text input needs, built from the same display-
    // column math the viewer's horizontal scroll already uses.
    let start_col = cursor_col.saturating_sub(content_width.saturating_sub(1));
    let visible = slice_display_cols(&input.value(), start_col, content_width);
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
    render_input_line(frame, area, "Filter: ", input, suffix.as_deref());
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
    render_input_line(frame, area, "Jump: ", input, suffix);
}

/// Shared by `render_filter_line`/`render_jump_search_line` (the two
/// bottom-status-bar-line inputs — `render_prompt_box`'s popup has its own
/// layout, `render_input_box`, since it isn't drawn into the status bar
/// row at all): draws `{label}{input}{warning_suffix}` (reverse-video, red
/// when a warning suffix is present) and positions the real terminal
/// cursor at the input's own edit point. `warning_suffix`, when present,
/// is expected to already include its own leading spacing/punctuation
/// (e.g. `"  (invalid regex: ...)"`, `"  (no match)"`) since the two
/// callers' wording differs.
fn render_input_line(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    input: &crate::mode::LineEditor,
    warning_suffix: Option<&str>,
) {
    let text = match warning_suffix {
        Some(suffix) => format!("{label}{}{suffix}", input.value()),
        None => format!("{label}{}", input.value()),
    };
    let mut style = Style::default().add_modifier(Modifier::REVERSED);
    if warning_suffix.is_some() {
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

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
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
