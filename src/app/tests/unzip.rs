//! The `u` (whole-archive extract) action end to end: format detection,
//! the stem-named destination directory it creates in the other pane, and
//! the rejections. The worker side of each format is covered in
//! `tasks::archive`'s own tests — these are about what `begin_unzip` /
//! `continue_unzip` decide before spawning it.

use super::super::test_support::*;
use super::super::*;

/// Dispatches `u` on `name` in the left pane and waits for the extraction
/// task to finish, returning the app so the caller can assert on the log.
fn unzip(app: &mut App, name: &str) {
    app.active_pane_mut().reload().unwrap();
    move_cursor_onto(app.active_pane_mut(), name);
    app.dispatch(Action::Unzip);
    wait_for_tasks_done(app);
}

#[test]
fn unzip_extracts_a_zip_into_a_new_stem_named_subdirectory() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_archive(left.path());
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "project.zip");

    let dest = right.path().join("project");
    assert!(dest.join("readme.txt").is_file(), "{:?}", app.log);
    assert!(dest.join("src/main.rs").is_file());
    assert!(
        !right.path().join("readme.txt").exists(),
        "nothing may land directly in the other pane's cwd"
    );
}

#[test]
fn unzip_uniquifies_the_subdirectory_when_the_stem_is_taken() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_archive(left.path());
    std::fs::create_dir(right.path().join("project")).unwrap();
    std::fs::write(right.path().join("project/keep.txt"), b"sentinel").unwrap();
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "project.zip");

    assert!(right.path().join("project-1/readme.txt").is_file());
    assert_eq!(
        std::fs::read(right.path().join("project/keep.txt")).unwrap(),
        b"sentinel",
        "`u` must never write into an existing directory"
    );
}

#[test]
fn unzip_extracts_a_tar_gz_into_a_stem_named_subdirectory() {
    // The case that failed outright before `u` became extension-aware.
    // `project`, not `project.tar` — `Path::file_stem` is not enough here.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_tar_gz(left.path());
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "project.tar.gz");

    assert!(
        right.path().join("project/readme.txt").is_file(),
        "{:?}",
        app.log
    );
    assert!(right.path().join("project/src/main.rs").is_file());
    assert!(!right.path().join("project.tar").exists());
}

#[test]
fn unzip_does_not_probe_a_tar_for_zip_encryption() {
    // `zip_has_encrypted_entries` opens its argument *as a zip*, so an
    // ungated probe would fail a `.tar.gz` before extraction was ever
    // attempted.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_tar_gz(left.path());
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "project.tar.gz");

    assert!(
        !app.log.iter().any(|l| l.message.contains("zip archive")),
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[test]
fn unzip_on_a_single_gz_writes_the_payload_without_a_wrapper_directory() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_gz(left.path(), b"plain notes");
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "notes.txt.gz");

    assert_eq!(
        std::fs::read(right.path().join("notes.txt")).unwrap(),
        b"plain notes"
    );
    assert!(
        !right.path().join("notes").exists(),
        "a single payload gets no wrapper directory"
    );
}

#[test]
fn unzip_on_a_single_gz_refuses_when_the_payload_name_is_taken() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_gz(left.path(), b"new");
    std::fs::write(right.path().join("notes.txt"), b"sentinel").unwrap();
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "notes.txt.gz");

    assert!(app.log.back().unwrap().is_error);
    assert!(
        app.log.back().unwrap().message.contains("already exists"),
        "log: {}",
        app.log.back().unwrap().message
    );
    assert_eq!(
        std::fs::read(right.path().join("notes.txt")).unwrap(),
        b"sentinel"
    );
}

#[test]
fn unzip_on_an_unsupported_extension_logs_a_clear_error() {
    // No 7z/rar/zstd backend exists, so these must be rejected by name
    // rather than failing opaquely inside a zip parser.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("bundle.7z"), b"not really a 7z").unwrap();
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "bundle.7z");

    assert!(app.log.back().unwrap().is_error);
    assert!(
        app.log
            .back()
            .unwrap()
            .message
            .contains("not a supported archive format"),
        "log: {}",
        app.log.back().unwrap().message
    );
}

#[test]
fn unzip_on_a_directory_named_like_an_archive_is_rejected() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::create_dir(left.path().join("looks.zip")).unwrap();
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "looks.zip");

    assert!(app.log.back().unwrap().is_error);
    assert!(
        app.log.back().unwrap().message.contains("not a file"),
        "log: {}",
        app.log.back().unwrap().message
    );
}

#[test]
fn unzip_never_opens_an_overwrite_confirm() {
    // Extraction into a fresh directory can't collide, so the old
    // `PendingOp::UnzipOverwrite` confirm is structurally unreachable —
    // even with a same-named file sitting in the destination.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    make_test_archive(left.path());
    std::fs::write(right.path().join("readme.txt"), b"sentinel").unwrap();
    let mut app = test_app(left.path(), right.path());

    unzip(&mut app, "project.zip");

    assert!(matches!(app.mode, Mode::Normal), "no confirm may open");
    assert_eq!(
        std::fs::read(right.path().join("readme.txt")).unwrap(),
        b"sentinel"
    );
    assert!(right.path().join("project/readme.txt").is_file());
}
