//! Renders `Mode::ProcessManager`: a full-frame `ps` listing with a title
//! row, a column header, the rows themselves, and a footer carrying the sort
//! state plus any `ps` failure. Takes over the whole frame the way
//! `Mode::Help`/`Mode::Log` do (see `ui::draw`), and like them keeps the
//! plain terminal colors rather than the settings screen's blue dialog — the
//! settings palette exists to say "you are editing config", which this
//! screen isn't.
//!
//! Render-only: keys live in `App::handle_process_manager_key`, and the rows
//! arrive already sorted (`App::apply_process_snapshot`).

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, Paragraph};

use crate::app::App;
use crate::config::ColorsConfig;
use crate::mode::{Mode, ProcessManagerState};
use crate::process::ProcessInfo;
use crate::ui::layout::PaneLayout;
use crate::ui::modal;
use crate::ui::pane_view::human_size;
use crate::ui::settings_view::windowed_range;
use crate::ui::text;

/// `%CPU`/`%MEM` are fixed-width: `ps` caps them at `100.0`, which is
/// exactly as wide as the headers.
const PERCENT_WIDTH: usize = 5;
/// `RSS` is fixed-width too, since `human_size` can't produce more than
/// `1023.9G`.
const RSS_WIDTH: usize = 7;
/// A user name wider than this is truncated rather than pushing the command
/// column off the screen. Long enough for any real login name.
const MAX_USER_WIDTH: usize = 16;

/// Column widths for one frame's worth of rows. Computed from the whole
/// snapshot rather than the visible window so the columns don't shift
/// underneath the user as they scroll — which is cheap here because every
/// input is either an integer's digit count or an existing string's length,
/// with nothing formatted or allocated.
struct Widths {
    pid: usize,
    ppid: usize,
    user: usize,
    state: usize,
    etime: usize,
}

impl Widths {
    fn measure(procs: &[ProcessInfo]) -> Self {
        let mut widths = Widths {
            pid: "PID".len(),
            ppid: "PPID".len(),
            user: "USER".len(),
            state: "STAT".len(),
            etime: "ELAPSED".len(),
        };
        for p in procs {
            widths.pid = widths.pid.max(digits(p.pid));
            widths.ppid = widths.ppid.max(digits(p.ppid));
            widths.user = widths.user.max(p.user.chars().count()).min(MAX_USER_WIDTH);
            widths.state = widths.state.max(p.state.chars().count());
            widths.etime = widths.etime.max(p.etime.chars().count());
        }
        widths
    }

    /// Every column but `COMMAND`, plus the single space between each and
    /// the one-column indent — i.e. where the command column starts.
    fn fixed_columns(&self) -> usize {
        1 + self.pid
            + 1
            + self.ppid
            + 1
            + self.user
            + 1
            + PERCENT_WIDTH
            + 1
            + PERCENT_WIDTH
            + 1
            + RSS_WIDTH
            + 1
            + self.state
            + 1
            + self.etime
            + 1
    }
}

fn digits(value: u32) -> usize {
    value.checked_ilog10().unwrap_or(0) as usize + 1
}

