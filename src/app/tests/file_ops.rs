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
fn confirm_ignores_unrelated_keys_and_cancels_on_esc() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keep.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "keep.txt");

    app.dispatch(Action::Delete);
    assert!(matches!(app.mode, Mode::Confirm { .. }));

    // Stray keys — navigation, space, a random letter — must neither
    // execute nor dismiss the dialog.
    for code in [
        KeyCode::Char('x'),
        KeyCode::Char(' '),
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Backspace,
    ] {
        app.handle_event(AppEvent::Input(code, KeyModifiers::NONE));
        assert!(
            matches!(app.mode, Mode::Confirm { .. }),
            "{code:?} must leave the confirm dialog open"
        );
    }

    // Esc cancels like n/N.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(dir.path().join("keep.txt").exists());
    assert!(app.tasks.running.is_empty());
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
fn rename_marks_walks_marked_entries_in_display_order_with_progress_titles() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let mut app = test_app(dir.path(), dir.path());
    for name in ["a.txt", "c.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }

    app.dispatch(Action::RenameMarks);
    match &app.mode {
        Mode::Prompt {
            kind:
                PromptKind::RenameMany {
                    current,
                    done,
                    total,
                    ..
                },
            input,
        } => {
            assert_eq!(current, "a.txt", "display order, not mark order");
            assert_eq!((*done, *total), (0, 2));
            assert_eq!(input.value(), "a.txt", "prefilled with the current name");
        }
        other => panic!("expected RenameMany prompt, got {other:?}"),
    }

    // Rename a.txt -> z.txt.
    for _ in 0..5 {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in "z.txt".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    // The second prompt is for c.txt, titled (2/2) via done=1.
    match &app.mode {
        Mode::Prompt {
            kind: PromptKind::RenameMany { current, done, .. },
            ..
        } => {
            assert_eq!(current, "c.txt");
            assert_eq!(*done, 1);
        }
        other => panic!("expected the second RenameMany prompt, got {other:?}"),
    }
    // Confirm unchanged -> counts as a skip, sequence ends.
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));

    assert!(dir.path().join("z.txt").exists());
    assert!(!dir.path().join("a.txt").exists());
    assert!(dir.path().join("c.txt").exists(), "skipped entry untouched");
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("rename marks finished (1/2 renamed)")),
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
    assert!(
        app.active_pane().marks.is_empty(),
        "marks are consumed by the sequence"
    );
}

#[test]
fn rename_marks_esc_cancels_the_remainder_keeping_finished_renames() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let mut app = test_app(dir.path(), dir.path());
    for name in ["a.txt", "b.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }

    app.dispatch(Action::RenameMarks);
    // First rename goes through.
    for _ in 0..5 {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in "renamed.txt".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    // Esc on the second prompt.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        dir.path().join("renamed.txt").exists(),
        "the finished rename stands"
    );
    assert!(
        dir.path().join("b.txt").exists(),
        "the cancelled one is untouched"
    );
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("rename marks cancelled (1/2 renamed)"))
    );
}

#[test]
fn rename_marks_without_marks_logs_an_error_not_a_cursor_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::RenameMarks);
    assert!(
        matches!(app.mode, Mode::Normal),
        "no prompt without marks — rename_marks never falls back to the cursor"
    );
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("no marked entries to rename"))
    );
}

#[test]
fn rename_marks_excludes_marks_hidden_by_the_filter() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["match_1.txt", "other.txt"] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let mut app = test_app(dir.path(), dir.path());
    for name in ["match_1.txt", "other.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }
    // Filter so only match_1.txt stays visible; other.txt's mark is now
    // hidden.
    app.active_pane_mut()
        .set_filter(crate::filter::FilterSpec::parse("match"));

    app.dispatch(Action::RenameMarks);
    match &app.mode {
        Mode::Prompt {
            kind: PromptKind::RenameMany { total, .. },
            ..
        } => assert_eq!(*total, 1, "only the visible mark is included"),
        other => panic!("expected RenameMany prompt, got {other:?}"),
    }
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("hidden by the filter")),
        "the exclusion must be announced"
    );
}

#[test]
fn copy_collision_opens_the_dialog_even_with_confirm_operations_false() {
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
    assert!(
        matches!(app.mode, Mode::TransferCollision { .. }),
        "a collision is never silently overwritten, even with confirm_operations=false; got {:?}",
        app.mode
    );

    // Enter on the default highlight (Overwrite) resolves and spawns.
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
}

#[test]
fn collision_dialog_moves_on_the_keymap_cursor_keys() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = App::new(
        left.path().to_path_buf(),
        right.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["n".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    match &app.mode {
        Mode::TransferCollision { state } => assert_eq!(state.cursor, 1),
        other => panic!("expected Mode::TransferCollision, got {other:?}"),
    }
}

#[test]
fn copy_collision_opens_the_per_file_dialog_before_spawning() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    match &app.mode {
        Mode::TransferCollision { state } => {
            assert_eq!(state.index, 1);
            assert_eq!(state.total, 1);
            assert_eq!(state.current.name, "a.txt");
            assert_eq!(state.cursor, 0, "highlight starts on Overwrite");
        }
        other => panic!("expected Mode::TransferCollision, got {other:?}"),
    }
    assert!(
        app.tasks.running.is_empty(),
        "must not spawn before the dialog is answered"
    );
    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"existing"
    );

    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
}

#[test]
fn collision_dialog_skip_leaves_the_destination_untouched() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(left.path().join("b.txt"), b"fresh").unwrap();
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
    // Mark both: b.txt has no collision (goes straight to resolved),
    // a.txt collides (asked about).
    for name in ["a.txt", "b.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }

    app.dispatch(Action::Copy);
    assert!(matches!(app.mode, Mode::TransferCollision { .. }));
    // Down x2 -> Skip, Enter.
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"existing",
        "Skip must leave the colliding destination untouched"
    );
    assert_eq!(
        std::fs::read(right.path().join("b.txt")).unwrap(),
        b"fresh",
        "the non-colliding source still transfers"
    );
}

