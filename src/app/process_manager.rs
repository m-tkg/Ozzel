//! `App`'s half of the process manager (`S-p`): opening and closing the
//! view, the periodic `ps` probe, applying snapshots, sorting, and sending
//! signals. The parsing/sorting logic it drives lives in `crate::process`
//! and the drawing in `crate::ui::process_view`.
//!
//! Split out of `app/mod.rs` for the same reason as `pager` and
//! `settings_ui`: one screen's worth of `impl App` that nothing else calls
//! into.

use std::sync::atomic::Ordering;
use std::time::Instant;

use chrono::Local;

use crate::event::{KeyCode, KeyModifiers};
use crate::keymap::MenuNav;
use crate::mode::{Mode, PendingKill};
use crate::pane::PAGE_SIZE;
use crate::process::{self, ProcessInfo, ProcessSortKey, Signal};
use crate::tasks::process_list;

use super::App;

/// Which sort key a letter selects inside the view. Chosen to spell the
/// column they order (`c`pu, `m`em, `p`id, `u`ser, `n`ame, `t`ime, `s`ize)
/// while avoiding `i`/`k`, which the default keymap uses for cursor
/// movement.
fn sort_key_for(code: KeyCode) -> Option<ProcessSortKey> {
    match code {
        KeyCode::Char('p') => Some(ProcessSortKey::Pid),
        KeyCode::Char('u') => Some(ProcessSortKey::User),
        KeyCode::Char('c') => Some(ProcessSortKey::Cpu),
        KeyCode::Char('m') => Some(ProcessSortKey::Mem),
        KeyCode::Char('s') => Some(ProcessSortKey::Rss),
        KeyCode::Char('t') => Some(ProcessSortKey::Time),
        KeyCode::Char('n') => Some(ProcessSortKey::Name),
        _ => None,
    }
}

/// One cursor movement, resolved from a keypress before anything touches
/// the list (which the borrow checker appreciates, since resolving it needs
/// `self.keymap` and applying it needs `&mut self.mode`).
enum CursorMove {
    Rel(isize),
    Top,
    Bottom,
}

impl App {
    /// `S-p`: opens the process manager and fires the first probe
    /// immediately, so the list is there rather than a beat behind.
    ///
    /// Unix only. `ps` exists on macOS and Linux with a column syntax
    /// `crate::process` can rely on, and `libc::kill` is how a process gets
    /// signalled; Windows would need `tasklist`/`taskkill` and a second
    /// parser, so it logs and stays put — the same call this makes for
    /// `chmod`.
    pub(super) fn begin_process_manager(&mut self) {
        #[cfg(not(unix))]
        {
            self.log_error("the process manager is not supported on this platform");
        }
        #[cfg(unix)]
        {
            use crate::mode::ProcessManagerState;

            // Busiest-first is the question this screen usually exists to
            // answer.
            let sort_key = ProcessSortKey::Cpu;
            self.mode = Mode::ProcessManager {
                state: Box::new(ProcessManagerState {
                    processes: Vec::new(),
                    sort_key,
                    ascending: process::default_ascending(sort_key),
                    cursor: 0,
                    loading: true,
                    error: None,
                    updated_at: None,
                    pending_kill: None,
                }),
            };
            self.spawn_process_probe();
        }
    }

    /// Closes the view, aborting whatever probe is in flight — otherwise a
    /// `ps` started a moment ago would keep running (and its thread keep a
    /// channel sender alive) for a screen nobody is looking at.
    fn close_process_manager(&mut self) {
        self.cancel_process_probe();
        self.mode = Mode::Normal;
    }

