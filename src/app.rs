//! Top-level application state: the two panes, which one is active, and the
//! `Action` dispatch hub every input eventually funnels through.

use std::path::PathBuf;

use crate::action::Action;
use crate::event::{AppEvent, KeyCode, KeyModifiers};
use crate::pane::{PAGE_SIZE, Pane};

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

pub struct App {
    pub panes: [Pane; 2],
    pub active: ActivePane,
    pub should_quit: bool,
    /// Most recent error from a dispatched action (e.g. a failed reload),
    /// surfaced in the log area. Cleared on the next successful action.
    pub last_error: Option<String>,
}

impl App {
    pub fn new(left: PathBuf, right: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            panes: [Pane::new(left)?, Pane::new(right)?],
            active: ActivePane::Left,
            should_quit: false,
            last_error: None,
        })
    }

    pub fn active_pane(&self) -> &Pane {
        &self.panes[self.active.index()]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active.index()]
    }

    /// Translates a normalized terminal event into an `Action` (via the
    /// hardcoded keymap below) and dispatches it. `Tick` carries no action.
    pub fn handle_event(&mut self, event: AppEvent) {
        if let AppEvent::Input(code, modifiers) = event
            && let Some(action) = action_for_input(code, modifiers)
        {
            self.dispatch(action);
        }
    }

    /// The single match hub every action flows through. Kept infallible on
    /// the outside (errors are recorded into `last_error` instead of
    /// propagated) so the input loop never has to think about failure.
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
            Action::Quit => {
                self.should_quit = true;
                Ok(())
            }
        };

        self.last_error = result.err().map(|err| err.to_string());
    }
}

/// Hardcoded dyna-filer-style default keymap. `keymap.rs` (Phase 2) will
/// replace this with a configurable resolver; every caller already only
/// depends on `Action`, never on raw key codes, so swapping it in later is
/// a one-file change.
fn action_for_input(code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char('c') => Some(Action::Quit),
            KeyCode::Char('r') => Some(Action::Refresh),
            _ => None,
        };
    }

    match code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Up => Some(Action::CursorUp),
        KeyCode::Down => Some(Action::CursorDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home => Some(Action::Top),
        KeyCode::End => Some(Action::Bottom),
        KeyCode::Tab => Some(Action::SwitchPane),
        KeyCode::Enter => Some(Action::Enter),
        KeyCode::Backspace => Some(Action::Parent),
        KeyCode::Char('s') => Some(Action::CycleSort),
        KeyCode::Char('.') => Some(Action::ToggleHidden),
        KeyCode::Char('u') => Some(Action::SwapPanes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_action_sets_should_quit() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().to_path_buf(), dir.path().to_path_buf()).unwrap();
        assert!(!app.should_quit);
        app.dispatch(Action::Quit);
        assert!(app.should_quit);
    }

    #[test]
    fn switch_pane_toggles_active() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(dir.path().to_path_buf(), dir.path().to_path_buf()).unwrap();
        assert_eq!(app.active, ActivePane::Left);
        app.dispatch(Action::SwitchPane);
        assert_eq!(app.active, ActivePane::Right);
    }

    #[test]
    fn swap_panes_swaps_cwd() {
        let left_dir = tempfile::tempdir().unwrap();
        let right_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            left_dir.path().to_path_buf(),
            right_dir.path().to_path_buf(),
        )
        .unwrap();
        app.dispatch(Action::SwapPanes);
        assert_eq!(app.panes[0].cwd, right_dir.path());
        assert_eq!(app.panes[1].cwd, left_dir.path());
    }

    #[test]
    fn q_and_ctrl_c_both_map_to_quit() {
        assert_eq!(
            action_for_input(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            action_for_input(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
    }
}
