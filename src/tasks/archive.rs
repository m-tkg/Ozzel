//! Background zip create/extract workers. Runs on its own thread (spawned
//! by `TaskManager::spawn`); reports per-file progress, throttled like the
//! copy/move worker.

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::event::TaskEvent;
use crate::tasks::{TaskId, Throttle};

const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------
// Create
// ---------------------------------------------------------------------

enum EntryKind {
    File,
    Dir,
    Symlink,
}

struct EntryPlan {
    path: PathBuf,
    /// Forward-slash-joined path relative to the target's *parent*, so
    /// marking `src/` produces entries `src/...` rather than flattening
    /// everything to the archive root.
    name: String,
    kind: EntryKind,
}

/// Worker entry point for a zip-create task; matches the `TaskManager::spawn`
/// closure signature.
pub fn run_zip(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    targets: Vec<PathBuf>,
    archive_path: PathBuf,
) {
    let result = run_zip_inner(id, &tx, &cancel, &targets, &archive_path);
    let outcome = match result {
        Ok(count) => Ok(format!(
            "zipped {count} file(s) to {}",
            archive_path.display()
        )),
        Err(err) => Err(err.to_string()),
    };
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: outcome,
    });
}

fn run_zip_inner(
    id: TaskId,
    tx: &Sender<TaskEvent>,
    cancel: &Arc<AtomicBool>,
    targets: &[PathBuf],
    archive_path: &Path,
) -> anyhow::Result<usize> {
    let mut entries = Vec::new();
    for target in targets {
        collect_entries(target, &mut entries)?;
    }
    let total = entries
        .iter()
        .filter(|e| !matches!(e.kind, EntryKind::Dir))
        .count() as u64;

    let file = fs::File::create(archive_path)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut done = 0u64;
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut zipped = 0usize;

    for entry in &entries {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }

        match entry.kind {
            EntryKind::Dir => {
                writer.add_directory(entry.name.clone(), options)?;
                continue;
            }
            EntryKind::Symlink => match fs::read_link(&entry.path) {
                Ok(link_target) => {
                    let target_str = link_target.to_string_lossy().into_owned();
                    if let Err(err) = writer.add_symlink(entry.name.clone(), target_str, options) {
                        let _ = tx.send(TaskEvent::Log {
                            id,
                            line: format!(
                                "{}: could not store symlink ({err}), skipped",
                                entry.path.display()
                            ),
                        });
                        continue;
                    }
                    zipped += 1;
                }
                Err(err) => {
                    let _ = tx.send(TaskEvent::Log {
                        id,
                        line: format!("{}: {err}, skipped", entry.path.display()),
                    });
                    continue;
                }
            },
            EntryKind::File => {
                writer.start_file(entry.name.clone(), options)?;
                let mut reader = fs::File::open(&entry.path)?;
                let mut buf = vec![0u8; CHUNK_SIZE];
                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    writer.write_all(&buf[..n])?;
                }
                zipped += 1;
            }
        }

        done += 1;
        if throttle.allow(Instant::now()) {
            let _ = tx.send(TaskEvent::Progress {
                id,
                done,
                total,
                detail: entry.name.clone(),
            });
        }
    }

    writer.finish()?;
    Ok(zipped)
}

/// Walks `target` (a file, dir, or symlink) into a flat plan of archive
/// entries, all named relative to `target`'s parent.
fn collect_entries(target: &Path, out: &mut Vec<EntryPlan>) -> anyhow::Result<()> {
    let meta = fs::symlink_metadata(target)?;
    let base_parent = target.parent().unwrap_or_else(|| Path::new(""));

    if meta.is_symlink() {
        out.push(EntryPlan {
            path: target.to_path_buf(),
            name: entry_name(base_parent, target),
            kind: EntryKind::Symlink,
        });
        return Ok(());
    }

    if !meta.is_dir() {
        out.push(EntryPlan {
            path: target.to_path_buf(),
            name: entry_name(base_parent, target),
            kind: EntryKind::File,
        });
        return Ok(());
    }

    out.push(EntryPlan {
        path: target.to_path_buf(),
        name: entry_name(base_parent, target),
        kind: EntryKind::Dir,
    });
    for walk_entry in WalkDir::new(target).min_depth(1) {
        let walk_entry = walk_entry?;
        let kind = if walk_entry.file_type().is_symlink() {
            EntryKind::Symlink
        } else if walk_entry.file_type().is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        out.push(EntryPlan {
            name: entry_name(base_parent, walk_entry.path()),
            path: walk_entry.path().to_path_buf(),
            kind,
        });
    }
    Ok(())
}

