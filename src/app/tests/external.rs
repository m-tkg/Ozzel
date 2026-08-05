use super::super::test_support::*;
use super::super::*;

#[test]
fn command_line_prompt_commit_sets_pending_external() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::CommandLine);
    assert!(matches!(
        app.mode,
        Mode::Prompt {
            kind: PromptKind::Command,
            ..
        }
    ));

    for c in "ls -la".chars() {
        app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    let req = app
        .outbox
        .external
        .take()
        .expect("expected a pending external request");
    assert_eq!(req.cmdline, "ls -la");
    assert_eq!(req.cwd, dir.path());
    assert!(req.pause_after);
}

#[test]
fn command_line_empty_input_cancels_without_pending_external() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::CommandLine);
    app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.outbox.external.is_none());
}

#[test]
fn open_editor_queues_suspended_command_with_configured_editor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file.txt"), b"hi").unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            editor: Some("vim".to_string()),
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "file.txt");

    app.dispatch(Action::OpenEditor);
    let req = app
        .outbox
        .external
        .take()
        .expect("expected a pending external request");
    assert!(
        req.cmdline.starts_with("vim "),
        "cmdline was: {}",
        req.cmdline
    );
    assert!(req.cmdline.contains("file.txt"));
    assert!(
        !req.pause_after,
        "editors don't get the press-any-key pause"
    );
}

#[test]
fn open_editor_errors_when_cursor_is_on_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            editor: Some("vim".to_string()),
            ..Config::default()
        },
    )
    .unwrap();
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "sub");

    app.dispatch(Action::OpenEditor);
    assert!(app.outbox.external.is_none());
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("not on a file"))
    );
}

#[test]
fn edit_config_creates_the_template_when_missing_and_queues_reload() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            editor: Some("vim".to_string()),
            ..Config::default()
        },
    )
    .unwrap();

    // Nested + missing: exercises both the create_dir_all and the
    // template-writing halves of ensure_config_file_exists.
    let config_path = dir.path().join("nested").join("config.toml");
    assert!(!config_path.exists());

    app.begin_edit_config_at(config_path.clone());

    assert!(
        config_path.exists(),
        "must create the file from the template"
    );
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        written.contains("delete_behavior"),
        "should be the examples/config.toml template, got: {written}"
    );

    let req = app
        .outbox
        .external
        .take()
        .expect("expected a pending external request");
    assert!(req.cmdline.starts_with("vim "), "cmdline: {}", req.cmdline);
    assert!(req.cmdline.contains("config.toml"));
    assert!(!req.pause_after);
    assert!(
        app.outbox.config_reload,
        "must queue a reload for after the editor exits"
    );
}

#[test]
fn edit_config_does_not_overwrite_an_existing_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "delete_behavior = \"permanent\"").unwrap();

    app.begin_edit_config_at(config_path.clone());

    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        "delete_behavior = \"permanent\"",
        "an existing config file must be left untouched"
    );
}

#[test]
fn edit_config_falls_back_to_vim_when_no_editor_or_env_var_is_set() {
    let dir = tempfile::tempdir().unwrap();
    // No `config.editor` set — this is exactly the case OpenEditor
    // would refuse ("no editor configured"), but edit_config must
    // still work out of the box per the user's request.
    let mut app = test_app(dir.path(), dir.path());
    // Isolate from whatever $EDITOR happens to be set in the test
    // environment, so this assertion is deterministic everywhere.
    // SAFETY: single-threaded w.r.t. this var within this test process
    // is not guaranteed by the test harness, but no other test reads
    // or depends on $EDITOR, so this is safe in practice.
    unsafe {
        std::env::remove_var("EDITOR");
    }

    app.begin_edit_config_at(dir.path().join("config.toml"));

    let req = app.outbox.external.unwrap();
    assert!(
        req.cmdline.starts_with("vim "),
        "must fall back to a hardcoded vim, cmdline: {}",
        req.cmdline
    );
}

#[test]
fn reload_config_success_swaps_the_keymap_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // Not bound by default; proves the *new* config's keymap is the one
    // actually in effect afterward, not just re-parsed and discarded.
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
        None
    );

    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "[keys]\n\"z\" = \"quit\"\n").unwrap();
    app.reload_config_from(&config_path);

    assert_eq!(
        app.keymap.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
        Some(Action::Quit),
        "the reloaded config's [keys] override must take effect immediately"
    );
    assert!(
        app.log
            .iter()
            .any(|l| !l.is_error && l.message == "config reloaded"),
        "log: {:?}",
        app.log
    );
}

