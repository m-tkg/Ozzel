//! Group-1 (sort & display) app-level tests: the sort dialog (`t`),
//! per-directory sort memory, the size-format toggle (`v`), and the
//! directory-size task's event routing (`z`).

use super::super::test_support::*;
use super::super::*;

#[test]
fn sort_dialog_opens_preselecting_the_current_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::SortDialog);
    match &app.mode {
        Mode::SortSelect { cursor } => assert_eq!(*cursor, 0, "(Name, asc) is row 0"),
        other => panic!("expected SortSelect, got {other:?}"),
    }

    // Reopen after a state change: the cursor lands on the current row.
    app.mode = Mode::Normal;
    app.active_pane_mut().set_sort(SortKey::Size, false);
    app.dispatch(Action::SortDialog);
    match &app.mode {
        Mode::SortSelect { cursor } => assert_eq!(*cursor, 3, "(Size, desc) is row 3"),
        other => panic!("expected SortSelect, got {other:?}"),
    }
}

#[test]
fn sort_dialog_moves_on_the_keymap_cursor_keys_and_still_wraps() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["n".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();

    app.dispatch(Action::SortDialog);
    for _ in 0..3 {
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    }
    match &app.mode {
        Mode::SortSelect { cursor } => assert_eq!(*cursor, 3),
        other => panic!("expected SortSelect, got {other:?}"),
    }

    // Past the last row the dialog wraps, exactly as the arrows do.
    for _ in 3..App::SORT_DIALOG_CHOICES.len() {
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    }
    match &app.mode {
        Mode::SortSelect { cursor } => assert_eq!(*cursor, 0, "keymap nav must wrap too"),
        other => panic!("expected SortSelect, got {other:?}"),
    }
}

#[test]
fn sort_dialog_enter_applies_and_records_the_pref() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::SortDialog);
    for _ in 0..3 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.active_pane().sort, SortKey::Size);
    assert!(!app.active_pane().ascending);
    let cwd = app.active_pane().cwd.clone();
    assert_eq!(app.sort_prefs.get(&cwd), Some(("size", false)));
}

#[test]
fn sort_dialog_esc_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::SortDialog);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.active_pane().sort, SortKey::Name);
    assert!(app.active_pane().ascending);
    let cwd = app.active_pane().cwd.clone();
    assert_eq!(app.sort_prefs.get(&cwd), None);
}

#[test]
fn cycle_sort_records_the_pref_too() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::CycleSort); // Name -> Size
    let cwd = app.active_pane().cwd.clone();
    assert_eq!(app.sort_prefs.get(&cwd), Some(("size", true)));
}

#[test]
fn navigating_into_a_directory_restores_its_remembered_sort() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(dir.path(), dir.path());

    // A previously-remembered pref for `sub` (as if from a past session).
    app.sort_prefs.record(sub.clone(), "mtime", false);

    select_entry_named(&mut app, "sub");
    app.dispatch(Action::Open);

    assert_eq!(app.active_pane().cwd, sub);
    assert_eq!(app.active_pane().sort, SortKey::MTime);
    assert!(!app.active_pane().ascending);
}

#[test]
fn navigating_without_a_pref_keeps_the_panes_current_sort() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().set_sort(SortKey::Ext, false);

    select_entry_named(&mut app, "sub");
    app.dispatch(Action::Open);

    assert_eq!(app.active_pane().cwd, sub);
    assert_eq!(
        app.active_pane().sort,
        SortKey::Ext,
        "no pref -> sort follows the pane, the pre-existing behavior"
    );
}

#[test]
fn apply_startup_sort_prefs_covers_both_panes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let cwd = app.panes[0].cwd.clone();
    app.sort_prefs.record(cwd, "ext", false);

    app.apply_startup_sort_prefs();
    assert_eq!(app.panes[0].sort, SortKey::Ext);
    assert!(!app.panes[0].ascending);
    assert_eq!(app.panes[1].sort, SortKey::Ext, "same cwd, same pref");
}

#[test]
fn toggle_size_format_cycles_and_persists_to_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, config_path) = settings_test_app(dir.path());
    assert_eq!(app.config.size_format, crate::config::SizeFormat::Human);

    app.dispatch(Action::ToggleSizeFormat);
    assert_eq!(app.config.size_format, crate::config::SizeFormat::Bytes);
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("size_format = \"bytes\""), "{text}");

    app.dispatch(Action::ToggleSizeFormat);
    app.dispatch(Action::ToggleSizeFormat);
    assert_eq!(
        app.config.size_format,
        crate::config::SizeFormat::Human,
        "three presses cycle back around"
    );
    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("size_format = \"human\""), "{text}");
}

#[test]
fn calc_dir_size_task_stamps_the_pane_and_logs_a_total() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("f.bin"), vec![0u8; 256]).unwrap();
    let mut app = test_app(dir.path(), dir.path());

    select_entry_named(&mut app, "sub");
    app.dispatch(Action::CalcDirSize);
    wait_for_tasks_done(&mut app);

    assert_eq!(
        app.active_pane().dir_size_overrides.get(&sub),
        Some(&256),
        "the DirSize event must land on the requesting pane"
    );
    assert!(
        app.log
            .iter()
            .any(|l| !l.is_error && l.message.contains("total 256 bytes")),
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[test]
fn calc_dir_size_on_a_file_only_selection_logs_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    select_entry_named(&mut app, "plain.txt");
    app.dispatch(Action::CalcDirSize);
    assert!(app.tasks.running.is_empty(), "no task for a plain file");
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("no directory selected"))
    );
}

#[test]
fn sort_dialog_choices_and_labels_stay_index_aligned() {
    assert_eq!(
        App::SORT_DIALOG_CHOICES.len(),
        crate::ui::modal::SORT_DIALOG_LABELS.len()
    );
    for (i, (key, ascending)) in App::SORT_DIALOG_CHOICES.iter().enumerate() {
        let label = crate::ui::modal::SORT_DIALOG_LABELS[i];
        assert!(
            label.starts_with(key.as_str()),
            "label {label:?} must start with {:?}",
            key.as_str()
        );
        let expected_dir = if *ascending {
            "ascending"
        } else {
            "descending"
        };
        assert!(
            label.ends_with(expected_dir),
            "label {label:?} must end with {expected_dir:?}"
        );
    }
}
