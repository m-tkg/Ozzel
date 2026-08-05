//! JSON-persisted directory history and bookmarks. Follows the same
//! CLI-tool XDG convention as `config.rs` on macOS/Linux
//! (`$XDG_DATA_HOME/ozzel`, falling back to `~/.local/share/ozzel`) rather
//! than Apple's `~/Library/Application Support`; Windows uses
//! `directories::ProjectDirs`' roaming app-data dir.
//!
//! Loading never hard-fails: a missing file is just an empty default, and
//! a corrupt one falls back to empty too, with a message the caller is
//! expected to log rather than silently swallow. Saving is the caller's
//! responsibility to trigger (see `App::bookmarks_dirty` and the
//! save-on-quit call in `main.rs`) — this module has no autosave timer.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How many directories each pane's history ring remembers.
const HISTORY_CAP: usize = 50;

/// Which pane a history ring belongs to. A small local type (rather than
/// reusing `app::ActivePane`) so this module doesn't depend upward on the
/// app layer; `app.rs` converts between the two at its call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Per-pane MRU ring of visited directories, deduped (a re-visited path
/// moves back to the front rather than appearing twice) and capped at
/// `HISTORY_CAP`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub left: Vec<PathBuf>,
    #[serde(default)]
    pub right: Vec<PathBuf>,
}

impl History {
    pub fn ring(&self, side: Side) -> &[PathBuf] {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// Records a visit to `path`, moving it to the front if it was already
    /// present, and trimming the ring back down to `HISTORY_CAP`.
    pub fn record(&mut self, side: Side, path: PathBuf) {
        let ring = match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        };
        ring.retain(|p| p != &path);
        ring.insert(0, path);
        ring.truncate(HISTORY_CAP);
    }
}

/// An ordered, deduplicated list of bookmarked directories.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmarks {
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}

impl Bookmarks {
    /// Adds `path` if it isn't already bookmarked. Returns whether it was
    /// actually added (`false` means it was already there).
    pub fn add(&mut self, path: PathBuf) -> bool {
        if self.paths.contains(&path) {
            false
        } else {
            self.paths.push(path);
            true
        }
    }

    /// Removes every occurrence of `path`. Returns whether anything was
    /// removed.
    pub fn remove_path(&mut self, path: &Path) -> bool {
        let before = self.paths.len();
        self.paths.retain(|p| p != path);
        self.paths.len() != before
    }
}

/// Path to `history.json`, if this platform has a resolvable data dir.
/// Private — only `load_history`/`save_history` in this module use it.
fn history_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("history.json"))
}

/// Path to `bookmarks.json`, if this platform has a resolvable data dir.
/// Private — only `load_bookmarks`/`save_bookmarks` in this module use it.
fn bookmarks_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("bookmarks.json"))
}

fn data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::xdg::windows_project_dirs().map(|dirs| dirs.data_dir().to_path_buf())
    }
    #[cfg(not(windows))]
    {
        let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let home_dir = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
        unix_data_dir(xdg_data_home, home_dir)
    }
}

/// `persist.rs`'s own thin wrapper around the resolution logic shared with
/// `config::unix_config_path` (see `crate::xdg::unix_ozzel_dir`'s doc
/// comment): `$XDG_DATA_HOME` when set to an absolute path, otherwise
/// `~/.local/share`, joined with `ozzel` — unlike `config.rs`'s wrapper,
/// there's no further filename to join here, since `history_path`/
/// `bookmarks_path` each append their own. A standalone function so tests
/// can exercise the fallback logic without mutating process-global
/// environment state.
#[cfg_attr(
    windows,
    allow(dead_code, reason = "only used on the unix data_dir path")
)]
fn unix_data_dir(xdg_data_home: Option<PathBuf>, home_dir: Option<PathBuf>) -> Option<PathBuf> {
    crate::xdg::unix_ozzel_dir(xdg_data_home, home_dir, ".local/share")
}

pub fn load_history() -> (History, Option<String>) {
    match history_path() {
        Some(path) => load_json(&path),
        None => (History::default(), None),
    }
}

pub fn load_bookmarks() -> (Bookmarks, Option<String>) {
    match bookmarks_path() {
        Some(path) => load_json(&path),
        None => (Bookmarks::default(), None),
    }
}

pub fn save_history(history: &History) -> Result<()> {
    let path = history_path().context("no data directory available")?;
    save_json(&path, history)
}

pub fn save_bookmarks(bookmarks: &Bookmarks) -> Result<()> {
    let path = bookmarks_path().context("no data directory available")?;
    save_json(&path, bookmarks)
}

