//! "Virtual Directory": browsing an archive's contents as if it were a
//! real directory, without ever extracting the whole thing to disk first.
//! A `Pane` holding `Some(VirtualDir)` reads its listing from here (see
//! `read_archive_dir_entries`) instead of `entry::read_dir_entries`;
//! everything else about the pane (sort, filter, marks, hidden-toggle) is
//! unmodified generic code that has no idea what archive format it's
//! looking at.
//!
//! Two backends, dispatched on by [`ArchiveKind`] (detected purely from
//! the filename, see [`detect_archive_kind`]): zip (via the `zip` crate's
//! random-access central-directory reads) and the tar family — plain
//! `.tar`, plus gzip/bzip2/xz-compressed tar, all read by streaming
//! `tar::Archive` once per call (tar has no central directory to seek
//! into the way zip does — see [`TarCompression`] and
//! [`open_tar_archive`]). `tasks::archive`'s marks-extraction
//! (`run_extract`) uses [`open_tar_archive`]/[`enclosed_tar_path`]
//! directly for its own streaming extraction pass; the zip family's
//! equivalent stays entirely inside `tasks::archive` via the `zip` crate,
//! unchanged.
//!
//! Deliberately never touches `Pane::cwd`: a virtual directory's "current
//! location" is tracked entirely by `VirtualDir::inner` (the path *inside*
//! the archive), so entering/leaving/navigating within one is invisible to
//! `App::navigate`'s cwd-based history bookkeeping (before == after, so it
//! silently records nothing) — this is what lets both panes independently
//! browse into an archive without polluting `S-left`/`S-right`/the
//! persisted history with archive-internal moves.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
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

/// Which archive backend a Virtual Directory reads through — see this
/// module's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar(TarCompression),
}

/// How a tar-family archive's byte stream is compressed, if at all —
/// determines which decoder [`open_tar_archive`] wraps the raw file
/// reader in before handing it to `tar::Archive`.
///
/// Format support and why: `Gzip` (`flate2`) and `Bzip2`
/// (`bzip2` crate, whose *default* feature set builds against the
/// pure-Rust `libbz2-rs-sys` backend rather than linking the real C
/// `libbzip2` — no C toolchain needed, same bar as every other dependency
/// here) both decode via a `Read` adapter, so the tar stream is decoded
/// lazily as `tar::Archive` reads from it. `Xz` (`lzma-rs`, `#![forbid(
/// unsafe_code)]`, pure Rust) only exposes a whole-buffer
/// `BufRead -> Write` decode function, not a `Read` adapter, so
/// `open_tar_archive` decompresses the entire archive into memory once up
/// front for that case instead of streaming it — acceptable for the
/// "reasonable size" archives this feature targets, same as gzip/bzip2's
/// own one-time-per-open cost (see this module's doc comment).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarCompression {
    /// A plain, uncompressed `.tar` — named `Plain` rather than `None` so
    /// `TarCompression::Plain` (and a `use TarCompression::*` glob import,
    /// as the tests below do) never collides with `Option::None`.
    Plain,
    Gzip,
    Bzip2,
    Xz,
}

/// Detects which archive backend (if any) `path` names, purely from its
/// filename — case-insensitive, and, for the tar family, matching the
/// *whole* compound suffix (`.tar.gz`, not just `.gz`) rather than
/// `Path::extension()`, which only ever sees the last dot-separated part.
/// `None` for anything else, meaning "not a Virtual-Directory candidate at
/// all" to every caller (`is_archive_file`/`App::begin_open`).
pub fn detect_archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_lowercase();
    const TAR_SUFFIXES: &[(&str, TarCompression)] = &[
        (".tar.gz", TarCompression::Gzip),
        (".tgz", TarCompression::Gzip),
        (".tar.bz2", TarCompression::Bzip2),
        (".tbz2", TarCompression::Bzip2),
        (".tar.xz", TarCompression::Xz),
        (".txz", TarCompression::Xz),
        (".tar", TarCompression::Plain),
    ];
    if name.ends_with(".zip") {
        return Some(ArchiveKind::Zip);
    }
    for (suffix, compression) in TAR_SUFFIXES {
        if name.ends_with(suffix) {
            return Some(ArchiveKind::Tar(*compression));
        }
    }
    None
}

