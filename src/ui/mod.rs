//! Top-level draw routine: two 50% panes, a log area (4 content rows once
//! its border is accounted for), and a 1-line status bar (replaced by the
//! prompt line in `Mode::Prompt`, with a centered confirm box drawn on top
//! in `Mode::Confirm`).

mod log_view;
mod modal;
mod pane_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

use crate::app::{ActivePane, App};
use crate::mode::Mode;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(6), // 4 content rows + top/bottom border
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

    log_view::render(frame, rows[1], app);

    match &app.mode {
        Mode::Filter { .. } => {
            let error = app
                .active_pane()
                .filter
                .as_ref()
                .and_then(|f| f.error())
                .map(str::to_string);
            modal::render_filter_line(frame, rows[2], &app.mode, error.as_deref());
        }
        Mode::Prompt { .. } => modal::render_prompt_line(frame, rows[2], &app.mode),
        Mode::Confirm { message, .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_confirm(frame, area, message);
        }
        Mode::Normal => render_status_bar(frame, rows[2], app),
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let marks = app.active_pane().marks.len();
    let marks_text = if marks > 0 {
        format!("{marks} marked  |  ")
    } else {
        String::new()
    };
    let tasks_text = if app.tasks.running.is_empty() {
        String::new()
    } else {
        format!("{} running  |  ", app.tasks.running.len())
    };
    let status = Paragraph::new(format!(
        " {}{}{}  |  q:quit  Tab:switch  Space:mark  C/M/D/R/K  s:sort  .:hidden",
        tasks_text,
        marks_text,
        app.active_pane().cwd.display()
    ))
    .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(status, area);
}
