mod action;
mod app;
mod color;
mod config;
mod entry;
mod event;
mod external;
mod filter;
mod function_list;
mod help;
mod keymap;
mod mode;
mod ops;
mod pane;
mod persist;
mod tasks;
mod ui;
mod viewer;
mod virtual_dir;

use std::io::{self, Stdout, Write as _};
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};

use app::App;

/// Whether the kitty keyboard-enhancement flags are currently pushed onto
/// the terminal. Shared (rather than a field on `TerminalGuard`) because
/// the panic hook is installed *before* the guard exists and runs entirely
/// outside its scope, so it has no other way to know whether a pop is
/// needed before restoring the terminal.
static KEYBOARD_ENHANCEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Same story as `KEYBOARD_ENHANCEMENT_ACTIVE`, for mouse capture
/// (`config.mouse`, default on): the panic hook needs to know whether to
/// disable it before restoring the terminal, and it runs outside
/// `TerminalGuard`'s own scope. Unlike the keyboard-enhancement flag
/// (fixed for the whole session), this one changes *dynamically* at
/// runtime — see `sync_mouse_capture` — so it doubles as the "what's
/// actually active right now" source of truth for anything that needs to
/// know (the panic hook, `Drop`, and `run_suspended`'s own push/pop
/// symmetry around a suspended child process).
static MOUSE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The single choke point for enabling/disabling mouse capture on the
/// real terminal at runtime: compares `wanted` against the shared
/// `MOUSE_CAPTURE_ACTIVE` flag and only actually writes the enable/
/// disable escape when they differ (see `mouse_capture_needs_sync`),
/// then keeps the flag itself in sync — which is what lets the panic
/// hook/`Drop` (and `run_suspended`'s suspend/resume symmetry) always
/// trust it to reflect the terminal's real current state, never a stale
/// "as configured at startup" value. Called once per main-loop iteration
/// with `App::wants_mouse_capture()` — that single call site structurally
/// covers every mode transition (entering/leaving Viewer/Log/Help, a
/// config reload flipping `mouse`, etc.) without needing one scattered
/// through every place a transition could happen.
fn sync_mouse_capture(wanted: bool) -> io::Result<()> {
    let active = MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst);
    if !mouse_capture_needs_sync(active, wanted) {
        return Ok(());
    }
    if wanted {
        execute!(io::stdout(), EnableMouseCapture)?;
    } else {
        execute!(io::stdout(), DisableMouseCapture)?;
    }
    MOUSE_CAPTURE_ACTIVE.store(wanted, Ordering::SeqCst);
    Ok(())
}

/// The pure decision half of `sync_mouse_capture`, factored out so the
/// "avoid redundant writes" guard is unit-testable without a real
/// terminal.
fn mouse_capture_needs_sync(active: bool, wanted: bool) -> bool {
    active != wanted
}

/// ozzel: a two-pane TUI file manager.
#[derive(Parser, Debug)]
#[command(name = "ozzel", version, about = "Two-pane TUI file manager")]
struct Cli {
    /// Starting directory for the left pane (defaults to the current directory)
    left_dir: Option<PathBuf>,
    /// Starting directory for the right pane (defaults to the current directory)
    right_dir: Option<PathBuf>,
    /// Path to write the focused pane's directory to on quit (see
    /// `write_cwd_file`) — pair with a shell wrapper function that `cd`s
    /// into it afterward (README documents one for zsh/bash and
    /// PowerShell). Gated by `config.quit_cd` (default on): when this flag
    /// is absent, nothing is ever written, regardless of that setting.
    #[arg(long)]
    cwd_file: Option<PathBuf>,
}

/// Restores the terminal (raw mode + alternate screen, plus the kitty
/// keyboard-enhancement flags if they were pushed) when dropped, so that
/// every early-return path (including panics) leaves the shell usable.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Whether this terminal answered the kitty keyboard-protocol query
    /// (see `supports_keyboard_enhancement`'s docs). When `true`,
    /// `S-enter` is reported distinctly from plain `Enter`; when `false`,
    /// every keypress this session sees `S-enter` as arrives as plain
    /// `Enter` instead, since there is no way to disambiguate them.
    keyboard_enhancement: bool,
}

