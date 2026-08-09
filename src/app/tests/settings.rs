use super::super::test_support::*;
use super::super::*;

#[test]
fn settings_action_opens_the_categories_screen() {
    let mut app = test_app(
        tempfile::tempdir().unwrap().path(),
        tempfile::tempdir().unwrap().path(),
    );
    app.dispatch(Action::Settings);
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Categories { cursor: 0 }
        }
    ));
}

#[test]
fn settings_categories_navigate_and_esc_closes() {
    let mut app = test_app(
        tempfile::tempdir().unwrap().path(),
        tempfile::tempdir().unwrap().path(),
    );
    app.begin_settings();
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Categories { cursor: 1 }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Colors,
                cursor: 0
            }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Categories { cursor: 1 }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn settings_bool_toggle_saves_and_reloads_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert!(app.config.mouse, "mouse defaults to true");

    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Behavior,
            cursor: 3, // mouse
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!app.config.mouse, "toggled off and live-reloaded");
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("mouse = false"), "{text}");
    // Still on the Items screen, not kicked out of it.
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Behavior,
                cursor: 3
            }
        }
    ));
}

#[test]
fn settings_delete_behavior_editor_commits_the_selected_variant() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());
    assert_eq!(app.config.delete_behavior, DeleteBehavior::Trash);

    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Behavior,
            cursor: 4, // delete_behavior
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Editor {
                editor: SettingsEditor::DeleteBehavior { cursor: 0 },
                ..
            }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.config.delete_behavior, DeleteBehavior::Permanent);
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Items {
                category: Category::Behavior,
                cursor: 4
            }
        }
    ));
}

#[test]
fn settings_color_palette_pick_applies_the_named_color() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());

    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Colors,
            cursor: 2, // directory
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    // The editor opens with the cursor parked on the current value
    // (Cyan, `directory`'s default) — walk to "magenta" from there,
    // wherever it lands in the palette, rather than hardcoding an
    // absolute offset from the top.
    let magenta_index = settings::COLOR_PALETTE
        .iter()
        .position(|(_, c)| *c == ratatui::style::Color::Magenta)
        .unwrap();
    let cyan_index = settings::COLOR_PALETTE
        .iter()
        .position(|(_, c)| *c == ratatui::style::Color::Cyan)
        .unwrap();
    let key = if magenta_index >= cyan_index {
        KeyCode::Down
    } else {
        KeyCode::Up
    };
    for _ in 0..magenta_index.abs_diff(cyan_index) {
        app.handle_event(AppEvent::Input(key, KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.config.colors.directory, ratatui::style::Color::Magenta);
}

#[test]
fn settings_color_hex_input_applies_a_custom_color() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Colors,
            item_cursor: 0,
            editor: SettingsEditor::Color {
                key: "cursor",
                cursor: settings::COLOR_PALETTE.len(),
                editing_hex: false,
                hex_input: LineEditor::new(),
            },
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    for c in "112233".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.config.colors.cursor,
        ratatui::style::Color::Rgb(0x11, 0x22, 0x33)
    );
}

#[test]
fn settings_text_editor_sets_and_then_clears_an_optional_field() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());
    assert_eq!(app.config.editor, None);

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Startup,
            item_cursor: 1,
            editor: SettingsEditor::Text {
                field: TextField::Editor,
                input: LineEditor::new(),
            },
        },
    };
    for c in "nvim".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.config.editor, Some("nvim".to_string()));

    // Re-open and clear it back to unset.
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
    for _ in 0.."nvim".len() {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.config.editor, None);
}

#[test]
fn settings_viewer_entry_add_then_delete_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());
    assert!(app.config.viewers.is_empty());

    // "+ add new" is cursor 0 when the map is empty.
    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Viewers,
            cursor: 0,
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    for c in "md".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Tab, KeyModifiers::NONE));
    for c in "glow {}".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.config.viewers.get("md"), Some(&"glow {}".to_string()));

    // Delete it straight from the item list.
    let Mode::Settings {
        screen: SettingsScreen::Items { cursor, .. },
    } = app.mode
    else {
        panic!("expected Items screen after committing, got {:?}", app.mode);
    };
    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Viewers,
            cursor,
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(!app.config.viewers.contains_key("md"));
}

