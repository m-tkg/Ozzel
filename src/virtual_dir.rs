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

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

use crate::entry::{EntryKind, FsEntry};

/// The full raw-entry listing of an archive, plus the `(mtime, len)` of the
/// archive file it was read from — see `VirtualDir::list` for how this is
/// kept valid across navigation and invalidated when the archive itself
/// changes underneath a pane.
#[derive(Debug)]
struct CachedEntries {
    mtime: Option<SystemTime>,
    len: u64,
    raw: Vec<RawEntry>,
}

/// A pane's position inside an archive: which `.zip` file, and which
/// directory level within it (`inner == ""` is the archive root).
#[derive(Debug, Clone)]
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
    /// The archive's full raw-entry listing, read once and reused for every
    /// `list()` call (descend/go_parent/reload) as long as the archive
    /// file's mtime+len haven't changed — see `list`. `Rc<RefCell<..>>`
    /// rather than a plain field so cloning a `VirtualDir` (done once, in
    /// `Pane::virtual_go_parent`) shares the same cache instead of
    /// resetting it — sharing the cache across that clone is the entire
    /// point, since go_parent immediately does another lookup with it.
    entry_cache: Rc<RefCell<Option<CachedEntries>>>,
    /// The password that successfully decrypted an entry of this archive,
    /// cached for the rest of this Virtual Directory session so browsing
    /// several files in one encrypted zip asks only once. Same
    /// `Rc<RefCell<..>>` clone-sharing story as `entry_cache` (so
    /// `virtual_go_parent`'s clone keeps it); dropped with the
    /// `VirtualDir` when the pane exits the archive. Never persisted.
    password: Rc<RefCell<Option<String>>>,
}

impl VirtualDir {
    /// Enters Virtual Directory mode at `archive_path`'s root — an empty
    /// entry cache, populated lazily on the first `list()` call.
    pub fn new(archive_path: PathBuf) -> Self {
        let archive_name = archive_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| archive_path.display().to_string());
        Self {
            archive_path,
            archive_name,
            inner: PathBuf::new(),
            entry_cache: Rc::new(RefCell::new(None)),
            password: Rc::new(RefCell::new(None)),
        }
    }

    /// The session-cached password, if one has been accepted — see the
    /// field's doc comment.
    pub fn cached_password(&self) -> Option<String> {
        self.password.borrow().clone()
    }

    /// Remembers `password` for the rest of this Virtual Directory
    /// session (called after a decrypt actually succeeded with it).
    pub fn cache_password(&self, password: String) {
        *self.password.borrow_mut() = Some(password);
    }

    /// Forgets a cached password that turned out to be wrong after all
    /// (the archive changed underneath, say).
    pub fn clear_password(&self) {
        *self.password.borrow_mut() = None;
    }

    /// Lists the immediate children of `inner`, reusing the cached raw
    /// entry list when the archive file hasn't changed since it was last
    /// read — no reopening/reparsing/decompressing the archive on a cache
    /// hit, regardless of how expensive that would be for this format (see
    /// this module's doc comment on `.tar.xz`'s whole-buffer decompress).
    ///
    /// Validity check: re-`stat`s `archive_path` and compares `(mtime,
    /// len)` against what the cache was built from. A changed value means
    /// the archive itself was replaced — read fresh raw entries and
    /// replace the cache. If the archive can no longer be `stat`ed at all
    /// (e.g. deleted or on an unmounted volume) but a cache already exists,
    /// the stale cache is served rather than erroring — a pane already
    /// browsing an archive shouldn't lose the ability to navigate around
    /// its previously-read listing just because the underlying file
    /// vanished; only a real "not readable and never was" case (no cache,
    /// can't stat, can't read) surfaces an error, exactly like before this
    /// cache existed.
    pub fn list(&self, inner: &Path) -> Result<Vec<FsEntry>> {
        let stat = fs::metadata(&self.archive_path).ok();
        let mut cache = self.entry_cache.borrow_mut();
        let stale = match (&*cache, &stat) {
            (Some(c), Some(m)) => c.mtime != m.modified().ok() || c.len != m.len(),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if stale {
            let raw = read_archive_raw_entries(&self.archive_path)?;
            let (len, mtime) = match &stat {
                Some(m) => (m.len(), m.modified().ok()),
                None => (0, None),
            };
            *cache = Some(CachedEntries { mtime, len, raw });
        }
        let raw = &cache.as_ref().expect("just populated above").raw;
        Ok(group_children(raw, inner))
    }
}

