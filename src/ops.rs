//! Synchronous filesystem operations backing the copy/move/delete/rename/
//! mkdir actions. Kept as plain functions taking paths in and a `Result`
//! out (no progress reporting, no cancellation) so Phase 3 can lift them
//! into `tasks/copy_move.rs` / `tasks/delete.rs` with minimal churn.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::DeleteBehavior;

/// Deletes every path in `targets` according to `behavior`. Directories are
/// removed recursively; symlinks are removed as links, never followed.
pub fn delete_paths(targets: &[PathBuf], behavior: DeleteBehavior) -> Result<()> {
    match behavior {
        DeleteBehavior::Trash => trash::delete_all(targets).context("failed to move to trash"),
        DeleteBehavior::Permanent => {
            for target in targets {
                delete_one_permanently(target)?;
            }
            Ok(())
        }
    }
}

fn delete_one_permanently(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat: {}", path.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory: {}", path.display()))
    } else {
        // Covers plain files and symlinks (remove_file unlinks the link
        // itself rather than following it).
        fs::remove_file(path).with_context(|| format!("failed to remove: {}", path.display()))
    }
}

/// Creates `parent/name`. Rejects an empty name and an already-existing
/// target instead of silently clobbering or no-op'ing.
pub fn mkdir(parent: &Path, name: &str) -> Result<()> {
    validate_component(name)?;
    let target = parent.join(name);
    if target.exists() {
        bail!("already exists: {name}");
    }
    fs::create_dir(&target)
        .with_context(|| format!("failed to create directory: {}", target.display()))
}

/// Renames `parent/from` to `parent/to`, rejecting an empty/unchanged name
/// or one containing a path separator (renaming should never move an entry
/// to a different directory).
pub fn rename(parent: &Path, from: &str, to: &str) -> Result<()> {
    validate_component(to)?;
    if to == from {
        bail!("name unchanged");
    }
    let src = parent.join(from);
    let dest = parent.join(to);
    if dest.exists() {
        bail!("already exists: {to}");
    }
    fs::rename(&src, &dest).with_context(|| format!("failed to rename to: {}", dest.display()))
}

fn validate_component(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name cannot be empty");
    }
    if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
        bail!("name cannot contain a path separator");
    }
    if name == "." || name == ".." {
        bail!("invalid name: {name}");
    }
    Ok(())
}

/// Which existing destination paths a transfer would overwrite.
pub fn find_collisions(sources: &[PathBuf], dest_dir: &Path) -> Vec<PathBuf> {
    sources
        .iter()
        .filter_map(|src| src.file_name().map(|name| dest_dir.join(name)))
        .filter(|dest| dest.exists())
        .collect()
}

/// Copies `src` (file, dir, or symlink) into `dest_dir`, preserving its
/// base name. Directories are copied recursively; symlinks are recreated
/// as links rather than having their target copied.
pub fn copy_into(src: &Path, dest_dir: &Path) -> Result<()> {
    let name = src
        .file_name()
        .with_context(|| format!("source has no file name: {}", src.display()))?;
    copy_recursive(src, &dest_dir.join(name))
}

fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat: {}", src.display()))?;

    if meta.is_symlink() {
        copy_symlink(src, dest)
    } else if meta.is_dir() {
        fs::create_dir_all(dest)
            .with_context(|| format!("failed to create directory: {}", dest.display()))?;
        for child in
            fs::read_dir(src).with_context(|| format!("failed to read: {}", src.display()))?
        {
            let child = child?;
            copy_recursive(&child.path(), &dest.join(child.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dest)
            .with_context(|| format!("failed to copy to: {}", dest.display()))
            .map(|_| ())
    }
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target =
        fs::read_link(src).with_context(|| format!("failed to read link: {}", src.display()))?;
    std::os::unix::fs::symlink(&target, dest)
        .with_context(|| format!("failed to recreate symlink: {}", dest.display()))
}

#[cfg(windows)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target =
        fs::read_link(src).with_context(|| format!("failed to read link: {}", src.display()))?;
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(&target, dest)
    } else {
        std::os::windows::fs::symlink_file(&target, dest)
    }
    .with_context(|| format!("failed to recreate symlink: {}", dest.display()))
}

