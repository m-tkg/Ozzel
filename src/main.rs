use std::io::{self, Stdout, Write as _};
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use ozzel::app::App;
use ozzel::cli::{Cli, Command, resolve_startup_dir, should_write_cwd_file, write_cwd_file};
use ozzel::terminal::{
    MOUSE_CAPTURE_ACTIVE, TerminalGuard, install_panic_hook, sync_mouse_capture,
};
use ozzel::update::self_update;
use ozzel::{config, event, external, persist, ui};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Handled before anything else touches the filesystem/terminal: no
    // config load, no panic hook, no `TerminalGuard` — `update` is a
    // plain one-shot CLI action, not a TUI session.
    if let Some(Command::Update { force }) = cli.command {
        return self_update(force);
    }

    let left = resolve_startup_dir(cli.left_dir)?;
    let right = resolve_startup_dir(cli.right_dir)?;
    // Loaded (and, on a malformed file, rejected) before we ever touch the
    // terminal so a config typo prints a normal, readable error message.
    let config = config::load()?;

    let mouse = config.mouse;
    let quit_cd = config.quit_cd;

    install_panic_hook();
    let mut guard = TerminalGuard::new(mouse)?;
    // Panes start empty — `run` draws one frame before either directory
    // gets read (see `App::load_initial_dirs`'s doc comment), so a slow
    // mount or huge directory never leaves the alternate screen blank and
    // frozen right after startup.
    let mut app = App::new_unloaded(left, right, config)?;

    // History/bookmarks are loaded after the terminal is already up (unlike
    // config): a missing or corrupt file here is never fatal, just an
    // empty-defaults-plus-a-log-line situation, so there's no reason to
    // gate terminal startup on it the way a malformed config does.
    let (history, history_warning) = persist::load_history();
    let (bookmarks, bookmarks_warning) = persist::load_bookmarks();
    let (sort_prefs, sort_prefs_warning) = persist::load_sort_prefs();
    app.history = history;
    app.bookmarks = bookmarks;
    app.sort_prefs = sort_prefs;
    app.apply_startup_sort_prefs();
    // Opt this (real, non-test) `App` into filesystem watching, so panes
    // pick up changes made outside ozzel — see
    // `App::enable_directory_watching`, which honors `auto_refresh`.
    app.enable_directory_watching();
    if let Some(msg) = history_warning {
        app.log_error(msg);
    }
    if let Some(msg) = bookmarks_warning {
        app.log_error(msg);
    }
    if let Some(msg) = sort_prefs_warning {
        app.log_error(msg);
    }

    run(&mut guard.terminal, &mut app, guard.keyboard_enhancement)?;

    // Best-effort: the terminal is about to be restored by `guard`'s Drop
    // regardless, so a save failure here has nowhere good to be shown —
    // logged to stderr rather than silently dropped.
    if let Err(err) = persist::save_history(&app.history) {
        eprintln!("ozzel: failed to save history: {err}");
    }
    if let Err(err) = persist::save_bookmarks(&app.bookmarks) {
        eprintln!("ozzel: failed to save bookmarks: {err}");
    }
    if let Err(err) = persist::save_sort_prefs(&app.sort_prefs) {
        eprintln!("ozzel: failed to save sort prefs: {err}");
    }

    // `guard` (and with it, the terminal) is still alive here — writing the
    // cwd file after it's dropped isn't necessary (this doesn't touch the
    // terminal at all), but doing it before means a shell wrapper's `cd`
    // and the terminal restore are never racing each other for the last
    // word on screen.
    if let Some(path) = &cli.cwd_file
        && should_write_cwd_file(quit_cd, Some(path))
    {
        let cwd = app.active_pane().cwd.clone();
        if let Err(err) = write_cwd_file(path, &cwd) {
            eprintln!("ozzel: failed to write --cwd-file: {err}");
        }
    }

    Ok(())
}

