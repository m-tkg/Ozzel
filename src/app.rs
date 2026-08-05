//! Top-level application state: the two panes, which one is active, the
//! current input mode, running background tasks, and the `Action` dispatch
//! hub every Normal-mode key eventually funnels through.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use chrono::{DateTime, Local};
use directories::BaseDirs;

use crate::action::Action;
use crate::config::{self, Config};
use crate::entry::EntryKind;
use crate::event::{
    AppEvent, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind, TaskEvent,
};
use crate::external::{self, ExternalRequest};
use crate::filter::FilterSpec;
use crate::keymap::Keymap;
use crate::mode::{LineEditor, Mode, PendingOp, PromptKind, SelectKind, TransferKind, ViewMode};
use crate::ops;
use crate::pane::{PAGE_SIZE, Pane};
use crate::persist::{Bookmarks, History, Side};
use crate::tasks::delete as delete_task;
use crate::tasks::{TaskManager, archive, copy_move};
use crate::viewer;

/// Log lines are capped so a long session's log can't grow without bound.
const LOG_CAPACITY: usize = 500;
/// How many lines `Mode::Viewer`'s PageUp/PageDown jumps.
const VIEWER_PAGE_SIZE: usize = 20;
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
    /// `ui::log_view::format_timestamp_prefix`).
    pub timestamp: DateTime<Local>,
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
    /// Set whenever `bookmarks` is mutated; `main.rs`'s loop checks this
    /// once per iteration and saves (clearing the flag) when set, per the
    /// plan's "save ... after bookmark changes".
    pub bookmarks_dirty: bool,
    /// Set by `:` (arbitrary command) and `e` (editor); `main.rs`'s loop
    /// takes this after each event and, if present, suspends the TUI to
    /// run it via `external::run_suspended`.
    pub pending_external: Option<ExternalRequest>,
    /// Set alongside `pending_external` by `,` (edit_config) specifically:
    /// `main.rs`'s loop checks this right after the queued editor exits
    /// and, if set, calls `reload_config` — a plain bool rather than
    /// folding it into `ExternalRequest` itself, since every other
    /// external command has nothing to do afterward.
    pub pending_config_reload: bool,
    /// Set by `y` (copy_path); `main.rs`'s loop takes this after each event
    /// and, if present, writes the OSC 52 "set clipboard" escape directly
    /// to stdout (no need to suspend the TUI for this one, unlike
    /// `pending_external` — it's a single silent write, not a child
    /// process taking over the screen).
    pub pending_clipboard: Option<String>,
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
}

/// One pane's on-screen geometry as of the last frame — just enough for
/// mouse hit-testing to map a click's `(x, y)` back to "row N of this
/// pane's visible entries" (see `hit_test_row`).
#[derive(Debug, Clone, Copy)]
pub struct PaneLayout {
    /// The pane's full drawn area, borders included.
    pub area: ratatui::layout::Rect,
    /// The entry-list rows' area specifically (inside the border and any
    /// header rows) — what `hit_test_row` actually maps `y` against.
    pub rows_area: ratatui::layout::Rect,
    /// Index of the first visible entry (`Pane::visible_entries()[start]`
    /// is whatever's drawn at `rows_area`'s first row) — mirrors
    /// `ui::pane_view`'s own `scroll_offset` so hit-testing agrees with
    /// what's actually on screen.
    pub start: usize,
}

/// Which pane a left-button drag is constrained to, and the drag's own
/// running state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragState {
    pub pane: ActivePane,
    /// The entry index (into `visible_entries()`) the drag started on —
    /// marking is applied to every row between this and the current one,
    /// inclusive, each time the pointer moves to a new row.
    pub origin_index: usize,
}

/// Maps a click/drag/wheel screen coordinate to an entry index (into
/// `Pane::visible_entries()`), given the pane's last-drawn `rows_area` and
/// scroll `start` offset — pure and free-standing so it's directly
/// unit-testable without any `App`/`Pane` machinery. Returns `None` when
/// `(x, y)` falls outside `rows_area` entirely (out of range, or over the
/// header/border instead of a row). Does *not* clamp against how many
/// entries actually exist past `start` — callers that need that (a short
/// listing scrolled so its last row doesn't fill the viewport) additionally
/// bound the result against `visible_entries().len()`.
fn hit_test_row(layout: &PaneLayout, x: u16, y: u16) -> Option<usize> {
    let area = layout.rows_area;
    if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
        return None;
    }
    Some(layout.start + (y - area.y) as usize)
}

/// Appends one `LogLine` (capacity-capped, timestamped `Local::now()`) to
/// `log` directly. A free function rather than an `&mut self` method so it
/// can be called from inside a loop that already holds a `&mut self.panes`
/// borrow (see `App::reload_both`) — `App::log_push` is a thin wrapper
/// around this for the common case where no such conflict exists.
fn push_log_line(log: &mut VecDeque<LogLine>, message: String, is_error: bool) {
    if log.len() >= LOG_CAPACITY {
        log.pop_front();
    }
    log.push_back(LogLine {
        message,
        is_error,
        timestamp: Local::now(),
    });
}

/// Builds a `Keymap` from `config`: the compiled-in defaults, with `[keys]`
/// merged in and then `[bindings]` applied on top (so `[bindings]` wins on
/// any combo both sections mention — see `Keymap::apply_bindings`). Shared
/// by `App::new` (startup) and `App::apply_reloaded_config` (`,`'s live
/// reload), so the two can never build a keymap two different ways.
fn build_keymap(config: &Config) -> anyhow::Result<Keymap> {
    let mut keymap = Keymap::default_dyna();
    keymap
        .merge_overrides(&config.keys)
        .context("invalid [keys] entry in config")?;
    keymap
        .apply_bindings(&config.bindings)
        .context("invalid [bindings] entry in config")?;
    Ok(keymap)
}

