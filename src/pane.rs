//! A single browsable directory pane: current directory, its listing,
//! cursor, sort/filter state, and the memory of where the cursor should
//! land when returning to a directory from one of its children.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::Result;

use crate::entry::{EntryKind, FsEntry, read_dir_entries};
use crate::filter::FilterSpec;
use crate::virtual_dir::{self, VirtualDir};

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

    /// Stable string form — the value persisted in `sort_prefs.json` and
    /// shown in the pane header's sort tag. `from_str` is its inverse;
    /// unknown strings (a hand-edited or future-version prefs file) map to
    /// `None` so a stale pref is ignored rather than crashing.
    pub fn as_str(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::MTime => "mtime",
            SortKey::Ext => "ext",
        }
    }

    // Named to mirror `as_str` above; not an actual `FromStr` impl (that
    // trait's `Err` type would add ceremony for a lookup whose only
    // failure mode is "unknown, ignore it") — same call `mode.rs`'s
    // `LineEditor::from_str` already makes.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "name" => Some(SortKey::Name),
            "size" => Some(SortKey::Size),
            "mtime" => Some(SortKey::MTime),
            "ext" => Some(SortKey::Ext),
            _ => None,
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

/// Where to restore a pane's cursor after a delete's post-completion
/// reload — see `Pane::anchor_above`/`reload_preserving_cursor_onto`. A
/// plain `String` name isn't quite enough on its own since the row
/// immediately above the topmost deleted entry might be the synthetic
/// `..` row, which `restore_cursor_onto`'s name-matching deliberately
/// never matches (there's no `FsEntry` to match a name against).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorAnchor {
    Parent,
    Entry(String),
}

pub struct Pane {
    pub cwd: PathBuf,
    pub entries: Vec<FsEntry>,
    pub cursor: usize,
    pub sort: SortKey,
    pub ascending: bool,
    /// Whether name comparisons treat digit runs as numbers (`file2` <
    /// `file10`). Mirrors `Config::natural_sort`; pushed onto both panes by
    /// `App::apply_natural_sort` on startup, config reload, and settings
    /// edits (the pane itself never reads config).
    pub natural_sort: bool,
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
    /// Browser-style "back" stack: every `cwd` this pane has left behind,
    /// oldest first. Pushed by `App::record_history_if_changed` (every
    /// cwd-changing action funnels through it), popped by `history_back`
    /// (`S-left`). Per-pane and entirely in-memory — distinct from the
    /// persisted, cross-session `History` MRU list `HistoryJump` (`S-h`)
    /// shows.
    pub back: Vec<PathBuf>,
    /// Browser-style "forward" stack, the mirror of `back` — popped by
    /// `history_forward` (`S-right`), pushed by `history_back`. Cleared on
    /// every *new* cwd change (going somewhere new invalidates "forward",
    /// same as a real browser).
    pub forward: Vec<PathBuf>,
    /// Computed sizes from the `z` (calc_dir_size) task, keyed by the
    /// directory's full path. Only entries whose parent is the *current*
    /// `cwd` are ever stored (a result arriving after the pane moved away
    /// is dropped), and the map is cleared on every cwd change
    /// (`jump_to`/`go_parent`/`enter_virtual`) — "computed size" is a
    /// property of this visit, not of the directory forever. `reload()`
    /// re-applies the map onto the fresh entries so a background task's
    /// completion reload doesn't wipe the numbers off screen.
    pub dir_size_overrides: HashMap<PathBuf, u64>,
    /// Free space on the filesystem holding `cwd`, refreshed by every
    /// `reload()` (a cheap statvfs-class syscall) — `None` only when the
    /// lookup itself failed. Shown in the pane header's second row
    /// (`ui::pane_view`); while browsing an archive it keeps reporting
    /// the real directory holding the archive, which is also what an
    /// extraction would land on.
    pub free_bytes: Option<u64>,
    /// This directory's git status (branch + per-child markers), stamped
    /// here by `App::handle_task_event` when a background `git status`
    /// run for the *current* `cwd` finishes; `None` outside a git work
    /// tree, while a refresh is still in flight for a brand-new cwd, or
    /// with `show_git_status` off. Cleared on every cwd change (same
    /// sites as `dir_size_overrides`) but deliberately *not* by
    /// `reload()` — the stale-but-close status stays on screen until the
    /// refresh `App::maybe_refresh_git` kicks off replaces it, instead of
    /// the column flickering off and back on.
    pub git: Option<crate::git::GitDirStatus>,
    /// `Some` when this pane is browsing inside a `.zip` archive as a
    /// Virtual Directory instead of a real directory — see
    /// `virtual_dir`'s module doc comment for
    /// the overall design. `cwd` is deliberately left pointing at the real
    /// directory containing the archive the whole time a pane is virtual
    /// (nothing about entering/navigating/leaving one ever changes it),
    /// which is what makes every other cwd-keyed mechanism (history,
    /// `App::navigate`, `:`'s cwd) keep working unmodified.
    pub virtual_dir: Option<VirtualDir>,
    /// Cache of `visible_entries()`'s sorted+filtered index list into
    /// `entries` (an `Rc` so a cache hit is a refcount bump, not a `Vec`
    /// clone). `None` means "stale, recompute on next call".
    ///
    /// Invalidated *explicitly* at every mutation site that can change
    /// what's visible or its order — `entries` (`reload`/`enter_virtual`/
    /// `virtual_descend`/`virtual_go_parent`), `filter` (`set_filter`,
    /// and the same directory-changing methods above which also reset
    /// it), `show_hidden` (`toggle_hidden`), and `sort`/`ascending`
    /// (`cycle_sort`; `ascending` currently has no setter) — rather than
    /// via a version counter on those fields: every one of those mutations
    /// already lives in this handful of `Pane` methods, so an explicit
    /// `invalidate_visible_cache()` call at each is exhaustively
    /// auditable, and a version counter would just move the same "did I
    /// remember" risk onto the same call sites without removing it.
    /// `marks` deliberately does *not* invalidate this — marking never
    /// changes what's visible or its order, only how a row is drawn.
    /// `RefCell` rather than a plain field because `visible_entries` must
    /// stay `&self` (it's called from many read-only call sites across the
    /// app); invalidation always happens from a `&mut self` method, so it
    /// uses `RefCell::get_mut` (no runtime borrow check) instead.
    visible_cache: RefCell<Option<Rc<Vec<usize>>>>,
}

