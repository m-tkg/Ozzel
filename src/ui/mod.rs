//! Top-level draw routine: two 50% panes, a log area (4 content rows once
//! its border is accounted for), and a 1-line status bar — replaced by the
//! Filter/JumpSearch input line in those modes (deliberately *not* a
//! centered popup: the list below has to stay visible while narrowing it
//! live), with a centered box drawn on top of the normal status bar for
//! `Mode::Confirm` and `Mode::Prompt` alike (a prompt's text input isn't a
//! live-narrowing interaction the way Filter/JumpSearch are, so obscuring
//! the panes behind it costs nothing).

mod file_search_view;
mod function_list_view;
mod help_view;
pub mod layout;
// `pub` (rather than `pub(crate)`) so `benches/`'s criterion bench, an
// external target, can reach `wrap_log_lines` directly.
pub mod log_view;
// `pub(crate)` (not private) so `app`'s tests can assert the sort
// dialog's labels stay index-aligned with `App::SORT_DIALOG_CHOICES`.
pub(crate) mod modal;
// `pub(crate)` so `App::collision_info` can reuse `format_mtime`.
pub(crate) mod pane_view;
mod settings_view;
mod text;
mod viewer_view;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Paragraph;

use crate::app::{ActivePane, App};
use crate::mode::Mode;
pub use layout::PaneLayout;

/// Everything a call to [`draw`] learns about this frame's on-screen
/// geometry that `App` needs for the *next* frame/event (mouse
/// hit-testing, `Mode::Log`'s `/`/`?` search) — returned rather than
/// written directly into `app` from inside `draw`/`log_view::render_full`,
/// so drawing a frame never mutates `App`'s externally-visible state as a
/// side effect; the caller (`main.rs`'s loop) applies it afterward via
/// [`App::apply_layout_feedback`](crate::app::App::apply_layout_feedback).
/// Both fields default to `None` (via `#[derive(Default)]`) and are left
/// that way whenever this frame didn't touch the corresponding geometry —
/// `apply_layout_feedback` then simply leaves `App`'s existing value alone,
/// the same "stale until a relevant frame draws, harmless" idiom
/// `App::pane_layout`/`App::log_view_width` already relied on before this
/// struct existed.
#[derive(Debug, Default)]
pub struct LayoutFeedback {
    /// This frame's two-pane geometry — `Some` only when the normal
    /// two-pane layout was actually drawn (never during a Viewer/Help/Log/
    /// Settings full-frame takeover, none of which are clickable in a way
    /// that needs it — see `App::pane_at`).
    pub panes: Option<[PaneLayout; 2]>,
    /// The full log view's content width this frame — `Some` only when
    /// `Mode::Log`'s full-frame view was drawn (see
    /// `log_view::render_full`).
    pub log_view_width: Option<u16>,
}

