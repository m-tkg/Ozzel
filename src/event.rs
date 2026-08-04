//! Normalizes the raw terminal event stream into [`AppEvent`]. This is the
//! single chokepoint for turning crossterm events into something the rest
//! of the app understands.

use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEvent, KeyEventKind};
pub use ratatui::crossterm::event::{KeyCode, KeyModifiers};

/// Events fed into `App::handle_event`. `Task` (background job progress)
/// arrives in Phase 3 once `tasks/` exists; for now only keyboard input and
/// an idle tick exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Input(KeyCode, KeyModifiers),
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
