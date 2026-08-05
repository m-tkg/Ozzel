//! `Mode::Settings` input handling: the categories/items menu, each
//! editor variant, and the `toml_edit`-based write-back to the config
//! file. Split out of `app/mod.rs` (Phase 6, Step 2).

use super::*;

impl App {
    /// The settings screen's Keybindings category listing — one formatted
    /// `" {action:<20} {combos}"` line per `Action::ALL` entry — from a
    /// cache keyed by `Keymap::generation`, same story as `App::help_lines`:
    /// `settings::combos_for` scans the whole keymap per action, and
    /// without this it was doing that for all ~44 actions on every single
    /// frame the Keybindings list is on screen, not just when a binding
    /// actually changed. `pub(crate)` so `ui::settings_view` can call it.
    pub(crate) fn settings_keybinding_lines(&mut self) -> &[String] {
        let generation = self.keymap.generation();
        let stale = match &self.settings_keybinding_lines_cache {
            Some((cached_gen, _)) => *cached_gen != generation,
            None => true,
        };
        if stale {
            let lines = Action::ALL
                .iter()
                .map(|action| {
                    let combos = settings::combos_for(&self.keymap, *action).join(", ");
                    format!(" {:<20} {combos}", action.config_name())
                })
                .collect();
            self.settings_keybinding_lines_cache = Some((generation, lines));
        }
        &self.settings_keybinding_lines_cache.as_ref().unwrap().1
    }

    /// `S`/`S-s`: opens the settings screen at the top-level category menu.
    pub(super) fn begin_settings(&mut self) {
        self.mode = Mode::Settings {
            screen: SettingsScreen::Categories { cursor: 0 },
        };
    }

    /// The real config file's path, honoring `settings_config_path`'s
    /// test override — see its doc comment. Every settings-screen write
    /// goes through this, never `config::config_path()` directly.
    fn settings_path(&self) -> Option<PathBuf> {
        self.settings_config_path
            .clone()
            .or_else(config::config_path)
    }

    /// Runs `write` against the real config path, then — on success —
    /// live-reloads exactly like `,` (edit_config) does
    /// (`reload_config_from`), so a settings-screen edit takes effect
    /// immediately. Every commit in the settings screen (a bool flip, a
    /// select/palette/hex pick, a text field's Enter, a keybinding add/
    /// remove) goes through this one function, so "write, then reload,
    /// with a write or reload failure just logging and otherwise leaving
    /// the running config untouched" only has to be implemented once.
    /// Since the screen's item list always displays live values read
    /// straight from `self.config` (never a locally-cached copy), a
    /// reload failure here automatically shows up as "the value on
    /// screen didn't change" — the coordinator's "revert UI value" for
    /// free, with no separate rollback code needed.
    fn settings_save(&mut self, write: impl FnOnce(&Path) -> anyhow::Result<()>) {
        let Some(path) = self.settings_path() else {
            self.log_error("could not determine the config file location on this platform");
            return;
        };
        if let Err(err) = write(&path) {
            self.log_error(format!("settings: failed to save: {err}"));
            return;
        }
        self.reload_config_from(&path);
    }

    /// Item-list length for `category` — see `SettingsScreen::Items`'s doc
    /// comment for what each category's list actually contains.
    fn settings_item_count(&self, category: Category) -> usize {
        match category {
            Category::Behavior => settings::BEHAVIOR_ITEMS.len(),
            Category::Colors => settings::COLOR_ITEMS.len(),
            Category::Startup => settings::STARTUP_ITEMS.len(),
            // +1 for the synthetic "+ add new" slot at the end.
            Category::Viewers => self.config.viewers.len() + 1,
            Category::Keybindings => Action::ALL.len(),
        }
    }

    /// `[viewers]`'s extensions, sorted — the `Viewers` category's item
    /// list is this plus one synthetic "+ add new" slot at the end (index
    /// `== extensions.len()`, handled specially by every caller since it
    /// has no backing config entry).
    fn settings_viewer_extensions(&self) -> Vec<String> {
        let mut extensions: Vec<String> = self.config.viewers.keys().cloned().collect();
        extensions.sort();
        extensions
    }

    fn settings_color_value(&self, key: &str) -> ratatui::style::Color {
        match key {
            "cursor" => self.config.colors.cursor,
            "cursor_inactive" => self.config.colors.cursor_inactive,
            "directory" => self.config.colors.directory,
            "hidden" => self.config.colors.hidden,
            "executable" => self.config.colors.executable,
            _ => unreachable!("unknown [colors] key {key:?}"),
        }
    }