/// `App::help_lines`/`App::settings_keybinding_lines` cache their
/// respective keymap-derived listings, keyed by `Keymap::generation` —
/// this pins the actual invalidation trigger (a config reload swapping
/// in a new `Keymap`, same as the test above) rather than just the
/// content, so a bug that served a stale cache across a real keymap
/// change would fail here even though both listings still *build*
/// correctly in isolation.
#[test]
fn help_lines_and_settings_keybinding_lines_reflect_a_reloaded_keymap() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    let quit_row_before = app
        .help_lines()
        .iter()
        .find(|l| matches!(l, HelpLine::Binding { action, .. } if *action == "quit"))
        .cloned();
    match &quit_row_before {
        Some(HelpLine::Binding { keys, .. }) => assert!(keys.contains('q'), "keys: {keys}"),
        other => panic!("expected a quit binding row, got {other:?}"),
    }
    let quit_index = Action::ALL.iter().position(|a| *a == Action::Quit).unwrap();
    // Compare against the exact combos, not a raw substring check —
    // `action.config_name()` is itself "quit" (contains a 'q'!), so a
    // plain `.contains('q')`/`!.contains('q')` on the whole formatted
    // line would pass or fail for the wrong reason.
    let combos_before = settings::combos_for(&app.keymap, Action::Quit);
    assert!(combos_before.iter().any(|c| c == "q"), "{combos_before:?}");
    let quit_keybinding_line_before = app.settings_keybinding_lines()[quit_index].clone();
    assert!(
        quit_keybinding_line_before.contains(&combos_before.join(", ")),
        "{quit_keybinding_line_before}"
    );

    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "[keys]\n\"q\" = \"none\"\n\"z\" = \"quit\"\n").unwrap();
    app.reload_config_from(&config_path);

    let quit_row_after = app
        .help_lines()
        .iter()
        .find(|l| matches!(l, HelpLine::Binding { action, .. } if *action == "quit"))
        .cloned();
    match &quit_row_after {
        Some(HelpLine::Binding { keys, .. }) => {
            assert!(keys.contains('z'), "keys: {keys}");
            assert!(!keys.split(", ").any(|k| k == "q"), "keys: {keys}");
        }
        other => panic!("expected a quit binding row, got {other:?}"),
    }
    let combos_after = settings::combos_for(&app.keymap, Action::Quit);
    assert!(combos_after.iter().any(|c| c == "z"), "{combos_after:?}");
    assert!(!combos_after.iter().any(|c| c == "q"), "{combos_after:?}");
    let quit_keybinding_line_after = app.settings_keybinding_lines()[quit_index].clone();
    assert!(
        quit_keybinding_line_after.contains(&combos_after.join(", ")),
        "{quit_keybinding_line_after}"
    );
    assert_ne!(
        quit_keybinding_line_before, quit_keybinding_line_after,
        "the cache must not still be serving the pre-reload combos"
    );
}

#[test]
fn reload_config_failure_keeps_the_old_config_and_keymap_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();

    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "delete_behavior = [not valid").unwrap();
    app.reload_config_from(&config_path);

    assert_eq!(
        app.config.delete_behavior,
        crate::config::DeleteBehavior::Permanent,
        "a parse error must leave the old config completely untouched"
    );
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.starts_with("config reload failed")),
        "log: {:?}",
        app.log
    );
}

#[test]
fn reload_config_unknown_top_level_key_keeps_the_old_config_and_logs() {
    // Regression test for the reported bug: a `[viewers]` entry left
    // uncommented while its section header stayed commented out used
    // to be silently dropped by serde's default behavior. With
    // `deny_unknown_fields` this must now be treated exactly like any
    // other malformed reload — old config/keymap untouched, error
    // logged — never applied half-parsed.
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            delete_behavior: crate::config::DeleteBehavior::Permanent,
            ..Config::default()
        },
    )
    .unwrap();

    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "md = \"mdviewer {}\"").unwrap();
    app.reload_config_from(&config_path);

    assert_eq!(
        app.config.delete_behavior,
        crate::config::DeleteBehavior::Permanent,
        "an unknown-key parse error must leave the old config completely untouched"
    );
    assert!(app.config.viewers.is_empty());
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.starts_with("config reload failed")),
        "log: {:?}",
        app.log
    );
}

#[test]
fn reload_config_bad_keys_entry_keeps_the_old_keymap_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let original_quit = app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE);

    let config_path = dir.path().join("config.toml");
    std::fs::write(&config_path, "[keys]\n\"q\" = \"not_a_real_action\"\n").unwrap();
    app.reload_config_from(&config_path);

    assert_eq!(
        app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
        original_quit,
        "an invalid [keys] action name must leave the old keymap untouched"
    );
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.starts_with("config reload failed")),
        "log: {:?}",
        app.log
    );
}