impl Pane {
    pub fn new(cwd: PathBuf) -> Result<Self> {
        let mut pane = Self::new_empty(cwd);
        pane.reload()?;
        Ok(pane)
    }

    /// Builds a pane with an empty listing and no I/O at all — the
    /// "not yet loaded" placeholder `App::new_unloaded` uses so the first
    /// frame can be drawn before `reload()` (which may block on a slow
    /// mount or a huge directory) ever runs. Every other piece of state
    /// matches exactly what `Pane::new` sets up before its own `reload()`.
    pub fn new_empty(cwd: PathBuf) -> Self {
        Self {
            cwd,
            entries: Vec::new(),
            cursor: 0,
            sort: SortKey::Name,
            ascending: true,
            natural_sort: true,
            show_hidden: false,
            cursor_memory: HashMap::new(),
            marks: HashSet::new(),
            filter: None,
            back: Vec::new(),
            forward: Vec::new(),
            dir_size_overrides: HashMap::new(),
            free_bytes: None,
            git: None,
            virtual_dir: None,
            visible_cache: RefCell::new(None),
        }
    }

    /// Stamps (or clears) this pane's git status — no cwd validation here;
    /// the caller (`App::handle_task_event`) is the one holding the task-
    /// to-pane bookkeeping that knows whether this result is still current.
    pub fn set_git_status(&mut self, status: Option<crate::git::GitDirStatus>) {
        self.git = status;
    }

    /// Marks the `visible_entries()` cache stale — see `visible_cache`'s
    /// doc comment for exactly which mutations require this.
    fn invalidate_visible_cache(&mut self) {
        *self.visible_cache.get_mut() = None;
    }

    /// True while this pane is browsing inside a `.zip` archive (Virtual
    /// Directory) rather than a real directory. `App` consults this to
    /// gate every action that needs a real filesystem path and doesn't
    /// have its own virtual-mode meaning (rename/mkdir/delete/move/
    /// duplicate/zip/unzip/open_editor/open_default) — see
    /// `App::reject_if_virtual`.
    pub fn is_virtual(&self) -> bool {
        self.virtual_dir.is_some()
    }

    /// Re-reads the current listing, keeping the cursor in bounds: from
    /// disk for a real pane, or from the archive's central directory at
    /// the current inner level for a virtual one.
    pub fn reload(&mut self) -> Result<()> {
        self.entries = match &self.virtual_dir {
            Some(vd) => vd.list(&vd.inner)?,
            None => read_dir_entries(&self.cwd)?,
        };
        self.free_bytes = fs4::available_space(&self.cwd).ok();
        self.apply_dir_size_overrides();
        self.invalidate_visible_cache();
        self.clamp_cursor();
        Ok(())
    }

    /// Re-stamps computed directory sizes (`dir_size_overrides`) onto the
    /// freshly (re-)read `entries` — see the field's doc comment.
    fn apply_dir_size_overrides(&mut self) {
        if self.dir_size_overrides.is_empty() {
            return;
        }
        for e in &mut self.entries {
            if let Some(&size) = self.dir_size_overrides.get(&e.path) {
                e.size = size;
            }
        }
    }

    /// Records one finished directory-size computation. Ignored unless the
    /// path is a direct child of the *current* real directory (the pane may
    /// have navigated away, or into an archive, while the task ran).
    pub fn set_dir_size(&mut self, path: PathBuf, bytes: u64) {
        if self.virtual_dir.is_some() || path.parent() != Some(self.cwd.as_path()) {
            return;
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.path == path) {
            e.size = bytes;
        }
        self.dir_size_overrides.insert(path, bytes);
        // Sizes feed the Size sort key, so the order may have changed.
        self.invalidate_visible_cache();
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

    /// Like `reload_preserving_cursor`, but for the one case where "the
    /// cursor's own pre-reload name" is exactly the wrong anchor: a
    /// delete's post-completion reload, where the cursor was sitting on
    /// one of the now-gone deleted entries, and blindly re-searching for
    /// *that* name would always miss and fall back to index 0 — landing
    /// back at the top of the pane regardless of where the delete
    /// actually happened. `anchor` (from `Pane::anchor_above`, captured
    /// *before* the delete ran) is used instead when present; with `None`
    /// (nothing was above the topmost deleted row to anchor onto), this
    /// falls back to `reload`'s plain clamped-index behavior — the
    /// already-numerically-correct "stays at the top" case.
    pub fn reload_preserving_cursor_onto(&mut self, anchor: Option<CursorAnchor>) -> Result<()> {
        self.reload()?;
        match anchor {
            Some(CursorAnchor::Parent) => self.cursor = 0,
            Some(CursorAnchor::Entry(name)) => self.restore_cursor_onto(&name),
            None => self.clamp_cursor(),
        }
        Ok(())
    }

    /// The cursor position to restore onto after deleting `targets`: the
    /// name of whatever visible row sits immediately above the topmost
    /// (lowest visible-index) entry being deleted — captured *before* the
    /// deletion actually happens (`targets` are still present in
    /// `visible_entries()` at this point), so it survives the reload the
    /// delete triggers once it completes. `None` when there's nothing
    /// above the topmost deleted row (it was already the first visible
    /// row): the caller's fallback in that case is to just clamp the
    /// existing cursor index, not hunt for a name that was never there.
    pub fn anchor_above(&self, targets: &[PathBuf]) -> Option<CursorAnchor> {
        let target_set: HashSet<&PathBuf> = targets.iter().collect();
        let items = self.visible_entries();
        let topmost = items.iter().position(|item| match item {
            VisibleItem::Entry(e) => target_set.contains(&e.path),
            VisibleItem::Parent => false,
        })?;
        if topmost == 0 {
            return None;
        }
        Some(match &items[topmost - 1] {
            VisibleItem::Parent => CursorAnchor::Parent,
            VisibleItem::Entry(e) => CursorAnchor::Entry(e.name.clone()),
        })
    }

    pub fn is_root(&self) -> bool {
        self.cwd.parent().is_none()
    }

    /// Hidden-file filter + incremental filter + sort (dirs always grouped
    /// before files) with a synthetic `..` row prepended unless `cwd` is
    /// the filesystem root (`..` is never subject to either filter).
    pub fn visible_entries(&self) -> Vec<VisibleItem<'_>> {
        let indices = self.visible_indices();

        let mut items = Vec::with_capacity(indices.len() + 1);
        // A virtual pane always shows ".." regardless of the real `cwd`'s
        // root-ness: at the archive root, ".." is what exits back to the
        // real directory (see `virtual_go_parent`), which is always
        // possible even if `cwd` itself happens to be the filesystem
        // root.
        if !self.is_root() || self.virtual_dir.is_some() {
            items.push(VisibleItem::Parent);
        }
        items.extend(
            indices
                .iter()
                .map(|&i| VisibleItem::Entry(&self.entries[i])),
        );
        items
    }

