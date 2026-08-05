//! Background zip create/extract workers. Runs on its own thread (spawned
//! by `TaskManager::spawn`); reports per-file progress, throttled like the
//! copy/move worker. `run_extract` (Virtual Directory marks/`C`
//! extraction) also handles the tar family via `virtual_dir`'s streaming
//! primitives — zip creation (`run_zip`) and the separate whole-archive
//! `u`/`Unzip` action (`run_unzip`/`top_level_collisions`) stay zip-only,
//! unchanged; extending those to the tar family isn't in scope here.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Instant;

use anyhow::Context as _;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::event::TaskEvent;
use crate::tasks::{CHUNK_SIZE, PROGRESS_MIN_INTERVAL, TaskId, Throttle, send_log};
use crate::virtual_dir::{self, ArchiveKind};

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
                        send_log(
                            tx,
                            id,
                            format!(
                                "{}: could not store symlink ({err}), skipped",
                                entry.path.display()
                            ),
                        );
                        continue;
                    }
                    zipped += 1;
                }
                Err(err) => {
                    send_log(tx, id, format!("{}: {err}, skipped", entry.path.display()));
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
            send_log(
                tx,
                id,
                format!("{display_name}: non-UTF-8 name, extracted best-effort"),
            );
        }

        // Zip-slip protection: `enclosed_name()` returns `None` for any
        // entry whose path is absolute or normalizes to something outside
        // the archive root (e.g. `../../etc/passwd`).
        let Some(relative) = entry.enclosed_name() else {
            send_log(
                tx,
                id,
                format!("{display_name}: unsafe path in archive, skipped"),
            );
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
            send_log(
                tx,
                id,
                format!("{display_name}: symlink entries are not extracted, skipped"),
            );
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

// ---------------------------------------------------------------------
// Extract (Virtual Directory partial extraction — `C` inside a `.zip`)
// ---------------------------------------------------------------------

/// Top-level destination-name collisions for a Virtual Directory
/// extraction, for a pre-extract Confirm — pure and archive-I/O-free
/// (unlike `top_level_collisions`, which has to open the archive to learn
/// each entry's own top component): `inner_targets` already *are* each
/// target's own top-level name (its `file_name()`), since extraction
/// always lands each target directly under `dest_dir` by that name.
pub fn extract_collisions(inner_targets: &[PathBuf], dest_dir: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut collisions = Vec::new();
    for target in inner_targets {
        let Some(name) = target.file_name() else {
            continue;
        };
        let dest = dest_dir.join(name);
        if seen.insert(dest.clone()) && dest.exists() {
            collisions.push(dest);
        }
    }
    collisions
}

/// Worker entry point for a Virtual Directory extraction; matches the
/// `TaskManager::spawn` closure signature. `inner_targets` are
/// archive-internal paths (from the virtual pane's marks-or-cursor) —
/// each one lands under `dest_dir` by its own `file_name()`, a file
/// extracted directly or a directory extracted as the whole subtree
/// under it.
pub fn run_extract(
    id: TaskId,
    tx: Sender<TaskEvent>,
    cancel: Arc<AtomicBool>,
    archive_path: PathBuf,
    inner_targets: Vec<PathBuf>,
    dest_dir: PathBuf,
) {
    let ctx = ExtractCtx {
        id,
        tx: &tx,
        cancel: &cancel,
        archive_path: &archive_path,
        inner_targets: &inner_targets,
        dest_dir: &dest_dir,
    };
    let result = match virtual_dir::detect_archive_kind(&archive_path) {
        Some(ArchiveKind::Tar(compression)) => run_tar_extract_inner(&ctx, compression),
        // `Some(ArchiveKind::Zip)` and, defensively, `None` (can't
        // actually happen — Virtual Directory mode is only ever entered
        // via `App::begin_open`'s `virtual_dir::is_archive_file` check,
        // which already validated the extension) both fall through to the
        // original zip-only path; a `None` archive_path still fails
        // there, cleanly, via `ZipArchive::new`'s own error.
        _ => run_extract_inner(&ctx),
    };
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

/// Parameters shared by the zip and tar-family "Virtual Directory extract"
/// inner workers (`run_extract_inner`/`run_tar_extract_inner`) — every one
/// of `run_extract`'s own arguments except the tar-only `compression`,
/// bundled so neither inner function has to re-thread six-plus separate
/// parameters (which, for `run_tar_extract_inner` plus its own
/// `compression`, previously tripped clippy's `too_many_arguments`).
struct ExtractCtx<'a> {
    id: TaskId,
    tx: &'a Sender<TaskEvent>,
    cancel: &'a Arc<AtomicBool>,
    archive_path: &'a Path,
    inner_targets: &'a [PathBuf],
    dest_dir: &'a Path,
}

/// One planned extraction step: which archive entry (`index`) lands at
/// which real `dest` path, and whether it's a directory (just needs
/// creating) or a file (needs its content copied out).
struct ExtractPlan {
    index: usize,
    dest: PathBuf,
    is_dir: bool,
}

fn run_extract_inner(ctx: &ExtractCtx) -> anyhow::Result<usize> {
    let file = fs::File::open(ctx.archive_path)?;
    let mut archive = ZipArchive::new(file)?;

    // Every safely-named entry's (enclosed_name, index), read once via the
    // metadata-only `by_index_raw` (see `virtual_dir::read_zip_dir_entries`'s
    // doc comment on why: this must work even for a password-protected
    // archive, since matching *names* against `inner_targets` needs no
    // decryption — only the later per-file `by_index` read does, and that's
    // exactly where a "password required" error should surface).
    let mut safe_entries: Vec<(PathBuf, usize)> = Vec::new();
    for i in 0..archive.len() {
        let raw = archive.by_index_raw(i)?;
        if let Some(name) = raw.enclosed_name() {
            safe_entries.push((name, i));
        }
    }

    let mut plan: Vec<ExtractPlan> = Vec::new();
    for target in ctx.inner_targets {
        let dest_name = target
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| target.clone());
        let mut matched_any = false;

        for (name, index) in &safe_entries {
            let dest = if name == target {
                ctx.dest_dir.join(&dest_name)
            } else if let Ok(rel) = name.strip_prefix(target) {
                if rel.as_os_str().is_empty() {
                    continue; // same entry as the `name == target` case above
                }
                ctx.dest_dir.join(&dest_name).join(rel)
            } else {
                continue;
            };
            matched_any = true;
            let is_dir = archive.by_index_raw(*index)?.is_dir();
            plan.push(ExtractPlan {
                index: *index,
                dest,
                is_dir,
            });
        }

        if !matched_any {
            send_log(
                ctx.tx,
                ctx.id,
                format!("{}: not found in archive, skipped", target.display()),
            );
        }
    }

    let total = plan.iter().filter(|p| !p.is_dir).count() as u64;
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut done = 0u64;
    let mut extracted = 0usize;

    for p in &plan {
        if ctx.cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }

        if p.is_dir {
            fs::create_dir_all(&p.dest)?;
            continue;
        }
        if let Some(parent) = p.dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut entry = archive.by_index(p.index)?;
        let mut out = fs::File::create(&p.dest)?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
        }
        extracted += 1;

        done += 1;
        if throttle.allow(Instant::now()) {
            let _ = ctx.tx.send(TaskEvent::Progress {
                id: ctx.id,
                done,
                total,
                detail: p.dest.display().to_string(),
            });
        }
    }

    Ok(extracted)
}