    fn cancel_process_probe(&mut self) {
        if let Some((_, cancel)) = self.latest_process_probe.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Spawns one `ps` probe, superseding any still in flight. The single
    /// entry point for both the periodic refresh and the explicit ones
    /// (`r`, and the one right after a successful kill).
    ///
    /// Detached (`TaskManager::spawn_detached`) rather than tracked: a
    /// tracked task would put "1 running" in the status bar, drop the event
    /// loop to its 50ms active poll interval, make `C-k` cancel the refresh,
    /// and ask "tasks are running, really quit?" on the way out — all of it
    /// wrong for a probe the user never started and can't see.
    ///
    /// Background rather than synchronous, unlike `build_file_info`'s
    /// one-shot I/O at open time: this runs again every couple of seconds,
    /// and `ps` on a busy machine takes tens of milliseconds. Freezing the
    /// event loop for that on a timer is exactly the kind of stutter a held-
    /// down cursor key would land in.
    pub(super) fn spawn_process_probe(&mut self) {
        self.cancel_process_probe();
        let (id, cancel) = self.tasks.spawn_detached(process_list::run_process_list);
        self.pending_process_list.insert(id);
        self.latest_process_probe = Some((id, cancel));
        self.process_probed_at = Some(Instant::now());
    }

    /// The single decision point for re-probing, called after every event
    /// (`Tick`s included — nothing else would drive a refresh while the user
    /// isn't typing). Cheap to call constantly: the first `matches!` returns
    /// immediately whenever the view is closed, which is almost always.
    pub(super) fn maybe_refresh_processes(&mut self) {
        if !matches!(self.mode, Mode::ProcessManager { .. }) {
            return;
        }
        if !self.config.process_auto_refresh {
            return;
        }
        // One probe at a time. On a machine where `ps` takes longer than the
        // interval, spawning regardless would stack threads faster than they
        // finish.
        if self.latest_process_probe.is_some() {
            return;
        }
        let due = self
            .process_probed_at
            .is_none_or(|at| at.elapsed() >= process::PROCESS_REFRESH_INTERVAL);
        if due {
            self.spawn_process_probe();
        }
    }

    /// Installs a fresh snapshot (or records why there isn't one). Ignored
    /// when the view has since closed — a probe cancelled on the way out can
    /// still have a result in the channel.
    pub(super) fn apply_process_snapshot(&mut self, result: Result<Vec<ProcessInfo>, String>) {
        // Deferred out of the borrow below: `state` holds `&mut self.mode`,
        // and logging needs `&mut self` again.
        let newly_failed;
        {
            let Mode::ProcessManager { state } = &mut self.mode else {
                return;
            };
            state.loading = false;
            match result {
                Ok(mut procs) => {
                    state.error = None;
                    process::sort_processes(&mut procs, state.sort_key, state.ascending);
                    let anchor = state.processes.get(state.cursor).map(|p| p.pid);
                    state.cursor = process::reanchor_cursor(&procs, anchor, state.cursor);
                    state.processes = procs;
                    state.updated_at = Some(Local::now());
                    newly_failed = None;
                }
                Err(msg) => {
                    // The footer shows this every frame; the log only wants
                    // it when it's news. At one probe every two seconds, a
                    // line per failure would be thirty a minute — enough to
                    // push everything else out of a 500-line buffer in under
                    // twenty.
                    newly_failed =
                        (state.error.as_deref() != Some(msg.as_str())).then(|| msg.clone());
                    state.error = Some(msg);
                }
            }
        }
        if let Some(msg) = newly_failed {
            self.log_error(format!("process list: {msg}"));
        }
    }

    /// Fixed keys for `Mode::ProcessManager`.
    ///
    /// The view's own keys are matched *before* falling back to
    /// `Keymap::menu_nav`, the order every modal uses (see
    /// `handle_select_key`), so no rebind can shadow the sort or signal
    /// keys. The flip side — a user who has moved `cursor_up` onto `c` gets
    /// the `%CPU` sort there instead — is documented in the help screen's
    /// fixed-key section.
    pub(super) fn handle_process_manager_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if matches!(&self.mode, Mode::ProcessManager { state } if state.pending_kill.is_some()) {
            self.handle_process_kill_confirm_key(code);
            return;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.close_process_manager();
                return;
            }
            KeyCode::Char('r') => {
                // Explicit, so it works even with auto-refresh switched off.
                self.spawn_process_probe();
                return;
            }
            KeyCode::Char('x') => {
                self.begin_process_kill(Signal::Term);
                return;
            }
            KeyCode::Char('X') => {
                self.begin_process_kill(Signal::Kill);
                return;
            }
            _ => {}
        }

        if let Some(key) = sort_key_for(code) {
            self.apply_process_sort(key);
            return;
        }