#[test]
fn settings_keybinding_add_writes_bindings_and_applies_live() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('x'), KeyModifiers::NONE),
        None
    );

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Keybindings,
            item_cursor: 0,
            editor: SettingsEditor::Keybinding {
                action: Action::Mkdir,
                cursor: 0,
            },
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Editor {
                editor: SettingsEditor::KeybindingCapture { .. },
                ..
            }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Editor {
                editor: SettingsEditor::KeybindingConfirm { conflict: None, .. },
                ..
            }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.keymap.resolve(KeyCode::Char('x'), KeyModifiers::NONE),
        Some(Action::Mkdir)
    );
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[bindings]"), "{text}");
    assert!(text.contains("mkdir"), "{text}");
}

#[test]
fn settings_keybinding_remove_a_default_combo_writes_keys_none() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    // "r" is a compiled-in default for Rename.
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('r'), KeyModifiers::NONE),
        Some(Action::Rename)
    );
    let cursor = settings::combos_for(&app.keymap, Action::Rename)
        .iter()
        .position(|c| c == "r")
        .unwrap();

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Keybindings,
            item_cursor: 0,
            editor: SettingsEditor::Keybinding {
                action: Action::Rename,
                cursor,
            },
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(
        app.keymap.resolve(KeyCode::Char('r'), KeyModifiers::NONE),
        None,
        "removed"
    );
    // "R"/S-r must survive untouched (only "r" was removed).
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('R'), KeyModifiers::SHIFT),
        Some(Action::Rename)
    );
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[keys]"), "{text}");
}

#[test]
fn settings_keybinding_capture_conflict_steal_moves_the_combo() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _config_path) = settings_test_app(dir.path());
    // "r" defaults to Rename; capture it for Mkdir and steal it.
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('r'), KeyModifiers::NONE),
        Some(Action::Rename)
    );

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Keybindings,
            item_cursor: 0,
            editor: SettingsEditor::Keybinding {
                action: Action::Mkdir,
                cursor: 0,
            },
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Char('r'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Editor {
                editor: SettingsEditor::KeybindingConfirm {
                    conflict: Some(Action::Rename),
                    ..
                },
                ..
            }
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));

    assert_eq!(
        app.keymap.resolve(KeyCode::Char('r'), KeyModifiers::NONE),
        Some(Action::Mkdir),
        "stolen"
    );
}

#[test]
fn settings_keybinding_capture_esc_cancels_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());

    app.mode = Mode::Settings {
        screen: SettingsScreen::Editor {
            category: Category::Keybindings,
            item_cursor: 0,
            editor: SettingsEditor::Keybinding {
                action: Action::Mkdir,
                cursor: 0,
            },
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        Mode::Settings {
            screen: SettingsScreen::Editor {
                editor: SettingsEditor::Keybinding {
                    action: Action::Mkdir,
                    cursor: 0
                },
                ..
            }
        }
    ));
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(text, "", "Esc during capture must never write anything");
}

#[test]
fn settings_toggle_persists_cursor_memory_and_pushes_it_onto_both_panes() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert!(app.config.cursor_memory, "defaults to on");
    assert!(app.panes.iter().all(|p| p.cursor_memory_enabled));

    let cursor = settings::BEHAVIOR_ITEMS
        .iter()
        .position(|item| item.key == "cursor_memory")
        .unwrap();
    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Behavior,
            cursor,
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!app.config.cursor_memory, "toggled and reloaded");
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("cursor_memory = false"), "{text}");
    // The reload has to reach the panes — they never read config
    // themselves, so a toggle that stops at `app.config` would leave the
    // feature running.
    assert!(app.panes.iter().all(|p| !p.cursor_memory_enabled));
}

#[test]
fn settings_toggle_persists_clear_on_suspend() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert!(app.config.clear_on_suspend, "defaults to on");

    let cursor = settings::BEHAVIOR_ITEMS
        .iter()
        .position(|item| item.key == "clear_on_suspend")
        .unwrap();
    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Behavior,
            cursor,
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!app.config.clear_on_suspend, "toggled and reloaded");
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("clear_on_suspend = false"), "{text}");
}
