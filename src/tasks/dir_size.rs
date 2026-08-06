//! The `calc_dir_size` (`z`) worker: recursively sums each target
//! directory's file sizes, reporting one `TaskEvent::DirSize` per finished
//! directory (so results land on screen incrementally) and a grand total in
//! its `Finished` summary. Symlinks are never followed (`WalkDir`'s
//! default), matching how copy/zip already treat links as the link itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use walkdir::WalkDir;

use super::{TaskEvent, TaskId, finish_cancelled, send_log};

pub fn run_dir_size(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    targets: Vec<PathBuf>,
) {
    let total_dirs = targets.len() as u64;
    let mut grand_total: u64 = 0;

    for (i, dir) in targets.iter().enumerate() {
        let mut bytes: u64 = 0;
        for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                finish_cancelled(&tx, id);
                return;
            }
            if entry.file_type().is_file()
                && let Ok(meta) = entry.metadata()
            {
                bytes += meta.len();
            }
        }
        grand_total += bytes;

        send_log(&tx, id, format!("du: {} = {} bytes", dir.display(), bytes));
        let _ = tx.send(TaskEvent::DirSize {
            id,
            path: dir.clone(),
            bytes,
        });
        let _ = tx.send(TaskEvent::Progress {
            id,
            done: (i + 1) as u64,
            total: total_dirs,
            detail: dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
    }

    let _ = tx.send(TaskEvent::Finished {
        id,
        result: Ok(format!(
            "computed {total_dirs} dir(s), total {grand_total} bytes"
        )),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;

    fn collect_events(rx: &mpsc::Receiver<TaskEvent>) -> Vec<TaskEvent> {
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        events
    }

    #[test]
    fn sums_files_recursively_and_reports_per_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("target");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), b"12345").unwrap();
        fs::write(dir.join("sub/b.txt"), b"1234567890").unwrap();

        let (tx, rx) = mpsc::channel();
        run_dir_size(
            TaskId::next(),
            tx,
            Arc::new(AtomicBool::new(false)),
            vec![dir.clone()],
        );

        let events = collect_events(&rx);
        let dir_size = events.iter().find_map(|e| match e {
            TaskEvent::DirSize { path, bytes, .. } => Some((path.clone(), *bytes)),
            _ => None,
        });
        assert_eq!(dir_size, Some((dir, 15)));
        assert!(events.iter().any(|e| matches!(
            e,
            TaskEvent::Finished { result: Ok(msg), .. } if msg.contains("total 15 bytes")
        )));
    }

    #[test]
    fn cancel_reports_cancelled_without_dir_size() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("target");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"xx").unwrap();

        let (tx, rx) = mpsc::channel();
        run_dir_size(
            TaskId::next(),
            tx,
            Arc::new(AtomicBool::new(true)),
            vec![dir],
        );

        let events = collect_events(&rx);
        assert!(events.iter().any(|e| matches!(
            e,
            TaskEvent::Finished { result: Err(msg), .. } if msg == "cancelled"
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TaskEvent::DirSize { .. }))
        );
    }
}
