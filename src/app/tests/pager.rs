use super::super::test_support::*;
use super::super::*;

#[test]
fn open_action_opens_the_built_in_viewer_on_a_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "notes.txt");

    app.dispatch(Action::Open);
    match &app.mode {
        Mode::Viewer {
            path,
            lines,
            bytes,
            view_mode,
            scroll,
            h_scroll,
            truncated,
            search,
        } => {
            assert_eq!(path, &dir.path().join("notes.txt"));
            assert_eq!(
                lines,
                &vec!["one".to_string(), "two".to_string(), "three".to_string()]
            );
            assert_eq!(bytes.as_slice(), b"one\ntwo\nthree");
            assert_eq!(*view_mode, ViewMode::Text);
            assert_eq!(*scroll, 0);
            assert_eq!(*h_scroll, 0);
            assert!(!truncated);
            assert!(matches!(search, ViewerSearch::Idle));
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn open_on_a_directory_symlink_navigates_instead_of_opening_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let real_dir = dir.path().join("real_dir");
    std::fs::create_dir(&real_dir).unwrap();
    let link = dir.path().join("link_to_dir");
    std::os::unix::fs::symlink(&real_dir, &link).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "link_to_dir");

    app.dispatch(Action::Open);
    assert!(
        matches!(app.mode, Mode::Normal),
        "must navigate, not open a viewer"
    );
    assert_eq!(
        app.active_pane().cwd,
        link,
        "cwd must be the link's own path"
    );
}

#[cfg(unix)]
#[test]
fn open_on_a_file_symlink_opens_the_viewer_on_the_targets_content() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    std::fs::write(&target, "hello through the link").unwrap();
    let link = dir.path().join("link_to_file.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "link_to_file.txt");

    app.dispatch(Action::Open);
    match &app.mode {
        Mode::Viewer { path, lines, .. } => {
            // The viewer opens the *link's* path (reading through it
            // transparently follows to the target's bytes — no path
            // substitution needed).
            assert_eq!(path, &link);
            assert_eq!(lines, &vec!["hello through the link".to_string()]);
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn open_on_a_dangling_symlink_logs_an_error_instead_of_opening_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "dangling");

    app.dispatch(Action::Open);
    assert!(
        matches!(app.mode, Mode::Normal),
        "a dangling symlink must not open the viewer or navigate"
    );
    assert!(
        app.log.iter().any(|l| l.is_error),
        "opening a dangling symlink must log an error; log: {:?}",
        app.log
    );
}

#[test]
fn open_on_a_file_opens_the_viewer_instead_of_navigating() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "readme.txt");

    app.dispatch(Action::Open);
    assert!(matches!(app.mode, Mode::Viewer { .. }));
    assert_eq!(
        app.panes[0].cwd,
        dir.path(),
        "cwd must not change for a file"
    );
}

#[test]
fn open_opens_a_binary_file_directly_in_hex_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bin.dat"), [b'a', 0u8, b'b']).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "bin.dat");

    app.dispatch(Action::Open);
    assert_eq!(view_mode_of(&app), ViewMode::Hex);
}

#[test]
fn viewer_tab_toggles_between_text_and_hex_and_resets_scroll() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "notes.txt");
    app.dispatch(Action::Open);
    assert_eq!(view_mode_of(&app), ViewMode::Text);

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 1);

    app.handle_event(AppEvent::Input(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(view_mode_of(&app), ViewMode::Hex);
    assert_eq!(scroll_of(&app), 0, "toggling mode resets scroll");

    app.handle_event(AppEvent::Input(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(view_mode_of(&app), ViewMode::Text);
}

#[test]
fn viewer_hex_scroll_clamps_by_sixteen_byte_rows() {
    let dir = tempfile::tempdir().unwrap();
    // 20 NUL bytes -> sniffed as binary (opens in hex mode) and
    // ceil(20/16) = 2 hex rows, so max scroll index is 1.
    std::fs::write(dir.path().join("bytes.dat"), vec![0u8; 20]).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "bytes.dat");
    app.dispatch(Action::Open);
    assert_eq!(view_mode_of(&app), ViewMode::Hex);

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 1);
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 1, "must clamp at the last hex row");

    app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 1);
    app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0);
}

