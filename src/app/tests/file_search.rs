use super::super::test_support::*;
use super::super::*;

fn results_len(app: &App) -> usize {
    match &app.mode {
        Mode::FileSearch { results, .. } => results.len(),
        other => panic!("expected Mode::FileSearch, got {other:?}"),
    }
}

fn error_of(app: &App) -> Option<String> {
    match &app.mode {
        Mode::FileSearch { error, .. } => error.clone(),
        other => panic!("expected Mode::FileSearch, got {other:?}"),
    }
}

/// `left/alpha.txt`, `left/sub/`, `left/sub/dir/`, `left/sub/dir/target.txt`.
fn make_nested(dir: &std::path::Path) {
    std::fs::write(dir.join("alpha.txt"), b"").unwrap();
    std::fs::create_dir_all(dir.join("sub").join("dir")).unwrap();
    std::fs::write(dir.join("sub").join("dir").join("target.txt"), b"").unwrap();
}

#[test]
fn file_search_opens_with_every_entry_listed() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    // alpha.txt, sub, sub/dir, sub/dir/target.txt
    assert_eq!(results_len(&app), 4);
}

#[test]
fn incremental_typing_narrows_results_on_every_keystroke() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "targ");
    assert_eq!(results_len(&app), 1);
    // Case-insensitive substring, same grammar as the pane filter.
    app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    type_into_viewer(&mut app, "G");
    assert_eq!(results_len(&app), 1);
}

#[test]
fn enter_jumps_to_the_hit_with_the_cursor_on_it() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "target");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.active_pane().cwd, dir.path().join("sub").join("dir"));
    assert_eq!(cursor_entry_name(&app), "target.txt");
}

#[test]
fn a_directory_hit_jumps_to_its_parent_with_the_cursor_on_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "dir");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.active_pane().cwd, dir.path().join("sub"));
    assert_eq!(cursor_entry_name(&app), "dir");
}

#[test]
fn esc_cancels_without_moving_the_pane() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "target");
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.active_pane().cwd, dir.path());
}

#[test]
fn hidden_entries_follow_the_panes_show_hidden_flag() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("visible.txt"), b"").unwrap();
    std::fs::create_dir(dir.path().join(".hidden")).unwrap();
    std::fs::write(dir.path().join(".hidden").join("inside.txt"), b"").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    assert_eq!(results_len(&app), 1, "dotfiles pruned by default");
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    app.dispatch(Action::ToggleHidden);
    app.dispatch(Action::FileSearch);
    // .hidden, .hidden/inside.txt, visible.txt
    assert_eq!(results_len(&app), 3);
}

#[test]
fn enter_run_mode_searches_on_enter_then_jumps_on_the_next_enter() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let left = dir.path();
    let config = Config {
        file_search_incremental: false,
        ..Config::default()
    };
    let mut app = App::new(left.to_path_buf(), left.to_path_buf(), config).unwrap();

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "target");
    // Edits alone never re-search in this mode.
    assert_eq!(results_len(&app), 4, "results lag the input until Enter");

    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(results_len(&app), 1, "first Enter runs the search");

    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal), "second Enter jumps");
    assert_eq!(cursor_entry_name(&app), "target.txt");
}

#[test]
fn an_invalid_regex_matches_nothing_and_surfaces_the_error() {
    let dir = tempfile::tempdir().unwrap();
    make_nested(dir.path());
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::FileSearch);
    type_into_viewer(&mut app, "re:[");
    assert_eq!(results_len(&app), 0);
    assert!(error_of(&app).is_some());
    // Deleting back to a valid query clears the error again.
    app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(error_of(&app).is_none());
}

#[test]
fn file_search_is_rejected_in_a_virtual_pane() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    select_entry_named(&mut app, "project.zip");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());

    app.dispatch(Action::FileSearch);
    assert!(matches!(app.mode, Mode::Normal), "must not open the popup");
}

#[test]
fn settings_toggle_persists_file_search_incremental() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert!(app.config.file_search_incremental, "defaults to true");

    let cursor = settings::BEHAVIOR_ITEMS
        .iter()
        .position(|item| item.key == "file_search_incremental")
        .unwrap();
    app.mode = Mode::Settings {
        screen: SettingsScreen::Items {
            category: Category::Behavior,
            cursor,
        },
    };
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!app.config.file_search_incremental, "toggled and reloaded");
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("file_search_incremental = false"), "{text}");
}
