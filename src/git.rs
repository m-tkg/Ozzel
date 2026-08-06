//! Git status data types and the `git status --porcelain -z` parser —
//! pure data transformation, no process spawning or I/O (that's
//! `tasks::git_status`'s job), so every parsing rule here is unit-testable
//! without a real repository.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One entry's aggregated status marker, in *descending* display priority
/// (`Conflict` wins when a directory aggregates several) — the derived
/// `Ord` encodes exactly that, which `combine` relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitMarker {
    /// Unmerged (either side `U`, or the `DD`/`AA` both-sides cases).
    Conflict,
    Modified,
    Added,
    Deleted,
    Renamed,
    /// `??`.
    Untracked,
}

impl GitMarker {
    /// The single character the pane's git column renders.
    pub fn ch(self) -> char {
        match self {
            GitMarker::Conflict => 'U',
            GitMarker::Modified => 'M',
            GitMarker::Added => 'A',
            GitMarker::Deleted => 'D',
            GitMarker::Renamed => 'R',
            GitMarker::Untracked => '?',
        }
    }

    /// The higher-priority (lower `Ord`) of two markers — how a directory
    /// row aggregates the statuses of everything under it.
    fn combine(self, other: GitMarker) -> GitMarker {
        self.min(other)
    }
}

/// One directory's worth of git status, stamped onto a `Pane` by
/// `App::handle_task_event` when its background `git status` run finishes.
#[derive(Debug, Clone, PartialEq)]
pub struct GitDirStatus {
    /// Current branch name (short hash when detached).
    pub branch: String,
    /// Absolute path (a direct child of the pane's cwd) -> marker.
    /// Statuses deeper in the tree are aggregated up onto the child
    /// directory of the pane's cwd they live under.
    pub statuses: HashMap<PathBuf, GitMarker>,
}

/// Maps one porcelain `XY` pair to a marker, or `None` for pairs that
/// shouldn't render anything (clean/ignored). Worktree side (`y`) wins
/// when both are set — that's the state the user is looking at on disk.
fn marker_for(x: u8, y: u8) -> Option<GitMarker> {
    if x == b'U' || y == b'U' || (x == b'D' && y == b'D') || (x == b'A' && y == b'A') {
        return Some(GitMarker::Conflict);
    }
    if x == b'?' && y == b'?' {
        return Some(GitMarker::Untracked);
    }
    if x == b'!' && y == b'!' {
        return None; // ignored — never requested, but harmless to skip
    }
    let code = if y != b' ' { y } else { x };
    match code {
        b'M' | b'T' => Some(GitMarker::Modified),
        b'A' => Some(GitMarker::Added),
        b'D' => Some(GitMarker::Deleted),
        b'R' | b'C' => Some(GitMarker::Renamed),
        _ => None,
    }
}