/// Whether `path` names something ozzel should try to browse as a Virtual
/// Directory when `open`ed — any recognized archive format, see
/// [`detect_archive_kind`]. Only consulted for a *file* under the cursor
/// in a real (non-virtual) pane — a nested archive found *inside* another
/// one is deliberately not recursed into (see this module's doc comment
/// and `App::begin_open`'s nested-archive handling); it just opens in the
/// plain viewer like any other file.
pub fn is_archive_file(path: &Path) -> bool {
    detect_archive_kind(path).is_some()
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

/// One archive entry, already reduced to exactly what the shared
/// listing-grouping logic (`group_children`) needs — a safe (zip-slip/
/// tar-slip-checked) path plus enough metadata to synthesize a row. Built
/// by both backends (`read_zip_dir_entries`/`read_tar_dir_entries`) so
/// `group_children`'s "one path component below `inner` is a direct
/// child, anything deeper implies synthesized intermediate `Dir`s" logic
/// exists exactly once regardless of archive format.
struct RawEntry {
    path: PathBuf,
    kind: EntryKind,
    size: u64,
    mtime: Option<SystemTime>,
}

/// Reads `archive_path` (any format `detect_archive_kind` recognizes) and
/// synthesizes the immediate children of `inner` as `FsEntry` rows — the
/// dispatching, format-agnostic entry point `Pane` reads a virtual
/// listing through (`Pane::reload`/`enter_virtual`/`virtual_descend`).
/// See `read_zip_dir_entries`/`read_tar_dir_entries` for the two backends'
/// own listing/path-safety notes.
pub fn read_archive_dir_entries(archive_path: &Path, inner: &Path) -> Result<Vec<FsEntry>> {
    match detect_archive_kind(archive_path) {
        Some(ArchiveKind::Zip) => read_zip_dir_entries(archive_path, inner),
        Some(ArchiveKind::Tar(compression)) => {
            read_tar_dir_entries(archive_path, compression, inner)
        }
        None => bail!(
            "not a recognized archive format: {}",
            archive_path.display()
        ),
    }
}

/// Reads `archive_path`'s central directory into `RawEntry`s — see
/// `group_children` for how those become the actual listing.
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
/// entirely — they're simply never shown, so there's nothing for a later
/// `open`/extract to act on. This is stricter than
/// `tasks::archive::run_unzip`'s "log and skip" policy since a plain
/// directory listing has nowhere to log to.
fn read_zip_dir_entries(archive_path: &Path, inner: &Path) -> Result<Vec<FsEntry>> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("not a valid zip archive: {}", archive_path.display()))?;

    let mut raw = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .with_context(|| format!("failed to read archive entry {i}"))?;
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let kind = if entry.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let mtime = entry.last_modified().and_then(zip_datetime_to_systemtime);
        raw.push(RawEntry {
            path,
            kind,
            size: entry.size(),
            mtime,
        });
    }
    Ok(group_children(raw, inner))
}

/// The tar-family counterpart of `read_zip_dir_entries`: streams the whole
/// archive once (tar has no central directory to seek into — see this
/// module's doc comment) via `open_tar_archive`, skipping any entry
/// `enclosed_tar_path` rejects (the tar-slip equivalent of zip's
/// `enclosed_name`), and collecting the rest into `RawEntry`s for
/// `group_children`. Symlink entries are listed with `EntryKind::Symlink`
/// (visible, but never synthesized as an intermediate directory — a
/// symlink can't sensibly have children in a listing that never resolves
/// it) rather than excluded, matching the plan's "list them visibly".
fn read_tar_dir_entries(
    archive_path: &Path,
    compression: TarCompression,
    inner: &Path,
) -> Result<Vec<FsEntry>> {
    let mut archive = open_tar_archive(archive_path, compression)?;
    let mut raw = Vec::new();
    for entry in archive
        .entries()
        .with_context(|| format!("failed to read archive: {}", archive_path.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read an entry in {}", archive_path.display()))?;
        let raw_path = entry
            .path()
            .with_context(|| {
                format!(
                    "failed to read an entry's name in {}",
                    archive_path.display()
                )
            })?
            .into_owned();
        let Some(path) = enclosed_tar_path(&raw_path) else {
            continue;
        };
        let entry_type = entry.header().entry_type();
        let kind = if entry_type.is_dir() {
            EntryKind::Dir
        } else if entry_type.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        let mtime = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs));
        raw.push(RawEntry {
            path,
            kind,
            size: entry.size(),
            mtime,
        });
    }
    Ok(group_children(raw, inner))
}

