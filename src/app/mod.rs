//! Top-level application state: the two panes, which one is active, the
//! current input mode, running background tasks, and the `Action` dispatch
//! hub every Normal-mode key eventually funnels through.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, Local};
use directories::BaseDirs;

use crate::action::Action;
use crate::config::{self, Config, DeleteBehavior};
use crate::entry::EntryKind;
use crate::event::{AppEvent, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use crate::external::{self, ExternalRequest};
use crate::file_search::{self, MAX_TREE_ENTRIES};
use crate::filter::FilterSpec;
use crate::help::HelpLine;
use crate::keymap::{KeyCombo, Keymap, MenuNav};
use crate::mode::{
    COLLISION_CHOICES, ChmodState, CollisionInfo, CollisionState, LineEditor, Mode,
    PasswordPending, PendingOp, PromptKind, SYNC_CHOICES, SearchDirection, SelectKind,
    SettingsEditor, SettingsScreen, TextField, TransferKind, ViewMode, ViewerSearch, ViewerSyntax,
};
use crate::ops;
use crate::pane::{CursorAnchor, PAGE_SIZE, Pane, SortKey};
use crate::persist::{Bookmarks, History, Side, SortPrefs};
use crate::search;
use crate::settings::{self, Category};
use crate::tasks::delete as delete_task;
use crate::tasks::{TaskEvent, TaskId, TaskManager, archive, copy_move};
use crate::ui::layout::PaneLayout;
use crate::viewer;
use crate::virtual_dir;

/// Log lines are capped so a long session's log can't grow without bound.
const LOG_CAPACITY: usize = 500;
/// How many lines `Mode::Viewer`'s PageUp/PageDown (also `f`/`Space`/`b`)
/// jumps.
const VIEWER_PAGE_SIZE: usize = 20;
/// How many lines `Mode::Viewer`'s `d`/`u` (`less`-style half-page scroll)
/// jumps.
const VIEWER_HALF_PAGE_SIZE: usize = VIEWER_PAGE_SIZE / 2;
/// How many display columns `Mode::Viewer`'s Left/Right scrolls per press.
const VIEWER_H_SCROLL_STEP: usize = 8;
/// How many rows one mouse-wheel "tick" moves a pane's cursor, or scrolls a
/// modal (viewer/log/help) — see `App::handle_mouse`.
const MOUSE_WHEEL_STEP: usize = 3;
/// A second left-click on the same row within this window counts as a
/// double-click (opens the entry), rather than a second single-click (which
/// would just re-set the cursor to where it already is).
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Left,
    Right,
}

impl ActivePane {
    pub fn other(self) -> Self {
        match self {
            ActivePane::Left => ActivePane::Right,
            ActivePane::Right => ActivePane::Left,
        }
    }

    fn index(self) -> usize {
        match self {
            ActivePane::Left => 0,
            ActivePane::Right => 1,
        }
    }
}

/// `persist::History` is keyed by a local `Side` type rather than
/// `ActivePane` directly, so `persist.rs` doesn't have to depend upward on
/// the app layer; this is the one place that bridges the two.
impl From<ActivePane> for Side {
    fn from(pane: ActivePane) -> Self {
        match pane {
            ActivePane::Left => Side::Left,
            ActivePane::Right => Side::Right,
        }
    }
}

/// One line in the log area.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub message: String,
    pub is_error: bool,
    /// When this line was appended (local time), captured once by
    /// `App::log_push` — never recomputed at render time, so the log
    /// area's rendering stays a pure function of already-stored data (see
    /// `crate::logwrap::format_timestamp_prefix`).
    pub timestamp: DateTime<Local>,
    /// `crate::logwrap::format_timestamp_prefix(timestamp)`, computed once
    /// here by `LogLine::new` rather than by every `wrap_log_lines`/
    /// `wrap_log_lines_tail` call — with up to `LOG_CAPACITY` lines live,
    /// re-running `chrono`'s `format` on every one of them every frame the
    /// bottom log panel (or the full log view) draws was real, measurable
    /// per-frame cost for something that never changes after the line is
    /// appended. `pub(crate)` (not `pub`): read directly by `ui::log_view`
    /// (via the `logwrap::LoggableLine` impl below), but never constructed
    /// outside `LogLine::new`, which is what keeps it in sync with
    /// `timestamp`.
    pub(crate) formatted_timestamp: String,
}

impl LogLine {
    pub fn new(message: String, is_error: bool, timestamp: DateTime<Local>) -> Self {
        let formatted_timestamp = crate::logwrap::format_timestamp_prefix(timestamp);
        Self {
            message,
            is_error,
            timestamp,
            formatted_timestamp,
        }
    }
}

/// Lets `crate::logwrap::wrap_log_lines`/`wrap_log_lines_tail` operate on
/// `LogLine` without that module depending on `app` — see `logwrap`'s own
/// doc comment for why the dependency runs this direction instead.
impl crate::logwrap::LoggableLine for LogLine {
    fn message(&self) -> &str {
        &self.message
    }

    fn is_error(&self) -> bool {
        self.is_error
    }

    fn formatted_timestamp(&self) -> &str {
        &self.formatted_timestamp
    }
}

/// Every side-channel output a single `App::handle_event`/`dispatch` call
/// might queue for `main.rs`'s loop to actually carry out — each needs a
/// resource only `main.rs` holds (the `Terminal`, a raw stdout handle) or
/// a disk write `App` itself never performs, so setting the field is as
/// far as `App` alone can take it. Bundled into one struct, drained in one
/// `App::take_outbox` call per loop iteration, rather than as four
/// separately-named fields each polled by hand.
#[derive(Debug, Default)]
pub struct Outbox {
    /// Set by `:` (arbitrary command) and `e` (editor); `main.rs`'s loop
    /// takes this after each event and, if present, suspends the TUI to
    /// run it via `external::run_suspended`.
    pub external: Option<ExternalRequest>,
    /// Set alongside `external` by `,` (edit_config) specifically:
    /// `main.rs`'s loop checks this right after the queued editor exits
    /// and, if set, calls `reload_config` — a plain bool rather than
    /// folding it into `ExternalRequest` itself, since every other
    /// external command has nothing to do afterward.
    pub config_reload: bool,
    /// Set by `y` (copy_path); `main.rs`'s loop takes this after each event
    /// and, if present, writes the OSC 52 "set clipboard" escape directly
    /// to stdout (no need to suspend the TUI for this one, unlike
    /// `external` — it's a single silent write, not a child process taking
    /// over the screen).
    pub clipboard: Option<String>,
    /// Set whenever `App::bookmarks` is mutated; `main.rs`'s loop checks
    /// this once per iteration and saves (clearing the flag) when set, per
    /// the plan's "save ... after bookmark changes".
    pub bookmarks_dirty: bool,
}

pub struct App {
    pub panes: [Pane; 2],
    pub active: ActivePane,
    pub should_quit: bool,
    pub mode: Mode,
    pub config: Config,
    pub keymap: Keymap,
    pub log: VecDeque<LogLine>,
    pub tasks: TaskManager,
    /// The receiving end of every worker thread's `Sender<TaskEvent>` clone
    /// (`tasks` holds the sender side). Drained once per main-loop
    /// iteration by `drain_tasks`, ahead of the next terminal poll.
    task_rx: mpsc::Receiver<TaskEvent>,
    /// Per-pane visited-directory rings. Loaded/saved entirely by the
    /// caller (`main.rs`) — `App` itself never touches disk, so
    /// constructing an `App` in a test never has a side effect on the
    /// real user's `~/.local/share/ozzel/`. Starts empty; `main.rs`
    /// overwrites it with `persist::load_history()`'s result right after
    /// `App::new` returns.
    pub history: History,
    /// Bookmarked directories; same load/save-is-the-caller's-job story as
    /// `history`.
    pub bookmarks: Bookmarks,
    /// Per-directory remembered sort choices (`t`/`s` record, every cwd
    /// change restores); same load/save-is-the-caller's-job story as
    /// `history` — `main.rs` loads it after `App::new` and saves on quit.
    pub sort_prefs: SortPrefs,
    /// Every side-channel output an event might queue for `main.rs`'s loop
    /// to carry out — see `Outbox`'s own doc comment. Drained once per
    /// iteration by `take_outbox`, rather than four separately-polled
    /// fields.
    outbox: Outbox,
    /// The last-drawn screen geometry for each pane (see
    /// `ui::mod::draw`) — refreshed every frame, read by mouse
    /// hit-testing (`App::handle_mouse`) to turn a click's screen
    /// coordinates back into "which pane, which row". `None` until the
    /// first frame draws (there's nothing to click before then).
    pub pane_layout: [Option<PaneLayout>; 2],
    /// The pane a left-button drag started in, plus the marking already
    /// applied this drag — set on mouse-down over an entry row, cleared on
    /// mouse-up. Constrains a drag-mark to a single pane even if the
    /// pointer crosses into the other one mid-drag (see
    /// `App::handle_mouse`).
    pub drag: Option<DragState>,
    /// `(pane, entry index, when)` of the most recent left-click-down on an
    /// entry row, used purely to detect a double-click (see
    /// `App::handle_mouse_left_down`) — `None` once consumed by a
    /// double-click or once `DOUBLE_CLICK_WINDOW` has passed.
    last_click: Option<(ActivePane, usize, Instant)>,
    /// Which pane a delete task's cursor should land on (and where — see
    /// `Pane::anchor_above`), keyed by that task's own `TaskId` so an
    /// unrelated task finishing first can never misapply it. Populated by
    /// `spawn_delete` right before the task starts, consumed (removed) by
    /// `handle_task_event` the moment *that exact* task's `Finished`
    /// event arrives.
    pending_delete_anchor: HashMap<TaskId, (ActivePane, Option<CursorAnchor>)>,
    /// Which pane a `calc_dir_size` task's incoming `TaskEvent::DirSize`
    /// results should be applied to, keyed by that task's `TaskId` — the
    /// same "keyed by the task's own id so an unrelated task can never
    /// misapply it" pattern as `pending_delete_anchor`. Consumed (removed)
    /// when that task's `Finished` arrives.
    pending_dir_size: HashMap<TaskId, ActivePane>,
    /// Which pane a background `git status` task's `TaskEvent::GitStatus`
    /// result should be applied to — same TaskId-keyed pattern as
    /// `pending_dir_size`. Also the discriminator `handle_task_event`
    /// uses to swallow these tasks' `Finished` events (no log line, no
    /// pane reload, no mark clearing — a passive status probe must never
    /// have the side effects a real file operation's completion has).
    pending_git_status: HashMap<TaskId, ActivePane>,
    /// The most recently spawned git-status task per pane — only *its*
    /// result is ever applied; a slower, older probe's result arriving
    /// after a newer one was spawned is dropped (same "stale results are
    /// discarded" defense `Pane::set_dir_size` has, keyed by task
    /// recency instead of path). The cancel flag rides along so spawning
    /// a replacement probe can abort the superseded one — probes are
    /// detached (`TaskManager::spawn_detached`), so there's no `running`
    /// entry to reach a cancel flag through.
    latest_git_task: [Option<(TaskId, std::sync::Arc<std::sync::atomic::AtomicBool>)>; 2],
    /// The cwd each pane's git status was last probed for —
    /// `maybe_refresh_git`'s cheap change detector (one `PathBuf`
    /// comparison per pane per event). Reset to `None` to force a
    /// re-probe (config reload, task completion, ...).
    git_checked_dir: [Option<PathBuf>; 2],
    /// Overrides `config::config_path()` for the settings screen's own
    /// `toml_edit` writes and its post-write `reload_config_from` call —
    /// `None` (the real-app default) means "use the real, XDG-resolved
    /// location", exactly like `reload_config`'s own default. Tests point
    /// this at a tempdir file instead, the same reasoning
    /// `begin_edit_config_at`/`reload_config_from` already use an
    /// injectable path for: `cargo test` must never read from or write to
    /// a real user's config file.
    settings_config_path: Option<PathBuf>,
    /// The content width `ui::log_view::render_full` used on its most
    /// recent call — refreshed every frame while `Mode::Log` is up, the
    /// same "stale until the first frame draws, harmless" caching
    /// `pane_layout` already relies on for mouse hit-testing. `Mode::Log`'s
    /// own `/`/`?` search needs *some* width to rewrap the log into the
    /// exact same rows the screen shows (search matches against what's
    /// visible, not the raw unwrapped messages), and `App` otherwise has no
    /// way to know the terminal's width at all. Defaults to
    /// `DEFAULT_LOG_VIEW_WIDTH`, a reasonable guess for the one keystroke
    /// (if any) that could theoretically race the first frame. `pub`, same
    /// as `pane_layout`, so `ui::log_view::render_full` (a different
    /// module) can write it directly every frame.
    pub log_view_width: u16,
    /// Bumped by `push_log_line` on every append — the invalidation key for
    /// `log_wrap_cache` (see `log_wrapped`), so the full log view/search
    /// don't re-wrap (and re-timestamp) the entire in-memory log on every
    /// single frame it's open, only when it actually changed or the
    /// terminal was resized.
    log_generation: u64,
    /// `log_wrapped`'s cache: the last `(log_generation, width)` this was
    /// built at, plus the result. `None` until the first call.
    #[allow(
        clippy::type_complexity,
        reason = "one-off cache tuple, not worth a named type"
    )]
    log_wrap_cache: Option<(u64, u16, Vec<(String, bool)>)>,
    /// `App::help_lines`'s cache: the last `Keymap::generation` the help
    /// screen's line listing (`crate::help::build_lines`) was built from,
    /// plus the result — rebuilding that listing walks every bound action,
    /// which only needs to happen again when the keymap itself changes
    /// (config reload / a settings-screen rebind), not on every keystroke
    /// scrolling or searching the already-open help screen.
    help_lines_cache: Option<(u64, Vec<crate::help::HelpLine>)>,
    /// `App::settings_keybinding_lines`'s cache, same
    /// `Keymap::generation`-keyed story as `help_lines_cache` — the
    /// settings screen's Keybindings category otherwise calls
    /// `settings::combos_for` (a full scan of the keymap) for all
    /// `Action::ALL` actions on every single frame it's shown, not just the
    /// ones scrolled into view.
    settings_keybinding_lines_cache: Option<(u64, Vec<String>)>,
    /// `App::function_list_filtered_actions`'s cache, keyed by the exact
    /// typed query string rather than a generation counter (there's no
    /// single mutable "keymap" here to version — the query itself already
    /// says everything needed to know whether the filtered list could have
    /// changed): the command palette re-renders on every keystroke *and*
    /// every plain cursor-up/down, but the filtered action list only
    /// actually changes when the typed text does.
    function_list_cache: Option<(String, Vec<Action>)>,
    /// Whether `main.rs`'s loop should call `terminal.draw` this
    /// iteration — the dirty flag behind Phase 1's "only redraw when
    /// something changed" performance fix. Starts `true` (the first frame
    /// always needs drawing) and is set back to `true` by `handle_event`
    /// for anything but a bare `Tick` (see its doc comment — deliberately
    /// coarse: any real input/task/resize event just marks the whole frame
    /// dirty rather than tracking exactly what changed), by `main.rs`'s
    /// loop itself while any task is running (so the status bar's
    /// running-task gauge keeps updating even between task events), and
    /// after resuming from a suspended external command (the terminal was
    /// just handed back, and the old frame is stale regardless of whether
    /// `App` state changed). `main.rs` is the only reader; it flips this
    /// back to `false` immediately after each draw.
    pub needs_redraw: bool,
}

