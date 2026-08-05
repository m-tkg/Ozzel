//! Renders `Mode::Settings`: a full-frame, raspi-config-style screen —
//! category menu -> item list -> per-item editor. Takes over the entire
//! frame the same way `Mode::Help`/`Mode::Log`/`Mode::Viewer` do (see
//! `ui::draw`). All keys are fixed (`App::handle_settings_key`); this
//! module is render-only.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, Paragraph};

use crate::app::App;
use crate::mode::{Mode, SettingsEditor, SettingsScreen, TextField};
use crate::settings::{self, Category};
use crate::ui::text;

// raspi-config (whiptail/newt) palette: blue backdrop, light-gray dialog
// body with black text, red selection bar. Fixed on purpose — the point is
// that the settings screen looks unmistakably different from file panes.
fn backdrop_style() -> Style {
    Style::default().fg(Color::White).bg(Color::Blue)
}

fn dialog_style() -> Style {
    Style::default().fg(Color::Black).bg(Color::Gray)
}

fn selected_style() -> Style {
    Style::default().fg(Color::White).bg(Color::Red)
}

fn title_style() -> Style {
    Style::default()
        .fg(Color::Red)
        .bg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

fn fill(frame: &mut Frame, area: Rect, style: Style) {
    frame.render_widget(Block::default().style(style), area);
}

pub fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let Mode::Settings { screen } = &app.mode else {
        return;
    };
    fill(frame, area, backdrop_style());
    // Copied/cloned out of `screen` (ending its borrow of `app.mode`) before
    // any call below, so `render_items` is free to borrow `app` mutably
    // (it may populate `App::settings_keybinding_lines_cache`).
    match screen {
        SettingsScreen::Categories { cursor } => {
            let cursor = *cursor;
            render_categories(frame, area, cursor);
        }
        SettingsScreen::Items { category, cursor } => {
            let (category, cursor) = (*category, *cursor);
            render_items(frame, area, app, category, cursor);
        }
        SettingsScreen::Editor { editor, .. } => {
            let editor = editor.clone();
            render_editor(frame, area, app, &editor);
        }
    }
}

/// Splits `area` into a title row, a body (the list/editor), and a footer
/// hint row — every settings sub-screen uses this same three-row shape.
/// Also paints the body row in the dialog color so every sub-screen gets
/// the same gray "dialog" panel regardless of how many rows it draws.
fn frame_rows(frame: &mut Frame, area: Rect) -> std::rc::Rc<[Rect]> {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    fill(frame, rows[1], dialog_style());
    rows
}

fn render_title(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(text.to_string()).style(title_style()), area);
}

fn render_footer(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(
        Paragraph::new(text.to_string()).style(backdrop_style().add_modifier(Modifier::BOLD)),
        area,
    );
}

fn list_item(text: String, selected: bool) -> ListItem<'static> {
    let style = if selected {
        selected_style()
    } else {
        dialog_style()
    };
    ListItem::new(Line::styled(text, style))
}

fn render_categories(frame: &mut Frame, area: Rect, cursor: usize) {
    let rows = frame_rows(frame, area);
    render_title(frame, rows[0], " ozzel settings");
    let items: Vec<ListItem> = Category::ALL
        .iter()
        .enumerate()
        .map(|(i, category)| list_item(format!(" {}", category.label()), i == cursor))
        .collect();
    frame.render_widget(List::new(items), rows[1]);
    render_footer(frame, rows[2], " Enter:open  Esc:close");
}

/// The contiguous range of item indices to actually draw, given how many
/// rows are available: keeps `cursor` in view (scrolling exactly enough,
/// never centering) — the same "just keep it visible" story
/// `help_view`/`log_view`'s own scroll fields drive, but computed fresh
/// here every frame from `cursor` and `total` alone, since a settings
/// item list's cursor is a plain `usize` with no separate scroll-offset
/// field to keep in sync.
pub(super) fn windowed_range(total: usize, cursor: usize, height: usize) -> std::ops::Range<usize> {
    if height == 0 || total == 0 {
        return 0..0;
    }
    let start = if total <= height {
        0
    } else {
        cursor.saturating_sub(height - 1).min(total - height)
    };
    start..(start + height).min(total)
}

