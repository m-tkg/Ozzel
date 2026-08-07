//! `config.auto_refresh`: reloading a pane when its directory is changed
//! from outside ozzel.
//!
//! Never starts a real OS watcher — `App::new` doesn't create one (only
//! `main.rs`'s `enable_directory_watching` does), so these drive
//! `mark_fs_dirty_for_test`, the same flag a delivered watcher event sets.
//! That keeps them independent of inotify/FSEvents delivery timing, which
//! differs per platform and would make this suite flaky on CI.

use super::super::test_support::*;
use super::super::*;

#[test]
fn an_external_change_reloads_the_pane_on_the_next_event() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), right.path());
    app.panes[0].reload().unwrap();
    assert!(!app.panes[0].entries.iter().any(|e| e.name == "new.txt"));

    // Something outside ozzel writes into the left pane's directory.
    std::fs::write(left.path().join("new.txt"), b"from Finder").unwrap();
    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.handle_event(AppEvent::Tick);

    assert!(
        app.panes[0].entries.iter().any(|e| e.name == "new.txt"),
        "the pane must pick the new file up without C-r"
    );
    assert!(
        app.needs_redraw,
        "a Tick doesn't mark the frame dirty on its own — the refresh must"
    );
}

#[test]
fn an_external_change_only_reloads_the_pane_it_belongs_to() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), right.path());
    std::fs::write(right.path().join("untouched.txt"), b"x").unwrap();

    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.handle_event(AppEvent::Tick);

    assert!(
        !app.panes[1]
            .entries
            .iter()
            .any(|e| e.name == "untouched.txt"),
        "the right pane wasn't marked, so it must not have been reloaded"
    );
}

#[test]
fn an_external_change_keeps_the_cursor_on_the_same_entry() {
    let left = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(left.path().join(name), b"x").unwrap();
    }
    let mut app = test_app(left.path(), left.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "c.txt");

    // A new file sorting *before* the cursor entry would shift it by index.
    std::fs::write(left.path().join("aaa.txt"), b"x").unwrap();
    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.handle_event(AppEvent::Tick);

    assert_eq!(cursor_entry_name(&app), "c.txt");
}

#[test]
fn an_external_change_keeps_marks() {
    let left = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("marked.txt"), b"x").unwrap();
    let mut app = test_app(left.path(), left.path());
    app.active_pane_mut().reload().unwrap();
    let marked = left.path().join("marked.txt");
    app.panes[0].marks.insert(marked.clone());

    std::fs::write(left.path().join("new.txt"), b"x").unwrap();
    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.handle_event(AppEvent::Tick);

    assert!(app.panes[0].marks.contains(&marked));
}

#[test]
fn an_external_change_is_deferred_while_a_modal_is_open_then_applied() {
    // A listing shifting under an open prompt is a real hazard — several
    // pending operations captured entry paths when they opened.
    let left = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), left.path());
    app.active_pane_mut().reload().unwrap();
    app.dispatch(Action::Mkdir);
    assert!(matches!(app.mode, Mode::Prompt { .. }), "precondition");

    std::fs::write(left.path().join("new.txt"), b"x").unwrap();
    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.handle_event(AppEvent::Tick);
    assert!(
        !app.panes[0].entries.iter().any(|e| e.name == "new.txt"),
        "must not reload under an open prompt"
    );

    // Esc closes the prompt; the deferred refresh must then land, rather
    // than having been dropped.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.panes[0].entries.iter().any(|e| e.name == "new.txt"));
}

#[test]
fn an_external_change_forces_a_git_reprobe_for_that_pane() {
    let left = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), left.path());
    app.git_checked_dir = [
        Some(left.path().to_path_buf()),
        Some(left.path().to_path_buf()),
    ];

    app.mark_fs_dirty_for_test(ActivePane::Left);
    app.apply_fs_refresh();

    assert!(app.git_checked_dir[0].is_none(), "left must re-probe");
    assert_eq!(
        app.git_checked_dir[1].as_deref(),
        Some(left.path()),
        "the untouched pane keeps its probe"
    );
}

#[test]
fn auto_refresh_false_never_starts_a_watcher() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            auto_refresh: false,
            ..Config::default()
        },
    )
    .unwrap();

    app.enable_directory_watching();

    assert!(app.watcher.is_none());
    // ...and a drain with no watcher is a harmless no-op.
    app.drain_fs_events();
}