/// Groups `raw` (every safe entry in the whole archive) into the
/// immediate children of `inner`: an entry whose path has exactly one
/// component *below* `inner` is a direct child (using its own kind/size/
/// mtime); anything deeper only tells us an intermediate directory
/// component must exist between it and `inner`, synthesized as `Dir` even
/// when the archive never stored an explicit directory entry for it
/// (common for archives built by tools that only record file entries). A
/// later `RawEntry` at the same synthesized-or-real name overwrites an
/// earlier synthesized placeholder — but never a real entry, since
/// `HashMap::entry(..).or_insert(..)` (the synthesized path) only ever
/// fires when nothing's there yet, while a leaf write uses `insert`
/// unconditionally; archives are vanishingly unlikely to have both a real
/// entry *and* deeper entries "under" it at the same name; this matches
/// the pre-existing zip-only behavior exactly.
fn group_children(raw: Vec<RawEntry>, inner: &Path) -> Vec<FsEntry> {
    struct Child {
        kind: EntryKind,
        size: u64,
        mtime: Option<SystemTime>,
    }
    let mut children: HashMap<String, Child> = HashMap::new();

    for entry in raw {
        let relative_to_inner = match entry.path.strip_prefix(inner) {
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
            children.insert(
                name,
                Child {
                    kind: entry.kind,
                    size: entry.size,
                    mtime: entry.mtime,
                },
            );
        } else {
            children.entry(name).or_insert(Child {
                kind: EntryKind::Dir,
                size: 0,
                mtime: None,
            });
        }
    }

    children
        .into_iter()
        .map(|(name, child)| {
            let path = if inner.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                inner.join(&name)
            };
            let (name_lower, ext_lower) = crate::entry::lower_keys(&name);
            FsEntry {
                is_hidden: name.starts_with('.'),
                name,
                name_lower,
                ext_lower,
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
                // Never resolved — a symlink entry inside an archive (tar
                // only; zip has no such concept in this codebase) is
                // listed visibly (`EntryKind::Symlink`) but its target is
                // never followed for display purposes, unlike a real
                // filesystem symlink's `FsEntry::symlink_target`.
                symlink_target: None,
            }
        })
        .collect()
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

/// Opens `archive_path` as a tar stream, transparently decompressing it
/// first according to `compression` — the one place that knows how to
/// turn any supported tar-family file into a plain `tar::Archive`, reused
/// by both `read_tar_dir_entries`/`extract_single_from_tar` here and
/// `tasks::archive::run_extract`'s own streaming extraction pass. See
/// `TarCompression`'s doc comment for why `Xz` is the odd one out
/// (whole-buffer decompress up front rather than a streaming `Read`
/// adapter).
pub fn open_tar_archive(
    archive_path: &Path,
    compression: TarCompression,
) -> Result<tar::Archive<Box<dyn Read>>> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let reader: Box<dyn Read> = match compression {
        TarCompression::Plain => Box::new(file),
        TarCompression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        TarCompression::Bzip2 => Box::new(bzip2::read::BzDecoder::new(file)),
        TarCompression::Xz => {
            let mut input = io::BufReader::new(file);
            let mut decompressed = Vec::new();
            lzma_rs::xz_decompress(&mut input, &mut decompressed).map_err(|err| {
                anyhow::anyhow!(
                    "failed to decompress xz archive {}: {err}",
                    archive_path.display()
                )
            })?;
            Box::new(io::Cursor::new(decompressed))
        }
    };
    Ok(tar::Archive::new(reader))
}

