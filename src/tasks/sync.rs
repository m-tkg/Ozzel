//! The directory-sync (`Y`) worker: makes the destination pane's
//! directory match the source pane's, in one of two modes chosen in the
//! sync dialog — *update* (copy new/missing files only, never delete)
//! or *mirror* (update **plus** delete whatever exists only in the
//! destination). Three phases: scan (decide what to copy, remember every
//! source-relative path), copy (through `copy_move`'s chunked/progress
//! machinery), and — mirror only — delete the leftovers, respecting the
//! configured `DeleteBehavior` (trash by default). Symlinks are never
//! followed; they compare by link target and are recreated as links.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use walkdir::WalkDir;

use super::TaskEvent;
use crate::config::DeleteBehavior;
use crate::tasks::copy_move::{OpOutcome, TransferCtx, copy_one, copy_symlink};
use crate::tasks::delete::{delete_one_permanently, trash_all};
use crate::tasks::{
    CHUNK_SIZE, PROGRESS_MIN_INTERVAL, TaskId, Throttle, finish_cancelled, send_log,
};

/// A destination file this much *newer* than its source is still left
/// alone by the size-equal/mtime comparison — FAT-family filesystems
/// round mtimes to 2-second granularity, so exact comparison would
/// endlessly re-copy identical files across such mounts.
const MTIME_TOLERANCE: Duration = Duration::from_secs(1);

/// One planned copy: exact source and destination plus the byte size
/// that was already counted into the progress total.
struct PlannedCopy {
    src: PathBuf,
    dest: PathBuf,
}

pub fn run_sync(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    src_dir: PathBuf,
    dest_dir: PathBuf,
    mirror: bool,
    delete_behavior: DeleteBehavior,
) {
    // ---- Phase 1: scan ---------------------------------------------------
    let _ = tx.send(TaskEvent::Progress {
        id,
        done: 0,
        total: 0,
        detail: "scanning".to_string(),
    });

    let mut keep: HashSet<PathBuf> = HashSet::new();
    let mut planned: Vec<PlannedCopy> = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut scan_failures = 0usize;
    let mut skipped_mismatch = 0usize;

    for entry in WalkDir::new(&src_dir).min_depth(1) {
        if cancel.load(Ordering::Relaxed) {
            finish_cancelled(&tx, id);
            return;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                send_log(&tx, id, format!("scan: {err}"));
                scan_failures += 1;
                continue;
            }
        };
        let rel = entry
            .path()
            .strip_prefix(&src_dir)
            .expect("walkdir entries are always under the walked root")
            .to_path_buf();
        keep.insert(rel.clone());
        let dest = dest_dir.join(&rel);

        match plan_one(entry.path(), &dest, mirror) {
            Plan::Skip => {}
            Plan::SkipMismatch => {
                send_log(
                    &tx,
                    id,
                    format!(
                        "{}: source and destination kinds differ — skipped (mirror replaces these)",
                        rel.display()
                    ),
                );
                skipped_mismatch += 1;
            }
            Plan::Copy { bytes } => {
                total_bytes += bytes;
                planned.push(PlannedCopy {
                    src: entry.path().to_path_buf(),
                    dest,
                });
            }
            Plan::Error(msg) => {
                send_log(&tx, id, format!("{}: {msg}", rel.display()));
                scan_failures += 1;
            }
        }
    }

    // ---- Phase 2: copy ---------------------------------------------------
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut ctx = TransferCtx {
        tx: &tx,
        id,
        cancel: &cancel,
        done_bytes: 0,
        total_bytes,
        throttle: &mut throttle,
        buf: vec![0u8; CHUNK_SIZE],
    };
    let mut copied = 0usize;
    let mut copy_failures = 0usize;

    for plan in &planned {
        if ctx.is_cancelled() {
            finish_cancelled(&tx, id);
            return;
        }
        // A stale destination of a *different* kind (mirror mode let it
        // through) has to go first — `copy_one` would otherwise fail or,
        // worse, copy into a directory that should have become a file.
        if let Ok(meta) = fs::symlink_metadata(&plan.dest)
            && fs::symlink_metadata(&plan.src)
                .map(|m| kind_of(&m) != kind_of(&meta))
                .unwrap_or(false)
        {
            let removed = if meta.is_dir() {
                fs::remove_dir_all(&plan.dest)
            } else {
                fs::remove_file(&plan.dest)
            };
            if let Err(err) = removed {
                send_log(&tx, id, format!("{}: {err}", plan.dest.display()));
                copy_failures += 1;
                continue;
            }
        }
        match sync_copy_one(&mut ctx, &plan.src, &plan.dest) {
            Ok(()) => copied += 1,
            Err(OpOutcome::Cancelled) => {
                finish_cancelled(&tx, id);
                return;
            }
            Err(OpOutcome::Failed(msg)) => {
                send_log(&tx, id, format!("{}: {msg}", plan.src.display()));
                copy_failures += 1;
            }
        }
    }
    let copied_bytes = ctx.done_bytes;

    // ---- Phase 3: mirror delete -------------------------------------------
    let mut deleted = 0usize;
    let mut delete_failures = 0usize;
    if mirror {
        // Deepest-first so a doomed directory's contents go before the
        // directory itself; anything under an already-deleted directory
        // just fails its stat and is skipped silently.
        for entry in WalkDir::new(&dest_dir)
            .min_depth(1)
            .contents_first(true)
            .into_iter()
            .filter_map(Result::ok)
        {
            if cancel.load(Ordering::Relaxed) {
                finish_cancelled(&tx, id);
                return;
            }
            let rel = entry
                .path()
                .strip_prefix(&dest_dir)
                .expect("walkdir entries are always under the walked root");
            if keep.contains(rel) {
                continue;
            }
            if fs::symlink_metadata(entry.path()).is_err() {
                continue; // vanished with an ancestor already deleted
            }
            let result = match delete_behavior {
                DeleteBehavior::Trash => {
                    trash_all(&[entry.path().to_path_buf()]).map_err(anyhow::Error::from)
                }
                DeleteBehavior::Permanent => delete_one_permanently(entry.path()),
            };
            match result {
                Ok(()) => {
                    send_log(&tx, id, format!("sync delete: {}", entry.path().display()));
                    deleted += 1;
                }
                Err(err) => {
                    send_log(&tx, id, format!("{}: {err}", entry.path().display()));
                    delete_failures += 1;
                }
            }
        }
    }

    // ---- Summary ----------------------------------------------------------
    let failures = scan_failures + copy_failures + delete_failures + skipped_mismatch;
    let mut summary = format!("synced: {copied} copied ({copied_bytes} bytes)");
    if mirror {
        summary.push_str(&format!(", {deleted} deleted"));
    }
    let result = if failures == 0 {
        Ok(summary)
    } else {
        summary.push_str(&format!(", {failures} failed/skipped (see log)"));
        Err(summary)
    };
    let _ = tx.send(TaskEvent::Finished { id, result });
}