fn entry_name(base_parent: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(base_parent).unwrap_or(path);
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------
// Extract
// ---------------------------------------------------------------------

/// Top-level destination paths that already exist, for a pre-extract
/// Confirm. Coarse by design (only checks each entry's *first* path
/// component, deduplicated) — matching the plan's "coarse check is fine".
pub fn top_level_collisions(archive_path: &Path, dest_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let file = fs::File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;

    let mut seen = HashSet::new();
    let mut collisions = Vec::new();
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let Some(top) = relative.components().next() else {
            continue;
        };
        let top_path = dest_dir.join(top.as_os_str());
        if seen.insert(top_path.clone()) && top_path.exists() {
            collisions.push(top_path);
        }
    }
    Ok(collisions)
}

/// Worker entry point for an unzip task; matches the `TaskManager::spawn`
/// closure signature.
pub fn run_unzip(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    archive_path: PathBuf,
    dest_dir: PathBuf,
) {
    let result = run_unzip_inner(id, &tx, &cancel, &archive_path, &dest_dir);
    let outcome = match result {
        Ok(count) => Ok(format!(
            "extracted {count} file(s) to {}",
            dest_dir.display()
        )),
        Err(err) => Err(err.to_string()),
    };
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: outcome,
    });
}

fn run_unzip_inner(
    id: TaskId,
    tx: &Sender<TaskEvent>,
    cancel: &Arc<AtomicBool>,
    archive_path: &Path,
    dest_dir: &Path,
) -> anyhow::Result<usize> {
    let file = fs::File::open(archive_path)?;
    let mut archive = ZipArchive::new(file)?;
    let total = archive.len() as u64;

    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut extracted = 0usize;

    for i in 0..archive.len() {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }

        let mut entry = archive.by_index(i)?;
        let display_name = entry.name().to_string();

        if !is_valid_utf8_name(entry.name_raw()) {
            let _ = tx.send(TaskEvent::Log {
                id,
                line: format!("{display_name}: non-UTF-8 name, extracted best-effort"),
            });
        }

        // Zip-slip protection: `enclosed_name()` returns `None` for any
        // entry whose path is absolute or normalizes to something outside
        // the archive root (e.g. `../../etc/passwd`).
        let Some(relative) = entry.enclosed_name() else {
            let _ = tx.send(TaskEvent::Log {
                id,
                line: format!("{display_name}: unsafe path in archive, skipped"),
            });
            continue;
        };
        let dest_path = dest_dir.join(&relative);

        if entry.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if entry.is_symlink() {
            // MVP: symlink entries aren't restored as symlinks (a
            // maliciously crafted symlink target could itself point
            // outside `dest_dir`, a second class of zip-slip distinct from
            // the path check above). Logged and skipped rather than
            // silently ignored.
            let _ = tx.send(TaskEvent::Log {
                id,
                line: format!("{display_name}: symlink entries are not extracted, skipped"),
            });
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&dest_path)?;
            let mut buf = vec![0u8; CHUNK_SIZE];
            loop {
                let n = entry.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
            }
            extracted += 1;
        }

        let done = (i + 1) as u64;
        if throttle.allow(Instant::now()) {
            let _ = tx.send(TaskEvent::Progress {
                id,
                done,
                total,
                detail: display_name,
            });
        }
    }

    Ok(extracted)
}

