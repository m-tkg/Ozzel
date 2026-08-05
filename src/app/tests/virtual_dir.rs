use super::super::test_support::*;
use super::super::*;
use std::path::Path;

#[test]
fn open_on_a_zip_file_enters_virtual_directory_mode() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");

    app.dispatch(Action::Open);

    let pane = app.active_pane();
    assert!(pane.is_virtual());
    assert_eq!(
        pane.cwd,
        dir.path(),
        "cwd must stay the real containing dir"
    );
    let names: Vec<String> = pane
        .visible_entries()
        .iter()
        .filter_map(|item| match item {
            crate::pane::VisibleItem::Entry(e) => Some(e.name.clone()),
            crate::pane::VisibleItem::Parent => None,
        })
        .collect();
    assert!(names.contains(&"readme.txt".to_string()));
    assert!(names.contains(&"src".to_string()));
}

#[test]
fn virtual_directory_navigation_descends_and_exits_with_cursor_restored() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());

    // Descend into "src".
    move_cursor_onto(app.active_pane_mut(), "src");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());
    assert!(
        app.active_pane()
            .visible_entries()
            .iter()
            .any(|item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "main.rs"))
    );

    // Backspace back up to the archive root.
    app.dispatch(Action::Parent);
    assert!(app.active_pane().is_virtual());
    assert!(
        app.active_pane().visible_entries().iter().any(
            |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "readme.txt")
        )
    );

    // Backspace again exits Virtual Directory mode, cursor lands back
    // on the .zip file itself.
    app.dispatch(Action::Parent);
    let pane = app.active_pane();
    assert!(!pane.is_virtual());
    assert_eq!(pane.cwd, dir.path());
    match pane.visible_entries().get(pane.cursor) {
        Some(crate::pane::VisibleItem::Entry(e)) => assert_eq!(e.name, "project.zip"),
        other => panic!("expected cursor to rest on project.zip, got {other:?}"),
    }
}

#[test]
fn extract_via_copy_copies_marked_entry_to_the_other_panes_real_cwd() {
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    make_test_archive(src_dir.path());
    let mut app = test_app(src_dir.path(), dest_dir.path());

    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());

    move_cursor_onto(app.active_pane_mut(), "readme.txt");
    app.config.confirm_operations = false;
    app.dispatch(Action::Copy);
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(dest_dir.path().join("readme.txt")).unwrap(),
        b"hello from inside the zip"
    );
}

#[test]
fn extract_via_copy_of_a_directory_extracts_the_whole_subtree() {
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    make_test_archive(src_dir.path());
    let mut app = test_app(src_dir.path(), dest_dir.path());

    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    move_cursor_onto(app.active_pane_mut(), "src");
    app.config.confirm_operations = false;
    app.dispatch(Action::Copy);
    wait_for_tasks_done(&mut app);

    assert_eq!(
        std::fs::read(dest_dir.path().join("src/main.rs")).unwrap(),
        b"fn main() {}"
    );
}

#[test]
fn move_is_rejected_inside_a_virtual_directory() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    move_cursor_onto(app.active_pane_mut(), "readme.txt");

    app.dispatch(Action::Move);

    assert!(app.log.back().unwrap().is_error);
    assert!(app.tasks.running.is_empty(), "must not have spawned a move");
}

#[test]
fn mutating_actions_are_all_rejected_inside_a_virtual_directory() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());

    for action in [
        Action::Rename,
        Action::Mkdir,
        Action::Delete,
        Action::Duplicate,
        Action::ZipMarked,
        Action::Unzip,
        Action::OpenEditor,
        Action::OpenDefault,
    ] {
        let mut app = test_app(dir.path(), dir.path());
        move_cursor_onto(app.active_pane_mut(), "project.zip");
        app.dispatch(Action::Open);
        move_cursor_onto(app.active_pane_mut(), "readme.txt");

        app.dispatch(action);

        assert!(
            app.log.back().unwrap().is_error,
            "{action:?} must log a rejection in a virtual pane"
        );
        assert!(
            matches!(app.mode, Mode::Normal),
            "{action:?} must not open a prompt/confirm"
        );
    }
}

#[test]
fn open_on_a_text_file_inside_a_virtual_directory_opens_the_viewer() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    move_cursor_onto(app.active_pane_mut(), "readme.txt");

    app.dispatch(Action::Open);

    match &app.mode {
        Mode::Viewer { lines, .. } => {
            assert_eq!(lines.join("\n"), "hello from inside the zip");
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

#[test]
fn filter_and_sort_work_on_a_virtual_listing() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);

    app.active_pane_mut()
        .set_filter(FilterSpec::parse("readme"));
    let names: Vec<String> = app
        .active_pane()
        .visible_entries()
        .iter()
        .filter_map(|item| match item {
            crate::pane::VisibleItem::Entry(e) => Some(e.name.clone()),
            crate::pane::VisibleItem::Parent => None,
        })
        .collect();
    assert_eq!(names, vec!["readme.txt".to_string()]);

    app.active_pane_mut().set_filter(None);
    app.active_pane_mut().cycle_sort();
    // Sorting must not panic/crash on a virtual listing; the exact
    // resulting order isn't the point of this test.
    assert!(!app.active_pane().visible_entries().is_empty());
}

