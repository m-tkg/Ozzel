//! Renders the two modal overlays: the bottom prompt line (`Mode::Prompt`,
//! drawn in place of the status bar) and the centered confirm box
//! (`Mode::Confirm`, drawn on top of everything else).

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::mode::{Mode, PromptKind};

/// Draws the `Prompt` line into `area` (normally the status bar's row) and
/// positions the real terminal cursor at the edit point. No-op if `mode`
/// is not `Prompt`.
pub fn render_prompt_line(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::Prompt { kind, input } = mode else {
        return;
    };
    let label = match kind {
        PromptKind::Rename { .. } => "Rename: ",
        PromptKind::Mkdir => "New directory: ",
        PromptKind::ZipName { .. } => "Zip as: ",
        PromptKind::Command => ": ",
        PromptKind::Duplicate { .. } => "Duplicate as: ",
    };
    render_input_line(frame, area, label, input, None);
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

/// Shared by `render_prompt_line`/`render_filter_line`/`render_jump_search_line`:
/// draws `{label}{input}{warning_suffix}` (reverse-video, red when a
/// warning suffix is present) and positions the real terminal cursor at
/// the input's own edit point. `warning_suffix`, when present, is
/// expected to already include its own leading spacing/punctuation (e.g.
/// `"  (invalid regex: ...)"`, `"  (no match)"`) since the two callers'
/// wording differs.
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
