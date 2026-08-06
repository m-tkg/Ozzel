//! Input mode: `Normal` routes keys through the `Keymap`, `Prompt` and
//! `Confirm` are modal and consume fixed editing keys directly (see
//! `App::handle_prompt_key` / `App::handle_confirm_key`).

use std::path::PathBuf;
use std::rc::Rc;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::file_search::FileSearchTree;
use crate::keymap::KeyCombo;
use crate::settings::Category;
use crate::viewer::Matcher;

/// What a `Mode::Prompt` is collecting text for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Rename {
        orig: String,
    },
    Mkdir,
    /// Collecting the archive file name for a zip-create; `targets` is
    /// captured at prompt-open time (the marks/cursor selection that
    /// triggered it), not re-read when the prompt commits.
    ZipName {
        targets: Vec<PathBuf>,
    },
    /// Collecting a `:`-command line to run with the TUI suspended.
    Command,
    /// Collecting the new name for `duplicate` (`c`): prefilled with the
    /// cursor entry's current name, committed as a copy of `source` under
    /// the typed name in the *same* directory. `source` is captured at
    /// prompt-open time, same story as `ZipName`'s `targets`.
    Duplicate {
        source: PathBuf,
    },
    /// The rename branch of one `Mode::TransferCollision` decision:
    /// collecting the new destination name for the collision currently
    /// being asked about (`state.current`). Boxed — `CollisionState`
    /// carries the whole remaining worklist, and `PromptKind` is embedded
    /// in the frequently-moved `Mode`. Committing a valid, non-colliding
    /// name resolves this entry and returns to the collision dialog for
    /// the next one; a name that *also* collides re-asks the same entry;
    /// Esc cancels the entire transfer (same as Esc in the dialog).
    CollisionRename {
        state: Box<CollisionState>,
    },
    /// Collecting a password for an encrypted zip (masked input — see
    /// `ui::modal::render_prompt_box`). `pending` carries the operation
    /// to retry once the password is typed; committing verifies it on the
    /// main thread first (a wrong one logs and re-prompts) and, for the
    /// Virtual-Directory-scoped operations, caches it on the `VirtualDir`
    /// for the rest of the session. See `App::commit_archive_password`.
    ArchivePassword {
        pending: PasswordPending,
    },
    /// One step of `rename_marks` (`m`): renaming `current` (a name in
    /// `dir`), with `queue` holding the names still to go. Committing one
    /// rename re-enters `Mode::Prompt` with the next `RenameMany` state
    /// (see `App::commit_rename_many`); Esc cancels the whole remainder.
    /// `done`/`total` drive the `(n/total)` progress in the prompt title.
    /// `dir` is captured at start time rather than re-read from the pane,
    /// so a reload mid-sequence can't silently retarget the renames.
    RenameMany {
        dir: PathBuf,
        current: String,
        queue: std::collections::VecDeque<String>,
        done: usize,
        total: usize,
    },
    /// Collecting the timestamp for `touch` (`T`): prefilled with the
    /// cursor entry's current mtime, applied to every path in `targets`
    /// on commit (empty input = now). `targets` is captured at prompt-open
    /// time, same story as `ZipName`'s. See `App::commit_touch`.
    TouchTime {
        targets: Vec<PathBuf>,
    },
}

/// The operation a `PromptKind::ArchivePassword` retries once a password
/// has been typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordPending {
    /// Opening one archive-internal file in the built-in viewer.
    View {
        archive_path: PathBuf,
        inner_path: PathBuf,
    },
    /// A Virtual Directory `C` extraction (continues into the normal
    /// collision-confirm flow once the password verifies).
    Extract {
        archive_path: PathBuf,
        inner_targets: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    /// A whole-archive `u` unzip (continues into the overwrite-confirm
    /// flow once the password verifies).
    Unzip {
        archive_path: PathBuf,
        dest_dir: PathBuf,
    },
}

/// Which jump menu a `Mode::Select` is showing — `d` (delete) only makes
/// sense for `Bookmark`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    History,
    Bookmark,
}

/// Which direction a marked-or-cursor transfer is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Copy,
    Move,
}

/// Which representation the built-in viewer is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Text,
    /// An `xxd`-style hex dump of the raw bytes (see `viewer::format_hex_line`).
    Hex,
}

impl ViewMode {
    pub fn toggle(self) -> Self {
        match self {
            ViewMode::Text => ViewMode::Hex,
            ViewMode::Hex => ViewMode::Text,
        }
    }
}