#[test]
fn jump_to_a_bookmark_exits_virtual_directory_mode() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let other_dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    assert!(app.active_pane().is_virtual());

    app.jump_active_pane_to(other_dir.path().to_path_buf());

    assert!(!app.active_pane().is_virtual());
    assert_eq!(app.active_pane().cwd, other_dir.path());
}

#[test]
fn extension_viewer_command_matches_case_insensitively_on_a_dotless_key() {
    let mut viewers = std::collections::HashMap::new();
    viewers.insert("md".to_string(), "glow {}".to_string());

    assert_eq!(
        extension_viewer_command(&viewers, Path::new("readme.md"), None),
        Some("glow {}".to_string())
    );
    assert_eq!(
        extension_viewer_command(&viewers, Path::new("readme.MD"), None),
        Some("glow {}".to_string())
    );
    assert_eq!(
        extension_viewer_command(&viewers, Path::new("readme.txt"), None),
        None
    );
    assert_eq!(
        extension_viewer_command(&viewers, Path::new("readme"), None),
        None
    );
}

#[test]
fn extension_viewer_command_falls_back_to_the_symlink_targets_extension() {
    let mut viewers = std::collections::HashMap::new();
    viewers.insert("md".to_string(), "glow {}".to_string());
    viewers.insert("txt".to_string(), "less {}".to_string());

    // No extension on the link itself -> falls back to the target's.
    assert_eq!(
        extension_viewer_command(&viewers, Path::new("mylink"), Some(Path::new("notes.md"))),
        Some("glow {}".to_string())
    );
    // The link's own extension has a configured entry -> that wins,
    // even though the target's extension would resolve to a different
    // one.
    assert_eq!(
        extension_viewer_command(
            &viewers,
            Path::new("mylink.txt"),
            Some(Path::new("notes.md"))
        ),
        Some("less {}".to_string())
    );
    // Neither resolves to anything configured.
    assert_eq!(
        extension_viewer_command(
            &viewers,
            Path::new("mylink.rs"),
            Some(Path::new("notes.rs"))
        ),
        None
    );
}

#[test]
fn open_on_a_file_with_a_configured_viewer_queues_an_external_command() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.md"), b"# hi").unwrap();
    let mut viewers = std::collections::HashMap::new();
    viewers.insert("md".to_string(), "glow {}".to_string());
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            viewers,
            ..Config::default()
        },
    )
    .unwrap();
    move_cursor_onto(app.active_pane_mut(), "readme.md");

    app.dispatch(Action::Open);

    let req = app
        .outbox
        .external
        .as_ref()
        .expect("expected a queued external viewer command");
    assert!(req.cmdline.starts_with("glow "));
    assert!(req.cmdline.contains("readme.md"));
    assert!(!req.pause_after);
    assert_eq!(req.cwd, dir.path());
    // The built-in viewer must not have opened instead.
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn open_on_a_file_without_a_configured_viewer_falls_back_to_the_built_in_viewer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.txt"), b"hello").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    move_cursor_onto(app.active_pane_mut(), "readme.txt");

    app.dispatch(Action::Open);

    assert!(app.outbox.external.is_none());
    assert!(matches!(app.mode, Mode::Viewer { .. }));
}

#[test]
fn open_on_an_archived_file_with_a_configured_viewer_falls_back_to_the_built_in_viewer() {
    let dir = tempfile::tempdir().unwrap();
    make_test_archive(dir.path());
    let mut viewers = std::collections::HashMap::new();
    viewers.insert("txt".to_string(), "less {}".to_string());
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            viewers,
            ..Config::default()
        },
    )
    .unwrap();
    move_cursor_onto(app.active_pane_mut(), "project.zip");
    app.dispatch(Action::Open);
    move_cursor_onto(app.active_pane_mut(), "readme.txt");

    app.dispatch(Action::Open);

    assert!(
        app.outbox.external.is_none(),
        "external viewers must never fire on a virtual (in-archive) entry"
    );
    assert!(matches!(app.mode, Mode::Viewer { .. }));
    assert!(
        app.log
            .back()
            .unwrap()
            .message
            .contains("don't apply inside archives")
    );
}
