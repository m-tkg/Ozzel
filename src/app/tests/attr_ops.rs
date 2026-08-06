//! Tests for the metadata-editing actions: symlink creation (`@`), the
//! chmod dialog (`A`), touch (`T`), and the file-info modal (`I`).

use super::super::test_support::*;
use super::super::*;

fn type_chars(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

fn log_contains(app: &App, needle: &str) -> bool {
    app.log.iter().any(|l| l.message.contains(needle))
}

#[cfg(unix)]
#[test]
fn symlink_confirms_then_creates_a_link_in_the_other_pane() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("target.txt"), b"hi").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "target.txt");

    app.dispatch(Action::Symlink);
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));

    let link = right.path().join("target.txt");
    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        left.path().join("target.txt")
    );
}

#[test]
fn symlink_rejects_both_panes_on_the_same_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Symlink);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "same directory"));
}

#[test]
fn symlink_on_a_virtual_pane_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "project.zip");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());

    app.dispatch(Action::Symlink);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "read-only"));
}

#[cfg(unix)]
#[test]
fn chmod_dialog_opens_with_the_cursor_entrys_mode_and_applies_on_enter() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, b"x").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Chmod);
    match &app.mode {
        Mode::Chmod { state } => assert_eq!(state.bits, 0o644),
        other => panic!("expected Chmod mode, got {other:?}"),
    }

    // Toggle user-x: Right x2 (cursor to user/x), Space.
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Char(' '), KeyModifiers::NONE));
    match &app.mode {
        Mode::Chmod { state } => assert_eq!(state.bits, 0o744),
        other => panic!("expected Chmod mode, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o744);
}

#[cfg(unix)]
#[test]
fn chmod_digit_key_sets_the_highlighted_row_as_an_octal_digit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");
    app.dispatch(Action::Chmod);

    // Move to the "other" row (Down x2) and set it to 0.
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Char('0'), KeyModifiers::NONE));
    match &app.mode {
        Mode::Chmod { state } => assert_eq!(state.bits & 0o007, 0),
        other => panic!("expected Chmod mode, got {other:?}"),
    }

    // Esc cancels without touching the file.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn touch_prompt_full_format_sets_the_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Touch);
    assert!(matches!(
        app.mode,
        Mode::Prompt {
            kind: PromptKind::TouchTime { .. },
            ..
        }
    ));

    // Clear the prefill, then type an exact local timestamp.
    while let Mode::Prompt { input, .. } = &app.mode {
        if input.value().is_empty() {
            break;
        }
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    type_chars(&mut app, "2020-01-02 03:04:05");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let dt: chrono::DateTime<chrono::Local> = mtime.into();
    assert_eq!(
        dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        "2020-01-02 03:04:05"
    );
}

#[test]
fn touch_prompt_short_date_format_zero_fills_the_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Touch);
    while let Mode::Prompt { input, .. } = &app.mode {
        if input.value().is_empty() {
            break;
        }
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    type_chars(&mut app, "2021-06-07");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let dt: chrono::DateTime<chrono::Local> = mtime.into();
    assert_eq!(
        dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        "2021-06-07 00:00:00"
    );
}

#[test]
fn touch_prompt_invalid_input_logs_an_error_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, b"x").unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Touch);
    while let Mode::Prompt { input, .. } = &app.mode {
        if input.value().is_empty() {
            break;
        }
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    type_chars(&mut app, "not a date");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(log_contains(&app, "invalid time"));
    let after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(before, after);
}

#[test]
fn file_info_opens_a_modal_with_the_core_rows_and_closes_on_esc() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), b"hello").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::FileInfo);
    match &app.mode {
        Mode::FileInfo { info } => {
            assert_eq!(info.title, "f.txt");
            let labels: Vec<&str> = info.rows.iter().map(|(l, _)| l.as_str()).collect();
            assert!(labels.contains(&"path"), "labels: {labels:?}");
            assert!(labels.contains(&"type"), "labels: {labels:?}");
            assert!(labels.contains(&"size"), "labels: {labels:?}");
            assert!(labels.contains(&"modified"), "labels: {labels:?}");
            #[cfg(unix)]
            {
                assert!(labels.contains(&"permissions"), "labels: {labels:?}");
                assert!(labels.contains(&"owner"), "labels: {labels:?}");
                assert!(labels.contains(&"inode"), "labels: {labels:?}");
            }
            let size_row = &info.rows.iter().find(|(l, _)| l == "size").unwrap().1;
            assert!(size_row.contains("5 bytes"), "size row: {size_row}");
        }
        other => panic!("expected FileInfo mode, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[cfg(unix)]
#[test]
fn file_info_on_a_symlink_shows_the_link_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "link.txt");

    app.dispatch(Action::FileInfo);
    match &app.mode {
        Mode::FileInfo { info } => {
            let link_row = &info.rows.iter().find(|(l, _)| l == "link to").unwrap().1;
            assert!(link_row.contains("real.txt"), "link row: {link_row}");
            assert!(!link_row.contains("dangling"), "link row: {link_row}");
        }
        other => panic!("expected FileInfo mode, got {other:?}"),
    }
}

#[test]
fn file_info_with_marks_appends_a_summary_row() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"aa").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"bbb").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");
    app.dispatch(Action::Mark); // marks a.txt, moves down onto b.txt
    select_entry_named(&mut app, "b.txt");
    app.dispatch(Action::Mark);
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::FileInfo);
    match &app.mode {
        Mode::FileInfo { info } => {
            let marked_row = &info.rows.iter().find(|(l, _)| l == "marked").unwrap().1;
            assert!(marked_row.contains("2 item(s)"), "marked row: {marked_row}");
            assert!(marked_row.contains("5 bytes"), "marked row: {marked_row}");
        }
        other => panic!("expected FileInfo mode, got {other:?}"),
    }
}
