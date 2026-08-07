//! Tests for the `Y` (sync_dirs) flow: the mode dialog, the
//! mirror-always-confirms rule, and the rejected pane layouts.

use super::super::test_support::*;
use super::super::*;

fn log_contains(app: &App, needle: &str) -> bool {
    app.log.iter().any(|l| l.message.contains(needle))
}

#[test]
fn sync_opens_the_mode_dialog() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), right.path());

    app.dispatch(Action::SyncDirs);
    match &app.mode {
        Mode::SyncSelect { src, dest, cursor } => {
            assert_eq!(src, left.path());
            assert_eq!(dest, right.path());
            assert_eq!(*cursor, 0);
        }
        other => panic!("expected SyncSelect, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn sync_dialog_moves_on_the_keymap_cursor_keys() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["n".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();

    app.dispatch(Action::SyncDirs);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    match &app.mode {
        Mode::SyncSelect { cursor, .. } => assert_eq!(*cursor, 1, "-> mirror"),
        other => panic!("expected SyncSelect, got {other:?}"),
    }
}

#[test]
fn mirror_choice_always_confirms_even_with_confirm_operations_off() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), right.path());
    app.config.confirm_operations = false;

    app.dispatch(Action::SyncDirs);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE)); // -> mirror
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    match &app.mode {
        Mode::Confirm { message, on_yes } => {
            assert!(message.contains("DELETED"), "message: {message}");
            assert!(matches!(on_yes, PendingOp::SyncDirs { mirror: true, .. }));
        }
        other => panic!("mirror must always confirm, got {other:?}"),
    }
}

#[test]
fn update_choice_with_confirm_off_spawns_and_syncs() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"payload").unwrap();
    std::fs::write(right.path().join("extra.txt"), b"keep").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.config.confirm_operations = false;
    app.active_pane_mut().reload().unwrap();

    app.dispatch(Action::SyncDirs);
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE)); // update
    assert!(matches!(app.mode, Mode::Normal));
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"payload"
    );
    assert!(
        right.path().join("extra.txt").exists(),
        "update mode must never delete destination-only files"
    );
}

#[test]
fn update_choice_with_confirm_on_goes_through_confirm_first() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(left.path(), right.path());
    assert!(app.config.confirm_operations, "default is confirm on");
    app.active_pane_mut().reload().unwrap();

    app.dispatch(Action::SyncDirs);
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE)); // update
    match &app.mode {
        Mode::Confirm { on_yes, .. } => {
            assert!(matches!(on_yes, PendingOp::SyncDirs { mirror: false, .. }));
        }
        other => panic!("expected Confirm, got {other:?}"),
    }
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert!(right.path().join("a.txt").exists());
}

#[test]
fn same_directory_panes_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::SyncDirs);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "same directory"));
}

#[test]
fn nested_panes_are_rejected_both_ways() {
    let outer = tempfile::tempdir().unwrap();
    let inner = outer.path().join("inner");
    std::fs::create_dir(&inner).unwrap();

    // Active = outer (src) contains dest.
    let mut app = test_app(outer.path(), &inner);
    app.dispatch(Action::SyncDirs);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "subdirectory"));

    // Active = inner (src) is contained by dest.
    let mut app = test_app(&inner, outer.path());
    app.dispatch(Action::SyncDirs);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "subdirectory"));
}
