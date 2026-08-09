//! Synchronous `git` CLI invocations — the process-spawning half of git
//! support, kept apart from `crate::git` (pure data types and the
//! porcelain parser, deliberately I/O-free). Both the background
//! `git status` worker (`tasks::git_status`) and the foreground `git diff`
//! action (`App::begin_git_diff`) shell out through `output` here, so the
//! flags every invocation must carry (`--no-optional-locks`,
//! `--no-pager`) live in exactly one place.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

/// Runs `git` with `args` in `dir`, returning stdout only on a zero exit;
/// `None` covers every failure mode alike — `git` missing from `PATH`, not
/// a repository, a non-zero exit.
///
/// `--no-optional-locks` matters more than it looks: without it a plain
/// `git status` rewrites `.git/index` to refresh its stat cache, and since
/// `App` watches the git directory (see `git::GitDirStatus::git_dir`) that
/// write would come straight back as a change event, re-probing forever.
/// `--no-pager` is belt-and-braces: git already skips its pager when
/// stdout isn't a TTY, but a `core.pager` misconfiguration would otherwise
/// be able to hang a spawned child holding the pipe.
pub fn output<I, S>(dir: &Path, args: I) -> Option<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("--no-pager")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The unified diff of `path` (a file *or* a directory, since a directory
/// row's marker aggregates everything under it) as git itself renders it,
/// run with `dir` as the working directory.
///
/// Compares against `HEAD`, not the index: the pane's marker column
/// already folds the index and worktree sides together (see
/// `git::marker_for`), so "what changed since the last commit" is the
/// question the marker actually raises — a staged-then-further-edited file
/// shows both halves in one diff instead of hiding the staged part.
/// A repository with no commits yet has no `HEAD` to name, which fails the
/// whole invocation, so that case falls back to the plain
/// index-vs-worktree diff.
pub fn diff(dir: &Path, path: &Path) -> Result<Vec<u8>, String> {
    let spec = literal_pathspec(dir, path);
    let spec = spec.as_os_str();
    let (diff, no_color, head, sep) = (
        OsStr::new("diff"),
        OsStr::new("--no-color"),
        OsStr::new("HEAD"),
        OsStr::new("--"),
    );
    if let Some(out) = output(dir, [diff, no_color, head, sep, spec]) {
        return Ok(out);
    }
    // Unborn HEAD: drop the revision and diff the index against the
    // worktree instead.
    output(dir, [diff, no_color, sep, spec]).ok_or_else(|| "git diff failed".to_string())
}

/// `path` as a pathspec relative to `dir`, wrapped in `:(literal)` so a
/// name containing `*`, `?`, `[` or a leading `:` is matched verbatim
/// rather than read as a glob. Built as an `OsString` throughout — a
/// non-UTF-8 filename must reach `git` byte-for-byte, which a lossy
/// `String` conversion would break.
///
/// A `path` that somehow isn't under `dir` falls back to its own last
/// component; `App::begin_git_diff` only ever passes the cursor entry of
/// the pane whose `cwd` is `dir`, so that branch is unreachable in
/// practice.
fn literal_pathspec(dir: &Path, path: &Path) -> OsString {
    let rel = path
        .strip_prefix(dir)
        .ok()
        .or_else(|| path.file_name().map(Path::new))
        .unwrap_or(path);
    let mut spec = OsString::from(":(literal)");
    spec.push(rel.as_os_str());
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_pathspec_relativizes_and_prefixes() {
        let spec = literal_pathspec(Path::new("/repo/sub"), Path::new("/repo/sub/a.txt"));
        assert_eq!(spec, OsString::from(":(literal)a.txt"));
    }

    /// The whole reason for `:(literal)`: a glob metacharacter in a real
    /// filename has to survive into the pathspec unescaped, and git is
    /// then told not to interpret it.
    #[test]
    fn literal_pathspec_keeps_glob_metacharacters_verbatim() {
        let spec = literal_pathspec(Path::new("/repo"), Path::new("/repo/a[1].txt"));
        assert_eq!(spec, OsString::from(":(literal)a[1].txt"));
    }

    #[test]
    fn literal_pathspec_outside_the_dir_falls_back_to_the_file_name() {
        let spec = literal_pathspec(Path::new("/repo"), Path::new("/elsewhere/a.txt"));
        assert_eq!(spec, OsString::from(":(literal)a.txt"));
    }

    #[test]
    fn output_of_a_non_repository_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(output(dir.path(), ["rev-parse", "--show-toplevel"]).is_none());
    }
}