    /// The sorted+filtered indices into `entries` that `visible_entries`
    /// renders (never including the synthetic `..` row, which has no
    /// index) — cached in `visible_cache`, recomputed (filter + full sort)
    /// only on a cache miss.
    fn visible_indices(&self) -> Rc<Vec<usize>> {
        if let Some(cached) = self.visible_cache.borrow().as_ref() {
            return Rc::clone(cached);
        }

        let mut indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.is_hidden)
            .filter(|(_, e)| match &self.filter {
                Some(spec) => spec.matches(&e.name, &e.name_lower),
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
                self.natural_sort,
            )
        });

        let indices = Rc::new(indices);
        *self.visible_cache.borrow_mut() = Some(Rc::clone(&indices));
        indices
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

    /// `move_cursor` with wrap-around at both ends: one step past the last
    /// row lands on the first (the synthetic `..` when present) and vice
    /// versa. Only single-step cursor movement (`CursorUp`/`CursorDown`
    /// with `cursor_wrap = true`) uses this — PageUp/PageDown, Home/End,
    /// and the mouse wheel always clamp, so a page jump or a wheel flick
    /// near an edge never silently teleports to the other end of the list.
    pub fn move_cursor_wrapping(&mut self, delta: isize) {
        let len = self.visible_entries().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        if delta < 0 && self.cursor == 0 {
            self.cursor = len - 1;
        } else if delta > 0 && self.cursor == len - 1 {
            self.cursor = 0;
        } else {
            self.move_cursor(delta);
        }
    }

    pub fn cursor_to_top(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_to_bottom(&mut self) {
        let len = self.visible_entries().len();
        self.cursor = len.saturating_sub(1);
    }

    /// Enter acts on whatever is under the cursor: `..` goes to the parent
    /// (see [`Pane::go_parent`]), a directory — or a symlink resolving to
    /// one, see [`crate::entry::FsEntry::is_dir_like`] — descends into it,
    /// anything else (file, file-symlink, dangling symlink) is a no-op
    /// here (`App::begin_open` handles those before ever calling `enter`).
    /// Descending into a directory-symlink uses the link's own path
    /// verbatim (`e.path`, never canonicalized) as the new `cwd` — so
    /// `/a/link` stays `/a/link`, and `go_parent` naturally lands back on
    /// `/a` afterward, with the cursor restored onto `link` itself.
    pub fn enter(&mut self) -> Result<()> {
        enum Target {
            Parent,
            Descend(PathBuf),
            None,
        }

        let target = match self.visible_entries().get(self.cursor) {
            Some(VisibleItem::Parent) => Target::Parent,
            Some(VisibleItem::Entry(e)) if e.is_dir_like() => Target::Descend(e.path.clone()),
            _ => Target::None,
        };

        match target {
            Target::Parent => self.go_parent(),
            Target::Descend(path) if self.virtual_dir.is_some() => self.virtual_descend(path),
            Target::Descend(path) => self.jump_to(path),
            Target::None => Ok(()),
        }
    }

    /// Enters Virtual Directory mode at `archive_path`'s root: validates
    /// it's actually readable as a zip (a corrupt file or an unsupported
    /// format fails here, before any UI state changes) and, on success,
    /// swaps this pane over to it exactly like a `jump_to` — cursor/marks/
    /// filter reset — except `cwd` is untouched (see the struct's doc
    /// comment on `virtual_dir`).
    pub fn enter_virtual(&mut self, archive_path: PathBuf) -> Result<()> {
        let vd = VirtualDir::new(archive_path);
        let entries = vd.list(Path::new(""))?;
        // Mirrors `go_parent`'s own cursor_memory bookkeeping: recording
        // "the archive's own name" here is what lets `virtual_go_parent`
        // restore the cursor onto the .zip file when it exits back out,
        // via the exact same `restore_cursor_onto` real panes already use.
        self.cursor_memory
            .insert(self.cwd.clone(), vd.archive_name.clone());
        self.virtual_dir = Some(vd);
        self.entries = entries;
        self.cursor = 0;
        self.marks.clear();
        self.filter = None;
        self.dir_size_overrides.clear();
        self.git = None;
        self.invalidate_visible_cache();
        Ok(())
    }

    /// Descends one level into the archive (a directory row was entered).
    fn virtual_descend(&mut self, inner_path: PathBuf) -> Result<()> {
        let Some(vd) = &self.virtual_dir else {
            return Ok(());
        };
        let entries = vd.list(&inner_path)?;
        self.virtual_dir.as_mut().unwrap().inner = inner_path;
        self.entries = entries;
        self.cursor = 0;
        self.marks.clear();
        self.filter = None;
        self.invalidate_visible_cache();
        Ok(())
    }

    /// `..` inside a virtual directory: climbs one archive-internal level
    /// (remembering, and later restoring, the cursor position exactly like
    /// `go_parent` does for real directories — via a synthetic
    /// `cursor_memory` key built from the archive path, since there's no
    /// real directory path to key it on), or — once already at the
    /// archive root — exits Virtual Directory mode entirely, back to the
    /// real directory containing the archive, with the cursor restored
    /// onto the `.zip` file itself.
    fn virtual_go_parent(&mut self) -> Result<()> {
        let Some(vd) = self.virtual_dir.clone() else {
            return Ok(());
        };

        if vd.inner.as_os_str().is_empty() {
            self.virtual_dir = None;
            self.reload()?;
            self.restore_cursor_onto(&vd.archive_name);
            return Ok(());
        }

        let leaving_name = vd
            .inner
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let new_inner = vd.inner.parent().map(Path::to_path_buf).unwrap_or_default();

        let entries = vd.list(&new_inner)?;
        self.virtual_dir.as_mut().unwrap().inner = new_inner.clone();
        self.entries = entries;
        self.marks.clear();
        self.filter = None;
        self.invalidate_visible_cache();

        let memory_key = vd.archive_path.join(virtual_dir::inner_display(&new_inner));
        self.cursor_memory.insert(memory_key, leaving_name.clone());
        self.restore_cursor_onto(&leaving_name);
        Ok(())
    }

    /// Changes `cwd` to an arbitrary directory — descending into a
    /// subdirectory via `enter()`, or jumping there from a history/
    /// bookmark/home menu selection. Resets cursor/marks/filter on
    /// success; reverts (and reloads the old `cwd` back) on failure, so a
    /// bad jump target never leaves the pane stuck mid-transition.
    pub fn jump_to(&mut self, path: PathBuf) -> Result<()> {
        let previous_cwd = std::mem::replace(&mut self.cwd, path);
        // `jump_to` always means "go to this real directory" (bookmarks,
        // home, the history menu, and `S-left`/`S-right` all funnel
        // through it) — so it exits Virtual Directory mode as a side
        // effect whenever it's active, the same way climbing out of the
        // archive root does. Saved rather than just cleared, so a failed
        // jump (bad target) reverts *both* `cwd` and `virtual_dir`
        // together, leaving the pane exactly as it was rather than
        // silently exiting the archive on a jump that never actually
        // happened.
        let previous_virtual = self.virtual_dir.take();
        // Cleared *before* the reload so `apply_dir_size_overrides` never
        // stamps a previous directory's sizes onto same-pathed entries; on
        // failure the old cwd's overrides are simply lost (accepted — the
        // revert already re-reads the listing from disk anyway). Git
        // status follows the same rule (a failed jump just re-fetches).
        self.dir_size_overrides.clear();
        self.git = None;
        match self.reload() {
            Ok(()) => {
                self.cursor = 0;
                self.marks.clear();
                self.filter = None;
                Ok(())
            }
            Err(err) => {
                self.cwd = previous_cwd;
                self.virtual_dir = previous_virtual;
                let _ = self.reload();
                Err(err)
            }
        }
    }

    /// Moves `cwd` up to its parent (no-op at the filesystem root) and
    /// restores the cursor onto the directory just left, remembering that
    /// choice in `cursor_memory` for next time.
    pub fn go_parent(&mut self) -> Result<()> {
        if self.virtual_dir.is_some() {
            return self.virtual_go_parent();
        }
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
        self.dir_size_overrides.clear();
        self.git = None;
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

    pub(crate) fn restore_cursor_onto(&mut self, name: &str) {
        let idx = self.visible_entries().iter().position(|item| match item {
            VisibleItem::Entry(e) => e.name == name,
            VisibleItem::Parent => false,
        });
        self.cursor = idx.unwrap_or(0);
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.invalidate_visible_cache();
    }

    /// Sets both sort fields at once — the sort dialog (`t`) and
    /// per-directory sort restoration both land here. Like `cycle_sort`,
    /// deliberately does not try to keep the cursor on the same entry.
    pub fn set_sort(&mut self, key: SortKey, ascending: bool) {
        self.sort = key;
        self.ascending = ascending;
        self.invalidate_visible_cache();
    }

    /// Pushes the config's `natural_sort` value onto this pane — see the
    /// field's doc comment. Invalidates only on an actual change.
    pub fn set_natural_sort(&mut self, natural: bool) {
        if self.natural_sort != natural {
            self.natural_sort = natural;
            self.invalidate_visible_cache();
        }
    }

    /// The entry under the cursor, or `None` if the cursor is on `..` or
    /// the pane is empty — the single resolution point every `selected_entry_*`
    /// convenience accessor below is a thin wrapper over, and what callers
    /// needing more than one of those fields at once (e.g. `App::begin_open`)
    /// should call directly instead of stacking several accessor calls (each
    /// of which independently walks `visible_entries()`/the cursor).
    pub fn selected_entry(&self) -> Option<&FsEntry> {
        match self.visible_entries_cached_at_cursor() {
            Some(VisibleItem::Entry(e)) => Some(e),
            _ => None,
        }
    }

    /// Resolves `visible_entries().get(self.cursor)` — pulled out purely so
    /// `selected_entry` and the accessors that predate it share one call
    /// site rather than each re-deriving the same lookup.
    fn visible_entries_cached_at_cursor(&self) -> Option<VisibleItem<'_>> {
        self.visible_entries().get(self.cursor).copied()
    }

    /// The name of the entry under the cursor, or `None` if the cursor is
    /// on `..` or the pane is empty.
    pub fn selected_entry_name(&self) -> Option<String> {
        self.selected_entry().map(|e| e.name.clone())
    }

    /// The full path of the entry under the cursor (file or directory),
    /// or `None` if the cursor is on `..` or the pane is empty. Used by
    /// `OpenDefault`/`OpenEditor`, which act on whatever's selected
    /// regardless of kind.
    pub fn selected_entry_path(&self) -> Option<PathBuf> {
        self.selected_entry().map(|e| e.path.clone())
    }

    /// The kind of the entry under the cursor, or `None` for `..`/empty.
    /// Used to decide whether `Enter` should navigate (dirs) or open with
    /// the OS default handler (everything else).
    pub fn selected_entry_kind(&self) -> Option<EntryKind> {
        self.selected_entry().map(|e| e.kind)
    }

    /// Whether the cursor is on something `Enter`/`o` should *navigate*
    /// into rather than open as a file — a real directory, or a symlink
    /// resolving to one (`FsEntry::is_dir_like`). `None` for `..`/empty
    /// (the caller, `App::begin_open`, always treats `..` as navigable
    /// regardless — see `Pane::enter`). Deliberately distinct from
    /// `selected_entry_kind`, which stays the link's own *raw* kind: this
    /// one is purely a navigation/display decision, never consulted by any
    /// file operation (copy/move/delete/duplicate/zip always re-stat with
    /// `fs::symlink_metadata` independently — see `FsEntry::symlink_target`'s
    /// doc comment).
    pub fn selected_entry_is_dir_like(&self) -> Option<bool> {
        self.selected_entry().map(|e| e.is_dir_like())
    }

    /// Indices (into `visible_entries()`) of every real entry whose name
    /// starts with `prefix`, case-insensitive, in display order — `..` is
    /// never a match. Used by prefix-jump search (`\`, `Mode::JumpSearch`)
    /// for both "jump to the first match" (typing) and "cycle to the
    /// next/previous match" (`Down`/`Up` — see `App::handle_jump_search_key`).
    /// An empty `prefix` matches nothing (there's no meaningful "first
    /// entry" search before anything's been typed).
    pub fn jump_matches(&self, prefix: &str) -> Vec<usize> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let prefix_lower = prefix.to_lowercase();
        self.visible_entries()
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match item {
                VisibleItem::Entry(e) if e.name_lower.starts_with(&prefix_lower) => Some(i),
                _ => None,
            })
            .collect()
    }

    /// Toggles the mark on whatever is under the cursor (a no-op on `..`
    /// or an empty pane) and advances the cursor down one row when a
    /// toggle actually happened.
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

    /// Live rubber-band range select for a mouse drag (`App::handle_mouse_left_drag`):
    /// recomputes `self.marks` from scratch as `snapshot` (the marks
    /// exactly as they were before the drag began) with every real entry
    /// in visible-index range `[lo, hi]` toggled relative to it. Called
    /// fresh on *every* `Drag` event with the drag's current range, never
    /// incrementally — which is exactly what makes a row that was swept
    /// over and then retreated out of automatically revert to its
    /// pre-drag state, rather than staying toggled forever.
    pub fn apply_drag_range(&mut self, snapshot: &HashSet<PathBuf>, lo: usize, hi: usize) {
        let range_paths: Vec<PathBuf> = self
            .visible_entries()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= lo && *i <= hi)
            .filter_map(|(_, item)| match item {
                VisibleItem::Entry(e) => Some(e.path.clone()),
                VisibleItem::Parent => None,
            })
            .collect();

        let mut marks = snapshot.clone();
        for path in range_paths {
            if !marks.remove(&path) {
                marks.insert(path);
            }
        }
        self.marks = marks;
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

    /// The *names* of the currently visible marked entries, in display
    /// order — `rename_marks`' worklist. Unlike `marked_or_cursor` (a
    /// `HashSet` iteration with nondeterministic order, and deliberately
    /// filter-blind), a rename sequence walks entries in the order the
    /// user sees them, and a mark hidden by the active filter is *not*
    /// included: renaming things that aren't on screen, one blind prompt
    /// at a time, is a footgun — the caller logs how many were excluded.
    /// Returns `(visible_names, hidden_mark_count)`.
    pub fn marked_names_in_display_order(&self) -> (Vec<String>, usize) {
        let names: Vec<String> = self
            .visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) if self.marks.contains(&e.path) => Some(e.name.clone()),
                _ => None,
            })
            .collect();
        let hidden = self.marks.len().saturating_sub(names.len());
        (names, hidden)
    }

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.invalidate_visible_cache();
        self.clamp_cursor();
    }

    /// Sets (or clears, with `None`) the incremental filter and re-clamps
    /// the cursor, since the visible list may have just shrunk.
    pub fn set_filter(&mut self, filter: Option<FilterSpec>) {
        self.filter = filter;
        self.invalidate_visible_cache();
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        let len = self.visible_entries().len();
        if self.cursor >= len {
            self.cursor = len.saturating_sub(1);
        }
    }
}

