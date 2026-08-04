//! Normalizes the raw terminal event stream into [`AppEvent`]. This is the
//! single chokepoint for turning crossterm events into something the rest
//! of the app understands.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};
pub use ratatui::crossterm::event::{KeyCode, KeyModifiers};

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
    Task(TaskEvent),
    Tick,
}

/// Polls the terminal for up to `timeout` and returns a normalized
/// `AppEvent`. Only `KeyEventKind::Press` is ever forwarded as `Input` —
/// Windows also emits Release/Repeat key events, which would otherwise
/// double-handle every keystroke — and anything that is not a key press
/// (mouse, resize, focus, paste) collapses to `Tick`.
pub fn read_event(timeout: Duration) -> Result<AppEvent> {
    if event::poll(timeout)?
        && let Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) = event::read()?
    {
        return Ok(AppEvent::Input(code, modifiers));
    }
    Ok(AppEvent::Tick)
}
