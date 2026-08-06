//! Recursive file-*name* search (`Mode::FileSearch`, `g`): walks the active
//! pane's cwd once into an in-memory snapshot (`FileSearchTree`), then every
//! query edit re-matches against that snapshot without touching the disk
//! again — which is what makes incremental (per-keystroke) search affordable
//! on large trees. Matching reuses `FilterSpec`, so the query grammar is
//! exactly the pane filter's: case-insensitive substring by default, `re:`
//! prefix for a case-sensitive regex. Distinct from `crate::search`, which
//! is the viewer/help/log views' `less`-style *text* search.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::entry::is_hidden_name;
use crate::filter::FilterSpec;

/// Hard cap on how many entries one snapshot may hold, so opening the
/// search at a filesystem root (or on a runaway tree) stays bounded in
/// both walk time and memory. Hitting it sets `FileSearchTree::truncated`,
/// which the popup surfaces in its title.
pub const MAX_TREE_ENTRIES: usize = 100_000;

/// One file or directory found under the search root.
#[derive(Debug)]
pub struct TreeEntry {
    /// Absolute path (root joined with the walked relative path).
    pub path: PathBuf,
    /// Final component only — what `Pane::restore_cursor_onto` matches on
    /// after the post-selection jump.
    pub name: String,
    /// `name.to_lowercase()`, precomputed once per entry so substring
    /// matching never re-lowercases per keystroke — same trick as
    /// `FsEntry::name_lower`.
    pub name_lower: String,
    /// Path relative to the search root, as one display string — what the
    /// result list shows.
    pub display: String,
    pub is_dir: bool,
}

/// The one-shot snapshot of everything under the search root, taken when
/// the popup opens. Immutable for the popup's lifetime; queries only ever
/// index into `entries`.
#[derive(Debug)]
pub struct FileSearchTree {
    pub root: PathBuf,
    pub entries: Vec<TreeEntry>,
    /// The walk hit `max_entries` and stopped early — results may be
    /// incomplete.
    pub truncated: bool,
}

/// Walks `root` recursively (depth-first, `min_depth(1)` so the root
/// itself is not an entry, symlinks not followed) into a snapshot.
/// `include_hidden=false` mirrors the pane's own hidden-file rule: dotfile
/// entries are skipped and hidden *directories* are never descended into.
/// Unreadable entries (permission errors, races) are silently skipped —
/// a partial snapshot beats no search at all.
pub fn collect_tree(root: &Path, include_hidden: bool, max_entries: usize) -> FileSearchTree {
    let mut entries = Vec::new();
    let mut truncated = false;
    let walker = WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_entry(|e| include_hidden || !is_hidden_name(&e.file_name().to_string_lossy()));
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if entries.len() >= max_entries {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let display = entry
            .path()
            .strip_prefix(root)
            .expect("walkdir entries are always under the root they were walked from")
            .to_string_lossy()
            .into_owned();
        entries.push(TreeEntry {
            path: entry.path().to_path_buf(),
            name_lower: name.to_lowercase(),
            name,
            display,
            is_dir: entry.file_type().is_dir(),
        });
    }
    FileSearchTree {
        root: root.to_path_buf(),
        entries,
        truncated,
    }
}

/// Indices into `tree.entries` whose *name* (final component, not the full
/// relative path) matches `spec` — `None` (empty query) matches everything,
/// the same "empty input means no filter" convention `FilterSpec::parse`
/// itself has.
pub fn search(tree: &FileSearchTree, spec: Option<&FilterSpec>) -> Vec<usize> {
    match spec {
        None => (0..tree.entries.len()).collect(),
        Some(spec) => tree
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| spec.matches(&e.name, &e.name_lower))
            .map(|(idx, _)| idx)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::write(path, b"").unwrap();
    }

    fn make_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("alpha.txt"));
        touch(&root.join(".hidden_file"));
        std::fs::create_dir(root.join("sub")).unwrap();
        touch(&root.join("sub").join("Beta.rs"));
        std::fs::create_dir(root.join(".hidden_dir")).unwrap();
        touch(&root.join(".hidden_dir").join("inside.txt"));
        dir
    }

    fn names(tree: &FileSearchTree) -> Vec<&str> {
        let mut names: Vec<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn collect_tree_recurses_and_excludes_the_root_itself() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), true, MAX_TREE_ENTRIES);
        assert_eq!(
            names(&tree),
            [
                ".hidden_dir",
                ".hidden_file",
                "Beta.rs",
                "alpha.txt",
                "inside.txt",
                "sub"
            ]
        );
        assert!(!tree.truncated);
    }

    #[test]
    fn collect_tree_skips_hidden_entries_and_never_descends_into_hidden_dirs() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), false, MAX_TREE_ENTRIES);
        // `.hidden_dir` is pruned entirely, so `inside.txt` must not leak
        // through even though it isn't itself a dotfile.
        assert_eq!(names(&tree), ["Beta.rs", "alpha.txt", "sub"]);
    }

    #[test]
    fn collect_tree_truncates_at_max_entries() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), true, 2);
        assert_eq!(tree.entries.len(), 2);
        assert!(tree.truncated);
    }

    #[test]
    fn display_is_relative_to_the_root() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), true, MAX_TREE_ENTRIES);
        let beta = tree.entries.iter().find(|e| e.name == "Beta.rs").unwrap();
        assert_eq!(
            beta.display,
            Path::new("sub").join("Beta.rs").to_string_lossy()
        );
        assert_eq!(beta.path, dir.path().join("sub").join("Beta.rs"));
        assert!(!beta.is_dir);
        assert!(
            tree.entries
                .iter()
                .find(|e| e.name == "sub")
                .unwrap()
                .is_dir
        );
    }

    #[test]
    fn search_substring_is_case_insensitive() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), false, MAX_TREE_ENTRIES);
        let spec = FilterSpec::parse("beta").unwrap();
        let hits = search(&tree, Some(&spec));
        assert_eq!(hits.len(), 1);
        assert_eq!(tree.entries[hits[0]].name, "Beta.rs");
    }

    #[test]
    fn search_re_prefix_is_a_case_sensitive_regex() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), false, MAX_TREE_ENTRIES);
        let spec = FilterSpec::parse(r"re:^B.*\.rs$").unwrap();
        let hits = search(&tree, Some(&spec));
        assert_eq!(hits.len(), 1);
        assert_eq!(tree.entries[hits[0]].name, "Beta.rs");
        // Case-sensitive: a lowercase `b` anchor must not match `Beta.rs`.
        let spec = FilterSpec::parse(r"re:^b").unwrap();
        assert!(search(&tree, Some(&spec)).is_empty());
    }

    #[test]
    fn search_invalid_regex_matches_nothing_and_carries_the_error() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), false, MAX_TREE_ENTRIES);
        let spec = FilterSpec::parse("re:[").unwrap();
        assert!(search(&tree, Some(&spec)).is_empty());
        assert!(spec.error().is_some());
    }

    #[test]
    fn search_none_spec_matches_everything() {
        let dir = make_tree();
        let tree = collect_tree(dir.path(), false, MAX_TREE_ENTRIES);
        assert_eq!(search(&tree, None).len(), tree.entries.len());
    }
}