#[test]
fn viewer_scroll_clamps_to_the_line_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lines.txt"), "1\n2\n3").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "lines.txt");
    app.dispatch(Action::Open);

    // Up from the very top stays at 0.
    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0);

    // Down twice reaches the last line (3 lines: max index 2)...
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);
    // ...and one more Down doesn't overshoot it.
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);

    app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0);
    app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);
    app.handle_event(AppEvent::Input(KeyCode::Char('g'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0);
    app.handle_event(AppEvent::Input(KeyCode::Char('G'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);
}

#[test]
fn viewer_page_up_down_clamp_too() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (0..5).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(dir.path().join("many.txt"), content).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "many.txt");
    app.dispatch(Action::Open);

    app.handle_event(AppEvent::Input(KeyCode::PageDown, KeyModifiers::NONE));
    assert_eq!(
        scroll_of(&app),
        4,
        "PageDown must clamp to the last line, not overshoot"
    );
    app.handle_event(AppEvent::Input(KeyCode::PageUp, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0, "PageUp must clamp to 0, not underflow");
}

#[test]
fn viewer_left_right_scroll_horizontally_without_underflow() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("wide.txt"), "a line of text").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "wide.txt");
    app.dispatch(Action::Open);

    // Left at the start must not underflow (saturating_sub).
    app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(h_scroll_of(&app), 0);

    app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
    let after_one_right = h_scroll_of(&app);
    assert!(after_one_right > 0);
    app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(h_scroll_of(&app), 0);
}

#[test]
fn viewer_q_and_esc_close_back_to_normal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");

    app.dispatch(Action::Open);
    assert!(matches!(app.mode, Mode::Viewer { .. }));
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));

    app.dispatch(Action::Open);
    assert!(matches!(app.mode, Mode::Viewer { .. }));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn viewer_forward_search_jumps_to_the_first_match_at_or_below_the_top() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(matches!(
        &app.mode,
        Mode::Viewer {
            search: ViewerSearch::Editing {
                direction: SearchDirection::Forward,
                ..
            },
            ..
        }
    ),);
    type_into_viewer(&mut app, "needle");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(scroll_of(&app), 2, "first match at-or-below the top (0)");
    match search_of(&app) {
        ViewerSearch::Active {
            pattern,
            matches,
            current,
            wrapped,
            ..
        } => {
            assert_eq!(pattern, "needle");
            assert_eq!(matches, vec![2, 5, 8]);
            assert_eq!(current, 0);
            assert!(!wrapped);
        }
        other => panic!("expected an active search, got {other:?}"),
    }
}