/// How the viewer's *text* mode styles its lines: `Plain` for ordinary
/// files, `Diff` for the `=` action's generated unified diff (`+`/`-`
/// green/red, `@@` cyan, `---`/`+++` bold). Hex mode and every scroll/
/// search key ignore this entirely — it's purely a per-line base style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerSyntax {
    #[default]
    Plain,
    Diff,
}

/// Which way a viewer search (`/` vs `?`) reads the file — `n` repeats the
/// last search in this direction, `N` in the reverse of it, exactly like
/// `less`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// `/` — toward the end of the file.
    Forward,
    /// `?` — toward the start of the file.
    Backward,
}

impl SearchDirection {
    pub fn reversed(self) -> Self {
        match self {
            SearchDirection::Forward => SearchDirection::Backward,
            SearchDirection::Backward => SearchDirection::Forward,
        }
    }

    /// The `less`-style key that starts a search in this direction — the
    /// same glyph used as the input line's prompt label (`ui::help_view`/
    /// `ui::log_view`/`ui::viewer_view`'s search input line) and as the
    /// prefix on an already-committed search's footer note.
    pub fn label(self) -> &'static str {
        match self {
            SearchDirection::Forward => "/",
            SearchDirection::Backward => "?",
        }
    }
}

/// `less`-style incremental search state, shared by every full-frame
/// scrollable text view — `Mode::Viewer`, `Mode::Help`, and `Mode::Log` each
/// carry their own `search: ViewerSearch` field (kept under this name
/// rather than renamed to something viewer-agnostic, to avoid churning a
/// rename across three views for a purely cosmetic change — see
/// `crate::search`'s module doc comment). `Idle` is the steady state
/// outside of any search; pressing `/` or `?` moves to `Editing` (a bottom
/// input line, same UI pattern as `Filter`/`JumpSearch`); `Enter` there
/// runs the search and — if it matched anything — moves to `Active`, which
/// drives both the highlighted matches on screen and `n`/`N` navigation
/// until `Esc` (in plain, non-editing state) clears it back to `Idle`. The
/// actual state-machine transitions live in `crate::search`, not on this
/// type itself — this is pure data.
// No `PartialEq`/`Eq` here (or on `Mode`, which embeds this in three
// variants) — `matcher` below is a compiled `regex::Regex` under the hood,
// which implements neither. Nothing actually compares a `Mode`/`ViewerSearch`
// by value across the codebase (every existing "is it this variant" check is
// `matches!`/pattern-match, never `==`), so dropping the derives costs
// nothing real.
#[derive(Debug, Clone, Default)]
pub enum ViewerSearch {
    #[default]
    Idle,
    Editing {
        input: LineEditor,
        direction: SearchDirection,
        /// The search state (if any) that was active before `/`/`?` was
        /// pressed, restored verbatim on `Esc` — canceling an in-progress
        /// search must never lose a previous search's highlights, the same
        /// way `less` leaves you exactly where you were before pressing
        /// `/` again if you back out of it.
        previous: Box<ViewerSearch>,
    },
    Active {
        /// The raw text as typed — kept alongside `matcher` (rather than
        /// re-deriving it) purely for display (the footer's `/pattern`)
        /// and so `n`/`N`'s wraparound bookkeeping has something `Debug`/
        /// `Clone`-able to carry.
        pattern: String,
        /// The compiled matcher for `pattern`, built once by
        /// `crate::search::run` — every render of the viewer/help/log
        /// screens while this search is active borrows this instead of
        /// re-running `RegexBuilder::build` from scratch. `Rc` (not a bare
        /// `Matcher`) so `ViewerSearch` can stay `Clone` without requiring
        /// `Matcher: Clone` (a `regex::Regex` is, incidentally, `Clone`,
        /// but `Rc::clone` is a refcount bump either way — cheaper, and one
        /// less thing to keep true).
        matcher: Rc<Matcher>,
        direction: SearchDirection,
        /// Line indices (text mode) or hex-dump row indices (hex mode)
        /// containing at least one match, ascending. Always non-empty —
        /// a search with zero matches never produces `Active` (see
        /// `App::run_viewer_search`).
        matches: Vec<usize>,
        /// Index into `matches` of the line the viewer is currently
        /// parked on — what `n`/`N` advance and the footer's `i/N` counter
        /// reads.
        current: usize,
        /// Set by `App::run_viewer_search`/`App::viewer_search_step`
        /// whenever the *most recent* jump had to wrap around the start or
        /// end of the file to find a match — drives the footer's `less`-
        /// style "search wrapped" notice. Cleared by the next jump that
        /// *doesn't* wrap, not time-based.
        wrapped: bool,
    },
}