/// `App::log_view_width`'s pre-first-frame fallback — an unremarkable
/// terminal width, just enough to make `wrap_log_lines` behave sanely
/// (never `0`, which would drop every log line — see `wrap_log_lines`'s own
/// `width == 0` early return) if a `/`/`?` search somehow fired before
/// `Mode::Log` ever rendered a single frame.
const DEFAULT_LOG_VIEW_WIDTH: u16 = 80;

/// Looks up `path`'s extension (lowercased, dot stripped — matching the
/// documented `[viewers]` key format) in `viewers`, returning the command
/// template to run instead of the built-in viewer. Falls back to
/// `fallback_target`'s extension (a symlink's resolved target — see
/// `App::begin_open`) when `path`'s own extension has no configured entry,
/// so `mylink` (no extension) pointing at `notes.md` still picks up an
/// `md` viewer, while `mylink.txt -> notes.md` still prefers the link's
/// own `txt` entry if one exists. `None` when neither has a match (or
/// neither has an extension at all) — either way `App::begin_open` falls
/// back to the built-in viewer. A free function (rather than a method)
/// purely so it's directly unit-testable without constructing an `App`.
fn extension_viewer_command(
    viewers: &std::collections::HashMap<String, String>,
    path: &Path,
    fallback_target: Option<&Path>,
) -> Option<String> {
    extension_key(path)
        .and_then(|ext| viewers.get(&ext).cloned())
        .or_else(|| {
            fallback_target
                .and_then(extension_key)
                .and_then(|ext| viewers.get(&ext).cloned())
        })
}

/// `path`'s extension, lowercased — the `[viewers]` key format. `None` for
/// a path with none.
fn extension_key(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_lowercase())
}

/// Appends one `LogLine` (capacity-capped, timestamped `Local::now()`) to
/// `log` directly, bumping `*generation` so any `(generation, width)`-keyed
/// wrap cache (see `App::log_wrapped`) knows to rebuild. A free function
/// rather than an `&mut self` method so it can be called from inside a loop
/// that already holds a `&mut self.panes` borrow (see `App::reload_both`) —
/// `App::log_push` is a thin wrapper around this for the common case where
/// no such conflict exists.
fn push_log_line(
    log: &mut VecDeque<LogLine>,
    generation: &mut u64,
    message: String,
    is_error: bool,
) {
    if log.len() >= LOG_CAPACITY {
        log.pop_front();
    }
    log.push_back(LogLine::new(message, is_error, Local::now()));
    *generation = generation.wrapping_add(1);
}

/// Builds a `Keymap` from `config`: the compiled-in defaults, with `[keys]`
/// merged in and then `[bindings]` applied on top (so `[bindings]` wins on
/// any combo both sections mention — see `Keymap::apply_bindings`). Shared
/// by `App::new` (startup) and `App::apply_reloaded_config` (`,`'s live
/// reload), so the two can never build a keymap two different ways.
fn build_keymap(config: &Config) -> anyhow::Result<Keymap> {
    let mut keymap = Keymap::defaults();
    keymap
        .merge_overrides(&config.keys)
        .context("invalid [keys] entry in config")?;
    keymap
        .apply_bindings(&config.bindings)
        .context("invalid [bindings] entry in config")?;
    Ok(keymap)
}

/// What `Mode::Viewer`'s `/`/`?` search matches against: the decoded text
/// lines as-is in text mode, or every row's `xxd`-style formatting in hex
/// mode (see `viewer::format_hex_lines`) — spec'd as "search the formatted
/// line strings" rather than the raw bytes, so a hex search behaves the
/// way it looks on screen. `Cow` so text mode (the common case, and
/// potentially a 10 MiB file's worth of lines) never clones; only hex
/// mode's on-demand formatting allocates.
fn viewer_search_haystack<'a>(
    lines: &'a [String],
    bytes: &[u8],
    view_mode: ViewMode,
) -> std::borrow::Cow<'a, [String]> {
    match view_mode {
        ViewMode::Text => std::borrow::Cow::Borrowed(lines),
        ViewMode::Hex => std::borrow::Cow::Owned(viewer::format_hex_lines(bytes)),
    }
}

impl App {
    /// Builds the app with both panes already loaded — a plain
    /// `new_unloaded` followed immediately by `load_initial_dirs`. Every
    /// test in this codebase uses this (via `test_app`/direct calls) and
    /// expects a ready-to-use listing straight away; `main.rs`'s real
    /// startup path uses `new_unloaded` instead so it can draw a first
    /// frame before either directory read runs — see that method's doc
    /// comment.
    pub fn new(left: PathBuf, right: PathBuf, config: Config) -> anyhow::Result<Self> {
        let mut app = Self::new_unloaded(left, right, config)?;
        app.load_initial_dirs()?;
        Ok(app)
    }

    /// Builds the app with both panes empty and unread — no filesystem
    /// access at all beyond what `build_keymap`/`Config` already did.
    /// `main.rs` uses this so the terminal's very first frame (entering
    /// the alternate screen, an empty two-pane layout) can be drawn
    /// *before* `load_initial_dirs`'s directory reads ever run, so a slow
    /// mount (NFS/SMB) or a huge directory never leaves the screen blank
    /// and frozen right after startup with no feedback at all.
    pub fn new_unloaded(left: PathBuf, right: PathBuf, config: Config) -> anyhow::Result<Self> {
        let keymap = build_keymap(&config)?;
        let (tx, task_rx) = mpsc::channel();

        let mut left = Pane::new_empty(left);
        let mut right = Pane::new_empty(right);
        left.set_natural_sort(config.natural_sort);
        right.set_natural_sort(config.natural_sort);

        Ok(Self {
            panes: [left, right],
            active: ActivePane::Left,
            should_quit: false,
            mode: Mode::Normal,
            config,
            keymap,
            log: VecDeque::new(),
            tasks: TaskManager::new(tx),
            task_rx,
            history: History::default(),
            bookmarks: Bookmarks::default(),
            sort_prefs: SortPrefs::default(),
            outbox: Outbox::default(),
            pane_layout: [None, None],
            drag: None,
            last_click: None,
            pending_delete_anchor: HashMap::new(),
            pending_dir_size: HashMap::new(),
            pending_git_status: HashMap::new(),
            latest_git_task: [None, None],
            git_checked_dir: [None, None],
            settings_config_path: None,
            log_view_width: DEFAULT_LOG_VIEW_WIDTH,
            log_generation: 0,
            log_wrap_cache: None,
            help_lines_cache: None,
            settings_keybinding_lines_cache: None,
            function_list_cache: None,
            needs_redraw: true,
        })
    }

    /// Performs the initial directory read for both panes that
    /// `new_unloaded` skipped — see its doc comment. Left/right are loaded
    /// in that fixed order and the first failure is returned immediately
    /// (matching `new`'s previous all-in-one-constructor behavior, back
    /// when `Pane::new`'s own `?` did this inline); `main.rs`'s `run` calls
    /// this once, right after drawing the first (empty) frame.
    pub fn load_initial_dirs(&mut self) -> anyhow::Result<()> {
        self.panes[0].reload()?;
        self.panes[1].reload()?;
        self.needs_redraw = true;
        Ok(())
    }