#[test]
fn viewer_n_and_capital_n_cycle_forward_and_backward_with_wraparound() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "needle");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);

    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 5);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 8);

    // One more `n` past the last match wraps to the first.
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 2);
    match search_of(&app) {
        ViewerSearch::Active { wrapped, .. } => assert!(wrapped, "must note the wrap"),
        other => panic!("expected an active search, got {other:?}"),
    }

    // `N` reverses: from the (wrapped-to) first match, going backward
    // lands on the last one again, and this jump does NOT wrap (moving
    // from index 0 to index 2 backward is exactly the wraparound this
    // assertion is checking resets `wrapped` back to false only when
    // the step itself isn't a wrap — here it is one, so it stays set).
    app.handle_event(AppEvent::Input(KeyCode::Char('N'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 8);

    // A plain, non-wrapping `N` from here clears the notice.
    app.handle_event(AppEvent::Input(KeyCode::Char('N'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 5);
    match search_of(&app) {
        ViewerSearch::Active { wrapped, .. } => assert!(!wrapped),
        other => panic!("expected an active search, got {other:?}"),
    }
}

#[test]
fn viewer_backward_search_and_its_n_direction_is_inverted() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    // Move to line 8 first so the backward search has somewhere to
    // search "upward" from other than the very top.
    for _ in 0..8 {
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(scroll_of(&app), 8);

    app.handle_event(AppEvent::Input(KeyCode::Char('?'), KeyModifiers::NONE));
    assert!(matches!(
        &app.mode,
        Mode::Viewer {
            search: ViewerSearch::Editing {
                direction: SearchDirection::Backward,
                ..
            },
            ..
        }
    ));
    type_into_viewer(&mut app, "needle");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    // Already sitting on a match (line 8) — backward search finds the
    // nearest match at-or-*before* the top line, which is itself.
    assert_eq!(scroll_of(&app), 8);

    // `n` repeats the ORIGINAL (backward) direction: upward.
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 5);

    // `N` inverts it: downward, back to 8.
    app.handle_event(AppEvent::Input(KeyCode::Char('N'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 8);
}

#[test]
fn viewer_esc_while_editing_cancels_and_restores_the_previous_search() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "needle");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    let before = search_of(&app);
    assert_eq!(scroll_of(&app), 2);

    // Start a second, different search but back out of it.
    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "filler");
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    // `ViewerSearch` has no `PartialEq` (its `matcher` is a compiled
    // `regex::Regex` under the hood — see the type's own doc comment),
    // so compare the fields that actually carry the search's identity
    // directly instead.
    match (&search_of(&app), &before) {
        (
            ViewerSearch::Active {
                pattern: p1,
                direction: d1,
                matches: m1,
                current: c1,
                wrapped: w1,
                ..
            },
            ViewerSearch::Active {
                pattern: p2,
                direction: d2,
                matches: m2,
                current: c2,
                wrapped: w2,
                ..
            },
        ) => {
            assert_eq!(p1, p2, "canceling must restore the exact same pattern");
            assert_eq!(d1, d2, "canceling must restore the exact same direction");
            assert_eq!(m1, m2, "canceling must restore the exact same matches");
            assert_eq!(c1, c2, "canceling must restore the exact same cursor");
            assert_eq!(w1, w2, "canceling must restore the exact same wrap flag");
        }
        other => panic!("expected both to be an active search, got {other:?}"),
    }
    assert_eq!(scroll_of(&app), 2, "canceling must not move the cursor");
    assert!(matches!(app.mode, Mode::Viewer { .. }), "must stay open");
}

#[test]
fn viewer_esc_in_normal_state_clears_the_search_then_a_second_esc_closes() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "needle");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(search_of(&app), ViewerSearch::Active { .. }));

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(search_of(&app), ViewerSearch::Idle));
    assert!(
        matches!(app.mode, Mode::Viewer { .. }),
        "first Esc only clears highlights, stays open"
    );

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal), "second Esc closes");
}

#[test]
fn viewer_enter_with_an_empty_pattern_cancels_like_esc() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(search_of(&app), ViewerSearch::Idle));
    assert_eq!(scroll_of(&app), 0);
    assert!(matches!(app.mode, Mode::Viewer { .. }));
}

#[test]
fn viewer_search_with_no_match_logs_an_error_and_leaves_search_idle() {
    let dir = tempfile::tempdir().unwrap();
    write_needle_file(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "haystack.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "nonexistent");
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(search_of(&app), ViewerSearch::Idle));
    assert_eq!(scroll_of(&app), 0, "no match must not move the cursor");
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("nonexistent")),
        "log: {:?}",
        app.log
    );
}

#[test]
fn viewer_hex_mode_search_matches_the_formatted_hex_dump_line() {
    let dir = tempfile::tempdir().unwrap();
    // 20 bytes of zeros -> sniffs as binary, opens in hex mode, and
    // spans 2 sixteen-byte rows (ceil(20/16) = 2).
    let mut bytes = vec![0u8; 20];
    bytes[17] = b'Z'; // lands in the second hex row (bytes 16..20)
    std::fs::write(dir.path().join("bin.dat"), &bytes).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "bin.dat");
    assert_eq!(view_mode_of(&app), ViewMode::Hex);

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into_viewer(&mut app, "5a"); // hex for 'Z', case-insensitive
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(scroll_of(&app), 1, "the row containing the 'Z' byte");
}

