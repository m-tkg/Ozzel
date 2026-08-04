//! Top-level draw routine: two 50% panes, a 4-line log area, and a 1-line
//! status bar.

mod pane_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{ActivePane, App};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);

    pane_view::render(
        frame,
        panes[0],
        &app.panes[0],
        app.active == ActivePane::Left,
    );
    pane_view::render(
        frame,
        panes[1],
        &app.panes[1],
        app.active == ActivePane::Right,
    );

    let log_text = app.last_error.as_deref().unwrap_or("");
    let log_block = Block::default().title("Log").borders(Borders::ALL);
    frame.render_widget(Paragraph::new(log_text).block(log_block), rows[1]);

    let status = Paragraph::new(format!(
        " {}  |  q:quit  Tab:switch  Enter:open  Backspace:parent  s:sort  .:hidden  C-r:refresh",
        app.active_pane().cwd.display()
    ))
    .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(status, rows[2]);
}