/// The five answers the collision dialog (`Mode::TransferCollision`)
/// offers for one same-name conflict, in display/cursor order.
pub const COLLISION_CHOICES: [&str; 5] =
    ["Overwrite", "Rename", "Skip", "Overwrite All", "Skip All"];

/// Everything a same-name collision dialog needs to walk its conflicts
/// one at a time: the destination pairs already settled (`resolved` —
/// non-colliding sources start here), the colliding sources still to ask
/// about (`pending`), and the one currently on screen (`current`). Built
/// by `App::begin_transfer` when a Copy/Move finds collisions; consumed
/// by `App::handle_transfer_collision_key`, which spawns the task from
/// `resolved` once every conflict has an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionState {
    pub kind: TransferKind,
    pub dest_dir: PathBuf,
    /// `(src, exact dest)` pairs ready to transfer.
    pub resolved: Vec<(PathBuf, PathBuf)>,
    /// Colliding sources not yet asked about.
    pub pending: std::collections::VecDeque<PathBuf>,
    pub current: CollisionInfo,
    /// 1-based ordinal of `current` among `total` collisions (title's
    /// `(2/5)`).
    pub index: usize,
    pub total: usize,
    /// Highlight index into `COLLISION_CHOICES`.
    pub cursor: usize,
}

/// The display facts for one collision: both sides' size/mtime lines,
/// pre-formatted by `App::collision_info` (the dialog renderer stays
/// I/O-free), with `[New]` already appended to whichever side is newer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionInfo {
    pub src: PathBuf,
    /// `src`'s file name — the dest name it collides under, and the
    /// prefill for the Rename branch.
    pub name: String,
    pub src_line: String,
    pub dest_line: String,
}

/// The state of the chmod dialog (`Mode::Chmod`): a 3x3 grid of rwx
/// toggles (rows = user/group/other, columns = r/w/x) editing one absolute
/// mode applied to every path in `targets` on Enter. Only the lower 0o777
/// is edited — `ops::chmod` preserves the setuid/setgid/sticky bits each
/// target already has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChmodState {
    pub targets: Vec<PathBuf>,
    /// The mode being edited (lower 0o777 only).
    pub bits: u32,
    /// Grid position, 0..9: row = `cursor / 3` (user/group/other), column
    /// = `cursor % 3` (r/w/x).
    pub cursor: usize,
}

impl ChmodState {
    /// The permission bit the grid cell at `cursor` toggles.
    pub fn bit_at(cursor: usize) -> u32 {
        let row = cursor / 3; // 0 = user, 1 = group, 2 = other
        let col = cursor % 3; // 0 = r, 1 = w, 2 = x
        let base = [0o400u32, 0o200, 0o100][col];
        base >> (row * 3)
    }
}

/// One pre-formatted `label: value` listing shown by the file-info modal
/// (`Mode::FileInfo`), built by `App::begin_file_info` at open time (fresh
/// `symlink_metadata`, owner/group resolution, ...) so the renderer stays
/// I/O-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfoData {
    /// The dialog title (the entry's name).
    pub title: String,
    /// `(label, value)` rows, already in display order. An empty label
    /// renders as a separator/blank line.
    pub rows: Vec<(String, String)>,
}