/// The tar-family equivalent of zip's `enclosed_name()`: `None` for a
/// path that's absolute, has a Windows path prefix, or contains a `..`
/// component anywhere — i.e. one that would escape the archive root if
/// joined onto a destination directory. `tar::Entry::unpack`/`unpack_in`
/// have their own equivalent protection built in, but every caller here
/// (`read_tar_dir_entries`, `tasks::archive::run_extract`'s tar path)
/// extracts/lists entries manually rather than using those, so this ports
/// the exact same "reject and let the caller skip it" policy
/// `read_zip_dir_entries`'s doc comment describes for zip — plain `.`
/// components are dropped (not rejected), same as `enclosed_name`.
pub fn enclosed_tar_path(path: &Path) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if result.as_os_str().is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Extracts a single archive entry fully into memory for the viewer
/// (`open`/Enter on a file inside a virtual directory) — capped at
/// `size_cap` bytes, same truncation contract as `viewer::load`'s
/// on-disk path (`(bytes, truncated)`). Dispatches on `detect_archive_kind`
/// the same way `read_archive_dir_entries` does.
pub fn extract_single_to_memory(
    archive_path: &Path,
    inner_path: &Path,
    size_cap: u64,
) -> Result<(Vec<u8>, bool)> {
    match detect_archive_kind(archive_path) {
        Some(ArchiveKind::Zip) => extract_single_from_zip(archive_path, inner_path, size_cap),
        Some(ArchiveKind::Tar(compression)) => {
            extract_single_from_tar(archive_path, compression, inner_path, size_cap)
        }
        None => bail!(
            "not a recognized archive format: {}",
            archive_path.display()
        ),
    }
}

