use super::super::test_support::*;
use super::super::*;

#[test]
fn new_unloaded_builds_both_panes_with_no_entries_and_no_io() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();

    let app = App::new_unloaded(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config::default(),
    )
    .unwrap();
    assert!(app.panes[0].entries.is_empty());
    assert!(app.panes[1].entries.is_empty());
}

#[test]
fn load_initial_dirs_populates_both_panes_matching_a_plain_new() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();

    let mut app = App::new_unloaded(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config::default(),
    )
    .unwrap();
    app.load_initial_dirs().unwrap();

    let eager = test_app(dir.path(), dir.path());
    assert_eq!(app.panes[0].entries.len(), eager.panes[0].entries.len());
    assert_eq!(app.panes[0].entries[0].name, "a.txt");
    assert!(app.needs_redraw);
}

#[test]
fn load_initial_dirs_propagates_a_reload_failure() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    std::fs::create_dir(&gone).unwrap();

    let mut app =
        App::new_unloaded(gone.clone(), dir.path().to_path_buf(), Config::default()).unwrap();
    std::fs::remove_dir(&gone).unwrap();

    assert!(app.load_initial_dirs().is_err());
}
