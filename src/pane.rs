//! A single browsable directory pane: current directory, its listing,
//! cursor, sort/filter state, and the memory of where the cursor should
//! land when returning to a directory from one of its children.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::entry::{EntryKind, FsEntry, read_dir_entries};
use crate::filter::FilterSpec;

/// How many rows a PageUp/PageDown jumps. Phase 2+ may make this track the
/// actual rendered viewport height; a fixed constant is enough for MVP
/// browsing.
pub const PAGE_SIZE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    MTime,
    Ext,
}

impl SortKey {
    /// Cycles through sort keys in a fixed order; used by the `s` action.
    pub fn next(self) -> Self {
        match self {
            SortKey::Name => SortKey::Size,
            SortKey::Size => SortKey::MTime,
            SortKey::MTime => SortKey::Ext,
            SortKey::Ext => SortKey::Name,
        }
    }
}

/// One renderable row of a pane's listing: either the synthetic parent
/// (`..`) row or a real filesystem entry.
#[derive(Debug, Clone, Copy)]
pub enum VisibleItem<'a> {
    Parent,
    Entry(&'a FsEntry),
}

pub struct Pane {
    pub cwd: PathBuf,
    pub entries: Vec<FsEntry>,
    pub cursor: usize,
    pub sort: SortKey,
    pub ascending: bool,
    pub show_hidden: bool,
    /// Directory path -> name of the child entry that should be focused
    /// when that directory is (re-)entered. Populated whenever `go_parent`
    /// climbs out of a directory, so pressing Backspace and then looking at
    /// the pane shows the cursor sitting back on the directory just left.
    pub cursor_memory: HashMap<PathBuf, String>,
    /// Paths marked for a bulk copy/move/delete. Survives a plain
    /// `reload()` of the same directory (re-sort, hidden toggle) but is
    /// cleared whenever `cwd` actually changes. Deliberately independent of
    /// `filter`: a mark on an entry that's since been filtered out of view
    /// is still a mark (see `marked_or_cursor`'s doc comment).
    pub marks: HashSet<PathBuf>,
    /// The active incremental filter, if any. Applied in `visible_entries`
    /// alongside the hidden-file filter; cleared on `cwd` change.
    pub filter: Option<FilterSpec>,
}

impl Pane {
    pub fn new(cwd: PathBuf) -> Result<Self> {
        let mut pane = Self {
            cwd,
            entries: Vec::new(),
            cursor: 0,
            sort: SortKey::Name,
            ascending: true,
            show_hidden: false,
            cursor_memory: HashMap::new(),
            marks: HashSet::new(),
            filter: None,
        };
        pane.reload()?;
        Ok(pane)
    }

    /// Re-reads `cwd` from disk, keeping the cursor in bounds.
    pub fn reload(&mut self) -> Result<()> {
        self.entries = read_dir_entries(&self.cwd)?;
        self.clamp_cursor();
        Ok(())
    }

    /// Like `reload`, but tries to keep the cursor on the same-named entry
    /// it was on before the reload (falling back to `reload`'s plain index
    /// clamp when there was no prior selection, or it's gone). Used after
    /// a background task finishes, since the listing may have changed size
    /// or order out from under the cursor.
    pub fn reload_preserving_cursor(&mut self) -> Result<()> {
        let previous = self.selected_entry_name();
        self.reload()?;
        if let Some(name) = previous {
            self.restore_cursor_onto(&name);
        }
        Ok(())
    }

    pub fn is_root(&self) -> bool {
        self.cwd.parent().is_none()
    }

