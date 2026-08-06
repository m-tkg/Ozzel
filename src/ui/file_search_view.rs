//! Renders the `g` file-name search (`Mode::FileSearch`): a centered popup
//! with the query input line on top and the matching root-relative paths
//! listed below it, the highlighted one reversed — the same popup-overlay
//! shape as `function_list_view` (which this deliberately mirrors), plus
//! two extras of its own: a red error line under the input when a `re:`
//! pattern failed to compile, and title markers for a truncated snapshot
//! and (in Enter-run mode) results that lag the typed query.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::app::App;
use crate::mode::Mode;
use crate::ui::modal::centered_rect;

/// Same fraction of the frame as the command palette — see
/// `function_list_view`'s constants.
const WIDTH_NUM: u16 = 3;
const WIDTH_DEN: u16 = 4;
const HEIGHT_NUM: u16 = 3;
const HEIGHT_DEN: u16 = 4;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let Mode::FileSearch {
        input,
        cursor,
        tree,
        results,
        last_run_query,
        error,
    } = &app.mode
    else {
        return;
    };
    let cursor = *cursor;

    let width = (area.width * WIDTH_NUM / WIDTH_DEN).clamp(1, area.width);
    let height = (area.height * HEIGHT_NUM / HEIGHT_DEN).clamp(1, area.height);
    let popup = centered_rect(area, width, height);

    // Last path component only — a full tempdir/home path would blow past
    // the popup's width and push the hit count off the edge.
    let root_name = tree
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| tree.root.display().to_string());
    let mut title = format!("File Search: {root_name} ({} hits)", results.len());
    if tree.truncated {
        title.push_str(" [truncated]");
    }
    // Only reachable with `file_search_incremental = false` — incremental
    // mode re-runs on every edit, so input and last run never diverge.
    if input.value() != *last_run_query {
        title.push_str(" [Enter to search]");
    }

    frame.render_widget(Clear, popup);
    let block = Block::default().title(title).borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 {
        return;
    }
    let error_height = if error.is_some() { 1 } else { 0 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(error_height),
            Constraint::Min(0),
        ])
        .split(inner);

    let input_text = format!("> {}", input.value());
    frame.render_widget(Paragraph::new(input_text), rows[0]);
    let col = rows[0].x + 2 + input.cursor_display_col() as u16;
    frame.set_cursor_position((
        col.min(rows[0].x + rows[0].width.saturating_sub(1)),
        rows[0].y,
    ));

    if let Some(error) = error {
        let line = Paragraph::new(format!("re: {error}")).style(Style::default().fg(Color::Red));
        frame.render_widget(line, rows[1]);
    }

    // Only the visible window's rows get formatted/allocated into
    // `ListItem`s — same story as `function_list_view`.
    let window =
        super::settings_view::windowed_range(results.len(), cursor, rows[2].height as usize);
    let list_rows: Vec<ListItem> = results[window.clone()]
        .iter()
        .enumerate()
        .map(|(offset, &entry_idx)| {
            let idx = window.start + offset;
            let entry = &tree.entries[entry_idx];
            let text = if entry.is_dir {
                format!("{}{}", entry.display, std::path::MAIN_SEPARATOR)
            } else {
                entry.display.clone()
            };
            let style = if idx == cursor {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(Line::styled(text, style))
        })
        .collect();
    frame.render_widget(List::new(list_rows), rows[2]);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::config::Config;
    use crate::event::{KeyCode, KeyModifiers};
    use crate::mode::Mode;

    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        let mut text = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn draw_app(app: &mut crate::app::App) -> Terminal<TestBackend> {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                crate::ui::draw(frame, app);
            })
            .unwrap();
        terminal
    }

    fn type_str(app: &mut crate::app::App, text: &str) {
        for c in text.chars() {
            app.handle_event(crate::event::AppEvent::Input(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            ));
        }
    }

    #[test]
    fn popup_shows_title_hits_and_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("needle.txt"), b"").unwrap();
        let mut app = crate::app::App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap();
        app.dispatch(crate::action::Action::FileSearch);
        type_str(&mut app, "needle");

        let screen = screen_text(&draw_app(&mut app));
        assert!(screen.contains("File Search:"), "screen: {screen}");
        assert!(screen.contains("(1 hits)"), "screen: {screen}");
        assert!(screen.contains("> needle"), "screen: {screen}");
        let relative = std::path::Path::new("sub").join("needle.txt");
        assert!(
            screen.contains(&relative.to_string_lossy().into_owned()),
            "screen: {screen}"
        );
        assert!(!screen.contains("[truncated]"), "screen: {screen}");
        assert!(!screen.contains("[Enter to search]"), "screen: {screen}");
    }

    #[test]
    fn invalid_regex_shows_a_red_error_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let mut app = crate::app::App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap();
        app.dispatch(crate::action::Action::FileSearch);
        type_str(&mut app, "re:[");

        let screen = screen_text(&draw_app(&mut app));
        assert!(screen.contains("(0 hits)"), "screen: {screen}");
        assert!(screen.contains("re: regex parse error"), "screen: {screen}");
    }

    #[test]
    fn enter_run_mode_flags_stale_results_in_the_title() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let config = Config {
            file_search_incremental: false,
            ..Config::default()
        };
        let mut app =
            crate::app::App::new(dir.path().to_path_buf(), dir.path().to_path_buf(), config)
                .unwrap();
        app.dispatch(crate::action::Action::FileSearch);
        type_str(&mut app, "a");
        assert!(matches!(app.mode, Mode::FileSearch { .. }));

        let screen = screen_text(&draw_app(&mut app));
        assert!(screen.contains("[Enter to search]"), "screen: {screen}");
    }

    #[test]
    fn truncated_snapshot_is_flagged_in_the_title() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let mut app = crate::app::App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap();
        app.dispatch(crate::action::Action::FileSearch);
        if let Mode::FileSearch { tree, .. } = &mut app.mode {
            let mut truncated_tree = crate::file_search::collect_tree(dir.path(), true, 1);
            truncated_tree.truncated = true;
            *tree = std::rc::Rc::new(truncated_tree);
        }

        let screen = screen_text(&draw_app(&mut app));
        assert!(screen.contains("[truncated]"), "screen: {screen}");
    }
}
