//! Top-level application state: the two panes, which one is active, the
//! current input mode, and the `Action` dispatch hub every Normal-mode key
//! eventually funnels through.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::action::Action;
use crate::config::Config;
use crate::event::{AppEvent, KeyCode, KeyModifiers};
use crate::keymap::Keymap;
use crate::mode::{LineEditor, Mode, PendingOp, PromptKind, TransferKind};
use crate::ops;
use crate::pane::{PAGE_SIZE, Pane};

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
}

impl App {
    pub fn new(left: PathBuf, right: PathBuf, config: Config) -> anyhow::Result<Self> {
        let mut keymap = Keymap::default_dyna();
        keymap
            .merge_overrides(&config.keys)
            .context("invalid [keys] entry in config")?;

        Ok(Self {
            panes: [Pane::new(left)?, Pane::new(right)?],
            active: ActivePane::Left,
            should_quit: false,
            mode: Mode::Normal,
            config,
            keymap,
            log: VecDeque::new(),
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

    fn reload_both(&mut self) {
        for pane in &mut self.panes {
            if let Err(err) = pane.reload() {
                self.log.push_back(LogLine {
                    message: err.to_string(),
                    is_error: true,
                });
            }
        }
    }

    /// Routes a normalized terminal event: `Normal` mode consults the
    /// `Keymap`, `Prompt`/`Confirm` consume fixed editing/confirmation keys
    /// directly (they never look at the keymap). `Tick` is a no-op.
    pub fn handle_event(&mut self, event: AppEvent) {
        let AppEvent::Input(code, modifiers) = event else {
            return;
        };

        // Snapshot which mode we're in so the match below doesn't need a
        // live borrow of `self.mode` while also calling `&mut self` methods.
        match &self.mode {
            Mode::Normal => {
                if let Some(action) = self.keymap.resolve(code, modifiers) {
                    self.dispatch(action);
                }
            }
            Mode::Prompt { .. } => self.handle_prompt_key(code, modifiers),
            Mode::Confirm { .. } => self.handle_confirm_key(code),
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
                let left = self.panes[0].reload();
                let right = self.panes[1].reload();
                left.and(right)
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
            Action::Quit => {
                self.should_quit = true;
                Ok(())
            }
        };

        if let Err(err) = result {
            self.log_error(err.to_string());
        }
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

        let collisions = ops::find_collisions(&sources, &dest_dir);
        if collisions.is_empty() {
            self.run_transfer(kind, &sources, &dest_dir);
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

    fn run_transfer(&mut self, kind: TransferKind, sources: &[PathBuf], dest_dir: &Path) {
        let mut succeeded = 0usize;
        let mut errors = Vec::new();
        for src in sources {
            let result = match kind {
                TransferKind::Copy => ops::copy_into(src, dest_dir),
                TransferKind::Move => ops::move_into(src, dest_dir),
            };
            match result {
                Ok(()) => succeeded += 1,
                Err(err) => errors.push(format!("{}: {err}", src.display())),
            }
        }

        let verb = match kind {
            TransferKind::Copy => "copied",
            TransferKind::Move => "moved",
        };
        if succeeded > 0 {
            self.log_info(format!(
                "{verb} {succeeded} item(s) to {}",
                dest_dir.display()
            ));
        }
        for error in errors {
            self.log_error(error);
        }

        self.reload_both();
        self.active_pane_mut().clear_marks();
    }

    fn run_delete(&mut self, targets: &[PathBuf]) {
        match ops::delete_paths(targets, self.config.delete_behavior) {
            Ok(()) => self.log_info(format!("deleted {} item(s)", targets.len())),
            Err(err) => self.log_error(err.to_string()),
        }
        self.reload_both();
        self.active_pane_mut().clear_marks();
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
        let cwd = self.active_pane().cwd.clone();
        match kind {
            PromptKind::Mkdir => match ops::mkdir(&cwd, &value) {
                Ok(()) => self.log_info(format!("created directory: {value}")),
                Err(err) => self.log_error(err.to_string()),
            },
            PromptKind::Rename { orig } => match ops::rename(&cwd, &orig, &value) {
                Ok(()) => self.log_info(format!("renamed {orig} -> {value}")),
                Err(err) => self.log_error(err.to_string()),
            },
        }
        self.reload_both();
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
            PendingOp::Delete { targets } => self.run_delete(&targets),
            PendingOp::Overwrite {
                kind,
                sources,
                dest_dir,
            } => self.run_transfer(kind, &sources, &dest_dir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(left: &Path, right: &Path) -> App {
        App::new(left.to_path_buf(), right.to_path_buf(), Config::default()).unwrap()
    }

    #[test]
    fn quit_action_sets_should_quit() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(dir.path(), dir.path());
        assert!(!app.should_quit);
        app.dispatch(Action::Quit);
        assert!(app.should_quit);
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

    #[test]
    fn rename_prompt_is_prefilled_and_commits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.txt"), b"hi").unwrap();
        let mut app = test_app(dir.path(), dir.path());
        app.active_pane_mut().reload().unwrap();
        // Move cursor onto "old.txt" (index 1: ".." is 0 since tempdir has
        // a real parent).
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "old.txt"),
            )
            .unwrap();
        app.active_pane_mut().cursor = idx;

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
    fn delete_requires_confirmation_then_removes() {
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
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "victim.txt"),
            )
            .unwrap();
        app.active_pane_mut().cursor = idx;

        app.dispatch(Action::Delete);
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        assert!(
            dir.path().join("victim.txt").exists(),
            "not deleted before confirm"
        );

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!dir.path().join("victim.txt").exists());
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
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "keep.txt"),
            )
            .unwrap();
        app.active_pane_mut().cursor = idx;

        app.dispatch(Action::Delete);
        app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(dir.path().join("keep.txt").exists());
    }

    #[test]
    fn copy_action_copies_marked_or_cursor_entry_to_other_pane() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"hi").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "a.txt"),
            )
            .unwrap();
        app.active_pane_mut().cursor = idx;

        app.dispatch(Action::Copy);
        assert!(
            matches!(app.mode, Mode::Normal),
            "no collision => runs immediately"
        );
        assert!(right.path().join("a.txt").exists());
        assert!(left.path().join("a.txt").exists(), "copy keeps the source");
    }

    #[test]
    fn copy_collision_requires_confirmation() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        std::fs::write(left.path().join("a.txt"), b"new").unwrap();
        std::fs::write(right.path().join("a.txt"), b"existing").unwrap();

        let mut app = test_app(left.path(), right.path());
        app.active_pane_mut().reload().unwrap();
        let idx = app
            .active_pane()
            .visible_entries()
            .iter()
            .position(
                |item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "a.txt"),
            )
            .unwrap();
        app.active_pane_mut().cursor = idx;

        app.dispatch(Action::Copy);
        assert!(matches!(app.mode, Mode::Confirm { .. }));
        assert_eq!(
            std::fs::read(right.path().join("a.txt")).unwrap(),
            b"existing"
        );

        app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(std::fs::read(right.path().join("a.txt")).unwrap(), b"new");
    }
}