/// What the scan decided for one source entry.
enum Plan {
    /// Destination is already up to date (or an update-mode dir that
    /// exists) — nothing to do.
    Skip,
    /// Update mode found a source/destination kind mismatch — left alone.
    SkipMismatch,
    /// Copy it; `bytes` feeds the progress total (0 for dirs/symlinks).
    Copy {
        bytes: u64,
    },
    Error(String),
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Kind {
    File,
    Dir,
    Symlink,
}

fn kind_of(meta: &fs::Metadata) -> Kind {
    if meta.is_symlink() {
        Kind::Symlink
    } else if meta.is_dir() {
        Kind::Dir
    } else {
        Kind::File
    }
}

/// The sync comparison for one source entry against its would-be
/// destination path.
fn plan_one(src: &Path, dest: &Path, mirror: bool) -> Plan {
    let src_meta = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(err) => return Plan::Error(err.to_string()),
    };
    let dest_meta = fs::symlink_metadata(dest).ok();

    let src_kind = kind_of(&src_meta);
    match dest_meta {
        // Nothing there yet: dirs and symlinks are structural (0 bytes of
        // progress), files count their size.
        None => Plan::Copy {
            bytes: if src_kind == Kind::File {
                src_meta.len()
            } else {
                0
            },
        },
        Some(dest_meta) => {
            let dest_kind = kind_of(&dest_meta);
            if src_kind != dest_kind {
                return if mirror {
                    Plan::Copy {
                        bytes: if src_kind == Kind::File {
                            src_meta.len()
                        } else {
                            0
                        },
                    }
                } else {
                    Plan::SkipMismatch
                };
            }
            match src_kind {
                Kind::Dir => Plan::Skip,
                Kind::Symlink => {
                    // Compare link targets; a differing link is recreated.
                    let same = fs::read_link(src)
                        .ok()
                        .zip(fs::read_link(dest).ok())
                        .is_some_and(|(a, b)| a == b);
                    if same {
                        Plan::Skip
                    } else {
                        Plan::Copy { bytes: 0 }
                    }
                }
                Kind::File => {
                    if src_meta.len() != dest_meta.len() || src_newer(&src_meta, &dest_meta) {
                        Plan::Copy {
                            bytes: src_meta.len(),
                        }
                    } else {
                        Plan::Skip
                    }
                }
            }
        }
    }
}

