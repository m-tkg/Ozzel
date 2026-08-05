//! Renders `Mode::Help`: a full-frame, scrollable listing of the *current*
//! effective keymap (after `[keys]`/`[bindings]` merges), grouped by
//! category, followed by a static section of fixed keys that live outside
//! the keymap entirely. Takes over the entire frame, the same way the
//! viewer does. Same `less`-style `/`/`?` search as the viewer (see
//! `crate::search`) — matches highlight via `super::viewer_view::styled_line`
//! against `help::build_display_lines`' plain-text rendering of these same
//! rows, so the search haystack and what's actually drawn can never drift
//! apart.

use std::rc::Rc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::help::HelpLine;
use crate::mode::{LineEditor, Mode, SearchDirection, ViewerSearch};
use crate::viewer::Matcher;

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    // Pulled out as owned copies *before* `app.help_lines()` below (which
    // needs `app` mutably — it may rebuild and cache the listing): `scroll`
    // is `Copy`, and `search` is cheap to clone (its `matcher`, if any, is
    // just an `Rc` bump) — this way nothing here has to juggle two live
    // borrows of `app` at once.
    let (scroll, search) = match &app.mode {
        Mode::Help { scroll, search } => (*scroll, search.clone()),
        _ => return,
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    // While actively typing a search pattern, the matcher doesn't exist
    // yet — only an `Active` search (see `super::viewer_view`'s identical
    // reasoning) has anything to highlight. The matcher itself was compiled
    // once by `crate::search::run`, not rebuilt here every frame.
    let matcher = match &search {
        ViewerSearch::Active { matcher, .. } => Some(Rc::clone(matcher)),
        _ => None,
    };

    // Only rebuilt when the keymap itself changed (see `App::help_lines`) —
    // not on every scroll/search keystroke against the already-open screen.
    let lines = app.help_lines();
    let viewport_height = rows[0].height as usize;
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(viewport_height)
        .map(|line| render_line(line, matcher.as_deref()))
        .collect();
    frame.render_widget(Paragraph::new(visible), rows[0]);

    let total = lines.len();
    let bottom = (scroll + viewport_height).min(total);
    let range_text = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}-{}/{}", scroll + 1, bottom, total)
    };

    // `lines`' last use was just above, so `app` is no longer borrowed from
    // here on — everything below reads `search` (already an owned copy)
    // instead of `app.mode` again.
    // A search input in progress takes over the footer row entirely, same
    // as the viewer's own footer does.
    if let ViewerSearch::Editing {
        input, direction, ..
    } = &search
    {
        render_search_input_line(frame, rows[1], *direction, input);
        return;
    }

    let search_note = match &search {
        ViewerSearch::Active {
            pattern,
            direction,
            matches,
            current,
            wrapped,
            ..
        } => {
            let prefix = match direction {
                SearchDirection::Forward => '/',
                SearchDirection::Backward => '?',
            };
            let wrap_note = if *wrapped { "  (search wrapped)" } else { "" };
            format!(
                "  {prefix}{pattern}  {}/{}{wrap_note}",
                current + 1,
                matches.len()
            )
        }
        _ => String::new(),
    };
    let footer =
        format!(" ozzel keybindings  [{range_text}]{search_note}  /,?:search  q/Esc/h:close");
    let footer_style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(footer).style(footer_style), rows[1]);
}

/// Mirrors `ui::viewer_view::render_search_input_line` — see its doc
/// comment; kept as a small private duplicate here for the same reason
/// `ui::log_view`'s copy is (this footer row's only other dependency on
/// `viewer_view` is `styled_line`, already reused below).
fn render_search_input_line(
    frame: &mut Frame,
    area: Rect,
    direction: SearchDirection,
    input: &LineEditor,
) {
    let label = match direction {
        SearchDirection::Forward => "/",
        SearchDirection::Backward => "?",
    };
    let text = format!("{label}{}", input.value());
    let style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(text).style(style), area);

    let col = area.x
        + unicode_width::UnicodeWidthStr::width(label) as u16
        + input.cursor_display_col() as u16;
    let col = col.min(area.x + area.width.saturating_sub(1));
    frame.set_cursor_position((col, area.y));
}

/// Builds one rendered `Line` for `line`: `Header`s are bold (never
/// highlighted — a header is a section title, never a keybinding fact
/// worth searching for... though a search matching its text still just
/// won't highlight, since `styled_line` unconditionally colors match
/// spans, it renders under the bold either way), everything else is
/// plain text with `styled_line`'s usual match highlighting.
/// `line.text()` (see `HelpLine::text`) is exactly what `n`/`N`/`/`/`?`
/// search against (`help::build_display_lines`), so a highlighted span
/// here is always at the same byte offset the search itself found.
fn render_line(line: &HelpLine, matcher: Option<&Matcher>) -> Line<'static> {
    let text = line.text();
    match line {
        HelpLine::Header(_) => Line::styled(text, Style::default().add_modifier(Modifier::BOLD)),
        HelpLine::Blank => Line::raw(String::new()),
        HelpLine::Binding { .. } | HelpLine::Text(_) => {
            // `styled_line` matches against `text` exactly as-is (the same
            // string `help::build_display_lines` puts in the search
            // haystack), so a highlighted span's byte offsets always line
            // up with what the search actually found — the 2-column
            // indent is applied afterward, as a separate unstyled leading
            // span, rather than folded into the searched string itself.
            let content = super::viewer_view::styled_line(&text, 0, usize::MAX, matcher);
            let mut spans = vec![ratatui::text::Span::raw("  ")];
            spans.extend(content.spans);
            Line::from(spans)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap()
    }

    /// With mouse capture dynamically disabled while the help screen is
    /// up (see `App::wants_mouse_capture`), a native terminal click-drag
    /// selection must only ever be able to grab real keybinding text,
    /// never a border glyph or the title — this renders a real frame and
    /// checks for any box-drawing character `Borders::ALL` might have
    /// produced.
    #[test]
    fn render_draws_no_border_characters() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        const BORDER_GLYPHS: [char; 11] = ['─', '│', '┌', '┐', '└', '┘', '┬', '┴', '├', '┤', '┼'];

        let mut app = test_app();
        app.mode = Mode::Help {
            scroll: 0,
            search: ViewerSearch::Idle,
        };

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut app))
            .unwrap();

        for cell in terminal.backend().buffer().content() {
            let symbol = cell.symbol();
            assert!(
                !BORDER_GLYPHS.iter().any(|g| symbol.contains(*g)),
                "unexpected border glyph {symbol:?} in the rendered help screen"
            );
        }
    }

    #[test]
    fn render_highlights_an_active_search_match() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = test_app();
        app.mode = Mode::Help {
            scroll: 0,
            search: ViewerSearch::Active {
                pattern: "rename".to_string(),
                matcher: Rc::new(Matcher::build("rename")),
                direction: SearchDirection::Forward,
                matches: vec![0],
                current: 0,
                wrapped: false,
            },
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let has_reversed = buffer
            .content()
            .iter()
            .any(|cell| cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(has_reversed, "expected at least one reversed-video cell");
    }

    #[test]
    fn footer_shows_search_prompt_hint() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = test_app();
        app.mode = Mode::Help {
            scroll: 0,
            search: ViewerSearch::Idle,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut bottom = String::new();
        for x in 0..80u16 {
            bottom.push_str(buffer[(x, 23)].symbol());
        }
        assert!(bottom.contains("/,?:search"), "{bottom}");
    }
}