/// A genuinely encrypted zip entry surfaces `by_index`'s own "password
/// required" error here, which is exactly the "log clear error" the plan
/// asks for — there's no special case to write.
fn extract_single_from_zip(
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

/// tar is sequential — there's no random access by name the way zip's
/// `by_index`/central directory gives, so this re-streams the archive
/// from the start looking for `inner_path`, an accepted O(n) cost per the
/// plan (files are opened one at a time, not in a hot loop).
fn extract_single_from_tar(
    archive_path: &Path,
    compression: TarCompression,
    inner_path: &Path,
    size_cap: u64,
) -> Result<(Vec<u8>, bool)> {
    let mut archive = open_tar_archive(archive_path, compression)?;
    for entry in archive
        .entries()
        .with_context(|| format!("failed to read archive: {}", archive_path.display()))?
    {
        let mut entry = entry
            .with_context(|| format!("failed to read an entry in {}", archive_path.display()))?;
        let Some(path) = entry.path().ok().and_then(|p| enclosed_tar_path(&p)) else {
            continue;
        };
        if path != inner_path {
            continue;
        }
        let full_size = entry.size();
        let to_read = full_size.min(size_cap) as usize;
        let mut buf = vec![0u8; to_read];
        entry
            .read_exact(&mut buf)
            .with_context(|| format!("failed to read {}", inner_path.display()))?;
        return Ok((buf, full_size > size_cap));
    }
    bail!("not found in archive: {}", inner_path.display());
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

    // --- tar-family test fixtures -----------------------------------------

    /// Builds an uncompressed tar byte stream in memory: `files` as plain
    /// file entries, `symlinks` as symlink entries (`(path, target)`).
    /// Directories are never written explicitly — every tar test relies on
    /// the same "synthesize intermediate directories" path the zip tests
    /// do, so coverage stays equivalent across both backends.
    fn build_tar_bytes(files: &[(&str, &[u8])], symlinks: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        for (path, target) in symlinks {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_cksum();
            builder.append_link(&mut header, path, target).unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// The tar-family counterpart of `make_archive`: same three-entry
    /// shape (one root file, one file two levels deep with no explicit
    /// `src/` directory entry), written as `name` under `compression`.
    fn make_tar_archive(name: &str, compression: TarCompression) -> (tempfile::TempDir, PathBuf) {
        make_tar_archive_with(
            name,
            compression,
            &[
                ("readme.txt", b"hello".as_slice()),
                ("src/main.rs", b"fn main() {}".as_slice()),
                ("src/nested/deep.txt", b"deep".as_slice()),
            ],
            &[],
        )
    }

    fn make_tar_archive_with(
        name: &str,
        compression: TarCompression,
        files: &[(&str, &[u8])],
        symlinks: &[(&str, &str)],
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join(name);
        let tar_bytes = build_tar_bytes(files, symlinks);
        match compression {
            TarCompression::Plain => fs::write(&archive_path, &tar_bytes).unwrap(),
            TarCompression::Gzip => {
                let file = fs::File::create(&archive_path).unwrap();
                let mut encoder =
                    flate2::write::GzEncoder::new(file, flate2::Compression::default());
                encoder.write_all(&tar_bytes).unwrap();
                encoder.finish().unwrap();
            }
            TarCompression::Bzip2 => {
                let file = fs::File::create(&archive_path).unwrap();
                let mut encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::new(6));
                encoder.write_all(&tar_bytes).unwrap();
                encoder.finish().unwrap();
            }
            TarCompression::Xz => {
                let mut compressed = Vec::new();
                lzma_rs::xz_compress(&mut &tar_bytes[..], &mut compressed).unwrap();
                fs::write(&archive_path, &compressed).unwrap();
            }
        }
        (dir, archive_path)
    }

    #[test]
    fn detect_archive_kind_matches_zip_and_every_tar_variant_case_insensitively() {
        use ArchiveKind::*;
        use TarCompression::*;
        let cases: &[(&str, Option<ArchiveKind>)] = &[
            ("a.zip", Some(Zip)),
            ("a.ZIP", Some(Zip)),
            ("a.Zip", Some(Zip)),
            ("a.tar", Some(Tar(Plain))),
            ("a.TAR", Some(Tar(Plain))),
            ("a.tar.gz", Some(Tar(Gzip))),
            ("a.TAR.GZ", Some(Tar(Gzip))),
            ("a.tgz", Some(Tar(Gzip))),
            ("a.TGZ", Some(Tar(Gzip))),
            ("a.tar.bz2", Some(Tar(Bzip2))),
            ("a.tbz2", Some(Tar(Bzip2))),
            ("a.tar.xz", Some(Tar(Xz))),
            ("a.txz", Some(Tar(Xz))),
            ("a", None),
            ("a.gz", None), // a bare `.gz` with no `.tar` isn't a tar-family archive
            ("a.txt", None),
        ];
        for (name, expected) in cases {
            assert_eq!(
                detect_archive_kind(Path::new(name)),
                *expected,
                "for {name:?}"
            );
        }
    }

    #[test]
    fn is_archive_file_matches_any_recognized_format() {
        assert!(is_archive_file(Path::new("a.zip")));
        assert!(is_archive_file(Path::new("a.tar")));
        assert!(is_archive_file(Path::new("a.tar.gz")));
        assert!(is_archive_file(Path::new("a.tgz")));
        assert!(is_archive_file(Path::new("a.tar.bz2")));
        assert!(is_archive_file(Path::new("a.tbz2")));
        assert!(is_archive_file(Path::new("a.tar.xz")));
        assert!(is_archive_file(Path::new("a.txz")));
        assert!(!is_archive_file(Path::new("a.txt")));
        assert!(!is_archive_file(Path::new("a")));
    }

    #[test]
    fn root_listing_has_one_file_and_one_synthesized_dir() {
        let (_dir, archive_path) = make_archive();
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
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
        let entries = read_archive_dir_entries(&archive_path, Path::new("src")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["main.rs", "nested"]);

        let nested = entries.iter().find(|e| e.name == "nested").unwrap();
        assert_eq!(nested.kind, EntryKind::Dir);

        let deep_entries =
            read_archive_dir_entries(&archive_path, Path::new("src/nested")).unwrap();
        assert_eq!(deep_entries.len(), 1);
        assert_eq!(deep_entries[0].name, "deep.txt");
        assert_eq!(deep_entries[0].size, 4);
    }

    #[test]
    fn listing_a_directory_with_no_entries_is_empty() {
        let (_dir, archive_path) = make_archive();
        let entries = read_archive_dir_entries(&archive_path, Path::new("nonexistent")).unwrap();
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

        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["safe.txt"]);
    }

    // --- tar family: listing, every compression variant --------------------

    #[test]
    fn tar_root_listing_has_one_file_and_one_synthesized_dir() {
        let (_dir, archive_path) = make_tar_archive("project.tar", TarCompression::Plain);
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
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
    fn tar_nested_listing_synthesizes_intermediate_directories() {
        let (_dir, archive_path) = make_tar_archive("project.tar", TarCompression::Plain);
        let entries = read_archive_dir_entries(&archive_path, Path::new("src")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["main.rs", "nested"]);

        let deep_entries =
            read_archive_dir_entries(&archive_path, Path::new("src/nested")).unwrap();
        assert_eq!(deep_entries.len(), 1);
        assert_eq!(deep_entries[0].name, "deep.txt");
        assert_eq!(deep_entries[0].size, 4);
    }

    #[test]
    fn tar_gz_and_tgz_listings_match_the_uncompressed_equivalent() {
        for (name, compression) in [
            ("project.tar.gz", TarCompression::Gzip),
            ("project.tgz", TarCompression::Gzip),
        ] {
            let (_dir, archive_path) = make_tar_archive(name, compression);
            let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
            let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            names.sort();
            assert_eq!(names, vec!["readme.txt", "src"], "for {name}");
        }
    }

    #[test]
    fn tar_bz2_listing_and_extraction_work() {
        let (_dir, archive_path) = make_tar_archive("project.tar.bz2", TarCompression::Bzip2);
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["readme.txt", "src"]);

        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 10 * 1024 * 1024)
                .unwrap();
        assert_eq!(bytes, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn tar_xz_listing_and_extraction_work() {
        let (_dir, archive_path) = make_tar_archive("project.tar.xz", TarCompression::Xz);
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        let mut names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["readme.txt", "src"]);

        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 10 * 1024 * 1024)
                .unwrap();
        assert_eq!(bytes, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn tar_listing_handles_japanese_names() {
        let (_dir, archive_path) = make_tar_archive_with(
            "project.tar",
            TarCompression::Plain,
            &[(
                "日本語ディレクトリ/日本語ファイル.txt",
                "こんにちは".as_bytes(),
            )],
            &[],
        );
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "日本語ディレクトリ");
        assert_eq!(entries[0].kind, EntryKind::Dir);

        let inner =
            read_archive_dir_entries(&archive_path, Path::new("日本語ディレクトリ")).unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "日本語ファイル.txt");

        let (bytes, _) = extract_single_to_memory(
            &archive_path,
            Path::new("日本語ディレクトリ/日本語ファイル.txt"),
            10 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(bytes, "こんにちは".as_bytes());
    }

    #[test]
    fn tar_listing_shows_symlink_entries_visibly() {
        let (_dir, archive_path) = make_tar_archive_with(
            "project.tar",
            TarCompression::Plain,
            &[("real.txt", b"hi")],
            &[("link.txt", "real.txt")],
        );
        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        let link = entries.iter().find(|e| e.name == "link.txt").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[test]
    fn tar_slip_entries_are_excluded_from_the_listing() {
        // `tar::Builder::append_data`/`Header::set_path` both refuse to
        // write a `..`-containing path at all — a real malicious archive
        // wasn't built through this crate's safe writer API either, so
        // the fixture pokes the raw header bytes directly (the tar
        // equivalent of the zip test's low-level `ZipWriter::start_file`
        // with an unchecked name) to actually produce one.
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("evil.tar");
        let tar_bytes = {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, data) in [("../evil.txt", b"pwned".as_slice()), ("safe.txt", b"ok")] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                let name_bytes = path.as_bytes();
                header.as_old_mut().name[..name_bytes.len()].copy_from_slice(name_bytes);
                header.set_cksum();
                builder.append(&header, data).unwrap();
            }
            builder.into_inner().unwrap()
        };
        fs::write(&archive_path, &tar_bytes).unwrap();

        let entries = read_archive_dir_entries(&archive_path, Path::new("")).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["safe.txt"]);
    }

    #[test]
    fn extract_single_to_memory_from_tar_truncates_at_the_cap() {
        let (_dir, archive_path) = make_tar_archive("project.tar", TarCompression::Plain);
        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 2).unwrap();
        assert_eq!(bytes, b"he");
        assert!(truncated);
    }

    #[test]
    fn extract_single_to_memory_from_tar_errors_on_a_missing_entry() {
        let (_dir, archive_path) = make_tar_archive("project.tar", TarCompression::Plain);
        let result =
            extract_single_to_memory(&archive_path, Path::new("nope.txt"), 10 * 1024 * 1024);
        assert!(result.is_err());
    }

    #[test]
    fn enclosed_tar_path_rejects_parent_dir_and_absolute_paths() {
        assert_eq!(
            enclosed_tar_path(Path::new("safe/file.txt")).unwrap(),
            Path::new("safe/file.txt")
        );
        assert_eq!(
            enclosed_tar_path(Path::new("./safe/file.txt")).unwrap(),
            Path::new("safe/file.txt")
        );
        assert!(enclosed_tar_path(Path::new("../evil.txt")).is_none());
        assert!(enclosed_tar_path(Path::new("safe/../../evil.txt")).is_none());
        assert!(enclosed_tar_path(Path::new("/etc/passwd")).is_none());
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
