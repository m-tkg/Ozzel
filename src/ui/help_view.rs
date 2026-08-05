//! Renders `Mode::Help`: a full-frame, scrollable listing of the *current*
//! effective keymap (after `[keys]`/`[bindings]` merges), grouped by
//! category, followed by a static section of fixed keys that live outside
//! the keymap entirely. Takes over the entire frame, the same way the
//! viewer does.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::help::{self, HelpLine};
use crate::keymap::Keymap;
use crate::mode::Mode;

pub fn render(frame: &mut Frame, area: Rect, mode: &Mode, keymap: &Keymap) {
    let Mode::Help { scroll } = mode else {
        return;
    };

    let lines = help::build_lines(keymap);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    let viewport_height = rows[0].height as usize;
    let visible: Vec<Line> = lines
        .iter()
        .skip(*scroll)
        .take(viewport_height)
        .map(render_line)
        .collect();
    frame.render_widget(Paragraph::new(visible), rows[0]);

    let total = lines.len();
    let bottom = (*scroll + viewport_height).min(total);
    let range_text = if total == 0 {
        "0/0".to_string()
    } else {
        format!("{}-{}/{}", *scroll + 1, bottom, total)
    };
    let footer = format!(" ozzel keybindings  [{range_text}]  q/Esc/h:close");
    let footer_style = Style::default().add_modifier(Modifier::REVERSED);
    frame.render_widget(Paragraph::new(footer).style(footer_style), rows[1]);
}

fn render_line(line: &HelpLine) -> Line<'static> {
    match line {
        HelpLine::Header(title) => {
            Line::styled(title.clone(), Style::default().add_modifier(Modifier::BOLD))
        }
        HelpLine::Binding {
            keys,
            action,
            description,
        } => Line::raw(format!("  {keys:<16} {action:<16} {description}")),
        HelpLine::Text(text) => Line::raw(format!("  {text}")),
        HelpLine::Blank => Line::raw(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let keymap = Keymap::defaults();
        let mode = Mode::Help { scroll: 0 };

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &mode, &keymap))
            .unwrap();

        for cell in terminal.backend().buffer().content() {
            let symbol = cell.symbol();
            assert!(
                !BORDER_GLYPHS.iter().any(|g| symbol.contains(*g)),
                "unexpected border glyph {symbol:?} in the rendered help screen"
            );
        }
    }
}
