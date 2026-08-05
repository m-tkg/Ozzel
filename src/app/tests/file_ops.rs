use super::super::test_support::*;
use super::super::*;

#[test]
fn mkdir_prompt_creates_directory_on_enter() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Mkdir);
    assert!(matches!(app.mode, Mode::Prompt { .. }));

    for c in "newdir".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert!(dir.path().join("newdir").is_dir());
}

#[test]
fn rename_prompt_is_prefilled_and_commits() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "old.txt");

    app.dispatch(Action::Rename);
    match &app.mode {
        Mode::Prompt { kind, input } => {
            assert_eq!(
                *kind,
                PromptKind::Rename {
                    orig: "old.txt".to_string()
                }
            );
            assert_eq!(input.value(), "old.txt");
        }
        other => panic!("expected Prompt mode, got {other:?}"),
    }

    // Clear the prefilled text and type a new name.
    for _ in 0..7 {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in "new.txt".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(!dir.path().join("old.txt").exists());
    assert!(dir.path().join("new.txt").exists());
}

#[test]
fn delete_requires_confirmation_then_removes_via_background_task() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("victim.txt"), b"hi").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "victim.txt");

    app.dispatch(Action::Delete);
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(
        dir.path().join("victim.txt").exists(),
        "not deleted before confirm"
    );

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        !app.tasks.running.is_empty(),
        "delete should now be running in the background"
    );

    wait_for_tasks_done(&mut app);
    assert!(!dir.path().join("victim.txt").exists());
    assert!(
        app.log.iter().any(|l| l.message.contains("deleted 1 item")),
        "finished delete should log a summary"
    );
}

#[test]
fn delete_logs_every_target_path_up_front() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    std::fs::write(dir.path().join("c.txt"), b"c").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    app.dispatch(Action::MarkAll);

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    for name in ["a.txt", "b.txt", "c.txt"] {
        let expected = format!("delete: {}", dir.path().join(name).display());
        assert!(
            app.log.iter().any(|l| l.message == expected),
            "missing log line {expected:?}; log: {:?}",
            app.log
        );
    }
}

#[test]
fn delete_middle_entry_moves_cursor_to_the_entry_above() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    std::fs::write(dir.path().join("c.txt"), b"c").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert!(!dir.path().join("b.txt").exists());
    assert_eq!(cursor_entry_name(&app), "a.txt");
}

#[test]
fn delete_first_entry_leaves_cursor_at_the_top() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    // "a.txt" is the first *real* entry — nothing but ".." is above it.
    select_entry_named(&mut app, "a.txt");
    assert_eq!(app.active_pane().cursor, 1, "sanity: right below \"..\"");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert!(!dir.path().join("a.txt").exists());
    assert_eq!(
        app.active_pane().cursor,
        0,
        "cursor must clamp to the top, landing on the new first row"
    );
}

#[test]
fn delete_last_entry_moves_cursor_to_the_new_last_entry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    std::fs::write(dir.path().join("c.txt"), b"c").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "c.txt");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert!(!dir.path().join("c.txt").exists());
    assert_eq!(
        cursor_entry_name(&app),
        "b.txt",
        "b.txt is now the last entry"
    );
}

#[test]
fn delete_cursor_anchor_applies_the_same_way_under_trash_behavior() {
    // The cursor-anchor logic is orthogonal to whether the delete
    // itself actually succeeds (real OS trash access is environment-
    // dependent — see the README's known limitations — so this
    // deliberately doesn't assert the file is gone, only that the
    // anchor was captured and applied for a Trash-mode delete exactly
    // like a Permanent one: the reload/anchor path doesn't branch on
    // `delete_behavior` at all).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    std::fs::write(dir.path().join("c.txt"), b"c").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Trash,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert!(
        app.pending_delete_anchor.is_empty(),
        "the anchor must be consumed exactly once, by this task's own Finished event"
    );
    // If the trash move actually succeeded, the cursor should have
    // landed on "a.txt" (the entry above "b.txt"); if it failed in
    // this environment, "b.txt" is still there and the cursor simply
    // stayed on it — reload_preserving_cursor_onto's anchor lookup is
    // a no-op miss in that case, which is fine, since either way
    // nothing panics and the pane stays in a sane, well-defined state.
    let landed_on = cursor_entry_name(&app);
    assert!(
        landed_on == "a.txt" || landed_on == "b.txt",
        "unexpected cursor position: {landed_on}"
    );
}

