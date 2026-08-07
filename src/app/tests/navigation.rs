use super::super::test_support::*;
use super::super::*;

#[test]
fn open_on_directory_navigates_and_records_history() {
    // `open` merges the old Enter/View actions: on a directory it
    // navigates (the old View action used to error here instead —
    // "cursor is not on a file" — that behavior is gone now that
    // there's only one context-dependent action bound to both `Enter`
    // and `o`).
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "sub");

    app.dispatch(Action::Open);
    assert_eq!(app.panes[0].cwd, dir.path().join("sub"));
    assert_eq!(
        app.history.ring(Side::Left).first(),
        Some(&dir.path().join("sub"))
    );
}

#[test]
fn parent_navigation_also_records_history() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(&sub, &sub);

    app.dispatch(Action::Parent);
    assert_eq!(app.panes[0].cwd, dir.path());
    assert_eq!(
        app.history.ring(Side::Left).first(),
        Some(&dir.path().to_path_buf())
    );
}

#[test]
fn go_home_jumps_to_configured_home_and_records_history() {
    let start = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            home: Some(home.path().to_path_buf()),
            ..Config::default()
        },
    )
    .unwrap();

    app.dispatch(Action::GoHome);
    assert_eq!(app.panes[0].cwd, home.path());
    assert_eq!(
        app.history.ring(Side::Left).first(),
        Some(&home.path().to_path_buf())
    );
}

#[test]
fn go_home_errors_and_stays_put_when_configured_home_is_missing() {
    let start = tempfile::tempdir().unwrap();
    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            home: Some(PathBuf::from("/does/not/exist/at/all/ozzel-test")),
            ..Config::default()
        },
    )
    .unwrap();

    app.dispatch(Action::GoHome);
    assert_eq!(app.panes[0].cwd, start.path());
    assert!(app.log.iter().any(|l| l.is_error));
}

/// Reported real-world case: `home = "~/work"` where `~/work` is
/// itself a symlink to some other real directory (the user's actual
/// setup: `~/work -> Dropbox/work`). `config::parse_config` expands
/// `~` (tested directly in `config::tests`); this checks the other
/// half of the chain — that `begin_go_home`'s existing `is_dir()` +
/// jump logic, given the *already-expanded* symlink path a real
/// config load would hand it, still lands the pane on the symlink's
/// resolved contents rather than erroring on it.
#[test]
#[cfg(unix)]
fn go_home_jumps_through_a_symlinked_home_target() {
    let start = tempfile::tempdir().unwrap();
    let home_parent = tempfile::tempdir().unwrap();
    let real_target = tempfile::tempdir().unwrap();
    std::fs::write(real_target.path().join("marker.txt"), b"hi").unwrap();
    let link = home_parent.path().join("work");
    std::os::unix::fs::symlink(real_target.path(), &link).unwrap();

    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            home: Some(link.clone()),
            ..Config::default()
        },
    )
    .unwrap();

    app.dispatch(Action::GoHome);
    assert_eq!(app.panes[0].cwd, link);
    assert!(app.panes[0].cwd.join("marker.txt").is_file());
    assert!(!app.log.iter().any(|l| l.is_error));
}

#[test]
fn bookmark_add_dedups_and_marks_dirty_only_when_actually_added() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    assert!(!app.outbox.bookmarks_dirty);

    app.dispatch(Action::BookmarkAdd);
    assert_eq!(app.bookmarks.paths, vec![dir.path().to_path_buf()]);
    assert!(app.outbox.bookmarks_dirty);

    app.outbox.bookmarks_dirty = false;
    app.dispatch(Action::BookmarkAdd); // duplicate
    assert_eq!(app.bookmarks.paths.len(), 1, "must not add a duplicate");
    assert!(
        !app.outbox.bookmarks_dirty,
        "a no-op add must not mark dirty again"
    );
}

