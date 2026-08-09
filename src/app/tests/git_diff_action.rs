//! Tests for the `S-g` (git diff) action. Unlike `git_status.rs`, which
//! injects fake events, these run a real `git` in a tempdir — the whole
//! point of the action is what the `git` CLI answers, and `git` is present
//! wherever this is built (CI checks the repository out with it), so a
//! failure to run it is a real failure, not a reason to skip.
//!
//! The pane's `git` field is stamped by hand rather than by waiting on a
//! background probe: `begin_git_diff` reads the marker off that field, and
//! `dispatch` (unlike `handle_event`) never runs the probe sweep, so a
//! hand-built `GitDirStatus` is both sufficient and deterministic.

use super::super::test_support::*;
use super::super::*;

use crate::git::{GitDirStatus, GitMarker};

fn log_contains(app: &App, needle: &str) -> bool {
    app.log.iter().any(|l| l.message.contains(needle))
}

/// Runs `git` in `root`, asserting a zero exit. Identity is passed per
/// invocation so the test never depends on (or touches) a real user's
/// git config.
fn git(root: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=test"])
        .args(args)
        .output()
        .expect("git must be installed")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A repository with one committed file per `(name, body)` pair.
fn init_repo(root: &Path, files: &[(&str, &str)]) {
    git(root, &["init", "-q", "--initial-branch=main"]);
    for (name, body) in files {
        std::fs::write(root.join(name), body).unwrap();
        git(root, &["add", "--", name]);
    }
    git(root, &["commit", "-qm", "initial"]);
}

/// Points a fresh `App` at `root` with the cursor on `name` and a git
/// status claiming `marker` for it — the state a finished probe would have
/// left behind.
fn app_on(root: &Path, name: &str, marker: GitMarker) -> App {
    let mut app = test_app(root, root);
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, name);
    let cwd = app.active_pane().cwd.clone();
    let mut statuses = HashMap::new();
    statuses.insert(cwd.join(name), marker);
    app.active_pane_mut().set_git_status(Some(GitDirStatus {
        git_dir: cwd.join(".git"),
        branch: "main".to_string(),
        statuses,
    }));
    app
}

fn viewer_lines(app: &App) -> Vec<String> {
    match &app.mode {
        Mode::Viewer { syntax, lines, .. } => {
            assert_eq!(*syntax, ViewerSyntax::Diff);
            lines.clone()
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

#[test]
fn git_diff_of_a_modified_file_opens_the_viewer_with_diff_syntax() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("f.txt", "one\ntwo\nthree\n")]);
    std::fs::write(dir.path().join("f.txt"), "one\nTWO\nthree\n").unwrap();
    let mut app = app_on(dir.path(), "f.txt", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    let lines = viewer_lines(&app);
    assert!(lines.iter().any(|l| l.starts_with("-two")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("+TWO")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("@@")), "{lines:?}");
    match &app.mode {
        Mode::Viewer {
            path, view_mode, ..
        } => {
            assert!(path.display().to_string().starts_with("git diff: "));
            assert_eq!(*view_mode, ViewMode::Text);
        }
        other => panic!("expected Mode::Viewer, got {other:?}"),
    }
}

/// The reason the command is `git diff HEAD` and not a plain `git diff`:
/// a staged change is part of what raised the marker, so hiding it would
/// show an empty diff for a file the pane says is modified.
#[test]
fn git_diff_covers_the_staged_side_too() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("f.txt", "one\n")]);
    std::fs::write(dir.path().join("f.txt"), "one\nstaged\n").unwrap();
    git(dir.path(), &["add", "--", "f.txt"]);
    std::fs::write(dir.path().join("f.txt"), "one\nstaged\nworktree\n").unwrap();
    let mut app = app_on(dir.path(), "f.txt", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    let lines = viewer_lines(&app);
    assert!(lines.iter().any(|l| l.starts_with("+staged")), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("+worktree")),
        "{lines:?}"
    );
}

/// A directory row's marker aggregates everything below it, so its diff
/// has to cover the whole subtree rather than resolving to nothing.
#[test]
fn git_diff_on_a_directory_row_diffs_its_whole_subtree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    init_repo(dir.path(), &[("sub/inner.txt", "before\n")]);
    std::fs::write(dir.path().join("sub/inner.txt"), "after\n").unwrap();
    let mut app = app_on(dir.path(), "sub", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    let lines = viewer_lines(&app);
    assert!(lines.iter().any(|l| l.starts_with("-before")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("+after")), "{lines:?}");
}

/// `:(literal)` earns its place here: without it the `[1]` would be read
/// as a one-character glob class, match nothing, and produce an empty
/// diff for a file that plainly changed.
#[test]
fn git_diff_handles_a_glob_metacharacter_in_the_file_name() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("a[1].txt", "before\n")]);
    std::fs::write(dir.path().join("a[1].txt"), "after\n").unwrap();
    let mut app = app_on(dir.path(), "a[1].txt", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    let lines = viewer_lines(&app);
    assert!(lines.iter().any(|l| l.starts_with("-before")), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("+after")), "{lines:?}");
}

/// No commits yet means no `HEAD` to diff against — the fallback branch
/// in `git_cmd::diff` has to answer with the index-vs-worktree diff
/// instead of reporting a failed command.
#[test]
fn git_diff_in_a_repository_with_no_commits_falls_back_to_the_index() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--initial-branch=main"]);
    std::fs::write(dir.path().join("f.txt"), "staged\n").unwrap();
    git(dir.path(), &["add", "--", "f.txt"]);
    std::fs::write(dir.path().join("f.txt"), "staged\nworktree\n").unwrap();
    let mut app = app_on(dir.path(), "f.txt", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    let lines = viewer_lines(&app);
    assert!(
        lines.iter().any(|l| l.starts_with("+worktree")),
        "{lines:?}"
    );
}

#[test]
fn git_diff_on_an_unmarked_entry_logs_and_stays_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("f.txt", "one\n"), ("clean.txt", "x\n")]);
    std::fs::write(dir.path().join("f.txt"), "changed\n").unwrap();
    // The status only knows about `f.txt`; the cursor sits on the clean one.
    let mut app = app_on(dir.path(), "f.txt", GitMarker::Modified);
    select_entry_named(&mut app, "clean.txt");

    app.dispatch(Action::GitDiff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "clean.txt: no git changes"));
}

#[test]
fn git_diff_on_an_untracked_entry_logs_and_stays_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("f.txt", "one\n")]);
    std::fs::write(dir.path().join("new.txt"), "brand new\n").unwrap();
    let mut app = app_on(dir.path(), "new.txt", GitMarker::Untracked);

    app.dispatch(Action::GitDiff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "untracked"));
}

#[test]
fn git_diff_without_a_git_status_logs_and_stays_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "f.txt");

    app.dispatch(Action::GitDiff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "no git status for this directory"));
}

/// A marker with nothing for `git diff` to print logs rather than opening
/// an empty viewer. Reachable in the real app with a stale marker — the
/// edit was undone (or committed elsewhere) since the last probe — which
/// is exactly what this sets up.
#[test]
fn git_diff_with_empty_output_logs_and_stays_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), &[("f.txt", "same\n")]);
    let mut app = app_on(dir.path(), "f.txt", GitMarker::Modified);

    app.dispatch(Action::GitDiff);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(log_contains(&app, "git diff is empty"));
}