/// The operation a `Mode::Confirm` will perform if the user answers yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    Delete {
        targets: Vec<PathBuf>,
    },
    /// Confirmed symlink creation: one link per source in `targets`,
    /// created in `dest_dir`. See `App::begin_symlink`.
    Symlink {
        targets: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    /// A confirmed Copy/Move ready to spawn, as exact `(src, dest)`
    /// pairs. Reached only through the *no-collision* confirm
    /// (`config.confirm_operations`) — a transfer with same-name
    /// collisions goes through `Mode::TransferCollision` instead, which
    /// spawns directly without a second confirm.
    Transfer {
        kind: TransferKind,
        pairs: Vec<(PathBuf, PathBuf)>,
        dest_dir: PathBuf,
    },
    /// The archive path already existed; overwrite it.
    ZipOverwrite {
        targets: Vec<PathBuf>,
        archive_path: PathBuf,
    },
    /// One or more top-level entries in the archive already exist in the
    /// destination directory; overwrite them. `password` (already
    /// verified at prompt time) rides along for an encrypted zip.
    UnzipOverwrite {
        archive_path: PathBuf,
        dest_dir: PathBuf,
        password: Option<String>,
    },
    /// A confirmed extraction from a Virtual Directory (`C` while the
    /// active pane is browsing inside a `.zip`): a partial extraction of
    /// the marked/cursor entries. `inner_targets` are archive-internal paths
    /// (marks/cursor from the virtual pane), extracted into `dest_dir`
    /// (the *other*, necessarily real, pane's cwd).
    Extract {
        archive_path: PathBuf,
        inner_targets: Vec<PathBuf>,
        dest_dir: PathBuf,
        /// Already verified at prompt time for an encrypted zip; `None`
        /// for everything else.
        password: Option<String>,
    },
    /// Confirmed by the quit-while-busy guard: tasks are still running but
    /// the user wants out anyway.
    Quit,
}

