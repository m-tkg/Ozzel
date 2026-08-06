//! Tests for the `=` (diff) action: viewer entry with `ViewerSyntax::Diff`,
//! and every "don't open" path (missing counterpart, identical, binary,
//! directory).

use super::super::test_support::*;
use super::super::*;

fn log_contains(app: &App, needle: &str) -> bool {
    app.log.iter().any(|l| l.message.contains(needle))
}

#[test]
fn diff_opens_the_viewer_with_diff_syntax_and_unified_content() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(right.path().join("f.txt"), "one\nTWO\nthree\n").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Diff);
    match &app.mode {
        Mode::Viewer {
            syntax,
            lines,
            view_mode,
            path,
            ..
        } => {
            assert_eq!(*syntax, ViewerSyntax::Diff);
            assert_eq!(*view_mode, ViewMode::Text);
            assert!(path.display().to_string().starts_with("diff: "));
            assert!(
                lines.iter().any(|l| l.starts_with("-two")),
                "lines: {lines:?}"
            );
            assert!(
                lines.iter().any(|l| l.starts_with("+TWO")),
                "lines: {lines:?}"
            );
            assert!(
                lines.iter().any(|l| l.starts_with("@@")),
                "lines: {lines:?}"
            );
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

#[test]
fn diff_with_no_same_named_file_logs_an_error() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("only-here.txt"), "x\n").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "only-here.txt");

    app.dispatch(Action::Diff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "no file named 'only-here.txt'"));
}

#[test]
fn diff_of_identical_files_logs_and_stays_in_normal_mode() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("f.txt"), "same\n").unwrap();
    std::fs::write(right.path().join("f.txt"), "same\n").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Diff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "identical"));
}

#[test]
fn diff_of_binary_files_logs_and_stays_in_normal_mode() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("f.bin"), b"\x00\x01\x02").unwrap();
    std::fs::write(right.path().join("f.bin"), b"\x00\x09\x08").unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.bin");

    app.dispatch(Action::Diff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "binary"));
}

#[test]
fn diff_on_a_directory_logs_an_error() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("sub")).unwrap();
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "sub");

    app.dispatch(Action::Diff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "not directories"));
}

#[test]
fn diff_with_both_panes_on_the_same_file_logs_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::Diff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "same file"));
}
