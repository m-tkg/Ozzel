//! Normalizes the raw terminal event stream into [`AppEvent`]. This is the
//! single chokepoint for turning crossterm events into something the rest
//! of the app understands.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};
pub use ratatui::crossterm::event::{
    KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::tasks::TaskId;

/// A message from a background task's worker thread back to the main
/// thread, sent over the `mpsc::Sender<TaskEvent>` every `TaskManager::spawn`
/// closure receives a clone of.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    Progress {
        id: TaskId,
        done: u64,
        total: u64,
        detail: String,
    },
    Log {
        id: TaskId,
        line: String,
    },
    Finished {
        id: TaskId,
        result: Result<String, String>,
    },
}

/// Events fed into `App::handle_event`. `Task` events are drained from the
/// `TaskManager`'s channel (see `App::drain_tasks`) ahead of every terminal
/// poll; `Tick` is what a poll timeout with no input produces.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Input(KeyCode, KeyModifiers),
    /// Forwarded only when mouse capture is enabled (`config.mouse`, see
    /// `main.rs`'s `TerminalGuard`) — with capture off, crossterm never
    /// reports mouse events in the first place, so this variant simply
    /// never occurs. Handled by `App::handle_mouse`.
    Mouse(MouseEvent),
    Task(TaskEvent),
    /// The terminal was resized. Carries no data — `ui::draw` always reads
    /// the current size fresh from the terminal on the next actual draw
    /// (see `Terminal::draw`'s own autoresize); this variant exists purely
    /// so `App::handle_event`'s dirty-flag tracking (`needs_redraw`) can
    /// tell a resize apart from a `Tick` and force that next draw to
    /// happen, rather than the new size sitting unrendered until some
    /// unrelated input arrives.
    Resize,
    Tick,
}

/// Polls the terminal for up to `timeout` and returns a normalized
/// `AppEvent`. Only `KeyEventKind::Press` is ever forwarded as `Input` —
/// Windows also emits Release/Repeat key events, which would otherwise
/// double-handle every keystroke — mouse events forward as `Mouse`
/// (only ever produced when mouse capture is on), a terminal resize
/// forwards as `Resize`, and anything else (focus, paste) collapses to
/// `Tick`.
pub fn read_event(timeout: Duration) -> Result<AppEvent> {
    if event::poll(timeout)? {
        match event::read()? {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                ..
            }) => return Ok(AppEvent::Input(code, modifiers)),
            Event::Mouse(mouse_event) => return Ok(AppEvent::Mouse(mouse_event)),
            Event::Resize(_, _) => return Ok(AppEvent::Resize),
            _ => {}
        }
    }
    Ok(AppEvent::Tick)
}