// See `ViewerSearch`'s doc comment for why this has no `PartialEq`/`Eq`:
// `Viewer`/`Help`/`Log` all embed a `search: ViewerSearch`, which now holds
// a compiled matcher.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// Incremental filter/search: every edit keystroke live-applies to the
    /// active pane's `Pane.filter` (see `App::handle_filter_key`).
    Filter {
        input: LineEditor,
    },
    /// Prefix-jump incremental search (`\`): pure cursor movement, never
    /// hides/filters the listing (that's `Filter`'s job) — every keystroke
    /// moves the active pane's cursor to the first visible entry whose
    /// name starts with what's typed so far. See
    /// `App::handle_jump_search_key`.
    JumpSearch {
        input: LineEditor,
        /// The cursor position when the search was opened, restored
        /// verbatim on `Esc` — `Enter` leaves the cursor wherever the
        /// search left it instead.
        original_cursor: usize,
    },
    /// A centered jump menu (history or bookmarks): up/down move, Enter
    /// selects (the active pane cd's there), Esc cancels, and `d` deletes
    /// the highlighted entry when `kind` is `Bookmark`.
    Select {
        kind: SelectKind,
        title: String,
        items: Vec<(String, PathBuf)>,
        cursor: usize,
    },
    Prompt {
        kind: PromptKind,
        input: LineEditor,
    },
    Confirm {
        message: String,
        on_yes: PendingOp,
    },
    /// The per-file same-name collision dialog a Copy/Move with conflicts
    /// walks through before anything is spawned (one 5-way choice per
    /// conflict — see `CollisionState`/`COLLISION_CHOICES`). Esc cancels
    /// the whole transfer, not just the current entry.
    TransferCollision {
        state: CollisionState,
    },
    /// The built-in full-frame text viewer (`x`/`o`/Enter-on-file),
    /// `less`-like. Fixed keys only (see `App::handle_viewer_key`):
    /// Up/Down/PageUp/PageDown/Home/End (`g`/`G`, `j`/`k`, `f`/`b`/`Space`,
    /// `d`/`u` too) scroll vertically, Left/Right scroll horizontally, Tab
    /// toggles text/hex mode, `/`/`?` open a forward/backward search input
    /// (see `ViewerSearch`), `n`/`N` repeat it forward/backward, `q`/Esc
    /// closes back to the filer (Esc clears an active search's highlights
    /// first instead, if there is one).
    Viewer {
        path: PathBuf,
        lines: Vec<String>,
        /// The raw bytes backing `lines` (lossily decoded from these),
        /// retained so hex mode can render the exact on-disk bytes.
        bytes: Vec<u8>,
        view_mode: ViewMode,
        /// Index of the first visible line (text mode) or first visible
        /// 16-byte row (hex mode) — reset to 0 on every Tab toggle, since
        /// the two modes don't share a scroll position.
        scroll: usize,
        /// Display-column offset of the first visible column (text mode
        /// only; hex mode ignores this and always shows full rows).
        h_scroll: usize,
        /// The file was larger than the viewer's size cap and got cut off.
        truncated: bool,
        /// Text-mode line styling — `Plain` for real files, `Diff` for
        /// the `=` action's generated unified diff. See `ViewerSyntax`.
        syntax: ViewerSyntax,
        /// `less`-style `/`/`?` search state — see `ViewerSearch`.
        search: ViewerSearch,
    },
    /// The full-frame keybinding help screen (`h`/`?`). Fixed keys only
    /// (see `App::handle_help_key`): the same `less`-style scroll set the
    /// viewer's text mode has (Up/Down/`j`/`k`, `Space`/`f`/PageDown,
    /// `b`/PageUp, `d`/`u` half page, `g`/Home top, `G`/End bottom) plus
    /// `/`/`?`/`n`/`N` search (see `search`, `crate::search`), `q`/Esc/`h`
    /// closes back to the filer (a first `Esc` while a search is active
    /// clears it instead, same two-step close the viewer has). The
    /// listing itself (`crate::help::build_lines`) is computed on demand
    /// from the live `Keymap`, never stored here, so it always reflects
    /// the current effective bindings.
    Help {
        /// Index of the first visible line.
        scroll: usize,
        /// `less`-style `/`/`?` search state — see `ViewerSearch`. The
        /// haystack it searches is `crate::help::build_display_lines`'s
        /// output, rebuilt fresh per search the same way `scroll`'s own
        /// clamping rebuilds `build_lines` fresh per keypress.
        search: ViewerSearch,
    },
    /// The full-frame in-memory log viewer (`S-l`/`L`). Fixed keys only
    /// (see `App::handle_log_view_key`), the same `less`-style scroll set
    /// Help/the viewer have — mapped onto `scroll_from_bottom`'s inverted
    /// sense (see below: "up"/`k`/`b` *increment* it, "down"/`j`/`f`
    /// *decrement* it) — plus `/`/`?`/`n`/`N` search (see `search`), same
    /// two-step `Esc` close as Help/the viewer. `scroll_from_bottom` is
    /// measured in wrapped display rows *up from the newest content* (0 =
    /// pinned to the bottom, which is where this mode always opens) rather
    /// than a raw line index, since how many rows the log wraps into
    /// depends on terminal width — a width `App` has no access to;
    /// `ui::log_view::render_full` (which does have it) is what actually
    /// turns this into a display offset and clamps it, so this field can
    /// grow past the real maximum here with no ill effect (see also
    /// `Home`'s use of `usize::MAX`). `App::log_view_width` caches that
    /// same width (updated every `render_full` call, the same "stale
    /// until the first frame, harmless" story `pane_layout` already
    /// relies on) so a `/`/`?` search can rewrap the log the same way and
    /// therefore search against exactly what's on screen.
    Log {
        scroll_from_bottom: usize,
        search: ViewerSearch,
    },
    /// The command palette (`F`/`S-f`): a filterable, scrollable list of
    /// every action. Typing narrows the
    /// list (see `crate::function_list::filter_actions`); `cursor` indexes
    /// into that *filtered* list, not `Action::ALL`, so it's clamped
    /// relative to whatever the current `input` matches.
    FunctionList {
        input: LineEditor,
        cursor: usize,
    },
    /// The recursive file-name search popup (`g`): the subtree under the
    /// active pane's cwd, walked *once* into `tree` when the popup opened,
    /// narrowed by the pane filter's query grammar (see
    /// `crate::file_search`). Selecting a hit jumps the pane to its parent
    /// directory with the cursor on it — see `App::handle_file_search_key`.
    FileSearch {
        input: LineEditor,
        /// Index into `results` (the narrowed list), not `tree.entries`.
        cursor: usize,
        /// The one-shot snapshot — `Rc` because `Mode` is `Clone` and a
        /// snapshot can hold up to `MAX_TREE_ENTRIES` entries; a refcount
        /// bump beats deep-copying that on every mode clone (single-
        /// threaded `App` state, so `Rc` over `Arc`).
        tree: Rc<FileSearchTree>,
        /// Indices into `tree.entries` matching the *last-run* query.
        /// Always current in incremental mode; in Enter-run mode
        /// (`config.file_search_incremental = false`) it deliberately lags
        /// the input until the next `Enter`.
        results: Vec<usize>,
        /// The query `results` was computed from — `Enter`'s "re-run or
        /// jump?" pivot: a mismatch against the current input means the
        /// results are stale, so `Enter` re-runs instead of jumping.
        last_run_query: String,
        /// A `re:` pattern's compile error from the last run
        /// (`FilterSpec::error`), shown under the input line.
        error: Option<String>,
    },
    /// The chmod dialog (`A`): a centered 3x3 rwx toggle grid. Fixed keys
    /// (see `App::handle_chmod_key`): arrows move over the grid, Space
    /// toggles the highlighted bit, `0`-`7` set the highlighted row's
    /// class as an octal digit, Enter applies to every target, Esc
    /// cancels.
    Chmod {
        state: ChmodState,
    },
    /// The file-info modal (`I`): a read-only `label: value` listing of
    /// the cursor entry's metadata, built once at open time. Any of
    /// Esc/`q`/Enter closes it (see `App::handle_file_info_key`). Boxed —
    /// the rows can be a couple hundred bytes and `Mode` moves around a
    /// lot.
    FileInfo {
        info: Box<FileInfoData>,
    },
    /// The sort dialog (`t`): a small centered modal listing every
    /// (sort key, direction) combination — Name↑, Name↓, Size↑, … —
    /// `cursor` indexing that fixed 8-row list. Opens with the cursor on
    /// the active pane's current state; Enter applies (and records the
    /// choice per-directory — see `App::handle_sort_select_key`), Esc
    /// closes without changing anything.
    SortSelect {
        cursor: usize,
    },
    /// The full-frame settings screen (`S`/`S-s`): raspi-config-style
    /// category -> item -> editor navigation, see `crate::settings` for
    /// the category/item catalog and persistence, `App::handle_settings_key`
    /// for the actual key-dispatch/transitions this only stores the result
    /// of.
    Settings {
        screen: SettingsScreen,
    },
}