#[test]
fn bookmark_jump_menu_enter_navigates_active_pane() {
    let target = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = test_app(start.path(), start.path());
    app.bookmarks.add(target.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    assert!(matches!(
        app.mode,
        Mode::Select {
            kind: SelectKind::Bookmark,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.panes[0].cwd, target.path());
}

#[test]
fn bookmark_jump_menu_esc_cancels_without_navigating() {
    let target = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = test_app(start.path(), start.path());
    app.bookmarks.add(target.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.panes[0].cwd, start.path(), "Esc must not navigate");
}

#[test]
fn bookmark_menu_down_then_d_deletes_the_highlighted_entry() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = test_app(start.path(), start.path());
    app.bookmarks.add(a.path().to_path_buf());
    app.bookmarks.add(b.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(app.bookmarks.paths, vec![a.path().to_path_buf()]);
    assert!(app.outbox.bookmarks_dirty);
    match &app.mode {
        Mode::Select { items, .. } => {
            assert_eq!(items.len(), 1, "menu list must refresh after delete")
        }
        other => panic!("expected Select mode to stay open, got {other:?}"),
    }
}

/// Opens the bookmark menu over three bookmarked tempdirs. The `TempDir`
/// guards come back with it so the caller keeps them alive for the length
/// of the test.
fn bookmark_menu_app() -> (App, Vec<PathBuf>, Vec<tempfile::TempDir>) {
    let dirs: Vec<tempfile::TempDir> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let paths: Vec<PathBuf> = dirs[1..].iter().map(|d| d.path().to_path_buf()).collect();
    let mut app = test_app(dirs[0].path(), dirs[0].path());
    for path in &paths {
        app.bookmarks.add(path.clone());
    }
    app.dispatch(Action::BookmarkJump);
    (app, paths, dirs)
}

/// The menu's own list and the persisted one are index-for-index the same
/// list; every reorder assertion checks both, since a bug that desyncs them
/// would make the highlight point at a different bookmark than it shows.
fn assert_menu_order(app: &App, expected: &[PathBuf], expected_cursor: usize) {
    match &app.mode {
        Mode::Select { items, cursor, .. } => {
            let shown: Vec<PathBuf> = items.iter().map(|(_, p)| p.clone()).collect();
            assert_eq!(shown, expected, "menu order");
            assert_eq!(*cursor, expected_cursor, "cursor must follow the entry");
        }
        other => panic!("expected the bookmark menu to stay open, got {other:?}"),
    }
    assert_eq!(app.bookmarks.paths, expected, "persisted order");
}

#[test]
fn bookmark_menu_shift_down_moves_the_entry_and_carries_the_cursor() {
    let (mut app, paths, _dirs) = bookmark_menu_app();
    app.outbox.bookmarks_dirty = false;

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::SHIFT));

    assert_menu_order(
        &app,
        &[paths[0].clone(), paths[2].clone(), paths[1].clone()],
        2,
    );
    assert!(app.outbox.bookmarks_dirty);
}

#[test]
fn bookmark_menu_shift_up_moves_the_entry_back() {
    let (mut app, paths, _dirs) = bookmark_menu_app();

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::SHIFT));
    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::SHIFT));

    assert_menu_order(&app, &paths, 1);
}

#[test]
fn bookmark_menu_shift_up_at_the_top_is_a_no_op() {
    let (mut app, paths, _dirs) = bookmark_menu_app();
    app.outbox.bookmarks_dirty = false;

    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::SHIFT));

    assert_menu_order(&app, &paths, 0);
    assert!(
        !app.outbox.bookmarks_dirty,
        "a no-op reorder must not schedule a save"
    );
}

#[test]
fn bookmark_menu_shift_down_at_the_bottom_is_a_no_op() {
    let (mut app, paths, _dirs) = bookmark_menu_app();
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.outbox.bookmarks_dirty = false;

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::SHIFT));

    assert_menu_order(&app, &paths, 2);
    assert!(!app.outbox.bookmarks_dirty);
}

#[test]
fn bookmark_menu_reorder_wins_over_shift_up_being_bound_to_top() {
    // S-Up/S-Down are `top`/`bottom` in the default keymap; inside the
    // bookmark menu the reorder keys shadow them on purpose.
    let (mut app, paths, _dirs) = bookmark_menu_app();
    assert_eq!(
        app.keymap.resolve(KeyCode::Up, KeyModifiers::SHIFT),
        Some(Action::Top),
        "precondition: S-Up is `top` by default"
    );

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::SHIFT));

    assert_menu_order(
        &app,
        &[paths[1].clone(), paths[0].clone(), paths[2].clone()],
        0,
    );
}

