//! Synchronous filesystem operations for the two metadata-only actions:
//! mkdir and rename. Both are near-instant (no bytes to move), so unlike
//! copy/move/delete they don't need a background task or progress
//! reporting — see `tasks/copy_move.rs` and `tasks/delete.rs` for those.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

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
}