/// Loads and parses `path` as JSON, falling back to `T::default()` (with an
/// explanatory message) if the file is missing, unreadable, or malformed.
/// Missing is silent (`None`); unreadable/malformed carries a message the
/// caller should log — a corrupt file is different from "never created
/// one yet" and worth telling the user about.
fn load_json<T: Default + serde::de::DeserializeOwned>(path: &Path) -> (T, Option<String>) {
    if !path.exists() {
        return (T::default(), None);
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            return (
                T::default(),
                Some(format!("failed to read {}: {err}", path.display())),
            );
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => (value, None),
        Err(err) => (
            T::default(),
            Some(format!(
                "failed to parse {}: {err} (using empty defaults)",
                path.display()
            )),
        ),
    }
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_record_dedups_and_moves_to_front() {
        let mut history = History::default();
        history.record(Side::Left, PathBuf::from("/a"));
        history.record(Side::Left, PathBuf::from("/b"));
        history.record(Side::Left, PathBuf::from("/a")); // re-visit
        assert_eq!(
            history.ring(Side::Left),
            &[PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[test]
    fn history_record_caps_ring_length() {
        let mut history = History::default();
        for i in 0..(HISTORY_CAP + 10) {
            history.record(Side::Left, PathBuf::from(format!("/dir{i}")));
        }
        assert_eq!(history.ring(Side::Left).len(), HISTORY_CAP);
        // Most-recent-first: the very last one recorded is at the front.
        assert_eq!(
            history.ring(Side::Left)[0],
            PathBuf::from(format!("/dir{}", HISTORY_CAP + 9))
        );
    }

    #[test]
    fn history_left_and_right_rings_are_independent() {
        let mut history = History::default();
        history.record(Side::Left, PathBuf::from("/left-only"));
        history.record(Side::Right, PathBuf::from("/right-only"));
        assert_eq!(history.ring(Side::Left), &[PathBuf::from("/left-only")]);
        assert_eq!(history.ring(Side::Right), &[PathBuf::from("/right-only")]);
    }

    #[test]
    fn bookmarks_add_dedups() {
        let mut bookmarks = Bookmarks::default();
        assert!(bookmarks.add(PathBuf::from("/a")));
        assert!(
            !bookmarks.add(PathBuf::from("/a")),
            "adding twice should report no-op"
        );
        assert_eq!(bookmarks.paths, vec![PathBuf::from("/a")]);
    }

    #[test]
    fn bookmarks_remove_path_removes_and_reports() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/a"));
        bookmarks.add(PathBuf::from("/b"));
        assert!(bookmarks.remove_path(Path::new("/a")));
        assert!(!bookmarks.remove_path(Path::new("/a")), "already gone");
        assert_eq!(bookmarks.paths, vec![PathBuf::from("/b")]);
    }

    // `unix_data_dir` is XDG-style resolution meaningful only on unix
    // (Windows paths like "/custom/xdg" aren't absolute, and the join
    // semantics differ), so these tests are unix-only.
    #[test]
    #[cfg(unix)]
    fn unix_data_dir_prefers_absolute_xdg_data_home() {
        let path = unix_data_dir(
            Some(PathBuf::from("/custom/xdg")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(path, Some(PathBuf::from("/custom/xdg/ozzel")));
    }

    #[test]
    #[cfg(unix)]
    fn unix_data_dir_falls_back_to_home_local_share() {
        let path = unix_data_dir(None, Some(PathBuf::from("/home/user")));
        assert_eq!(path, Some(PathBuf::from("/home/user/.local/share/ozzel")));
    }

    #[test]
    #[cfg(unix)]
    fn unix_data_dir_ignores_relative_xdg_data_home() {
        let path = unix_data_dir(
            Some(PathBuf::from("relative")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(path, Some(PathBuf::from("/home/user/.local/share/ozzel")));
    }

    #[test]
    fn persist_round_trips_history_through_a_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");

        let mut history = History::default();
        history.record(Side::Left, PathBuf::from("/projects/ozzel"));
        history.record(Side::Right, PathBuf::from("/tmp/scratch"));
        save_json(&path, &history).unwrap();

        let (loaded, warning): (History, Option<String>) = load_json(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, history);
    }

    #[test]
    fn persist_round_trips_bookmarks_through_a_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");

        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/home/user/projects"));
        save_json(&path, &bookmarks).unwrap();

        let (loaded, warning): (Bookmarks, Option<String>) = load_json(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, bookmarks);
    }

    #[test]
    fn corrupt_json_falls_back_to_empty_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let (loaded, warning): (History, Option<String>) = load_json(&path);
        assert_eq!(loaded, History::default());
        assert!(warning.is_some());
    }

    #[test]
    fn missing_file_is_empty_with_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");

        let (loaded, warning): (History, Option<String>) = load_json(&path);
        assert_eq!(loaded, History::default());
        assert!(warning.is_none(), "a merely-missing file is not an error");
    }

    #[test]
    fn bookmarks_paths_are_a_hash_set_free_ordered_list() {
        // Sanity check that Bookmarks doesn't silently reorder — it's
        // documented as an ordered Vec, not a HashSet.
        let mut bookmarks = Bookmarks::default();
        bookmarks.add(PathBuf::from("/z"));
        bookmarks.add(PathBuf::from("/a"));
        assert_eq!(
            bookmarks.paths,
            vec![PathBuf::from("/z"), PathBuf::from("/a")]
        );
        let as_set: std::collections::HashSet<_> = bookmarks.paths.iter().collect();
        assert_eq!(as_set.len(), 2);
    }
}
