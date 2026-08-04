//! Top-level application state: the two panes, which one is active, the
//! current input mode, running background tasks, and the `Action` dispatch
//! hub every Normal-mode key eventually funnels through.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;

use anyhow::Context as _;

use crate::action::Action;
use crate::config::Config;
use crate::event::{AppEvent, KeyCode, KeyModifiers, TaskEvent};
use crate::filter::FilterSpec;
use crate::keymap::Keymap;
use crate::mode::{LineEditor, Mode, PendingOp, PromptKind, TransferKind};
use crate::ops;
use crate::pane::{PAGE_SIZE, Pane};
use crate::tasks::delete as delete_task;
use crate::tasks::{TaskManager, archive, copy_move};

/// Log lines are capped so a long session's log can't grow without bound.
const LOG_CAPACITY: usize = 500;

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

/// One line in the log area.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub message: String,
    pub is_error: bool,
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
}

impl App {
    pub fn new(left: PathBuf, right: PathBuf, config: Config) -> anyhow::Result<Self> {
        let mut keymap = Keymap::default_dyna();
        keymap
            .merge_overrides(&config.keys)
            .context("invalid [keys] entry in config")?;
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
        })
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active.index()]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active.index()]
    }

    fn log_push(&mut self, message: String, is_error: bool) {
        if self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(LogLine { message, is_error });
    }

    pub fn log_info(&mut self, message: impl Into<String>) {
        self.log_push(message.into(), false);
    }

    pub fn log_error(&mut self, message: impl Into<String>) {
        self.log_push(message.into(), true);
    }

    /// Reloads both panes, trying to keep each pane's cursor on the
    /// same-named entry it was on before (see
    /// `Pane::reload_preserving_cursor`). Reload failures are logged, not
    /// propagated — one pane's unreadable directory shouldn't crash the UI.
    fn reload_both(&mut self) {
        for pane in &mut self.panes {
            if let Err(err) = pane.reload_preserving_cursor() {
                self.log.push_back(LogLine {
                    message: err.to_string(),
                    is_error: true,
                });
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
                Mode::Prompt { .. } => self.handle_prompt_key(code, modifiers),
                Mode::Confirm { .. } => self.handle_confirm_key(code),
            },
            AppEvent::Task(task_event) => self.handle_task_event(task_event),
            AppEvent::Tick => {}
        }
    }

    /// The single match hub every action flows through. Kept infallible on
    /// the outside (errors are logged instead of propagated) so the input
    /// loop never has to think about failure.
    pub fn dispatch(&mut self, action: Action) {
        let result = match action {
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
            Action::Enter => self.active_pane_mut().enter(),
            Action::Parent => self.active_pane_mut().go_parent(),
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
            Action::Quit => {
                self.begin_quit();
                Ok(())
            }
        };

        if let Err(err) = result {
            self.log_error(err.to_string());
        }
    }

    /// `q` quits immediately when nothing is running; otherwise asks for
    /// confirmation, since the spawned worker threads are detached and get
    /// killed outright (not gracefully stopped) if the process exits while
    /// they're still writing.
    fn begin_quit(&mut self) {
        if self.tasks.running.is_empty() {
            self.should_quit = true;
            return;
        }
        let message = format!(
            "{} task(s) running — quit anyway? (y/n)",
            self.tasks.running.len()
        );
        self.mode = Mode::Confirm {
            message,
            on_yes: PendingOp::Quit,
        };
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
        if collisions.is_empty() {
            self.spawn_transfer(kind, sources, dest_dir);
        } else {
            let message = format!("Overwrite {} existing item(s)? (y/n)", collisions.len());
            self.mode = Mode::Confirm {
                message,
                on_yes: PendingOp::Overwrite {
                    kind,
                    sources,
                    dest_dir,
                },
            };
        }
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
        }
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
    fn quit_action_sets_should_quit_when_nothing_running() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert!(!app.should_quit);
        app.dispatch(Action::Quit);
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
    fn copy_action_spawns_a_background_task_that_copies_to_the_other_pane() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        select_entry_named(&mut app, "a.txt");

        app.dispatch(Action::Copy);
        assert!(
            matches!(app.mode, Mode::Normal),
            "no collision => no prompt"
        );
        assert!(
            !app.tasks.running.is_empty(),
            "copy should be running in the background"
        );

        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
        assert!(left.path().join("a.txt").exists(), "copy keeps the source");
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
        assert_eq!(app.tasks.running.len(), 1);

        // A second, independent transfer spawned while the first is (very
        // likely still) in flight.
        app.panes[0].cwd = mid.path().to_path_buf();
        app.panes[0].reload().unwrap();
        select_entry_named(&mut app, "b.txt");
        app.dispatch(Action::Copy);

        wait_for_tasks_done(&mut app);
        assert!(right.path().join("a.txt").exists());
        assert!(right.path().join("b.txt").exists());
    }
}
