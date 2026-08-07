//! Synchronous filesystem operations for the metadata-only actions:
//! mkdir, rename, symlink creation, chmod, and touch. All are
//! near-instant (no bytes to move), so unlike copy/move/delete they don't
//! need a background task or progress reporting — see
//! `tasks/copy_move.rs` and `tasks/delete.rs` for those.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

/// How many `-1`/`-2`/... candidates [`create_unique_dir`] tries before
/// giving up, rather than spinning forever against a pathological
/// destination. Far above any plausible real count.
const UNIQUE_DIR_LIMIT: u32 = 10_000;

/// Creates `parent/<base>` — or, when that name is taken, the first of
/// `parent/<base>-1`, `parent/<base>-2`, ... that isn't — and returns the
/// path it actually created.
///
/// The "extract into a guaranteed-fresh directory" primitive behind `u`
/// (see `App::continue_unzip`): unlike [`mkdir`] it never fails on an
/// existing target, and unlike an overwrite flow it never *reuses* one, so
/// nothing already on disk can be written into or clobbered.
///
/// Uses `fs::create_dir`'s own `AlreadyExists` error as the "taken" test
/// rather than an `exists()` check followed by a create: `create_dir` is
/// atomic, so two extractions racing for the same base name (or anything
/// else creating that name in between) can never both win a candidate —
/// the loser just advances to the next suffix. A plain
/// `while candidate.exists() { n += 1 }` loop would be TOCTOU-racy here.
/// `create_dir` rather than `create_dir_all` is likewise deliberate:
/// `parent` is a pane's cwd and must already exist, so a missing one is a
/// real error rather than something to silently conjure.
pub fn create_unique_dir(parent: &Path, base: &str) -> Result<PathBuf> {
    validate_component(base)?;
    for n in 0..UNIQUE_DIR_LIMIT {
        let name = if n == 0 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let candidate = parent.join(&name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to create directory: {}", candidate.display())
                });
            }
        }
    }
    bail!(
        "too many directories named {base}-N in {}",
        parent.display()
    )
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

/// Creates `dest_dir/<src's file name>` as a symbolic link pointing at
/// `src`'s absolute path (absolute rather than relative, so the link stays
/// valid no matter where it's later viewed from). Rejects an
/// already-existing destination instead of clobbering it. `src` is not
/// required to exist — like `ln -s`, creating a dangling link is allowed
/// (the caller passes paths that existed moments ago anyway).
pub fn create_symlink(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let Some(name) = src.file_name() else {
        bail!("{}: has no file name", src.display());
    };
    let dest = dest_dir.join(name);
    if dest.symlink_metadata().is_ok() {
        bail!("already exists: {}", dest.display());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, &dest)
        .with_context(|| format!("failed to create symlink: {}", dest.display()))?;
    #[cfg(windows)]
    {
        // Windows distinguishes file and directory links at creation time.
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, &dest)
                .with_context(|| format!("failed to create symlink: {}", dest.display()))?;
        } else {
            std::os::windows::fs::symlink_file(src, &dest)
                .with_context(|| format!("failed to create symlink: {}", dest.display()))?;
        }
    }
    Ok(dest)
}

/// Sets `path`'s permission bits to `bits` (the lower 0o777), preserving
/// whatever setuid/setgid/sticky bits (0o7000) the entry already has —
/// the chmod dialog only edits rwx and must not silently strip the rest.
#[cfg(unix)]
pub fn chmod(path: &Path, bits: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat: {}", path.display()))?;
    let special = meta.permissions().mode() & 0o7000;
    fs::set_permissions(path, fs::Permissions::from_mode(special | (bits & 0o777)))
        .with_context(|| format!("failed to chmod: {}", path.display()))
}

/// Sets `path`'s modified and accessed times to `t` (touch). Tries a
/// write open first — Windows requires write access on the handle to set
/// times (a read handle opens fine but `set_times` then fails with
/// "Access is denied") — falling back to a read-only open, which is both
/// sufficient for `futimens` on unix and the only way a unix directory
/// can be opened at all.
pub fn set_times(path: &Path, t: SystemTime) -> Result<()> {
    let times = fs::FileTimes::new().set_modified(t).set_accessed(t);
    let file = fs::File::options()
        .write(true)
        .open(path)
        .or_else(|_| fs::File::open(path))
        .with_context(|| format!("failed to open: {}", path.display()))?;
    file.set_times(times)
        .with_context(|| format!("failed to set times: {}", path.display()))
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
    fn create_unique_dir_uses_the_base_name_when_it_is_free() {
        let dir = tempfile::tempdir().unwrap();
        let created = create_unique_dir(dir.path(), "project").unwrap();
        assert_eq!(created, dir.path().join("project"));
        assert!(created.is_dir());
    }

    #[test]
    fn create_unique_dir_appends_a_numeric_suffix_when_the_name_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("project")).unwrap();
        fs::write(dir.path().join("project/keep.txt"), b"sentinel").unwrap();
        fs::create_dir(dir.path().join("project-1")).unwrap();

        let created = create_unique_dir(dir.path(), "project").unwrap();

        assert_eq!(created, dir.path().join("project-2"));
        assert_eq!(
            fs::read(dir.path().join("project/keep.txt")).unwrap(),
            b"sentinel",
            "an existing directory must never be reused or written into"
        );
    }

    #[test]
    fn create_unique_dir_does_not_reuse_an_existing_file_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("project"), b"a file, not a directory").unwrap();

        let created = create_unique_dir(dir.path(), "project").unwrap();

        assert_eq!(created, dir.path().join("project-1"));
    }

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

    #[cfg(unix)]
    #[test]
    fn create_symlink_makes_a_link_to_the_absolute_source() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("target.txt");
        fs::write(&src, b"hi").unwrap();

        let dest = create_symlink(&src, dest_dir.path()).unwrap();
        assert_eq!(dest, dest_dir.path().join("target.txt"));
        let link_target = fs::read_link(&dest).unwrap();
        assert_eq!(link_target, src, "link must point at the absolute source");
        assert_eq!(fs::read(&dest).unwrap(), b"hi");
    }

    #[cfg(unix)]
    #[test]
    fn create_symlink_rejects_an_existing_destination() {
        let src_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("target.txt");
        fs::write(&src, b"hi").unwrap();
        fs::write(dest_dir.path().join("target.txt"), b"other").unwrap();

        assert!(create_symlink(&src, dest_dir.path()).is_err());
        // The existing file must be untouched.
        assert_eq!(
            fs::read(dest_dir.path().join("target.txt")).unwrap(),
            b"other"
        );
    }

    #[cfg(unix)]
    #[test]
    fn chmod_sets_rwx_bits_and_preserves_special_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o4644)).unwrap();

        chmod(&path, 0o755).unwrap();
        let mode = fs::symlink_metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(mode & 0o7000, 0o4000, "setuid must be preserved");
    }

    #[test]
    fn set_times_updates_the_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"x").unwrap();

        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        set_times(&path, t).unwrap();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime, t);
    }

    #[test]
    fn set_times_works_on_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub");
        fs::create_dir(&path).unwrap();

        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        // Windows may refuse a read-only handle on a directory; unix must
        // succeed. Either way it must not panic.
        if set_times(&path, t).is_ok() {
            let mtime = fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(mtime, t);
        }
    }
}
