//! Background delete worker. Trash-mode is a single `trash::delete_all`
//! call (coarse before/after progress, per the plan — the crate doesn't
//! expose per-file granularity); permanent-mode reports progress per
//! target entry and is cancellable between entries.
//!
//! On macOS specifically, trash-mode uses the `NsFileManager` delete
//! method rather than the crate's default (`Finder`, which shells out to
//! `osascript` and asks the Finder app to do the move). The Finder/
//! AppleScript path requires an Automation permission grant that a
//! terminal app doesn't have by default, and fails outright without it
//! ("The AppleScript exited with error") — `NsFileManager` calls
//! `NSFileManager`'s trash API directly and needs no such permission.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::config::DeleteBehavior;
use crate::event::TaskEvent;
use crate::tasks::TaskId;

/// Worker entry point for a delete task; matches the `TaskManager::spawn`
/// closure signature (plus the `targets`/`behavior` it's bound with via a
/// closure at the call site, since `spawn`'s `F` only takes
/// `(TaskId, Sender<TaskEvent>, Arc<AtomicBool>)`).
pub fn run_delete(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    targets: Vec<PathBuf>,
    behavior: DeleteBehavior,
) {
    match behavior {
        DeleteBehavior::Trash => run_trash(id, &tx, &targets),
        DeleteBehavior::Permanent => run_permanent(id, &tx, &cancel, &targets),
    }
}

fn run_trash(id: TaskId, tx: &Sender<TaskEvent>, targets: &[PathBuf]) {
    let total = targets.len() as u64;
    let _ = tx.send(TaskEvent::Progress {
        id,
        done: 0,
        total,
        detail: "moving to trash".to_string(),
    });

    let result = trash_all(targets);
    let finished = match result {
        Ok(()) => {
            let _ = tx.send(TaskEvent::Progress {
                id,
                done: total,
                total,
                detail: String::new(),
            });
            Ok(format!("moved {} item(s) to trash", targets.len()))
        }
        Err(err) => Err(format!("failed to move to trash: {err}")),
    };
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: finished,
    });
}

/// Moves `targets` to the OS trash. On macOS this explicitly selects the
/// `NsFileManager` delete method (see the module doc comment above) instead
/// of the crate's `Finder`/AppleScript default, which fails in a plain
/// terminal without an Automation permission grant. Every other platform
/// uses the crate's own default (`freedesktop.org` trash spec on Linux,
/// the Windows shell API on Windows), which needs no such workaround.
fn trash_all(targets: &[PathBuf]) -> Result<(), trash::Error> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete_all(targets)
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::delete_all(targets)
    }
}

fn run_permanent(
    id: TaskId,
    tx: &Sender<TaskEvent>,
    cancel: &Arc<AtomicBool>,
    targets: &[PathBuf],
) {
    let total = targets.len() as u64;
    let mut failures = 0usize;

    for (idx, target) in targets.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(TaskEvent::Finished {
                id,
                result: Err("cancelled".to_string()),
            });
            return;
        }

        let detail = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(err) = delete_one_permanently(target) {
            let _ = tx.send(TaskEvent::Log {
                id,
                line: format!("{}: {err}", target.display()),
            });
            failures += 1;
        }
        let done = idx as u64 + 1;
        let _ = tx.send(TaskEvent::Progress {
            id,
            done,
            total,
            detail,
        });
    }

    let succeeded = targets.len() - failures;
    let result = if failures == 0 {
        Ok(format!("deleted {succeeded} item(s)"))
    } else {
        Err(format!(
            "deleted {succeeded}/{}, {failures} failed (see log)",
            targets.len()
        ))
    };
    let _ = tx.send(TaskEvent::Finished { id, result });
}

fn delete_one_permanently(path: &Path) -> anyhow::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        // Covers plain files and symlinks (remove_file unlinks the link
        // itself rather than following it).
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskId as TId;
    use std::sync::mpsc;

    fn drain(rx: &mpsc::Receiver<TaskEvent>) -> Vec<TaskEvent> {
        rx.try_iter().collect()
    }

    #[test]
    fn permanent_delete_removes_files_and_dirs_with_monotonic_progress() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), b"hi").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/child.txt"), b"hi").unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TId::next();
        let targets = vec![dir.path().join("file.txt"), dir.path().join("sub")];

        run_delete(id, tx, cancel, targets, DeleteBehavior::Permanent);

        let events = drain(&rx);
        let mut last_done = 0u64;
        for event in &events {
            if let TaskEvent::Progress { done, .. } = event {
                assert!(*done >= last_done);
                last_done = *done;
            }
        }
        assert_eq!(last_done, 2);
        match events.last() {
            Some(TaskEvent::Finished { result: Ok(_), .. }) => {}
            other => panic!("expected Finished(Ok), got {other:?}"),
        }
        assert!(!dir.path().join("file.txt").exists());
        assert!(!dir.path().join("sub").exists());
    }

    #[test]
    fn permanent_delete_cancel_flag_set_before_start_aborts_immediately() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), b"hi").unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true));
        let id = TId::next();
        run_delete(
            id,
            tx,
            cancel,
            vec![dir.path().join("file.txt")],
            DeleteBehavior::Permanent,
        );

        let events = drain(&rx);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TaskEvent::Finished {
                result: Err(msg), ..
            } => assert_eq!(msg, "cancelled"),
            other => panic!("expected Finished(Err(\"cancelled\")), got {other:?}"),
        }
        assert!(
            dir.path().join("file.txt").exists(),
            "cancelled before touching anything"
        );
    }

    #[test]
    fn permanent_delete_partial_failure_is_logged_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.txt"), b"hi").unwrap();
        let missing = dir.path().join("does-not-exist.txt");

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TId::next();
        run_delete(
            id,
            tx,
            cancel,
            vec![dir.path().join("real.txt"), missing],
            DeleteBehavior::Permanent,
        );

        let events = drain(&rx);
        assert!(events.iter().any(|e| matches!(e, TaskEvent::Log { .. })));
        match events.last() {
            Some(TaskEvent::Finished { result: Err(_), .. }) => {}
            other => panic!("expected Finished(Err(_)), got {other:?}"),
        }
    }
}