#[test]
fn viewer_vim_style_scroll_keys_behave_like_their_arrow_equivalents() {
    let dir = tempfile::tempdir().unwrap();
    let content: String = (0..50)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.path().join("many.txt"), content).unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_viewer_on(&mut app, "many.txt");

    app.handle_event(AppEvent::Input(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 1, "j must scroll down one line, like Down");
    app.handle_event(AppEvent::Input(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0, "k must scroll up one line, like Up");

    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(
        scroll_of(&app),
        VIEWER_HALF_PAGE_SIZE,
        "d must scroll down half a page"
    );
    app.handle_event(AppEvent::Input(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0, "u must scroll back up half a page");

    app.handle_event(AppEvent::Input(KeyCode::Char('f'), KeyModifiers::NONE));
    assert_eq!(
        scroll_of(&app),
        VIEWER_PAGE_SIZE,
        "f must scroll down a full page, like PageDown"
    );
    app.handle_event(AppEvent::Input(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(scroll_of(&app), 0, "b must scroll back up a full page");

    app.handle_event(AppEvent::Input(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(
        scroll_of(&app),
        VIEWER_PAGE_SIZE,
        "Space must page down exactly like f/PageDown"
    );
}

#[test]
fn help_action_opens_the_help_screen() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Help);
    assert!(matches!(app.mode, Mode::Help { scroll: 0, .. }));
}

#[test]
fn h_and_question_mark_both_open_help_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.handle_event(AppEvent::Input(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Help { .. }));
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));

    app.handle_event(AppEvent::Input(KeyCode::Char('?'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Help { .. }));
}

#[test]
fn shift_h_opens_history_jump_and_h_no_longer_does() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // No history recorded yet, so History still logs its usual error —
    // the point here is just that `S-h` reaches HistoryJump, not `h`.
    app.handle_event(AppEvent::Input(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("no history")),
        "S-h must resolve to HistoryJump: {:?}",
        app.log
    );
    assert!(!matches!(app.mode, Mode::Help { .. }));
}

#[test]
fn go_home_end_to_end_is_bound_only_to_tilde() {
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

    // `S-h`/`H` no longer reaches GoHome (it's HistoryJump now); the
    // pane must not have moved.
    app.handle_event(AppEvent::Input(KeyCode::Char('H'), KeyModifiers::SHIFT));
    assert_eq!(app.panes[0].cwd, start.path());

    app.handle_event(AppEvent::Input(KeyCode::Char('~'), KeyModifiers::NONE));
    assert_eq!(app.panes[0].cwd, home.path());
}

#[test]
fn help_screen_scroll_clamps_and_closes_with_q_esc_or_h() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::Help);
    let max_scroll = crate::help::build_lines(&app.keymap)
        .len()
        .saturating_sub(1);
    assert!(max_scroll > 0, "the listing must have more than one line");

    // Up from the top stays at 0.
    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 0);

    app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), max_scroll);
    // One more Down past the end doesn't overshoot.
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), max_scroll);

    app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 0);

    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));

    app.dispatch(Action::Help);
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));

    app.dispatch(Action::Help);
    app.handle_event(AppEvent::Input(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn help_listing_reflects_a_keys_override_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            keys: {
                let mut keys = std::collections::HashMap::new();
                keys.insert("z".to_string(), "quit".to_string());
                keys
            },
            ..Config::default()
        },
    )
    .unwrap();

    let lines = crate::help::build_lines(&app.keymap);
    let quit_row = lines
        .iter()
        .find(|l| matches!(l, crate::help::HelpLine::Binding { action, .. } if *action == "quit"));
    match quit_row {
        Some(crate::help::HelpLine::Binding { keys, .. }) => {
            assert!(keys.contains('z'), "keys: {keys}")
        }
        other => panic!("expected a quit binding row, got {other:?}"),
    }

    app.dispatch(Action::Help);
    assert!(matches!(app.mode, Mode::Help { .. }));
}