/// The tar-family counterpart of `run_extract_inner`. tar has no central
/// directory to plan against ahead of time the way zip's `by_index` does
/// (see `virtual_dir`'s module doc comment), so this is a single
/// streaming pass: for every entry (skipping any `enclosed_tar_path`
/// rejects — the tar-slip equivalent of zip's `enclosed_name` check),
/// checks it against every `inner_targets` entry the same "exact match or
/// under this subtree" way `run_extract_inner` does, and — since a
/// `tar::Entry` is a one-shot `Read` that can't be re-opened by index the
/// way zip's `by_index(i)` can — buffers a *file* entry's bytes once if
/// it matched more than one target, so extracting to multiple
/// destinations (an unusual but possible case: marking both a directory
/// and a file already inside it) still works correctly rather than
/// silently dropping every destination after the first.
///
/// Progress is reported with `total: 0` (renders as a static 0% — see
/// `ui::log_view::render_gauge`) rather than a real fraction: unlike
/// zip's upfront `archive.len()`, learning the true total ahead of time
/// here would mean a full second streaming pass — for a compressed tar,
/// a second full decompression — purely to count entries, which isn't
/// worth doubling the actual extraction's cost just for a progress
/// percentage. `detail` (the destination path) still updates live, so the
/// task is visibly progressing even without a percentage.
fn run_tar_extract_inner(
    ctx: &ExtractCtx,
    compression: virtual_dir::TarCompression,
) -> anyhow::Result<usize> {
    let mut archive = virtual_dir::open_tar_archive(ctx.archive_path, compression)?;
    let mut throttle = Throttle::new(PROGRESS_MIN_INTERVAL);
    let mut extracted = 0usize;
    let mut matched_targets: HashSet<usize> = HashSet::new();

    for entry in archive
        .entries()
        .with_context(|| format!("failed to read archive: {}", ctx.archive_path.display()))?
    {
        if ctx.cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let mut entry = entry.with_context(|| {
            format!("failed to read an entry in {}", ctx.archive_path.display())
        })?;
        let entry_type = entry.header().entry_type();
        let raw_path = entry
            .path()
            .with_context(|| {
                format!(
                    "failed to read an entry's name in {}",
                    ctx.archive_path.display()
                )
            })?
            .into_owned();
        let Some(path) = virtual_dir::enclosed_tar_path(&raw_path) else {
            send_log(
                ctx.tx,
                ctx.id,
                format!("{}: unsafe path in archive, skipped", raw_path.display()),
            );
            continue;
        };

        // Every requested target this entry falls under — usually zero or
        // one, but see the doc comment above for why it can legitimately
        // be more than one.
        let mut dests: Vec<PathBuf> = Vec::new();
        for (ti, target) in ctx.inner_targets.iter().enumerate() {
            let dest_name = target
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| target.clone());
            let matched = if path == *target {
                Some(ctx.dest_dir.join(&dest_name))
            } else {
                match path.strip_prefix(target) {
                    Ok(rel) if !rel.as_os_str().is_empty() => {
                        Some(ctx.dest_dir.join(&dest_name).join(rel))
                    }
                    _ => None,
                }
            };
            if let Some(dest) = matched {
                matched_targets.insert(ti);
                dests.push(dest);
            }
        }
        if dests.is_empty() {
            continue;
        }

        if entry_type.is_dir() {
            for dest in &dests {
                fs::create_dir_all(dest)?;
            }
            continue;
        }
        if entry_type.is_symlink() {
            send_log(
                ctx.tx,
                ctx.id,
                format!(
                    "{}: symlink entries are not extracted, skipped",
                    path.display()
                ),
            );
            continue;
        }
        if !entry_type.is_file() {
            send_log(
                ctx.tx,
                ctx.id,
                format!("{}: unsupported entry type, skipped", path.display()),
            );
            continue;
        }

        // Read once regardless of how many destinations it lands at — a
        // `tar::Entry` can't be re-read.
        let mut buf = Vec::with_capacity(entry.size() as usize);
        io::copy(&mut entry, &mut buf)
            .with_context(|| format!("failed to read {}", path.display()))?;
        for dest in &dests {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(dest, &buf).with_context(|| format!("failed to write {}", dest.display()))?;
        }
        extracted += 1;

        if throttle.allow(Instant::now()) {
            let _ = ctx.tx.send(TaskEvent::Progress {
                id: ctx.id,
                done: extracted as u64,
                total: 0,
                detail: dests[0].display().to_string(),
            });
        }
    }

    for (ti, target) in ctx.inner_targets.iter().enumerate() {
        if !matched_targets.contains(&ti) {
            send_log(
                ctx.tx,
                ctx.id,
                format!("{}: not found in archive, skipped", target.display()),
            );
        }
    }

    Ok(extracted)
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

    fn make_virtual_test_archive() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        writer.start_file("readme.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        writer.start_file("src/main.rs", options).unwrap();
        writer.write_all(b"fn main() {}").unwrap();
        writer.start_file("src/nested/deep.txt", options).unwrap();
        writer.write_all(b"deep").unwrap();
        writer.finish().unwrap();
        (dir, archive_path)
    }

    #[test]
    fn extract_collisions_reports_existing_names_only() {
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(dest_dir.path().join("readme.txt"), b"already here").unwrap();

        let targets = vec![PathBuf::from("readme.txt"), PathBuf::from("src")];
        let collisions = extract_collisions(&targets, dest_dir.path());
        assert_eq!(collisions, vec![dest_dir.path().join("readme.txt")]);
    }

    #[test]
    fn run_extract_extracts_a_single_file() {
        let (_dir, archive_path) = make_virtual_test_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("readme.txt")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("readme.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn run_extract_extracts_a_whole_subtree_under_its_own_name() {
        let (_dir, archive_path) = make_virtual_test_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("src")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("src/main.rs")).unwrap(),
            b"fn main() {}"
        );
        assert_eq!(
            fs::read(dest_dir.path().join("src/nested/deep.txt")).unwrap(),
            b"deep"
        );
    }

    #[test]
    fn run_extract_logs_and_skips_a_target_missing_from_the_archive() {
        let (_dir, archive_path) = make_virtual_test_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("nope.txt")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        assert!(events.iter().any(
            |e| matches!(e, TaskEvent::Log { line, .. } if line.contains("not found in archive"))
        ));
    }

    #[test]
    fn run_extract_rejects_zip_slip_entries_within_the_extracted_subtree() {
        // A malicious archive whose only entry under the target directory
        // is a zip-slip path — `enclosed_name()` filters it out of
        // `safe_entries` entirely, so it must simply never be extracted.
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("evil.zip");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            writer.start_file("safe/../../evil.txt", options).unwrap();
            writer.write_all(b"pwned").unwrap();
            writer.finish().unwrap();
        }

        let dest_dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("safe")],
            dest_dir.path().to_path_buf(),
        );
        let _ = drain(&rx);

        assert!(
            !dest_dir.path().parent().unwrap().join("evil.txt").exists(),
            "zip-slip entry must never be written outside dest_dir"
        );
        let dest_entries: Vec<_> = fs::read_dir(dest_dir.path()).unwrap().collect();
        assert!(dest_entries.is_empty());
    }

    // --- tar family: `run_extract` (marks/`C`) -----------------------------

    fn tar_bytes(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn make_virtual_test_tar_archive() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.tar");
        let bytes = tar_bytes(&[
            ("readme.txt", b"hello"),
            ("src/main.rs", b"fn main() {}"),
            ("src/nested/deep.txt", b"deep"),
        ]);
        fs::write(&archive_path, bytes).unwrap();
        (dir, archive_path)
    }

    #[test]
    fn run_extract_extracts_a_single_file_from_a_tar_archive() {
        let (_dir, archive_path) = make_virtual_test_tar_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("readme.txt")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("readme.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn run_extract_extracts_a_whole_subtree_from_a_tar_archive() {
        let (_dir, archive_path) = make_virtual_test_tar_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("src")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("src/main.rs")).unwrap(),
            b"fn main() {}"
        );
        assert_eq!(
            fs::read(dest_dir.path().join("src/nested/deep.txt")).unwrap(),
            b"deep"
        );
    }

    #[test]
    fn run_extract_logs_and_skips_a_target_missing_from_a_tar_archive() {
        let (_dir, archive_path) = make_virtual_test_tar_archive();
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("nope.txt")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        assert!(events.iter().any(
            |e| matches!(e, TaskEvent::Log { line, .. } if line.contains("not found in archive"))
        ));
    }

    #[test]
    fn run_extract_from_a_compressed_tar_gz_archive_round_trips() {
        // End-to-end through `run_extract`'s own `detect_archive_kind`
        // dispatch (not calling `run_tar_extract_inner` directly) — proves
        // the `.tar.gz` extension actually routes to the tar-family path
        // and that decompression works inside the real extraction flow.
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.tar.gz");
        let tar = tar_bytes(&[("readme.txt", b"hello")]);
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap();
        }
        let dest_dir = tempfile::tempdir().unwrap();

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("readme.txt")],
            dest_dir.path().to_path_buf(),
        );

        assert!(matches!(
            drain(&rx).last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        assert_eq!(
            fs::read(dest_dir.path().join("readme.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn run_extract_rejects_tar_slip_entries_within_the_extracted_subtree() {
        // Same low-level-header technique as
        // `virtual_dir::tests::tar_slip_entries_are_excluded_from_the_listing`
        // — the safe writer API itself refuses to write a `..` path.
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("evil.tar");
        let tar = {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            let data = b"pwned";
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            let name = b"safe/../../evil.txt";
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder.append(&header, &data[..]).unwrap();
            builder.into_inner().unwrap()
        };
        fs::write(&archive_path, &tar).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("safe")],
            dest_dir.path().to_path_buf(),
        );
        let _ = drain(&rx);

        assert!(
            !dest_dir.path().parent().unwrap().join("evil.txt").exists(),
            "tar-slip entry must never be written outside dest_dir"
        );
        let dest_entries: Vec<_> = fs::read_dir(dest_dir.path()).unwrap().collect();
        assert!(dest_entries.is_empty());
    }

    #[test]
    fn run_extract_logs_and_skips_symlink_entries_in_a_tar_archive() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.tar");
        let tar = {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder
                .append_link(&mut header, "link.txt", "target.txt")
                .unwrap();
            builder.into_inner().unwrap()
        };
        fs::write(&archive_path, &tar).unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let id = TaskId::next();
        run_extract(
            id,
            tx,
            cancel,
            archive_path,
            vec![PathBuf::from("link.txt")],
            dest_dir.path().to_path_buf(),
        );

        let events = drain(&rx);
        assert!(
            events.iter().any(
                |e| matches!(e, TaskEvent::Log { line, .. } if line.contains("symlink entries are not extracted"))
            ),
            "events: {events:?}"
        );
        assert!(!dest_dir.path().join("link.txt").exists());
    }
}