/// Parses `git status --porcelain -z` output (v1, NUL-separated, paths
/// relative to `repo_root`) into pane-cwd-child markers: an entry directly
/// in `pane_dir` keeps its own path; anything deeper is aggregated onto
/// the `pane_dir` child directory it lives under (priority per
/// `GitMarker::combine`); anything outside `pane_dir` entirely is dropped.
///
/// The one structural trap in `-z` format: a rename/copy record
/// (`X` ∈ {R, C}) is *two* NUL-separated fields — `XY new\0old\0` — so
/// the original path must be consumed (and ignored) or every following
/// record mis-parses.
pub fn parse_porcelain(
    repo_root: &Path,
    pane_dir: &Path,
    raw: &[u8],
) -> HashMap<PathBuf, GitMarker> {
    let mut statuses: HashMap<PathBuf, GitMarker> = HashMap::new();
    let mut fields = raw.split(|&b| b == 0);

    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue; // empty trailing field, or garbage too short to hold "XY p"
        }
        let (x, y) = (record[0], record[1]);
        // record[2] is the fixed space separator.
        let rel = String::from_utf8_lossy(&record[3..]);
        if x == b'R' || x == b'C' {
            // Consume (and ignore) the rename/copy source path field.
            let _ = fields.next();
        }
        let Some(marker) = marker_for(x, y) else {
            continue;
        };

        let abs = repo_root.join(rel.as_ref());
        let Ok(rel_to_pane) = abs.strip_prefix(pane_dir) else {
            continue; // outside the pane's directory
        };
        let Some(first) = rel_to_pane.components().next() else {
            continue; // the pane directory itself
        };
        let target = pane_dir.join(first);
        statuses
            .entry(target)
            .and_modify(|existing| *existing = existing.combine(marker))
            .or_insert(marker);
    }

    statuses
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(pane: &str, raw: &[u8]) -> HashMap<PathBuf, GitMarker> {
        parse_porcelain(Path::new("/repo"), Path::new(pane), raw)
    }

    #[test]
    fn plain_modified_added_deleted_and_untracked() {
        let raw = b" M a.txt\0A  b.txt\0 D c.txt\0?? d.txt\0";
        let statuses = parse("/repo", raw);
        assert_eq!(statuses[Path::new("/repo/a.txt")], GitMarker::Modified);
        assert_eq!(statuses[Path::new("/repo/b.txt")], GitMarker::Added);
        assert_eq!(statuses[Path::new("/repo/c.txt")], GitMarker::Deleted);
        assert_eq!(statuses[Path::new("/repo/d.txt")], GitMarker::Untracked);
    }

    #[test]
    fn rename_consumes_the_second_field() {
        // `R  new.txt\0old.txt\0` followed by another record: if the old
        // path weren't consumed, `old.txt` would mis-parse as a record and
        // ` M after.txt` would be lost.
        let raw = b"R  new.txt\0old.txt\0 M after.txt\0";
        let statuses = parse("/repo", raw);
        assert_eq!(statuses[Path::new("/repo/new.txt")], GitMarker::Renamed);
        assert_eq!(statuses[Path::new("/repo/after.txt")], GitMarker::Modified);
        assert!(!statuses.contains_key(Path::new("/repo/old.txt")));
    }

    #[test]
    fn conflict_pairs_map_to_conflict() {
        let raw = b"UU both.txt\0DD gone.txt\0AA added.txt\0";
        let statuses = parse("/repo", raw);
        assert_eq!(statuses[Path::new("/repo/both.txt")], GitMarker::Conflict);
        assert_eq!(statuses[Path::new("/repo/gone.txt")], GitMarker::Conflict);
        assert_eq!(statuses[Path::new("/repo/added.txt")], GitMarker::Conflict);
    }

    #[test]
    fn worktree_side_wins_over_index_side() {
        // `MD` = modified in index, deleted in worktree — the on-disk
        // state (deleted) is what the user is looking at.
        let raw = b"MD a.txt\0";
        let statuses = parse("/repo", raw);
        assert_eq!(statuses[Path::new("/repo/a.txt")], GitMarker::Deleted);
    }

    #[test]
    fn deep_paths_aggregate_onto_the_pane_child_with_priority() {
        let raw = b" M sub/inner/a.txt\0?? sub/b.txt\0UU sub/c.txt\0?? other/x.txt\0";
        let statuses = parse("/repo", raw);
        // Conflict beats Modified beats Untracked for the same child dir.
        assert_eq!(statuses[Path::new("/repo/sub")], GitMarker::Conflict);
        assert_eq!(statuses[Path::new("/repo/other")], GitMarker::Untracked);
        assert_eq!(statuses.len(), 2);
    }

    #[test]
    fn pane_dir_below_repo_root_rebases_and_filters() {
        let raw = b" M sub/a.txt\0 M unrelated.txt\0";
        let statuses = parse("/repo/sub", raw);
        assert_eq!(statuses[Path::new("/repo/sub/a.txt")], GitMarker::Modified);
        assert_eq!(
            statuses.len(),
            1,
            "entries outside the pane dir are dropped"
        );
    }

    #[test]
    fn untracked_directory_entry_keeps_its_own_row() {
        // git reports an entirely-untracked directory as one `dir/` entry.
        let raw = b"?? newdir/\0";
        let statuses = parse("/repo", raw);
        assert_eq!(statuses[Path::new("/repo/newdir")], GitMarker::Untracked);
    }

    #[test]
    fn non_utf8_paths_parse_lossily_without_panicking() {
        let mut raw: Vec<u8> = b" M ".to_vec();
        raw.extend_from_slice(&[0xff, 0xfe, b'.', b't', b'x', b't']);
        raw.push(0);
        let statuses = parse("/repo", &raw);
        assert_eq!(statuses.len(), 1);
    }

    #[test]
    fn empty_output_yields_no_statuses() {
        assert!(parse("/repo", b"").is_empty());
    }
}
