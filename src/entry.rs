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

        entries.push(FsEntry {
            is_hidden: is_hidden_name(&name),
            name,
            path: entry_path,
            kind,
            size: metadata.len(),
            mtime: metadata.modified().ok(),
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