impl TerminalGuard {
    /// `mouse` mirrors `config.mouse` (default on): when `true`, mouse
    /// capture is enabled right alongside the kitty keyboard-enhancement
    /// flags, and `MOUSE_CAPTURE_ACTIVE` records it for `Drop`/the panic
    /// hook the same way `KEYBOARD_ENHANCEMENT_ACTIVE` already does. `false`
    /// never enables it at all, leaving the terminal's native text
    /// selection usable for the whole session (see the README's mouse
    /// section for the Shift-to-select trade-off when it *is* on).
    fn new(mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        // Queried right after raw mode is up, and only once per run: this
        // makes a real terminal round-trip (it writes an escape query and
        // reads the response), so re-querying per-keystroke would add
        // needless latency for a value that can't change mid-session.
        let keyboard_enhancement = supports_keyboard_enhancement().unwrap_or(false);
        let mut stdout = io::stdout();
        if keyboard_enhancement {
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            KEYBOARD_ENHANCEMENT_ACTIVE.store(true, Ordering::SeqCst);
        }
        sync_mouse_capture(mouse)?;
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            keyboard_enhancement,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        if KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let _ = disable_raw_mode();
    }
}

/// Installs a panic hook that restores the terminal *before* the default
/// hook prints the panic message, otherwise the message would be swallowed
/// by the alternate screen or mangled by raw mode. Installed before
/// `TerminalGuard` exists, so it can't borrow the guard's fields — it
/// consults the shared `KEYBOARD_ENHANCEMENT_ACTIVE`/`MOUSE_CAPTURE_ACTIVE`
/// flags instead.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        if KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let _ = disable_raw_mode();
        default_hook(info);
    }));
}