    /// Hidden-file filter + incremental filter + sort (dirs always grouped
    /// before files) with a synthetic `..` row prepended unless `cwd` is
    /// the filesystem root (`..` is never subject to either filter).
    pub fn visible_entries(&self) -> Vec<VisibleItem<'_>> {
        let mut indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.is_hidden)
            .filter(|(_, e)| match &self.filter {
                Some(spec) => spec.matches(&e.name),
                None => true,
            })
            .map(|(i, _)| i)
            .collect();

        indices.sort_by(|&a, &b| {
            compare_entries(
                &self.entries[a],
                &self.entries[b],
                self.sort,
                self.ascending,
            )
        });

        let mut items = Vec::with_capacity(indices.len() + 1);
        if !self.is_root() {
            items.push(VisibleItem::Parent);
        }
        items.extend(
            indices
                .into_iter()
                .map(|i| VisibleItem::Entry(&self.entries[i])),
        );
        items
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.visible_entries().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let new = (self.cursor as isize + delta).clamp(0, len as isize - 1);
        self.cursor = new as usize;
    }

    pub fn cursor_to_top(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_to_bottom(&mut self) {
        let len = self.visible_entries().len();
        self.cursor = len.saturating_sub(1);
    }

    /// Enter acts on whatever is under the cursor: `..` goes to the parent
    /// (see [`Pane::go_parent`]), a directory descends into it, anything
    /// else (file, symlink) is a no-op for now.
    pub fn enter(&mut self) -> Result<()> {
        enum Target {
            Parent,
            Descend(PathBuf),
            None,
        }

        let target = match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Parent) => Target::Parent,
            Some(VisibleItem::Entry(e)) if e.kind == EntryKind::Dir => {
                Target::Descend(e.path.clone())
            }
            _ => Target::None,
        };

        match target {
            Target::Parent => self.go_parent(),
            Target::Descend(path) => self.jump_to(path),
            Target::None => Ok(()),
        }
    }

    /// Changes `cwd` to an arbitrary directory — descending into a
    /// subdirectory via `enter()`, or jumping there from a history/
    /// bookmark/home menu selection. Resets cursor/marks/filter on
    /// success; reverts (and reloads the old `cwd` back) on failure, so a
    /// bad jump target never leaves the pane stuck mid-transition.
    pub fn jump_to(&mut self, path: PathBuf) -> Result<()> {
        let previous = std::mem::replace(&mut self.cwd, path);
        match self.reload() {
            Ok(()) => {
                self.cursor = 0;
                self.marks.clear();
                self.filter = None;
                Ok(())
            }
            Err(err) => {
                self.cwd = previous;
                let _ = self.reload();
                Err(err)
            }
        }
    }

    /// Moves `cwd` up to its parent (no-op at the filesystem root) and
    /// restores the cursor onto the directory just left, remembering that
    /// choice in `cursor_memory` for next time.
    pub fn go_parent(&mut self) -> Result<()> {
        let Some(parent) = self.cwd.parent().map(Path::to_path_buf) else {
            return Ok(());
        };
        let leaving_name = self
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());

        self.cwd = parent;
        self.marks.clear();
        self.filter = None;
        self.reload()?;

        match leaving_name {
            Some(name) => {
                self.cursor_memory.insert(self.cwd.clone(), name.clone());
                self.restore_cursor_onto(&name);
            }
            None => self.cursor = 0,
        }
        Ok(())
    }

    fn restore_cursor_onto(&mut self, name: &str) {
        let idx = self.visible_entries().iter().position(|item| match item {
            VisibleItem::Entry(e) => e.name == name,
            VisibleItem::Parent => false,
        });
        self.cursor = idx.unwrap_or(0);
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
    }

    /// The name of the entry under the cursor, or `None` if the cursor is
    /// on `..` or the pane is empty.
    pub fn selected_entry_name(&self) -> Option<String> {
        match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Entry(e)) => Some(e.name.clone()),
            _ => None,
        }
    }

    /// The full path of the entry under the cursor (file or directory),
    /// or `None` if the cursor is on `..` or the pane is empty. Used by
    /// `OpenDefault`/`OpenEditor`, which act on whatever's selected
    /// regardless of kind.
    pub fn selected_entry_path(&self) -> Option<PathBuf> {
        match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Entry(e)) => Some(e.path.clone()),
            _ => None,
        }
    }

    /// The kind of the entry under the cursor, or `None` for `..`/empty.
    /// Used to decide whether `Enter` should navigate (dirs) or open with
    /// the OS default handler (everything else, dyna-filer style).
    pub fn selected_entry_kind(&self) -> Option<EntryKind> {
        match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Entry(e)) => Some(e.kind),
            _ => None,
        }
    }

    /// Toggles the mark on whatever is under the cursor (a no-op on `..`
    /// or an empty pane) and, per dyna-filer convention, advances the
    /// cursor down one row when a toggle actually happened.
    pub fn toggle_mark_cursor(&mut self) {
        let path = match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Entry(e)) => Some(e.path.clone()),
            _ => None,
        };
        if let Some(path) = path {
            self.flip_mark(path);
            self.move_cursor(1);
        }
    }

    /// Toggles the mark on every currently visible real entry (never
    /// `..`).
    pub fn toggle_mark_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .visible_entries()
            .into_iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.path.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        for path in paths {
            self.flip_mark(path);
        }
    }

    fn flip_mark(&mut self, path: PathBuf) {
        if !self.marks.remove(&path) {
            self.marks.insert(path);
        }
    }

    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    /// The paths an operation (copy/move/delete/zip) should act on: the
    /// marks if any exist, otherwise just whatever is under the cursor.
    /// Never includes the synthetic `..` row.
    ///
    /// Deliberately ignores the active filter: if an entry was marked and
    /// then filtered out of view (rather than being cleared), it's still
    /// part of `self.marks` and still a legitimate target — narrowing what
    /// you can *see* isn't the same as narrowing what you *selected*.
    pub fn marked_or_cursor(&self) -> Vec<PathBuf> {
        if !self.marks.is_empty() {
            return self.marks.iter().cloned().collect();
        }
        match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Entry(e)) => vec![e.path.clone()],
            _ => Vec::new(),
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.clamp_cursor();
    }

    /// Sets (or clears, with `None`) the incremental filter and re-clamps
    /// the cursor, since the visible list may have just shrunk.
    pub fn set_filter(&mut self, filter: Option<FilterSpec>) {
        self.filter = filter;
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        let len = self.visible_entries().len();
        if self.cursor >= len {
            self.cursor = len.saturating_sub(1);
        }
    }
}