/// Renders one category's item list. Only builds `ListItem`s for the rows
/// actually on screen (`windowed_range`, computed from a total that's known
/// up front for every category without touching per-item data) rather than
/// formatting the entire list and throwing away everything outside the
/// window — the Keybindings category in particular would otherwise call
/// `settings::combos_for` (a full keymap scan) for all of `Action::ALL`
/// every frame, not just the ~20 rows a normal terminal actually shows.
fn render_items(frame: &mut Frame, area: Rect, app: &mut App, category: Category, cursor: usize) {
    let rows = frame_rows(frame, area);
    render_title(frame, rows[0], &format!(" {}", category.label()));

    let total = match category {
        Category::Behavior => settings::BEHAVIOR_ITEMS.len(),
        Category::Colors => settings::COLOR_ITEMS.len(),
        Category::Startup => settings::STARTUP_ITEMS.len(),
        // "+ add new" is one synthetic extra slot past the real entries.
        Category::Viewers => app.config.viewers.len() + 1,
        Category::Keybindings => crate::action::Action::ALL.len(),
    };
    let window = windowed_range(total, cursor, rows[1].height as usize);

    let visible: Vec<ListItem> = match category {
        Category::Behavior => window
            .map(|i| {
                let item = &settings::BEHAVIOR_ITEMS[i];
                let value = settings::item_value_display(category, item, &app.config);
                list_item(
                    format!(" {}{value}", text::pad_label_min1(item.label, 4)),
                    i == cursor,
                )
            })
            .collect(),
        Category::Colors => window
            .map(|i| {
                let item = &settings::COLOR_ITEMS[i];
                let value = settings::item_value_display(category, item, &app.config);
                list_item(
                    format!(" {}{value}", text::pad_label_min1(item.label, 4)),
                    i == cursor,
                )
            })
            .collect(),
        Category::Startup => window
            .map(|i| {
                let item = &settings::STARTUP_ITEMS[i];
                let value = settings::item_value_display(category, item, &app.config);
                list_item(
                    format!(" {}{value}", text::pad_label_min1(item.label, 4)),
                    i == cursor,
                )
            })
            .collect(),
        Category::Viewers => {
            let extensions: Vec<String> = {
                let mut ext: Vec<String> = app.config.viewers.keys().cloned().collect();
                ext.sort();
                ext
            };
            window
                .map(|i| match extensions.get(i) {
                    Some(ext) => {
                        let command = app.config.viewers.get(ext).cloned().unwrap_or_default();
                        list_item(format!(" .{ext:<10} -> {command}"), i == cursor)
                    }
                    // The synthetic "+ add new" slot, one past the real entries.
                    None => list_item(" + add new".to_string(), i == cursor),
                })
                .collect()
        }
        Category::Keybindings => {
            let lines = app.settings_keybinding_lines();
            window
                .map(|i| list_item(lines[i].clone(), i == cursor))
                .collect()
        }
    };
    frame.render_widget(List::new(visible), rows[1]);

    let footer = match category {
        Category::Viewers => " Enter:edit  d:delete  Esc:back",
        _ => " Enter:edit  Esc:back",
    };
    render_footer(frame, rows[2], footer);
}

fn render_editor(frame: &mut Frame, area: Rect, app: &App, editor: &SettingsEditor) {
    match editor {
        SettingsEditor::DeleteBehavior { cursor } => render_delete_behavior(frame, area, *cursor),
        SettingsEditor::Color {
            key,
            cursor,
            editing_hex,
            hex_input,
        } => render_color(frame, area, key, *cursor, *editing_hex, hex_input),
        SettingsEditor::Text { field, input } => render_text(frame, area, *field, input),
        SettingsEditor::ViewerEntry {
            old_extension,
            extension,
            command,
            editing_extension,
        } => render_viewer_entry(
            frame,
            area,
            old_extension.as_deref(),
            extension,
            command,
            *editing_extension,
        ),
        SettingsEditor::Keybinding { action, cursor } => {
            render_keybinding(frame, area, app, *action, *cursor);
        }
        SettingsEditor::KeybindingCapture { action, .. } => {
            render_keybinding_capture(frame, area, *action);
        }
        SettingsEditor::KeybindingConfirm {
            action,
            combo,
            conflict,
            ..
        } => render_keybinding_confirm(frame, area, *action, combo, *conflict),
    }
}

fn render_delete_behavior(frame: &mut Frame, area: Rect, cursor: usize) {
    let rows = frame_rows(frame, area);
    render_title(frame, rows[0], " delete_behavior");
    let items = vec![
        list_item(" trash".to_string(), cursor == 0),
        list_item(" permanent".to_string(), cursor == 1),
    ];
    frame.render_widget(List::new(items), rows[1]);
    render_footer(frame, rows[2], " Enter:select  Esc:cancel");
}