/// Poll timeout used while at least one background task is running — kept
/// at the tight interval so a running copy/move/delete's status-bar gauge
/// keeps refreshing promptly. See `App::needs_redraw`'s doc comment for how
/// the loop below makes sure a redraw actually happens every iteration
/// while this applies, even on a poll that times out with nothing to
/// report.
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Poll timeout used the rest of the time (no task running) — long enough
/// that an idle session barely wakes the CPU, short enough to stay well
/// under human-noticeable. Safe to lengthen independently of input
/// latency: a keypress reaches `handle_event` the moment crossterm reports
/// it (`event::poll` returns as soon as *something* arrives, well before
/// `timeout` elapses) — this constant only bounds how long a poll blocks
/// with *nothing* to report.
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Draws one frame and applies whatever [`ui::LayoutFeedback`] it reports
/// back into `app` — the one call site both of `run`'s `terminal.draw`
/// calls (the pre-`load_initial_dirs` first frame, and the main loop's
/// dirty-flag-gated redraw) go through, so the two can never drift into
/// applying feedback differently. `terminal.draw`'s callback must return
/// `()` (see `ratatui`'s own signature), so the `LayoutFeedback` `ui::draw`
/// returns is threaded out through this `Option` rather than as the
/// closure's own return value.
fn draw_frame(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    let mut feedback = None;
    terminal.draw(|frame| feedback = Some(ui::draw(frame, &mut *app)))?;
    app.apply_layout_feedback(feedback.expect("the draw callback always runs exactly once"));
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    keyboard_enhancement: bool,
) -> anyhow::Result<()> {
    // Drawn once, unconditionally, before `load_initial_dirs`'s directory
    // reads ever run: `app` arrives with both panes empty (`main`'s
    // `App::new_unloaded`), so this is what actually puts something on
    // screen the instant the alternate screen opens, rather than leaving
    // it blank while a slow mount or huge directory is read. A real read
    // failure here (bad permissions etc.) surfaces as `run`'s own `Err`
    // exactly like a directory-read failure used to surface from `App::new`
    // itself, before this split existed.
    draw_frame(terminal, app)?;
    app.load_initial_dirs()?;

    loop {
        // Gated on the dirty flag rather than drawn unconditionally every
        // iteration (Phase 1 hot-path fix): `ui::draw` used to run on every
        // poll timeout too, even though `AppEvent::Tick` is a no-op and
        // nothing about the frame could possibly have changed. See
        // `App::needs_redraw`'s doc comment for exactly what sets/clears
        // this.
        if app.needs_redraw {
            draw_frame(terminal, app)?;
            app.needs_redraw = false;
        }

        // Drain background-task progress/log/finish events before the next
        // terminal poll, so a running copy/move/delete's gauge and log
        // lines update promptly instead of waiting behind a keystroke.
        app.drain_tasks();

        // Externally-made changes (Finder, another shell) land here, and
        // are applied straight away so the next redraw — at the top of the
        // very next iteration — already shows them.
        app.drain_fs_events();

        // A running task's gauge (elapsed time, done/total) needs to keep
        // refreshing every iteration even on an iteration where no new
        // `TaskEvent` happened to arrive — `drain_tasks`/`handle_event`
        // alone only mark a redraw dirty when an event was actually
        // processed, which isn't guaranteed every single iteration while a
        // task is merely running in the background.
        let task_running = !app.tasks.running.is_empty();
        if task_running {
            app.needs_redraw = true;
        }

        let poll_interval = if task_running {
            ACTIVE_POLL_INTERVAL
        } else {
            IDLE_POLL_INTERVAL
        };
        let event = event::read_event(poll_interval)?;
        app.handle_event(event);

        // Every side-channel output `app.handle_event` above might have
        // queued — see `app::Outbox`'s doc comment — drained here in one
        // call rather than polled field by field.
        let outbox = app.take_outbox();

        // `:` and `e` queue a suspend request rather than running the
        // child process inline, since only `main.rs` holds the `Terminal`
        // handle `external::run_suspended` needs to leave/re-enter the
        // alternate screen around it. `MOUSE_CAPTURE_ACTIVE` is read fresh
        // here (rather than a value frozen at startup) since it can now
        // change dynamically — but a suspend can only ever be queued from
        // `Normal` mode, where capture always matches `config.mouse`
        // anyway, so this is really just "don't assume it's always on".
        if let Some(req) = outbox.external {
            let mouse_was_active = MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst);
            match external::run_suspended(terminal, &req, keyboard_enhancement, mouse_was_active) {
                Ok(Some(spawn_error)) => app.log_error(spawn_error),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
            app.drain_tasks();
            app.refresh_panes();
            // The terminal was just handed back from the suspended child
            // (alternate screen re-entered, raw mode restored) — the frame
            // drawn before the suspend is stale regardless of whether any
            // `App` state above actually changed, so this must force a
            // redraw unconditionally.
            app.needs_redraw = true;

            // `,` (edit_config) queues this alongside the suspend request
            // itself; reload only after the editor has actually exited, so
            // the new config takes effect immediately without a restart.
            if outbox.config_reload {
                app.reload_config();
            }
        }

        // `y` (copy_path) queues the OSC 52 escape rather than writing it
        // itself, same reasoning as `external`: only `main.rs` holds a raw
        // handle to stdout it's safe to interleave writes with (the
        // alternate-screen/raw-mode dance elsewhere all goes through here
        // too). Best-effort — a write failure has nowhere useful to be
        // surfaced beyond stderr, and must never crash the session over a
        // clipboard copy.
        if let Some(text) = outbox.clipboard {
            let seq = external::osc52_copy_sequence(&text);
            let mut stdout = io::stdout();
            let _ = stdout.write_all(seq.as_bytes());
            let _ = stdout.flush();
        }

        if outbox.bookmarks_dirty
            && let Err(err) = persist::save_bookmarks(&app.bookmarks)
        {
            app.log_error(format!("failed to save bookmarks: {err}"));
        }

        // One choke point for every mode transition that might change
        // whether mouse capture should be on (entering/leaving Viewer/
        // Log/Help, a `,` config reload flipping `mouse`, ...): checked
        // every iteration rather than at each individual transition site,
        // and `sync_mouse_capture` itself no-ops when nothing changed.
        sync_mouse_capture(app.wants_mouse_capture())?;

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
