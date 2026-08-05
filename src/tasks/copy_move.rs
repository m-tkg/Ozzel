//! Background copy/move worker. Runs on its own thread (spawned by
//! `TaskManager::spawn`); reports progress in bytes, throttled to at most
//! one `Progress` event per `PROGRESS_MIN_INTERVAL`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use walkdir::WalkDir;

use crate::event::TaskEvent;
use crate::tasks::{TaskId, Throttle};

const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Files at or below this size skip the manual read/write loop entirely in
/// favor of a single `std::fs::copy` call — cheap enough that per-chunk
/// progress reporting wouldn't be visible anyway (the whole transfer
/// finishes well inside one `PROGRESS_MIN_INTERVAL` tick), so there's
/// nothing lost by handing it to the OS's own accelerated path instead
/// (clonefile on macOS/APFS, copy_file_range on Linux) rather than paying
/// for a userspace buffer round-trip on every single file.
const WHOLE_FILE_COPY_THRESHOLD: u64 = 4 * 1024 * 1024; // 4 MiB

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferMode {
    Copy,
    Move,
}

/// One entry failed but the task keeps going (logged, not fatal) versus
/// the whole task being cancelled (stops immediately).
enum OpOutcome {
    Cancelled,
    Failed(String),
}

/// Everything a single copy/move step needs, bundled so the recursive
/// copy/move helpers below don't have to thread six-plus separate
/// arguments through every call.
struct TransferCtx<'a> {
    tx: &'a Sender<TaskEvent>,
    id: TaskId,
    cancel: &'a Arc<AtomicBool>,
    done_bytes: u64,
    total_bytes: u64,
    throttle: &'a mut Throttle,
    /// The chunked-copy read buffer, allocated once per task and reused for
    /// every file over `WHOLE_FILE_COPY_THRESHOLD` — a bulk copy/move of
    /// many large files previously re-zeroed a fresh 1 MiB `Vec` per file;
    /// this way the allocation happens once regardless of file count.
    buf: Vec<u8>,
}

impl TransferCtx<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn send_progress(&mut self, detail: &str) {
        let _ = self.tx.send(TaskEvent::Progress {
            id: self.id,
            done: self.done_bytes,
            total: self.total_bytes,
            detail: detail.to_string(),
        });
    }

    /// Sends a throttled progress update after `n` more bytes landed.
    fn advance(&mut self, n: u64, detail: &str) {
        self.done_bytes += n;
        if self.throttle.allow(Instant::now()) {
            self.send_progress(detail);
        }
    }
}

/// Which destination paths a transfer would overwrite. Run on the main
/// thread *before* spawning the task, so the app can put up a Confirm
/// first — cheap, since it's just `Path::exists` per source.
pub fn find_collisions(sources: &[PathBuf], dest_dir: &Path) -> Vec<PathBuf> {
    sources
        .iter()
        .filter_map(|src| src.file_name().map(|name| dest_dir.join(name)))
        .filter(|dest| dest.exists())
        .collect()
}

/// Worker entry point for a copy task; matches the `TaskManager::spawn`
/// closure signature.
pub fn run_copy(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    sources: Vec<PathBuf>,
    dest_dir: PathBuf,
) {
    run_transfer(id, tx, cancel, sources, dest_dir, TransferMode::Copy);
}

/// Worker entry point for a move task; matches the `TaskManager::spawn`
/// closure signature.
pub fn run_move(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    sources: Vec<PathBuf>,
    dest_dir: PathBuf,
) {
    run_transfer(id, tx, cancel, sources, dest_dir, TransferMode::Move);
}

/// Worker entry point for `duplicate` (`c`): copies `source` to an exact
/// `dest` path (unlike `run_copy`/`run_move`, which copy *into* a
/// directory, keeping the source's own name) — reuses `copy_one` directly,
/// since it already takes an exact destination. Single-item, so there's no
/// per-source loop or failure-count summary to build.
pub fn run_duplicate(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    source: PathBuf,
    dest: PathBuf,
) {
    let total = path_size(&source);
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut ctx = TransferCtx {
        tx: &tx,
        id,
        cancel: &cancel,
        done_bytes: 0,
        total_bytes: total,
        throttle: &mut throttle,
        buf: vec![0u8; CHUNK_SIZE],
    };

    let result = match copy_one(&mut ctx, &source, &dest) {
        Ok(()) => Ok(format!("duplicated to {}", dest.display())),
        Err(OpOutcome::Cancelled) => {
            finish_cancelled(&tx, id);
            return;
        }
        Err(OpOutcome::Failed(msg)) => Err(format!("{}: {msg}", source.display())),
    };
    let _ = tx.send(TaskEvent::Finished { id, result });
}