/// Sort comparator: directories are always grouped before files/symlinks
/// regardless of `sort`/`ascending`, then the requested key breaks ties,
/// with name as a final deterministic tiebreaker.
fn compare_entries(a: &FsEntry, b: &FsEntry, sort: SortKey, ascending: bool) -> Ordering {
    let a_is_dir = a.kind == EntryKind::Dir;
    let b_is_dir = b.kind == EntryKind::Dir;
    if a_is_dir != b_is_dir {
        return if a_is_dir {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }

    let name_cmp = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
    let ord = match sort {
        SortKey::Name => name_cmp(),
        SortKey::Size => a.size.cmp(&b.size).then_with(name_cmp),
        SortKey::MTime => a.mtime.cmp(&b.mtime).then_with(name_cmp),
        SortKey::Ext => extension_lower(&a.name)
            .cmp(&extension_lower(&b.name))
            .then_with(name_cmp),
    };

    if ascending { ord } else { ord.reverse() }
}

fn extension_lower(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn entry(name: &str, kind: EntryKind, size: u64, mtime_offset_secs: u64) -> FsEntry {
        FsEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            kind,
            size,
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_offset_secs)),
            is_hidden: name.starts_with('.'),
        }
    }

    #[test]
    fn dirs_are_grouped_before_files_for_every_sort_key() {
        let file_a = entry("a.txt", EntryKind::File, 100, 5);
        let dir_z = entry("z_dir", EntryKind::Dir, 0, 1);

        for sort in [SortKey::Name, SortKey::Size, SortKey::MTime, SortKey::Ext] {
            let ord = compare_entries(&dir_z, &file_a, sort, true);
            assert_eq!(
                ord,
                Ordering::Less,
                "dir should sort before file for {sort:?}"
            );
            let ord_desc = compare_entries(&dir_z, &file_a, sort, false);
            assert_eq!(
                ord_desc,
                Ordering::Less,
                "dir should sort before file even when descending, for {sort:?}"
            );
        }
    }

    #[test]
    fn sorts_by_name_case_insensitively() {
        let a = entry("Banana.txt", EntryKind::File, 1, 1);
        let b = entry("apple.txt", EntryKind::File, 1, 1);
        assert_eq!(
            compare_entries(&a, &b, SortKey::Name, true),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_by_size() {
        let small = entry("a.txt", EntryKind::File, 10, 1);
        let big = entry("b.txt", EntryKind::File, 1000, 1);
        assert_eq!(
            compare_entries(&small, &big, SortKey::Size, true),
            Ordering::Less
        );
        assert_eq!(
            compare_entries(&small, &big, SortKey::Size, false),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_by_mtime() {
        let old = entry("a.txt", EntryKind::File, 1, 1);
        let new = entry("b.txt", EntryKind::File, 1, 999);
        assert_eq!(
            compare_entries(&old, &new, SortKey::MTime, true),
            Ordering::Less
        );
    }

    #[test]
    fn sorts_by_extension_then_name() {
        let a = entry("b.rs", EntryKind::File, 1, 1);
        let b = entry("a.txt", EntryKind::File, 1, 1);
        assert_eq!(compare_entries(&a, &b, SortKey::Ext, true), Ordering::Less);
    }

    #[test]
    fn sort_key_cycles_through_all_variants() {
        assert_eq!(SortKey::Name.next(), SortKey::Size);
        assert_eq!(SortKey::Size.next(), SortKey::MTime);
        assert_eq!(SortKey::MTime.next(), SortKey::Ext);
        assert_eq!(SortKey::Ext.next(), SortKey::Name);
    }

    #[test]
    fn cursor_memory_restores_position_on_go_parent() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("aaa")).unwrap();
        fs::create_dir(dir.path().join("bbb")).unwrap();
        fs::create_dir(dir.path().join("ccc")).unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        // Entries sorted by name: .. is absent (would only appear if not
        // root of the walk; tempdir has a real parent, so ".." is index 0),
        // then aaa, bbb, ccc.
        // Move cursor onto "bbb" and descend into it.
        let bbb_idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "bbb"))
            .unwrap();
        pane.cursor = bbb_idx;
        pane.enter().unwrap();
        assert_eq!(pane.cwd, dir.path().join("bbb"));

        pane.go_parent().unwrap();
        assert_eq!(pane.cwd, dir.path());
        match pane.visible_entries().get(pane.cursor) {
            Some(VisibleItem::Entry(e)) => assert_eq!(e.name, "bbb"),
            other => panic!("expected cursor to rest on bbb, got {other:?}"),
        }
        assert_eq!(
            pane.cursor_memory.get(dir.path()).map(String::as_str),
            Some("bbb")
        );
    }

    #[test]
    fn go_parent_at_filesystem_root_is_a_no_op() {
        // Find the actual root of the current platform's filesystem.
        let mut root = PathBuf::from(".").canonicalize().unwrap();
        while let Some(parent) = root.parent() {
            root = parent.to_path_buf();
        }
        let mut pane = Pane::new(root.clone()).unwrap();
        let cwd_before = pane.cwd.clone();
        pane.go_parent().unwrap();
        assert_eq!(pane.cwd, cwd_before);
    }

    #[test]
    fn toggle_hidden_shows_and_hides_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".secret"), b"x").unwrap();
        fs::write(dir.path().join("visible.txt"), b"x").unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        let names_hidden_off: Vec<String> = pane
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        assert!(!names_hidden_off.contains(&".secret".to_string()));

        pane.toggle_hidden();
        let names_hidden_on: Vec<String> = pane
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        assert!(names_hidden_on.contains(&".secret".to_string()));
    }

    fn pane_with_files(names: &[&str]) -> (tempfile::TempDir, Pane) {
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        let pane = Pane::new(dir.path().to_path_buf()).unwrap();
        (dir, pane)
    }

    #[test]
    fn marked_or_cursor_falls_back_to_cursor_when_no_marks() {
        let (_dir, mut pane) = pane_with_files(&["a.txt"]);
        let idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "a.txt"))
            .unwrap();
        pane.cursor = idx;
        let targets = pane.marked_or_cursor();
        assert_eq!(targets, vec![pane.cwd.join("a.txt")]);
    }

    #[test]
    fn marked_or_cursor_never_includes_parent_row() {
        let (_dir, pane) = pane_with_files(&[]);
        // Cursor sits on ".." (the only row) since the dir is empty.
        assert!(pane.marked_or_cursor().is_empty());
    }

    #[test]
    fn marked_or_cursor_prefers_marks_over_cursor() {
        let (_dir, mut pane) = pane_with_files(&["a.txt", "b.txt"]);
        pane.marks.insert(pane.cwd.join("b.txt"));
        // Cursor is on "a.txt" (or ".."), but marks take priority.
        assert_eq!(pane.marked_or_cursor(), vec![pane.cwd.join("b.txt")]);
    }

    #[test]
    fn toggle_mark_cursor_marks_and_advances() {
        let (_dir, mut pane) = pane_with_files(&["a.txt", "b.txt"]);
        let a_idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "a.txt"))
            .unwrap();
        pane.cursor = a_idx;

        pane.toggle_mark_cursor();
        assert!(pane.marks.contains(&pane.cwd.join("a.txt")));
        assert_eq!(pane.cursor, a_idx + 1);

        // Toggling again on the same path (moving back up) unmarks it.
        pane.cursor = a_idx;
        pane.toggle_mark_cursor();
        assert!(!pane.marks.contains(&pane.cwd.join("a.txt")));
    }

    #[test]
    fn toggle_mark_cursor_on_parent_row_is_a_no_op() {
        let (_dir, mut pane) = pane_with_files(&["a.txt"]);
        pane.cursor = 0; // ".." is always first when not at filesystem root
        assert!(matches!(
            pane.visible_entries().first(),
            Some(VisibleItem::Parent)
        ));
        pane.toggle_mark_cursor();
        assert!(pane.marks.is_empty());
        assert_eq!(pane.cursor, 0);
    }

    #[test]
    fn toggle_mark_all_marks_then_unmarks_every_real_entry() {
        let (_dir, mut pane) = pane_with_files(&["a.txt", "b.txt"]);
        pane.toggle_mark_all();
        assert_eq!(pane.marks.len(), 2);
        pane.toggle_mark_all();
        assert!(pane.marks.is_empty());
    }

    #[test]
    fn marks_survive_reload_but_clear_on_cwd_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        pane.marks.insert(dir.path().join("a.txt"));

        pane.reload().unwrap();
        assert_eq!(pane.marks.len(), 1, "plain reload must not clear marks");

        let sub_idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "sub"))
            .unwrap();
        pane.cursor = sub_idx;
        pane.enter().unwrap();
        assert!(pane.marks.is_empty(), "descending must clear marks");

        pane.marks.insert(pane.cwd.join("nonexistent"));
        pane.go_parent().unwrap();
        assert!(pane.marks.is_empty(), "go_parent must clear marks");
    }

    #[test]
    fn set_filter_narrows_visible_entries() {
        let (_dir, mut pane) = pane_with_files(&["report.txt", "summary.txt", "notes.md"]);
        pane.set_filter(FilterSpec::parse("report"));

        let names: Vec<String> = pane
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        assert_eq!(names, vec!["report.txt".to_string()]);
    }

    #[test]
    fn set_filter_matches_japanese_substrings() {
        let (_dir, mut pane) = pane_with_files(&["日本語ファイル.txt", "english.txt"]);
        pane.set_filter(FilterSpec::parse("日本語"));

        let names: Vec<String> = pane
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        assert_eq!(names, vec!["日本語ファイル.txt".to_string()]);
    }

    #[test]
    fn parent_row_is_never_filtered_out() {
        let (_dir, mut pane) = pane_with_files(&["a.txt"]);
        pane.set_filter(FilterSpec::parse("nothing-matches-this"));
        assert!(matches!(
            pane.visible_entries().first(),
            Some(VisibleItem::Parent)
        ));
    }

    #[test]
    fn set_filter_clamps_cursor_when_the_visible_list_shrinks() {
        let (_dir, mut pane) = pane_with_files(&["a.txt", "b.txt", "c.txt"]);
        // Move to the last row (c.txt).
        pane.cursor_to_bottom();
        let bottom = pane.cursor;
        assert!(bottom > 0);

        // Filter down to a single match; cursor must not point past the
        // end of the now-much-shorter list.
        pane.set_filter(FilterSpec::parse("a.txt"));
        assert!(pane.cursor < pane.visible_entries().len());
    }

    #[test]
    fn filter_is_cleared_on_cwd_change() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        // A filter that still matches "sub" itself, so it stays reachable.
        pane.set_filter(FilterSpec::parse("sub"));
        assert!(pane.filter.is_some());

        let sub_idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "sub"))
            .unwrap();
        pane.cursor = sub_idx;

        pane.enter().unwrap();
        assert_eq!(pane.cwd, dir.path().join("sub"));
        assert!(pane.filter.is_none(), "descending must clear the filter");

        pane.set_filter(FilterSpec::parse("anything"));
        pane.go_parent().unwrap();
        assert!(pane.filter.is_none(), "go_parent must clear the filter");
    }

    #[test]
    fn marked_or_cursor_still_returns_marks_hidden_by_the_active_filter() {
        // Documents the deliberate choice: filtering narrows what's
        // *visible*, not what's *selected*. A mark made before filtering
        // (or on an entry the current filter now hides) still counts.
        let (_dir, mut pane) = pane_with_files(&["report.txt", "summary.txt"]);
        pane.marks.insert(pane.cwd.join("summary.txt"));

        pane.set_filter(FilterSpec::parse("report"));
        // "summary.txt" is no longer visible under this filter...
        let visible_names: Vec<String> = pane
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect();
        assert!(!visible_names.contains(&"summary.txt".to_string()));

        // ...but it's still a valid transfer target.
        assert_eq!(pane.marked_or_cursor(), vec![pane.cwd.join("summary.txt")]);
    }
}