/// Opens the bookmark menu over `count` bookmarked tempdirs — the paging
/// counterpart of `bookmark_menu_app`, which only makes three.
fn paged_bookmark_menu_app(count: usize) -> (App, Vec<PathBuf>, Vec<tempfile::TempDir>) {
    let dirs: Vec<tempfile::TempDir> = (0..count + 1)
        .map(|_| tempfile::tempdir().unwrap())
        .collect();
    let paths: Vec<PathBuf> = dirs[1..].iter().map(|d| d.path().to_path_buf()).collect();
    let mut app = test_app(dirs[0].path(), dirs[0].path());
    for path in &paths {
        app.bookmarks.add(path.clone());
    }
    app.dispatch(Action::BookmarkJump);
    (app, paths, dirs)
}

fn select_cursor(app: &App) -> usize {
    match &app.mode {
        Mode::Select { cursor, .. } => *cursor,
        other => panic!("expected the bookmark menu to stay open, got {other:?}"),
    }
}

#[test]
fn bookmark_menu_digit_jumps_straight_to_that_row_of_the_page() {
    let (mut app, paths, _dirs) = paged_bookmark_menu_app(12);

    app.handle_event(AppEvent::Input(KeyCode::Char('3'), KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal), "the digit also commits");
    assert_eq!(app.panes[0].cwd, paths[2]);
}

#[test]
fn bookmark_menu_right_turns_the_page_and_digits_index_into_it() {
    let (mut app, paths, _dirs) = paged_bookmark_menu_app(12);

    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 9, "page 2 starts at the 10th entry");

    app.handle_event(AppEvent::Input(KeyCode::Char('3'), KeyModifiers::NONE));
    assert_eq!(app.panes[0].cwd, paths[11], "digits are page-relative");
}

#[test]
fn bookmark_menu_page_keys_clamp_at_both_ends() {
    let (mut app, _paths, _dirs) = paged_bookmark_menu_app(12);

    app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 0, "no page before the first");

    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 9, "no page after the last");
}

#[test]
fn bookmark_menu_page_turn_clamps_onto_a_short_final_page() {
    // 12 bookmarks: page 2 holds three, so row 5 of page 1 has no
    // counterpart there and the highlight lands on the last entry.
    let (mut app, _paths, _dirs) = paged_bookmark_menu_app(12);
    for _ in 0..4 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(select_cursor(&app), 4);

    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 11);

    // ...and back: row 3 of the short page maps to row 3 of the first.
    app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 2);
}

#[test]
fn bookmark_menu_digit_past_the_last_row_of_a_short_page_is_ignored() {
    let (mut app, _paths, dirs) = paged_bookmark_menu_app(12);
    let start = dirs[0].path().to_path_buf();
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));

    app.handle_event(AppEvent::Input(KeyCode::Char('4'), KeyModifiers::NONE));

    assert_eq!(select_cursor(&app), 9, "the menu stays open, untouched");
    assert_eq!(app.panes[0].cwd, start, "and nothing navigated");
}

#[test]
fn bookmark_menu_up_down_scrolling_crosses_a_page_boundary() {
    // The page is derived from the cursor, so plain Down off the end of a
    // page has to land on the next one rather than stall.
    let (mut app, paths, _dirs) = paged_bookmark_menu_app(12);
    for _ in 0..9 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(select_cursor(&app), 9);

    app.handle_event(AppEvent::Input(KeyCode::Char('1'), KeyModifiers::NONE));
    assert_eq!(
        app.panes[0].cwd, paths[9],
        "row 1 of the page we scrolled to"
    );
}