/// Which level of the settings screen is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsScreen {
    /// The category menu (`Category::ALL`); `cursor` indexes into it.
    Categories { cursor: usize },
    /// The item list within one category. `cursor` indexes into: that
    /// category's fixed `settings::BEHAVIOR_ITEMS`/`COLOR_ITEMS`/
    /// `STARTUP_ITEMS` array (`Behavior`/`Colors`/`Startup`), the config's
    /// current `[viewers]` extensions sorted plus one synthetic
    /// "+ add new" slot at the end (`Viewers`), or `Action::ALL`
    /// (`Keybindings`).
    Items { category: Category, cursor: usize },
    /// A per-item editor is active on top of the item list it was opened
    /// from (`category`/`item_cursor`, restored on Esc/commit).
    Editor {
        category: Category,
        item_cursor: usize,
        editor: SettingsEditor,
    },
}

/// Which top-level (`home`/`editor`) or `[viewers]`-entry text field a
/// `SettingsEditor::Text`/`ViewerEntry` is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Home,
    Editor,
}

/// State for whichever per-item editor is currently open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEditor {
    /// `delete_behavior`'s two-way select: `cursor` 0 = trash, 1 = permanent.
    DeleteBehavior { cursor: usize },
    /// `size_format`'s three-way select: `cursor` 0 = bytes,
    /// 1 = bytes_grouped, 2 = human.
    SizeFormat { cursor: usize },
    /// A curated named-color palette (`settings::COLOR_PALETTE`) plus one
    /// synthetic "custom hex" slot at the end (index
    /// `COLOR_PALETTE.len()`); `cursor` indexes across both. `editing_hex`
    /// is true once Enter has been pressed on the hex slot, at which point
    /// `hex_input` holds what's being typed (prefilled with the item's
    /// current value's hex form when there's no exact palette match).
    Color {
        key: &'static str,
        cursor: usize,
        editing_hex: bool,
        hex_input: LineEditor,
    },
    /// `home`/`editor` (top-level optional text settings).
    Text { field: TextField, input: LineEditor },
    /// Adding (`old_extension: None`) or editing/renaming
    /// (`old_extension: Some(...)`) one `[viewers]` entry — two stacked
    /// fields, `Tab` swaps which one `editing_extension` says has focus.
    ViewerEntry {
        old_extension: Option<String>,
        extension: LineEditor,
        command: LineEditor,
        editing_extension: bool,
    },
    /// One action's bound-combo list (`settings::combos_for`); `cursor`
    /// indexes into it. `a` starts a capture (-> `KeybindingCapture`), `d`
    /// removes the combo at `cursor`.
    Keybinding { action: Action, cursor: usize },
    /// Mid-capture: the very next keypress becomes the new combo, except
    /// `Esc`, which is reserved to cancel back to `Keybinding` instead of
    /// ever being capturable itself. `cursor` is carried through purely so
    /// canceling restores the exact `Keybinding { action, cursor }` this
    /// capture was started from.
    KeybindingCapture { action: Action, cursor: usize },
    /// Confirming a just-captured combo before it's written — a plain
    /// bind when `conflict` is `None`, a steal offer (naming the losing
    /// action) when it's `Some`. `Enter`/`y` confirms, `Esc`/`n` cancels
    /// back to `Keybinding` without writing anything. `cursor`, same
    /// reason as `KeybindingCapture`'s.
    KeybindingConfirm {
        action: Action,
        combo: KeyCombo,
        conflict: Option<Action>,
        cursor: usize,
    },
}

