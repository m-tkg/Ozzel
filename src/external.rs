//! Suspends the TUI, runs a shell command line with the terminal handed
//! back to it, and restores the TUI when it exits. Used by both `:`
//! (arbitrary command, paused after) and `e` (editor, not paused — editors
//! already take over the whole screen and return control cleanly).

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
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
pub fn run_suspended(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    req: &ExternalRequest,
) -> Result<Option<String>> {
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

    let spawn_error = match spawn_result {
        Ok(status) => {
            if req.pause_after {
                print!("\r\n[ozzel] exit: {status} — press any key\r\n");
                let _ = io::stdout().flush();
                enable_raw_mode().context("failed to re-enable raw mode")?;
                wait_for_keypress().context("failed to wait for keypress")?;
            } else {
                enable_raw_mode().context("failed to re-enable raw mode")?;
            }
            None
        }
        Err(err) => {
            enable_raw_mode().context("failed to re-enable raw mode")?;
            Some(format!("failed to run `{}`: {err}", req.cmdline))
        }
    };

    execute!(io::stdout(), EnterAlternateScreen).context("failed to re-enter alternate screen")?;
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
}
