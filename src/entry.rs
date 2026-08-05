//! Filesystem entry model and directory reading.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

/// What kind of filesystem object an [`FsEntry`] refers to. Symlinks are
/// their own kind (never resolved to their target) so that callers can
/// decide how to treat them without accidentally following a link during
/// browsing, copy, or delete. See [`FsEntry::symlink_target`] for what a
/// symlink's target actually resolves to, kept separate from this so file
/// operations (copy/move/delete/duplicate/zip) — which must *never* follow
/// a link — can keep branching on the raw, unresolved `kind` alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// What a symlink's target resolves to, following the *entire* chain the
/// way `fs::metadata` does (so a symlink to a symlink to a directory still
/// resolves to `Dir`). See [`FsEntry::symlink_target`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkTarget {
    Dir,
    File,
}

/// A single row a directory listing can show.
#[derive(Debug, Clone, PartialEq)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub is_hidden: bool,
    /// The raw unix permission bits (`st_mode & 0o7777`), captured once at
    /// read time so rendering the permissions column never needs an extra
    /// per-row `stat` call — see `ui::pane_view::format_permissions`.
    /// `None` on non-unix platforms, which fall back to `readonly` instead.
    pub unix_mode: Option<u32>,
    /// `std::fs::Permissions::readonly()` — meaningful (and used) on every
    /// platform, but only actually *rendered* on non-unix ones, where
    /// `unix_mode` is `None` (see `format_permissions`'s Windows fallback).
    pub readonly: bool,
    /// Whether this entry counts as "executable" for the `[colors]
    /// executable` row color (see `entry_type_color`) — computed once here
    /// rather than at render time, per platform: unix checks any `x` bit on
    /// a regular file (`is_executable_unix`); Windows checks the file
    /// extension against a small PATHEXT-ish set (`is_executable_windows`).
    /// Directories are never executable regardless of platform.
    pub is_executable: bool,
    /// For a symlink entry (`kind == EntryKind::Symlink`) only: what its
    /// target resolves to — `None` for a dangling symlink (target doesn't
    /// exist / can't be stat'd), and trivially `None` for any non-symlink
    /// entry too. Costs one extra `fs::metadata` call (which *does*
    /// follow) per symlink, never per entry — see `read_dir_entries`.
    ///
    /// This is what lets a directory-symlink behave like a directory
    /// everywhere in navigation/display (`FsEntry::is_dir_like`,
    /// `pane::compare_entries`'s sort grouping, `ui::pane_view`'s color/
    /// size column, `App::begin_open`'s descend-vs-view dispatch) while
    /// `kind` itself stays the link's own, unresolved kind — file
    /// operations (copy/move/delete/duplicate/zip, see
    /// `tasks::copy_move`/`tasks::delete`/`tasks::archive`) independently
    /// re-stat with `fs::symlink_metadata` (no follow) and never even look
    /// at this field, so they keep operating on the link itself,
    /// unconditionally, regardless of what it points to.
    pub symlink_target: Option<SymlinkTarget>,
}

impl FsEntry {
    /// Whether this entry should be treated as a directory for
    /// navigation/sorting/coloring purposes: a real directory, or a
    /// symlink whose target resolves to one. `false` for a dangling
    /// symlink or a symlink-to-a-file, same as a plain file.
    pub fn is_dir_like(&self) -> bool {
        self.kind == EntryKind::Dir || self.symlink_target == Some(SymlinkTarget::Dir)
    }
}

/// unix executable detection: any owner/group/other `x` bit set.
/// - A regular file: checked against its own mode bits (`own_mode`).
/// - A symlink whose target resolves to a regular file
///   (`symlink_target == Some(SymlinkTarget::File)`): checked against the
///   *target's* mode bits (`target_mode`) instead — a symlink's own mode
///   bits are conventionally `777` on Linux regardless of what they point
///   to, which would make the `executable` color meaningless noise
///   otherwise; using the target's real mode is what makes it a useful
///   signal again.
/// - Anything else (a directory, a directory-symlink, a dangling
///   symlink): never executable.
#[cfg(unix)]
fn is_executable_unix(
    kind: EntryKind,
    own_mode: u32,
    symlink_target: Option<SymlinkTarget>,
    target_mode: Option<u32>,
) -> bool {
    match (kind, symlink_target) {
        (EntryKind::File, _) => own_mode & 0o111 != 0,
        (EntryKind::Symlink, Some(SymlinkTarget::File)) => {
            target_mode.is_some_and(|m| m & 0o111 != 0)
        }
        _ => false,
    }
}