/// Takes `&mut App` purely because some of the modes it dispatches to
/// (`log_view::render_full`, `help_view::render`, ...) call back into
/// `App`'s own memoization caches (`App::log_wrapped`, `App::help_lines`,
/// ...), which need `&mut self` to rebuild-and-cache; drawing a frame
/// otherwise never mutates `App` — see [`LayoutFeedback`]'s doc comment for
/// the two fields that used to be exceptions to that.
pub fn draw(frame: &mut Frame, app: &mut App) -> LayoutFeedback {
    let area = frame.area();

    // The viewer/log full-frame takeovers (panes, log, and status bar are
    // all replaced) are handled before any of the normal layout is
    // computed. Neither one is clickable in a way that needs
    // `pane_layout`, so this frame simply reports no pane geometry (see
    // `LayoutFeedback`'s doc comment — harmless: `App::pane_at` just won't
    // match anything relevant while one of these modes is up, and
    // Normal-mode mouse handling never runs during them anyway).
    if matches!(app.mode, Mode::Viewer { .. }) {
        viewer_view::render(frame, area, &app.mode);
        return LayoutFeedback::default();
    }
    if matches!(app.mode, Mode::Help { .. }) {
        help_view::render(frame, area, app);
        return LayoutFeedback::default();
    }
    if let Mode::Log {
        scroll_from_bottom, ..
    } = &app.mode
    {
        let scroll_from_bottom = *scroll_from_bottom;
        let log_view_width = log_view::render_full(frame, area, app, scroll_from_bottom);
        return LayoutFeedback {
            panes: None,
            log_view_width: Some(log_view_width),
        };
    }
    if matches!(app.mode, Mode::Settings { .. }) {
        settings_view::render(frame, area, app);
        return LayoutFeedback::default();
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
    let size_format = app.config.size_format;
    let left_layout = pane_view::render(
        frame,
        panes[0],
        &app.panes[0],
        app.active == ActivePane::Left,
        colors,
        show_permissions,
        size_format,
    );
    let right_layout = pane_view::render(
        frame,
        panes[1],
        &app.panes[1],
        app.active == ActivePane::Right,
        colors,
        show_permissions,
        size_format,
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
        Mode::JumpSearch { input, .. } => {
            let has_match = !app.active_pane().jump_matches(&input.value()).is_empty();
            modal::render_jump_search_line(frame, rows[2], &app.mode, has_match);
        }
        Mode::Select { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_select(frame, area, &app.mode);
        }
        Mode::Prompt { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_prompt_box(frame, area, &app.mode);
        }
        Mode::Confirm { message, .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_confirm(frame, area, message);
        }
        Mode::TransferCollision { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_transfer_collision(frame, area, &app.mode);
        }
        Mode::FunctionList { .. } => {
            render_status_bar(frame, rows[2], app);
            function_list_view::render(frame, area, app);
        }
        Mode::FileSearch { .. } => {
            render_status_bar(frame, rows[2], app);
            file_search_view::render(frame, area, app);
        }
        Mode::SortSelect { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_sort_select(frame, area, &app.mode);
        }
        Mode::Chmod { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_chmod(frame, area, &app.mode);
        }
        Mode::FileInfo { .. } => {
            render_status_bar(frame, rows[2], app);
            modal::render_file_info(frame, area, &app.mode);
        }
        Mode::Normal => render_status_bar(frame, rows[2], app),
        Mode::Viewer { .. } | Mode::Help { .. } | Mode::Log { .. } | Mode::Settings { .. } => {
            unreachable!("handled by the full-frame takeover return above")
        }
    }

    LayoutFeedback {
        panes: Some([left_layout, right_layout]),
        log_view_width: None,
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::Config;
    use crate::mode::{LineEditor, PromptKind};

    #[test]
    fn prompt_mode_leaves_the_bottom_status_bar_showing_the_normal_status_not_the_old_inline_input()
    {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap();
        app.mode = Mode::Prompt {
            kind: PromptKind::Mkdir,
            input: LineEditor::from_str("newdir"),
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &mut app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bottom_row_y = 23u16;
        let mut bottom_row = String::new();
        for x in 0..80u16 {
            bottom_row.push_str(buffer[(x, bottom_row_y)].symbol());
        }

        // The bottom row is the ordinary status bar (cwd path + h:help
        // hint) — the prompt's own label/input never lands there anymore.
        assert!(bottom_row.contains("h:help"), "bottom row: {bottom_row:?}");
        assert!(
            !bottom_row.contains("New directory"),
            "the prompt label must not appear in the bottom status bar: {bottom_row:?}"
        );
        assert!(
            !bottom_row.contains("newdir"),
            "the prompt's typed content must not appear in the bottom status bar: {bottom_row:?}"
        );

        // It does appear somewhere in the frame, in the centered popup.
        let mut full_screen = String::new();
        for y in 0..24u16 {
            for x in 0..80u16 {
                full_screen.push_str(buffer[(x, y)].symbol());
            }
            full_screen.push('\n');
        }
        assert!(
            full_screen.contains("New directory"),
            "screen: {full_screen}"
        );
        assert!(full_screen.contains("newdir"), "screen: {full_screen}");
    }
}
