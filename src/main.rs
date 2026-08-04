use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

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

/// Single chokepoint for turning a raw crossterm `Event` into a normalized
/// `(KeyCode, KeyModifiers)` pair. Filters out everything but `Press` events
/// so that Windows' extra Release/Repeat events never double-handle a key.
fn normalize_key(event: &Event) -> Option<(KeyCode, KeyModifiers)> {
    match event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) => Some((*code, *modifiers)),
        _ => None,
    }
}

fn pane_title(dir: &Option<PathBuf>) -> String {
    dir.as_deref()
        .map(Path::display)
        .map(|d| d.to_string())
        .unwrap_or_else(|| ".".to_string())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    install_panic_hook();
    let mut guard = TerminalGuard::new()?;
    run(&mut guard.terminal, &cli.left_dir, &cli.right_dir)?;

    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    left: &Option<PathBuf>,
    right: &Option<PathBuf>,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, left, right))?;

        if event::poll(Duration::from_millis(50))? {
            let event = event::read()?;
            if let Some((code, modifiers)) = normalize_key(&event) {
                match code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn draw(frame: &mut ratatui::Frame, left: &Option<PathBuf>, right: &Option<PathBuf>) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);

    frame.render_widget(
        Block::default()
            .title(pane_title(left))
            .borders(Borders::ALL),
        panes[0],
    );
    frame.render_widget(
        Block::default()
            .title(pane_title(right))
            .borders(Borders::ALL),
        panes[1],
    );
    frame.render_widget(Block::default().title("Log").borders(Borders::ALL), rows[1]);

    let status = Paragraph::new(" q: quit  Ctrl+C: quit")
        .style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_widget(status, rows[2]);
}