        let movement = match code {
            KeyCode::Up => Some(CursorMove::Rel(-1)),
            KeyCode::Down => Some(CursorMove::Rel(1)),
            KeyCode::PageUp => Some(CursorMove::Rel(-(PAGE_SIZE as isize))),
            KeyCode::PageDown => Some(CursorMove::Rel(PAGE_SIZE as isize)),
            KeyCode::Home | KeyCode::Char('g') => Some(CursorMove::Top),
            KeyCode::End | KeyCode::Char('G') => Some(CursorMove::Bottom),
            _ => match self.keymap.menu_nav(code, modifiers) {
                Some(MenuNav::Up) => Some(CursorMove::Rel(-1)),
                Some(MenuNav::Down) => Some(CursorMove::Rel(1)),
                None => None,
            },
        };
        if let Some(movement) = movement {
            self.move_process_cursor(movement);
        }
    }

    fn move_process_cursor(&mut self, movement: CursorMove) {
        let Mode::ProcessManager { state } = &mut self.mode else {
            return;
        };
        let Some(last) = state.processes.len().checked_sub(1) else {
            return;
        };
        state.cursor = match movement {
            // Clamping (rather than wrapping) at both ends: this is a long
            // list, and wrapping off the top of a thousand rows is never
            // what was meant.
            CursorMove::Rel(delta) => {
                (state.cursor as isize + delta).clamp(0, last as isize) as usize
            }
            CursorMove::Top => 0,
            CursorMove::Bottom => last,
        };
    }

    /// Re-sorts on a sort key press: the same key again flips the direction,
    /// a different one starts at that column's natural direction
    /// (`process::default_ascending`). The cursor stays on its process
    /// rather than its row number.
    fn apply_process_sort(&mut self, key: ProcessSortKey) {
        let Mode::ProcessManager { state } = &mut self.mode else {
            return;
        };
        state.ascending = if state.sort_key == key {
            !state.ascending
        } else {
            process::default_ascending(key)
        };
        state.sort_key = key;
        let anchor = state.processes.get(state.cursor).map(|p| p.pid);
        process::sort_processes(&mut state.processes, key, state.ascending);
        state.cursor = process::reanchor_cursor(&state.processes, anchor, state.cursor);
    }

    /// Asks before signalling the process under the cursor. The target is
    /// captured *now*, so a refresh landing while the question is up can't
    /// redirect the signal at whatever process slid into that row.
    fn begin_process_kill(&mut self, signal: Signal) {
        let Mode::ProcessManager { state } = &mut self.mode else {
            return;
        };
        let Some(target) = state.processes.get(state.cursor) else {
            return;
        };
        state.pending_kill = Some(PendingKill {
            pid: target.pid,
            name: target.name.clone(),
            signal,
        });
    }

    /// `y`/`n`/Esc while a kill confirmation is up. Everything else is
    /// ignored rather than falling through to the list's keys — the same
    /// rule `handle_confirm_key` follows, so a stray keypress can't answer
    /// the question by accident.
    fn handle_process_kill_confirm_key(&mut self, code: KeyCode) {
        let confirmed = {
            let Mode::ProcessManager { state } = &mut self.mode else {
                return;
            };
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => state.pending_kill.take(),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    state.pending_kill = None;
                    None
                }
                _ => None,
            }
        };
        if let Some(kill) = confirmed {
            self.execute_kill(kill);
        }
    }

    fn execute_kill(&mut self, kill: PendingKill) {
        // Killing ozzel from inside ozzel leaves the terminal in raw mode on
        // the alternate screen: `TerminalGuard`'s `Drop` doesn't run on a
        // signal, and neither does the panic hook. The user is left typing
        // blind into a shell that looks like a file manager.
        if kill.pid == std::process::id() {
            self.log_error("refusing to kill ozzel itself");
            return;
        }
        // `kill(2)` reads 0 as "my whole process group" — the shell that
        // launched ozzel and everything in it — and 1 as init.
        // `process::send_signal` refuses both as well; this is the layer
        // that can say so in the log.
        if kill.pid <= 1 {
            self.log_error(format!("refusing to signal pid {}", kill.pid));
            return;
        }

        match process::send_signal(kill.pid, kill.signal) {
            Ok(()) => {
                self.log_info(format!(
                    "sent {} to {} (pid {})",
                    kill.signal.label(),
                    kill.name,
                    kill.pid
                ));
                // Refresh now rather than at the next tick, so the row goes
                // away while it's still obvious why.
                self.spawn_process_probe();
            }
            // EPERM (someone else's process) and ESRCH (it exited between
            // the snapshot and the keypress) are ordinary outcomes here, not
            // failures worth unwinding — the log is where they belong.
            Err(err) => self.log_error(format!("{err:#}")),
        }
    }
}
