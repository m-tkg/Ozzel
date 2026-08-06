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
fn git_output(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
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
    // One process for both facts: repo root (line 1) and branch (line 2).
    // Any failure — not a repository, git missing — is the "no status"
    // outcome, reported as a success so the caller logs nothing.
    let Some(head) = git_output(
        &dir,
        &["rev-parse", "--show-toplevel", "--abbrev-ref", "HEAD"],
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
        status: Some(GitDirStatus { branch, statuses }),
    });
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: Ok(format!("{count} changed entr(ies)")),
    });
}
