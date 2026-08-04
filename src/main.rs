mod action;
mod app;
mod config;
mod entry;
mod event;
mod external;
mod filter;
mod keymap;
mod mode;
mod ops;
mod pane;
mod persist;
mod tasks;
mod ui;

use std::io::{self, Stdout};
use std::panic;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use app::App;

/// ozzel: a dyna-filer-style two-pane TUI file manager.
#[derive(Parser, Debug)]
#[command(name = "ozzel", version, about = "Two-pane TUI file manager")]
struct Cli {
    /// Starting directory for the left pane (defaults to the current directory)
    left_dir: Option<PathBuf>,
    /// Starting directory for the right pane (defaults to the current directory)
    right_dir: Option<PathBuf>,
}

/// Restores the terminal (raw mode + alternate screen) when dropped, so that
/// every early-return path (including panics) leaves the shell usable.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Installs a panic hook that restores the terminal *before* the default
/// hook prints the panic message, otherwise the message would be swallowed
/// by the alternate screen or mangled by raw mode.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
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

    install_panic_hook();
    let mut guard = TerminalGuard::new()?;
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

    run(&mut guard.terminal, &mut app)?;

    // Best-effort: the terminal is about to be restored by `guard`'s Drop
    // regardless, so a save failure here has nowhere good to be shown —
    // logged to stderr rather than silently dropped.
    if let Err(err) = persist::save_history(&app.history) {
        eprintln!("ozzel: failed to save history: {err}");
    }
    if let Err(err) = persist::save_bookmarks(&app.bookmarks) {
        eprintln!("ozzel: failed to save bookmarks: {err}");
    }

    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Drain background-task progress/log/finish events before the next
        // terminal poll, so a running copy/move/delete's gauge and log
        // lines update promptly instead of waiting behind a keystroke.
        app.drain_tasks();

        let event = event::read_event(Duration::from_millis(50))?;
        app.handle_event(event);

        // `:` and `e` queue a suspend request rather than running the
        // child process inline, since only `main.rs` holds the `Terminal`
        // handle `external::run_suspended` needs to leave/re-enter the
        // alternate screen around it.
        if let Some(req) = app.pending_external.take() {
            match external::run_suspended(terminal, &req) {
                Ok(Some(spawn_error)) => app.log_error(spawn_error),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
            app.drain_tasks();
            app.refresh_panes();
        }

        if app.bookmarks_dirty {
            if let Err(err) = persist::save_bookmarks(&app.bookmarks) {
                app.log_error(format!("failed to save bookmarks: {err}"));
            }
            app.bookmarks_dirty = false;
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
