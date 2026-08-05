//! Renders the `F`/`S-f` command palette (`Mode::FunctionList`): a centered
//! popup — dyna's "Function List" — with an incremental-filter input line
//! on top and the matching actions listed below it, the highlighted one
//! reversed. A popup overlay (like `modal::render_select`), not a
//! full-frame takeover: the two panes stay visible (if dimmed by the
//! `Clear` underneath the popup box) so the palette doesn't feel like it
//! left the file manager.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::mode::Mode;

/// How wide/tall the palette popup is, as a fraction of the full frame —
/// generous enough to show several actions' descriptions without wrapping
/// on a normal-sized terminal.
const WIDTH_NUM: u16 = 3;
const WIDTH_DEN: u16 = 4;
const HEIGHT_NUM: u16 = 3;
const HEIGHT_DEN: u16 = 4;

pub fn render(frame: &mut Frame, area: Rect, mode: &Mode) {
    let Mode::FunctionList { input, cursor } = mode else {
        return;
    };
    let actions = crate::function_list::filter_actions(&input.value());

    let width = (area.width * WIDTH_NUM / WIDTH_DEN).clamp(1, area.width);
    let height = (area.height * HEIGHT_NUM / HEIGHT_DEN).clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title("Function List")
        .borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let input_text = format!("> {}", input.value());
    frame.render_widget(Paragraph::new(input_text), rows[0]);
    let col = rows[0].x + 2 + input.cursor_display_col() as u16;
    frame.set_cursor_position((
        col.min(rows[0].x + rows[0].width.saturating_sub(1)),
        rows[0].y,
    ));

    let name_width = actions
        .iter()
        .map(|a| UnicodeWidthStr::width(a.config_name()))
        .max()
        .unwrap_or(0);
    let list_rows: Vec<ListItem> = actions
        .iter()
        .enumerate()
        .map(|(idx, action)| {
            let text = format!(
                "{:width$}  {}",
                action.config_name(),
                action.description(),
                width = name_width
            );
            let style = if idx == *cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::styled(text, style))
        })
        .collect();
    frame.render_widget(List::new(list_rows), rows[1]);
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