/// Sort comparator: directories — and directory-symlinks, see
/// `FsEntry::is_dir_like` — are always grouped before files/file-symlinks
/// regardless of `sort`/`ascending`, then the requested key breaks ties,
/// with name as a final deterministic tiebreaker.
fn compare_entries(
    a: &FsEntry,
    b: &FsEntry,
    sort: SortKey,
    ascending: bool,
    natural: bool,
) -> Ordering {
    let a_is_dir = a.is_dir_like();
    let b_is_dir = b.is_dir_like();
    if a_is_dir != b_is_dir {
        return if a_is_dir {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }

    let name_cmp = || {
        if natural {
            natural_cmp(&a.name_lower, &b.name_lower)
        } else {
            a.name_lower.cmp(&b.name_lower)
        }
    };
    let ord = match sort {
        SortKey::Name => name_cmp(),
        SortKey::Size => a.size.cmp(&b.size).then_with(name_cmp),
        SortKey::MTime => a.mtime.cmp(&b.mtime).then_with(name_cmp),
        SortKey::Ext => a.ext_lower.cmp(&b.ext_lower).then_with(name_cmp),
    };

    if ascending { ord } else { ord.reverse() }
}

/// Digit-as-number ("natural") string comparison: maximal ASCII digit runs
/// compare as numbers (`file2` < `file10`), everything else compares as
/// plain chars. Equal-valued runs with different leading-zero counts (`01`
/// vs `1`) — and, failing that, fully equal-looking strings — fall back to
/// `str::cmp` so the ordering stays total and deterministic. Allocation-free:
/// digit runs are compared by significant-digit length then lexically,
/// never parsed into an integer (no overflow on absurdly long runs).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ab = a.as_bytes();
    let mut bb = b.as_bytes();

    fn split_digits(s: &[u8]) -> (&[u8], &[u8]) {
        let end = s
            .iter()
            .position(|c| !c.is_ascii_digit())
            .unwrap_or(s.len());
        s.split_at(end)
    }
    fn strip_zeros(s: &[u8]) -> &[u8] {
        let start = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
        &s[start..]
    }

    loop {
        match (ab.first(), bb.first()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&ca), Some(&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let (da, rest_a) = split_digits(ab);
                    let (db, rest_b) = split_digits(bb);
                    let sa = strip_zeros(da);
                    let sb = strip_zeros(db);
                    let ord = sa.len().cmp(&sb.len()).then_with(|| sa.cmp(sb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ab = rest_a;
                    bb = rest_b;
                } else {
                    // Byte-wise UTF-8 comparison orders identically to
                    // code-point comparison (UTF-8 preserves it), so no
                    // char decoding is needed here.
                    let ord = ca.cmp(&cb);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ab = &ab[1..];
                    bb = &bb[1..];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime};

    fn entry(name: &str, kind: EntryKind, size: u64, mtime_offset_secs: u64) -> FsEntry {
        let (name_lower, ext_lower) = crate::entry::lower_keys(name);
        FsEntry {
            name: name.to_string(),
            name_lower,
            ext_lower,
            path: PathBuf::from(name),
            kind,
            size,
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_offset_secs)),
            is_hidden: name.starts_with('.'),
            unix_mode: None,
            readonly: false,
            is_executable: false,
            symlink_target: None,
        }
    }

    /// A symlink entry resolving to `target` — see `entry` for the
    /// non-symlink case.
    fn symlink_entry(name: &str, target: crate::entry::SymlinkTarget) -> FsEntry {
        FsEntry {
            symlink_target: Some(target),
            ..entry(name, EntryKind::Symlink, 0, 1)
        }
    }

    #[test]
    fn dirs_are_grouped_before_files_for_every_sort_key() {
        let file_a = entry("a.txt", EntryKind::File, 100, 5);
        let dir_z = entry("z_dir", EntryKind::Dir, 0, 1);

        for sort in [SortKey::Name, SortKey::Size, SortKey::MTime, SortKey::Ext] {
            let ord = compare_entries(&dir_z, &file_a, sort, true, false);
            assert_eq!(
                ord,
                Ordering::Less,
                "dir should sort before file for {sort:?}"
            );
            let ord_desc = compare_entries(&dir_z, &file_a, sort, false, false);
            assert_eq!(
                ord_desc,
                Ordering::Less,
                "dir should sort before file even when descending, for {sort:?}"
            );
        }
    }

    #[test]
    fn a_directory_symlink_sorts_with_real_directories_not_files() {
        let file_a = entry("a.txt", EntryKind::File, 100, 5);
        let link = symlink_entry("z_link", crate::entry::SymlinkTarget::Dir);

        for sort in [SortKey::Name, SortKey::Size, SortKey::MTime, SortKey::Ext] {
            assert_eq!(
                compare_entries(&link, &file_a, sort, true, false),
                Ordering::Less,
                "a directory-symlink should sort before a file for {sort:?}, same as a real directory"
            );
        }
    }

    #[test]
    fn a_file_symlink_sorts_with_files_not_directories() {
        let dir_z = entry("z_dir", EntryKind::Dir, 0, 1);
        let link = symlink_entry("a_link", crate::entry::SymlinkTarget::File);

        assert_eq!(
            compare_entries(&dir_z, &link, SortKey::Name, true, false),
            Ordering::Less,
            "a real directory should still sort before a file-symlink"
        );
    }

    #[test]
    fn sorts_by_name_case_insensitively() {
        let a = entry("Banana.txt", EntryKind::File, 1, 1);
        let b = entry("apple.txt", EntryKind::File, 1, 1);
        assert_eq!(
            compare_entries(&a, &b, SortKey::Name, true, false),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_by_size() {
        let small = entry("a.txt", EntryKind::File, 10, 1);
        let big = entry("b.txt", EntryKind::File, 1000, 1);
        assert_eq!(
            compare_entries(&small, &big, SortKey::Size, true, false),
            Ordering::Less
        );
        assert_eq!(
            compare_entries(&small, &big, SortKey::Size, false, false),
            Ordering::Greater
        );
    }

    #[test]
    fn sorts_by_mtime() {
        let old = entry("a.txt", EntryKind::File, 1, 1);
        let new = entry("b.txt", EntryKind::File, 1, 999);
        assert_eq!(
            compare_entries(&old, &new, SortKey::MTime, true, false),
            Ordering::Less
        );
    }

    #[test]
    fn sorts_by_extension_then_name() {
        let a = entry("b.rs", EntryKind::File, 1, 1);
        let b = entry("a.txt", EntryKind::File, 1, 1);
        assert_eq!(
            compare_entries(&a, &b, SortKey::Ext, true, false),
            Ordering::Less
        );
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

    #[cfg(unix)]
    #[test]
    fn entering_a_directory_symlink_descends_using_the_links_own_path_not_the_target() {
        // /dir/real_target and /dir/link_to_target (-> real_target), plus
        // a file inside the target so the listing after descending is
        // verifiable.
        let dir = tempfile::tempdir().unwrap();
        let real_target = dir.path().join("real_target");
        fs::create_dir(&real_target).unwrap();
        fs::write(real_target.join("inside.txt"), b"hi").unwrap();
        let link = dir.path().join("link_to_target");
        std::os::unix::fs::symlink(&real_target, &link).unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        let link_idx = pane
            .visible_entries()
            .iter()
            .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == "link_to_target"))
            .unwrap();
        pane.cursor = link_idx;
        pane.enter().unwrap();

        // `cwd` is the link's own path verbatim — never canonicalized to
        // `real_target`.
        assert_eq!(pane.cwd, link);
        assert_ne!(pane.cwd, real_target);
        // `fs::read_dir` follows the final symlink automatically, so the
        // listing still shows what's actually inside the target.
        assert!(
            pane.visible_entries()
                .iter()
                .any(|item| matches!(item, VisibleItem::Entry(e) if e.name == "inside.txt")),
            "listing after descending into a directory-symlink must show the target's contents"
        );

        // Backspace naturally returns to the link's own parent — a plain
        // `Path::parent()` on `/dir/link_to_target` is `/dir`, regardless
        // of what the link points to.
        pane.go_parent().unwrap();
        assert_eq!(pane.cwd, dir.path());
        match pane.visible_entries().get(pane.cursor) {
            Some(VisibleItem::Entry(e)) => assert_eq!(e.name, "link_to_target"),
            other => panic!("expected cursor to rest on link_to_target, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn entering_a_dangling_symlink_or_a_file_symlink_does_not_navigate() {
        let dir = tempfile::tempdir().unwrap();
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"hi").unwrap();
        let file_link = dir.path().join("file_link");
        std::os::unix::fs::symlink(&target, &file_link).unwrap();

        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        let cwd_before = pane.cwd.clone();

        for name in ["dangling", "file_link"] {
            let idx = pane
                .visible_entries()
                .iter()
                .position(|item| matches!(item, VisibleItem::Entry(e) if e.name == name))
                .unwrap();
            pane.cursor = idx;
            pane.enter().unwrap();
            assert_eq!(pane.cwd, cwd_before, "{name} must not navigate");
        }
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

    // --- `visible_entries` cache correctness --------------------------
    //
    // These don't (and can't, from outside the module) inspect
    // `visible_cache` directly — they instead prove the *externally
    // observable contract* the cache must never break: two consecutive
    // calls agree while nothing has changed (a cache hit must return the
    // same thing a fresh computation would), and every mutation that's
    // supposed to invalidate it (`reload`, `set_filter`, `toggle_hidden`,
    // `cycle_sort`) is actually reflected on the very next call.

    fn visible_names(pane: &Pane) -> Vec<String> {
        pane.visible_entries()
            .iter()
            .filter_map(|item| match item {
                VisibleItem::Entry(e) => Some(e.name.clone()),
                VisibleItem::Parent => None,
            })
            .collect()
    }

    #[test]
    fn repeated_calls_with_no_mutation_return_the_same_result() {
        let (_dir, pane) = pane_with_files(&["b.txt", "a.txt", "c.txt"]);
        let first = visible_names(&pane);
        let second = visible_names(&pane);
        let third = visible_names(&pane);
        assert_eq!(first, vec!["a.txt", "b.txt", "c.txt"]);
        assert_eq!(first, second, "a cache hit must match a fresh computation");
        assert_eq!(second, third);
    }

    #[test]
    fn cache_reflects_a_reload_that_adds_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        assert_eq!(visible_names(&pane), vec!["a.txt"]);

        // Populate the cache, then change the directory out from under it
        // and reload — a stale cache would still show only "a.txt".
        fs::write(dir.path().join("b.txt"), b"x").unwrap();
        pane.reload().unwrap();
        assert_eq!(visible_names(&pane), vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn cache_reflects_a_filter_change() {
        let (_dir, mut pane) = pane_with_files(&["report.txt", "summary.txt"]);
        assert_eq!(visible_names(&pane), vec!["report.txt", "summary.txt"]);

        pane.set_filter(FilterSpec::parse("report"));
        assert_eq!(visible_names(&pane), vec!["report.txt"]);

        pane.set_filter(None);
        assert_eq!(visible_names(&pane), vec!["report.txt", "summary.txt"]);
    }

    #[test]
    fn cache_reflects_a_hidden_toggle() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".secret"), b"x").unwrap();
        fs::write(dir.path().join("visible.txt"), b"x").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();

        assert_eq!(visible_names(&pane), vec!["visible.txt"]);
        pane.toggle_hidden();
        assert_eq!(visible_names(&pane), vec![".secret", "visible.txt"]);
        pane.toggle_hidden();
        assert_eq!(visible_names(&pane), vec!["visible.txt"]);
    }

    #[test]
    fn cache_reflects_a_sort_change() {
        // Named so name-order and size-order disagree: name-ascending is
        // "a.txt, z.txt", but "a.txt" is the bigger file, so size-ascending
        // must flip to "z.txt, a.txt" — a stale cache would still show the
        // name-sorted order.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), vec![0u8; 1000]).unwrap();
        fs::write(dir.path().join("z.txt"), b"1").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();

        assert_eq!(visible_names(&pane), vec!["a.txt", "z.txt"]);
        pane.cycle_sort(); // Name -> Size
        assert_eq!(pane.sort, SortKey::Size);
        assert_eq!(visible_names(&pane), vec!["z.txt", "a.txt"]);
    }

    #[test]
    fn natural_cmp_orders_digit_runs_as_numbers() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("file10", "file2"), Ordering::Greater);
        assert_eq!(natural_cmp("file2", "file2"), Ordering::Equal);
        // Leading zeros: equal value falls back to plain string order for
        // a deterministic total order.
        assert_eq!(natural_cmp("file01", "file1"), Ordering::Less);
        assert_eq!(natural_cmp("a1b2", "a1b10"), Ordering::Less);
        // No digits at all behaves like plain cmp.
        assert_eq!(natural_cmp("abc", "abd"), Ordering::Less);
        // Digit run vs non-digit at the same position: plain byte order.
        assert_eq!(natural_cmp("a1", "aa"), Ordering::Less);
        // Prefix relationship.
        assert_eq!(natural_cmp("file", "file2"), Ordering::Less);
        // Huge runs that would overflow an integer parse still compare.
        assert_eq!(
            natural_cmp("x99999999999999999999998", "x99999999999999999999999"),
            Ordering::Less
        );
    }

    #[test]
    fn natural_sort_flag_switches_name_ordering() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["file1.txt", "file2.txt", "file10.txt"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        assert!(pane.natural_sort, "natural sort is the default");
        assert_eq!(
            visible_names(&pane),
            vec!["file1.txt", "file2.txt", "file10.txt"]
        );

        pane.set_natural_sort(false);
        assert_eq!(
            visible_names(&pane),
            vec!["file1.txt", "file10.txt", "file2.txt"],
            "lexicographic order when disabled"
        );
    }

    #[test]
    fn set_sort_applies_key_and_direction() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), vec![0u8; 1000]).unwrap();
        fs::write(dir.path().join("z.txt"), b"1").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();

        pane.set_sort(SortKey::Size, false);
        assert_eq!(pane.sort, SortKey::Size);
        assert!(!pane.ascending);
        assert_eq!(visible_names(&pane), vec!["a.txt", "z.txt"]);

        pane.set_sort(SortKey::Size, true);
        assert_eq!(visible_names(&pane), vec!["z.txt", "a.txt"]);
    }

    #[test]
    fn move_cursor_wrapping_wraps_at_both_ends() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"x").unwrap();
        fs::write(dir.path().join("b.txt"), b"x").unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        let len = pane.visible_entries().len(); // "..", a, b

        pane.cursor = 0;
        pane.move_cursor_wrapping(-1);
        assert_eq!(pane.cursor, len - 1, "up from the top wraps to the bottom");
        pane.move_cursor_wrapping(1);
        assert_eq!(pane.cursor, 0, "down from the bottom wraps to the top");
        // Mid-list movement behaves like plain move_cursor.
        pane.move_cursor_wrapping(1);
        assert_eq!(pane.cursor, 1);
    }

    #[test]
    fn move_cursor_wrapping_on_empty_listing_is_a_noop() {
        let mut pane = Pane::new_empty(PathBuf::from("/"));
        pane.move_cursor_wrapping(-1);
        assert_eq!(pane.cursor, 0);
        pane.move_cursor_wrapping(1);
        assert_eq!(pane.cursor, 0);
    }

    #[test]
    fn set_dir_size_stamps_entry_survives_reload_and_clears_on_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("f.txt"), vec![0u8; 123]).unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();

        pane.set_dir_size(sub.clone(), 123);
        let entry = pane.entries.iter().find(|e| e.path == sub).unwrap();
        assert_eq!(entry.size, 123);
        assert_eq!(pane.dir_size_overrides.get(&sub), Some(&123));

        // Survives a reload of the same directory (a finished task's
        // reload_both must not wipe the number).
        pane.reload().unwrap();
        let entry = pane.entries.iter().find(|e| e.path == sub).unwrap();
        assert_eq!(entry.size, 123);

        // Feeds the size sort.
        pane.set_sort(SortKey::Size, true);
        assert!(pane.visible_entries().len() > 1);

        // Cleared once the pane navigates away.
        pane.jump_to(sub.clone()).unwrap();
        assert!(pane.dir_size_overrides.is_empty());
    }

    #[test]
    fn set_dir_size_ignores_results_for_a_different_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut pane = Pane::new(dir.path().to_path_buf()).unwrap();
        // A result whose parent isn't the pane's cwd (the pane moved on
        // while the task ran) must be dropped, not stored.
        pane.set_dir_size(PathBuf::from("/somewhere/else"), 42);
        assert!(pane.dir_size_overrides.is_empty());
    }

    #[test]
    fn sort_key_as_str_round_trips_through_from_str() {
        for key in [SortKey::Name, SortKey::Size, SortKey::MTime, SortKey::Ext] {
            assert_eq!(SortKey::from_str(key.as_str()), Some(key));
        }
        assert_eq!(SortKey::from_str("bogus"), None);
    }
}
