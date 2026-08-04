//! A single browsable directory pane: current directory, its listing,
//! cursor, sort/filter state, and the memory of where the cursor should
//! land when returning to a directory from one of its children.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::entry::{EntryKind, FsEntry, read_dir_entries};

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

    pub fn is_root(&self) -> bool {
        self.cwd.parent().is_none()
    }

    /// Hidden filter + sort (dirs always grouped before files) with a
    /// synthetic `..` row prepended unless `cwd` is the filesystem root.
    pub fn visible_entries(&self) -> Vec<VisibleItem<'_>> {
        let mut indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.is_hidden)
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
            Target::Descend(path) => self.descend(path),
            Target::None => Ok(()),
        }
    }

    fn descend(&mut self, path: PathBuf) -> Result<()> {
        let previous = std::mem::replace(&mut self.cwd, path);
        match self.reload() {
            Ok(()) => {
                self.cursor = 0;
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

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
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
}