fn run_transfer(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    sources: Vec<PathBuf>,
    dest_dir: PathBuf,
    mode: TransferMode,
) {
    let sizes: Vec<u64> = sources.iter().map(|s| path_size(s)).collect();
    let total: u64 = sizes.iter().sum();
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut ctx = TransferCtx {
        tx: &tx,
        id,
        cancel: &cancel,
        done_bytes: 0,
        total_bytes: total,
        throttle: &mut throttle,
        buf: vec![0u8; CHUNK_SIZE],
    };
    let mut failures = 0usize;

    for (src, size) in sources.iter().zip(sizes.iter()) {
        if ctx.is_cancelled() {
            finish_cancelled(&tx, id);
            return;
        }

        let Some(name) = src.file_name() else {
            send_log(
                &tx,
                id,
                format!("{}: has no file name, skipped", src.display()),
            );
            failures += 1;
            continue;
        };
        let dest = dest_dir.join(name);

        let outcome = match mode {
            TransferMode::Copy => copy_one(&mut ctx, src, &dest),
            TransferMode::Move => move_one(&mut ctx, src, &dest, *size),
        };

        match outcome {
            Ok(()) => {}
            Err(OpOutcome::Cancelled) => {
                finish_cancelled(&tx, id);
                return;
            }
            Err(OpOutcome::Failed(msg)) => {
                send_log(&tx, id, format!("{}: {msg}", src.display()));
                failures += 1;
            }
        }
    }

    let verb = match mode {
        TransferMode::Copy => "copied",
        TransferMode::Move => "moved",
    };
    let succeeded = sources.len() - failures;
    let result = if failures == 0 {
        Ok(format!(
            "{succeeded} item(s) {verb} to {}",
            dest_dir.display()
        ))
    } else {
        Err(format!(
            "{succeeded}/{} {verb}, {failures} failed (see log)",
            sources.len()
        ))
    };
    let _ = tx.send(TaskEvent::Finished { id, result });
}

/// Copies `src` (file, dir, or symlink) to `dest`, recursing into
/// directories with `walkdir` and reporting byte progress for regular
/// files. Symlinks are recreated as links, never followed.
fn copy_one(ctx: &mut TransferCtx, src: &Path, dest: &Path) -> Result<(), OpOutcome> {
    let meta = fs::symlink_metadata(src).map_err(|e| OpOutcome::Failed(e.to_string()))?;

    if meta.is_symlink() {
        return copy_symlink(src, dest).map_err(|e| OpOutcome::Failed(e.to_string()));
    }

    if !meta.is_dir() {
        return copy_file_chunked(ctx, src, dest);
    }

    fs::create_dir_all(dest).map_err(|e| OpOutcome::Failed(e.to_string()))?;
    for entry in WalkDir::new(src).min_depth(1).into_iter() {
        if ctx.is_cancelled() {
            return Err(OpOutcome::Cancelled);
        }
        let entry = entry.map_err(|e| OpOutcome::Failed(e.to_string()))?;
        let relative = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir entries are always under the root they were walked from");
        let target = dest.join(relative);

        if entry.file_type().is_symlink() {
            copy_symlink(entry.path(), &target).map_err(|e| OpOutcome::Failed(e.to_string()))?;
        } else if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| OpOutcome::Failed(e.to_string()))?;
        } else {
            copy_file_chunked(ctx, entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copies one regular file, dispatching on size: at or below
/// `WHOLE_FILE_COPY_THRESHOLD` a single `std::fs::copy` handles the whole
/// thing (OS-accelerated, permissions included, cancellation only checked
/// before it starts); above it, the manual chunked read/write loop keeps
/// per-chunk cancellation checks and progress reporting, then explicitly
/// propagates `src`'s permissions afterward so both paths leave `dest` with
/// the same metadata regardless of which one ran.
fn copy_file_chunked(ctx: &mut TransferCtx, src: &Path, dest: &Path) -> Result<(), OpOutcome> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| OpOutcome::Failed(e.to_string()))?;
    }
    if ctx.is_cancelled() {
        return Err(OpOutcome::Cancelled);
    }
    let size = fs::metadata(src)
        .map_err(|e| OpOutcome::Failed(e.to_string()))?
        .len();
    let detail = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    if size <= WHOLE_FILE_COPY_THRESHOLD {
        fs::copy(src, dest).map_err(|e| OpOutcome::Failed(e.to_string()))?;
        ctx.done_bytes += size;
        ctx.send_progress(&detail); // whole-file jump; always worth reporting, not throttled
        return Ok(());
    }

    let mut reader = fs::File::open(src).map_err(|e| OpOutcome::Failed(e.to_string()))?;
    let mut writer = fs::File::create(dest).map_err(|e| OpOutcome::Failed(e.to_string()))?;

    loop {
        if ctx.is_cancelled() {
            return Err(OpOutcome::Cancelled);
        }
        let n = reader
            .read(&mut ctx.buf)
            .map_err(|e| OpOutcome::Failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&ctx.buf[..n])
            .map_err(|e| OpOutcome::Failed(e.to_string()))?;
        ctx.advance(n as u64, &detail);
    }
    drop(writer);
    let src_permissions = fs::metadata(src)
        .map_err(|e| OpOutcome::Failed(e.to_string()))?
        .permissions();
    fs::set_permissions(dest, src_permissions).map_err(|e| OpOutcome::Failed(e.to_string()))?;
    Ok(())
}