/// A grapheme-safe single-line text editor: every unit the cursor moves
/// over, inserts before, or deletes is a whole grapheme cluster, so
/// Japanese (and other multi-byte) filenames edit correctly instead of
/// getting split mid-character.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineEditor {
    graphemes: Vec<String>,
    /// Index into `graphemes` (0..=len), not a byte offset.
    cursor: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    // Named to mirror `std::str::FromStr::from_str` for call-site
    // readability (`LineEditor::from_str(...)`, no `Result` unwrap
    // needed since this can't fail) — not an actual `FromStr` impl, so
    // silence clippy's trait-name-collision lint rather than rename it.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        let graphemes: Vec<String> = s.graphemes(true).map(str::to_string).collect();
        let cursor = graphemes.len();
        Self { graphemes, cursor }
    }

    pub fn value(&self) -> String {
        self.graphemes.concat()
    }

    /// Inserts `ch` immediately before the cursor and advances past it.
    pub fn insert(&mut self, ch: char) {
        self.graphemes.insert(self.cursor, ch.to_string());
        self.cursor += 1;
    }

    /// Deletes the grapheme before the cursor (classic backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.graphemes.remove(self.cursor);
        }
    }

    /// Deletes the grapheme under/after the cursor (forward delete).
    pub fn delete(&mut self) {
        if self.cursor < self.graphemes.len() {
            self.graphemes.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.graphemes.len() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.graphemes.len();
    }

    /// Display-column offset of the cursor from the start of the line,
    /// accounting for wide (e.g. full-width Japanese) graphemes.
    pub fn cursor_display_col(&self) -> usize {
        self.graphemes[..self.cursor]
            .iter()
            .map(|g| UnicodeWidthStr::width(g.as_str()))
            .sum()
    }

    /// The cursor's grapheme index (0..=grapheme_count) — masked
    /// (password) rendering positions the cursor by this rather than
    /// `cursor_display_col`, since each grapheme renders as exactly one
    /// `*` column regardless of its real width.
    pub fn cursor_grapheme_index(&self) -> usize {
        self.cursor
    }

    /// How many graphemes the value holds — the number of `*`s a masked
    /// render shows.
    pub fn grapheme_count(&self) -> usize {
        self.graphemes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_value_round_trip_ascii() {
        let mut editor = LineEditor::new();
        for c in "hello".chars() {
            editor.insert(c);
        }
        assert_eq!(editor.value(), "hello");
    }

    #[test]
    fn backspace_removes_one_grapheme_not_one_byte() {
        let mut editor = LineEditor::from_str("日本語ファイル名.txt");
        editor.backspace();
        assert_eq!(editor.value(), "日本語ファイル名.tx");
    }

    #[test]
    fn insert_in_the_middle_of_japanese_text() {
        let mut editor = LineEditor::from_str("日本語.txt");
        editor.move_home();
        editor.move_right();
        editor.move_right();
        editor.insert('X');
        assert_eq!(editor.value(), "日本X語.txt");
    }

    #[test]
    fn delete_removes_grapheme_after_cursor() {
        let mut editor = LineEditor::from_str("日本語");
        editor.move_home();
        editor.delete();
        assert_eq!(editor.value(), "本語");
    }

    #[test]
    fn move_left_right_home_end_stay_in_bounds() {
        let mut editor = LineEditor::from_str("ab");
        editor.move_right(); // already at end, no-op
        assert_eq!(editor.cursor, 2);
        editor.move_home();
        editor.move_left(); // already at start, no-op
        assert_eq!(editor.cursor, 0);
        editor.move_end();
        assert_eq!(editor.cursor, 2);
    }

    #[test]
    fn cursor_display_col_accounts_for_wide_graphemes() {
        let mut editor = LineEditor::from_str("日本");
        assert_eq!(editor.cursor_display_col(), 4); // two width-2 graphemes
        editor.move_home();
        assert_eq!(editor.cursor_display_col(), 0);
    }
}