#[cfg(not(any(unix, windows)))]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    let target =
        fs::read_link(src).with_context(|| format!("failed to read link: {}", src.display()))?;
    bail!(
        "symlinks are not supported on this platform: {} -> {}",
        dest.display(),
        target.display()
    )
}

/// Moves `src` into `dest_dir`. Tries a plain rename first (atomic and
/// cheap); falls back to copy-then-delete when the rename fails (e.g.
/// `EXDEV` across filesystems or drives), per the plan's known-risk note
/// that such fallback moves are not atomic.
pub fn move_into(src: &Path, dest_dir: &Path) -> Result<()> {
    let name = src
        .file_name()
        .with_context(|| format!("source has no file name: {}", src.display()))?;
    let dest = dest_dir.join(name);

    if fs::rename(src, &dest).is_ok() {
        return Ok(());
    }

    copy_recursive(src, &dest)
        .with_context(|| format!("move fallback: failed to copy to: {}", dest.display()))?;

    let meta =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat: {}", src.display()))?;
    let remove_result = if meta.is_dir() {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    };
    remove_result.with_context(|| {
        format!(
            "copied to {} but failed to remove source: {}",
            dest.display(),
            src.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn mkdir_creates_directory_and_rejects_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        mkdir(dir.path(), "newdir").unwrap();
        assert!(dir.path().join("newdir").is_dir());
        assert!(mkdir(dir.path(), "newdir").is_err());
    }

    #[test]
    fn mkdir_rejects_empty_and_separator_names() {
        let dir = tempfile::tempdir().unwrap();
        assert!(mkdir(dir.path(), "").is_err());
        assert!(mkdir(dir.path(), "a/b").is_err());
    }

    #[test]
    fn rename_moves_within_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("old.txt"), b"hi").unwrap();
        rename(dir.path(), "old.txt", "new.txt").unwrap();
        assert!(!dir.path().join("old.txt").exists());
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn rename_rejects_empty_unchanged_separator_and_collision() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        fs::write(dir.path().join("b.txt"), b"hi").unwrap();
        assert!(rename(dir.path(), "a.txt", "").is_err());
        assert!(rename(dir.path(), "a.txt", "a.txt").is_err());
        assert!(rename(dir.path(), "a.txt", "sub/a.txt").is_err());
        assert!(rename(dir.path(), "a.txt", "b.txt").is_err());
    }

    #[test]
    fn copy_into_copies_file_and_directory_recursively() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();

        fs::write(src_dir.path().join("file.txt"), b"hello").unwrap();
        fs::create_dir(src_dir.path().join("nested")).unwrap();
        fs::write(src_dir.path().join("nested/child.txt"), b"world").unwrap();

        copy_into(&src_dir.path().join("file.txt"), dest_dir.path()).unwrap();
        copy_into(&src_dir.path().join("nested"), dest_dir.path()).unwrap();

        assert_eq!(
            fs::read(dest_dir.path().join("file.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            fs::read(dest_dir.path().join("nested/child.txt")).unwrap(),
            b"world"
        );
        // Source is untouched by a copy.
        assert!(src_dir.path().join("file.txt").exists());
    }

    #[test]
    fn move_into_relocates_and_removes_source() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        fs::write(src_dir.path().join("file.txt"), b"hello").unwrap();

        move_into(&src_dir.path().join("file.txt"), dest_dir.path()).unwrap();

        assert!(!src_dir.path().join("file.txt").exists());
        assert_eq!(
            fs::read(dest_dir.path().join("file.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn find_collisions_reports_existing_destinations_only() {
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
    fn delete_paths_permanent_removes_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), b"hi").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/child.txt"), b"hi").unwrap();

        let targets = vec![dir.path().join("file.txt"), dir.path().join("sub")];
        delete_paths(&targets, DeleteBehavior::Permanent).unwrap();

        assert!(!dir.path().join("file.txt").exists());
        assert!(!dir.path().join("sub").exists());
    }
}
