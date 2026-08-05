//! Filesystem entry model and directory reading.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};

/// What kind of filesystem object an [`FsEntry`] refers to. Symlinks are
/// their own kind (never resolved to their target) so that callers can
/// decide how to treat them without accidentally following a link during
/// browsing, copy, or delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
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
}

/// unix executable detection: any owner/group/other `x` bit set, on a
/// regular file specifically — deliberately narrower than "not a
/// directory" (symlinks are excluded too): `read_dir_entries` captures a
/// symlink's *own* metadata (never the target's, per this module's
/// no-follow policy), and a symlink's mode bits are conventionally `777`
/// on Linux regardless of what they point to, which would make the
/// `executable` color meaningless noise on every symlink rather than a
/// useful signal.
#[cfg(unix)]
fn is_executable_unix(kind: EntryKind, mode: u32) -> bool {
    kind == EntryKind::File && mode & 0o111 != 0
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
/// recurse and does not follow symlinks (a symlinked directory is reported
/// as [`EntryKind::Symlink`], not [`EntryKind::Dir`]).
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

        #[cfg(unix)]
        let (unix_mode, is_executable) = {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode();
            (Some(mode), is_executable_unix(kind, mode))
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
}
