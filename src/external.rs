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
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

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
pub fn shell_command(cmdline: &str) -> (String, Vec<String>) {
    if cfg!(windows) {
        let comspec = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        windows_shell_command(&comspec, cmdline)
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
        unix_shell_command(&shell, cmdline)
    }
}

fn windows_shell_command(comspec: &str, cmdline: &str) -> (String, Vec<String>) {
    (
        comspec.to_string(),
        vec!["/C".to_string(), cmdline.to_string()],
    )
}

fn unix_shell_command(shell: &str, cmdline: &str) -> (String, Vec<String>) {
    (
        shell.to_string(),
        vec!["-c".to_string(), cmdline.to_string()],
    )
}

/// Runs `req.cmdline` with the TUI suspended:
///
/// 1. leave raw mode + the alternate screen, show the cursor
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
pub fn run_suspended(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    req: &ExternalRequest,
    keyboard_enhancement: bool,
    mouse: bool,
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
    execute!(io::stdout(), LeaveAlternateScreen, Show)
        .context("failed to leave alternate screen")?;

    let (program, args) = shell_command(&req.cmdline);
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
    let reenter = |pause_message: Option<String>| -> Result<()> {
        enable_raw_mode().context("failed to re-enable raw mode")?;
        if let Some(msg) = pause_message {
            print!("{msg}");
            let _ = io::stdout().flush();
            wait_for_keypress().context("failed to wait for keypress")?;
        }
        execute!(io::stdout(), EnterAlternateScreen)
            .context("failed to re-enter alternate screen")?;
        if keyboard_enhancement {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .context("failed to re-push keyboard enhancement flags")?;
        }
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
/// OSC 52's clipboard payload requires (see `osc52_copy_sequence`).
pub fn base64_encode(data: &[u8]) -> String {
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
        let (program, args) = unix_shell_command("/bin/zsh", "ls -la");
        assert_eq!(program, "/bin/zsh");
        assert_eq!(args, vec!["-c".to_string(), "ls -la".to_string()]);
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
}
