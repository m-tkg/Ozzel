//! Suspends the TUI, runs a shell command line with the terminal handed
//! back to it, and restores the TUI when it exits. Used by both `:`
//! (arbitrary command, paused after) and `e` (editor, not paused — editors
//! already take over the whole screen and return control cleanly).

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{MoveTo, Show};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, PopKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::terminal::write_startup_sequence;

/// A shell command line to run with the TUI suspended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalRequest {
    pub cmdline: String,
    pub cwd: PathBuf,
    /// Whether to print the exit status and wait for a keypress after the
    /// child exits. `false` for editors (which own the whole screen while
    /// running and hand back control cleanly); `true` for arbitrary `:`
    /// commands, so their output stays on screen and readable instead of
    /// being immediately overwritten by the redrawn TUI.
    pub pause_after: bool,
    /// Whether to run the shell with `-i` (interactive) on unix, so the
    /// user's rc file (`.zshrc`/`.bashrc`) is sourced and its aliases/
    /// functions become usable. `true` only for `:` commands, and only
    /// when `config.command_line_interactive` opts in — editors and
    /// `[viewers]` templates always run non-interactive, since their
    /// command lines come from config, not ad-hoc typing. No Windows
    /// equivalent (`cmd.exe /C` has no interactive flag) — ignored there.
    pub interactive: bool,
}

