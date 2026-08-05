//! Top-level draw routine: two 50% panes, a log area (4 content rows once
//! its border is accounted for), and a 1-line status bar (replaced by the
//! prompt line in `Mode::Prompt`, with a centered confirm box drawn on top
//! in `Mode::Confirm`).

mod function_list_view;
mod help_view;
mod log_view;
mod modal;
mod pane_view;
mod viewer_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

use crate::app::{ActivePane, App};
use crate::mode::Mode;

/// Takes `&mut App` (rather than `&App`, as before mouse support) purely so
/// each frame can refresh `App::pane_layout` with this frame's real
/// on-screen geometry — mouse hit-testing (`App::handle_mouse`) reads it
/// back on the *next* event, never mutates it itself, so `App` still never
/// changes as a side effect of anything except this one assignment.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // The viewer/log full-frame takeovers (panes, log, and status bar are
    // all replaced) are handled before any of the normal layout is
    // computed. Neither one is clickable in a way that needs
    // `pane_layout`, so it's simply left stale (harmless: `App::pane_at`
    // just won't match anything relevant while one of these modes is up,
    // and Normal-mode mouse handling never runs during them anyway).
    if matches!(app.mode, Mode::Viewer { .. }) {
        viewer_view::render(frame, area, &app.mode);
        return;
    }
    if matches!(app.mode, Mode::Help { .. }) {
        help_view::render(frame, area, &app.mode, &app.keymap);
        return;
    }
    if let Mode::Log { scroll_from_bottom } = &app.mode {
        log_view::render_full(frame, area, app, *scroll_from_bottom);
        return;
    }

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

    let colors = pane_view::PaneColors {
        cursor: app.config.colors.cursor,
        cursor_inactive: app.config.colors.cursor_inactive,
        dim_inactive: app.config.colors.dim_inactive,
        directory: app.config.colors.directory,
        hidden: app.config.colors.hidden,
        executable: app.config.colors.executable,
    };
    let show_permissions = app.config.show_permissions;
    let left_layout = pane_view::render(
        frame,
        panes[0],
        &app.panes[0],
        app.active == ActivePane::Left,
        colors,
        show_permissions,
    );
    let right_layout = pane_view::render(
        frame,
        panes[1],
        &app.panes[1],
        app.active == ActivePane::Right,
        colors,
        show_permissions,
    );
    app.pane_layout = [Some(left_layout), Some(right_layout)];

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
        Mode::JumpSearch { input, .. } => {
            let has_match = !app.active_pane().jump_matches(&input.value()).is_empty();
            modal::render_jump_search_line(frame, rows[2], &app.mode, has_match);
        }
        Mode::Select { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_select(frame, area, &app.mode);
        }
        Mode::Prompt { .. } => modal::render_prompt_line(frame, rows[2], &app.mode),
        Mode::Confirm { message, .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_confirm(frame, area, message);
        }
        Mode::FunctionList { .. } => {
            render_status_bar(frame, rows[2], app);
            function_list_view::render(frame, area, &app.mode);
        }
        Mode::Normal => render_status_bar(frame, rows[2], app),
        Mode::Viewer { .. } | Mode::Help { .. } | Mode::Log { .. } => {
            unreachable!("handled by the full-frame takeover return above")
        }
    }
}

/// One line, built from whatever's actually relevant right now (active
/// filter, mark count, running-task count) followed by a single trailing
/// `h:help` hint — the full keybinding list used to be spelled out here,
/// but that's redundant now that the help screen (`h`/`?`) shows the
/// complete, current-config-aware keymap; all cut off by the terminal's
/// own width if it doesn't fit.
fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let pane = app.active_pane();
    let mut segments = Vec::new();
    if let Some(filter) = &pane.filter {
        segments.push(format!("flt:{}", filter.raw));
    }
    if !pane.marks.is_empty() {
        segments.push(format!("{} marked", pane.marks.len()));
    }
    if !app.tasks.running.is_empty() {
        segments.push(format!("{} running", app.tasks.running.len()));
    }
    let info = if segments.is_empty() {
        String::new()
    } else {
        format!("{}  |  ", segments.join("  "))
    };

    let status = Paragraph::new(format!(" {}{}  |  h:help", info, pane.cwd.display()))
        .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(status, area);
}