/// Windows executable detection: a regular file whose extension is one of
/// a small, common "you'd expect this to run" set — deliberately not a
/// full `PATHEXT` parse (which is user/environment-configurable and would
/// need reading `%PATHEXT%` at read-dir time), just the obvious ones.
#[cfg(windows)]
fn is_executable_windows(kind: EntryKind, name: &str) -> bool {
    if kind != EntryKind::File {
        return false;
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(ext.as_str(), "exe" | "bat" | "cmd" | "ps1" | "com")
}

/// Dotfile convention for "hidden". On Windows this deliberately ignores the
/// filesystem's hidden attribute for now (per plan: "on Windows just dotfile
/// for now") to keep the rule identical across platforms.
fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// Reads the immediate children of `path` into [`FsEntry`] rows. Does not
/// recurse and does not follow symlinks for its *own* kind/metadata (a
/// symlinked directory is reported as [`EntryKind::Symlink`], not
/// [`EntryKind::Dir`]) — but does additionally resolve each symlink's
/// *target* kind into [`FsEntry::symlink_target`] (one extra, following
/// `fs::metadata` call per symlink, never per entry) so callers can treat
/// a directory-symlink like a directory without this module ever
/// conflating "what the link itself is" with "what it points to".
pub fn read_dir_entries(path: &Path) -> Result<Vec<FsEntry>> {
    let read_dir = std::fs::read_dir(path)
        .with_context(|| format!("failed to read directory: {}", path.display()))?;

    let mut entries = Vec::new();
    for dir_entry in read_dir {
        let dir_entry = dir_entry?;
        let name = dir_entry.file_name().to_string_lossy().into_owned();
        let entry_path = dir_entry.path();

        let file_type = dir_entry
            .file_type()
            .with_context(|| format!("failed to stat: {}", entry_path.display()))?;
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink
        } else if file_type.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };

        // `DirEntry::metadata` does not traverse symlinks, so a symlinked
        // file/dir reports the link's own metadata here rather than the
        // target's.
        let metadata = dir_entry
            .metadata()
            .with_context(|| format!("failed to stat: {}", entry_path.display()))?;

        // The one extra, *following* stat — only for symlinks, never for a
        // plain file/dir. `Ok` and non-dangling -> `Some`; a broken link
        // (or any other resolution failure) simply leaves this `None`,
        // which is exactly "neither navigable nor viewable" downstream.
        let target_metadata = if kind == EntryKind::Symlink {
            std::fs::metadata(&entry_path).ok()
        } else {
            None
        };
        let symlink_target = target_metadata.as_ref().map(|m| {
            if m.is_dir() {
                SymlinkTarget::Dir
            } else {
                SymlinkTarget::File
            }
        });

        #[cfg(unix)]
        let (unix_mode, is_executable) = {
            use std::os::unix::fs::PermissionsExt;
            let own_mode = metadata.permissions().mode();
            let target_mode = target_metadata.as_ref().map(|m| m.permissions().mode());
            (
                Some(own_mode),
                is_executable_unix(kind, own_mode, symlink_target, target_mode),
            )
        };
        #[cfg(not(unix))]
        let (unix_mode, is_executable): (Option<u32>, bool) =
            (None, is_executable_windows(kind, &name));

        entries.push(FsEntry {
            is_hidden: is_hidden_name(&name),
            name,
            path: entry_path,
            kind,
            size: metadata.len(),
            mtime: metadata.modified().ok(),
            unix_mode,
            readonly: metadata.permissions().readonly(),
            is_executable,
            symlink_target,
        });
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reads_files_dirs_and_hidden_flag() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("visible.txt"), b"hi").unwrap();
        fs::write(dir.path().join(".hidden"), b"hi").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        let mut entries = read_dir_entries(dir.path()).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 3);

        let hidden = entries.iter().find(|e| e.name == ".hidden").unwrap();
        assert!(hidden.is_hidden);
        assert_eq!(hidden.kind, EntryKind::File);

        let visible = entries.iter().find(|e| e.name == "visible.txt").unwrap();
        assert!(!visible.is_hidden);
        assert_eq!(visible.size, 2);

        let subdir = entries.iter().find(|e| e.name == "subdir").unwrap();
        assert_eq!(subdir.kind, EntryKind::Dir);
        assert!(!subdir.is_hidden);
    }

    #[cfg(unix)]
    #[test]
    fn executable_bit_marks_a_file_executable_but_not_a_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("plain.txt");
        fs::write(&plain, b"hi").unwrap();
        let exec_dir = dir.path().join("exec_dir");
        fs::create_dir(&exec_dir).unwrap();
        fs::set_permissions(&exec_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let script_entry = entries.iter().find(|e| e.name == "run.sh").unwrap();
        assert!(script_entry.is_executable);
        assert_eq!(script_entry.unix_mode.unwrap() & 0o777, 0o755);

        let plain_entry = entries.iter().find(|e| e.name == "plain.txt").unwrap();
        assert!(!plain_entry.is_executable);

        // A directory's own `x` bits ("searchable") must never make it
        // count as `executable` for row-coloring purposes.
        let dir_entry = entries.iter().find(|e| e.name == "exec_dir").unwrap();
        assert!(!dir_entry.is_executable);
    }

    #[cfg(windows)]
    #[test]
    fn executable_extension_marks_a_file_executable_but_not_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run.exe"), b"hi").unwrap();
        fs::write(dir.path().join("plain.txt"), b"hi").unwrap();
        fs::create_dir(dir.path().join("run.exe.d")).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let exe_entry = entries.iter().find(|e| e.name == "run.exe").unwrap();
        assert!(exe_entry.is_executable);
        let plain_entry = entries.iter().find(|e| e.name == "plain.txt").unwrap();
        assert!(!plain_entry.is_executable);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_its_own_kind_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"hi").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link.txt").unwrap();
        assert_eq!(link_entry.kind, EntryKind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_directory_resolves_target_kind_dir_and_is_dir_like() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real_dir");
        fs::create_dir(&real_dir).unwrap();
        let link = dir.path().join("link_to_dir");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link_to_dir").unwrap();
        assert_eq!(link_entry.kind, EntryKind::Symlink);
        assert_eq!(link_entry.symlink_target, Some(SymlinkTarget::Dir));
        assert!(link_entry.is_dir_like());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_file_resolves_target_kind_file_and_is_not_dir_like() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"hi").unwrap();
        let link = dir.path().join("link_to_file");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link_to_file").unwrap();
        assert_eq!(link_entry.symlink_target, Some(SymlinkTarget::File));
        assert!(!link_entry.is_dir_like());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_resolves_to_no_target_and_is_not_dir_like() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("does_not_exist"), &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "dangling").unwrap();
        assert_eq!(link_entry.kind, EntryKind::Symlink);
        assert_eq!(link_entry.symlink_target, None);
        assert!(!link_entry.is_dir_like());
        assert!(!link_entry.is_executable);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_an_executable_file_is_colored_executable_by_target_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        fs::write(&script, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link_to_script");
        std::os::unix::fs::symlink(&script, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link_to_script").unwrap();
        assert!(
            link_entry.is_executable,
            "a file-symlink's executable color must follow the target's mode, \
             not the link's own (conventionally 777) mode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_non_executable_file_is_not_colored_executable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("plain.txt");
        fs::write(&target, b"hi").unwrap();
        let link = dir.path().join("link_to_plain");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link_to_plain").unwrap();
        assert!(!link_entry.is_executable);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_directory_is_never_colored_executable_even_if_target_dir_is_searchable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real_dir");
        fs::create_dir(&real_dir).unwrap();
        fs::set_permissions(&real_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.path().join("link_to_dir");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let entries = read_dir_entries(dir.path()).unwrap();
        let link_entry = entries.iter().find(|e| e.name == "link_to_dir").unwrap();
        assert!(!link_entry.is_executable);
    }
}