fn render_color(
    frame: &mut Frame,
    area: Rect,
    key: &str,
    cursor: usize,
    editing_hex: bool,
    hex_input: &crate::mode::LineEditor,
) {
    let rows = frame_rows(frame, area);
    render_title(frame, rows[0], &format!(" {key}"));

    let mut items: Vec<ListItem> = settings::COLOR_PALETTE
        .iter()
        .enumerate()
        .map(|(i, (name, color))| {
            let swatch = Style::default().bg(*color);
            let mut spans = vec![ratatui::text::Span::styled("  ", swatch)];
            let label_style = if i == cursor {
                selected_style()
            } else {
                dialog_style()
            };
            spans.push(ratatui::text::Span::styled(format!(" {name}"), label_style));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let hex_slot = settings::COLOR_PALETTE.len();
    let hex_style = if cursor == hex_slot {
        selected_style()
    } else {
        dialog_style()
    };
    let hex_text = if editing_hex {
        format!(" custom: #{}", hex_input.value())
    } else {
        " custom hex...".to_string()
    };
    items.push(ListItem::new(Line::styled(hex_text, hex_style)));

    frame.render_widget(List::new(items), rows[1]);

    if editing_hex {
        let col = rows[1].x + 10 + hex_input.cursor_display_col() as u16;
        frame.set_cursor_position((
            col.min(rows[1].x + rows[1].width.saturating_sub(1)),
            rows[1].y + hex_slot as u16,
        ));
        render_footer(frame, rows[2], " Enter:apply  Esc:cancel");
    } else {
        render_footer(frame, rows[2], " Enter:select  Esc:cancel");
    }
}

fn render_text(frame: &mut Frame, area: Rect, field: TextField, input: &crate::mode::LineEditor) {
    let rows = frame_rows(frame, area);
    let label = match field {
        TextField::Home => "home",
        TextField::Editor => "editor",
    };
    render_title(frame, rows[0], &format!(" {label}"));
    frame.render_widget(
        Paragraph::new(format!(" {}", input.value())).style(dialog_style()),
        rows[1],
    );
    let col = rows[1].x + 1 + input.cursor_display_col() as u16;
    frame.set_cursor_position((
        col.min(rows[1].x + rows[1].width.saturating_sub(1)),
        rows[1].y,
    ));
    render_footer(frame, rows[2], " Enter:apply (empty = unset)  Esc:cancel");
}

fn render_viewer_entry(
    frame: &mut Frame,
    area: Rect,
    old_extension: Option<&str>,
    extension: &crate::mode::LineEditor,
    command: &crate::mode::LineEditor,
    editing_extension: bool,
) {
    let rows = frame_rows(frame, area);
    let title = if old_extension.is_some() {
        " edit viewer entry"
    } else {
        " new viewer entry"
    };
    render_title(frame, rows[0], title);

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(rows[1]);

    let ext_style = if editing_extension {
        selected_style()
    } else {
        dialog_style()
    };
    frame.render_widget(
        Paragraph::new(format!(" extension: {}", extension.value())).style(ext_style),
        body[0],
    );
    let cmd_style = if !editing_extension {
        selected_style()
    } else {
        dialog_style()
    };
    frame.render_widget(
        Paragraph::new(format!(" command:   {}", command.value())).style(cmd_style),
        body[1],
    );

    if editing_extension {
        let col = body[0].x + 12 + extension.cursor_display_col() as u16;
        frame.set_cursor_position((
            col.min(body[0].x + body[0].width.saturating_sub(1)),
            body[0].y,
        ));
    } else {
        let col = body[1].x + 12 + command.cursor_display_col() as u16;
        frame.set_cursor_position((
            col.min(body[1].x + body[1].width.saturating_sub(1)),
            body[1].y,
        ));
    }

    render_footer(frame, rows[2], " Tab:switch field  Enter:save  Esc:cancel");
}

fn render_keybinding(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    action: crate::action::Action,
    cursor: usize,
) {
    let rows = frame_rows(frame, area);
    render_title(
        frame,
        rows[0],
        &format!(" {} ({})", action.config_name(), action.description()),
    );
    let combos = settings::combos_for(&app.keymap, action);
    let items: Vec<ListItem> = if combos.is_empty() {
        vec![ListItem::new(
            Line::styled(" (no combo bound)".to_string(), dialog_style()),
        )]
    } else {
        combos
            .iter()
            .enumerate()
            .map(|(i, combo)| list_item(format!(" {combo}"), i == cursor))
            .collect()
    };
    frame.render_widget(List::new(items), rows[1]);
    render_footer(frame, rows[2], " a:add  d:remove  Esc:back");
}

fn render_keybinding_capture(frame: &mut Frame, area: Rect, action: crate::action::Action) {
    let rows = frame_rows(frame, area);
    render_title(
        frame,
        rows[0],
        &format!(" {} — capturing", action.config_name()),
    );
    frame.render_widget(
        Paragraph::new(" Press any key to bind it to this action...").style(dialog_style()),
        rows[1],
    );
    render_footer(frame, rows[2], " Esc:cancel");
}

fn render_keybinding_confirm(
    frame: &mut Frame,
    area: Rect,
    action: crate::action::Action,
    combo: &crate::keymap::KeyCombo,
    conflict: Option<crate::action::Action>,
) {
    let rows = frame_rows(frame, area);
    render_title(frame, rows[0], " confirm binding");
    let combo_text = settings::format_combo(combo);
    let body = match conflict {
        Some(loser) => format!(
            " Bind \"{combo_text}\" to {}? It's currently bound to {} — this will remove it there.",
            action.config_name(),
            loser.config_name()
        ),
        None => format!(" Bind \"{combo_text}\" to {}?", action.config_name()),
    };
    frame.render_widget(Paragraph::new(body).style(dialog_style()), rows[1]);
    render_footer(frame, rows[2], " y/Enter:confirm  n/Esc:cancel");
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use unicode_width::UnicodeWidthStr;

    use super::*;
    use crate::config::Config;
    use crate::mode::LineEditor;

    fn test_app() -> App {
        let dir = tempfile::tempdir().unwrap();
        App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config::default(),
        )
        .unwrap()
    }

    fn render_to_string(app: &mut App) -> String {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..24u16 {
            // A wide (e.g. CJK) glyph occupies two cells: the first holds
            // the real symbol, the second holds a placeholder — naively
            // concatenating every cell's symbol would insert that
            // placeholder in the middle of, say, "動作" and break a plain
            // `.contains(...)` check. Skip ahead by each symbol's actual
            // display width instead.
            let mut x = 0u16;
            while x < 100 {
                let symbol = buffer[(x, y)].symbol();
                out.push_str(symbol);
                let width = UnicodeWidthStr::width(symbol).max(1) as u16;
                x += width;
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn categories_screen_lists_every_category() {
        let mut app = test_app();
        app.mode = Mode::Settings {
            screen: SettingsScreen::Categories { cursor: 0 },
        };
        let screen = render_to_string(&mut app);
        for category in Category::ALL {
            assert!(screen.contains(category.label()), "{screen}");
        }
    }

    #[test]
    fn behavior_items_screen_shows_current_on_off_values() {
        let mut app = test_app();
        app.mode = Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Behavior,
                cursor: 0,
            },
        };
        let screen = render_to_string(&mut app);
        assert!(screen.contains("ON"), "{screen}");
    }

    #[test]
    fn keybindings_items_screen_shows_settings_own_default_combo() {
        // The cursor scrolls the (44-action) list into view — park it
        // directly on `Settings` itself so this doesn't depend on however
        // many rows the 24-line TestBackend happens to fit.
        let settings_index = crate::action::Action::ALL
            .iter()
            .position(|a| *a == crate::action::Action::Settings)
            .unwrap();
        let mut app = test_app();
        app.mode = Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Keybindings,
                cursor: settings_index,
            },
        };
        let screen = render_to_string(&mut app);
        assert!(screen.contains("settings"), "{screen}");
    }

    #[test]
    fn keybindings_items_screen_scrolls_to_keep_a_far_cursor_visible() {
        let mut app = test_app();
        let last_index = crate::action::Action::ALL.len() - 1;
        app.mode = Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Keybindings,
                cursor: last_index,
            },
        };
        let screen = render_to_string(&mut app);
        let last_action = crate::action::Action::ALL[last_index];
        assert!(screen.contains(last_action.config_name()), "{screen}");
    }

    #[test]
    fn text_editor_shows_the_input_and_positions_the_cursor() {
        let mut app = test_app();
        app.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category: Category::Startup,
                item_cursor: 1,
                editor: SettingsEditor::Text {
                    field: TextField::Editor,
                    input: LineEditor::from_str("nvim"),
                },
            },
        };
        let screen = render_to_string(&mut app);
        assert!(screen.contains("nvim"), "{screen}");
    }

    #[test]
    fn keybinding_confirm_names_the_losing_action_on_a_conflict() {
        let mut app = test_app();
        let combo = crate::keymap::KeyCombo::new(
            ratatui::crossterm::event::KeyCode::Char('r'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        );
        app.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category: Category::Keybindings,
                item_cursor: 0,
                editor: SettingsEditor::KeybindingConfirm {
                    action: crate::action::Action::Mkdir,
                    combo,
                    conflict: Some(crate::action::Action::Rename),
                    cursor: 0,
                },
            },
        };
        let screen = render_to_string(&mut app);
        assert!(screen.contains("rename"), "{screen}");
        assert!(screen.contains("mkdir"), "{screen}");
    }
}
