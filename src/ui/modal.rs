//! Renders the two modal overlays: the bottom prompt line (`Mode::Prompt`,
//! drawn in place of the status bar) and the centered confirm box
//! (`Mode::Confirm`, drawn on top of everything else).

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
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
    };
    let text = format!("{label}{}", input.value());
    let paragraph = Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(paragraph, area);

    let col = area.x + UnicodeWidthStr::width(label) as u16 + input.cursor_display_col() as u16;
    let col = col.min(area.x + area.width.saturating_sub(1));
    frame.set_cursor_position((col, area.y));
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
