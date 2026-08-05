//! "Virtual Directory": browsing a `.zip` archive's contents as if it
//! were a real directory, without ever extracting the whole thing to
//! disk first. A `Pane` holding `Some(VirtualDir)` reads its
//! listing from here (see `read_zip_dir_entries`) instead of
//! `entry::read_dir_entries`; everything else about the pane (sort,
//! filter, marks, hidden-toggle) is unmodified generic code that has no
//! idea it's looking at a zip.
//!
//! Deliberately never touches `Pane::cwd`: a virtual directory's "current
//! location" is tracked entirely by `VirtualDir::inner` (the path *inside*
//! the archive), so entering/leaving/navigating within one is invisible to
//! `App::navigate`'s cwd-based history bookkeeping (before == after, so it
//! silently records nothing) — this is what lets both panes independently
//! browse into a zip without polluting `S-left`/`S-right`/the persisted
//! history with archive-internal moves.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::entry::{EntryKind, FsEntry};

/// A pane's position inside an archive: which `.zip` file, and which
/// directory level within it (`inner == ""` is the archive root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDir {
    /// The real, on-disk path of the `.zip` file itself — this is what
    /// every zip read (listing, viewing, extracting) opens.
    pub archive_path: PathBuf,
    /// `archive_path`'s file name, cached at entry time purely so the
    /// pane header and the exit-to-real-dir cursor restore
    /// (`Pane::virtual_go_parent`) don't need to re-derive it.
    pub archive_name: String,
    /// The current directory *inside* the archive, forward-slash joined
    /// component-wise regardless of platform (an archive's internal paths
    /// are a zip convention, not a filesystem one) — `PathBuf::new()`
    /// (empty) means the archive root.
    pub inner: PathBuf,
}

/// Whether `path` names something ozzel should try to browse as a Virtual
/// Directory when `open`ed: a `.zip` extension, case-insensitive. Only
/// consulted for a *file* under the cursor in a real (non-virtual) pane —
/// a `.zip` found *inside* an archive is deliberately not recursed into
/// (see this module's doc comment and `App::begin_open`'s nested-zip
/// handling); it just opens in the plain viewer like any other file.
pub fn is_zip_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// Renders `inner` (an archive-internal path) the way the pane header and
/// log messages show it: forward-slash joined, with a leading `/` so
/// `archive.zip:/a/b` reads unambiguously as "inside the archive" even
/// when `a`/`b` happen to look like a real absolute path fragment.
pub fn inner_display(inner: &Path) -> String {
    let joined = inner
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    format!("/{joined}")
}

/// The pane-header label for a virtual directory: `archive.zip:/inner/path`.
pub fn header_label(vd: &VirtualDir) -> String {
    format!("{}:{}", vd.archive_name, inner_display(&vd.inner))
}