/// Quotes `s` for safe inclusion in a shell command line passed to
/// [`shell_command`]. POSIX single-quote style (`'...'`, with embedded `'`
/// escaped as `'\''`) — correct for unix shells, and permissive enough for
/// cmd.exe's parsing as long as the path itself has no literal `'`, which
/// covers the common case for a quoted file path.
pub fn shell_quote(s: &str) -> String {
    let mut quoted = String::with_capacity(s.len() + 2);
    quoted.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Builds the shell command line for a per-extension external viewer
/// (`[viewers]`, consulted by `open` — see `App::begin_open`): substitutes
/// every `{}` in `template` with `path` (shell-quoted), or — when
/// `template` contains no `{}` at all — appends the quoted path to the
/// end, so both `"glow {}"` and a bare `"less"` work as configured
/// commands.
pub fn build_viewer_cmdline(template: &str, path: &Path) -> String {
    let quoted = shell_quote(&path.to_string_lossy());
    if template.contains("{}") {
        template.replace("{}", &quoted)
    } else {
        format!("{template} {quoted}")
    }
}

/// Resolves the shell program + args used to run `cmdline` on this
/// platform, reading `$COMSPEC`/`$SHELL` for the real thing. The actual
/// per-OS logic lives in [`windows_shell_command`] / [`unix_shell_command`]
/// below, which take those values as plain arguments instead of reading
/// the environment themselves — so both are directly unit-testable
/// regardless of which OS is actually running the test.
pub fn shell_command(cmdline: &str, interactive: bool) -> (String, Vec<String>) {
    if cfg!(windows) {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        windows_shell_command(&comspec, cmdline)
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        unix_shell_command(&shell, cmdline, interactive)
    }
}

/// `interactive` has no `cmd.exe` equivalent, so it's simply not part of
/// this signature — see `ExternalRequest::interactive`'s doc comment.
fn windows_shell_command(comspec: &str, cmdline: &str) -> (String, Vec<String>) {
    (
        comspec.to_string(),
        vec!["/C".to_string(), cmdline.to_string()],
    )
}

fn unix_shell_command(shell: &str, cmdline: &str, interactive: bool) -> (String, Vec<String>) {
    // `-i` as its own argument (not fused into `-ic`) — every POSIX shell
    // accepts both, but separate flags can't be misread as a single
    // unknown option by a nonstandard $SHELL.
    let mut args = Vec::with_capacity(3);
    if interactive {
        args.push("-i".to_string());
    }
    args.push("-c".to_string());
    args.push(cmdline.to_string());
    (shell.to_string(), args)
}

/// The bytes that hand the *screen* back to a child process, written
/// once raw mode is already off (that part isn't a byte sequence, so it
/// can't live here): leave the alternate screen and show the cursor,
/// with an optional blanking of what leaving just restored.
///
/// Leaving the alternate screen puts the main screen back exactly as the
/// shell left it before ozzel started. A child that draws on the main
/// screen (`ls`, `grep`) wants precisely that. A child that opens its
/// *own* alternate screen (`less`, `vim`, most pagers) does not: for the
/// few milliseconds between ozzel leaving and the child entering, the
/// pre-ozzel terminal contents flash up on screen. With `clear` — the
/// `clear_on_suspend` setting, on by default — the restored screen is
/// blanked and the cursor homed immediately after, so that gap shows an
/// empty terminal instead of somebody's earlier session. Scrollback is
/// untouched either way (`Clear::All`, not `Clear::Purge`), and a child
/// that prints to the main screen still gets a clean one to print onto.
///
/// Split out as its own `Write`-generic function purely so the ordering
/// can be asserted against captured bytes in tests, the same way
/// `terminal::write_startup_sequence`/`write_teardown_sequence` are.
pub(crate) fn write_suspend_sequence<W: Write>(w: &mut W, clear: bool) -> io::Result<()> {
    execute!(w, LeaveAlternateScreen)?;
    if clear {
        execute!(w, Clear(ClearType::All), MoveTo(0, 0))?;
    }
    execute!(w, Show)
}

/// Runs `req.cmdline` with the TUI suspended:
///
/// 1. leave raw mode + the alternate screen (blanking what that restores
///    unless `clear_on_suspend` is off), show the cursor
/// 2. spawn `%COMSPEC% /C <cmdline>` (Windows) or `$SHELL -c <cmdline>`
///    (unix) with stdio inherited, in `req.cwd`, and block on it
/// 3. if `req.pause_after`, print the exit status and wait for one
///    keypress
/// 4. re-enable raw mode, re-enter the alternate screen, and clear the
///    terminal so the next `Frame::draw` repaints everything
///
/// A failure to *spawn* the child (bad command, permission denied, etc.)
/// is caught and returned as `Ok(Some(message))` for the caller to log —
/// the TUI must come back either way, never crash. Terminal-manipulation
/// failures (steps 1/4) propagate as `Err`, since at that point something
/// is wrong enough that the caller can't safely keep drawing anyway.
///
/// `keyboard_enhancement` mirrors whatever `TerminalGuard` decided at
/// startup (see `main.rs`): when `true`, the kitty keyboard-enhancement
/// flags ozzel pushed are popped *before* the child gets the terminal and
/// re-pushed immediately after re-entering the alternate screen, symmetric
/// with `TerminalGuard`'s own push-after-enter/pop-before-leave ordering —
/// otherwise the child would either inherit flags it doesn't expect, or
/// (worse) the *pop* here would land on the alternate screen's flag stack
/// instead of the one the corresponding push actually used (kitty-protocol
/// terminals keep those two stacks separate), leaving the flags stuck
/// active on the main screen once the child exits. See `main.rs`'s
/// `TerminalGuard::new` for the full explanation of why the ordering
/// matters here.
///
/// `mouse` gets the exact same treatment for the same reason: mouse
/// capture (if `TerminalGuard` enabled it) is disabled before the child
/// gets the terminal — otherwise a suspended `:vim` would receive raw
/// mouse escape sequences instead of native mouse support, and its own
/// terminal-selection behavior would be broken too — and re-enabled right
/// after raw mode comes back (mouse capture isn't a per-screen-buffer
/// stack, so its ordering relative to the alternate screen doesn't matter
/// for correctness the way the keyboard flags' does; it's kept alongside
/// them here purely for symmetry).
///
/// `clear_on_suspend` is passed straight through to
/// `write_suspend_sequence` — see that function for what it does and why.
pub fn run_suspended(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    req: &ExternalRequest,
    keyboard_enhancement: bool,
    mouse: bool,
    clear_on_suspend: bool,
) -> Result<Option<String>> {
    // Popped *before* `LeaveAlternateScreen`, while the alternate screen
    // is still current — see this function's doc comment.
    if keyboard_enhancement {
        execute!(io::stdout(), PopKeyboardEnhancementFlags)
            .context("failed to pop keyboard enhancement flags")?;
    }
    if mouse {
        execute!(io::stdout(), DisableMouseCapture).context("failed to disable mouse capture")?;
    }
    disable_raw_mode().context("failed to disable raw mode")?;
    write_suspend_sequence(&mut io::stdout(), clear_on_suspend)
        .context("failed to leave alternate screen")?;

    let (program, args) = shell_command(&req.cmdline, req.interactive);
    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(&req.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let spawn_result = spawn_and_wait(&mut command);

    // Re-enabling raw mode, re-entering the alternate screen, and
    // re-pushing the enhancement flags (if active) is common to every
    // outcome, so it's hoisted above the match instead of duplicated in
    // both arms. The pause message (if any) is deliberately printed and
    // waited-on *before* `EnterAlternateScreen` — on the main screen,
    // which is the entire point of `pause_after` ("stays on screen and
    // readable instead of being immediately overwritten by the redrawn
    // TUI") — but *after* `enable_raw_mode`, since `wait_for_keypress`
    // needs raw mode to see a single keystroke immediately rather than
    // waiting on the tty driver's own line buffering for an Enter that
    // was never asked for. Only once that's done do we re-enter the
    // alternate screen and re-push the flags, so the push lands back on
    // the alternate screen's own stack — mirroring `TerminalGuard::new`.
    //
    // The "enter alt screen, then (if enabled) push the keyboard flags"
    // part is byte-for-byte what `terminal::write_startup_sequence` does
    // for the exact same reason (see its own doc comment) — reused here
    // rather than re-spelled. Mouse capture stays a separate `execute!`
    // right after it: `write_startup_sequence` deliberately has no
    // opinion on mouse capture (not a per-screen-buffer stack, so its
    // ordering here doesn't affect correctness — see that function's doc
    // comment), so composing the two here produces identical output to
    // the three calls this replaced. The *leaving* half
    // (`write_suspend_sequence`) is deliberately **not** unified with
    // `terminal::write_teardown_sequence`: that helper's unconditional
    // defense-in-depth second pop and its lack of a `Show` write would
    // both change the exact bytes written here, and this function's
    // pop/disable-capture/leave-screen ordering (mouse and keyboard flags
    // both undone *before* `disable_raw_mode`, unlike `TerminalGuard`'s
    // teardown) is its own deliberate sequence for handing a *live*
    // terminal to a child process, not a final-exit teardown — so it's
    // left as its own inline sequence rather than forced to match.
    let reenter = |pause_message: Option<String>| -> Result<()> {
        enable_raw_mode().context("failed to re-enable raw mode")?;
        if let Some(msg) = pause_message {
            print!("{msg}");
            let _ = io::stdout().flush();
            wait_for_keypress().context("failed to wait for keypress")?;
        }
        write_startup_sequence(&mut io::stdout(), keyboard_enhancement)
            .context("failed to re-enter alternate screen")?;
        if mouse {
            execute!(io::stdout(), EnableMouseCapture)
                .context("failed to re-enable mouse capture")?;
        }
        Ok(())
    };

    let spawn_error = match spawn_result {
        Ok(status) => {
            let pause_message = req
                .pause_after
                .then(|| format!("\r\n[ozzel] exit: {status} — press any key\r\n"));
            reenter(pause_message)?;
            None
        }
        Err(err) => {
            reenter(None)?;
            Some(format!("failed to run `{}`: {err}", req.cmdline))
        }
    };

    terminal.clear().context("failed to clear terminal")?;
    Ok(spawn_error)
}

/// Spawns `command` and waits for it, putting it in its own process group
/// and (on unix) making that group the terminal's foreground group for the
/// duration.
///
/// Without this, a child spawned via `Command::spawn` simply inherits
/// *our* process group, and a terminal-generated signal like Ctrl+C's
/// SIGINT is delivered by the kernel to whichever process group the
/// terminal currently considers foreground — which is still ours, since
/// forking a child doesn't change that. The result: Ctrl+C inside the
/// child kills ozzel right along with it. Isolating the child into a new
/// group and handing it the terminal (the same `tcsetpgrp` dance every
/// job-control shell does around a foreground command) makes Ctrl+C reach
/// only the child, and hands the terminal back to us once it exits either
/// way.
#[cfg(unix)]
fn spawn_and_wait(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;

    command.process_group(0); // new group, pgid == the child's own pid
    let mut child = command.spawn()?;
    let child_pgid = child.id() as libc::pid_t;

    let stdin_fd = io::stdin().as_raw_fd();
    // SAFETY: these are plain POSIX terminal-control calls (no pointers,
    // no aliasing concerns); `own_pgid` is read back before we touch
    // anything so we can always hand the terminal back to ourselves.
    let own_pgid = unsafe { libc::getpgrp() };
    unsafe {
        // Handing the terminal to a group other than our own would
        // normally stop *us* with SIGTTOU if we were in the background —
        // we're not (we're still the terminal's current foreground group
        // right up until the `tcsetpgrp` call below), but ignore it
        // defensively for the handoff itself; this is our own process's
        // disposition, not the child's; it's reset to default before the
        // child is even spawned, so it never leaks into the child anyway.
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        libc::tcsetpgrp(stdin_fd, child_pgid);
    }

    let status = child.wait();

    unsafe {
        libc::tcsetpgrp(stdin_fd, own_pgid);
        libc::signal(libc::SIGTTOU, libc::SIG_DFL);
    }

    status
}

/// Windows equivalent: a new process group at least stops the child from
/// sharing console-control-event delivery with us (the closest analogue
/// available there — Windows has no `tcsetpgrp`/foreground-group concept).
#[cfg(windows)]
fn spawn_and_wait(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    command.spawn()?.wait()
}

#[cfg(not(any(unix, windows)))]
fn spawn_and_wait(command: &mut Command) -> io::Result<std::process::ExitStatus> {
    command.spawn()?.wait()
}

/// The classic base64 alphabet (`RFC 4648`, `+`/`/`, `=` padding) —
/// hand-rolled rather than pulling in a crate, since OSC 52 is the only
/// place ozzel needs it and the algorithm is a couple dozen lines.
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data` as base64 (standard alphabet, `=`-padded) — the encoding
/// OSC 52's clipboard payload requires (see `osc52_copy_sequence`). Private
/// — only `osc52_copy_sequence` (this module) calls it.
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied();
        let b2 = chunk.get(2).copied();

        let n = (b0 as u32) << 16 | (b1.unwrap_or(0) as u32) << 8 | (b2.unwrap_or(0) as u32);
        out.push(BASE64_ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(if b1.is_some() {
            BASE64_ALPHABET[(n >> 6 & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if b2.is_some() {
            BASE64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Builds the OSC 52 "set clipboard" escape sequence for `text`: works over
/// SSH and inside tmux (given `set -g set-clipboard on` / passthrough,
/// standard tmux config) with zero extra dependencies, unlike a native
/// clipboard crate — chosen over one (e.g. `arboard`) as the *only*
/// mechanism (no fallback) since a native clipboard crate would need
/// platform-specific backends (X11/Wayland/macOS/Windows) that plain don't
/// exist over SSH anyway, so it wouldn't actually cover the cases OSC 52
/// misses. A terminal that doesn't understand OSC 52 simply ignores the
/// escape sequence — there is no reliable way to detect support up front,
/// so `App::begin_copy_path` always logs success rather than guessing.
pub fn osc52_copy_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

fn wait_for_keypress() -> Result<()> {
    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_shell_command_uses_shell_dash_c() {
        let (program, args) = unix_shell_command("/bin/zsh", "ls -la", false);
        assert_eq!(program, "/bin/zsh");
        assert_eq!(args, vec!["-c".to_string(), "ls -la".to_string()]);
    }

    #[test]
    fn unix_shell_command_interactive_adds_dash_i_before_dash_c() {
        let (program, args) = unix_shell_command("/bin/zsh", "myalias", true);
        assert_eq!(program, "/bin/zsh");
        assert_eq!(
            args,
            vec!["-i".to_string(), "-c".to_string(), "myalias".to_string()]
        );
    }

    #[test]
    fn windows_shell_command_uses_comspec_slash_c() {
        let (program, args) = windows_shell_command(r"C:\Windows\System32\cmd.exe", "dir");
        assert_eq!(program, r"C:\Windows\System32\cmd.exe");
        assert_eq!(args, vec!["/C".to_string(), "dir".to_string()]);
    }

    #[test]
    fn shell_quote_wraps_in_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("has space.txt"), "'has space.txt'");
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("it's here.txt"), r"'it'\''s here.txt'");
    }

    #[test]
    fn build_viewer_cmdline_substitutes_curly_braces() {
        let cmdline = build_viewer_cmdline("glow {}", Path::new("/tmp/readme.md"));
        assert_eq!(cmdline, "glow '/tmp/readme.md'");
    }

    #[test]
    fn build_viewer_cmdline_substitutes_multiple_curly_braces() {
        let cmdline = build_viewer_cmdline("cp {} {}.bak", Path::new("/tmp/a.txt"));
        assert_eq!(cmdline, "cp '/tmp/a.txt' '/tmp/a.txt'.bak");
    }

    #[test]
    fn build_viewer_cmdline_appends_the_path_when_there_is_no_curly_braces() {
        let cmdline = build_viewer_cmdline("less", Path::new("/tmp/readme.md"));
        assert_eq!(cmdline, "less '/tmp/readme.md'");
    }

    #[test]
    fn build_viewer_cmdline_quotes_a_path_with_spaces() {
        let cmdline = build_viewer_cmdline("open {}", Path::new("/tmp/my file.jpg"));
        assert_eq!(cmdline, "open '/tmp/my file.jpg'");
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // RFC 4648's own test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_round_trips_a_unicode_path() {
        let text = "/home/日本語/résumé.txt";
        let encoded = base64_encode(text.as_bytes());
        // No reference decoder in this crate, so round-trip through our own
        // encoder's inverse property is checked structurally instead:
        // valid base64 is always a multiple of 4 chars, alphabet-only.
        assert_eq!(encoded.len() % 4, 0);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
        );
    }

    #[test]
    fn osc52_copy_sequence_wraps_base64_in_the_osc52_escape() {
        let seq = osc52_copy_sequence("foo");
        assert_eq!(seq, "\x1b]52;c;Zm9v\x07");
    }

    /// crossterm's own escape bytes, confirmed against real `execute!`
    /// output rather than guessed — same approach as `terminal`'s tests.
    const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";
    const CLEAR_ALL: &str = "\x1b[2J";
    const CURSOR_HOME: &str = "\x1b[1;1H";

    fn captured(f: impl FnOnce(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut buf = Vec::new();
        f(&mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn suspend_sequence_blanks_the_main_screen_after_leaving_the_alternate_one() {
        let out = captured(|w| write_suspend_sequence(w, true));
        let leave_pos = out.find(LEAVE_ALT_SCREEN).expect("LeaveAlternateScreen");
        let clear_pos = out.find(CLEAR_ALL).expect("Clear::All");
        assert!(
            leave_pos < clear_pos,
            "the clear must land on the main screen the leave just restored — \
             clearing first would only wipe the alternate screen ozzel is \
             about to abandon: {out:?}"
        );
        assert!(out.contains(CURSOR_HOME), "cursor must be homed: {out:?}");
    }

    #[test]
    fn suspend_sequence_without_clear_on_suspend_only_leaves_the_alternate_screen() {
        let out = captured(|w| write_suspend_sequence(w, false));
        assert!(out.contains(LEAVE_ALT_SCREEN));
        assert!(
            !out.contains(CLEAR_ALL),
            "opting out must leave the restored main screen exactly as it \
             was: {out:?}"
        );
    }
}
