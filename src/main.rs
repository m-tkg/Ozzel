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
mod search;
mod settings;
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
use clap::{Parser, Subcommand};
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

/// Repository URL used by both `cargo install --git` (the actual update
/// mechanism) and the raw-content fetch that checks the remote version
/// first — see `self_update`.
const REPO_URL: &str = "https://github.com/m-tkg/Ozzel";

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

// `command` is a bare `Option<Subcommand>` living alongside the positional
// `left_dir`/`right_dir` args (rather than the more common "everything is
// a subcommand" shape) so that the plain, no-subcommand invocation —
// `ozzel`, `ozzel <left> <right>`, `ozzel --cwd-file f` — keeps working
// exactly as before; clap resolves the ambiguity between a subcommand name
// and a same-named positional value by matching the subcommand first (see
// `Command`'s comment for what that means for a directory literally named
// `update`). A plain (non-doc) comment on purpose: a doc comment here
// would leak this implementation rationale into `ozzel --help`'s output.
#[derive(Parser, Debug)]
#[command(
    name = "ozzel",
    version,
    about = "Two-pane TUI file manager",
    after_help = "\
`ozzel update` takes priority over a directory literally named `update`
in the current directory — pass `./update` (or an absolute path) to open
such a directory instead."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
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

// A `command` token consumes what would otherwise be `left_dir`'s
// positional slot — see `Cli`'s comment above — which is why `ozzel
// update` always means "run the update subcommand," never "open a
// directory named `update`" (use `./update` for that). Plain comment for
// the same --help-leakage reason as `Cli`'s.
#[derive(Subcommand, Debug)]
enum Command {
    /// Update ozzel to the latest version on GitHub (`cargo install --git`)
    #[command(after_help = "\
GitHub の main ブランチの版と比較し、新しければ
`cargo install --git https://github.com/m-tkg/Ozzel --force` を実行する。
ビルドに1〜2分かかる。cargo が必要。")]
    Update {
        /// Reinstall even if the remote version matches the current one
        #[arg(long)]
        force: bool,
    },
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

/// Writes the alternate-screen/keyboard-enhancement-flags half of startup,
/// in the one order that lands the push on the *alternate* screen's flag
/// stack rather than the main screen's: entering the alternate screen
/// *before* pushing. kitty-protocol terminals (Ghostty, kitty itself, ...)
/// maintain that flag stack *separately per screen buffer*, and every
/// corresponding pop in this codebase (`write_teardown_sequence` below,
/// and `external::run_suspended`'s own resume path) happens while the
/// alternate screen is still current — so a push landing on the wrong
/// stack here is exactly what leaves the flags stuck active on the main
/// screen after exit (Ctrl+A etc. showing up as literal `7;5u`-style
/// bytes in the shell). Split out from `TerminalGuard::new` purely so this
/// ordering is directly unit-testable against a `Vec<u8>` sink instead of
/// a real terminal (see the tests below) — mouse capture isn't part of it
/// since it isn't a per-screen-buffer stack, so its ordering here doesn't
/// affect correctness.
fn write_startup_sequence(w: &mut impl io::Write, keyboard_enhancement: bool) -> io::Result<()> {
    execute!(w, EnterAlternateScreen)?;
    if keyboard_enhancement {
        execute!(
            w,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

/// The mirror of `write_startup_sequence`, for final teardown (`Drop`/the
/// panic hook — *not* `external::run_suspended`'s temporary suspend,
/// which has its own reason to keep raw-mode/pause-message timing
/// interleaved differently): pops *before* `LeaveAlternateScreen`, while
/// the alternate screen — and therefore its flag stack, the one the
/// matching push actually landed on — is still current. Then, as defense
/// in depth, an *unconditional* second pop after leaving: if every push/
/// pop in this codebase stayed perfectly paired this is always popping an
/// empty stack (a spec'd no-op), but it costs nothing and means a desync
/// anywhere — a bug here, a terminal that doesn't fully implement the
/// separate-stacks behavior — still can't leave the *shell* stuck with
/// stray flags active, which is the failure mode that actually matters to
/// a user. `keyboard_enhancement_active` gates only the first (ordering-
/// critical) pop; the defense-in-depth one is unconditional on purpose.
fn write_teardown_sequence(
    w: &mut impl io::Write,
    keyboard_enhancement_active: bool,
) -> io::Result<()> {
    if keyboard_enhancement_active {
        execute!(w, PopKeyboardEnhancementFlags)?;
    }
    execute!(w, LeaveAlternateScreen)?;
    execute!(w, PopKeyboardEnhancementFlags)?;
    Ok(())
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
        // needless latency for a value that can't change mid-session. The
        // query itself doesn't care which screen buffer is active, so its
        // position relative to `write_startup_sequence` below is
        // arbitrary.
        let keyboard_enhancement = supports_keyboard_enhancement().unwrap_or(false);
        let mut stdout = io::stdout();
        write_startup_sequence(&mut stdout, keyboard_enhancement)?;
        if keyboard_enhancement {
            KEYBOARD_ENHANCEMENT_ACTIVE.store(true, Ordering::SeqCst);
        }
        sync_mouse_capture(mouse)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            keyboard_enhancement,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let active = KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst);
        let _ = write_teardown_sequence(&mut io::stdout(), active);
        let _ = disable_raw_mode();
    }
}

/// Installs a panic hook that restores the terminal *before* the default
/// hook prints the panic message, otherwise the message would be swallowed
/// by the alternate screen or mangled by raw mode. Installed before
/// `TerminalGuard` exists, so it can't borrow the guard's fields — it
/// consults the shared `KEYBOARD_ENHANCEMENT_ACTIVE`/`MOUSE_CAPTURE_ACTIVE`
/// flags instead. Uses the exact same `write_teardown_sequence` as
/// `TerminalGuard::drop` — a panic must leave the terminal exactly as
/// clean as a normal quit does.
fn install_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if MOUSE_CAPTURE_ACTIVE.swap(false, Ordering::SeqCst) {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let active = KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst);
        let _ = write_teardown_sequence(&mut io::stdout(), active);
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

/// Fetches `Cargo.toml`'s `package.version` from the `main` branch on
/// GitHub, `None` on any failure (network error, non-200, unparsable TOML,
/// missing field) — deliberately not `Result`: every caller (`self_update`)
/// treats "couldn't check" as just another reason to proceed with the
/// install rather than a distinct error to report, most notably before the
/// GitHub repository has even been made public (a 404, in practice).
fn fetch_remote_version() -> Option<String> {
    let url = "https://raw.githubusercontent.com/m-tkg/Ozzel/main/Cargo.toml";
    let body = ureq::get(url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let parsed: toml::Value = toml::from_str(&body).ok()?;
    Some(parsed.get("package")?.get("version")?.as_str()?.to_string())
}

/// What `self_update` should do given the remote version check's outcome —
/// pulled out as a pure enum/fn pair (rather than left inline as match arms
/// returning early) so the equal/newer/unknown × `--force` decision matrix
/// is directly unit-testable without a network call.
#[derive(Debug, PartialEq, Eq)]
enum UpdateDecision {
    /// Remote version matches the running binary's and `--force` wasn't
    /// given — nothing to do.
    AlreadyLatest,
    /// Proceed with `cargo install --git`: either the remote is a
    /// different version, `--force` was given, or the remote version
    /// couldn't be determined at all (missing repo, network error, ...).
    Install,
}

fn decide_update(current: &str, remote: Option<&str>, force: bool) -> UpdateDecision {
    match remote {
        Some(remote) if remote == current && !force => UpdateDecision::AlreadyLatest,
        _ => UpdateDecision::Install,
    }
}

/// Updates ozzel in place via `cargo install --git`, mirroring the sibling
/// `llmeter` project's `self_update`. Runs before any terminal setup (see
/// `main`), so it's plain stdout/stderr the whole way — never touches the
/// TUI machinery at all.
fn self_update(force: bool) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("現在のバージョン: {current}");

    let remote = fetch_remote_version();
    match decide_update(current, remote.as_deref(), force) {
        UpdateDecision::AlreadyLatest => {
            let remote = remote.expect("AlreadyLatest implies a known remote version");
            println!("最新版です（{remote}）。--force で強制再インストールできる。");
            return Ok(());
        }
        UpdateDecision::Install => match &remote {
            Some(remote) => println!("リモートのバージョン: {remote}。更新する。"),
            None => println!("リモートのバージョン確認に失敗。そのまま再インストールする。"),
        },
    }

    let status = std::process::Command::new("cargo")
        .args(["install", "--git", REPO_URL, "--force"])
        .status();
    match status {
        Ok(s) if s.success() => println!("更新完了。"),
        Ok(s) => bail!("cargo install が失敗した (exit: {s})"),
        Err(e) => bail!("cargo を実行できない: {e}（cargo のインストールが必要）"),
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

    // --- Kitty keyboard-enhancement flag stack ordering ---------------
    //
    // Regression tests for the bug this round fixes: the flags leaking
    // active into the shell after exit (Ctrl+A etc. arriving as literal
    // `7;5u`-style bytes) because a push/pop landed on the wrong one of
    // the kitty protocol's two *separate* per-screen-buffer flag stacks
    // (main vs. alternate). These don't need a real terminal — they
    // capture the exact bytes `write_startup_sequence`/
    // `write_teardown_sequence` write into a `Vec<u8>` and check the
    // *order* the alternate-screen and keyboard-flags escapes appear in,
    // which is the entire crux of the bug.

    /// crossterm's own escape bytes for each of the four commands
    /// involved, confirmed once against the real `execute!` macro output
    /// rather than guessed — see this test module's use of them below.
    const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
    // Only referenced by the keyboard-enhancement tests below, which are
    // unix-only (see their `#[cfg(not(windows))]`) — so on Windows these
    // two would otherwise be dead code.
    #[cfg_attr(
        windows,
        allow(dead_code, reason = "only used by unix-only tests below")
    )]
    const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";
    const PUSH_KEYBOARD_FLAGS: &str = "\x1b[>1u";
    #[cfg_attr(
        windows,
        allow(dead_code, reason = "only used by unix-only tests below")
    )]
    const POP_KEYBOARD_FLAGS: &str = "\x1b[<1u";

    fn captured(f: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    // Push/PopKeyboardEnhancementFlags have no legacy-Windows-API
    // implementation in crossterm — only an ANSI one. `execute!` decides
    // ANSI vs. legacy at runtime by probing the process's *real* stdout
    // console handle (crossterm::ansi_support::supports_ansi()), completely
    // independent of the `Vec<u8>` sink these tests write into. Under a
    // headless `cargo test` run on Windows CI there is no real console
    // handle to probe, so that check comes back false, `execute!` falls
    // back to the legacy path, and Push/PopKeyboardEnhancementFlags's
    // legacy implementation unconditionally returns
    // `Unsupported("Keyboard progressive enhancement not implemented for
    // the legacy Windows API.")` — no bytes are ever written, so there is
    // nothing for these ordering assertions to check. Unix's `execute!`
    // has no such runtime probe (it always writes ANSI), so these stay
    // exercised there.
    #[test]
    #[cfg(not(windows))]
    fn startup_sequence_pushes_keyboard_flags_after_entering_the_alternate_screen() {
        let out = captured(|w| write_startup_sequence(w, true));
        let enter_pos = out.find(ENTER_ALT_SCREEN).expect("EnterAlternateScreen");
        let push_pos = out
            .find(PUSH_KEYBOARD_FLAGS)
            .expect("PushKeyboardEnhancementFlags");
        assert!(
            enter_pos < push_pos,
            "push must come after entering the alternate screen, so it lands \
             on the alternate screen's own flag stack: {out:?}"
        );
    }

    #[test]
    fn startup_sequence_without_keyboard_enhancement_only_enters_the_alternate_screen() {
        let out = captured(|w| write_startup_sequence(w, false));
        assert!(out.contains(ENTER_ALT_SCREEN));
        assert!(!out.contains(PUSH_KEYBOARD_FLAGS));
    }

    // See the comment on `startup_sequence_pushes_keyboard_flags_after_...`
    // above: PopKeyboardEnhancementFlags hits the same Windows-CI-only
    // Unsupported error, unix-only for the same reason.
    #[test]
    #[cfg(not(windows))]
    fn teardown_sequence_pops_keyboard_flags_before_leaving_the_alternate_screen() {
        let out = captured(|w| write_teardown_sequence(w, true));
        let pop_positions: Vec<usize> = out
            .match_indices(POP_KEYBOARD_FLAGS)
            .map(|(i, _)| i)
            .collect();
        let leave_pos = out.find(LEAVE_ALT_SCREEN).expect("LeaveAlternateScreen");
        assert_eq!(
            pop_positions.len(),
            2,
            "expected the ordering-critical pop plus the defense-in-depth \
             extra one: {out:?}"
        );
        assert!(
            pop_positions[0] < leave_pos,
            "the first pop must land on the alternate screen's own flag \
             stack — the one the matching push actually used — which only \
             happens while the alternate screen is still current: {out:?}"
        );
        assert!(
            pop_positions[1] > leave_pos,
            "the defense-in-depth pop must come after leaving, so it \
             targets the main screen's stack too: {out:?}"
        );
    }

    // Same Windows-CI-only Unsupported error as the two tests above — the
    // unconditional defense-in-depth pop is itself a
    // PopKeyboardEnhancementFlags call, unix-only for the same reason.
    #[test]
    #[cfg(not(windows))]
    fn teardown_sequence_still_emits_the_defense_in_depth_pop_even_when_never_active() {
        // The whole point of the second pop is to cover *desync* cases —
        // it must never be gated on the same tracking that might itself
        // be wrong.
        let out = captured(|w| write_teardown_sequence(w, false));
        assert_eq!(out.match_indices(POP_KEYBOARD_FLAGS).count(), 1);
        assert!(out.find(POP_KEYBOARD_FLAGS).unwrap() > out.find(LEAVE_ALT_SCREEN).unwrap());
    }

    // Same Windows-CI-only Unsupported error as the tests above — this
    // round-trips both a push and a pop, unix-only for the same reason.
    #[test]
    #[cfg(not(windows))]
    fn startup_and_teardown_sequences_are_symmetric_round_trips() {
        // A push from `write_startup_sequence` followed immediately by a
        // `write_teardown_sequence` must net out to a clean pair on the
        // alternate screen's stack: enter, push, pop, leave, (extra pop).
        let mut buf = Vec::new();
        write_startup_sequence(&mut buf, true).unwrap();
        write_teardown_sequence(&mut buf, true).unwrap();
        let out = String::from_utf8(buf).unwrap();

        let enter_pos = out.find(ENTER_ALT_SCREEN).unwrap();
        let push_pos = out.find(PUSH_KEYBOARD_FLAGS).unwrap();
        let first_pop_pos = out.find(POP_KEYBOARD_FLAGS).unwrap();
        let leave_pos = out.find(LEAVE_ALT_SCREEN).unwrap();

        assert!(
            enter_pos < push_pos && push_pos < first_pop_pos && first_pop_pos < leave_pos,
            "expected enter < push < pop < leave: {out:?}"
        );
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

    // --- `ozzel update` version-compare decision -----------------------

    #[test]
    fn decide_update_same_version_without_force_is_already_latest() {
        assert_eq!(
            decide_update("1.2.3", Some("1.2.3"), false),
            UpdateDecision::AlreadyLatest
        );
    }

    #[test]
    fn decide_update_same_version_with_force_installs_anyway() {
        assert_eq!(
            decide_update("1.2.3", Some("1.2.3"), true),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_newer_remote_version_installs() {
        assert_eq!(
            decide_update("1.2.3", Some("1.3.0"), false),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_newer_remote_version_installs_with_force_too() {
        assert_eq!(
            decide_update("1.2.3", Some("1.3.0"), true),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_unknown_remote_version_always_installs() {
        // Most notably: the GitHub repo not existing/being public yet, per
        // `fetch_remote_version`'s doc comment — a failed version check is
        // never a reason to refuse to proceed.
        assert_eq!(decide_update("1.2.3", None, false), UpdateDecision::Install);
        assert_eq!(decide_update("1.2.3", None, true), UpdateDecision::Install);
    }

    // --- CLI parsing: optional subcommand alongside positional dirs ----
    //
    // `Cli` mixes a `#[command(subcommand)] command: Option<Command>` field
    // with the pre-existing positional `left_dir`/`right_dir` args (see the
    // comment above the `Cli` struct) specifically so `ozzel update` keeps
    // working as a real subcommand *and* `ozzel`, `ozzel <left> <right>`,
    // `ozzel --cwd-file f` all keep meaning exactly what they meant before
    // this round. These tests exercise `Cli::try_parse_from` directly —
    // no process spawn, no network.

    #[test]
    fn cli_parses_update_as_a_subcommand_not_a_left_dir() {
        let cli = Cli::try_parse_from(["ozzel", "update"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Update { force: false })
        ));
        assert_eq!(cli.left_dir, None);
    }

    #[test]
    fn cli_parses_update_dash_dash_force() {
        let cli = Cli::try_parse_from(["ozzel", "update", "--force"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Update { force: true })));
    }

    #[test]
    fn cli_with_no_arguments_has_no_subcommand_and_no_dirs() {
        let cli = Cli::try_parse_from(["ozzel"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.left_dir, None);
        assert_eq!(cli.right_dir, None);
    }

    #[test]
    fn cli_parses_two_positional_directories_with_no_subcommand() {
        let cli = Cli::try_parse_from(["ozzel", ".", ".."]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.left_dir, Some(PathBuf::from(".")));
        assert_eq!(cli.right_dir, Some(PathBuf::from("..")));
    }

    #[test]
    fn cli_parses_cwd_file_alone_with_no_positional_directories() {
        let cli = Cli::try_parse_from(["ozzel", "--cwd-file", "/tmp/f"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.left_dir, None);
        assert_eq!(cli.cwd_file, Some(PathBuf::from("/tmp/f")));
    }

    #[test]
    fn cli_parses_cwd_file_combined_with_positional_directories() {
        let cli = Cli::try_parse_from(["ozzel", "left", "right", "--cwd-file", "/tmp/f"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.left_dir, Some(PathBuf::from("left")));
        assert_eq!(cli.right_dir, Some(PathBuf::from("right")));
        assert_eq!(cli.cwd_file, Some(PathBuf::from("/tmp/f")));
    }

    #[test]
    fn cli_parses_a_dot_slash_prefixed_update_directory_as_a_positional_arg() {
        // The documented escape hatch for a real directory named `update`:
        // prefixing it means it no longer matches the subcommand token
        // literally, so it's just an ordinary positional path.
        let cli = Cli::try_parse_from(["ozzel", "./update"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.left_dir, Some(PathBuf::from("./update")));
    }
}