/// Returns the row area and scroll offset this frame drew with, so
/// `App::handle_mouse` can map a click back to the process under it — the
/// same feedback path `ui::pane_view` uses, and the reason nothing here has
/// to reach into `App` to record it (see `ui::LayoutFeedback`).
pub fn render(frame: &mut Frame, area: Rect, app: &App) -> Option<PaneLayout> {
    let Mode::ProcessManager { state } = &app.mode else {
        return None;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let widths = Widths::measure(&state.processes);
    let command_width = (rows[2].width as usize).saturating_sub(widths.fixed_columns());
    // Computed here rather than inside `render_rows` so the exact window
    // this frame drew is what gets reported back for hit-testing; deriving
    // it twice would be two chances to disagree.
    let range = windowed_range(state.processes.len(), state.cursor, rows[2].height as usize);

    render_title(frame, rows[0], state);
    frame.render_widget(
        Paragraph::new(header_line(&widths)).style(Style::default().add_modifier(Modifier::BOLD)),
        rows[1],
    );
    render_rows(
        frame,
        rows[2],
        state,
        &widths,
        command_width,
        &app.config.colors,
        range.clone(),
    );
    render_footer(frame, rows[3], state);

    // Drawn last so it sits on top of the list. `Mode::Confirm`'s own box,
    // reused as-is — the question a kill asks is the same shape as any
    // other destructive confirmation.
    if let Some(kill) = &state.pending_kill {
        modal::render_confirm(
            frame,
            area,
            &format!(
                "Send {} to {} (pid {})? (y/n)",
                kill.signal.label(),
                kill.name,
                kill.pid
            ),
        );
    }

    Some(PaneLayout {
        area,
        rows_area: rows[2],
        start: range.start,
    })
}

fn render_title(frame: &mut Frame, area: Rect, state: &ProcessManagerState) {
    let direction = if state.ascending { "^" } else { "v" };
    let updated = match state.updated_at {
        Some(at) => format!("updated {}", at.format("%H:%M:%S")),
        None => String::new(),
    };
    let title = format!(
        " ozzel processes  {} proc(s)  sort: {}{}  {}",
        state.processes.len(),
        state.sort_key.label(),
        direction,
        updated
    );
    frame.render_widget(
        Paragraph::new(title).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn header_line(widths: &Widths) -> String {
    format!(
        " {} {} {} {} {} {} {} {} {}",
        text::pad_left("PID", widths.pid),
        text::pad_left("PPID", widths.ppid),
        text::pad_right("USER", widths.user),
        text::pad_left("%CPU", PERCENT_WIDTH),
        text::pad_left("%MEM", PERCENT_WIDTH),
        text::pad_left("RSS", RSS_WIDTH),
        text::pad_right("STAT", widths.state),
        text::pad_left("ELAPSED", widths.etime),
        "COMMAND",
    )
}

fn format_row(p: &ProcessInfo, widths: &Widths, command_width: usize) -> String {
    format!(
        " {} {} {} {} {} {} {} {} {}",
        text::pad_left(&p.pid.to_string(), widths.pid),
        text::pad_left(&p.ppid.to_string(), widths.ppid),
        text::pad_right(&text::truncate_right(&p.user, widths.user), widths.user),
        text::pad_left(&format!("{:.1}", p.cpu), PERCENT_WIDTH),
        text::pad_left(&format!("{:.1}", p.mem), PERCENT_WIDTH),
        // Always human-readable, regardless of `config.size_format`: that
        // setting is about comparing file sizes byte-for-byte, whereas an
        // RSS figure is only ever read as a magnitude — and the exact-byte
        // formats would cost the command column six more columns.
        text::pad_left(&human_size(p.rss_kib.saturating_mul(1024)), RSS_WIDTH),
        text::pad_right(&p.state, widths.state),
        text::pad_left(&p.etime, widths.etime),
        // Truncated rather than wrapped: one process must stay one row, or
        // the cursor's index stops matching what's on screen.
        text::truncate_right(&p.command, command_width),
    )
}

/// `range` is the window `render` computed — only those rows get formatted,
/// since a thousand-process snapshot would otherwise build (and throw away)
/// a thousand strings every frame.
#[allow(clippy::too_many_arguments)]
fn render_rows(
    frame: &mut Frame,
    area: Rect,
    state: &ProcessManagerState,
    widths: &Widths,
    command_width: usize,
    colors: &ColorsConfig,
    range: std::ops::Range<usize>,
) {
    if state.processes.is_empty() {
        let message = if state.loading {
            " collecting process list..."
        } else {
            " no processes"
        };
        frame.render_widget(Paragraph::new(message), area);
        return;
    }

    let items: Vec<ListItem> = state.processes[range.clone()]
        .iter()
        .zip(range)
        .map(|(p, idx)| {
            let text = format_row(p, widths, command_width);
            if idx == state.cursor {
                // Same treatment the pane cursor gets, so the two screens
                // read as the same application.
                ListItem::new(Line::styled(
                    text,
                    Style::default()
                        .bg(colors.cursor)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                ListItem::new(Line::raw(text))
            }
        })
        .collect();
    frame.render_widget(List::new(items), area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &ProcessManagerState) {
    // A `ps` failure takes the footer over entirely: the rows above it are
    // the last good snapshot, and without this they'd look current.
    if let Some(error) = &state.error {
        frame.render_widget(
            Paragraph::new(format!(" ps failed: {error}"))
                .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            area,
        );
        return;
    }
    let footer = " p/u/c/m/s/t/n:sort (again reverses)  r:refresh  x:TERM  X:KILL  q/Esc:close";
    frame.render_widget(
        Paragraph::new(footer).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::config::Config;
    use crate::mode::PendingKill;
    use crate::process::{ProcessSortKey, Signal};

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap()
    }

    fn sample(pid: u32, name: &str, command: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid: 1,
            user: "masaki".to_string(),
            cpu: 1.5,
            mem: 0.5,
            rss_kib: 294912,
            state: "S".to_string(),
            etime: "01:02:03".to_string(),
            etime_secs: Some(3723),
            command: command.to_string(),
            name: name.to_string(),
        }
    }

    fn state_with(processes: Vec<ProcessInfo>) -> Box<ProcessManagerState> {
        Box::new(ProcessManagerState {
            processes,
            sort_key: ProcessSortKey::Cpu,
            ascending: false,
            cursor: 0,
            loading: false,
            error: None,
            updated_at: None,
            pending_kill: None,
        })
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    /// The geometry this frame reported back for mouse hit-testing.
    fn rendered_layout(app: &mut App, width: u16, height: u16) -> PaneLayout {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut captured = None;
        terminal
            .draw(|frame| {
                captured = render(frame, frame.area(), app);
            })
            .unwrap();
        captured.expect("the process view must report its row geometry")
    }

    #[test]
    fn the_reported_rows_area_starts_below_the_title_and_column_header() {
        let mut app = test_app();
        app.mode = Mode::ProcessManager {
            state: state_with(vec![sample(101, "zsh", "/bin/zsh")]),
        };

        let layout = rendered_layout(&mut app, 80, 10);

        // Title row, header row, then the list; the footer takes the last.
        assert_eq!(layout.rows_area.y, 2);
        assert_eq!(layout.rows_area.height, 10 - 3);
        assert_eq!(layout.start, 0);
    }

    #[test]
    fn the_reported_start_follows_the_scrolled_window() {
        let mut app = test_app();
        let procs: Vec<_> = (0..100).map(|i| sample(1000 + i, "p", "/bin/p")).collect();
        let mut state = state_with(procs);
        state.cursor = 99;
        app.mode = Mode::ProcessManager { state };

        let layout = rendered_layout(&mut app, 80, 10);

        // The window ends on the cursor, so it starts `height - 1` above it
        // — the same number `hit_test_row` adds back to a clicked row.
        assert_eq!(layout.start, 99 - (layout.rows_area.height as usize - 1));
    }

    #[test]
    fn the_process_view_renders_a_header_row_and_one_row_per_process() {
        let mut app = test_app();
        app.mode = Mode::ProcessManager {
            state: state_with(vec![
                sample(101, "zsh", "/bin/zsh"),
                sample(202, "ssh", "/usr/bin/ssh"),
            ]),
        };
        let lines = draw(&mut app, 80, 8);
        assert!(lines[0].contains("ozzel processes"), "{:?}", lines[0]);
        assert!(
            lines[1].contains("PID") && lines[1].contains("COMMAND"),
            "{:?}",
            lines[1]
        );
        assert!(
            lines[2].contains("101") && lines[2].contains("/bin/zsh"),
            "{:?}",
            lines[2]
        );
        assert!(lines[3].contains("202"), "{:?}", lines[3]);
    }

    #[test]
    fn the_process_view_scrolls_to_keep_a_far_cursor_visible() {
        let mut app = test_app();
        let procs: Vec<_> = (0..100).map(|i| sample(1000 + i, "p", "/bin/p")).collect();
        let mut state = state_with(procs);
        state.cursor = 99;
        app.mode = Mode::ProcessManager { state };

        let lines = draw(&mut app, 80, 8);
        let body = lines[2..7].join("\n");
        assert!(body.contains("1099"), "{body}");
        assert!(!body.contains("1000"), "{body}");
    }

    #[test]
    fn the_process_view_truncates_a_long_command_instead_of_wrapping() {
        let mut app = test_app();
        let long = format!("/bin/{}", "x".repeat(300));
        app.mode = Mode::ProcessManager {
            state: state_with(vec![sample(101, "x", &long)]),
        };
        let lines = draw(&mut app, 80, 6);
        assert_eq!(lines[2].chars().count(), 80);
        // The row after it is empty: nothing wrapped onto it.
        assert_eq!(lines[3].trim(), "");
    }

    #[test]
    fn the_title_shows_the_current_sort_key_and_direction() {
        let mut app = test_app();
        let mut state = state_with(vec![sample(101, "zsh", "/bin/zsh")]);
        state.sort_key = ProcessSortKey::Rss;
        state.ascending = true;
        app.mode = Mode::ProcessManager { state };
        let lines = draw(&mut app, 80, 6);
        assert!(lines[0].contains("sort: RSS^"), "{:?}", lines[0]);
    }

    #[test]
    fn a_ps_failure_is_shown_in_the_footer_with_the_previous_rows_still_listed() {
        let mut app = test_app();
        let mut state = state_with(vec![sample(101, "zsh", "/bin/zsh")]);
        state.error = Some("no such file".to_string());
        app.mode = Mode::ProcessManager { state };
        let lines = draw(&mut app, 80, 6);
        assert!(lines[2].contains("/bin/zsh"), "{:?}", lines[2]);
        assert!(
            lines[5].contains("ps failed: no such file"),
            "{:?}",
            lines[5]
        );
    }

    #[test]
    fn the_loading_state_says_so_instead_of_showing_an_empty_list() {
        let mut app = test_app();
        let mut state = state_with(Vec::new());
        state.loading = true;
        app.mode = Mode::ProcessManager { state };
        let lines = draw(&mut app, 80, 6);
        assert!(lines[2].contains("collecting"), "{:?}", lines[2]);
    }

    #[test]
    fn the_kill_confirmation_box_names_the_target_pid_and_signal() {
        let mut app = test_app();
        let mut state = state_with(vec![sample(101, "zsh", "/bin/zsh")]);
        state.pending_kill = Some(PendingKill {
            pid: 101,
            name: "zsh".to_string(),
            signal: Signal::Kill,
        });
        app.mode = Mode::ProcessManager { state };
        let all = draw(&mut app, 80, 10).join("\n");
        assert!(all.contains("SIGKILL"), "{all}");
        assert!(all.contains("101"), "{all}");
    }
}