/// Which archive backend a Virtual Directory reads through — see this
/// module's doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Tar(TarCompression),
    /// A single compressed file with no container around it (`.gz`/`.bz2`
    /// alone, not `.tar.gz`/`.tar.bz2`) — browsed as a synthetic
    /// one-entry archive whose sole entry is the decompressed payload
    /// (see `read_single_raw_entry`).
    Single(SingleCompression),
}

/// The compression wrapping an `ArchiveKind::Single` payload. A subset of
/// `TarCompression` on purpose: xz stays tar-only for now (`lzma-rs` has
/// no streaming `Read` adapter — see `TarCompression`'s doc comment — and
/// plain `.xz` files are far rarer than `.gz`/`.bz2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleCompression {
    Gzip,
    Bzip2,
}

/// How a genuinely password-related zip failure is distinguished from any
/// other archive error: mapped out of the `zip` crate's errors by
/// [`map_zip_error`] and downcast back (`err.downcast_ref::<ZipPasswordError>()`)
/// by the UI layer, which reacts by prompting for (or re-prompting after
/// a wrong) password instead of just logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ZipPasswordError {
    #[error("password required")]
    PasswordRequired,
    #[error("wrong password")]
    InvalidPassword,
}

/// Wraps a `zip` crate error, lifting the two password cases into
/// [`ZipPasswordError`] so callers can `downcast_ref` them; everything
/// else passes through as-is. The "password required" case arrives as
/// `UnsupportedArchive` with a fixed message rather than its own variant,
/// so it's matched by the message the crate itself defines
/// (`ZipError::PASSWORD_REQUIRED`).
pub fn map_zip_error(err: zip::result::ZipError) -> anyhow::Error {
    use zip::result::ZipError;
    match &err {
        ZipError::UnsupportedArchive(msg) if *msg == ZipError::PASSWORD_REQUIRED => {
            anyhow::Error::new(ZipPasswordError::PasswordRequired)
        }
        ZipError::InvalidPassword => anyhow::Error::new(ZipPasswordError::InvalidPassword),
        _ => anyhow::Error::new(err),
    }
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
    // Bare compressed files — checked strictly *after* the tar suffixes,
    // so `.tar.gz` has already matched as Tar(Gzip) above and never gets
    // here (`.tgz`/`.tbz2` don't end in `.gz`/`.bz2` at all).
    if name.ends_with(".gz") {
        return Some(ArchiveKind::Single(SingleCompression::Gzip));
    }
    if name.ends_with(".bz2") {
        return Some(ArchiveKind::Single(SingleCompression::Bzip2));
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
#[derive(Debug)]
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
    let raw = read_archive_raw_entries(archive_path)?;
    Ok(group_children(&raw, inner))
}

/// Reads every safe raw entry in `archive_path`, uncached — the shared
/// backbone of both `read_archive_dir_entries` (grouped fresh every call)
/// and `VirtualDir::list` (grouped from a cached copy of this same list).
fn read_archive_raw_entries(archive_path: &Path) -> Result<Vec<RawEntry>> {
    match detect_archive_kind(archive_path) {
        Some(ArchiveKind::Zip) => read_zip_raw_entries(archive_path),
        Some(ArchiveKind::Tar(compression)) => read_tar_raw_entries(archive_path, compression),
        Some(ArchiveKind::Single(compression)) => {
            Ok(vec![read_single_raw_entry(archive_path, compression)?])
        }
        None => bail!(
            "not a recognized archive format: {}",
            archive_path.display()
        ),
    }
}

/// The name a bare `.gz`/`.bz2` payload is listed (and extracted) under:
/// the archive's file name with its final extension stripped (`notes.txt.gz`
/// -> `notes.txt`), falling back to `"data"` for a bare `.gz` with nothing
/// in front of the dot.
pub fn single_payload_name(archive_path: &Path) -> String {
    let stem = archive_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if stem.is_empty() {
        "data".to_string()
    } else {
        stem
    }
}

/// Synthesizes the one-entry listing of a bare `.gz`/`.bz2` — see
/// `ArchiveKind::Single`. The entry's mtime is the archive file's own
/// (neither format records one usably: bzip2 not at all, gzip optionally
/// but rarely). Size: gzip's footer stores the uncompressed size mod 2^32
/// (ISIZE, the last 4 little-endian bytes — inaccurate above 4 GiB, an
/// accepted display-only caveat); bzip2 stores nothing, so `0` — the
/// column just shows the placeholder-ish zero until the file is opened.
fn read_single_raw_entry(archive_path: &Path, compression: SingleCompression) -> Result<RawEntry> {
    let meta = fs::metadata(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let size = match compression {
        SingleCompression::Gzip => gzip_isize(archive_path).unwrap_or(0),
        SingleCompression::Bzip2 => 0,
    };
    Ok(RawEntry {
        path: PathBuf::from(single_payload_name(archive_path)),
        kind: EntryKind::File,
        size,
        mtime: meta.modified().ok(),
    })
}

/// The gzip footer's ISIZE field: uncompressed length mod 2^32, from the
/// file's last 4 bytes. `None` for anything too short to have a footer.
fn gzip_isize(archive_path: &Path) -> Option<u64> {
    use std::io::{Seek, SeekFrom};
    let mut file = fs::File::open(archive_path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 18 {
        // gzip header (10) + footer (8): anything shorter isn't a valid
        // gzip stream at all, let alone one with a trustworthy footer.
        return None;
    }
    file.seek(SeekFrom::End(-4)).ok()?;
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf).ok()?;
    Some(u32::from_le_bytes(buf) as u64)
}

/// The `Read` decoder for a bare `.gz`/`.bz2` payload — the Single
/// counterpart of `open_tar_archive`, shared by the in-memory viewer path
/// (`extract_single_from_single`) and `tasks::archive`'s to-disk
/// extraction.
pub fn open_single_reader(
    archive_path: &Path,
    compression: SingleCompression,
) -> Result<Box<dyn Read>> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    Ok(match compression {
        SingleCompression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        SingleCompression::Bzip2 => Box::new(bzip2::read::BzDecoder::new(file)),
    })
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
fn read_zip_raw_entries(archive_path: &Path) -> Result<Vec<RawEntry>> {
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
    Ok(raw)
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
fn read_tar_raw_entries(archive_path: &Path, compression: TarCompression) -> Result<Vec<RawEntry>> {
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
    Ok(raw)
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
fn group_children(raw: &[RawEntry], inner: &Path) -> Vec<FsEntry> {
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

/// Whether any entry in `archive_path` (a zip) is encrypted — the cheap
/// metadata-only scan `begin_extract`/`begin_unzip` use to decide whether
/// to ask for a password *before* spawning a task that would only fail.
pub fn zip_has_encrypted_entries(archive_path: &Path) -> Result<bool> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("not a valid zip archive: {}", archive_path.display()))?;
    for i in 0..archive.len() {
        if archive.by_index_raw(i)?.encrypted() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Verifies `password` against the first encrypted entry by actually
/// decrypting one byte of it — run on the main thread before spawning an
/// extraction, so a typo'd password fails at the prompt (re-askable)
/// rather than mid-task. `Ok(())` when nothing is encrypted at all. Note
/// legacy ZipCrypto's checksum lets ~1/256 wrong passwords through this
/// check; those still fail later, inside the task, as a logged error.
pub fn verify_zip_password(archive_path: &Path, password: &str) -> Result<()> {
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("not a valid zip archive: {}", archive_path.display()))?;
    let mut target = None;
    for i in 0..archive.len() {
        let raw = archive.by_index_raw(i)?;
        if raw.encrypted() && raw.size() > 0 {
            target = Some(i);
            break;
        }
    }
    let Some(index) = target else {
        return Ok(());
    };
    let mut entry = zip_entry_reader(&mut archive, index, Some(password))?;
    let mut probe = [0u8; 1];
    entry
        .read_exact(&mut probe)
        .map_err(|e| anyhow::anyhow!("failed to verify password: {e}"))?;
    Ok(())
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
/// `password` only matters for zip (`Some` decrypts with it, `None` reads
/// plain entries and surfaces [`ZipPasswordError::PasswordRequired`] on an
/// encrypted one); the tar family and bare `.gz`/`.bz2` have no
/// encryption concept and ignore it.
pub fn extract_single_to_memory(
    archive_path: &Path,
    inner_path: &Path,
    size_cap: u64,
    password: Option<&str>,
) -> Result<(Vec<u8>, bool)> {
    match detect_archive_kind(archive_path) {
        Some(ArchiveKind::Zip) => {
            extract_single_from_zip(archive_path, inner_path, size_cap, password)
        }
        Some(ArchiveKind::Tar(compression)) => {
            extract_single_from_tar(archive_path, compression, inner_path, size_cap)
        }
        Some(ArchiveKind::Single(compression)) => {
            extract_single_from_single(archive_path, compression, inner_path, size_cap)
        }
        None => bail!(
            "not a recognized archive format: {}",
            archive_path.display()
        ),
    }
}

/// Opens zip entry `index` for reading, decrypting with `password` when
/// one is supplied *and the entry is actually encrypted* (a plain entry
/// in a partially-encrypted archive must not go through the decrypt path
/// — `by_index_decrypt` would misinterpret its leading bytes as a crypto
/// header). Password-shaped failures come back as [`ZipPasswordError`]
/// via [`map_zip_error`]. The one shared "read an entry's bytes with an
/// optional password" primitive — the viewer path here and
/// `tasks::archive`'s extraction both go through it.
pub fn zip_entry_reader<'a, R: io::Read + io::Seek>(
    archive: &'a mut ZipArchive<R>,
    index: usize,
    password: Option<&str>,
) -> Result<zip::read::ZipFile<'a, R>> {
    let encrypted = archive.by_index_raw(index)?.encrypted();
    let result = match password {
        Some(pw) if encrypted => archive.by_index_decrypt(index, pw.as_bytes()),
        _ => archive.by_index(index),
    };
    result.map_err(map_zip_error)
}

/// A genuinely encrypted zip entry surfaces as
/// [`ZipPasswordError::PasswordRequired`]/[`InvalidPassword`] here (see
/// [`zip_entry_reader`]), which is what lets the UI prompt for a password
/// and retry instead of just logging an opaque error.
fn extract_single_from_zip(
    archive_path: &Path,
    inner_path: &Path,
    size_cap: u64,
    password: Option<&str>,
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

    let mut entry = zip_entry_reader(&mut archive, index, password)?;
    let full_size = entry.size();
    let to_read = full_size.min(size_cap) as usize;
    let mut buf = vec![0u8; to_read];
    entry
        .read_exact(&mut buf)
        .with_context(|| format!("failed to read {}", inner_path.display()))?;
    Ok((buf, full_size > size_cap))
}

/// The Single (bare `.gz`/`.bz2`) viewer path: streams the decoder up to
/// `size_cap` bytes, probing one byte further to learn whether the
/// payload was truncated (neither format's header states the exact
/// uncompressed size reliably — see `read_single_raw_entry`).
fn extract_single_from_single(
    archive_path: &Path,
    compression: SingleCompression,
    inner_path: &Path,
    size_cap: u64,
) -> Result<(Vec<u8>, bool)> {
    let expected = single_payload_name(archive_path);
    if inner_path != Path::new(&expected) {
        bail!("not found in archive: {}", inner_path.display());
    }
    let mut reader = open_single_reader(archive_path, compression)?;
    let mut buf = Vec::new();
    reader
        .by_ref()
        .take(size_cap)
        .read_to_end(&mut buf)
        .with_context(|| format!("failed to decompress {}", archive_path.display()))?;
    let truncated = buf.len() as u64 == size_cap && reader.read(&mut [0u8; 1])? > 0;
    Ok((buf, truncated))
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

    fn make_gz(dir: &Path, name: &str, payload: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        path
    }

    fn make_bz2(dir: &Path, name: &str, payload: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        let mut enc = bzip2::write::BzEncoder::new(file, bzip2::Compression::default());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        path
    }

    fn make_encrypted_zip(dir: &Path) -> PathBuf {
        let path = dir.join("secret.zip");
        let file = fs::File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .with_aes_encryption(zip::AesMode::Aes256, "hunter2");
        writer.start_file("secret.txt", options).unwrap();
        writer.write_all(b"top secret payload").unwrap();
        writer.finish().unwrap();
        path
    }

    #[test]
    fn single_gz_lists_one_entry_with_isize_and_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"hello single gzip payload";
        let path = make_gz(dir.path(), "notes.txt.gz", payload);

        let entries = read_archive_dir_entries(&path, Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "notes.txt");
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[0].size, payload.len() as u64, "gzip ISIZE footer");

        let (bytes, truncated) =
            extract_single_to_memory(&path, Path::new("notes.txt"), 1024, None).unwrap();
        assert_eq!(bytes, payload);
        assert!(!truncated);
    }

    #[test]
    fn single_gz_respects_the_size_cap_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let payload = vec![b'x'; 100];
        let path = make_gz(dir.path(), "big.gz", &payload);

        let (bytes, truncated) =
            extract_single_to_memory(&path, Path::new("big"), 10, None).unwrap();
        assert_eq!(bytes.len(), 10);
        assert!(truncated);
    }

    #[test]
    fn single_bz2_lists_one_entry_with_unknown_size_and_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"bzip2 payload here";
        let path = make_bz2(dir.path(), "notes.txt.bz2", payload);

        let entries = read_archive_dir_entries(&path, Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "notes.txt");
        assert_eq!(entries[0].size, 0, "bzip2 records no uncompressed size");

        let (bytes, truncated) =
            extract_single_to_memory(&path, Path::new("notes.txt"), 1024, None).unwrap();
        assert_eq!(bytes, payload);
        assert!(!truncated);
    }

    #[test]
    fn single_extract_of_a_wrong_inner_name_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_gz(dir.path(), "notes.txt.gz", b"x");
        let err = extract_single_to_memory(&path, Path::new("wrong-name"), 1024, None).unwrap_err();
        assert!(err.to_string().contains("not found in archive"));
    }

    #[test]
    fn encrypted_zip_lists_without_a_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_encrypted_zip(dir.path());
        let entries = read_archive_dir_entries(&path, Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "secret.txt");
        assert!(zip_has_encrypted_entries(&path).unwrap());
    }

    #[test]
    fn encrypted_zip_read_without_password_is_password_required() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_encrypted_zip(dir.path());
        let err = extract_single_to_memory(&path, Path::new("secret.txt"), 1024, None).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ZipPasswordError>(),
            Some(&ZipPasswordError::PasswordRequired)
        );
    }

    #[test]
    fn encrypted_zip_reads_with_the_right_password_and_rejects_a_wrong_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_encrypted_zip(dir.path());

        let (bytes, truncated) =
            extract_single_to_memory(&path, Path::new("secret.txt"), 1024, Some("hunter2"))
                .unwrap();
        assert_eq!(bytes, b"top secret payload");
        assert!(!truncated);

        let err = extract_single_to_memory(&path, Path::new("secret.txt"), 1024, Some("nope"))
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<ZipPasswordError>(),
            Some(&ZipPasswordError::InvalidPassword)
        );
    }

    #[test]
    fn verify_zip_password_accepts_right_rejects_wrong_and_passes_plain() {
        let dir = tempfile::tempdir().unwrap();
        let encrypted = make_encrypted_zip(dir.path());
        assert!(verify_zip_password(&encrypted, "hunter2").is_ok());
        let err = verify_zip_password(&encrypted, "nope").unwrap_err();
        assert!(err.downcast_ref::<ZipPasswordError>().is_some());

        let (_plain_dir, plain) = make_archive();
        assert!(!zip_has_encrypted_entries(&plain).unwrap());
        assert!(
            verify_zip_password(&plain, "anything").is_ok(),
            "nothing encrypted -> nothing to verify against"
        );
    }

    #[test]
    fn plain_entries_in_a_zip_still_read_when_a_password_is_supplied() {
        // A password in hand must not break reading *unencrypted* entries
        // (zip_entry_reader only routes through decrypt for entries whose
        // own flag says encrypted).
        let (_dir, archive_path) = make_archive();
        let (bytes, _) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 1024, Some("pw"))
                .unwrap();
        assert_eq!(bytes, b"hello");
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
            // Bare compressed files are Single, and — the ordering
            // regression this pins — a `.tar.gz` stays Tar(Gzip) even
            // though it also ends in `.gz`.
            ("a.gz", Some(Single(SingleCompression::Gzip))),
            ("a.GZ", Some(Single(SingleCompression::Gzip))),
            ("a.bz2", Some(Single(SingleCompression::Bzip2))),
            ("notes.txt.gz", Some(Single(SingleCompression::Gzip))),
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
        let (bytes, truncated) = extract_single_to_memory(
            &archive_path,
            Path::new("readme.txt"),
            10 * 1024 * 1024,
            None,
        )
        .unwrap();
        assert_eq!(bytes, b"hello");
        assert!(!truncated);
    }

    #[test]
    fn extract_single_to_memory_truncates_at_the_cap() {
        let (_dir, archive_path) = make_archive();
        let (bytes, truncated) =
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 2, None).unwrap();
        assert_eq!(bytes, b"he");
        assert!(truncated);
    }

    #[test]
    fn extract_single_to_memory_errors_on_a_missing_entry() {
        let (_dir, archive_path) = make_archive();
        let result =
            extract_single_to_memory(&archive_path, Path::new("nope.txt"), 10 * 1024 * 1024, None);
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

        let (bytes, truncated) = extract_single_to_memory(
            &archive_path,
            Path::new("readme.txt"),
            10 * 1024 * 1024,
            None,
        )
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

        let (bytes, truncated) = extract_single_to_memory(
            &archive_path,
            Path::new("readme.txt"),
            10 * 1024 * 1024,
            None,
        )
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
            None,
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
            extract_single_to_memory(&archive_path, Path::new("readme.txt"), 2, None).unwrap();
        assert_eq!(bytes, b"he");
        assert!(truncated);
    }

    #[test]
    fn extract_single_to_memory_from_tar_errors_on_a_missing_entry() {
        let (_dir, archive_path) = make_tar_archive("project.tar", TarCompression::Plain);
        let result =
            extract_single_to_memory(&archive_path, Path::new("nope.txt"), 10 * 1024 * 1024, None);
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
        let mut vd = VirtualDir::new(PathBuf::from("/tmp/project.zip"));
        vd.inner = PathBuf::from("src/nested");
        assert_eq!(header_label(&vd), "project.zip:/src/nested");
    }

    #[test]
    fn header_label_at_archive_root_shows_a_bare_slash() {
        let vd = VirtualDir::new(PathBuf::from("/tmp/project.zip"));
        assert_eq!(header_label(&vd), "project.zip:/");
    }

    // --- entry-list cache ---------------------------------------------------

    #[test]
    fn list_reuses_the_cache_even_after_the_archive_file_is_deleted() {
        let (dir, archive_path) = make_archive();
        let vd = VirtualDir::new(archive_path.clone());
        let root = vd.list(Path::new("")).unwrap();
        assert_eq!(root.len(), 2);

        // Deleting the archive can't change what `stat` reports (there's
        // nothing left to stat), so a cache hit must fall back to serving
        // the already-cached listing rather than erroring.
        fs::remove_file(&archive_path).unwrap();
        let src = vd.list(Path::new("src")).unwrap();
        let mut names: Vec<&str> = src.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["main.rs", "nested"]);
        drop(dir);
    }

    #[test]
    fn list_refreshes_when_the_archive_is_replaced_with_different_content() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("project.zip");
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            writer
                .start_file("a.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"first").unwrap();
            writer.finish().unwrap();
        }

        let vd = VirtualDir::new(archive_path.clone());
        let first = vd.list(Path::new("")).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "a.txt");

        // Sleep isn't reliable enough across filesystems for an mtime-only
        // diff, so the replacement archive also changes size — `list`'s
        // staleness check is `(mtime, len)` together, and a size change
        // alone is enough to force a refresh even if the filesystem's mtime
        // resolution happens to make the timestamps compare equal.
        {
            let file = fs::File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            writer
                .start_file("a.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"first-and-then-some-more-bytes").unwrap();
            writer
                .start_file("b.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"second").unwrap();
            writer.finish().unwrap();
        }

        let second = vd.list(Path::new("")).unwrap();
        let mut names: Vec<&str> = second.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn cloning_a_virtual_dir_shares_the_same_cache() {
        let (dir, archive_path) = make_archive();
        let vd = VirtualDir::new(archive_path.clone());
        vd.list(Path::new("")).unwrap();
        let cloned = vd.clone();

        fs::remove_file(&archive_path).unwrap();
        // If the clone didn't share the cache, this would try to re-read
        // the now-deleted archive from scratch and fail.
        let entries = cloned.list(Path::new("")).unwrap();
        assert_eq!(entries.len(), 2);
        drop(dir);
    }
}