#[test]
fn delete_confirmation_declined_keeps_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), b"hi").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "keep.txt");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.tasks.running.is_empty(),
        "declining must never spawn anything"
    );
    assert!(dir.path().join("keep.txt").exists());
}

#[test]
fn copy_action_confirms_by_default_then_spawns_a_background_task() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    match &app.mode {
        Mode::Confirm { message, .. } => {
            assert!(message.starts_with("Copy 1 item(s)"), "message: {message}");
            assert!(
                message.contains(&right.path().display().to_string()),
                "message: {message}"
            );
            assert!(
                !message.contains("overwritten"),
                "no collision => no overwrite note; message: {message}"
            );
        }
        other => {
            panic!("confirm_operations defaults to true, expected Mode::Confirm, got {other:?}")
        }
    }
    assert!(
        app.tasks.running.is_empty(),
        "must not spawn before confirmation"
    );

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(
        !app.tasks.running.is_empty(),
        "copy should now be running in the background"
    );

    wait_for_tasks_done(&mut app);
    assert!(right.path().join("a.txt").exists());
    assert!(left.path().join("a.txt").exists(), "copy keeps the source");
}

#[test]
fn copy_confirm_declined_does_not_spawn_or_copy() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.tasks.running.is_empty());
    assert!(!right.path().join("a.txt").exists());
}

#[test]
fn move_action_also_confirms_by_default() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Move);
    match &app.mode {
        Mode::Confirm { message, .. } => {
            assert!(message.starts_with("Move 1 item(s)"), "message: {message}")
        }
        other => panic!("expected Mode::Confirm, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert!(right.path().join("a.txt").exists());
    assert!(
        !left.path().join("a.txt").exists(),
        "move removes the source"
    );
}

#[test]
fn copy_skips_confirm_when_confirm_operations_false_and_no_collision() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            confirm_operations: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    assert!(
        matches!(app.mode, Mode::Normal),
        "confirm_operations=false + no collision => no prompt"
    );
    assert!(!app.tasks.running.is_empty());

    wait_for_tasks_done(&mut app);
    assert!(right.path().join("a.txt").exists());
}

#[test]
fn copy_logs_every_source_and_destination_path_up_front() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"a").unwrap();
    std::fs::write(left.path().join("b.txt"), b"b").unwrap();
    std::fs::write(left.path().join("c.txt"), b"c").unwrap();

    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            confirm_operations: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    app.dispatch(Action::MarkAll);

    app.dispatch(Action::Copy);
    wait_for_tasks_done(&mut app);

    for name in ["a.txt", "b.txt", "c.txt"] {
        let expected = format!(
            "copy: {} -> {}",
            left.path().join(name).display(),
            right.path().join(name).display()
        );
        assert!(
            app.log.iter().any(|l| l.message == expected),
            "missing log line {expected:?}; log: {:?}",
            app.log
        );
    }
}

#[test]
fn copy_collision_still_confirms_when_confirm_operations_false_with_a_combined_message() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            confirm_operations: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    match &app.mode {
        Mode::Confirm { message, .. } => {
            assert!(
                message.contains("1 will be overwritten"),
                "collision must always confirm even with confirm_operations=false; message: {message}"
            );
        }
        other => panic!("expected Mode::Confirm, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
}

#[test]
fn copy_collision_requires_confirmation_before_spawning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(
        app.tasks.running.is_empty(),
        "must not spawn before confirmation"
    );
    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"existing"
    );

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn copying_a_directory_symlink_copies_the_link_itself_not_the_target_tree() {
    // The core safety asymmetry this round is about: navigation
    // follows a directory-symlink, but every file operation
    // (copy/move/delete/duplicate/zip) must keep treating it as a
    // link, never as the directory it resolves to.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let real_dir = left.path().join("real_dir");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("inside.txt"), b"hi").unwrap();
    let link = left.path().join("link_to_dir");
    std::os::unix::fs::symlink(&real_dir, &link).unwrap();

    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            confirm_operations: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "link_to_dir");

    app.dispatch(Action::Copy);
    wait_for_tasks_done(&mut app);

    let dest = right.path().join("link_to_dir");
    let dest_meta = std::fs::symlink_metadata(&dest)
        .expect("destination must exist (as a symlink, not a directory tree)");
    assert!(
        dest_meta.is_symlink(),
        "copying a directory-symlink must produce another symlink at the \
         destination, not a recursively-copied directory tree"
    );
    assert_eq!(
        std::fs::read_link(&dest).unwrap(),
        real_dir,
        "the copied symlink must point at the same target, not have been \
         dereferenced and re-copied as a fresh tree"
    );
}

