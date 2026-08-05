//! Terminal lifecycle plumbing: entering/leaving the alternate screen, the
//! kitty keyboard-enhancement flags, mouse capture, and the panic hook that
//! has to undo all of it before the default panic message prints. Shared by
//! `main.rs` (the real session) and `external::run_suspended` (which
//! temporarily undoes/redoes the same escapes around a suspended child
//! process — see that function's own doc comment for how its ordering
//! mirrors this module's).

use std::io::{self, Stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};

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
pub static MOUSE_CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);

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
pub fn sync_mouse_capture(wanted: bool) -> io::Result<()> {
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

/// Restores the terminal (raw mode + alternate screen, plus the kitty
/// keyboard-enhancement flags if they were pushed) when dropped, so that
/// every early-return path (including panics) leaves the shell usable.
pub struct TerminalGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Whether this terminal answered the kitty keyboard-protocol query
    /// (see `supports_keyboard_enhancement`'s docs). When `true`,
    /// `S-enter` is reported distinctly from plain `Enter`; when `false`,
    /// every keypress this session sees `S-enter` as arrives as plain
    /// `Enter` instead, since there is no way to disambiguate them.
    pub keyboard_enhancement: bool,
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
pub fn write_startup_sequence(
    w: &mut impl io::Write,
    keyboard_enhancement: bool,
) -> io::Result<()> {
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
pub fn write_teardown_sequence(
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
    pub fn new(mouse: bool) -> io::Result<Self> {
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
pub fn install_panic_hook() {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