/// Whether the source's mtime beats the destination's by more than the
/// FAT-granularity tolerance. Unreadable mtimes count as "not newer" —
/// with equal sizes there's nothing else to compare, so leave it alone.
fn src_newer(src: &fs::Metadata, dest: &fs::Metadata) -> bool {
    match (src.modified(), dest.modified()) {
        (Ok(s), Ok(d)) => s > d + MTIME_TOLERANCE,
        _ => false,
    }
}

/// Copies one planned entry: directories are just created (their contents
/// each have their own plan entry), symlinks are re-linked (replacing a
/// same-kind, different-target one), files go through `copy_one`'s
/// chunked/progress path.
fn sync_copy_one(ctx: &mut TransferCtx, src: &Path, dest: &Path) -> Result<(), OpOutcome> {
    let meta = fs::symlink_metadata(src).map_err(|e| OpOutcome::Failed(e.to_string()))?;
    match kind_of(&meta) {
        Kind::Dir => fs::create_dir_all(dest).map_err(|e| OpOutcome::Failed(e.to_string())),
        Kind::Symlink => {
            if fs::symlink_metadata(dest).is_ok() {
                fs::remove_file(dest).map_err(|e| OpOutcome::Failed(e.to_string()))?;
            }
            copy_symlink(src, dest).map_err(|e| OpOutcome::Failed(e.to_string()))
        }
        Kind::File => copy_one(ctx, src, dest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn run(
        src: &Path,
        dest: &Path,
        mirror: bool,
        behavior: DeleteBehavior,
    ) -> (Vec<TaskEvent>, Result<String, String>) {
        let (tx, rx) = mpsc::channel();
        run_sync(
            TaskId::next(),
            tx,
            Arc::new(AtomicBool::new(false)),
            src.to_path_buf(),
            dest.to_path_buf(),
            mirror,
            behavior,
        );
        let events: Vec<TaskEvent> = rx.try_iter().collect();
        let result = match events.last() {
            Some(TaskEvent::Finished { result, .. }) => result.clone(),
            other => panic!("expected a Finished event last, got {other:?}"),
        };
        (events, result)
    }

    fn set_mtime(path: &Path, secs_since_epoch: u64) {
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs_since_epoch);
        crate::ops::set_times(path, t).unwrap();
    }

    #[test]
    fn update_copies_missing_files_and_nested_structure() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("sub/inner")).unwrap();
        fs::write(src.path().join("a.txt"), b"aa").unwrap();
        fs::write(src.path().join("sub/inner/b.txt"), b"bbb").unwrap();

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fs::read(dest.path().join("a.txt")).unwrap(), b"aa");
        assert_eq!(
            fs::read(dest.path().join("sub/inner/b.txt")).unwrap(),
            b"bbb"
        );
    }

    #[test]
    fn update_overwrites_older_dest_but_leaves_newer_dest_alone() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        // Older destination, different content, same-length: mtime decides.
        fs::write(src.path().join("newer.txt"), b"NEW").unwrap();
        fs::write(dest.path().join("newer.txt"), b"old").unwrap();
        set_mtime(&src.path().join("newer.txt"), 2_000_000);
        set_mtime(&dest.path().join("newer.txt"), 1_000_000);
        // Newer destination must be left untouched.
        fs::write(src.path().join("older.txt"), b"SRC").unwrap();
        fs::write(dest.path().join("older.txt"), b"DST").unwrap();
        set_mtime(&src.path().join("older.txt"), 1_000_000);
        set_mtime(&dest.path().join("older.txt"), 2_000_000);

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(fs::read(dest.path().join("newer.txt")).unwrap(), b"NEW");
        assert_eq!(
            fs::read(dest.path().join("older.txt")).unwrap(),
            b"DST",
            "a newer destination must never be overwritten"
        );
    }

    #[test]
    fn mtime_within_tolerance_does_not_recopy() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("f.txt"), b"abc").unwrap();
        fs::write(dest.path().join("f.txt"), b"xyz").unwrap(); // same length
        // Source 1s newer — exactly at the tolerance boundary, not over it.
        set_mtime(&src.path().join("f.txt"), 1_000_001);
        set_mtime(&dest.path().join("f.txt"), 1_000_000);

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            fs::read(dest.path().join("f.txt")).unwrap(),
            b"xyz",
            "a within-tolerance mtime difference must not trigger a copy"
        );
    }

    #[test]
    fn update_never_deletes_extra_destination_files() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), b"a").unwrap();
        fs::write(dest.path().join("extra.txt"), b"keep me").unwrap();

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert!(dest.path().join("extra.txt").exists());
    }

    #[test]
    fn mirror_deletes_extra_files_and_directories() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("keep.txt"), b"k").unwrap();
        fs::write(dest.path().join("extra.txt"), b"x").unwrap();
        fs::create_dir_all(dest.path().join("extra-dir/nested")).unwrap();
        fs::write(dest.path().join("extra-dir/nested/f.txt"), b"x").unwrap();

        let (_, result) = run(src.path(), dest.path(), true, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert!(dest.path().join("keep.txt").exists());
        assert!(!dest.path().join("extra.txt").exists());
        assert!(!dest.path().join("extra-dir").exists());
        let summary = result.unwrap();
        assert!(summary.contains("deleted"), "summary: {summary}");
    }

    #[test]
    fn mirror_keeps_same_named_directories_and_their_synced_contents() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("shared")).unwrap();
        fs::write(src.path().join("shared/from-src.txt"), b"s").unwrap();
        fs::create_dir(dest.path().join("shared")).unwrap();
        fs::write(dest.path().join("shared/only-dest.txt"), b"d").unwrap();

        let (_, result) = run(src.path(), dest.path(), true, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert!(dest.path().join("shared/from-src.txt").exists());
        assert!(
            !dest.path().join("shared/only-dest.txt").exists(),
            "mirror removes dest-only files inside shared directories too"
        );
    }

    #[test]
    fn update_skips_kind_mismatch_and_reports_it() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("thing"), b"file").unwrap();
        fs::create_dir(dest.path().join("thing")).unwrap();

        let (events, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_err(), "mismatch must surface in the summary");
        assert!(dest.path().join("thing").is_dir(), "update never replaces");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TaskEvent::Log { line, .. } if line.contains("kinds differ")))
        );
    }

    #[test]
    fn mirror_replaces_kind_mismatch() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("thing"), b"file").unwrap();
        fs::create_dir(dest.path().join("thing")).unwrap();
        fs::write(dest.path().join("thing/leftover.txt"), b"x").unwrap();

        let (_, result) = run(src.path(), dest.path(), true, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert!(dest.path().join("thing").is_file());
        assert_eq!(fs::read(dest.path().join("thing")).unwrap(), b"file");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_synced_as_links_and_retargeted_when_different() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/target/one", src.path().join("link")).unwrap();
        std::os::unix::fs::symlink("/target/other", dest.path().join("link")).unwrap();

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            fs::read_link(dest.path().join("link")).unwrap(),
            Path::new("/target/one")
        );
    }

    #[test]
    fn cancel_before_start_reports_cancelled_and_copies_nothing() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), b"a").unwrap();

        let (tx, rx) = mpsc::channel();
        run_sync(
            TaskId::next(),
            tx,
            Arc::new(AtomicBool::new(true)),
            src.path().to_path_buf(),
            dest.path().to_path_buf(),
            false,
            DeleteBehavior::Permanent,
        );

        let events: Vec<TaskEvent> = rx.try_iter().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            TaskEvent::Finished { result: Err(msg), .. } if msg == "cancelled"
        )));
        assert!(!dest.path().join("a.txt").exists());
    }

    #[test]
    fn summary_counts_copies_and_bytes() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), b"12345").unwrap();

        let (_, result) = run(src.path(), dest.path(), false, DeleteBehavior::Permanent);
        let summary = result.unwrap();
        assert!(summary.contains("1 copied (5 bytes)"), "summary: {summary}");
    }
}
