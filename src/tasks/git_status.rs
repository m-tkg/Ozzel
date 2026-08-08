//! The background `git status` worker: shells out to the `git` CLI (no
//! libgit2 — the project's pure-Rust dependency policy rules out a C
//! toolchain, and `git` on PATH is the pragmatic alternative), parses the
//! porcelain output via `crate::git`, and reports one structured
//! `TaskEvent::GitStatus` per run. A directory that isn't inside a git
//! work tree (or a machine with no `git` at all) reports `status: None` —
//! that's a normal outcome, not an error.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::git::{GitDirStatus, parse_porcelain};

use super::{TaskEvent, TaskId, finish_cancelled};

/// Runs `git` with `args` in `dir`, returning stdout only on a zero exit.
///
/// `--no-optional-locks` matters more than it looks: without it a plain
/// `git status` rewrites `.git/index` to refresh its stat cache, and since
/// `App` now watches the git directory (see `GitDirStatus::git_dir`) that
/// write would come straight back as a change event, re-probing forever.
fn git_output(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn stdout_line(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).trim().to_string()
}

pub fn run_git_status(id: TaskId, tx: Sender<TaskEvent>, cancel: Arc<AtomicBool>, dir: PathBuf) {
    // One process for all three facts, in argument order: repo root
    // (line 1), branch (line 2), git directory (line 3). Any failure —
    // not a repository, git missing — is the "no status" outcome,
    // reported as a success so the caller logs nothing.
    let Some(head) = git_output(
        &dir,
        &[
            "rev-parse",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
            "--absolute-git-dir",
        ],
    ) else {
        let _ = tx.send(TaskEvent::GitStatus {
            id,
            dir,
            status: None,
        });
        let _ = tx.send(TaskEvent::Finished {
            id,
            result: Ok("not a git repository".to_string()),
        });
        return;
    };
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    let repo_root = PathBuf::from(lines.next().unwrap_or_default());
    let mut branch = lines.next().unwrap_or_default().to_string();
    let git_dir = PathBuf::from(lines.next().unwrap_or_default());
    // Detached HEAD reports the literal string "HEAD" — swap in the short
    // hash so the header shows something identifying.
    if branch == "HEAD" {
        branch = git_output(&dir, &["rev-parse", "--short", "HEAD"])
            .map(stdout_line)
            .unwrap_or(branch);
    }

    if cancel.load(Ordering::Relaxed) {
        finish_cancelled(&tx, id);
        return;
    }

    // Pathspec `.` scopes the walk to the pane's own subtree — in a huge
    // repository the pane only ever renders its own directory's children.
    let Some(raw) = git_output(
        &dir,
        &[
            "status",
            "--porcelain",
            "--untracked-files=normal",
            "-z",
            "--",
            ".",
        ],
    ) else {
        let _ = tx.send(TaskEvent::GitStatus {
            id,
            dir: dir.clone(),
            status: None,
        });
        let _ = tx.send(TaskEvent::Finished {
            id,
            result: Err("git status failed".to_string()),
        });
        return;
    };

    if cancel.load(Ordering::Relaxed) {
        finish_cancelled(&tx, id);
        return;
    }

    let statuses = parse_porcelain(&repo_root, &dir, &raw);
    let count = statuses.len();
    let _ = tx.send(TaskEvent::GitStatus {
        id,
        dir,
        status: Some(GitDirStatus {
            git_dir,
            branch,
            statuses,
        }),
    });
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: Ok(format!("{count} changed entr(ies)")),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitMarker;
    use std::sync::mpsc;

    /// The one test here that runs a real `git`, because the thing worth
    /// pinning down can't be faked: `rev-parse` emits its answers in
    /// *argument* order, so the git directory has to be read off line 3.
    /// Get that wrong and `App` watches a path spelled "main" and never
    /// notices a commit. `git` is present wherever this is built (CI
    /// checks the repository out with it), so a failure to run it is a
    /// real failure, not a reason to skip.
    #[test]
    fn a_real_repository_reports_its_git_dir_branch_and_untracked_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .expect("git must be installed")
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q", "--initial-branch=trunk"]);
        // An initial commit is required, not incidental: on an unborn HEAD
        // `rev-parse --abbrev-ref HEAD` fails, which takes the whole
        // combined invocation down and reports "no status" — the same
        // outcome as "not a repository".
        std::fs::write(root.join("committed.txt"), b"x").unwrap();
        run(&["add", "committed.txt"]);
        run(&[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-qm",
            "initial",
        ]);
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let (tx, rx) = mpsc::channel();
        run_git_status(
            TaskId::next(),
            tx,
            Arc::new(AtomicBool::new(false)),
            root.clone(),
        );

        let TaskEvent::GitStatus { status, .. } = rx.recv().unwrap() else {
            panic!("the first event must be the status");
        };
        let status = status.expect("a real repository must report a status");
        // Compared canonicalized, because git and Rust don't spell a
        // Windows path the same way: git answers
        // `C:/Users/…/.git` while `fs::canonicalize` gives
        // `\\?\C:\Users\…\.git`. Harmless for the watch itself —
        // `DirWatcher` canonicalizes both sides of its own comparison, and
        // reports changes back under the path it was handed — but it does
        // mean this assertion can't be a raw path equality.
        assert_eq!(
            std::fs::canonicalize(&status.git_dir).unwrap(),
            std::fs::canonicalize(root.join(".git")).unwrap()
        );
        assert_eq!(status.branch, "trunk");
        assert_eq!(status.statuses[&root.join("a.txt")], GitMarker::Untracked);
    }
}