/// Moves `src` to `dest`: a plain `fs::rename` first (atomic, and the
/// common case on the same filesystem/drive), falling back to
/// [`move_via_copy_then_delete`] when that fails — most commonly `EXDEV`
/// across filesystems or drives, though any rename failure takes this path.
/// Per the plan's known-risk note, that fallback is not atomic.
fn move_one(ctx: &mut TransferCtx, src: &Path, dest: &Path, size: u64) -> Result<(), OpOutcome> {
    if fs::rename(src, dest).is_ok() {
        let detail = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        ctx.done_bytes += size;
        ctx.send_progress(&detail); // whole-source jump; always worth reporting, not throttled
        return Ok(());
    }

    move_via_copy_then_delete(ctx, src, dest)
}

/// The EXDEV fallback: copy the whole tree, then remove the source. Kept
/// as its own function (rather than inlined in `move_one`) so it can be
/// exercised directly in tests without needing an actual cross-device
/// rename failure to trigger it.
fn move_via_copy_then_delete(
    ctx: &mut TransferCtx,
    src: &Path,
    dest: &Path,
) -> Result<(), OpOutcome> {
    copy_one(ctx, src, dest)?;

    let meta = fs::symlink_metadata(src).map_err(|e| {
        OpOutcome::Failed(format!(
            "copied to {} but failed to stat source: {e}",
            dest.display()
        ))
    })?;
    let remove_result = if meta.is_dir() {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    };
    remove_result.map_err(|e| {
        OpOutcome::Failed(format!(
            "copied to {} but failed to remove source: {e}",
            dest.display()
        ))
    })
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let target = fs::read_link(src)?;
    std::os::unix::fs::symlink(&target, dest)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let target = fs::read_link(src)?;
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(&target, dest)?;
    } else {
        std::os::windows::fs::symlink_file(&target, dest)?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(src: &Path, dest: &Path) -> anyhow::Result<()> {
    anyhow::bail!(
        "symlinks are not supported on this platform: {} -> {}",
        src.display(),
        dest.display()
    )
}

fn path_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_symlink() || meta.is_file() {
        return meta.len();
    }
    WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn send_log(tx: &Sender<TaskEvent>, id: TaskId, line: String) {
    let _ = tx.send(TaskEvent::Log { id, line });
}

fn finish_cancelled(tx: &Sender<TaskEvent>, id: TaskId) {
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: Err("cancelled".to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn drain(rx: &mpsc::Receiver<TaskEvent>) -> Vec<TaskEvent> {
        rx.try_iter().collect()
    }

    #[test]
    fn find_collisions_reports_only_existing_destinations() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("a.txt"), b"1").unwrap();
        fs::write(src_dir.path().join("b.txt"), b"1").unwrap();
        fs::write(dest_dir.path().join("a.txt"), b"existing").unwrap();

        let sources = vec![src_dir.path().join("a.txt"), src_dir.path().join("b.txt")];
        let collisions = find_collisions(&sources, dest_dir.path());
        assert_eq!(collisions, vec![dest_dir.path().join("a.txt")]);
    }

    #[test]
    fn run_copy_reports_monotonic_progress_and_finishes_ok() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        // Bigger than one chunk boundary isn't required for correctness,
        // but a few KiB gives more than a single Progress-worthy write.
        fs::write(src_dir.path().join("a.txt"), vec![b'x'; 4096]).unwrap();
        fs::write(src_dir.path().join("b.txt"), vec![b'y'; 4096]).unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        let sources = vec![src_dir.path().join("a.txt"), src_dir.path().join("b.txt")];

        run_copy(id, tx, cancel, sources, dest_dir.path().to_path_buf());

        let events = drain(&rx);
        let mut last_done = 0u64;
        for event in &events {
            if let TaskEvent::Progress { done, .. } = event {
                assert!(*done >= last_done, "progress must never go backwards");
                last_done = *done;
            }
        }
        match events.last() {
            Some(TaskEvent::Finished { result: Ok(_), .. }) => {}
            other => panic!("expected Finished(Ok), got {other:?}"),
        }
        assert!(dest_dir.path().join("a.txt").exists());
        assert!(dest_dir.path().join("b.txt").exists());
        // Copy leaves the source alone.
        assert!(src_dir.path().join("a.txt").exists());
    }

    #[test]
    fn run_move_relocates_and_removes_source() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("a.txt"), b"hello").unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_move(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("a.txt")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        match events.last() {
            Some(TaskEvent::Finished { result: Ok(_), .. }) => {}
            other => panic!("expected Finished(Ok), got {other:?}"),
        }
        assert!(!src_dir.path().join("a.txt").exists());
        assert_eq!(fs::read(dest_dir.path().join("a.txt")).unwrap(), b"hello");
    }

    #[test]
    fn cancel_flag_set_before_start_aborts_immediately() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("a.txt"), vec![b'x'; 4096]).unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled
        let id = TaskId::next();
        run_copy(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("a.txt")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        assert_eq!(events.len(), 1, "should abort before doing any work");
        match &events[0] {
            TaskEvent::Finished {
                result: Err(msg), ..
            } => assert_eq!(msg, "cancelled"),
            other => panic!("expected Finished(Err(\"cancelled\")), got {other:?}"),
        }
        assert!(!dest_dir.path().join("a.txt").exists());
    }

    #[test]
    fn partial_failure_is_reported_as_finished_err_with_detail() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("a.txt"), b"ok").unwrap();
        let missing = src_dir.path().join("does-not-exist.txt");

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_copy(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("a.txt"), missing],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        let has_log = events.iter().any(|e| matches!(e, TaskEvent::Log { .. }));
        assert!(
            has_log,
            "the failing source must produce a Log line, never silently"
        );
        match events.last() {
            Some(TaskEvent::Finished { result: Err(_), .. }) => {}
            other => panic!("expected Finished(Err(_)), got {other:?}"),
        }
        assert!(
            dest_dir.path().join("a.txt").exists(),
            "the successful item still lands"
        );
    }

    #[test]
    fn move_via_copy_then_delete_simulates_the_exdev_fallback() {
        // Calling this directly (rather than forcing a real cross-device
        // rename failure) is the "simulate EXDEV" path: it's exactly what
        // move_one falls back to when fs::rename fails for any reason.
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("a.txt");
        let dest = dest_dir.path().join("a.txt");
        fs::write(&src, b"hello").unwrap();

        let (tx, _rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        let mut throttle = Throttle::new(Duration::from_millis(100));
        let mut ctx = TransferCtx {
            tx: &tx,
            id,
            cancel: &cancel,
            done_bytes: 0,
            total_bytes: 5,
            throttle: &mut throttle,
            buf: vec![0u8; CHUNK_SIZE],
        };

        let result = move_via_copy_then_delete(&mut ctx, &src, &dest);

        assert!(result.is_ok());
        assert!(
            !src.exists(),
            "fallback must remove the source after copying"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
    }

    #[test]
    fn run_copy_recurses_into_directories_preserving_structure() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::create_dir(src_dir.path().join("nested")).unwrap();
        fs::write(src_dir.path().join("nested/child.txt"), b"world").unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_copy(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("nested")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("nested/child.txt")).unwrap(),
            b"world"
        );
    }

    #[test]
    fn run_copy_of_a_large_file_goes_through_the_chunked_path_and_finishes_ok() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        // Bigger than WHOLE_FILE_COPY_THRESHOLD so this exercises the
        // chunked read/write loop (reusing `ctx.buf`) rather than the
        // single `std::fs::copy` call the small-file path takes.
        let big = vec![b'z'; (WHOLE_FILE_COPY_THRESHOLD + 1) as usize];
        fs::write(src_dir.path().join("big.bin"), &big).unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_copy(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("big.bin")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        assert!(matches!(
            events.last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TaskEvent::Progress { .. })),
            "the chunked path must still report per-chunk progress"
        );
        assert_eq!(fs::read(dest_dir.path().join("big.bin")).unwrap(), big);
    }

    #[cfg(unix)]
    #[test]
    fn copy_preserves_source_permissions_on_both_the_small_and_chunked_paths() {
        use std::os::unix::fs::PermissionsExt;

        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();

        let small = src_dir.path().join("small.sh");
        fs::write(&small, b"#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&small, fs::Permissions::from_mode(0o755)).unwrap();

        let large = src_dir.path().join("large.bin");
        fs::write(&large, vec![b'a'; (WHOLE_FILE_COPY_THRESHOLD + 1) as usize]).unwrap();
        fs::set_permissions(&large, fs::Permissions::from_mode(0o640)).unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_copy(
            id,
            tx,
            cancel,
            vec![small, large],
            dest_dir.path().to_path_buf(),
        );
        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));

        let small_mode = fs::metadata(dest_dir.path().join("small.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let large_mode = fs::metadata(dest_dir.path().join("large.bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(small_mode, 0o755);
        assert_eq!(large_mode, 0o640);
    }
}