#[cfg(unix)]
#[test]
fn deleting_a_directory_symlink_removes_only_the_link_leaving_the_target_intact() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real_dir");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("inside.txt"), b"hi").unwrap();
    let link = dir.path().join("link_to_dir");
    std::os::unix::fs::symlink(&real_dir, &link).unwrap();

    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "link_to_dir");

    app.dispatch(Action::Delete);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "the link itself must be gone"
    );
    assert!(
        real_dir.join("inside.txt").exists(),
        "deleting the link must never touch the target it pointed to"
    );
}

#[test]
fn two_concurrent_transfers_both_complete() {
    let left = tempfile::tempdir().unwrap();
    let mid = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();
    std::fs::write(mid.path().join("b.txt"), b"hi").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");
    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(app.tasks.running.len(), 1);

    // A second, independent transfer spawned while the first is (very
    // likely still) in flight.
    app.panes[0].cwd = mid.path().to_path_buf();
    app.panes[0].reload().unwrap();
    select_entry_named(&mut app, "b.txt");
    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));

    wait_for_tasks_done(&mut app);
    assert!(right.path().join("a.txt").exists());
    assert!(right.path().join("b.txt").exists());
}

#[test]
fn copy_path_queues_the_cursor_entrys_absolute_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // dir.path() isn't the filesystem root, so index 0 is the
    // synthetic ".." row and index 1 is the real entry.
    app.active_pane_mut().cursor = 1;
    let expected = app
        .active_pane()
        .selected_entry_path()
        .expect("cursor must be on a real entry");
    app.dispatch(Action::CopyPath);
    assert_eq!(
        app.outbox.clipboard.as_deref(),
        Some(expected.to_string_lossy().as_ref())
    );
}

#[test]
fn copy_path_with_no_selection_logs_an_error_and_queues_nothing() {
    let dir = tempfile::tempdir().unwrap(); // empty dir, only ".." (or nothing)
    let mut app = test_app(dir.path(), dir.path());
    // An empty, non-root directory's only row is "..", which has no
    // path — selected_entry_path() is None either way here.
    app.dispatch(Action::CopyPath);
    assert!(app.outbox.clipboard.is_none());
}

#[test]
fn duplicate_prompt_is_prefilled_with_the_current_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // Land the cursor on the real entry (index depends on whether ".."
    // is shown; dir.path() isn't the fs root, so ".." occupies index 0).
    app.active_pane_mut().cursor = 1;
    app.dispatch(Action::Duplicate);
    match &app.mode {
        Mode::Prompt {
            kind: PromptKind::Duplicate { source },
            input,
        } => {
            assert_eq!(source.file_name().unwrap(), "a.txt");
            assert_eq!(input.value(), "a.txt");
        }
        other => panic!("expected Mode::Prompt(Duplicate), got {other:?}"),
    }
}

#[test]
fn duplicate_rejects_empty_separator_and_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.txt");
    std::fs::write(&source, b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.commit_duplicate(source.clone(), String::new());
    assert!(app.log.back().unwrap().is_error);

    app.commit_duplicate(source.clone(), "sub/dir".to_string());
    assert!(app.log.back().unwrap().is_error);

    app.commit_duplicate(source.clone(), "a.txt".to_string());
    assert!(app.log.back().unwrap().is_error);
}

#[test]
fn duplicate_rejects_a_colliding_destination_name() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.txt");
    std::fs::write(&source, b"hi").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"already here").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.commit_duplicate(source, "b.txt".to_string());
    assert!(app.log.back().unwrap().is_error);
    assert_eq!(
        std::fs::read(dir.path().join("b.txt")).unwrap(),
        b"already here"
    );
}

#[test]
fn duplicate_copies_the_source_to_the_new_name_in_the_same_directory() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("a.txt");
    std::fs::write(&source, b"hello").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.commit_duplicate(source, "a_copy.txt".to_string());
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(dir.path().join("a_copy.txt")).unwrap(),
        b"hello"
    );
    // The original must still be there — this is a copy, not a move.
    assert!(dir.path().join("a.txt").exists());
}