fn is_valid_utf8_name(name_raw: &[u8]) -> bool {
    std::str::from_utf8(name_raw).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn drain(rx: &mpsc::Receiver<TaskEvent>) -> Vec<TaskEvent> {
        rx.try_iter().collect()
    }

    #[test]
    fn is_valid_utf8_name_detects_non_utf8_bytes() {
        assert!(is_valid_utf8_name("plain.txt".as_bytes()));
        assert!(is_valid_utf8_name("日本語.txt".as_bytes()));
        assert!(!is_valid_utf8_name(&[0x82, 0xa0, 0x82, 0xa2])); // Shift-JIS bytes, not valid UTF-8
    }

    #[test]
    fn entry_name_is_relative_to_targets_parent_with_forward_slashes() {
        let base_parent = Path::new("/some/dir");
        let path = Path::new("/some/dir/src/nested/file.txt");
        assert_eq!(entry_name(base_parent, path), "src/nested/file.txt");
    }

    #[test]
    fn zip_then_unzip_round_trips_a_nested_tree_with_japanese_names() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let archive_path = dest_dir.path().join("archive.zip");

        fs::create_dir(src_dir.path().join("project")).unwrap();
        fs::write(src_dir.path().join("project/readme.txt"), b"hello").unwrap();
        fs::create_dir(src_dir.path().join("project/日本語ディレクトリ")).unwrap();
        fs::write(
            src_dir
                .path()
                .join("project/日本語ディレクトリ/日本語ファイル.txt"),
            "こんにちは".as_bytes(),
        )
        .unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_zip(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("project")],
            archive_path.clone(),
        );
        assert!(archive_path.exists());

        let extract_dir = tempfile::tempdir().unwrap();
        let (tx2, rx2) = mpsc::channel();
        let cancel2 = Arc::new(AtomicBool::new(false));
        let id2 = TaskId::next();
        run_unzip(
            id2,
            tx2,
            cancel2,
            archive_path,
            extract_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert!(matches!(
            drain(&rx2).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));

        let restored = extract_dir.path().join("project");
        assert_eq!(fs::read(restored.join("readme.txt")).unwrap(), b"hello");
        assert_eq!(
            fs::read_to_string(restored.join("日本語ディレクトリ/日本語ファイル.txt")).unwrap(),
            "こんにちは"
        );
    }

    #[test]
    fn zip_slip_entries_are_rejected_not_written_outside_dest() {
        // Hand-craft a malicious archive: a well-formed zip whose only
        // entry's name is "../evil.txt", written directly via the low
        // -level writer API (which doesn't itself validate names — only
        // the reader's `enclosed_name()` does).
        let src_dir = tempfile::tempdir().unwrap();
        let archive_path = src_dir.path().join("evil.zip");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            writer.start_file("../evil.txt", options).unwrap();
            writer.write_all(b"pwned").unwrap();
            writer.finish().unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_unzip(id, tx, cancel, archive_path, dest_dir.path().to_path_buf());

        let events = drain(&rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TaskEvent::Log { line, .. } if line.contains("unsafe path"))),
            "must log the rejected entry, not silently drop it"
        );
        // Nothing should have been written inside dest_dir, and definitely
        // nothing above it.
        let dest_entries: Vec<_> = fs::read_dir(dest_dir.path()).unwrap().collect();
        assert!(
            dest_entries.is_empty(),
            "malicious entry must not be written into dest_dir"
        );
        assert!(
            !dest_dir.path().parent().unwrap().join("evil.txt").exists(),
            "malicious entry must not escape dest_dir"
        );
    }

    #[test]
    fn top_level_collisions_reports_existing_names_only() {
        let src_dir = tempfile::tempdir().unwrap();
        let archive_path = src_dir.path().join("a.zip");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            writer.start_file("existing.txt", options).unwrap();
            writer.write_all(b"x").unwrap();
            writer.start_file("fresh.txt", options).unwrap();
            writer.write_all(b"y").unwrap();
            writer.finish().unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(dest_dir.path().join("existing.txt"), b"already here").unwrap();

        let collisions = top_level_collisions(&archive_path, dest_dir.path()).unwrap();
        assert_eq!(collisions, vec![dest_dir.path().join("existing.txt")]);
    }

    #[test]
    fn cancel_flag_set_before_start_aborts_zip_immediately() {
        let src_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("a.txt"), b"hi").unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let archive_path = dest_dir.path().join("out.zip");

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true));
        let id = TaskId::next();
        run_zip(
            id,
            tx,
            cancel,
            vec![src_dir.path().join("a.txt")],
            archive_path.clone(),
        );

        let events = drain(&rx);
        match events.last() {
            Some(TaskEvent::Finished {
                result: Err(msg), ..
            }) => assert_eq!(msg, "cancelled"),
            other => panic!("expected Finished(Err(\"cancelled\")), got {other:?}"),
        }
    }
}
