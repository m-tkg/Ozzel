//! Renders the log area: the tail of the log that still fits after
//! reserving one row per currently-running background task for a progress
//! gauge (`desc [####----] 43% current_file`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::app::App;
use crate::tasks::RunningTask;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().title("Log").borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let running: Vec<&RunningTask> = app.tasks.running.values().collect();
    let gauge_rows = running.len().min(inner.height as usize);
    let log_rows = inner.height - gauge_rows as u16;

    let mut constraints = Vec::with_capacity(gauge_rows + 1);
    if log_rows > 0 {
        constraints.push(Constraint::Length(log_rows));
    }
    constraints.extend(std::iter::repeat_n(Constraint::Length(1), gauge_rows));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    let mut row = 0;
    if log_rows > 0 {
        render_log_lines(frame, rows[row], app, log_rows as usize);
        row += 1;
    }
    for task in running.into_iter().take(gauge_rows) {
        render_gauge(frame, rows[row], task);
        row += 1;
    }
}

fn render_log_lines(frame: &mut Frame, area: Rect, app: &App, count: usize) {
    let lines: Vec<Line> = app
        .log
        .iter()
        .rev()
        .take(count)
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
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_gauge(frame: &mut Frame, area: Rect, task: &RunningTask) {
    let (done, total) = task.progress;
    let ratio = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    };
    let percent = (ratio * 100.0).round() as u32;
    let label = if task.detail.is_empty() {
        format!("{} {percent}%", task.desc)
    } else {
        format!("{} {percent}% {}", task.desc, task.detail)
    };
    let gauge = Gauge::default()
        .ratio(ratio)
        .label(label)
        .gauge_style(Style::default().fg(Color::Cyan));
    frame.render_widget(gauge, area);
}