#[test]
fn history_menu_ignores_the_digit_and_page_keys() {
    let a = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = test_app(start.path(), start.path());
    app.history.record(Side::Left, a.path().to_path_buf());

    app.dispatch(Action::HistoryJump);
    app.handle_event(AppEvent::Input(KeyCode::Char('1'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));

    match &app.mode {
        Mode::Select { kind, cursor, .. } => {
            assert_eq!(*kind, SelectKind::History);
            assert_eq!(*cursor, 0, "the history menu is not paged");
        }
        other => panic!("expected the history menu to stay open, got {other:?}"),
    }
    assert_eq!(app.panes[0].cwd, start.path(), "and nothing navigated");
}

#[test]
fn bookmark_menu_pages_on_a_remapped_focus_key() {
    let (mut app, paths, _dirs) = paged_bookmark_menu_app(12);
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('l'), KeyModifiers::NONE),
        Some(Action::FocusRight),
        "precondition: `l` is focus_right by default"
    );

    app.handle_event(AppEvent::Input(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(select_cursor(&app), 9);

    app.handle_event(AppEvent::Input(KeyCode::Char('2'), KeyModifiers::NONE));
    assert_eq!(app.panes[0].cwd, paths[10]);
}

#[test]
fn bookmark_menu_moves_the_cursor_on_a_remapped_cursor_down_key() {
    let target = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["n".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();
    app.bookmarks.add(other.path().to_path_buf());
    app.bookmarks.add(target.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.panes[0].cwd, target.path());
}

#[test]
fn bookmark_menu_arrows_still_move_after_the_arrows_are_unbound() {
    let target = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            keys: HashMap::from([
                ("up".to_string(), "none".to_string()),
                ("down".to_string(), "none".to_string()),
            ]),
            ..Config::default()
        },
    )
    .unwrap();
    assert_eq!(
        app.keymap.resolve(KeyCode::Down, KeyModifiers::NONE),
        None,
        "precondition: the arrows are unbound in Normal mode"
    );
    app.bookmarks.add(other.path().to_path_buf());
    app.bookmarks.add(target.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.panes[0].cwd,
        target.path(),
        "the menu's arrows are fixed, so unbinding them can't strand the user"
    );
}

#[test]
fn bookmark_menu_d_still_deletes_when_d_is_remapped_to_cursor_down() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let start = tempfile::tempdir().unwrap();
    let mut app = App::new(
        start.path().to_path_buf(),
        start.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["d".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();
    app.bookmarks.add(a.path().to_path_buf());
    app.bookmarks.add(b.path().to_path_buf());

    app.dispatch(Action::BookmarkJump);
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));

    assert_eq!(
        app.bookmarks.paths,
        vec![b.path().to_path_buf()],
        "the menu's own `d` must win over a rebind"
    );
}

#[test]
fn history_menu_shift_down_only_moves_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let bookmark = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.bookmarks.add(bookmark.path().to_path_buf());
    select_entry_named(&mut app, "sub");
    app.dispatch(Action::Open); // history: [sub]
    app.dispatch(Action::Parent); // history: [dir, sub]
    app.outbox.bookmarks_dirty = false;

    app.dispatch(Action::HistoryJump);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::SHIFT));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(app.panes[0].cwd, sub, "S-Down is plain movement here");
    assert_eq!(
        app.bookmarks.paths,
        vec![bookmark.path().to_path_buf()],
        "the history menu must never touch the bookmark list"
    );
    assert!(!app.outbox.bookmarks_dirty);
}

#[test]
fn history_menu_d_remapped_to_cursor_down_moves_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["d".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();
    select_entry_named(&mut app, "sub");
    app.dispatch(Action::Open);
    app.dispatch(Action::Parent);

    app.dispatch(Action::HistoryJump);
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        app.panes[0].cwd, sub,
        "`d` deletes nothing here, so it falls through to the keymap"
    );
}

#[test]
fn history_jump_menu_lists_most_recent_first_and_selects() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    select_entry_named(&mut app, "sub");
    app.dispatch(Action::Open); // history: [sub]
    app.dispatch(Action::Parent); // history: [dir, sub]

    app.dispatch(Action::HistoryJump);
    assert!(matches!(
        app.mode,
        Mode::Select {
            kind: SelectKind::History,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.panes[0].cwd, sub);
}

#[test]
fn history_jump_with_empty_history_logs_error_instead_of_opening_menu() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::HistoryJump);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.log.iter().any(|l| l.is_error));
}

#[test]
fn open_default_errors_when_no_entry_selected() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // Cursor starts on ".." (index 0): a tempdir always has a real
    // parent, so nothing is "selected" for OpenDefault's purposes.
    app.dispatch(Action::OpenDefault);
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("no entry selected"))
    );
}