#[test]
fn help_screen_less_style_scroll_keys_move_the_same_as_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Help);
    let max_scroll = crate::help::build_lines(&app.keymap)
        .len()
        .saturating_sub(1);
    assert!(
        max_scroll > VIEWER_PAGE_SIZE,
        "listing must be long enough to page"
    );

    app.handle_event(AppEvent::Input(KeyCode::Char('j'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 1);
    app.handle_event(AppEvent::Input(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 0);

    app.handle_event(AppEvent::Input(KeyCode::Char('f'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), VIEWER_PAGE_SIZE);
    app.handle_event(AppEvent::Input(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 0);

    app.handle_event(AppEvent::Input(KeyCode::Char(' '), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), VIEWER_PAGE_SIZE);
    app.handle_event(AppEvent::Input(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(
        help_scroll_of(&app),
        VIEWER_PAGE_SIZE - VIEWER_HALF_PAGE_SIZE
    );
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), VIEWER_PAGE_SIZE);

    app.handle_event(AppEvent::Input(KeyCode::Char('g'), KeyModifiers::NONE));
    assert_eq!(help_scroll_of(&app), 0);
    app.handle_event(AppEvent::Input(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert_eq!(help_scroll_of(&app), max_scroll);
}

#[test]
fn help_search_finds_a_binding_row_n_wraps_and_esc_two_step_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Help);

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(matches!(
        &app.mode,
        Mode::Help {
            search: ViewerSearch::Editing {
                direction: SearchDirection::Forward,
                ..
            },
            ..
        }
    ));
    for c in "duplicate".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    let (scroll_after_search, matches_len) = match &app.mode {
        Mode::Help {
            scroll,
            search: ViewerSearch::Active { matches, .. },
        } => (*scroll, matches.len()),
        other => panic!("expected an active Help search, got {other:?}"),
    };
    assert_eq!(matches_len, 1, "\"duplicate\" appears in exactly one row");
    let lines = crate::help::build_display_lines(&app.keymap);
    assert!(
        lines[scroll_after_search].contains("duplicate"),
        "landed on: {:?}",
        lines[scroll_after_search]
    );

    // A single match's `n`/`N` both just re-land on it (nothing else
    // to wrap to) but must still report `wrapped` since it's the only
    // match either direction.
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    match &app.mode {
        Mode::Help {
            search: ViewerSearch::Active { wrapped, .. },
            ..
        } => assert!(*wrapped),
        other => panic!("expected an active Help search, got {other:?}"),
    }

    // First Esc clears the search but stays open.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Help {
            search: ViewerSearch::Idle,
            ..
        }
    ));
    // Second Esc closes the screen.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn help_search_handles_a_japanese_pattern_without_byte_offset_corruption() {
    // The help listing's own text is English-only (action names/
    // descriptions, fixed-key lines), so a Japanese pattern always
    // reports "not found" here — the point of this test is that a
    // multi-byte pattern doesn't panic or corrupt anything on its way
    // through (byte-offset-based `Matcher::find_ranges`/highlighting),
    // not that it matches. The Log view's own Japanese-search test
    // covers the "does match" case, since real log messages (e.g. a
    // Japanese filename in an operation log line) commonly contain
    // Japanese text where the help listing never does.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Help);

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "コピー".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        Mode::Help {
            search: ViewerSearch::Idle,
            ..
        }
    ));
    assert!(app.log.back().unwrap().is_error);
}

#[test]
fn help_search_no_match_logs_and_leaves_search_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Help);

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "zzzznotpresent".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        Mode::Help {
            search: ViewerSearch::Idle,
            ..
        }
    ));
    assert!(app.log.back().unwrap().is_error);
}