    fn settings_back_to_items(&mut self, category: Category, item_cursor: usize) {
        self.mode = Mode::Settings {
            screen: SettingsScreen::Items {
                category,
                cursor: item_cursor,
            },
        };
    }

    /// Fixed keys for `Mode::Settings`; never consults the keymap, the
    /// same "this full-frame screen owns its own keys" story as
    /// Viewer/Help/Log/FunctionList. Dispatches on which level
    /// (`SettingsScreen`) is currently showing.
    pub(super) fn handle_settings_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Mode::Settings { screen } = &self.mode else {
            return;
        };
        match screen.clone() {
            SettingsScreen::Categories { cursor } => {
                self.handle_settings_categories_key(code, cursor);
            }
            SettingsScreen::Items { category, cursor } => {
                self.handle_settings_items_key(code, category, cursor);
            }
            SettingsScreen::Editor {
                category,
                item_cursor,
                editor,
            } => {
                self.handle_settings_editor_key(code, modifiers, category, item_cursor, editor);
            }
        }
    }

    fn handle_settings_categories_key(&mut self, code: KeyCode, cursor: usize) {
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Categories {
                        cursor: cursor.saturating_sub(1),
                    },
                };
            }
            KeyCode::Down => {
                let next = (cursor + 1).min(Category::ALL.len() - 1);
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Categories { cursor: next },
                };
            }
            KeyCode::Enter => {
                let category = Category::ALL[cursor];
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Items {
                        category,
                        cursor: 0,
                    },
                };
            }
            _ => {}
        }
    }

    fn handle_settings_items_key(&mut self, code: KeyCode, category: Category, cursor: usize) {
        let count = self.settings_item_count(category);
        match code {
            KeyCode::Esc => {
                let cat_cursor = Category::ALL
                    .iter()
                    .position(|&c| c == category)
                    .unwrap_or(0);
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Categories { cursor: cat_cursor },
                };
            }
            KeyCode::Up => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Items {
                        category,
                        cursor: cursor.saturating_sub(1),
                    },
                };
            }
            KeyCode::Down => {
                let next = if count == 0 {
                    0
                } else {
                    (cursor + 1).min(count - 1)
                };
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Items {
                        category,
                        cursor: next,
                    },
                };
            }
            KeyCode::Char('d') if category == Category::Viewers => {
                self.settings_delete_viewer_entry(category, cursor);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.settings_open_item(category, cursor);
            }
            _ => {}
        }
    }

    fn settings_open_item(&mut self, category: Category, cursor: usize) {
        match category {
            Category::Behavior => {
                if let Some(item) = settings::BEHAVIOR_ITEMS.get(cursor).copied() {
                    self.settings_open_bool_or_enum_item(category, cursor, item);
                }
            }
            Category::Colors => {
                if let Some(item) = settings::COLOR_ITEMS.get(cursor).copied() {
                    self.settings_open_color_item(category, cursor, item);
                }
            }
            Category::Startup => {
                if let Some(item) = settings::STARTUP_ITEMS.get(cursor).copied() {
                    self.settings_open_startup_item(category, cursor, item);
                }
            }
            Category::Viewers => self.settings_open_viewer_item(category, cursor),
            Category::Keybindings => self.settings_open_keybinding_item(category, cursor),
        }
    }

    fn settings_open_bool_or_enum_item(
        &mut self,
        category: Category,
        cursor: usize,
        item: settings::Item,
    ) {
        match item.kind {
            settings::ItemKind::Bool => {
                let current = match item.key {
                    "confirm_operations" => self.config.confirm_operations,
                    "confirm_quit" => self.config.confirm_quit,
                    "quit_cd" => self.config.quit_cd,
                    "mouse" => self.config.mouse,
                    "show_permissions" => self.config.show_permissions,
                    "dim_inactive" => self.config.colors.dim_inactive,
                    _ => unreachable!("unknown bool key {:?}", item.key),
                };
                let key = item.key;
                self.settings_save(|path| settings::save_bool(path, category, key, !current));
            }
            settings::ItemKind::DeleteBehaviorEnum => {
                let cursor_pos = match self.config.delete_behavior {
                    DeleteBehavior::Trash => 0,
                    DeleteBehavior::Permanent => 1,
                };
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor: cursor,
                        editor: SettingsEditor::DeleteBehavior { cursor: cursor_pos },
                    },
                };
            }
            settings::ItemKind::Color | settings::ItemKind::OptionalText => {
                unreachable!("Behavior items are always Bool or DeleteBehaviorEnum")
            }
        }
    }

    fn settings_open_color_item(
        &mut self,
        category: Category,
        cursor: usize,
        item: settings::Item,
    ) {
        let current = self.settings_color_value(item.key);
        let palette_index = settings::COLOR_PALETTE
            .iter()
            .position(|(_, c)| *c == current)
            .unwrap_or(settings::COLOR_PALETTE.len());
        self.settings_set_color_editor(
            category,
            cursor,
            item.key,
            palette_index,
            false,
            LineEditor::new(),
        );
    }

    fn settings_set_color_editor(
        &mut self,
        category: Category,
        item_cursor: usize,
        key: &'static str,
        cursor: usize,
        editing_hex: bool,
        hex_input: LineEditor,
    ) {
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor,
                editor: SettingsEditor::Color {
                    key,
                    cursor,
                    editing_hex,
                    hex_input,
                },
            },
        };
    }

    fn settings_open_startup_item(
        &mut self,
        category: Category,
        cursor: usize,
        item: settings::Item,
    ) {
        let field = match item.key {
            "home" => TextField::Home,
            "editor" => TextField::Editor,
            _ => unreachable!("unknown startup key {:?}", item.key),
        };
        let current = match field {
            TextField::Home => self
                .config
                .home
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            TextField::Editor => self.config.editor.clone().unwrap_or_default(),
        };
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor: cursor,
                editor: SettingsEditor::Text {
                    field,
                    input: LineEditor::from_str(&current),
                },
            },
        };
    }

    fn settings_open_viewer_item(&mut self, category: Category, cursor: usize) {
        let extensions = self.settings_viewer_extensions();
        let editor = if let Some(ext) = extensions.get(cursor) {
            let command = self.config.viewers.get(ext).cloned().unwrap_or_default();
            SettingsEditor::ViewerEntry {
                old_extension: Some(ext.clone()),
                extension: LineEditor::from_str(ext),
                command: LineEditor::from_str(&command),
                editing_extension: false,
            }
        } else {
            SettingsEditor::ViewerEntry {
                old_extension: None,
                extension: LineEditor::new(),
                command: LineEditor::new(),
                editing_extension: true,
            }
        };
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor: cursor,
                editor,
            },
        };
    }

    fn settings_delete_viewer_entry(&mut self, category: Category, cursor: usize) {
        let extensions = self.settings_viewer_extensions();
        let Some(ext) = extensions.get(cursor).cloned() else {
            return;
        };
        self.settings_save(|path| settings::remove_viewer_entry(path, &ext));
        let new_count = self.settings_item_count(category);
        let clamped = cursor.min(new_count.saturating_sub(1));
        self.mode = Mode::Settings {
            screen: SettingsScreen::Items {
                category,
                cursor: clamped,
            },
        };
    }

    fn settings_open_keybinding_item(&mut self, category: Category, cursor: usize) {
        let Some(&action) = Action::ALL.get(cursor) else {
            return;
        };
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor: cursor,
                editor: SettingsEditor::Keybinding { action, cursor: 0 },
            },
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_editor_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        category: Category,
        item_cursor: usize,
        editor: SettingsEditor,
    ) {
        match editor {
            SettingsEditor::DeleteBehavior { cursor } => {
                self.handle_settings_delete_behavior_key(code, category, item_cursor, cursor);
            }
            SettingsEditor::Color {
                key,
                cursor,
                editing_hex,
                hex_input,
            } => {
                self.handle_settings_color_key(
                    code,
                    modifiers,
                    category,
                    item_cursor,
                    key,
                    cursor,
                    editing_hex,
                    hex_input,
                );
            }
            SettingsEditor::Text { field, input } => {
                self.handle_settings_text_key(code, modifiers, category, item_cursor, field, input);
            }
            SettingsEditor::ViewerEntry {
                old_extension,
                extension,
                command,
                editing_extension,
            } => {
                self.handle_settings_viewer_entry_key(
                    code,
                    modifiers,
                    category,
                    item_cursor,
                    old_extension,
                    extension,
                    command,
                    editing_extension,
                );
            }
            SettingsEditor::Keybinding { action, cursor } => {
                self.handle_settings_keybinding_key(code, category, item_cursor, action, cursor);
            }
            SettingsEditor::KeybindingCapture { action, cursor } => {
                self.handle_settings_keybinding_capture_key(
                    code,
                    modifiers,
                    category,
                    item_cursor,
                    action,
                    cursor,
                );
            }
            SettingsEditor::KeybindingConfirm {
                action,
                combo,
                conflict,
                cursor,
            } => {
                self.handle_settings_keybinding_confirm_key(
                    code,
                    category,
                    item_cursor,
                    action,
                    combo,
                    conflict,
                    cursor,
                );
            }
        }
    }

    fn handle_settings_delete_behavior_key(
        &mut self,
        code: KeyCode,
        category: Category,
        item_cursor: usize,
        cursor: usize,
    ) {
        match code {
            KeyCode::Esc => self.settings_back_to_items(category, item_cursor),
            KeyCode::Up | KeyCode::Down => {
                let next = if cursor == 0 { 1 } else { 0 };
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::DeleteBehavior { cursor: next },
                    },
                };
            }
            KeyCode::Enter => {
                let value = if cursor == 0 {
                    DeleteBehavior::Trash
                } else {
                    DeleteBehavior::Permanent
                };
                self.settings_save(|path| settings::save_delete_behavior(path, value));
                self.settings_back_to_items(category, item_cursor);
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_color_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        category: Category,
        item_cursor: usize,
        key: &'static str,
        cursor: usize,
        editing_hex: bool,
        mut hex_input: LineEditor,
    ) {
        if editing_hex {
            match code {
                KeyCode::Esc => {
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        false,
                        LineEditor::new(),
                    );
                }
                KeyCode::Enter => {
                    let raw = hex_input.value();
                    let text = if raw.starts_with('#') {
                        raw.clone()
                    } else {
                        format!("#{raw}")
                    };
                    match crate::color::parse_color(&text) {
                        Ok(value) => {
                            self.settings_save(|path| settings::save_color(path, key, value));
                            self.settings_back_to_items(category, item_cursor);
                        }
                        Err(err) => {
                            self.log_error(format!("settings: invalid color {raw:?}: {err}"));
                        }
                    }
                }
                KeyCode::Backspace => {
                    hex_input.backspace();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::Delete => {
                    hex_input.delete();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::Left => {
                    hex_input.move_left();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::Right => {
                    hex_input.move_right();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::Home => {
                    hex_input.move_home();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::End => {
                    hex_input.move_end();
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    hex_input.insert(c);
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        hex_input,
                    );
                }
                _ => {}
            }
            return;
        }

        let hex_slot = settings::COLOR_PALETTE.len();
        match code {
            KeyCode::Esc => self.settings_back_to_items(category, item_cursor),
            KeyCode::Up => {
                self.settings_set_color_editor(
                    category,
                    item_cursor,
                    key,
                    cursor.saturating_sub(1),
                    false,
                    LineEditor::new(),
                );
            }
            KeyCode::Down => {
                let next = (cursor + 1).min(hex_slot);
                self.settings_set_color_editor(
                    category,
                    item_cursor,
                    key,
                    next,
                    false,
                    LineEditor::new(),
                );
            }
            KeyCode::Enter => {
                if cursor == hex_slot {
                    self.settings_set_color_editor(
                        category,
                        item_cursor,
                        key,
                        cursor,
                        true,
                        LineEditor::new(),
                    );
                } else if let Some((_, value)) = settings::COLOR_PALETTE.get(cursor).copied() {
                    self.settings_save(|path| settings::save_color(path, key, value));
                    self.settings_back_to_items(category, item_cursor);
                }
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_text_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        category: Category,
        item_cursor: usize,
        field: TextField,
        mut input: LineEditor,
    ) {
        match code {
            KeyCode::Esc => {
                self.settings_back_to_items(category, item_cursor);
                return;
            }
            KeyCode::Enter => {
                let value = input.value();
                let key = match field {
                    TextField::Home => "home",
                    TextField::Editor => "editor",
                };
                self.settings_save(|path| settings::save_optional_text(path, key, &value));
                self.settings_back_to_items(category, item_cursor);
                return;
            }
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.move_left(),
            KeyCode::Right => input.move_right(),
            KeyCode::Home => input.move_home(),
            KeyCode::End => input.move_end(),
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
            _ => {}
        }
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor,
                editor: SettingsEditor::Text { field, input },
            },
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_viewer_entry_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        category: Category,
        item_cursor: usize,
        old_extension: Option<String>,
        mut extension: LineEditor,
        mut command: LineEditor,
        editing_extension: bool,
    ) {
        match code {
            KeyCode::Esc => {
                self.settings_back_to_items(category, item_cursor);
                return;
            }
            KeyCode::Tab => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::ViewerEntry {
                            old_extension,
                            extension,
                            command,
                            editing_extension: !editing_extension,
                        },
                    },
                };
                return;
            }
            KeyCode::Enter => {
                let ext = extension.value();
                let cmd = command.value();
                if ext.trim().is_empty() || cmd.trim().is_empty() {
                    self.log_error("settings: extension and command must both be non-empty");
                    return;
                }
                let old = old_extension.clone();
                self.settings_save(|path| {
                    settings::save_viewer_entry_renaming(path, old.as_deref(), &ext, &cmd)
                });
                let extensions = self.settings_viewer_extensions();
                let new_cursor = extensions.iter().position(|e| e == &ext).unwrap_or(0);
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Items {
                        category,
                        cursor: new_cursor,
                    },
                };
                return;
            }
            _ => {}
        }

        let field = if editing_extension {
            &mut extension
        } else {
            &mut command
        };
        match code {
            KeyCode::Backspace => field.backspace(),
            KeyCode::Delete => field.delete(),
            KeyCode::Left => field.move_left(),
            KeyCode::Right => field.move_right(),
            KeyCode::Home => field.move_home(),
            KeyCode::End => field.move_end(),
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => field.insert(c),
            _ => {}
        }
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor,
                editor: SettingsEditor::ViewerEntry {
                    old_extension,
                    extension,
                    command,
                    editing_extension,
                },
            },
        };
    }

    fn handle_settings_keybinding_key(
        &mut self,
        code: KeyCode,
        category: Category,
        item_cursor: usize,
        action: Action,
        cursor: usize,
    ) {
        let combos = settings::combos_for(&self.keymap, action);
        match code {
            KeyCode::Esc => self.settings_back_to_items(category, item_cursor),
            KeyCode::Up => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::Keybinding {
                            action,
                            cursor: cursor.saturating_sub(1),
                        },
                    },
                };
            }
            KeyCode::Down => {
                let next = if combos.is_empty() {
                    0
                } else {
                    (cursor + 1).min(combos.len() - 1)
                };
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::Keybinding {
                            action,
                            cursor: next,
                        },
                    },
                };
            }
            KeyCode::Char('a') => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::KeybindingCapture { action, cursor },
                    },
                };
            }
            KeyCode::Char('d') => {
                if let Some(combo) = combos.get(cursor).cloned() {
                    self.settings_save(|path| settings::remove_binding(path, action, &combo));
                }
                let new_combos = settings::combos_for(&self.keymap, action);
                let clamped = if new_combos.is_empty() {
                    0
                } else {
                    cursor.min(new_combos.len() - 1)
                };
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::Keybinding {
                            action,
                            cursor: clamped,
                        },
                    },
                };
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_keybinding_capture_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        category: Category,
        item_cursor: usize,
        action: Action,
        cursor: usize,
    ) {
        if code == KeyCode::Esc {
            self.mode = Mode::Settings {
                screen: SettingsScreen::Editor {
                    category,
                    item_cursor,
                    editor: SettingsEditor::Keybinding { action, cursor },
                },
            };
            return;
        }
        let combo = KeyCombo::new(code, modifiers);
        if !settings::combo_is_representable(combo) {
            self.log_error("settings: that key can't be captured as a binding");
            self.mode = Mode::Settings {
                screen: SettingsScreen::Editor {
                    category,
                    item_cursor,
                    editor: SettingsEditor::Keybinding { action, cursor },
                },
            };
            return;
        }
        let conflict = settings::conflicting_action(&self.keymap, combo, action);
        self.mode = Mode::Settings {
            screen: SettingsScreen::Editor {
                category,
                item_cursor,
                editor: SettingsEditor::KeybindingConfirm {
                    action,
                    combo,
                    conflict,
                    cursor,
                },
            },
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_settings_keybinding_confirm_key(
        &mut self,
        code: KeyCode,
        category: Category,
        item_cursor: usize,
        action: Action,
        combo: KeyCombo,
        conflict: Option<Action>,
        cursor: usize,
    ) {
        match code {
            KeyCode::Enter | KeyCode::Char('y') => {
                let combo_str = settings::format_combo(&combo);
                if let Some(loser) = conflict {
                    self.settings_save(|path| settings::remove_binding(path, loser, &combo_str));
                }
                self.settings_save(|path| settings::add_binding(path, action, &combo_str));
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::Keybinding { action, cursor },
                    },
                };
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.mode = Mode::Settings {
                    screen: SettingsScreen::Editor {
                        category,
                        item_cursor,
                        editor: SettingsEditor::Keybinding { action, cursor },
                    },
                };
            }
            _ => {}
        }
    }
}