/// Reads `archive_path`'s central directory and synthesizes the immediate
/// children of `inner` as `FsEntry` rows: an entry with only one path
/// component *below* `inner` is a direct child (file or explicit
/// directory entry); anything deeper implies the intermediate directory
/// components between it and `inner`, which get synthesized as `Dir`
/// entries even when the archive never stored an explicit directory entry
/// for them (common for zips built by tools that only record file
/// entries).
///
/// Deliberately reads every entry via `by_index_raw` (metadata only, never
/// decompresses or checks the encryption flag) rather than `by_index`, so
/// *listing* a password-protected archive's structure still works even
/// though opening/extracting any individual encrypted entry will fail
/// later with a clear "password required" error at that point (see
/// `extract_single_to_memory`/`archive::run_extract`) — full support isn't
/// a goal, but there's no reason browsing the *names* should need a
/// password when reading the *bytes* is what actually needs one.
///
/// Zip-slip protection: entries whose `enclosed_name()` is `None` (a path
/// that's absolute or escapes the archive root) are silently excluded
/// from the listing entirely — they're simply never shown, so there's
/// nothing for a later `open`/extract to act on. This is stricter than
/// `tasks::archive::run_unzip`'s "log and skip" policy since a plain
/// directory listing has nowhere to log to.
pub fn read_zip_dir_entries(archive_path: &Path, inner: &Path) -> Result<Vec<FsEntry>> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("not a valid zip archive: {}", archive_path.display()))?;

    struct Child {
        kind: EntryKind,
        size: u64,
        mtime: Option<SystemTime>,
    }
    let mut children: HashMap<String, Child> = HashMap::new();

    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .with_context(|| format!("failed to read archive entry {i}"))?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let relative_to_inner = match relative.strip_prefix(inner) {
            Ok(r) if !r.as_os_str().is_empty() => r,
            _ => continue,
        };
        let mut components = relative_to_inner.components();
        let Some(first) = components.next() else {
            continue;
        };
        let name = first.as_os_str().to_string_lossy().into_owned();
        let is_leaf = components.next().is_none();

        if is_leaf {
            let kind = if entry.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let size = entry.size();
            let mtime = entry.last_modified().and_then(zip_datetime_to_systemtime);
            children.insert(name, Child { kind, size, mtime });
        } else {
            children.entry(name).or_insert(Child {
                kind: EntryKind::Dir,
                size: 0,
                mtime: None,
            });
        }
    }

    Ok(children
        .into_iter()
        .map(|(name, child)| {
            let path = if inner.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                inner.join(&name)
            };
            FsEntry {
                is_hidden: name.starts_with('.'),
                name,
                path,
                kind: child.kind,
                size: child.size,
                mtime: child.mtime,
                unix_mode: None,
                // Virtual entries are always read-only; reusing the
                // Windows-fallback permissions display for this (rather
                // than adding a whole separate "virtual" render path)
                // means the permissions column already shows `ro-`/`rod`
                // for every row in a virtual pane for free.
                readonly: true,
                is_executable: false,
                // Archive entries are only ever synthesized as Dir/File
                // above — a zip has no notion of a symlink entry that this
                // listing would need to resolve.
                symlink_target: None,
            }
        })
        .collect())
}

/// Converts zip's own `DateTime` (MS-DOS date/time, no timezone) to a
/// `SystemTime`, treated as UTC — the archive format doesn't record a
/// timezone at all, so there's no more "correct" interpretation available;
/// this only affects the displayed mtime column, never any real-world
/// comparison. Returns `None` on the (rare, malformed) case the embedded
/// date doesn't correspond to a real calendar date.
fn zip_datetime_to_systemtime(dt: zip::DateTime) -> Option<SystemTime> {
    let date =
        chrono::NaiveDate::from_ymd_opt(dt.year() as i32, dt.month() as u32, dt.day() as u32)?;
    let time = date.and_hms_opt(dt.hour() as u32, dt.minute() as u32, dt.second() as u32)?;
    Some(chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(time, chrono::Utc).into())
}