#[test]
fn show_log_opens_scrolled_to_the_bottom_and_q_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::ShowLog);
    match app.mode {
        Mode::Log {
            scroll_from_bottom, ..
        } => assert_eq!(scroll_from_bottom, 0),
        other => panic!("expected Mode::Log, got {other:?}"),
    }
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn log_view_scroll_keys_move_and_saturate() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::ShowLog);

    app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 1,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));

    // Down at 0 must saturate at 0, not underflow/panic.
    app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: usize::MAX,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));
}

#[test]
fn log_view_less_style_scroll_keys_move_the_same_direction_as_up_down() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::ShowLog);

    app.handle_event(AppEvent::Input(KeyCode::Char('k'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 1,
            ..
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('j'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Char('b'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: VIEWER_PAGE_SIZE,
            ..
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('f'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Char('u'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: VIEWER_HALF_PAGE_SIZE,
            ..
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));

    app.handle_event(AppEvent::Input(KeyCode::Char('g'), KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: usize::MAX,
            ..
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Char('G'), KeyModifiers::SHIFT));
    assert!(matches!(
        app.mode,
        Mode::Log {
            scroll_from_bottom: 0,
            ..
        }
    ));
}

#[test]
fn log_search_finds_a_message_n_wraps_and_esc_two_step_closes() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    for i in 0..5 {
        app.log_info(format!("plain line {i}"));
    }
    app.log_info("special NEEDLE line".to_string());
    app.dispatch(Action::ShowLog);
    app.log_view_width = 80;

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(matches!(
        &app.mode,
        Mode::Log {
            search: ViewerSearch::Editing {
                direction: SearchDirection::Forward,
                ..
            },
            ..
        }
    ));
    for c in "NEEDLE".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    match &app.mode {
        Mode::Log {
            search: ViewerSearch::Active { matches, .. },
            ..
        } => assert_eq!(matches.len(), 1),
        other => panic!("expected an active Log search, got {other:?}"),
    }

    // A first Esc clears the search but stays open; a second closes.
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(
        app.mode,
        Mode::Log {
            search: ViewerSearch::Idle,
            ..
        }
    ));
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn log_search_supports_japanese_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.log_info("日本語のログメッセージ".to_string());
    app.dispatch(Action::ShowLog);
    app.log_view_width = 80;

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "日本語".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    match &app.mode {
        Mode::Log {
            search: ViewerSearch::Active { matches, .. },
            ..
        } => assert!(!matches.is_empty()),
        other => panic!("expected an active Log search, got {other:?}"),
    }
}

#[test]
fn log_search_no_match_logs_an_error_and_leaves_search_idle() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.log_info("hello".to_string());
    app.dispatch(Action::ShowLog);
    app.log_view_width = 80;

    app.handle_event(AppEvent::Input(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "zzznotpresent".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        app.mode,
        Mode::Log {
            search: ViewerSearch::Idle,
            ..
        }
    ));
    assert!(app.log.back().unwrap().is_error);
}

/// `App::log_wrapped`'s `(log_generation, width)`-keyed cache must
/// rebuild on *either* trigger independently — a new log line at the
/// same width, or the same log at a different width — not just once at
/// startup.
#[test]
fn log_wrapped_cache_invalidates_on_a_new_line_or_a_different_width() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.log_info("first".to_string());

    let first = app.log_wrapped(80).to_vec();
    assert!(first.iter().any(|(text, _)| text.contains("first")));

    app.log_info("second".to_string());
    let after_append = app.log_wrapped(80).to_vec();
    assert!(
        after_append.iter().any(|(text, _)| text.contains("second")),
        "a newly-appended line must show up, not a stale cached wrap: {after_append:?}"
    );
    assert_ne!(
        first.len(),
        after_append.len(),
        "must reflect the new line, not a stale cache"
    );

    let narrow = app.log_wrapped(20).to_vec();
    assert!(
        narrow
            .iter()
            .all(|(text, _)| unicode_width::UnicodeWidthStr::width(text.as_str()) <= 20),
        "a different width must be honored, not served from the width-80 cache: {narrow:?}"
    );
}
