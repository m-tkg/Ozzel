//! Top-level draw routine: two 50% panes, a 4-line log area, and a 1-line
//! status bar (replaced by the prompt line in `Mode::Prompt`, with a
//! centered confirm box drawn on top in `Mode::Confirm`).

mod modal;
mod pane_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{ActivePane, App};
use crate::mode::Mode;

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

    render_log(frame, rows[1], app);

    match &app.mode {
        Mode::Prompt { .. } => modal::render_prompt_line(frame, rows[2], &app.mode),
        Mode::Confirm { message, .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_confirm(frame, area, message);
        }
        Mode::Normal => render_status_bar(frame, rows[2], app),
    }
}

fn render_log(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().title("Log").borders(Borders::ALL);
    let inner_height = area.height.saturating_sub(2) as usize;

    let lines: Vec<Line> = app
        .log
        .iter()
        .rev()
        .take(inner_height)
        .rev()
        .map(|line| {
            let style = if line.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            Line::styled(line.message.clone(), style)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let marks = app.active_pane().marks.len();
    let marks_text = if marks > 0 {
        format!("{marks} marked  |  ")
    } else {
        String::new()
    };
    let status = Paragraph::new(format!(
        " {}{}  |  q:quit  Tab:switch  Space:mark  C/M/D/R/K  s:sort  .:hidden",
        marks_text,
        app.active_pane().cwd.display()
    ))
    .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(status, area);
}