/// Extracts a single archive entry fully into memory for the viewer
/// (`open`/Enter on a file inside a virtual directory) — capped at
/// `size_cap` bytes, same truncation contract as `viewer::load`'s
/// on-disk path (`(bytes, truncated)`). A genuinely encrypted entry
/// surfaces `by_index`'s own "password required" error here, which is
/// exactly the "log clear error" the plan asks for — there's no special
/// case to write.
pub fn extract_single_to_memory(
    archive_path: &Path,
    inner_path: &Path,
    size_cap: u64,
) -> Result<(Vec<u8>, bool)> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("not a valid zip archive: {}", archive_path.display()))?;

    let mut found = None;
    for i in 0..archive.len() {
        let raw = archive.by_index_raw(i)?;
        if raw.enclosed_name().as_deref() == Some(inner_path) {
            found = Some(i);
            break;
        }
    }
    let Some(index) = found else {
        bail!("not found in archive: {}", inner_path.display());
    };

    let mut entry = archive.by_index(index)?;
    let full_size = entry.size();
    let to_read = full_size.min(size_cap) as usize;
    let mut buf = vec![0u8; to_read];
    entry
        .read_exact(&mut buf)
        .with_context(|| format!("failed to read {}", inner_path.display()))?;
    Ok((buf, full_size > size_cap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn make_archive() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        writer.start_file("readme.txt", options).unwrap();
        writer.write_all(b"hello").unwrap();
        // No explicit directory entry for "src/" — it must be synthesized
        // purely from the deeper file entry below it.
        writer.start_file("src/main.rs", options).unwrap();
        writer.write_all(b"fn main() {}").unwrap();
        writer.start_file("src/nested/deep.txt", options).unwrap();
        writer.write_all(b"deep").unwrap();
        writer.finish().unwrap();

        (dir, archive_path)
    }

    #[test]
    fn is_zip_file_is_case_insensitive_on_extension() {
        assert!(is_zip_file(Path::new("a.zip")));
        assert!(is_zip_file(Path::new("a.ZIP")));
        assert!(is_zip_file(Path::new("a.Zip")));
        assert!(!is_zip_file(Path::new("a.tar.gz")));
        assert!(!is_zip_file(Path::new("a")));
    }

    #[test]
    fn root_listing_has_one_file_and_one_synthesized_dir() {
        let (_dir, archive_path) = make_archive();
        let entries = read_zip_dir_entries(&archive_path, Path::new("")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["readme.txt", "src"]);

        let src = entries.iter().find(|e| e.name == "src").unwrap();
        assert_eq!(src.kind, EntryKind::Dir);
        let readme = entries.iter().find(|e| e.name == "readme.txt").unwrap();
        assert_eq!(readme.kind, EntryKind::File);
        assert_eq!(readme.size, 5);
        assert!(readme.readonly);
    }

    #[test]
    fn nested_listing_synthesizes_intermediate_directories() {
        let (_dir, archive_path) = make_archive();
        let entries = read_zip_dir_entries(&archive_path, Path::new("src")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["main.rs", "nested"]);

        let nested = entries.iter().find(|e| e.name == "nested").unwrap();
        assert_eq!(nested.kind, EntryKind::Dir);

        let deep_entries = read_zip_dir_entries(&archive_path, Path::new("src/nested")).unwrap();
        assert_eq!(deep_entries.len(), 1);
        assert_eq!(deep_entries[0].name, "deep.txt");
        assert_eq!(deep_entries[0].size, 4);
    }

    #[test]
    fn listing_a_directory_with_no_entries_is_empty() {
        let (_dir, archive_path) = make_archive();
        let entries = read_zip_dir_entries(&archive_path, Path::new("nonexistent")).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn extract_single_to_memory_reads_the_full_small_file() {
        let (_dir, archive_path) = make_archive();
        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 10 * 1024 * 1024)
                .unwrap();
        assert_eq!(bytes, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn extract_single_to_memory_truncates_at_the_cap() {
        let (_dir, archive_path) = make_archive();
        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 2).unwrap();
        assert_eq!(bytes, b"he");
        assert!(truncated);
    }

    #[test]
    fn extract_single_to_memory_errors_on_a_missing_entry() {
        let (_dir, archive_path) = make_archive();
        let result =
            extract_single_to_memory(&archive_path, Path::new("nope.txt"), 10 * 1024 * 1024);
        assert!(result.is_err());
    }

    #[test]
    fn zip_slip_entries_are_excluded_from_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("evil.zip");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            writer.start_file("../evil.txt", options).unwrap();
            writer.write_all(b"pwned").unwrap();
            writer.start_file("safe.txt", options).unwrap();
            writer.write_all(b"ok").unwrap();
            writer.finish().unwrap();
        }

        let entries = read_zip_dir_entries(&archive_path, Path::new("")).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["safe.txt"]);
    }

    #[test]
    fn header_label_formats_archive_and_inner_path() {
        let vd = VirtualDir {
            archive_path: PathBuf::from("/tmp/project.zip"),
            archive_name: "project.zip".to_string(),
            inner: PathBuf::from("src/nested"),
        };
        assert_eq!(header_label(&vd), "project.zip:/src/nested");
    }

    #[test]
    fn header_label_at_archive_root_shows_a_bare_slash() {
        let vd = VirtualDir {
            archive_path: PathBuf::from("/tmp/project.zip"),
            archive_name: "project.zip".to_string(),
            inner: PathBuf::new(),
        };
        assert_eq!(header_label(&vd), "project.zip:/");
    }
}