#[test]
fn collision_dialog_rename_transfers_under_the_new_name() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    // Down -> Rename, Enter opens the prompt prefilled with "a.txt".
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    match &app.mode {
        Mode::Prompt {
            kind: PromptKind::CollisionRename { .. },
            input,
        } => assert_eq!(input.value(), "a.txt"),
        other => panic!("expected the collision-rename prompt, got {other:?}"),
    }
    // Type a distinct name: clear then enter "kept.txt".
    for _ in 0..5 {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in "kept.txt".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(right.path().join("a.txt")).unwrap(),
        b"existing",
        "the original destination stays"
    );
    assert_eq!(
        std::fs::read(right.path().join("kept.txt")).unwrap(),
        b"new"
    );
}

#[test]
fn collision_dialog_rename_to_another_existing_name_reasks() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();
    std::fs::write(right.path().join("b.txt"), b"also existing").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE)); // Rename
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    // Retype "b.txt" — which also exists.
    for _ in 0..5 {
        app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
    }
    for c in "b.txt".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        matches!(app.mode, Mode::TransferCollision { .. }),
        "a rename target that also exists must re-ask, never overwrite; got {:?}",
        app.mode
    );
    assert!(app.tasks.running.is_empty());
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("already exists"))
    );
}

#[test]
fn collision_dialog_overwrite_all_and_skip_all_batch_the_rest() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(left.path().join(name), b"new").unwrap();
        std::fs::write(right.path().join(name), b"existing").unwrap();
    }

    // Overwrite All from the first conflict.
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }
    app.dispatch(Action::Copy);
    match &app.mode {
        Mode::TransferCollision { state } => assert_eq!(state.total, 3),
        other => panic!("expected TransferCollision, got {other:?}"),
    }
    for _ in 0..3 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE)); // -> Overwrite All
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_tasks_done(&mut app);
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(std::fs::read(right.path().join(name)).unwrap(), b"new");
    }

    // Skip All from the first conflict: nothing is transferred.
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(right.path().join(name), b"existing").unwrap();
    }
    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }
    app.dispatch(Action::Copy);
    for _ in 0..4 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE)); // -> Skip All
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.tasks.running.is_empty(), "nothing to spawn");
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("nothing to transfer"))
    );
    for name in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(std::fs::read(right.path().join(name)).unwrap(), b"existing");
    }
}

#[test]
fn collision_dialog_esc_cancels_the_whole_transfer() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(left.path().join("b.txt"), b"fresh").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    for name in ["a.txt", "b.txt"] {
        select_entry_named(&mut app, name);
        app.dispatch(Action::Mark);
    }
    app.dispatch(Action::Copy);
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.tasks.running.is_empty(),
        "Esc cancels everything — even the non-colliding b.txt must not transfer"
    );
    assert!(!right.path().join("b.txt").exists());
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("transfer cancelled"))
    );
}

#[test]
fn collision_info_marks_the_newer_side() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"new").unwrap();
    std::fs::write(right.path().join("a.txt"), b"existing").unwrap();
    // Make the destination decisively older (`File::set_modified`,
    // stable std — no extra dev-dependency needed).
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(right.path().join("a.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();

    let mut app = test_app(left.path(), right.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");
    app.dispatch(Action::Copy);

    match &app.mode {
        Mode::TransferCollision { state } => {
            assert!(
                state.current.src_line.contains("[New]"),
                "src is newer: {:?}",
                state.current.src_line
            );
            assert!(
                !state.current.dest_line.contains("[New]"),
                "dest is older: {:?}",
                state.current.dest_line
            );
            assert!(state.current.src_line.contains("bytes"));
        }
        other => panic!("expected TransferCollision, got {other:?}"),
    }
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
fn copy_path_on_the_parent_row_copies_the_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(&sub, &sub);
    // Index 0 is the synthetic ".." row, which is where the cursor starts.
    assert_eq!(app.active_pane().cursor, 0);

    app.dispatch(Action::CopyPath);

    assert_eq!(
        app.outbox.clipboard.as_deref(),
        Some(dir.path().to_string_lossy().as_ref()),
        ".. must copy the directory it would navigate to"
    );
    assert!(!app.log.iter().any(|l| l.is_error));
}

#[test]
fn copy_path_on_the_parent_row_of_an_empty_directory_still_works() {
    // An empty, non-root directory's only row is "..", which used to make
    // copy_path an error — there is always a parent to name.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(&sub, &sub);

    app.dispatch(Action::CopyPath);

    assert_eq!(
        app.outbox.clipboard.as_deref(),
        Some(dir.path().to_string_lossy().as_ref())
    );
}

#[test]
fn copy_dir_path_copies_the_active_panes_cwd() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a.txt"), b"hi").unwrap();
    let mut app = test_app(left.path(), right.path());
    // Wherever the cursor happens to be, this copies the directory itself.
    app.active_pane_mut().cursor = 1;

    app.dispatch(Action::CopyDirPath);

    assert_eq!(
        app.outbox.clipboard.as_deref(),
        Some(left.path().to_string_lossy().as_ref())
    );
}

#[test]
fn copy_dir_path_follows_the_active_pane() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = test_app(left.path(), right.path());

    app.dispatch(Action::SwitchPane);
    app.dispatch(Action::CopyDirPath);

    assert_eq!(
        app.outbox.clipboard.as_deref(),
        Some(right.path().to_string_lossy().as_ref())
    );
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