#[test]
fn focus_left_and_focus_right_activate_the_named_pane() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    assert_eq!(app.active, ActivePane::Left);

    app.dispatch(Action::FocusRight);
    assert_eq!(app.active, ActivePane::Right);

    // No-op when already active.
    app.dispatch(Action::FocusRight);
    assert_eq!(app.active, ActivePane::Right);

    app.dispatch(Action::FocusLeft);
    assert_eq!(app.active, ActivePane::Left);
}

#[test]
fn left_right_arrow_keys_switch_pane_focus_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.active, ActivePane::Right);
    app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.active, ActivePane::Left);
}

#[test]
fn history_back_and_forward_walk_the_per_pane_stack() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.navigate(|pane| pane.jump_to(sub.clone()));
    assert_eq!(app.active_pane().cwd, sub);

    app.dispatch(Action::HistoryBack);
    assert_eq!(app.active_pane().cwd, dir.path());

    app.dispatch(Action::HistoryForward);
    assert_eq!(app.active_pane().cwd, sub);
}

#[test]
fn history_back_with_nothing_to_go_back_to_logs_and_stays_put() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::HistoryBack);
    assert_eq!(app.active_pane().cwd, dir.path());
    assert!(app.log.back().unwrap().is_error);
}

#[test]
fn history_forward_with_nothing_to_go_forward_to_logs_and_stays_put() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::HistoryForward);
    assert_eq!(app.active_pane().cwd, dir.path());
    assert!(app.log.back().unwrap().is_error);
}

#[test]
fn a_new_navigation_after_going_back_clears_the_forward_stack() {
    let dir = tempfile::tempdir().unwrap();
    let sub_a = dir.path().join("a");
    let sub_b = dir.path().join("b");
    std::fs::create_dir(&sub_a).unwrap();
    std::fs::create_dir(&sub_b).unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.navigate(|pane| pane.jump_to(sub_a.clone()));
    app.dispatch(Action::HistoryBack); // back to dir.path()
    app.navigate(|pane| pane.jump_to(sub_b.clone())); // a fresh move

    app.dispatch(Action::HistoryForward);
    // Forward stack was cleared by the fresh navigation, so this must
    // be a no-op (still in sub_b), not a jump back to sub_a.
    assert_eq!(app.active_pane().cwd, sub_b);
}

#[test]
fn function_list_opens_empty_and_lists_every_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);
    match &app.mode {
        Mode::FunctionList { input, cursor } => {
            assert_eq!(input.value(), "");
            assert_eq!(*cursor, 0);
        }
        other => panic!("expected Mode::FunctionList, got {other:?}"),
    }
    assert_eq!(
        app.function_list_filtered_actions().len(),
        Action::ALL.len()
    );
}

#[test]
fn function_list_typing_i_still_filters_instead_of_moving_the_cursor() {
    // `i`/`k` are cursor_up/cursor_down in Normal mode, but the palette's
    // search field claims every printable character first.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);

    app.handle_event(AppEvent::Input(KeyCode::Char('i'), KeyModifiers::NONE));
    match &app.mode {
        Mode::FunctionList { input, cursor } => {
            assert_eq!(input.value(), "i", "typed, not consumed as a movement");
            assert_eq!(*cursor, 0);
        }
        other => panic!("expected Mode::FunctionList, got {other:?}"),
    }
}

#[test]
fn function_list_moves_on_a_ctrl_remapped_cursor_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            bindings: HashMap::from([("cursor_down".to_string(), vec!["C-n".to_string()])]),
            ..Config::default()
        },
    )
    .unwrap();
    app.dispatch(Action::FunctionList);

    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::CONTROL));
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::CONTROL));
    match &app.mode {
        Mode::FunctionList { input, cursor } => {
            assert_eq!(input.value(), "", "C-n is not text");
            assert_eq!(*cursor, 2);
        }
        other => panic!("expected Mode::FunctionList, got {other:?}"),
    }
}

#[test]
fn function_list_typing_narrows_and_resets_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    if let Mode::FunctionList { cursor, .. } = &app.mode {
        assert_eq!(*cursor, 1);
    }

    for c in "mkdir".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let filtered = app.function_list_filtered_actions();
    assert_eq!(filtered, vec![Action::Mkdir]);
    // Re-filtering resets the highlight to the top.
    if let Mode::FunctionList { cursor, .. } = &app.mode {
        assert_eq!(*cursor, 0);
    }
}