impl App {
    pub fn new(left: PathBuf, right: PathBuf, config: Config) -> anyhow::Result<Self> {
        let keymap = build_keymap(&config)?;
        let (tx, task_rx) = mpsc::channel();

        Ok(Self {
            panes: [Pane::new(left)?, Pane::new(right)?],
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
            bookmarks_dirty: false,
            pending_external: None,
            pending_config_reload: false,
            pending_clipboard: None,
            pane_layout: [None, None],
            drag: None,
            last_click: None,
        })
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active.index()]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active.index()]
    }

    fn log_push(&mut self, message: String, is_error: bool) {
        push_log_line(&mut self.log, message, is_error);
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
        for pane in &mut self.panes {
            if let Err(err) = pane.reload_preserving_cursor() {
                // Can't call `self.log_error` here: `pane` already holds a
                // `&mut self.panes` borrow, and a `&mut self` method call
                // would conflict with it even though `log` is a disjoint
                // field — so this goes through the free function instead,
                // borrowing only `self.log` directly.
                push_log_line(&mut self.log, err.to_string(), true);
            }
        }
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
        let finished = matches!(event, TaskEvent::Finished { .. });

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
            self.reload_both();
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
        match event {
            AppEvent::Input(code, modifiers) => match &self.mode {
                Mode::Normal => {
                    if let Some(action) = self.keymap.resolve(code, modifiers) {
                        self.dispatch(action);
                    }
                }
                Mode::Filter { .. } => self.handle_filter_key(code, modifiers),
                Mode::Select { .. } => self.handle_select_key(code),
                Mode::Prompt { .. } => self.handle_prompt_key(code, modifiers),
                Mode::Confirm { .. } => self.handle_confirm_key(code),
                Mode::Viewer { .. } => self.handle_viewer_key(code),
                Mode::Help { .. } => self.handle_help_key(code),
                Mode::Log { .. } => self.handle_log_view_key(code),
                Mode::FunctionList { .. } => self.handle_function_list_key(code, modifiers),
            },
            AppEvent::Mouse(mouse_event) => self.handle_mouse(mouse_event),
            AppEvent::Task(task_event) => self.handle_task_event(task_event),
            AppEvent::Tick => {}
        }
    }

    /// Entry point for every mouse event (only ever produced when
    /// `config.mouse` enabled capture — see `event::read_event`). Modal
    /// modes other than Viewer/Log/Help ignore the mouse entirely (per the
    /// plan: "other modals ignore mouse, Esc still by key"); Normal mode
    /// gets the full click/drag/wheel behavior in `handle_mouse_normal`.
    fn handle_mouse(&mut self, ev: MouseEvent) {
        match &self.mode {
            Mode::Viewer { .. } => self.handle_modal_wheel(ev, Self::handle_viewer_key),
            Mode::Log { .. } => self.handle_modal_wheel(ev, Self::handle_log_view_key),
            Mode::Help { .. } => self.handle_modal_wheel(ev, Self::handle_help_key),
            Mode::Normal => self.handle_mouse_normal(ev),
            _ => {}
        }
    }

    /// Shared wheel-scroll plumbing for the three full-frame modal views:
    /// each tick is just `MOUSE_WHEEL_STEP` repeats of the same `Up`/`Down`
    /// key their own key handler already understands, so the scroll math
    /// stays defined in exactly one place per mode.
    fn handle_modal_wheel(&mut self, ev: MouseEvent, key_handler: fn(&mut Self, KeyCode)) {
        let code = match ev.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            _ => return,
        };
        for _ in 0..MOUSE_WHEEL_STEP {
            key_handler(self, code);
        }
    }

    /// Which pane (if any) `(x, y)` falls inside, by its last-drawn `area`
    /// (see `App::pane_layout`, refreshed every frame by `ui::draw`).
    fn pane_at(&self, x: u16, y: u16) -> Option<ActivePane> {
        for (i, layout) in self.pane_layout.iter().enumerate() {
            let layout = layout.as_ref()?;
            if x >= layout.area.x
                && x < layout.area.x + layout.area.width
                && y >= layout.area.y
                && y < layout.area.y + layout.area.height
            {
                return Some(if i == 0 {
                    ActivePane::Left
                } else {
                    ActivePane::Right
                });
            }
        }
        None
    }

    fn pane_layout_for(&self, pane: ActivePane) -> Option<&PaneLayout> {
        self.pane_layout[pane.index()].as_ref()
    }

    /// Normal-mode mouse behavior: left click focuses (and, on an entry
    /// row, moves the cursor there), left drag range-marks within the pane
    /// the drag started in, double-click opens, and wheel scrolls the
    /// cursor of whichever pane it's over without changing focus.
    fn handle_mouse_normal(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_mouse_left_down(ev),
            MouseEventKind::Drag(MouseButton::Left) => self.handle_mouse_left_drag(ev),
            MouseEventKind::Up(MouseButton::Left) => self.drag = None,
            MouseEventKind::ScrollUp => self.handle_mouse_wheel(ev, -(MOUSE_WHEEL_STEP as isize)),
            MouseEventKind::ScrollDown => self.handle_mouse_wheel(ev, MOUSE_WHEEL_STEP as isize),
            _ => {}
        }
    }

    /// Left-button down: focuses whichever pane was clicked; on an entry
    /// row, also moves that pane's cursor there, starts a potential
    /// range-mark drag, and checks for a double-click (same row, same pane,
    /// within `DOUBLE_CLICK_WINDOW`) — which opens the entry instead of
    /// just moving the cursor a second time.
    fn handle_mouse_left_down(&mut self, ev: MouseEvent) {
        let Some(pane) = self.pane_at(ev.column, ev.row) else {
            return;
        };
        self.active = pane;
        let Some(layout) = self.pane_layout_for(pane) else {
            return;
        };
        let Some(row) = hit_test_row(layout, ev.column, ev.row) else {
            // Clicked the pane's header/border/blank area: focus only.
            self.last_click = None;
            self.drag = None;
            return;
        };
        let len = self.panes[pane.index()].visible_entries().len();
        if row >= len {
            self.last_click = None;
            self.drag = None;
            return;
        }

        let now = Instant::now();
        let is_double_click = matches!(
            self.last_click,
            Some((last_pane, last_row, at))
                if last_pane == pane && last_row == row && now.duration_since(at) < DOUBLE_CLICK_WINDOW
        );
        self.panes[pane.index()].cursor = row;
        if is_double_click {
            self.last_click = None;
            self.drag = None;
            self.begin_open();
            return;
        }
        self.last_click = Some((pane, row, now));
        // Arms a *potential* drag, but doesn't mark anything yet: a plain
        // click (mouse-down immediately followed by mouse-up, no `Drag`
        // events in between) must only move the cursor, never mark — only
        // `handle_mouse_left_drag`, once an actual `Drag` event proves the
        // pointer moved while the button was held, marks the origin row
        // (as part of its lo..=hi sweep, which includes `origin_index`
        // even when the drag hasn't left that row yet).
        self.drag = Some(DragState {
            pane,
            origin_index: row,
        });
    }

    /// Left-button drag: extends the range-mark from the drag's origin row
    /// to whatever row the pointer is over now, but *only* while the
    /// pointer stays within the pane the drag started in — crossing into
    /// the other pane, or off the entry rows entirely, is simply ignored
    /// (the mark stops growing, but nothing already marked is undone, and
    /// focus never changes mid-drag).
    fn handle_mouse_left_drag(&mut self, ev: MouseEvent) {
        let Some(drag) = self.drag else { return };
        let Some(layout) = self.pane_layout_for(drag.pane) else {
            return;
        };
        let Some(row) = hit_test_row(layout, ev.column, ev.row) else {
            return;
        };
        let len = self.panes[drag.pane.index()].visible_entries().len();
        if row >= len {
            return;
        }
        let (lo, hi) = if row <= drag.origin_index {
            (row, drag.origin_index)
        } else {
            (drag.origin_index, row)
        };
        for i in lo..=hi {
            self.panes[drag.pane.index()].mark_index(i, true);
        }
    }

    /// Mouse wheel over a pane in Normal mode: moves that pane's cursor by
    /// `delta` rows (negative = up) without changing which pane is active —
    /// tried focus-follow while implementing this and found it more
    /// surprising than leaving focus alone, since a stray wheel tick while
    /// reading the other pane would otherwise silently redirect keystrokes.
    fn handle_mouse_wheel(&mut self, ev: MouseEvent, delta: isize) {
        let Some(pane) = self.pane_at(ev.column, ev.row) else {
            return;
        };
        self.panes[pane.index()].move_cursor(delta);
    }

    /// The single match hub every action flows through. Kept infallible on
    /// the outside (errors are logged instead of propagated) so the input
    /// loop never has to think about failure.
    pub fn dispatch(&mut self, action: Action) {
        let result: anyhow::Result<()> = match action {
            Action::CursorUp => {
                self.active_pane_mut().move_cursor(-1);
                Ok(())
            }
            Action::CursorDown => {
                self.active_pane_mut().move_cursor(1);
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
            Action::Mkdir => {
                self.mode = Mode::Prompt {
                    kind: PromptKind::Mkdir,
                    input: LineEditor::new(),
                };
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
            Action::Duplicate => {
                self.begin_duplicate();
                Ok(())
            }
            Action::FunctionList => {
                self.begin_function_list();
                Ok(())
            }
            Action::Quit => {
                self.begin_quit();
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
            self.mode = Mode::Confirm {
                message,
                on_yes: PendingOp::Quit,
            };
            return;
        }

        if self.config.confirm_quit {
            self.mode = Mode::Confirm {
                message: "Quit ozzel? (y/n)".to_string(),
                on_yes: PendingOp::Quit,
            };
            return;
        }

        self.should_quit = true;
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
            Ok(()) => self.active_pane_mut().forward.push(current),
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
            Ok(()) => self.active_pane_mut().back.push(current),
            Err(err) => {
                self.active_pane_mut().forward.push(target);
                self.log_error(err.to_string());
            }
        }
    }

    fn jump_active_pane_to(&mut self, path: PathBuf) {
        self.navigate(|pane| pane.jump_to(path));
    }

    /// `Open`'s dyna-filer behavior (bound to `Enter`/`o` by default, and
    /// the single action `Enter`/`View` used to be split across before
    /// they were merged): `..`/directories navigate (and get recorded in
    /// history via `navigate`); anything else opens in the built-in
    /// viewer.
    fn begin_open(&mut self) {
        let pane = self.active_pane();
        // `..`/directories navigate (via `Pane::enter`, which already
        // handles both — and is a safe no-op on an empty pane); anything
        // else with a real kind (file, symlink) opens instead.
        let open_path = match pane.selected_entry_kind() {
            Some(kind) if kind != EntryKind::Dir => pane.selected_entry_path(),
            _ => None,
        };
        match open_path {
            Some(path) => self.open_viewer(&path),
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
            self.bookmarks_dirty = true;
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

    /// Fixed navigation keys for `Mode::Select`; never consults the
    /// keymap. Up/Down move the highlight, Enter jumps the active pane
    /// there, Esc cancels, `d` deletes the highlighted bookmark (a no-op
    /// outside the bookmark menu).
    fn handle_select_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up => {
                if let Mode::Select { cursor, .. } = &mut self.mode {
                    *cursor = cursor.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Mode::Select { cursor, items, .. } = &mut self.mode
                    && *cursor + 1 < items.len()
                {
                    *cursor += 1;
                }
            }
            KeyCode::Enter => self.commit_select(),
            KeyCode::Char('d') => self.delete_selected_bookmark(),
            _ => {}
        }
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
        self.bookmarks_dirty = true;
        self.log_info(format!("removed bookmark: {}", removed_path.display()));
    }

    /// Only fires on a file (never a directory): opens `config.editor`
    /// (falling back to `$EDITOR`) suspended, without the "press any key"
    /// pause — editors already take over the whole screen and hand control
    /// back cleanly on their own.
    fn begin_open_editor(&mut self) {
        let pane = self.active_pane();
        let target = match pane.selected_entry_kind() {
            Some(kind) if kind != EntryKind::Dir => pane.selected_entry_path(),
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
        self.pending_external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: false,
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
        self.pending_external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: false,
        });
        self.pending_config_reload = true;
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
                self.log_info("config reloaded");
            }
            Err(err) => self.log_error(format!("config reload failed: {err}")),
        }
    }

    fn begin_open_default(&mut self) {
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

    /// Loads `path` and switches to `Mode::Viewer`. Binary files no longer
    /// get rejected — they simply open in hex mode (see
    /// `viewer::LoadedFile::initial_mode`); only a genuine I/O error is
    /// logged and leaves the mode unchanged.
    fn open_viewer(&mut self, path: &Path) {
        match viewer::load(path) {
            Ok(loaded) => {
                self.mode = Mode::Viewer {
                    path: path.to_path_buf(),
                    lines: loaded.lines,
                    bytes: loaded.bytes,
                    view_mode: loaded.initial_mode,
                    scroll: 0,
                    h_scroll: 0,
                    truncated: loaded.truncated,
                };
            }
            Err(err) => self.log_error(format!("{}: {err}", path.display())),
        }
    }

    /// Fixed keys for `Mode::Viewer`; never consults the keymap. `q`/Esc
    /// is handled before the field-destructure below so assigning
    /// `self.mode = Mode::Normal` there never conflicts with the still-live
    /// borrow the other arms need (same pattern as `handle_prompt_key`).
    fn handle_viewer_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
            self.mode = Mode::Normal;
            return;
        }

        let Mode::Viewer {
            lines,
            bytes,
            view_mode,
            scroll,
            h_scroll,
            ..
        } = &mut self.mode
        else {
            return;
        };

        if code == KeyCode::Tab {
            *view_mode = view_mode.toggle();
            *scroll = 0;
            *h_scroll = 0;
            return;
        }

        let max_scroll = match view_mode {
            ViewMode::Text => lines.len().saturating_sub(1),
            ViewMode::Hex => bytes
                .len()
                .div_ceil(viewer::HEX_BYTES_PER_LINE)
                .saturating_sub(1),
        };
        match code {
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Down => *scroll = (*scroll + 1).min(max_scroll),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(VIEWER_PAGE_SIZE),
            KeyCode::PageDown => *scroll = (*scroll + VIEWER_PAGE_SIZE).min(max_scroll),
            KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
            KeyCode::End | KeyCode::Char('G') => *scroll = max_scroll,
            KeyCode::Left => *h_scroll = h_scroll.saturating_sub(VIEWER_H_SCROLL_STEP),
            KeyCode::Right => *h_scroll += VIEWER_H_SCROLL_STEP,
            _ => {}
        }
    }

    fn begin_help(&mut self) {
        self.mode = Mode::Help { scroll: 0 };
    }

    /// Fixed keys for `Mode::Help`; never consults the keymap (it would be
    /// circular — this screen exists to document the keymap). `q`/Esc/`h`
    /// close back to Normal; everything else scrolls the same way the
    /// viewer's text mode does. The listing is rebuilt from `self.keymap`
    /// on every keypress (cheap — a few dozen actions) rather than cached
    /// on `Mode::Help`, so it can never go stale relative to the keymap.
    fn handle_help_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('h')) {
            self.mode = Mode::Normal;
            return;
        }

        let max_scroll = crate::help::build_lines(&self.keymap)
            .len()
            .saturating_sub(1);
        let Mode::Help { scroll } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Down => *scroll = (*scroll + 1).min(max_scroll),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(VIEWER_PAGE_SIZE),
            KeyCode::PageDown => *scroll = (*scroll + VIEWER_PAGE_SIZE).min(max_scroll),
            KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
            KeyCode::End | KeyCode::Char('G') => *scroll = max_scroll,
            _ => {}
        }
    }

    /// `S-l`/`L`: opens the full-frame in-memory log viewer, scrolled to
    /// the bottom (the newest content — `scroll_from_bottom: 0`).
    fn begin_show_log(&mut self) {
        self.mode = Mode::Log {
            scroll_from_bottom: 0,
        };
    }

    /// Fixed keys for `Mode::Log`; same scroll keys as the viewer's text
    /// mode. `scroll_from_bottom` only grows/shrinks here — it's rendering
    /// (`ui::log_view::render_full`, which knows the terminal width and
    /// therefore the real wrapped row count) that clamps it to a
    /// meaningful range, so `Home` can just jump to `usize::MAX` and let
    /// the render side saturate it down to "the very top".
    fn handle_log_view_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Char('q') | KeyCode::Esc) {
            self.mode = Mode::Normal;
            return;
        }
        let Mode::Log { scroll_from_bottom } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up => *scroll_from_bottom = scroll_from_bottom.saturating_add(1),
            KeyCode::Down => *scroll_from_bottom = scroll_from_bottom.saturating_sub(1),
            KeyCode::PageUp => {
                *scroll_from_bottom = scroll_from_bottom.saturating_add(VIEWER_PAGE_SIZE)
            }
            KeyCode::PageDown => {
                *scroll_from_bottom = scroll_from_bottom.saturating_sub(VIEWER_PAGE_SIZE)
            }
            KeyCode::Home | KeyCode::Char('g') => *scroll_from_bottom = usize::MAX,
            KeyCode::End | KeyCode::Char('G') => *scroll_from_bottom = 0,
            _ => {}
        }
    }

    /// `y` (copy_path): copies the cursor entry's absolute path to the
    /// system clipboard via an OSC 52 terminal escape (see
    /// `external::osc52_copy_sequence`, written to stdout by `main.rs`'s
    /// loop once it drains `pending_clipboard`) — works over SSH/tmux and
    /// needs no extra dependency, unlike a native-clipboard crate. Never
    /// fails loudly: a terminal that doesn't understand OSC 52 just
    /// silently ignores it (no reliable way to detect support up front),
    /// so this always logs success rather than trying to guess.
    fn begin_copy_path(&mut self) {
        let Some(path) = self.active_pane().selected_entry_path() else {
            self.log_error("no entry selected to copy the path of");
            return;
        };
        let text = path.to_string_lossy().into_owned();
        self.pending_clipboard = Some(text.clone());
        self.log_info(format!("copied: {text}"));
    }

    /// `c` (duplicate): prompts for a new name (prefilled with the current
    /// one) and, on commit, copies the cursor entry to that name in the
    /// *same* directory — via the same background-task machinery as
    /// Copy/Move, so a large directory duplicates asynchronously with
    /// progress too.
    fn begin_duplicate(&mut self) {
        let Some(name) = self.active_pane().selected_entry_name() else {
            self.log_error("no entry selected to duplicate");
            return;
        };
        let Some(source) = self.active_pane().selected_entry_path() else {
            self.log_error("no entry selected to duplicate");
            return;
        };
        self.mode = Mode::Prompt {
            kind: PromptKind::Duplicate { source },
            input: LineEditor::from_str(&name),
        };
    }

    /// Validates and spawns the actual duplicate once the prompt commits:
    /// the name must be non-empty, must not contain a path separator (it
    /// stays in the same directory — this isn't a move), and must differ
    /// from the source's own name (a same-name "duplicate" would just
    /// collide with itself).
    fn commit_duplicate(&mut self, source: PathBuf, name: String) {
        if name.is_empty() {
            self.log_error("name cannot be empty");
            return;
        }
        if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
            self.log_error("name cannot contain a path separator");
            return;
        }
        let Some(parent) = source.parent() else {
            self.log_error("cursor entry has no parent directory");
            return;
        };
        let dest = parent.join(&name);
        if dest == source {
            self.log_error("new name must differ from the current name");
            return;
        }
        if dest.exists() {
            self.log_error(format!("{} already exists", dest.display()));
            return;
        }
        self.spawn_duplicate(source, dest);
    }

    /// Hands the actual duplicate off to a background task (see
    /// `tasks::copy_move::run_duplicate`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_duplicate(&mut self, source: PathBuf, dest: PathBuf) {
        let desc = format!("duplicate {} to {}", source.display(), dest.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            copy_move::run_duplicate(id, tx, cancel, source, dest);
        });
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
    /// in display order. A free function of `&self.mode` rather than a
    /// stored `Vec` on the mode itself, so the list can never go stale
    /// relative to whatever's actually typed.
    fn function_list_filtered_actions(&self) -> Vec<Action> {
        let Mode::FunctionList { input, .. } = &self.mode else {
            return Vec::new();
        };
        crate::function_list::filter_actions(&input.value())
    }

    /// Fixed keys for `Mode::FunctionList`; never consults the keymap (the
    /// palette's whole purpose is to run an action *by name*, key-free).
    /// `Esc` cancels; `Enter` closes the palette (back to Normal) and then
    /// dispatches the highlighted action — in that order, so an action
    /// that itself sets a mode (a Prompt/Confirm/Select) isn't immediately
    /// clobbered back to Normal afterward.
    fn handle_function_list_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                return;
            }
            KeyCode::Enter => {
                let action = self
                    .function_list_filtered_actions()
                    .get(match &self.mode {
                        Mode::FunctionList { cursor, .. } => *cursor,
                        _ => return,
                    })
                    .copied();
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

        let Mode::FunctionList { input, cursor } = &mut self.mode else {
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
        // The filtered list changes on every edit; keep the highlight in
        // bounds by just resetting to the top rather than trying to track
        // "the same action" across a re-filter.
        *cursor = 0;
    }

    fn begin_rename(&mut self) {
        match self.active_pane().selected_entry_name() {
            Some(name) => {
                self.mode = Mode::Prompt {
                    kind: PromptKind::Rename { orig: name.clone() },
                    input: LineEditor::from_str(&name),
                };
            }
            None => self.log_error("no entry selected to rename"),
        }
    }

    fn begin_delete(&mut self) {
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to delete");
            return;
        }
        let message = format!("Delete {} item(s)? (y/n)", targets.len());
        self.mode = Mode::Confirm {
            message,
            on_yes: PendingOp::Delete { targets },
        };
    }

    /// Copy/Move always confirm by default (`config.confirm_operations`);
    /// with it set to `false`, a transfer with no filename collision skips
    /// straight to `spawn_transfer` — but a collision *always* confirms
    /// regardless, and when both apply it's a single combined dialog
    /// (`Copy 3 item(s) -> /dest? (2 will be overwritten) (y/n)`) rather
    /// than two sequential ones.
    fn begin_transfer(&mut self, kind: TransferKind) {
        let sources = self.active_pane().marked_or_cursor();
        if sources.is_empty() {
            self.log_error("no entry selected");
            return;
        }
        let dest_dir = self.panes[self.active.other().index()].cwd.clone();
        if sources.iter().any(|src| dest_dir.starts_with(src)) {
            self.log_error("cannot copy/move a directory into itself or a descendant");
            return;
        }

        let collisions = copy_move::find_collisions(&sources, &dest_dir);
        if collisions.is_empty() && !self.config.confirm_operations {
            self.spawn_transfer(kind, sources, dest_dir);
            return;
        }

        let verb = match kind {
            TransferKind::Copy => "Copy",
            TransferKind::Move => "Move",
        };
        let mut message = format!(
            "{verb} {} item(s) -> {}?",
            sources.len(),
            dest_dir.display()
        );
        if !collisions.is_empty() {
            message.push_str(&format!(" ({} will be overwritten)", collisions.len()));
        }
        message.push_str(" (y/n)");

        self.mode = Mode::Confirm {
            message,
            on_yes: PendingOp::Overwrite {
                kind,
                sources,
                dest_dir,
            },
        };
    }

    /// Hands the actual copy/move off to a background task (see
    /// `tasks::copy_move`); `dispatch`/`execute_pending` return immediately,
    /// and completion arrives later as a `TaskEvent::Finished` drained by
    /// `drain_tasks`.
    fn spawn_transfer(&mut self, kind: TransferKind, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        let verb = match kind {
            TransferKind::Copy => "copy",
            TransferKind::Move => "move",
        };
        let desc = format!("{verb} {} item(s) to {}", sources.len(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| match kind {
            TransferKind::Copy => copy_move::run_copy(id, tx, cancel, sources, dest_dir),
            TransferKind::Move => copy_move::run_move(id, tx, cancel, sources, dest_dir),
        });
    }

    /// Hands the actual delete off to a background task (see
    /// `tasks::delete`); see `spawn_transfer` for the completion story.
    fn spawn_delete(&mut self, targets: Vec<PathBuf>) {
        let behavior = self.config.delete_behavior;
        let desc = format!("delete {} item(s)", targets.len());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            delete_task::run_delete(id, tx, cancel, targets, behavior);
        });
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
        self.active_pane_mut().set_filter(FilterSpec::parse(&value));
    }

    /// Fixed editing keys for `Mode::Prompt`; never consults the keymap.
    fn handle_prompt_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Esc => {
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
        self.pending_external = Some(ExternalRequest {
            cmdline,
            cwd,
            pause_after: true,
        });
    }

    fn commit_zip_name(&mut self, targets: Vec<PathBuf>, name: String) {
        if name.is_empty() {
            self.log_error("name cannot be empty");
            return;
        }
        if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
            self.log_error("name cannot contain a path separator");
            return;
        }

        let dest_dir = self.panes[self.active.other().index()].cwd.clone();
        let archive_path = dest_dir.join(&name);
        if archive_path.exists() {
            let message = format!("Overwrite {}? (y/n)", archive_path.display());
            self.mode = Mode::Confirm {
                message,
                on_yes: PendingOp::ZipOverwrite {
                    targets,
                    archive_path,
                },
            };
        } else {
            self.spawn_zip(targets, archive_path);
        }
    }

    /// Opens the zip-name prompt for the active pane's marked-or-cursor
    /// selection, pre-filled with `<first-target-stem>.zip`.
    fn begin_zip(&mut self) {
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to zip");
            return;
        }
        let stem = targets[0]
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string());
        let default_name = format!("{stem}.zip");
        self.mode = Mode::Prompt {
            kind: PromptKind::ZipName { targets },
            input: LineEditor::from_str(&default_name),
        };
    }

    /// Hands the actual zip creation off to a background task (see
    /// `tasks::archive::run_zip`); see `spawn_transfer` for the completion
    /// story.
    fn spawn_zip(&mut self, targets: Vec<PathBuf>, archive_path: PathBuf) {
        let desc = format!(
            "zip {} item(s) to {}",
            targets.len(),
            archive_path.display()
        );
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_zip(id, tx, cancel, targets, archive_path);
        });
    }

    /// The cursor entry must be a `.zip` file; extracts into the other
    /// pane's cwd, confirming first if any top-level entry would collide.
    fn begin_unzip(&mut self) {
        let Some(name) = self.active_pane().selected_entry_name() else {
            self.log_error("no entry selected to unzip");
            return;
        };
        if !name.to_lowercase().ends_with(".zip") {
            self.log_error("selected entry is not a .zip file");
            return;
        }
        let archive_path = self.active_pane().cwd.join(&name);
        let dest_dir = self.panes[self.active.other().index()].cwd.clone();

        match archive::top_level_collisions(&archive_path, &dest_dir) {
            Ok(collisions) if !collisions.is_empty() => {
                let message = format!("Overwrite {} existing item(s)? (y/n)", collisions.len());
                self.mode = Mode::Confirm {
                    message,
                    on_yes: PendingOp::UnzipOverwrite {
                        archive_path,
                        dest_dir,
                    },
                };
            }
            Ok(_) => self.spawn_unzip(archive_path, dest_dir),
            Err(err) => self.log_error(err.to_string()),
        }
    }

    /// Hands the actual extraction off to a background task (see
    /// `tasks::archive::run_unzip`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_unzip(&mut self, archive_path: PathBuf, dest_dir: PathBuf) {
        let desc = format!("unzip {} to {}", archive_path.display(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_unzip(id, tx, cancel, archive_path, dest_dir);
        });
    }

    /// Fixed confirmation keys for `Mode::Confirm`; never consults the
    /// keymap. `y`/`Y` executes the pending op, anything else (including
    /// Esc) cancels.
    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') => {
                if let Mode::Confirm { on_yes, .. } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                {
                    self.execute_pending(on_yes);
                }
            }
            _ => self.mode = Mode::Normal,
        }
    }

    fn execute_pending(&mut self, op: PendingOp) {
        match op {
            PendingOp::Delete { targets } => self.spawn_delete(targets),
            PendingOp::Overwrite {
                kind,
                sources,
                dest_dir,
            } => self.spawn_transfer(kind, sources, dest_dir),
            PendingOp::ZipOverwrite {
                targets,
                archive_path,
            } => self.spawn_zip(targets, archive_path),
            PendingOp::UnzipOverwrite {
                archive_path,
                dest_dir,
            } => self.spawn_unzip(archive_path, dest_dir),
            PendingOp::Quit => self.should_quit = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn test_app(left: &Path, right: &Path) -> App {
        App::new(left.to_path_buf(), right.to_path_buf(), Config::default()).unwrap()
    }

    /// Drains tasks in a loop until none are running, or panics after a
    /// generous timeout. Background tasks in these tests are tiny
    /// (single small files in a tempdir), so this should resolve almost
    /// immediately; the loop only exists to avoid a fixed sleep racing the
    /// worker thread.
    fn wait_for_tasks_done(app: &mut App) {
        for _ in 0..500 {
            app.drain_tasks();
            if app.tasks.running.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("background task(s) did not finish in time");
    }

    #[test]
    fn quit_action_confirms_by_default_then_quits_on_y() {
        // confirm_quit defaults to true: with nothing running, Quit must
        // now confirm rather than quit immediately.
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert!(!app.should_quit);
        app.dispatch(Action::Quit);
        assert!(!app.should_quit, "must confirm before quitting by default");
        match &app.mode {
            Mode::Confirm { message, .. } => assert_eq!(message, "Quit ozzel? (y/n)"),
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_confirm_declined_keeps_the_app_running_when_nothing_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::Quit);
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.should_quit);
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn quit_with_confirm_quit_false_and_no_tasks_quits_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                confirm_quit: false,
                ..Config::default()
            },
        )
        .unwrap();
        app.dispatch(Action::Quit);
        assert!(app.should_quit, "confirm_quit=false must quit immediately");
    }

    #[test]
    fn quit_tasks_running_confirm_is_unaffected_by_confirm_quit_false() {
        // The tasks-running confirm is unconditional — confirm_quit only
        // governs the "nothing running" case.
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                confirm_quit: false,
                ..Config::default()
            },
        )
        .unwrap();
        app.tasks.spawn("noop", |id, tx, _| {
            std::thread::sleep(Duration::from_millis(200));
            let _ = tx.send(TaskEvent::Finished {
                id,
                result: Ok("done".to_string()),
            });
        });

        app.dispatch(Action::Quit);
        assert!(
            !app.should_quit,
            "must still confirm when tasks are running, even with confirm_quit=false"
        );
        assert!(matches!(app.mode, Mode::Confirm { .. }));

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_with_running_task_asks_for_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.tasks.spawn("noop", |id, tx, _| {
            std::thread::sleep(Duration::from_millis(200));
            let _ = tx.send(TaskEvent::Finished {
                id,
                result: Ok("done".to_string()),
            });
        });

        app.dispatch(Action::Quit);
        assert!(!app.should_quit, "must not quit while a task is running");
        assert!(matches!(app.mode, Mode::Confirm { .. }));

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit, "confirming quit-anyway must still quit");
    }

    #[test]
    fn quit_confirmation_declined_keeps_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.tasks.spawn("noop", |id, tx, _| {
            std::thread::sleep(Duration::from_millis(200));
            let _ = tx.send(TaskEvent::Finished {
                id,
                result: Ok("done".to_string()),
            });
        });

        app.dispatch(Action::Quit);
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(!app.should_quit);
        assert!(matches!(app.mode, Mode::Normal));
        wait_for_tasks_done(&mut app);
    }

    #[test]
    fn switch_pane_toggles_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert_eq!(app.active, ActivePane::Left);
        app.dispatch(Action::SwitchPane);
        assert_eq!(app.active, ActivePane::Right);
    }

    #[test]
    fn swap_panes_swaps_cwd() {
        let left_dir = tempfile::tempdir().unwrap();
        let right_dir = tempfile::tempdir().unwrap();
        let mut app = test_app(left_dir.path(), right_dir.path());
        app.dispatch(Action::SwapPanes);
        assert_eq!(app.panes[0].cwd, right_dir.path());
        assert_eq!(app.panes[1].cwd, left_dir.path());
    }

    #[test]
    fn keymap_resolves_q_and_ctrl_c_to_quit() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path(), dir.path());
        assert_eq!(
            app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            app.keymap
                .resolve(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
    }

    #[test]
    fn mkdir_prompt_creates_directory_on_enter() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::Mkdir);
        assert!(matches!(app.mode, Mode::Prompt { .. }));

        for c in "newdir".chars() {
            app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.mode, Mode::Normal));
        assert!(dir.path().join("newdir").is_dir());
    }

    fn select_entry_named(app: &mut App, name: &str) {
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(|item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == name))
            .unwrap();
        app.active_pane_mut().cursor = idx;
    }

    #[test]
    fn rename_prompt_is_prefilled_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "old.txt");

        app.dispatch(Action::Rename);
        match &app.mode {
            Mode::Prompt { kind, input } => {
                assert_eq!(
                    *kind,
                    PromptKind::Rename {
                        orig: "old.txt".to_string()
                    }
                );
                assert_eq!(input.value(), "old.txt");
            }
            other => panic!("expected Prompt mode, got {other:?}"),
        }

        // Clear the prefilled text and type a new name.
        for _ in 0..7 {
            app.handle_event(AppEvent::Input(KeyCode::Backspace, KeyModifiers::NONE));
        }
        for c in "new.txt".chars() {
            app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

        assert!(!dir.path().join("old.txt").exists());
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn delete_requires_confirmation_then_removes_via_background_task() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("victim.txt"), b"hi").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                delete_behavior: crate::config::DeleteBehavior::Permanent,
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "victim.txt");

        app.dispatch(Action::Delete);
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        assert!(
            dir.path().join("victim.txt").exists(),
            "not deleted before confirm"
        );

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(
            !app.tasks.running.is_empty(),
            "delete should now be running in the background"
        );

        wait_for_tasks_done(&mut app);
        assert!(!dir.path().join("victim.txt").exists());
        assert!(
            app.log.iter().any(|l| l.message.contains("deleted 1 item")),
            "finished delete should log a summary"
        );
    }

    #[test]
    fn delete_confirmation_declined_keeps_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), b"hi").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                delete_behavior: crate::config::DeleteBehavior::Permanent,
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "keep.txt");

        app.dispatch(Action::Delete);
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(
            app.tasks.running.is_empty(),
            "declining must never spawn anything"
        );
        assert!(dir.path().join("keep.txt").exists());
    }

    #[test]
    fn copy_action_confirms_by_default_then_spawns_a_background_task() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        match &app.mode {
            Mode::Confirm { message, .. } => {
                assert!(message.starts_with("Copy 1 item(s)"), "message: {message}");
                assert!(
                    message.contains(&right.path().display().to_string()),
                    "message: {message}"
                );
                assert!(
                    !message.contains("overwritten"),
                    "no collision => no overwrite note; message: {message}"
                );
            }
            other => {
                panic!("confirm_operations defaults to true, expected Mode::Confirm, got {other:?}")
            }
        }
        assert!(
            app.tasks.running.is_empty(),
            "must not spawn before confirmation"
        );

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(
            !app.tasks.running.is_empty(),
            "copy should now be running in the background"
        );

        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
        assert!(left.path().join("a.txt").exists(), "copy keeps the source");
    }

    #[test]
    fn copy_confirm_declined_does_not_spawn_or_copy() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.tasks.running.is_empty());
        assert!(!right.path().join("a.txt").exists());
    }

    #[test]
    fn move_action_also_confirms_by_default() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Move);
        match &app.mode {
            Mode::Confirm { message, .. } => {
                assert!(message.starts_with("Move 1 item(s)"), "message: {message}")
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
        assert!(
            !left.path().join("a.txt").exists(),
            "move removes the source"
        );
    }

    #[test]
    fn copy_skips_confirm_when_confirm_operations_false_and_no_collision() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = App::new(
            left.path().to_path_buf(),
            right.path().to_path_buf(),
            Config {
                confirm_operations: false,
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        assert!(
            matches!(app.mode, Mode::Normal),
            "confirm_operations=false + no collision => no prompt"
        );
        assert!(!app.tasks.running.is_empty());

        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
    }

    #[test]
    fn copy_collision_still_confirms_when_confirm_operations_false_with_a_combined_message() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"new").unwrap();
        std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

        let mut app = App::new(
            left.path().to_path_buf(),
            right.path().to_path_buf(),
            Config {
                confirm_operations: false,
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        match &app.mode {
            Mode::Confirm { message, .. } => {
                assert!(
                    message.contains("1 will be overwritten"),
                    "collision must always confirm even with confirm_operations=false; message: {message}"
                );
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        wait_for_tasks_done(&mut app);
        assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
    }

    #[test]
    fn copy_collision_requires_confirmation_before_spawning() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"new").unwrap();
        std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        assert!(
            app.tasks.running.is_empty(),
            "must not spawn before confirmation"
        );
        assert_eq!(
            std::fs::read(right.path().join("a.txt")).unwrap(),
            b"existing"
        );

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        wait_for_tasks_done(&mut app);
        assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
    }

    #[test]
    fn two_concurrent_transfers_both_complete() {
        let left = tempfile::tempdir().unwrap();
        let mid = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();
        std::fs::write(mid.path().join("b.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");
        app.dispatch(Action::Copy);
        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(app.tasks.running.len(), 1);

        // A second, independent transfer spawned while the first is (very
        // likely still) in flight.
        app.panes[0].cwd = mid.path().to_path_buf();
        app.panes[0].reload().unwrap();
        select_entry_named(&mut app, "b.txt");
        app.dispatch(Action::Copy);
        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));

        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
        assert!(right.path().join("b.txt").exists());
    }

    #[test]
    fn open_on_directory_navigates_and_records_history() {
        // `open` merges the old Enter/View actions: on a directory it
        // navigates (the old View action used to error here instead —
        // "cursor is not on a file" — that behavior is gone now that
        // there's only one context-dependent action bound to both `Enter`
        // and `o`).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "sub");

        app.dispatch(Action::Open);
        assert_eq!(app.panes[0].cwd, dir.path().join("sub"));
        assert_eq!(
            app.history.ring(Side::Left).first(),
            Some(&dir.path().join("sub"))
        );
    }

    #[test]
    fn parent_navigation_also_records_history() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = test_app(&sub, &sub);

        app.dispatch(Action::Parent);
        assert_eq!(app.panes[0].cwd, dir.path());
        assert_eq!(
            app.history.ring(Side::Left).first(),
            Some(&dir.path().to_path_buf())
        );
    }

    #[test]
    fn go_home_jumps_to_configured_home_and_records_history() {
        let start = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            start.path().to_path_buf(),
            start.path().to_path_buf(),
            Config {
                home: Some(home.path().to_path_buf()),
                ..Config::default()
            },
        )
        .unwrap();

        app.dispatch(Action::GoHome);
        assert_eq!(app.panes[0].cwd, home.path());
        assert_eq!(
            app.history.ring(Side::Left).first(),
            Some(&home.path().to_path_buf())
        );
    }

    #[test]
    fn go_home_errors_and_stays_put_when_configured_home_is_missing() {
        let start = tempfile::tempdir().unwrap();
        let mut app = App::new(
            start.path().to_path_buf(),
            start.path().to_path_buf(),
            Config {
                home: Some(PathBuf::from("/does/not/exist/at/all/ozzel-test")),
                ..Config::default()
            },
        )
        .unwrap();

        app.dispatch(Action::GoHome);
        assert_eq!(app.panes[0].cwd, start.path());
        assert!(app.log.iter().any(|l| l.is_error));
    }

    #[test]
    fn bookmark_add_dedups_and_marks_dirty_only_when_actually_added() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert!(!app.bookmarks_dirty);

        app.dispatch(Action::BookmarkAdd);
        assert_eq!(app.bookmarks.paths, vec![dir.path().to_path_buf()]);
        assert!(app.bookmarks_dirty);

        app.bookmarks_dirty = false;
        app.dispatch(Action::BookmarkAdd); // duplicate
        assert_eq!(app.bookmarks.paths.len(), 1, "must not add a duplicate");
        assert!(
            !app.bookmarks_dirty,
            "a no-op add must not mark dirty again"
        );
    }

    #[test]
    fn bookmark_jump_menu_enter_navigates_active_pane() {
        let target = tempfile::tempdir().unwrap();
        let start = tempfile::tempdir().unwrap();
        let mut app = test_app(start.path(), start.path());
        app.bookmarks.add(target.path().to_path_buf());

        app.dispatch(Action::BookmarkJump);
        assert!(matches!(
            app.mode,
            Mode::Select {
                kind: SelectKind::Bookmark,
                ..
            }
        ));

        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.panes[0].cwd, target.path());
    }

    #[test]
    fn bookmark_jump_menu_esc_cancels_without_navigating() {
        let target = tempfile::tempdir().unwrap();
        let start = tempfile::tempdir().unwrap();
        let mut app = test_app(start.path(), start.path());
        app.bookmarks.add(target.path().to_path_buf());

        app.dispatch(Action::BookmarkJump);
        app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(app.panes[0].cwd, start.path(), "Esc must not navigate");
    }

    #[test]
    fn bookmark_menu_down_then_d_deletes_the_highlighted_entry() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let start = tempfile::tempdir().unwrap();
        let mut app = test_app(start.path(), start.path());
        app.bookmarks.add(a.path().to_path_buf());
        app.bookmarks.add(b.path().to_path_buf());

        app.dispatch(Action::BookmarkJump);
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        app.handle_event(AppEvent::Input(KeyCode::Char('d'), KeyModifiers::NONE));

        assert_eq!(app.bookmarks.paths, vec![a.path().to_path_buf()]);
        assert!(app.bookmarks_dirty);
        match &app.mode {
            Mode::Select { items, .. } => {
                assert_eq!(items.len(), 1, "menu list must refresh after delete")
            }
            other => panic!("expected Select mode to stay open, got {other:?}"),
        }
    }

    #[test]
    fn history_jump_menu_lists_most_recent_first_and_selects() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        select_entry_named(&mut app, "sub");
        app.dispatch(Action::Open); // history: [sub]
        app.dispatch(Action::Parent); // history: [dir, sub]

        app.dispatch(Action::HistoryJump);
        assert!(matches!(
            app.mode,
            Mode::Select {
                kind: SelectKind::History,
                ..
            }
        ));

        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.panes[0].cwd, sub);
    }

    #[test]
    fn history_jump_with_empty_history_logs_error_instead_of_opening_menu() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::HistoryJump);
        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.log.iter().any(|l| l.is_error));
    }

    #[test]
    fn command_line_prompt_commit_sets_pending_external() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::CommandLine);
        assert!(matches!(
            app.mode,
            Mode::Prompt {
                kind: PromptKind::Command,
                ..
            }
        ));

        for c in "ls -la".chars() {
            app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.mode, Mode::Normal));
        let req = app
            .pending_external
            .take()
            .expect("expected a pending external request");
        assert_eq!(req.cmdline, "ls -la");
        assert_eq!(req.cwd, dir.path());
        assert!(req.pause_after);
    }

    #[test]
    fn command_line_empty_input_cancels_without_pending_external() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::CommandLine);
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(app.mode, Mode::Normal));
        assert!(app.pending_external.is_none());
    }

    #[test]
    fn open_editor_queues_suspended_command_with_configured_editor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), b"hi").unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                editor: Some("vim".to_string()),
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "file.txt");

        app.dispatch(Action::OpenEditor);
        let req = app
            .pending_external
            .take()
            .expect("expected a pending external request");
        assert!(
            req.cmdline.starts_with("vim "),
            "cmdline was: {}",
            req.cmdline
        );
        assert!(req.cmdline.contains("file.txt"));
        assert!(
            !req.pause_after,
            "editors don't get the press-any-key pause"
        );
    }

    #[test]
    fn open_editor_errors_when_cursor_is_on_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                editor: Some("vim".to_string()),
                ..Config::default()
            },
        )
        .unwrap();
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "sub");

        app.dispatch(Action::OpenEditor);
        assert!(app.pending_external.is_none());
        assert!(
            app.log
                .iter()
                .any(|l| l.is_error && l.message.contains("not on a file"))
        );
    }

    #[test]
    fn edit_config_creates_the_template_when_missing_and_queues_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                editor: Some("vim".to_string()),
                ..Config::default()
            },
        )
        .unwrap();

        // Nested + missing: exercises both the create_dir_all and the
        // template-writing halves of ensure_config_file_exists.
        let config_path = dir.path().join("nested").join("config.toml");
        assert!(!config_path.exists());

        app.begin_edit_config_at(config_path.clone());

        assert!(
            config_path.exists(),
            "must create the file from the template"
        );
        let written = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            written.contains("delete_behavior"),
            "should be the examples/config.toml template, got: {written}"
        );

        let req = app
            .pending_external
            .take()
            .expect("expected a pending external request");
        assert!(req.cmdline.starts_with("vim "), "cmdline: {}", req.cmdline);
        assert!(req.cmdline.contains("config.toml"));
        assert!(!req.pause_after);
        assert!(
            app.pending_config_reload,
            "must queue a reload for after the editor exits"
        );
    }

    #[test]
    fn edit_config_does_not_overwrite_an_existing_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "delete_behavior = \"permanent\"").unwrap();

        app.begin_edit_config_at(config_path.clone());

        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            "delete_behavior = \"permanent\"",
            "an existing config file must be left untouched"
        );
    }

    #[test]
    fn edit_config_falls_back_to_vim_when_no_editor_or_env_var_is_set() {
        let dir = tempfile::tempdir().unwrap();
        // No `config.editor` set — this is exactly the case OpenEditor
        // would refuse ("no editor configured"), but edit_config must
        // still work out of the box per the user's request.
        let mut app = test_app(dir.path(), dir.path());
        // Isolate from whatever $EDITOR happens to be set in the test
        // environment, so this assertion is deterministic everywhere.
        // SAFETY: single-threaded w.r.t. this var within this test process
        // is not guaranteed by the test harness, but no other test reads
        // or depends on $EDITOR, so this is safe in practice.
        unsafe {
            std::env::remove_var("EDITOR");
        }

        app.begin_edit_config_at(dir.path().join("config.toml"));

        let req = app.pending_external.unwrap();
        assert!(
            req.cmdline.starts_with("vim "),
            "must fall back to a hardcoded vim, cmdline: {}",
            req.cmdline
        );
    }

    #[test]
    fn reload_config_success_swaps_the_keymap_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        // Not bound by default; proves the *new* config's keymap is the one
        // actually in effect afterward, not just re-parsed and discarded.
        assert_eq!(
            app.keymap.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
            None
        );

        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[keys]\n\"z\" = \"quit\"\n").unwrap();
        app.reload_config_from(&config_path);

        assert_eq!(
            app.keymap.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
            Some(Action::Quit),
            "the reloaded config's [keys] override must take effect immediately"
        );
        assert!(
            app.log
                .iter()
                .any(|l| !l.is_error && l.message == "config reloaded"),
            "log: {:?}",
            app.log
        );
    }

    #[test]
    fn reload_config_failure_keeps_the_old_config_and_keymap_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                delete_behavior: crate::config::DeleteBehavior::Permanent,
                ..Config::default()
            },
        )
        .unwrap();

        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "delete_behavior = [not valid").unwrap();
        app.reload_config_from(&config_path);

        assert_eq!(
            app.config.delete_behavior,
            crate::config::DeleteBehavior::Permanent,
            "a parse error must leave the old config completely untouched"
        );
        assert!(
            app.log
                .iter()
                .any(|l| l.is_error && l.message.starts_with("config reload failed")),
            "log: {:?}",
            app.log
        );
    }

    #[test]
    fn reload_config_bad_keys_entry_keeps_the_old_keymap_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        let original_quit = app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE);

        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "[keys]\n\"q\" = \"not_a_real_action\"\n").unwrap();
        app.reload_config_from(&config_path);

        assert_eq!(
            app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
            original_quit,
            "an invalid [keys] action name must leave the old keymap untouched"
        );
        assert!(
            app.log
                .iter()
                .any(|l| l.is_error && l.message.starts_with("config reload failed")),
            "log: {:?}",
            app.log
        );
    }

    #[test]
    fn open_default_errors_when_no_entry_selected() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        // Cursor starts on ".." (index 0): a tempdir always has a real
        // parent, so nothing is "selected" for OpenDefault's purposes.
        app.dispatch(Action::OpenDefault);
        assert!(
            app.log
                .iter()
                .any(|l| l.is_error && l.message.contains("no entry selected"))
        );
    }

    #[test]
    fn focus_left_and_focus_right_activate_the_named_pane() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert_eq!(app.active, ActivePane::Left);

        app.dispatch(Action::FocusRight);
        assert_eq!(app.active, ActivePane::Right);

        // No-op when already active.
        app.dispatch(Action::FocusRight);
        assert_eq!(app.active, ActivePane::Right);

        app.dispatch(Action::FocusLeft);
        assert_eq!(app.active, ActivePane::Left);
    }

    #[test]
    fn left_right_arrow_keys_switch_pane_focus_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(app.active, ActivePane::Right);
        app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(app.active, ActivePane::Left);
    }

    #[test]
    fn open_action_opens_the_built_in_viewer_on_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "notes.txt");

        app.dispatch(Action::Open);
        match &app.mode {
            Mode::Viewer {
                path,
                lines,
                bytes,
                view_mode,
                scroll,
                h_scroll,
                truncated,
            } => {
                assert_eq!(path, &dir.path().join("notes.txt"));
                assert_eq!(
                    lines,
                    &vec!["one".to_string(), "two".to_string(), "three".to_string()]
                );
                assert_eq!(bytes.as_slice(), b"one\ntwo\nthree");
                assert_eq!(*view_mode, ViewMode::Text);
                assert_eq!(*scroll, 0);
                assert_eq!(*h_scroll, 0);
                assert!(!truncated);
            }
            other => panic!("expected Mode::Viewer, got {other:?}"),
        }
    }

    #[test]
    fn open_on_a_file_opens_the_viewer_instead_of_navigating() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "hello").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "readme.txt");

        app.dispatch(Action::Open);
        assert!(matches!(app.mode, Mode::Viewer { .. }));
        assert_eq!(
            app.panes[0].cwd,
            dir.path(),
            "cwd must not change for a file"
        );
    }

    #[test]
    fn open_opens_a_binary_file_directly_in_hex_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin.dat"), [b'a', 0u8, b'b']).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "bin.dat");

        app.dispatch(Action::Open);
        assert_eq!(view_mode_of(&app), ViewMode::Hex);
    }

    #[test]
    fn viewer_tab_toggles_between_text_and_hex_and_resets_scroll() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "one\ntwo\nthree").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "notes.txt");
        app.dispatch(Action::Open);
        assert_eq!(view_mode_of(&app), ViewMode::Text);

        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 1);

        app.handle_event(AppEvent::Input(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(view_mode_of(&app), ViewMode::Hex);
        assert_eq!(scroll_of(&app), 0, "toggling mode resets scroll");

        app.handle_event(AppEvent::Input(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(view_mode_of(&app), ViewMode::Text);
    }

    #[test]
    fn viewer_hex_scroll_clamps_by_sixteen_byte_rows() {
        let dir = tempfile::tempdir().unwrap();
        // 20 NUL bytes -> sniffed as binary (opens in hex mode) and
        // ceil(20/16) = 2 hex rows, so max scroll index is 1.
        std::fs::write(dir.path().join("bytes.dat"), vec![0u8; 20]).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "bytes.dat");
        app.dispatch(Action::Open);
        assert_eq!(view_mode_of(&app), ViewMode::Hex);

        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 1);
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 1, "must clamp at the last hex row");

        app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 1);
        app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 0);
    }

    #[test]
    fn viewer_scroll_clamps_to_the_line_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lines.txt"), "1\n2\n3").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "lines.txt");
        app.dispatch(Action::Open);

        // Up from the very top stays at 0.
        app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 0);

        // Down twice reaches the last line (3 lines: max index 2)...
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 2);
        // ...and one more Down doesn't overshoot it.
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 2);

        app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 0);
        app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 2);
        app.handle_event(AppEvent::Input(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 0);
        app.handle_event(AppEvent::Input(KeyCode::Char('G'), KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 2);
    }

    #[test]
    fn viewer_page_up_down_clamp_too() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (0..5).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.path().join("many.txt"), content).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "many.txt");
        app.dispatch(Action::Open);

        app.handle_event(AppEvent::Input(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(
            scroll_of(&app),
            4,
            "PageDown must clamp to the last line, not overshoot"
        );
        app.handle_event(AppEvent::Input(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(scroll_of(&app), 0, "PageUp must clamp to 0, not underflow");
    }

    #[test]
    fn viewer_left_right_scroll_horizontally_without_underflow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wide.txt"), "a line of text").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "wide.txt");
        app.dispatch(Action::Open);

        // Left at the start must not underflow (saturating_sub).
        app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(h_scroll_of(&app), 0);

        app.handle_event(AppEvent::Input(KeyCode::Right, KeyModifiers::NONE));
        let after_one_right = h_scroll_of(&app);
        assert!(after_one_right > 0);
        app.handle_event(AppEvent::Input(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(h_scroll_of(&app), 0);
    }

    #[test]
    fn viewer_q_and_esc_close_back_to_normal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Open);
        assert!(matches!(app.mode, Mode::Viewer { .. }));
        app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));

        app.dispatch(Action::Open);
        assert!(matches!(app.mode, Mode::Viewer { .. }));
        app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
    }

    fn scroll_of(app: &App) -> usize {
        match &app.mode {
            Mode::Viewer { scroll, .. } => *scroll,
            other => panic!("expected Mode::Viewer, got {other:?}"),
        }
    }

    fn h_scroll_of(app: &App) -> usize {
        match &app.mode {
            Mode::Viewer { h_scroll, .. } => *h_scroll,
            other => panic!("expected Mode::Viewer, got {other:?}"),
        }
    }

    fn view_mode_of(app: &App) -> ViewMode {
        match &app.mode {
            Mode::Viewer { view_mode, .. } => *view_mode,
            other => panic!("expected Mode::Viewer, got {other:?}"),
        }
    }

    fn help_scroll_of(app: &App) -> usize {
        match &app.mode {
            Mode::Help { scroll } => *scroll,
            other => panic!("expected Mode::Help, got {other:?}"),
        }
    }

    #[test]
    fn help_action_opens_the_help_screen() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::Help);
        assert!(matches!(app.mode, Mode::Help { scroll: 0 }));
    }

    #[test]
    fn h_and_question_mark_both_open_help_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.handle_event(AppEvent::Input(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Help { .. }));
        app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));

        app.handle_event(AppEvent::Input(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Help { .. }));
    }

    #[test]
    fn shift_h_opens_history_jump_and_h_no_longer_does() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        // No history recorded yet, so History still logs its usual error —
        // the point here is just that `S-h` reaches HistoryJump, not `h`.
        app.handle_event(AppEvent::Input(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert!(
            app.log
                .iter()
                .any(|l| l.is_error && l.message.contains("no history")),
            "S-h must resolve to HistoryJump: {:?}",
            app.log
        );
        assert!(!matches!(app.mode, Mode::Help { .. }));
    }

    #[test]
    fn go_home_end_to_end_is_bound_only_to_tilde() {
        let start = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut app = App::new(
            start.path().to_path_buf(),
            start.path().to_path_buf(),
            Config {
                home: Some(home.path().to_path_buf()),
                ..Config::default()
            },
        )
        .unwrap();

        // `S-h`/`H` no longer reaches GoHome (it's HistoryJump now); the
        // pane must not have moved.
        app.handle_event(AppEvent::Input(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(app.panes[0].cwd, start.path());

        app.handle_event(AppEvent::Input(KeyCode::Char('~'), KeyModifiers::NONE));
        assert_eq!(app.panes[0].cwd, home.path());
    }

    #[test]
    fn help_screen_scroll_clamps_and_closes_with_q_esc_or_h() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.dispatch(Action::Help);
        let max_scroll = crate::help::build_lines(&app.keymap)
            .len()
            .saturating_sub(1);
        assert!(max_scroll > 0, "the listing must have more than one line");

        // Up from the top stays at 0.
        app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(help_scroll_of(&app), 0);

        app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(help_scroll_of(&app), max_scroll);
        // One more Down past the end doesn't overshoot.
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(help_scroll_of(&app), max_scroll);

        app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(help_scroll_of(&app), 0);

        app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));

        app.dispatch(Action::Help);
        app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));

        app.dispatch(Action::Help);
        app.handle_event(AppEvent::Input(KeyCode::Char('h'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn help_listing_reflects_a_keys_override_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Config {
                keys: {
                    let mut keys = std::collections::HashMap::new();
                    keys.insert("z".to_string(), "quit".to_string());
                    keys
                },
                ..Config::default()
            },
        )
        .unwrap();

        let lines = crate::help::build_lines(&app.keymap);
        let quit_row = lines.iter().find(
            |l| matches!(l, crate::help::HelpLine::Binding { action, .. } if *action == "quit"),
        );
        match quit_row {
            Some(crate::help::HelpLine::Binding { keys, .. }) => {
                assert!(keys.contains('z'), "keys: {keys}")
            }
            other => panic!("expected a quit binding row, got {other:?}"),
        }

        app.dispatch(Action::Help);
        assert!(matches!(app.mode, Mode::Help { .. }));
    }

    // --- Round 5: history back/forward -------------------------------

    #[test]
    fn history_back_and_forward_walk_the_per_pane_stack() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.navigate(|pane| pane.jump_to(sub.clone()));
        assert_eq!(app.active_pane().cwd, sub);

        app.dispatch(Action::HistoryBack);
        assert_eq!(app.active_pane().cwd, dir.path());

        app.dispatch(Action::HistoryForward);
        assert_eq!(app.active_pane().cwd, sub);
    }

    #[test]
    fn history_back_with_nothing_to_go_back_to_logs_and_stays_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::HistoryBack);
        assert_eq!(app.active_pane().cwd, dir.path());
        assert!(app.log.back().unwrap().is_error);
    }

    #[test]
    fn history_forward_with_nothing_to_go_forward_to_logs_and_stays_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::HistoryForward);
        assert_eq!(app.active_pane().cwd, dir.path());
        assert!(app.log.back().unwrap().is_error);
    }

    #[test]
    fn a_new_navigation_after_going_back_clears_the_forward_stack() {
        let dir = tempfile::tempdir().unwrap();
        let sub_a = dir.path().join("a");
        let sub_b = dir.path().join("b");
        std::fs::create_dir(&sub_a).unwrap();
        std::fs::create_dir(&sub_b).unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.navigate(|pane| pane.jump_to(sub_a.clone()));
        app.dispatch(Action::HistoryBack); // back to dir.path()
        app.navigate(|pane| pane.jump_to(sub_b.clone())); // a fresh move

        app.dispatch(Action::HistoryForward);
        // Forward stack was cleared by the fresh navigation, so this must
        // be a no-op (still in sub_b), not a jump back to sub_a.
        assert_eq!(app.active_pane().cwd, sub_b);
    }

    // --- Round 5: show_log ---------------------------------------------

    #[test]
    fn show_log_opens_scrolled_to_the_bottom_and_q_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::ShowLog);
        match app.mode {
            Mode::Log { scroll_from_bottom } => assert_eq!(scroll_from_bottom, 0),
            other => panic!("expected Mode::Log, got {other:?}"),
        }
        app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn log_view_scroll_keys_move_and_saturate() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::ShowLog);

        app.handle_event(AppEvent::Input(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            Mode::Log {
                scroll_from_bottom: 1
            }
        ));

        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            Mode::Log {
                scroll_from_bottom: 0
            }
        ));

        // Down at 0 must saturate at 0, not underflow/panic.
        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            Mode::Log {
                scroll_from_bottom: 0
            }
        ));

        app.handle_event(AppEvent::Input(KeyCode::Home, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            Mode::Log {
                scroll_from_bottom: usize::MAX
            }
        ));

        app.handle_event(AppEvent::Input(KeyCode::End, KeyModifiers::NONE));
        assert!(matches!(
            app.mode,
            Mode::Log {
                scroll_from_bottom: 0
            }
        ));
    }

    // --- Round 5: copy_path ---------------------------------------------

    #[test]
    fn copy_path_queues_the_cursor_entrys_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        // dir.path() isn't the filesystem root, so index 0 is the
        // synthetic ".." row and index 1 is the real entry.
        app.active_pane_mut().cursor = 1;
        let expected = app
            .active_pane()
            .selected_entry_path()
            .expect("cursor must be on a real entry");
        app.dispatch(Action::CopyPath);
        assert_eq!(
            app.pending_clipboard.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn copy_path_with_no_selection_logs_an_error_and_queues_nothing() {
        let dir = tempfile::tempdir().unwrap(); // empty dir, only ".." (or nothing)
        let mut app = test_app(dir.path(), dir.path());
        // An empty, non-root directory's only row is "..", which has no
        // path — selected_entry_path() is None either way here.
        app.dispatch(Action::CopyPath);
        assert!(app.pending_clipboard.is_none());
    }

    // --- Round 5: duplicate ----------------------------------------------

    #[test]
    fn duplicate_prompt_is_prefilled_with_the_current_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        // Land the cursor on the real entry (index depends on whether ".."
        // is shown; dir.path() isn't the fs root, so ".." occupies index 0).
        app.active_pane_mut().cursor = 1;
        app.dispatch(Action::Duplicate);
        match &app.mode {
            Mode::Prompt {
                kind: PromptKind::Duplicate { source },
                input,
            } => {
                assert_eq!(source.file_name().unwrap(), "a.txt");
                assert_eq!(input.value(), "a.txt");
            }
            other => panic!("expected Mode::Prompt(Duplicate), got {other:?}"),
        }
    }

    #[test]
    fn duplicate_rejects_empty_separator_and_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        std::fs::write(&source, b"hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.commit_duplicate(source.clone(), String::new());
        assert!(app.log.back().unwrap().is_error);

        app.commit_duplicate(source.clone(), "sub/dir".to_string());
        assert!(app.log.back().unwrap().is_error);

        app.commit_duplicate(source.clone(), "a.txt".to_string());
        assert!(app.log.back().unwrap().is_error);
    }

    #[test]
    fn duplicate_rejects_a_colliding_destination_name() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        std::fs::write(&source, b"hi").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"already here").unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.commit_duplicate(source, "b.txt".to_string());
        assert!(app.log.back().unwrap().is_error);
        assert_eq!(
            std::fs::read(dir.path().join("b.txt")).unwrap(),
            b"already here"
        );
    }

    #[test]
    fn duplicate_copies_the_source_to_the_new_name_in_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.txt");
        std::fs::write(&source, b"hello").unwrap();
        let mut app = test_app(dir.path(), dir.path());

        app.commit_duplicate(source, "a_copy.txt".to_string());
        wait_for_tasks_done(&mut app);

        assert_eq!(
            std::fs::read(dir.path().join("a_copy.txt")).unwrap(),
            b"hello"
        );
        // The original must still be there — this is a copy, not a move.
        assert!(dir.path().join("a.txt").exists());
    }

    // --- Round 5: function_list -------------------------------------------

    #[test]
    fn function_list_opens_empty_and_lists_every_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::FunctionList);
        match &app.mode {
            Mode::FunctionList { input, cursor } => {
                assert_eq!(input.value(), "");
                assert_eq!(*cursor, 0);
            }
            other => panic!("expected Mode::FunctionList, got {other:?}"),
        }
        assert_eq!(
            app.function_list_filtered_actions().len(),
            Action::ALL.len()
        );
    }

    #[test]
    fn function_list_typing_narrows_and_resets_the_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::FunctionList);

        app.handle_event(AppEvent::Input(KeyCode::Down, KeyModifiers::NONE));
        if let Mode::FunctionList { cursor, .. } = &app.mode {
            assert_eq!(*cursor, 1);
        }

        for c in "mkdir".chars() {
            app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let filtered = app.function_list_filtered_actions();
        assert_eq!(filtered, vec![Action::Mkdir]);
        // Re-filtering resets the highlight to the top.
        if let Mode::FunctionList { cursor, .. } = &app.mode {
            assert_eq!(*cursor, 0);
        }
    }

    #[test]
    fn function_list_enter_closes_the_palette_then_dispatches_the_highlighted_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::FunctionList);
        for c in "mkdir".chars() {
            app.handle_event(AppEvent::Input(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_event(AppEvent::Input(KeyCode::Enter, KeyModifiers::NONE));
        // `mkdir` opens a Prompt — proves the palette closed *and* the
        // action actually dispatched (not just returned to Normal).
        assert!(matches!(
            app.mode,
            Mode::Prompt {
                kind: PromptKind::Mkdir,
                ..
            }
        ));
    }

    #[test]
    fn function_list_esc_cancels_without_dispatching_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.dispatch(Action::FunctionList);
        app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
    }

    // --- Round 5: mouse hit-testing and dispatch --------------------------

    fn test_layout(area: ratatui::layout::Rect, header_rows: u16, start: usize) -> PaneLayout {
        PaneLayout {
            area,
            rows_area: ratatui::layout::Rect {
                x: area.x + 1,
                y: area.y + 1 + header_rows,
                width: area.width.saturating_sub(2),
                height: area.height.saturating_sub(2 + header_rows),
            },
            start,
        }
    }

    #[test]
    fn hit_test_row_maps_a_click_inside_rows_area_to_an_entry_index() {
        let layout = test_layout(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            },
            0,
            0,
        );
        // rows_area starts at (1, 1); the 3rd row down is y=3 -> index 2.
        assert_eq!(hit_test_row(&layout, 5, 3), Some(2));
    }

    #[test]
    fn hit_test_row_honors_the_scroll_start_offset() {
        let layout = test_layout(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            },
            0,
            5,
        );
        assert_eq!(hit_test_row(&layout, 5, 1), Some(5));
    }

    #[test]
    fn hit_test_row_returns_none_outside_the_rows_area() {
        let layout = test_layout(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            },
            0,
            0,
        );
        assert_eq!(hit_test_row(&layout, 0, 0), None); // border row
        assert_eq!(hit_test_row(&layout, 5, 9), None); // border row (bottom)
        assert_eq!(hit_test_row(&layout, 100, 3), None); // off to the right
    }

    #[test]
    fn hit_test_row_accounts_for_a_2_row_header() {
        let layout = test_layout(
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 20,
                height: 10,
            },
            1, // one extra header content row
            0,
        );
        // rows_area now starts one row lower (y=2), so y=2 is index 0.
        assert_eq!(hit_test_row(&layout, 5, 2), Some(0));
        assert_eq!(hit_test_row(&layout, 5, 1), None); // the header row itself
    }

    fn left_pane_area() -> ratatui::layout::Rect {
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        }
    }

    fn right_pane_area() -> ratatui::layout::Rect {
        ratatui::layout::Rect {
            x: 20,
            y: 0,
            width: 20,
            height: 10,
        }
    }

    /// Builds a test `App` with two populated dirs (`sub` and files inside
    /// `left`) and both panes' `pane_layout` filled in, mirroring what
    /// `ui::draw` sets up every real frame — mouse tests need this to
    /// resolve screen coordinates back to entries.
    fn mouse_test_app() -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"hi").unwrap();
        std::fs::create_dir(dir.path().join("c_dir")).unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.pane_layout = [
            Some(test_layout(left_pane_area(), 0, 0)),
            Some(test_layout(right_pane_area(), 0, 0)),
        ];
        (dir, app)
    }

    fn click(app: &mut App, column: u16, row: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn release(app: &mut App, column: u16, row: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn drag(app: &mut App, column: u16, row: u16) {
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn left_click_on_an_entry_row_focuses_the_pane_and_moves_the_cursor() {
        let (_dir, mut app) = mouse_test_app();
        app.active = ActivePane::Right; // start on the other pane
        // Entries sorted: ".." isn't shown here since dir.path() as both
        // panes' root is being used as a plain (non-fs-root) directory, so
        // row 0 is "..", row 1 is the first real entry.
        click(&mut app, 5, 2); // rows_area y starts at 1 -> row index 1
        assert_eq!(app.active, ActivePane::Left);
        assert_eq!(app.panes[0].cursor, 1);
    }

    #[test]
    fn left_click_does_not_mark_the_row_it_lands_on() {
        // Regression test: a plain click (down, then up, no drag in
        // between) must only move the cursor — marking is drag-only.
        let (_dir, mut app) = mouse_test_app();
        click(&mut app, 5, 2);
        release(&mut app, 5, 2);
        assert!(app.panes[0].marks.is_empty());
    }

    #[test]
    fn click_on_the_header_or_border_only_focuses_without_moving_the_cursor() {
        let (_dir, mut app) = mouse_test_app();
        app.panes[0].cursor = 0;
        app.active = ActivePane::Right;
        click(&mut app, 0, 0); // the border, not inside rows_area
        assert_eq!(app.active, ActivePane::Left);
        assert_eq!(app.panes[0].cursor, 0);
    }

    #[test]
    fn drag_across_multiple_rows_marks_every_row_swept_over() {
        let (_dir, mut app) = mouse_test_app();
        click(&mut app, 5, 1); // origin: row index 0 (the ".." row's real
        // sibling, or ".." itself — either way, drag from here)
        drag(&mut app, 5, 3); // sweep down to row index 2
        release(&mut app, 5, 3);
        let marked = app.panes[0].marks.len();
        assert!(marked >= 1, "expected at least one row marked by the drag");
    }

    #[test]
    fn drag_crossing_into_the_other_pane_does_not_mark_there_or_change_focus() {
        let (_dir, mut app) = mouse_test_app();
        click(&mut app, 5, 2); // start the drag in the left pane
        assert_eq!(app.active, ActivePane::Left);

        drag(&mut app, 25, 2); // pointer crosses into the right pane's area
        assert_eq!(
            app.active,
            ActivePane::Left,
            "focus must not change mid-drag"
        );
        assert!(
            app.panes[1].marks.is_empty(),
            "the pane the drag didn't start in must never get marked"
        );
    }

    #[test]
    fn double_click_on_a_directory_navigates_into_it() {
        let (dir, mut app) = mouse_test_app();
        // Find c_dir's row by scanning visible entries.
        let idx = app.panes[0]
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "c_dir"),
            )
            .unwrap();
        let row_y = 1 + idx as u16;
        click(&mut app, 5, row_y);
        release(&mut app, 5, row_y);
        click(&mut app, 5, row_y); // second click, same row, immediately after
        assert_eq!(app.panes[0].cwd, dir.path().join("c_dir"));
    }

    #[test]
    fn wheel_scroll_moves_the_cursor_of_the_pane_under_the_pointer_without_changing_focus() {
        let (_dir, mut app) = mouse_test_app();
        app.active = ActivePane::Left;
        app.panes[0].cursor = 0;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.panes[0].cursor, MOUSE_WHEEL_STEP.min(3));
        assert_eq!(app.active, ActivePane::Left);

        // Scroll over the *other* (currently inactive) pane: must move
        // that pane's cursor, and must still not change focus.
        app.panes[1].cursor = 0;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 25,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.panes[1].cursor > 0);
        assert_eq!(app.active, ActivePane::Left);
    }
}