/// Resolves a startup directory argument, falling back to the current
/// working directory when omitted, and failing loudly when an explicitly
/// requested directory does not exist.
fn resolve_startup_dir(dir: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match dir {
        Some(path) => {
            if !path.exists() {
                bail!("directory does not exist: {}", path.display());
            }
            if !path.is_dir() {
                bail!("not a directory: {}", path.display());
            }
            Ok(path)
        }
        None => std::env::current_dir().context("failed to determine current directory"),
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let left = resolve_startup_dir(cli.left_dir)?;
    let right = resolve_startup_dir(cli.right_dir)?;
    // Loaded (and, on a malformed file, rejected) before we ever touch the
    // terminal so a config typo prints a normal, readable error message.
    let config = config::load()?;

    let mouse = config.mouse;
    let quit_cd = config.quit_cd;

    install_panic_hook();
    let mut guard = TerminalGuard::new(mouse)?;
    let mut app = App::new(left, right, config)?;

    // History/bookmarks are loaded after the terminal is already up (unlike
    // config): a missing or corrupt file here is never fatal, just an
    // empty-defaults-plus-a-log-line situation, so there's no reason to
    // gate terminal startup on it the way a malformed config does.
    let (history, history_warning) = persist::load_history();
    let (bookmarks, bookmarks_warning) = persist::load_bookmarks();
    app.history = history;
    app.bookmarks = bookmarks;
    if let Some(msg) = history_warning {
        app.log_error(msg);
    }
    if let Some(msg) = bookmarks_warning {
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

/// Whether `main` should write `--cwd-file` at all: only when both the flag
/// was given (`cwd_file.is_some()`) *and* `quit_cd` didn't opt out. Kept as
/// a pure, standalone predicate (rather than inlined into the `if` at the
/// call site) purely so it's directly unit-testable without going through
/// `Cli::parse`/a real filesystem write.
fn should_write_cwd_file(quit_cd: bool, cwd_file: Option<&PathBuf>) -> bool {
    quit_cd && cwd_file.is_some()
}

/// Writes `cwd`'s path (as `Path::display` would render it) to `path`,
/// overwriting any existing content — the shell wrapper function
/// (README's `oz()`) reads this back and `cd`s into it. A plain
/// `std::fs::write`, no atomic-rename dance: this is a single small write
/// to a fresh `mktemp` file the wrapper itself owns for the run's
/// lifetime, not a file anything else could be concurrently reading.
fn write_cwd_file(path: &Path, cwd: &Path) -> io::Result<()> {
    std::fs::write(path, cwd.display().to_string())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    keyboard_enhancement: bool,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, &mut *app))?;

        // Drain background-task progress/log/finish events before the next
        // terminal poll, so a running copy/move/delete's gauge and log
        // lines update promptly instead of waiting behind a keystroke.
        app.drain_tasks();

        let event = event::read_event(Duration::from_millis(50))?;
        app.handle_event(event);

        // `:` and `e` queue a suspend request rather than running the
        // child process inline, since only `main.rs` holds the `Terminal`
        // handle `external::run_suspended` needs to leave/re-enter the
        // alternate screen around it. `MOUSE_CAPTURE_ACTIVE` is read fresh
        // here (rather than a value frozen at startup) since it can now
        // change dynamically — but a suspend can only ever be queued from
        // `Normal` mode, where capture always matches `config.mouse`
        // anyway, so this is really just "don't assume it's always on".
        if let Some(req) = app.pending_external.take() {
            let mouse_was_active = MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst);
            match external::run_suspended(terminal, &req, keyboard_enhancement, mouse_was_active) {
                Ok(Some(spawn_error)) => app.log_error(spawn_error),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
            app.drain_tasks();
            app.refresh_panes();

            // `,` (edit_config) queues this alongside the suspend request
            // itself; reload only after the editor has actually exited, so
            // the new config takes effect immediately without a restart.
            if app.pending_config_reload {
                app.pending_config_reload = false;
                app.reload_config();
            }
        }

        // `y` (copy_path) queues the OSC 52 escape rather than writing it
        // itself, same reasoning as `pending_external`: only `main.rs`
        // holds a raw handle to stdout it's safe to interleave writes with
        // (the alternate-screen/raw-mode dance elsewhere all goes through
        // here too). Best-effort — a write failure has nowhere useful to
        // be surfaced beyond stderr, and must never crash the session over
        // a clipboard copy.
        if let Some(text) = app.pending_clipboard.take() {
            let seq = external::osc52_copy_sequence(&text);
            let mut stdout = io::stdout();
            let _ = stdout.write_all(seq.as_bytes());
            let _ = stdout.flush();
        }

        if app.bookmarks_dirty {
            if let Err(err) = persist::save_bookmarks(&app.bookmarks) {
                app.log_error(format!("failed to save bookmarks: {err}"));
            }
            app.bookmarks_dirty = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_write_cwd_file_requires_both_the_flag_and_quit_cd() {
        let path = PathBuf::from("/tmp/whatever");
        assert!(should_write_cwd_file(true, Some(&path)));
        assert!(!should_write_cwd_file(false, Some(&path)));
        assert!(!should_write_cwd_file(true, None));
        assert!(!should_write_cwd_file(false, None));
    }

    #[test]
    fn mouse_capture_needs_sync_only_when_the_states_differ() {
        assert!(!mouse_capture_needs_sync(true, true));
        assert!(!mouse_capture_needs_sync(false, false));
        assert!(mouse_capture_needs_sync(true, false));
        assert!(mouse_capture_needs_sync(false, true));
    }

    #[test]
    fn write_cwd_file_writes_the_display_form_of_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cwd");
        let cwd = dir.path().join("some/nested/dir");
        write_cwd_file(&out, &cwd).unwrap();
        let contents = std::fs::read_to_string(&out).unwrap();
        assert_eq!(contents, cwd.display().to_string());
    }

    #[test]
    fn write_cwd_file_overwrites_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cwd");
        std::fs::write(&out, "stale").unwrap();
        write_cwd_file(&out, Path::new("/fresh")).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "/fresh");
    }
}