#[test]
fn function_list_enter_closes_the_palette_then_dispatches_the_highlighted_action() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);
    for c in "mkdir".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    // `mkdir` opens a Prompt — proves the palette closed *and* the
    // action actually dispatched (not just returned to Normal).
    assert!(matches!(
        app.mode,
        Mode::Prompt {
            kind: PromptKind::Mkdir,
            ..
        }
    ));
}

#[test]
fn function_list_esc_cancels_without_dispatching_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn jump_search_moves_cursor_to_the_first_case_insensitive_prefix_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("aa.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("ab.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::JumpSearch);
    assert!(matches!(app.mode, Mode::JumpSearch { .. }));
    // Uppercase input must still match the lowercase files.
    app.handle_event(AppEvent::Input(KeyCode::Char('A'), KeyModifiers::NONE));

    assert_eq!(
        cursor_entry_name(&app),
        "aa.txt",
        "must land on the first match in display order, not \"ab.txt\""
    );
}

#[test]
fn jump_search_handles_japanese_prefixes_and_narrows_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("あいう.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("あえお.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("かきく.txt"), b"a").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "かきく.txt");

    app.dispatch(Action::JumpSearch);
    app.handle_event(AppEvent::Input(KeyCode::Char('あ'), KeyModifiers::NONE));
    assert_eq!(
        cursor_entry_name(&app),
        "あいう.txt",
        "first match for the single-character prefix \"あ\""
    );

    // Narrowing the prefix further must re-search from the top and
    // move off "あいう.txt" onto the only remaining match.
    app.handle_event(AppEvent::Input(KeyCode::Char('え'), KeyModifiers::NONE));
    assert_eq!(cursor_entry_name(&app), "あえお.txt");
}

#[test]
fn jump_search_down_and_up_cycle_through_matches_with_wraparound() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cat1.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("cat2.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("cat3.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("dog.txt"), b"a").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();

    app.dispatch(Action::JumpSearch);
    for c in "cat".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(cursor_entry_name(&app), "cat1.txt");

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(cursor_entry_name(&app), "cat2.txt");
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(cursor_entry_name(&app), "cat3.txt");
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(
        cursor_entry_name(&app),
        "cat1.txt",
        "Down from the last match must wrap around to the first"
    );

    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(
        cursor_entry_name(&app),
        "cat3.txt",
        "Up from the first match must wrap around to the last"
    );
}

#[test]
fn jump_search_with_no_match_leaves_the_cursor_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::JumpSearch);
    app.handle_event(AppEvent::Input(KeyCode::Char('z'), KeyModifiers::NONE));
    assert_eq!(cursor_entry_name(&app), "b.txt");
}

#[test]
fn jump_search_esc_restores_the_cursor_to_where_the_search_started() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::JumpSearch);
    app.handle_event(AppEvent::Input(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(cursor_entry_name(&app), "a.txt", "sanity: search moved it");

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(cursor_entry_name(&app), "b.txt");
}

#[test]
fn jump_search_enter_keeps_the_cursor_where_the_search_left_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "b.txt");

    app.dispatch(Action::JumpSearch);
    app.handle_event(AppEvent::Input(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(cursor_entry_name(&app), "a.txt");
}

#[test]
fn jump_search_never_matches_the_parent_entry() {
    let dir = tempfile::tempdir().unwrap();
    // A real filename that happens to start with "..", to make sure a
    // "." or ".." prefix search matches a real entry rather than the
    // synthetic ".." parent row (which isn't a named `FsEntry` at all,
    // but this also documents the exclusion at the behavioral level).
    std::fs::write(dir.path().join("..extra.txt"), b"a").unwrap();
    std::fs::write(dir.path().join("normal.txt"), b"a").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    app.dispatch(Action::ToggleHidden);
    select_entry_named(&mut app, "normal.txt");

    assert!(matches!(
        app.active_pane().visible_entries()[0],
        crate::pane::VisibleItem::Parent
    ));

    app.dispatch(Action::JumpSearch);
    app.handle_event(AppEvent::Input(KeyCode::Char('.'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Char('.'), KeyModifiers::NONE));

    assert_eq!(cursor_entry_name(&app), "..extra.txt");
    assert_ne!(
        app.active_pane().cursor,
        0,
        "must never land on the parent row"
    );
}