    /// Applies this frame's on-screen geometry, as reported by
    /// [`ui::draw`](crate::ui::draw) — called once per draw by `main.rs`'s
    /// loop, right after `terminal.draw` returns. Split out from `draw`
    /// itself (which returns a [`ui::LayoutFeedback`](crate::ui::LayoutFeedback)
    /// rather than writing `pane_layout`/`log_view_width` directly) so that
    /// no render function ever mutates `App`'s externally-visible state as
    /// a side effect of drawing — see that struct's doc comment for the
    /// full reasoning. Each field left `None` this frame (a Viewer/Help/Log/
    /// Settings full-frame takeover doesn't touch pane geometry; only
    /// `Mode::Log`'s takeover touches the log width) simply leaves the
    /// corresponding `App` field at whatever it already was, the same
    /// "stale until a relevant frame draws, harmless" behavior both fields
    /// always had.
    pub fn apply_layout_feedback(&mut self, feedback: crate::ui::LayoutFeedback) {
        if let Some([left, right]) = feedback.panes {
            self.pane_layout = [Some(left), Some(right)];
        }
        if let Some(width) = feedback.log_view_width {
            self.log_view_width = width;
        }
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active.index()]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active.index()]
    }

    /// The pane that *isn't* active — e.g. a transfer/extract's implicit
    /// destination, or a virtual-directory check on "the other side".
    pub fn other_pane(&self) -> &Pane {
        &self.panes[self.active.other().index()]
    }

    pub fn other_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active.other().index()]
    }

    /// Takes every queued `Outbox` output at once, resetting it back to
    /// empty — `main.rs`'s loop calls this exactly once per iteration
    /// instead of separately polling (and clearing) four fields by hand.
    pub fn take_outbox(&mut self) -> Outbox {
        std::mem::take(&mut self.outbox)
    }

    /// Whether mouse capture should be active *right now* — consulted
    /// once per main-loop iteration by `main.rs`'s `sync_mouse_capture`,
    /// which is what actually enables/disables it on the real terminal.
    /// `false` while a full-frame text-*reading* mode (Viewer/Log/Help) is
    /// showing, so the terminal's own native text selection and
    /// scrollback take over instead of ozzel's own click/wheel handling —
    /// `true` everywhere else, including the Function List command
    /// palette (an interactive picker, not a reading mode, so it keeps
    /// capture on even though it has no mouse behavior of its own).
    /// Always `false` when `config.mouse` itself is off, regardless of
    /// mode.
    pub fn wants_mouse_capture(&self) -> bool {
        self.config.mouse
            && !matches!(
                self.mode,
                Mode::Viewer { .. } | Mode::Log { .. } | Mode::Help { .. }
            )
    }

    fn log_push(&mut self, message: String, is_error: bool) {
        push_log_line(&mut self.log, &mut self.log_generation, message, is_error);
    }

    pub fn log_info(&mut self, message: impl Into<String>) {
        self.log_push(message.into(), false);
    }

    pub fn log_error(&mut self, message: impl Into<String>) {
        self.log_push(message.into(), true);
    }

    /// Public entry point for `main.rs` to call after resuming from a
    /// suspended external command (per the plan's loop sketch): the
    /// child process may have created/deleted/renamed files under either
    /// pane, so both get a full reload.
    pub fn refresh_panes(&mut self) {
        self.reload_both();
    }

    /// Reloads both panes, trying to keep each pane's cursor on the
    /// same-named entry it was on before (see
    /// `Pane::reload_preserving_cursor`). Reload failures are logged, not
    /// propagated — one pane's unreadable directory shouldn't crash the UI.
    fn reload_both(&mut self) {
        self.reload_both_with_delete_anchor(None);
    }

    /// The actual body behind both `reload_both` (no anchor — every
    /// ordinary task completion) and a delete task's completion (which
    /// does have one): reloads both panes, using `Pane::reload_preserving_cursor`
    /// as usual *except* for whichever pane `anchor` names, if any, which
    /// gets `Pane::reload_preserving_cursor_onto` instead — see
    /// `Pane::anchor_above`'s doc comment for why a delete needs this
    /// distinct treatment.
    fn reload_both_with_delete_anchor(
        &mut self,
        anchor: Option<(ActivePane, Option<CursorAnchor>)>,
    ) {
        for (i, pane) in self.panes.iter_mut().enumerate() {
            let this_pane = if i == 0 {
                ActivePane::Left
            } else {
                ActivePane::Right
            };
            let result = match &anchor {
                Some((p, a)) if *p == this_pane => pane.reload_preserving_cursor_onto(a.clone()),
                _ => pane.reload_preserving_cursor(),
            };
            if let Err(err) = result {
                // Can't call `self.log_error` here: `pane` already holds a
                // `&mut self.panes` borrow, and a `&mut self` method call
                // would conflict with it even though `log`/`log_generation`
                // are disjoint fields — so this goes through the free
                // function instead, borrowing only those two directly.
                push_log_line(
                    &mut self.log,
                    &mut self.log_generation,
                    err.to_string(),
                    true,
                );
            }
        }
        // Whatever prompted this reload (a finished task, a synchronous
        // file operation, C-r) likely changed git state too — force the
        // next `maybe_refresh_git` sweep to re-probe both panes.
        self.git_checked_dir = [None, None];
    }

    /// Drains every `TaskEvent` currently waiting on the channel. Called
    /// once per main-loop iteration, before the next terminal poll, so
    /// progress/log/finish handling never waits behind a keystroke.
    pub fn drain_tasks(&mut self) {
        while let Ok(event) = self.task_rx.try_recv() {
            self.handle_event(AppEvent::Task(event));
        }
    }

    fn handle_task_event(&mut self, event: TaskEvent) {
        if let TaskEvent::Log { line, .. } = &event {
            self.log_info(line.clone());
        }
        // A dir-size result is routed straight onto the pane its task was
        // started from (`pending_dir_size`, keyed by TaskId) —
        // `Pane::set_dir_size` itself drops it if that pane has since
        // moved to a different directory.
        if let TaskEvent::DirSize { id, path, bytes } = &event {
            if let Some(&target) = self.pending_dir_size.get(id) {
                self.panes[target.index()].set_dir_size(path.clone(), *bytes);
            }
            return;
        }
        // A git-status result lands on its pane only if it's still
        // current on *both* axes: the newest probe spawned for that pane
        // (an older, slower run's result must never overwrite a newer
        // one's) and still the directory the pane is showing.
        if matches!(event, TaskEvent::GitStatus { .. }) {
            let TaskEvent::GitStatus { id, dir, status } = event else {
                return;
            };
            if let Some(&target) = self.pending_git_status.get(&id) {
                let idx = target.index();
                let is_latest = self.latest_git_task[idx]
                    .as_ref()
                    .is_some_and(|(latest, _)| *latest == id);
                let pane = &mut self.panes[idx];
                if is_latest && pane.cwd == dir && !pane.is_virtual() {
                    pane.set_git_status(status);
                }
            }
            return;
        }
        // A git-status probe's `Finished` is bookkeeping only — unlike
        // every real file operation's completion, it must never log a
        // summary line, reload the panes, or clear marks (probes are
        // passive; the regression test for the no-side-effects rule lives
        // in `app/tests/git_status.rs`). Probes are detached, so there's
        // no `running` entry to clean up either. Failures other than a
        // routine cancellation do get logged.
        if let TaskEvent::Finished { id, result } = &event
            && self.pending_git_status.remove(id).is_some()
        {
            for latest in &mut self.latest_git_task {
                if latest.as_ref().is_some_and(|(t, _)| t == id) {
                    *latest = None;
                }
            }
            if let Err(err) = result
                && err != "cancelled"
            {
                self.log_error(format!("git status: {err}"));
            }
            return;
        }
        let finished = matches!(event, TaskEvent::Finished { .. });
        // Only ever `Some` when `event` is this *exact* delete task's own
        // `Finished` — an unrelated task finishing first (or a second,
        // concurrent delete) can never pick up an anchor that isn't
        // theirs, since each is keyed by its own `TaskId`.
        let delete_anchor = if let TaskEvent::Finished { id, .. } = &event {
            self.pending_dir_size.remove(id);
            self.pending_delete_anchor.remove(id)
        } else {
            None
        };

        if let Some((summary, is_error)) = self.tasks.apply_event(&event) {
            if is_error {
                self.log_error(summary);
            } else {
                self.log_info(summary);
            }
        }

        if finished {
            // A finished transfer/delete may have touched either pane
            // (source and destination), so reload and unmark both rather
            // than trying to track exactly which one.
            self.reload_both_with_delete_anchor(delete_anchor);
            for pane in &mut self.panes {
                pane.clear_marks();
            }
        }
    }

    /// Routes a normalized terminal event: `Normal` mode consults the
    /// `Keymap`, `Prompt`/`Confirm` consume fixed editing/confirmation keys
    /// directly (they never look at the keymap). `Task` events update
    /// running-task state and the log; `Tick` is a no-op.
    pub fn handle_event(&mut self, event: AppEvent) {
        // Deliberately coarse — see `needs_redraw`'s doc comment: anything
        // but a bare `Tick` (a poll timeout with nothing to report) marks
        // the whole frame dirty, rather than tracking exactly which part
        // of the UI a given event actually touched. Over-drawing is
        // harmless; under-drawing is a bug.
        if !matches!(event, AppEvent::Tick) {
            self.needs_redraw = true;
        }
        match event {
            AppEvent::Input(code, modifiers) => match &self.mode {
                Mode::Normal => {
                    if let Some(action) = self.keymap.resolve(code, modifiers) {
                        self.dispatch(action);
                    }
                }
                Mode::Filter { .. } => self.handle_filter_key(code, modifiers),
                Mode::JumpSearch { .. } => self.handle_jump_search_key(code, modifiers),
                Mode::Select { .. } => self.handle_select_key(code, modifiers),
                Mode::Prompt { .. } => self.handle_prompt_key(code, modifiers),
                Mode::Confirm { .. } => self.handle_confirm_key(code),
                Mode::TransferCollision { .. } => {
                    self.handle_transfer_collision_key(code, modifiers)
                }
                Mode::Viewer { .. } => self.handle_viewer_key(code, modifiers),
                Mode::Help { .. } => self.handle_help_key(code, modifiers),
                Mode::Log { .. } => self.handle_log_view_key(code, modifiers),
                Mode::FunctionList { .. } => self.handle_function_list_key(code, modifiers),
                Mode::FileSearch { .. } => self.handle_file_search_key(code, modifiers),
                Mode::SortSelect { .. } => self.handle_sort_select_key(code, modifiers),
                Mode::SyncSelect { .. } => self.handle_sync_select_key(code, modifiers),
                Mode::Chmod { .. } => self.handle_chmod_key(code),
                Mode::FileInfo { .. } => self.handle_file_info_key(code),
                Mode::Settings { .. } => self.handle_settings_key(code, modifiers),
            },
            AppEvent::Mouse(mouse_event) => self.handle_mouse(mouse_event),
            AppEvent::Task(task_event) => self.handle_task_event(task_event),
            // No app state to update — `needs_redraw` (set above) is the
            // entire point of this variant; the next draw picks up the
            // terminal's new size on its own (see `AppEvent::Resize`'s doc
            // comment).
            AppEvent::Resize => {}
            AppEvent::Tick => {}
        }
        // After every event (Ticks included — that's what makes the very
        // first probe fire right after startup, before any keypress):
        // spawn a git-status probe for any pane whose cwd changed since
        // its last probe. One PathBuf comparison per pane when nothing
        // changed, so running this unconditionally is cheap.
        self.maybe_refresh_git();
    }

    /// The single chokepoint deciding when a pane's git status gets
    /// (re-)probed: whenever `git_checked_dir` disagrees with the pane's
    /// current cwd — which covers every navigation route (enter, parent,
    /// history, bookmarks, jumps, swap, startup) without instrumenting
    /// any of them — plus whenever something reset `git_checked_dir` to
    /// `None` to force it (task completions and synchronous file
    /// operations do, via `reload_both`). Virtual (archive) panes and
    /// `show_git_status = false` clear the status instead of probing.
    fn maybe_refresh_git(&mut self) {
        for idx in 0..2 {
            if !self.config.show_git_status || self.panes[idx].is_virtual() {
                if self.git_checked_dir[idx].is_some() {
                    self.git_checked_dir[idx] = None;
                    self.panes[idx].set_git_status(None);
                }
                continue;
            }
            if self.git_checked_dir[idx].as_deref() == Some(self.panes[idx].cwd.as_path()) {
                continue;
            }
            self.refresh_git_status(idx);
        }
    }

    /// Spawns one background `git status` probe for pane `idx`'s cwd,
    /// canceling whatever previous probe was still in flight for it (its
    /// result would be dropped anyway — see `latest_git_task`). Detached
    /// (`spawn_detached`): a passive probe must never show up as a
    /// running task, gate quitting, or be swept up by `cancel_tasks`.
    fn refresh_git_status(&mut self, idx: usize) {
        if let Some((_, prev_cancel)) = self.latest_git_task[idx].take() {
            prev_cancel.store(true, Ordering::Relaxed);
        }
        let dir = self.panes[idx].cwd.clone();
        let worker_dir = dir.clone();
        let (id, cancel) = self.tasks.spawn_detached(move |id, tx, cancel| {
            crate::tasks::git_status::run_git_status(id, tx, cancel, worker_dir);
        });
        let side = if idx == 0 {
            ActivePane::Left
        } else {
            ActivePane::Right
        };
        self.pending_git_status.insert(id, side);
        self.latest_git_task[idx] = Some((id, cancel));
        self.git_checked_dir[idx] = Some(dir);
    }

    /// The single match hub every action flows through. Kept infallible on
    /// the outside (errors are logged instead of propagated) so the input
    /// loop never has to think about failure.
    pub fn dispatch(&mut self, action: Action) {
        let result: anyhow::Result<()> = match action {
            Action::CursorUp => {
                self.move_cursor_step(-1);
                Ok(())
            }
            Action::CursorDown => {
                self.move_cursor_step(1);
                Ok(())
            }
            Action::PageUp => {
                self.active_pane_mut().move_cursor(-(PAGE_SIZE as isize));
                Ok(())
            }
            Action::PageDown => {
                self.active_pane_mut().move_cursor(PAGE_SIZE as isize);
                Ok(())
            }
            Action::Top => {
                self.active_pane_mut().cursor_to_top();
                Ok(())
            }
            Action::Bottom => {
                self.active_pane_mut().cursor_to_bottom();
                Ok(())
            }
            Action::SwitchPane => {
                self.active = self.active.other();
                Ok(())
            }
            Action::FocusLeft => {
                self.active = ActivePane::Left;
                Ok(())
            }
            Action::FocusRight => {
                self.active = ActivePane::Right;
                Ok(())
            }
            Action::Open => {
                self.begin_open();
                Ok(())
            }
            Action::Parent => {
                self.navigate(|pane| pane.go_parent());
                Ok(())
            }
            Action::CycleSort => {
                self.active_pane_mut().cycle_sort();
                self.record_sort_pref();
                Ok(())
            }
            Action::SortDialog => {
                self.begin_sort_dialog();
                Ok(())
            }
            Action::ToggleSizeFormat => {
                self.toggle_size_format();
                Ok(())
            }
            Action::CalcDirSize => {
                self.begin_calc_dir_size();
                Ok(())
            }
            Action::ToggleHidden => {
                self.active_pane_mut().toggle_hidden();
                Ok(())
            }
            Action::SwapPanes => {
                self.panes.swap(0, 1);
                Ok(())
            }
            Action::Refresh => {
                self.reload_both();
                Ok(())
            }
            Action::Mark => {
                self.active_pane_mut().toggle_mark_cursor();
                Ok(())
            }
            Action::MarkAll => {
                self.active_pane_mut().toggle_mark_all();
                Ok(())
            }
            Action::Rename => {
                self.begin_rename();
                Ok(())
            }
            Action::RenameMarks => {
                self.begin_rename_marks();
                Ok(())
            }
            Action::Mkdir => {
                self.begin_mkdir();
                Ok(())
            }
            Action::Delete => {
                self.begin_delete();
                Ok(())
            }
            Action::Copy => {
                self.begin_transfer(TransferKind::Copy);
                Ok(())
            }
            Action::Move => {
                self.begin_transfer(TransferKind::Move);
                Ok(())
            }
            Action::Filter => {
                self.mode = Mode::Filter {
                    input: LineEditor::new(),
                };
                Ok(())
            }
            Action::ClearFilter => {
                self.active_pane_mut().set_filter(None);
                Ok(())
            }
            Action::JumpSearch => {
                self.begin_jump_search();
                Ok(())
            }
            Action::FileSearch => {
                self.begin_file_search();
                Ok(())
            }
            Action::ZipMarked => {
                self.begin_zip();
                Ok(())
            }
            Action::Unzip => {
                self.begin_unzip();
                Ok(())
            }
            Action::HistoryJump => {
                self.begin_history_jump();
                Ok(())
            }
            Action::BookmarkJump => {
                self.begin_bookmark_jump();
                Ok(())
            }
            Action::BookmarkAdd => {
                self.begin_bookmark_add();
                Ok(())
            }
            Action::GoHome => {
                self.begin_go_home();
                Ok(())
            }
            Action::CommandLine => {
                self.mode = Mode::Prompt {
                    kind: PromptKind::Command,
                    input: LineEditor::new(),
                };
                Ok(())
            }
            Action::OpenEditor => {
                self.begin_open_editor();
                Ok(())
            }
            Action::OpenDefault => {
                self.begin_open_default();
                Ok(())
            }
            Action::Help => {
                self.begin_help();
                Ok(())
            }
            Action::EditConfig => {
                self.begin_edit_config();
                Ok(())
            }
            Action::HistoryBack => {
                self.begin_history_back();
                Ok(())
            }
            Action::HistoryForward => {
                self.begin_history_forward();
                Ok(())
            }
            Action::ShowLog => {
                self.begin_show_log();
                Ok(())
            }
            Action::CopyPath => {
                self.begin_copy_path();
                Ok(())
            }
            Action::CopyDirPath => {
                self.begin_copy_dir_path();
                Ok(())
            }
            Action::Duplicate => {
                self.begin_duplicate();
                Ok(())
            }
            Action::FunctionList => {
                self.begin_function_list();
                Ok(())
            }
            Action::Settings => {
                self.begin_settings();
                Ok(())
            }
            Action::Quit => {
                self.begin_quit();
                Ok(())
            }
            Action::CancelTasks => {
                self.cancel_running_tasks();
                Ok(())
            }
            Action::Symlink => {
                self.begin_symlink();
                Ok(())
            }
            Action::Chmod => {
                self.begin_chmod();
                Ok(())
            }
            Action::Touch => {
                self.begin_touch();
                Ok(())
            }
            Action::FileInfo => {
                self.begin_file_info();
                Ok(())
            }
            Action::Diff => {
                self.begin_diff();
                Ok(())
            }
            Action::SyncDirs => {
                self.begin_sync_dirs();
                Ok(())
            }
        };

        if let Err(err) = result {
            self.log_error(err.to_string());
        }
    }

    /// Tasks running always confirm ("N task(s) running — quit anyway?"),
    /// regardless of `confirm_quit` — the spawned worker threads are
    /// detached and get killed outright (not gracefully stopped) if the
    /// process exits while they're still writing, so that confirmation
    /// isn't optional. With nothing running, `confirm_quit` (default
    /// `true`) decides: confirm with a plain "Quit ozzel?" prompt, or quit
    /// immediately when set to `false`.
    fn begin_quit(&mut self) {
        if !self.tasks.running.is_empty() {
            let message = format!(
                "{} task(s) running — quit anyway? (y/n)",
                self.tasks.running.len()
            );
            self.confirm(message, PendingOp::Quit);
            return;
        }

        if self.config.confirm_quit {
            self.confirm("Quit ozzel? (y/n)", PendingOp::Quit);
            return;
        }

        self.should_quit = true;
    }

    /// `Action::CancelTasks`: sets *every* currently-running task's cancel
    /// flag at once — a single "stop everything" action rather than a
    /// per-task selection UI, since there's no way to distinguish one
    /// running task from another in the UI beyond its log gauge row. Every
    /// worker (copy/move/delete/zip/unzip/extract, via `TaskManager::spawn`'s
    /// `Arc<AtomicBool>`) already polls this flag between files/chunks and
    /// unwinds to its own `Finished(Err("cancelled"))` on its own thread;
    /// this only flips the flag and returns immediately, it never waits for
    /// a worker thread to actually stop.
    fn cancel_running_tasks(&mut self) {
        let n = self.tasks.running.len();
        if n == 0 {
            self.log_info("no running tasks");
            return;
        }
        for task in self.tasks.running.values() {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.log_info(format!("cancelling {n} task(s)"));
    }

    /// Single-step cursor movement (`CursorUp`/`CursorDown`) — the one
    /// place `config.cursor_wrap` is consulted. Page/Home/End/wheel
    /// movement always clamps (see `Pane::move_cursor_wrapping`).
    fn move_cursor_step(&mut self, delta: isize) {
        if self.config.cursor_wrap {
            self.active_pane_mut().move_cursor_wrapping(delta);
        } else {
            self.active_pane_mut().move_cursor(delta);
        }
    }

    /// The fixed (key, direction) rows the sort dialog shows, in cursor
    /// order. Kept as a const so the key handler and the renderer index
    /// the exact same list.
    pub const SORT_DIALOG_CHOICES: [(SortKey, bool); 8] = [
        (SortKey::Name, true),
        (SortKey::Name, false),
        (SortKey::Size, true),
        (SortKey::Size, false),
        (SortKey::MTime, true),
        (SortKey::MTime, false),
        (SortKey::Ext, true),
        (SortKey::Ext, false),
    ];

    /// `t`: opens the sort dialog with the cursor preselecting the active
    /// pane's current (key, direction).
    fn begin_sort_dialog(&mut self) {
        let pane = self.active_pane();
        let current = (pane.sort, pane.ascending);
        let cursor = Self::SORT_DIALOG_CHOICES
            .iter()
            .position(|&c| c == current)
            .unwrap_or(0);
        self.mode = Mode::SortSelect { cursor };
    }

    fn handle_sort_select_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Resolved before the `&mut self.mode` borrow below, and as a plain
        // `Copy` value — see `handle_select_key`.
        let nav = self.keymap.menu_nav(code, modifiers);
        let Mode::SortSelect { cursor } = &mut self.mode else {
            return;
        };
        let len = Self::SORT_DIALOG_CHOICES.len();
        match code {
            KeyCode::Up => *cursor = cursor.checked_sub(1).unwrap_or(len - 1),
            KeyCode::Down => *cursor = (*cursor + 1) % len,
            KeyCode::Enter => {
                let (key, ascending) = Self::SORT_DIALOG_CHOICES[*cursor];
                self.mode = Mode::Normal;
                self.active_pane_mut().set_sort(key, ascending);
                self.record_sort_pref();
            }
            KeyCode::Esc => self.mode = Mode::Normal,
            _ => match nav {
                Some(MenuNav::Up) => *cursor = cursor.checked_sub(1).unwrap_or(len - 1),
                Some(MenuNav::Down) => *cursor = (*cursor + 1) % len,
                None => {}
            },
        }
    }

    /// Records the active pane's current sort state as the preference for
    /// its cwd — called after any explicit sort change (`s`'s cycle, the
    /// `t` dialog's Enter). Skipped while virtual: `cwd` still points at
    /// the real directory *containing* the archive, so recording would
    /// mis-attribute an archive-internal sort change to it.
    fn record_sort_pref(&mut self) {
        if self.active_pane().is_virtual() {
            return;
        }
        let pane = self.active_pane();
        let (cwd, key, ascending) = (pane.cwd.clone(), pane.sort, pane.ascending);
        self.sort_prefs.record(cwd, key.as_str(), ascending);
    }

    /// Restores the active pane's remembered sort for its (new) cwd, if
    /// one was recorded; otherwise the pane keeps whatever sort it already
    /// had (the pre-existing "sort follows the pane" behavior).
    fn apply_sort_pref_to_active(&mut self) {
        let cwd = self.active_pane().cwd.clone();
        if let Some((key_str, ascending)) = self.sort_prefs.get(&cwd)
            && let Some(key) = SortKey::from_str(key_str)
        {
            self.active_pane_mut().set_sort(key, ascending);
        }
    }

    /// Applies loaded sort preferences to both panes' startup directories —
    /// called by `main.rs` once, right after it overwrites `sort_prefs`
    /// with the persisted file's contents (`App` itself never touches
    /// disk, same story as `history`).
    pub fn apply_startup_sort_prefs(&mut self) {
        for pane in &mut self.panes {
            if let Some((key_str, ascending)) = self.sort_prefs.get(&pane.cwd)
                && let Some(key) = SortKey::from_str(key_str)
            {
                pane.set_sort(key, ascending);
            }
        }
    }

    /// `v`: cycles the size column format, persists the choice to the
    /// config file (best-effort — a write failure is logged and the
    /// in-memory setting still applies), and logs the new state.
    fn toggle_size_format(&mut self) {
        let next = self.config.size_format.next();
        self.config.size_format = next;
        self.log_info(format!("size format: {}", next.as_str()));
        let path = self
            .settings_config_path
            .clone()
            .or_else(config::config_path);
        match path {
            Some(path) => {
                if let Err(err) = settings::save_size_format(&path, next) {
                    self.log_error(format!("failed to save size_format: {err}"));
                }
            }
            None => self.log_error("failed to save size_format: no config directory available"),
        }
    }

    /// Runs `f` against the active pane and, if its `cwd` actually
    /// changed, records the new directory in `history`. The shared
    /// entry point for every cwd-changing action (`Enter`, `Parent`,
    /// history/bookmark/home jumps) so history recording lives in exactly
    /// one place instead of being duplicated at each call site.
    fn navigate(&mut self, f: impl FnOnce(&mut Pane) -> anyhow::Result<()>) {
        let before = self.active_pane().cwd.clone();
        let result = f(self.active_pane_mut());
        self.record_history_if_changed(&before);
        if let Err(err) = result {
            self.log_error(err.to_string());
        }
    }

    /// Records `before` in the persisted MRU history *and* pushes it onto
    /// the active pane's `back` stack (clearing `forward`, same as a real
    /// browser navigating somewhere new) — but only when `cwd` actually
    /// changed, so a failed/no-op navigation doesn't pollute either. This
    /// is the single choke point every cwd-changing action goes through
    /// (`navigate`), so `history_back`/`history_forward` deliberately
    /// bypass it (see `begin_history_back`) — walking the stack must not
    /// re-push onto itself.
    fn record_history_if_changed(&mut self, before: &Path) {
        let after = self.active_pane().cwd.clone();
        if after.as_path() != before {
            self.history.record(self.active.into(), after);
            let pane = self.active_pane_mut();
            pane.back.push(before.to_path_buf());
            pane.forward.clear();
            self.apply_sort_pref_to_active();
        }
    }

    /// `S-left`: pops this pane's `back` stack and jumps there, pushing the
    /// current cwd onto `forward` (so `S-right` can return). Empty stack ->
    /// logged, stays put. Uses `Pane::jump_to` directly rather than
    /// `navigate`, since going through it would push back onto `back`
    /// again — the opposite of what "go back" means.
    fn begin_history_back(&mut self) {
        let Some(target) = self.active_pane_mut().back.pop() else {
            self.log_error("no earlier directory in this pane's history");
            return;
        };
        let current = self.active_pane().cwd.clone();
        match self.active_pane_mut().jump_to(target.clone()) {
            Ok(()) => {
                self.active_pane_mut().forward.push(current);
                // Bypasses `navigate`/`record_history_if_changed`, so the
                // per-directory sort restore has to happen here too.
                self.apply_sort_pref_to_active();
            }
            Err(err) => {
                // jump_to already reverted cwd on failure; put the target
                // back so a retry (or S-left again) isn't silently lossy.
                self.active_pane_mut().back.push(target);
                self.log_error(err.to_string());
            }
        }
    }

    /// `S-right`: the mirror of `begin_history_back`.
    fn begin_history_forward(&mut self) {
        let Some(target) = self.active_pane_mut().forward.pop() else {
            self.log_error("no later directory in this pane's history");
            return;
        };
        let current = self.active_pane().cwd.clone();
        match self.active_pane_mut().jump_to(target.clone()) {
            Ok(()) => {
                self.active_pane_mut().back.push(current);
                // Same bypass as `begin_history_back` — restore here.
                self.apply_sort_pref_to_active();
            }
            Err(err) => {
                self.active_pane_mut().forward.push(target);
                self.log_error(err.to_string());
            }
        }
    }

    fn jump_active_pane_to(&mut self, path: PathBuf) {
        self.navigate(|pane| pane.jump_to(path));
    }

    /// Enters `Mode::Confirm`: shows `message`, then runs `on_yes` (via
    /// `execute_pending`) if the user answers `y`. The "ask, then run a
    /// `PendingOp` on confirmation" shape every `begin_*`/`commit_*` that
    /// needs a yes/no gate (quit, delete, an overwrite, ...) ends with.
    fn confirm(&mut self, message: impl Into<String>, on_yes: PendingOp) {
        self.mode = Mode::Confirm {
            message: message.into(),
            on_yes,
        };
    }

    /// Guards every action that needs a real filesystem path on the active
    /// pane and doesn't have virtual-mode semantics of its own (`open`
    /// and `C`/Copy — repurposed as extract — are the two exceptions,
    /// handled separately in `begin_open`/`begin_transfer`): logs a
    /// rejection and returns `true` when the active pane is currently
    /// browsing inside an archive (Virtual Directory — zip or tar
    /// family), so the caller can bail out early. `label` names the
    /// action in the log line (e.g. `"rename"`).
    fn reject_if_virtual(&mut self, label: &str) -> bool {
        if self.active_pane().is_virtual() {
            self.log_error(format!(
                "virtual directory (archive) is read-only: {label} is not available here"
            ));
            true
        } else {
            false
        }
    }

    /// `Open`'s behavior (bound to `Enter`/`o` by default, and the single
    /// action `Enter`/`View` used to be split across before they were
    /// merged): `..`/directories navigate (and get recorded in history
    /// via `navigate`); anything else opens in the built-in viewer.
    ///
    /// Virtual Directory (browsing an archive — zip, or the tar family via
    /// `virtual_dir::ArchiveKind::Tar` — as if it were a directory — see
    /// `virtual_dir`) hooks into exactly this one function, not a parallel
    /// code path: a recognized archive *file* under the cursor in a real
    /// pane enters Virtual Directory mode instead of the viewer; a file
    /// inside an already-virtual pane extracts to memory and opens the
    /// viewer instead of reading from disk; `..`/directory navigation is
    /// unmodified — it already goes through `Pane::enter`, which is
    /// itself virtual-aware (see `Pane::virtual_descend`/
    /// `virtual_go_parent`) and never changes `cwd` while virtual, so
    /// `navigate`'s history bookkeeping silently no-ops for every
    /// archive-internal move (before == after). An archive found *inside*
    /// another one is deliberately not recursed into (no nested Virtual
    /// Directories) — it just opens in the viewer like any other file,
    /// which for a zip/tar's own binary content usually means a hex dump;
    /// harmless, not an error.
    fn begin_open(&mut self) {
        let pane = self.active_pane();
        let is_virtual = pane.is_virtual();
        // `..`/directories — and directory-symlinks, see
        // `FsEntry::is_dir_like` — navigate (via `Pane::enter`, which
        // already handles both, and is a safe no-op on an empty pane);
        // anything else (file, file-symlink, dangling symlink) opens
        // instead.
        let selected = pane.selected_entry();
        let selected_kind = selected.map(|e| e.kind);
        let open_path = match selected.map(|e| e.is_dir_like()) {
            Some(false) => selected.map(|e| e.path.clone()),
            _ => None,
        };
        let archive_path = pane.virtual_dir.as_ref().map(|vd| vd.archive_path.clone());
        // A file-symlink's `[viewers]` lookup tries the *link's* name
        // extension first (`open_path`, same as any other entry) and only
        // falls back to the target's extension if that doesn't match
        // anything — see `extension_viewer_command`. Resolving the target
        // only costs a `canonicalize` call, and only for a symlink whose
        // `open_path` is even present (a dangling one has none to resolve
        // anyway, and `canonicalize` on it would just fail harmlessly).
        let fallback_ext_target = match (selected_kind, &open_path) {
            (Some(EntryKind::Symlink), Some(path)) => std::fs::canonicalize(path).ok(),
            _ => None,
        };
        let viewer_cmd = open_path.as_ref().and_then(|path| {
            extension_viewer_command(&self.config.viewers, path, fallback_ext_target.as_deref())
        });

        match open_path {
            Some(inner_path) if is_virtual => {
                // External viewers need a real file on disk; a virtual
                // entry only ever exists as bytes extracted to memory, so
                // there's nothing to hand an external command — fall back
                // to the built-in viewer, but only *mention* the fallback
                // when there actually was a `[viewers]` entry that would
                // otherwise have applied (silently doing the same thing
                // as ever for an extension with no entry would just be
                // noise).
                if viewer_cmd.is_some() {
                    self.log_info(format!(
                        "external viewers don't apply inside archives; opening {} in the built-in viewer",
                        virtual_dir::inner_display(&inner_path)
                    ));
                }
                if let Some(archive_path) = archive_path {
                    self.open_viewer_virtual(&archive_path, &inner_path);
                }
            }
            Some(path) if virtual_dir::is_archive_file(&path) => {
                self.navigate(move |pane| pane.enter_virtual(path));
            }
            Some(path) => match viewer_cmd {
                Some(template) => {
                    let cwd = self.active_pane().cwd.clone();
                    let cmdline = external::build_viewer_cmdline(&template, &path);
                    self.outbox.external = Some(ExternalRequest {
                        cmdline,
                        cwd,
                        pause_after: false,
                        interactive: false,
                    });
                }
                None => self.open_viewer(&path),
            },
            None => self.navigate(|pane| pane.enter()),
        }
    }

    fn begin_history_jump(&mut self) {
        let ring = self.history.ring(self.active.into());
        if ring.is_empty() {
            self.log_error("no history for this pane yet");
            return;
        }
        let items = ring
            .iter()
            .map(|p| (p.display().to_string(), p.clone()))
            .collect();
        self.mode = Mode::Select {
            kind: SelectKind::History,
            title: "History".to_string(),
            items,
            cursor: 0,
        };
    }

    fn begin_bookmark_jump(&mut self) {
        if self.bookmarks.paths.is_empty() {
            self.log_error("no bookmarks yet");
            return;
        }
        let items = self
            .bookmarks
            .paths
            .iter()
            .map(|p| (p.display().to_string(), p.clone()))
            .collect();
        self.mode = Mode::Select {
            kind: SelectKind::Bookmark,
            title: "Bookmarks".to_string(),
            items,
            cursor: 0,
        };
    }

    fn begin_bookmark_add(&mut self) {
        let cwd = self.active_pane().cwd.clone();
        if self.bookmarks.add(cwd.clone()) {
            self.outbox.bookmarks_dirty = true;
            self.log_info(format!("bookmarked: {}", cwd.display()));
        } else {
            self.log_info(format!("already bookmarked: {}", cwd.display()));
        }
    }

    fn begin_go_home(&mut self) {
        let home = self
            .config
            .home
            .clone()
            .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()));
        let Some(home) = home else {
            self.log_error("could not determine home directory");
            return;
        };
        if !home.is_dir() {
            self.log_error(format!("not a directory: {}", home.display()));
            return;
        }
        self.jump_active_pane_to(home);
    }

    /// Keys for `Mode::Select`. The menu's own keys come first — Esc
    /// cancels, Enter jumps the active pane to the highlight, and in the
    /// bookmark menu `d` deletes it while Shift+Up/Shift+Down reorder it —
    /// and only a key the menu didn't claim falls through to the keymap's
    /// `cursor_up`/`cursor_down` (`Keymap::menu_nav`). That order is what
    /// keeps a rebind from shadowing a menu key: Shift+Up reorders here
    /// even though it's `top` in Normal mode, and `d` still deletes even
    /// if it's been bound to something else.
    ///
    /// The bare arrows stay wired up unconditionally, alongside whatever
    /// the keymap says, so that unbinding them in `[keys]`/`[bindings]`
    /// can't leave the menu impossible to drive.
    fn handle_select_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Resolved up front, and into a plain `Copy` value: taking this
        // borrow of `self.keymap` inside the `match` below would collide
        // with the `&mut self` the arms need.
        let nav = self.keymap.menu_nav(code, modifiers);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.commit_select(),
            KeyCode::Char('d') if self.select_is_bookmark() => self.delete_selected_bookmark(),
            KeyCode::Up if shift && self.select_is_bookmark() => {
                self.move_selected_bookmark(false);
            }
            KeyCode::Down if shift && self.select_is_bookmark() => {
                self.move_selected_bookmark(true);
            }
            KeyCode::Up => self.select_move_cursor(false),
            KeyCode::Down => self.select_move_cursor(true),
            _ => match nav {
                Some(MenuNav::Up) => self.select_move_cursor(false),
                Some(MenuNav::Down) => self.select_move_cursor(true),
                None => {}
            },
        }
    }

    fn select_is_bookmark(&self) -> bool {
        matches!(
            self.mode,
            Mode::Select {
                kind: SelectKind::Bookmark,
                ..
            }
        )
    }

    /// Moves the `Mode::Select` highlight one row, clamping at both ends
    /// (the menu has no wrap-around, unlike the fixed-length sort/sync
    /// dialogs).
    fn select_move_cursor(&mut self, down: bool) {
        let Mode::Select { cursor, items, .. } = &mut self.mode else {
            return;
        };
        if !down {
            *cursor = cursor.saturating_sub(1);
        } else if *cursor + 1 < items.len() {
            *cursor += 1;
        }
    }

    /// Moves the highlighted bookmark one slot up/down, carrying the
    /// cursor with it, in both the open menu and the persisted list.
    /// Returns whether anything moved: `false` in the history menu, at
    /// either end, and for an empty list.
    ///
    /// Relies on `Mode::Select.items` and `Bookmarks::paths` being
    /// index-for-index the same list — established by
    /// `begin_bookmark_jump`, and preserved by this and by
    /// `delete_selected_bookmark` (which likewise touches both). Nothing
    /// else can reach `self.bookmarks` while the menu is open.
    ///
    /// Deliberately silent: a reorder is cheap and reversible, and
    /// Shift+Down gets held down, so logging every step would bury the log
    /// — unlike `delete_selected_bookmark`, where the entry is gone.
    fn move_selected_bookmark(&mut self, down: bool) -> bool {
        let Mode::Select {
            kind: SelectKind::Bookmark,
            cursor,
            ..
        } = &self.mode
        else {
            return false;
        };
        let from = *cursor;
        // The persisted list is the gate — it owns the bounds check, so a
        // move at either end stops here without dirtying anything.
        let moved = if down {
            self.bookmarks.move_down(from)
        } else {
            self.bookmarks.move_up(from)
        };
        if !moved {
            return false;
        }
        let to = if down { from + 1 } else { from - 1 };
        if let Mode::Select { items, cursor, .. } = &mut self.mode {
            items.swap(from, to);
            *cursor = to;
        }
        self.outbox.bookmarks_dirty = true;
        true
    }

    fn commit_select(&mut self) {
        let target = match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Select { items, cursor, .. } => {
                items.into_iter().nth(cursor).map(|(_, path)| path)
            }
            other => {
                self.mode = other;
                None
            }
        };
        if let Some(path) = target {
            self.jump_active_pane_to(path);
        }
    }

    fn delete_selected_bookmark(&mut self) {
        let removed_path = {
            let Mode::Select {
                kind: SelectKind::Bookmark,
                items,
                cursor,
                ..
            } = &mut self.mode
            else {
                return;
            };
            if items.is_empty() || *cursor >= items.len() {
                return;
            }
            let (_, path) = items.remove(*cursor);
            if *cursor >= items.len() {
                *cursor = items.len().saturating_sub(1);
            }
            path
        };
        self.bookmarks.remove_path(&removed_path);
        self.outbox.bookmarks_dirty = true;
        self.log_info(format!("removed bookmark: {}", removed_path.display()));
    }

    /// Only fires on a file (never a directory): opens `config.editor`
    /// (falling back to `$EDITOR`) suspended, without the "press any key"
    /// pause — editors already take over the whole screen and hand control
    /// back cleanly on their own.
    fn begin_open_editor(&mut self) {
        if self.reject_if_virtual("open_editor") {
            return;
        }
        let pane = self.active_pane();
        // Same navigate-vs-open split as `begin_open`: a directory-symlink
        // is "not a file" here too, just like a real directory.
        let selected = pane.selected_entry();
        let target = match selected.map(|e| e.is_dir_like()) {
            Some(false) => selected.map(|e| e.path.clone()),
            _ => None,
        };
        let Some(path) = target else {
            self.log_error("cursor is not on a file");
            return;
        };

        let editor = self
            .config
            .editor
            .clone()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|s| !s.trim().is_empty());
        let Some(editor) = editor else {
            self.log_error("no editor configured (set editor in config.toml or $EDITOR)");
            return;
        };

        let cmdline = format!(
            "{editor} {}",
            external::shell_quote(&path.to_string_lossy())
        );
        let cwd = self.active_pane().cwd.clone();
        self.outbox.external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: false,
            interactive: false,
        });
    }

    /// `,` (edit_config): opens ozzel's own config file in an editor,
    /// creating it from the bundled template first if it doesn't exist yet
    /// (see `config::ensure_config_file_exists`). Unlike `OpenEditor`, this
    /// falls back to a hardcoded `vim` when neither `config.editor` nor
    /// `$EDITOR` is set, since a user reaching for "edit my config" wants
    /// it to just work rather than error out. Queues `pending_config_reload`
    /// alongside the suspend request so `main.rs`'s loop reloads the config
    /// live once the editor exits (see `reload_config`).
    fn begin_edit_config(&mut self) {
        let Some(path) = config::config_path() else {
            self.log_error("could not determine the config file location on this platform");
            return;
        };
        self.begin_edit_config_at(path);
    }

    /// Core of `begin_edit_config`, taking the path explicitly so tests can
    /// point it at a tempdir file instead of the real XDG-resolved
    /// location (which `cargo test` must never read from or write to).
    fn begin_edit_config_at(&mut self, path: PathBuf) {
        if let Err(err) = config::ensure_config_file_exists(&path) {
            self.log_error(format!("failed to create config file: {err}"));
            return;
        }

        let editor = self
            .config
            .editor
            .clone()
            .or_else(|| std::env::var("EDITOR").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "vim".to_string());

        let cmdline = format!(
            "{editor} {}",
            external::shell_quote(&path.to_string_lossy())
        );
        let cwd = self.active_pane().cwd.clone();
        self.outbox.external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: false,
            interactive: false,
        });
        self.outbox.config_reload = true;
    }

    /// Re-reads and re-parses the config file at the real, XDG-resolved
    /// location `config::load` uses at startup. Called by `main.rs`'s loop
    /// right after the `,` (edit_config) editor exits.
    pub fn reload_config(&mut self) {
        let Some(path) = config::config_path() else {
            self.log_error("could not determine the config file location on this platform");
            return;
        };
        self.reload_config_from(&path);
    }

    /// Core of `reload_config`, taking the path explicitly so tests can
    /// point it at a tempdir file. On a parse error, the *old* config and
    /// keymap are left completely untouched and the error is logged — this
    /// is the one config-error path that must never hard-fail, since the
    /// app is already running (unlike the startup load in `main.rs`, which
    /// is allowed to bail before the terminal is even touched).
    fn reload_config_from(&mut self, path: &Path) {
        match config::load_from_path(path) {
            Ok(new_config) => self.apply_reloaded_config(new_config),
            Err(err) => self.log_error(format!("config reload failed: {err}")),
        }
    }

    /// Rebuilds the keymap from `new_config` and, only if that succeeds,
    /// swaps both `self.config` and `self.keymap` in and logs success.
    /// `[keys]`/`[bindings]` errors surface here exactly like a malformed
    /// startup config would (same `build_keymap` used by `App::new`), but
    /// unlike startup, a bad keymap here must not touch the running app's
    /// state at all — the old config/keymap stay live.
    fn apply_reloaded_config(&mut self, new_config: Config) {
        match build_keymap(&new_config) {
            Ok(new_keymap) => {
                self.config = new_config;
                self.keymap = new_keymap;
                // Panes hold their own copy of `natural_sort` (they never
                // read config) — push the (possibly changed) value onto
                // both, invalidating their sort caches only on an actual
                // change (see `Pane::set_natural_sort`).
                let natural = self.config.natural_sort;
                for pane in &mut self.panes {
                    pane.set_natural_sort(natural);
                }
                self.log_info("config reloaded");
            }
            Err(err) => self.log_error(format!("config reload failed: {err}")),
        }
    }

    fn begin_open_default(&mut self) {
        if self.reject_if_virtual("open_default") {
            return;
        }
        match self.active_pane().selected_entry_path() {
            Some(path) => self.open_with_default(&path),
            None => self.log_error("no entry selected to open"),
        }
    }

    fn open_with_default(&mut self, path: &Path) {
        match open::that_detached(path) {
            Ok(()) => self.log_info(format!("opened {}", path.display())),
            Err(err) => self.log_error(format!("failed to open {}: {err}", path.display())),
        }
    }

    /// `y` (copy_path): copies the cursor entry's absolute path to the
    /// system clipboard. On the `..` row there's no entry to name, so it
    /// copies the directory `..` would navigate to instead — the same
    /// destination `Pane::enter` would take, which for a virtual pane at
    /// the archive root means the real directory holding the archive.
    fn begin_copy_path(&mut self) {
        let pane = self.active_pane();
        let text = if let Some(path) = pane.selected_entry_path() {
            self.clipboard_path_text(&path)
        } else if pane.cursor_is_parent_row() {
            match &pane.virtual_dir {
                // Inside the archive: one level up, still archive-internal.
                Some(vd) if !vd.inner.as_os_str().is_empty() => {
                    let parent = vd.inner.parent().unwrap_or(Path::new(""));
                    format!("{}:{}", vd.archive_name, virtual_dir::inner_display(parent))
                }
                // At the archive root `..` leaves the archive entirely, and
                // `cwd` still points at the real directory it came from.
                Some(_) => pane.cwd.to_string_lossy().into_owned(),
                None => match pane.cwd.parent() {
                    Some(parent) => parent.to_string_lossy().into_owned(),
                    // Unreachable in practice: `visible_entries` omits the
                    // `..` row at the filesystem root.
                    None => pane.cwd.to_string_lossy().into_owned(),
                },
            }
        } else {
            self.log_error("no entry selected to copy the path of");
            return;
        };
        self.queue_clipboard(text);
    }

    /// `Y` (copy_dir_path): copies the active pane's own directory,
    /// whatever the cursor happens to be on.
    fn begin_copy_dir_path(&mut self) {
        let pane = self.active_pane();
        let text = match &pane.virtual_dir {
            Some(vd) => virtual_dir::header_label(vd),
            None => pane.cwd.to_string_lossy().into_owned(),
        };
        self.queue_clipboard(text);
    }

    /// Renders a path for the clipboard the way the pane header shows it.
    /// Non-mutating, so unlike the rename/delete/etc. family the copy
    /// actions aren't rejected in a virtual pane — but a path there is only
    /// archive-internal (`Pane::virtual_dir`'s doc comment), not a real
    /// absolute one, so it's formatted as `archive.zip:/inner/path` rather
    /// than misleadingly presented as a real filesystem path.
    fn clipboard_path_text(&self, path: &Path) -> String {
        match &self.active_pane().virtual_dir {
            Some(vd) => format!("{}:{}", vd.archive_name, virtual_dir::inner_display(path)),
            None => path.to_string_lossy().into_owned(),
        }
    }

    /// Hands `text` to `main.rs`'s loop, which writes it out as an OSC 52
    /// terminal escape (see `external::osc52_copy_sequence`) — that works
    /// over SSH/tmux and needs no extra dependency, unlike a
    /// native-clipboard crate. Never fails loudly: a terminal that doesn't
    /// understand OSC 52 just silently ignores it (no reliable way to
    /// detect support up front), so this always logs success rather than
    /// trying to guess.
    fn queue_clipboard(&mut self, text: String) {
        self.log_info(format!("copied: {text}"));
        self.outbox.clipboard = Some(text);
    }

    /// `F`/`S-f`: opens the command palette with an empty filter (every
    /// action listed — see `crate::function_list::filter_actions`).
    fn begin_function_list(&mut self) {
        self.mode = Mode::FunctionList {
            input: LineEditor::new(),
            cursor: 0,
        };
    }

    /// Every action currently matching `Mode::FunctionList`'s typed query,
    /// in display order — from a cache keyed by the exact typed query
    /// string, rather than recomputed on every call. Derived from
    /// `&self.mode` rather than a `Vec` stored on the mode itself, so the
    /// list can never go stale relative to whatever's actually typed; the
    /// cache just means a plain cursor Up/Down (which re-renders but never
    /// changes the query) doesn't re-filter `Action::ALL` for nothing.
    /// `pub(crate)` so `ui::function_list_view` can call it too, instead of
    /// going straight to `crate::function_list::filter_actions` and missing
    /// the cache.
    pub(crate) fn function_list_filtered_actions(&mut self) -> &[Action] {
        let query = match &self.mode {
            Mode::FunctionList { input, .. } => input.value(),
            _ => return &[],
        };
        let stale = match &self.function_list_cache {
            Some((cached_query, _)) => *cached_query != query,
            None => true,
        };
        if stale {
            let actions = crate::function_list::filter_actions(&query);
            self.function_list_cache = Some((query, actions));
        }
        &self.function_list_cache.as_ref().unwrap().1
    }

    /// Keys for `Mode::FunctionList`. `Esc` cancels; `Enter` closes the
    /// palette (back to Normal) and then dispatches the highlighted action
    /// — in that order, so an action that itself sets a mode (a
    /// Prompt/Confirm/Select) isn't immediately clobbered back to Normal
    /// afterward.
    ///
    /// Everything the search field consumes — printable characters and the
    /// line-editing keys — is claimed before the keymap is consulted, so
    /// typing `i` or `k` still filters rather than moving the highlight.
    /// What's left over does reach `cursor_up`/`cursor_down`, which is how
    /// a modifier-based rebind (`C-p`/`C-n`) can drive the list here.
    fn handle_function_list_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Resolved before the `&mut self.mode` borrow below, and as a plain
        // `Copy` value — see `handle_select_key`.
        let nav = self.keymap.menu_nav(code, modifiers);
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                let cursor = match &self.mode {
                    Mode::FunctionList { cursor, .. } => *cursor,
                    _ => return,
                };
                let action = self.function_list_filtered_actions().get(cursor).copied();
                self.mode = Mode::Normal;
                if let Some(action) = action {
                    self.dispatch(action);
                }
                return;
            }
            KeyCode::Up => {
                if let Mode::FunctionList { cursor, .. } = &mut self.mode {
                    *cursor = cursor.saturating_sub(1);
                }
                return;
            }
            KeyCode::Down => {
                let len = self.function_list_filtered_actions().len();
                if let Mode::FunctionList { cursor, .. } = &mut self.mode
                    && *cursor + 1 < len
                {
                    *cursor += 1;
                }
                return;
            }
            _ => {}
        }

        // Taken before the `&mut self.mode` borrow, for the keymap-driven
        // Down below.
        let len = self.function_list_filtered_actions().len();
        let Mode::FunctionList { input, cursor } = &mut self.mode else {
            return;
        };
        let mut edited = true;
        match code {
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.move_left(),
            KeyCode::Right => input.move_right(),
            KeyCode::Home => input.move_home(),
            KeyCode::End => input.move_end(),
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
            _ => edited = false,
        }
        if edited {
            // The filtered list changes on every edit; keep the highlight
            // in bounds by just resetting to the top rather than trying to
            // track "the same action" across a re-filter.
            *cursor = 0;
            return;
        }
        // Only keys the search field didn't want get this far.
        match nav {
            Some(MenuNav::Up) => *cursor = cursor.saturating_sub(1),
            Some(MenuNav::Down) if *cursor + 1 < len => *cursor += 1,
            _ => {}
        }
    }

    /// `g`: opens the file-name search popup over a snapshot of the active
    /// pane's whole subtree, walked exactly once here (see
    /// `crate::file_search`'s module doc comment for why per-keystroke
    /// searches then never touch the disk). Starts with the empty query's
    /// results — every entry — rather than an empty list, so the popup is
    /// browsable before anything is typed.
    fn begin_file_search(&mut self) {
        if self.reject_if_virtual("file_search") {
            return;
        }
        let pane = self.active_pane();
        let tree = file_search::collect_tree(&pane.cwd, pane.show_hidden, MAX_TREE_ENTRIES);
        let results = file_search::search(&tree, None);
        self.mode = Mode::FileSearch {
            input: LineEditor::new(),
            cursor: 0,
            tree: Rc::new(tree),
            results,
            last_run_query: String::new(),
            error: None,
        };
    }

    /// Re-runs `Mode::FileSearch`'s query against its snapshot, updating
    /// `results`/`last_run_query`/`error` and resetting the highlight to
    /// the top. Skips entirely when the query hasn't changed since the
    /// last run — same "don't recompile the regex for a cursor-only
    /// keystroke" reasoning as `handle_filter_key`'s `unchanged` check.
    fn run_file_search(&mut self) {
        let Mode::FileSearch {
            input,
            cursor,
            tree,
            results,
            last_run_query,
            error,
        } = &mut self.mode
        else {
            return;
        };
        let query = input.value();
        if *last_run_query == query {
            return;
        }
        let spec = FilterSpec::parse(&query);
        *results = file_search::search(tree, spec.as_ref());
        *error = spec.as_ref().and_then(|s| s.error()).map(str::to_string);
        *last_run_query = query;
        *cursor = 0;
    }

    /// Fixed keys for `Mode::FileSearch`; never consults the keymap.
    /// `Esc` cancels; `Enter` either re-runs a stale query (only possible
    /// with `file_search_incremental = false`, where edits don't re-search)
    /// or, when the results already match the input, closes the popup and
    /// jumps the pane to the selected hit's parent directory with the
    /// cursor on it. Edit keys go to the `LineEditor`, then re-search
    /// immediately in incremental mode.
    fn handle_file_search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                let stale = match &self.mode {
                    Mode::FileSearch {
                        input,
                        last_run_query,
                        ..
                    } => input.value() != *last_run_query,
                    _ => return,
                };
                if stale {
                    self.run_file_search();
                    return;
                }
                let Mode::FileSearch {
                    tree,
                    results,
                    cursor,
                    ..
                } = &self.mode
                else {
                    return;
                };
                let Some(&idx) = results.get(*cursor) else {
                    return;
                };
                let entry = &tree.entries[idx];
                // Directories jump the same way files do — to the *parent*,
                // cursor on the hit — so one Enter more (`open`) descends
                // into a matched directory instead of teleporting past it.
                let parent = entry
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| tree.root.clone());
                let name = entry.name.clone();
                self.mode = Mode::Normal;
                // `navigate` (inside) already logs a failed jump and leaves
                // the pane where it was; `restore_cursor_onto` then simply
                // finds no such name and parks at 0 — safe either way.
                self.jump_active_pane_to(parent);
                self.active_pane_mut().restore_cursor_onto(&name);
                return;
            }
            KeyCode::Up => {
                if let Mode::FileSearch { cursor, .. } = &mut self.mode {
                    *cursor = cursor.saturating_sub(1);
                }
                return;
            }
            KeyCode::Down => {
                if let Mode::FileSearch {
                    cursor, results, ..
                } = &mut self.mode
                    && *cursor + 1 < results.len()
                {
                    *cursor += 1;
                }
                return;
            }
            _ => {}
        }

        let incremental = self.config.file_search_incremental;
        {
            let Mode::FileSearch { input, .. } = &mut self.mode else {
                return;
            };
            match code {
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.move_home(),
                KeyCode::End => input.move_end(),
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
                _ => {}
            }
        }
        // In Enter-run mode the previous results stay on screen (the view
        // flags them stale via `last_run_query`) until the user asks.
        if incremental {
            self.run_file_search();
        }
    }

    /// Fixed editing keys for `Mode::Filter`; never consults the keymap.
    /// Every edit live-applies to the active pane's filter (Esc clears it
    /// and cancels; Enter just leaves it in place and returns to Normal).
    fn handle_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.active_pane_mut().set_filter(None);
                return;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                return;
            }
            _ => {}
        }

        let value = {
            let Mode::Filter { input } = &mut self.mode else {
                return;
            };
            match code {
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.move_home(),
                KeyCode::End => input.move_end(),
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
                _ => {}
            }
            input.value()
        };
        // `FilterSpec::parse` compiles a `re:`-prefixed pattern as a regex
        // — real cost on every keystroke, including ones (arrow keys, Home/
        // End) that move the cursor without changing `value` at all. Since
        // `parse` is a pure function of the raw string, and the active
        // pane's current filter already remembers its own `raw`, comparing
        // against that first skips the recompile whenever the text itself
        // didn't actually change.
        let unchanged = match &self.active_pane().filter {
            Some(filter) => filter.raw == value,
            None => value.is_empty(),
        };
        if !unchanged {
            self.active_pane_mut().set_filter(FilterSpec::parse(&value));
        }
    }

    /// `\`: opens the prefix-jump search line, remembering the cursor
    /// position it started at (`Esc` restores exactly this; `Enter` does
    /// not).
    fn begin_jump_search(&mut self) {
        let original_cursor = self.active_pane().cursor;
        self.mode = Mode::JumpSearch {
            input: LineEditor::new(),
            original_cursor,
        };
    }

    /// Fixed editing keys for `Mode::JumpSearch`; never consults the
    /// keymap. Unlike `Mode::Filter`, this never touches `Pane::filter` —
    /// it's pure cursor movement, so `Esc`'s "undo" is restoring the
    /// cursor position instead of clearing a filter.
    fn handle_jump_search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                let Mode::JumpSearch {
                    original_cursor, ..
                } = &self.mode
                else {
                    return;
                };
                let original_cursor = *original_cursor;
                self.active_pane_mut().cursor = original_cursor;
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.jump_search_cycle(1);
                return;
            }
            KeyCode::Up => {
                self.jump_search_cycle(-1);
                return;
            }
            _ => {}
        }

        let value = {
            let Mode::JumpSearch { input, .. } = &mut self.mode else {
                return;
            };
            match code {
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.move_home(),
                KeyCode::End => input.move_end(),
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
                _ => {}
            }
            input.value()
        };
        self.jump_search_to_first_match(&value);
    }

    /// Every keystroke re-searches from the top of the list with the
    /// (now longer/shorter) typed prefix — deliberately *not* incremental
    /// from wherever the cursor currently sits, so backspacing back to a
    /// shorter prefix returns to the same first match typing that prefix
    /// fresh would. An empty `value` (nothing typed yet) leaves the
    /// cursor untouched; a `value` that matches nothing also leaves it
    /// untouched (the UI shows the `(no match)` hint instead — see
    /// `ui::draw`/`modal::render_jump_search_line`).
    fn jump_search_to_first_match(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        if let Some(&first) = self.active_pane().jump_matches(value).first() {
            self.active_pane_mut().cursor = first;
        }
    }

    /// `Down`/`Tab` (`step = 1`) or `Up` (`step = -1`) while
    /// `Mode::JumpSearch` is open: moves to the next/previous entry
    /// matching the *current* typed prefix, wrapping around. A no-op when
    /// nothing's typed yet or nothing matches. If the cursor isn't
    /// currently sitting on one of the matches (shouldn't normally happen,
    /// since typing always jumps onto a match when there is one, but is
    /// possible right after opening the search before anything's been
    /// typed), `step = 1` starts at the first match and `step = -1` at
    /// the last, rather than doing nothing.
    fn jump_search_cycle(&mut self, step: isize) {
        let Mode::JumpSearch { input, .. } = &self.mode else {
            return;
        };
        let value = input.value();
        if value.is_empty() {
            return;
        }
        let pane = self.active_pane();
        let matches = pane.jump_matches(&value);
        if matches.is_empty() {
            return;
        }
        let len = matches.len() as isize;
        let next = match matches.iter().position(|&i| i == pane.cursor) {
            Some(pos) => {
                let pos = pos as isize;
                ((pos + step).rem_euclid(len)) as usize
            }
            None if step > 0 => 0,
            None => matches.len() - 1,
        };
        self.active_pane_mut().cursor = matches[next];
    }

    /// Fixed editing keys for `Mode::Prompt`; never consults the keymap.
    fn handle_prompt_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                // A rename_marks sequence cancels *as a whole*: the
                // renames already committed stand (each was applied the
                // moment its prompt was confirmed), the rest are dropped,
                // and the log says how far it got.
                match &self.mode {
                    Mode::Prompt {
                        kind: PromptKind::RenameMany { done, total, .. },
                        ..
                    } => {
                        let (done, total) = (*done, *total);
                        self.log_info(format!("rename marks cancelled ({done}/{total} renamed)"));
                    }
                    // Esc in the collision-rename prompt cancels the
                    // whole transfer, exactly like Esc in the dialog it
                    // came from — not just this one entry.
                    Mode::Prompt {
                        kind: PromptKind::CollisionRename { .. },
                        ..
                    } => self.log_info("transfer cancelled"),
                    _ => {}
                }
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                if let Mode::Prompt { kind, input } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                {
                    self.commit_prompt(kind, input.value());
                }
                return;
            }
            _ => {}
        }

        let Mode::Prompt { input, .. } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.move_left(),
            KeyCode::Right => input.move_right(),
            KeyCode::Home => input.move_home(),
            KeyCode::End => input.move_end(),
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
            _ => {}
        }
    }

    fn commit_prompt(&mut self, kind: PromptKind, value: String) {
        match kind {
            PromptKind::Mkdir => {
                let cwd = self.active_pane().cwd.clone();
                match ops::mkdir(&cwd, &value) {
                    Ok(()) => self.log_info(format!("created directory: {value}")),
                    Err(err) => self.log_error(err.to_string()),
                }
                self.reload_both();
            }
            PromptKind::Rename { orig } => {
                let cwd = self.active_pane().cwd.clone();
                match ops::rename(&cwd, &orig, &value) {
                    Ok(()) => self.log_info(format!("renamed {orig} -> {value}")),
                    Err(err) => self.log_error(err.to_string()),
                }
                self.reload_both();
            }
            PromptKind::ZipName { targets } => self.commit_zip_name(targets, value),
            PromptKind::Command => self.commit_command(value),
            PromptKind::Duplicate { source } => self.commit_duplicate(source, value),
            PromptKind::RenameMany {
                dir,
                current,
                queue,
                done,
                total,
            } => self.commit_rename_many(dir, current, queue, done, total, value),
            PromptKind::CollisionRename { state } => self.commit_collision_rename(*state, value),
            PromptKind::ArchivePassword { pending } => self.commit_archive_password(pending, value),
            PromptKind::TouchTime { targets } => self.commit_touch(targets, value),
        }
    }

    /// A typed archive password: verified on the main thread (one-byte
    /// decrypt of the first encrypted entry — see
    /// `virtual_dir::verify_zip_password`), then the pending operation
    /// proceeds with it. A wrong password logs and re-opens the same
    /// prompt; for the Virtual-Directory-scoped operations (View/Extract)
    /// a verified password is cached on the `VirtualDir` so the rest of
    /// the session doesn't re-ask.
    fn commit_archive_password(&mut self, pending: PasswordPending, password: String) {
        let archive_path = match &pending {
            PasswordPending::View { archive_path, .. }
            | PasswordPending::Extract { archive_path, .. }
            | PasswordPending::Unzip { archive_path, .. } => archive_path.clone(),
        };
        if let Err(err) = virtual_dir::verify_zip_password(&archive_path, &password) {
            let is_wrong = err
                .downcast_ref::<virtual_dir::ZipPasswordError>()
                .is_some();
            self.log_error(err.to_string());
            if is_wrong {
                // Re-open the prompt for another try; any other failure
                // (unreadable archive, say) stays cancelled.
                self.mode = Mode::Prompt {
                    kind: PromptKind::ArchivePassword { pending },
                    input: LineEditor::new(),
                };
            }
            return;
        }

        match pending {
            PasswordPending::View {
                archive_path,
                inner_path,
            } => {
                if let Some(vd) = &self.active_pane().virtual_dir {
                    vd.cache_password(password.clone());
                }
                match virtual_dir::extract_single_to_memory(
                    &archive_path,
                    &inner_path,
                    viewer::SIZE_CAP,
                    Some(&password),
                ) {
                    Ok((bytes, truncated)) => {
                        self.show_virtual_bytes(&archive_path, &inner_path, bytes, truncated);
                    }
                    Err(err) => self.log_error(format!("{}: {err}", inner_path.display())),
                }
            }
            PasswordPending::Extract {
                archive_path,
                inner_targets,
                dest_dir,
            } => {
                if let Some(vd) = &self.active_pane().virtual_dir {
                    vd.cache_password(password.clone());
                }
                self.continue_extract(archive_path, inner_targets, dest_dir, Some(password));
            }
            PasswordPending::Unzip {
                archive_path,
                dest_dir,
            } => {
                // No VirtualDir session here (`u` runs from a real pane) —
                // nothing to cache the password on; it just rides the op.
                self.continue_unzip(archive_path, dest_dir, Some(password));
            }
        }
    }

    /// The collision dialog's "Rename" answer, committed: validates the
    /// typed name and resolves the current conflict to `dest_dir/<name>`.
    /// A name that itself collides (or is empty/contains a path
    /// separator) re-opens the same conflict's dialog with the problem
    /// logged — never silently overwriting through the rename path.
    fn commit_collision_rename(&mut self, mut state: CollisionState, value: String) {
        let invalid =
            value.is_empty() || value.contains(std::path::MAIN_SEPARATOR) || value.contains('/');
        if invalid {
            self.log_error(format!("invalid name: {value:?}"));
            self.mode = Mode::TransferCollision { state };
            return;
        }
        let dest = state.dest_dir.join(&value);
        if dest.exists() {
            self.log_error(format!("{value}: already exists in the destination too"));
            self.mode = Mode::TransferCollision { state };
            return;
        }
        state.resolved.push((state.current.src.clone(), dest));
        self.advance_collision(state);
    }

    /// One confirmed step of `rename_marks`: applies (or skips) this
    /// entry's rename, then either re-enters the prompt with the next
    /// queued name or logs the final summary. A failed rename is logged
    /// and the sequence *continues* — one locked file shouldn't abort the
    /// remaining nine renames the user already planned.
    fn commit_rename_many(
        &mut self,
        dir: PathBuf,
        current: String,
        mut queue: std::collections::VecDeque<String>,
        done: usize,
        total: usize,
        value: String,
    ) {
        let mut done = done;
        if value.is_empty() || value == current {
            self.log_info(format!("skipped: {current}"));
        } else {
            match ops::rename(&dir, &current, &value) {
                Ok(()) => {
                    self.log_info(format!("renamed {current} -> {value}"));
                    done += 1;
                }
                Err(err) => self.log_error(err.to_string()),
            }
        }

        match queue.pop_front() {
            Some(next) => {
                self.mode = Mode::Prompt {
                    input: LineEditor::from_str(&next),
                    kind: PromptKind::RenameMany {
                        dir,
                        current: next,
                        queue,
                        done,
                        total,
                    },
                };
                // Reload behind the prompt so the listing tracks each
                // rename as it happens rather than all at the end.
                self.reload_both();
            }
            None => {
                self.log_info(format!("rename marks finished ({done}/{total} renamed)"));
                self.reload_both();
            }
        }
    }

    /// Empty input cancels silently (matches Esc). A non-empty command is
    /// queued as `pending_external`; `main.rs`'s loop is what actually
    /// suspends the TUI and runs it, since that needs `&mut Terminal`,
    /// which `App` doesn't have.
    fn commit_command(&mut self, cmdline: String) {
        if cmdline.trim().is_empty() {
            return;
        }
        let cwd = self.active_pane().cwd.clone();
        self.outbox.external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: true,
            interactive: self.config.command_line_interactive,
        });
    }
}

mod file_ops;
mod mouse;
mod pager;
mod settings_ui;

use mouse::DragState;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;
