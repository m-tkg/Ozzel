//! The process-manager's pure logic: the `ps(1)` command line, a parser for
//! its output, sorting, cursor re-anchoring, and signal delivery.
//!
//! Everything here is free of terminal and app state — running `ps` lives in
//! `crate::tasks::process_list`, drawing in `crate::ui::process_view`, and
//! the mode/key wiring in `crate::app::process_manager` — so the fiddly
//! parts (a column layout that shifts with the widest value, `etime`
//! arithmetic, a cursor that must survive rows appearing and disappearing
//! underneath it) are unit-testable without an `App` or a `Frame`.
//!
//! `ps` was chosen over a crate like `sysinfo` for the same reason
//! `tasks::git_status` shells out to `git` rather than linking libgit2: the
//! project's dependency policy avoids a C toolchain, and this view is
//! Unix-only anyway, so `ps` plus `libc::kill` needs no new dependency at
//! all. The cost is that `%CPU` means whatever `ps` says it means (on Linux
//! a lifetime average, on macOS a decayed one) rather than the instantaneous
//! sample `top` shows — documented as a limitation rather than worked
//! around.

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::Result;

/// How long a command line is kept before it's cut. Real command lines run
/// to ten thousand characters (a sandboxed simulator process on macOS was
/// measured at 9971), and every one of them would be re-allocated on every
/// refresh while only the first ~200 columns can ever be on screen. Cutting
/// at parse time bounds the snapshot's memory instead of the renderer's.
pub const MAX_COMMAND_LEN: usize = 512;

/// How often the list re-runs `ps` while the view is open. Slow enough that
/// the cost is invisible, fast enough that a process you just killed is gone
/// before you wonder whether the key registered.
pub const PROCESS_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// The exact `ps` invocation `parse_ps_output` is written against. The two
/// are one unit — the `-o` list *is* the parser's column order — which is
/// why this isn't a config key: a user-edited column list would shift every
/// field silently rather than fail loudly.
///
/// Portability notes, all of which cost something if changed:
/// - Each column carries a `=` suffix, which suppresses the header row
///   (POSIX; works on both BSD `ps` and procps).
/// - `args` rather than `comm`, and it must stay last. Linux's `comm` is
///   truncated to 15 bytes by the kernel (`systemd-journald` arrives as
///   `systemd-journal`) and macOS's `comm` is a full path, so neither gives
///   a usable name on both; `args` gives the full command line everywhere,
///   and being last is what lets the parser treat "everything after field 8"
///   as the command even though it contains spaces.
/// - `pcpu`/`pmem` rather than `%cpu`/`%mem` — same columns under names that
///   don't put a `%` in an argument.
/// - `etime` rather than procps' `etimes` (seconds), which macOS doesn't
///   have; `parse_etime` does the conversion instead.
/// - `-e -ww -o` spelled out rather than clustered as `-ewwo`, which procps
///   is not reliable about. `-ww` only matters when stdout is a terminal
///   (`Command::output` gives a pipe, where BSD `ps` doesn't truncate), but
///   it costs nothing to be explicit.
pub const PS_ARGS: &[&str] = &[
    "-e",
    "-ww",
    "-o",
    "pid=,ppid=,user=,pcpu=,pmem=,rss=,stat=,etime=,args=",
];

/// How many whitespace-free columns precede `args` in `PS_ARGS`.
const LEADING_FIELDS: usize = 8;

/// One row of the process list, already parsed and ready to render.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub user: String,
    /// `pcpu`, as `ps` reports it — see the module docs on what it means.
    pub cpu: f32,
    /// `pmem`.
    pub mem: f32,
    /// `rss`, in KiB on both BSD `ps` and procps.
    pub rss_kib: u64,
    /// `stat`, kept as the raw string: the letters differ between platforms
    /// (`Ss` vs `Ssl+`) and decoding them would be a per-platform table for
    /// no gain.
    pub state: String,
    /// `etime` verbatim, for display.
    pub etime: String,
    /// `etime` in seconds, for sorting. `None` when it didn't parse, which
    /// sorts to the bottom in both directions.
    pub etime_secs: Option<u64>,
    /// The full command line, cut to `MAX_COMMAND_LEN`.
    pub command: String,
    /// `argv[0]`'s basename — what the `n` sort key orders by and what the
    /// kill confirmation names.
    pub name: String,
}

/// Which column the list is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortKey {
    Pid,
    User,
    Cpu,
    Mem,
    Rss,
    Time,
    Name,
}

impl ProcessSortKey {
    /// The footer label, also used in the header line.
    pub fn label(self) -> &'static str {
        match self {
            ProcessSortKey::Pid => "PID",
            ProcessSortKey::User => "USER",
            ProcessSortKey::Cpu => "%CPU",
            ProcessSortKey::Mem => "%MEM",
            ProcessSortKey::Rss => "RSS",
            ProcessSortKey::Time => "ELAPSED",
            ProcessSortKey::Name => "COMMAND",
        }
    }
}

/// The direction a key sorts in the *first* time it's pressed; pressing it
/// again flips this (the `top`/`htop` convention). Usage columns start
/// descending because "what's eating the machine" is the question they're
/// there to answer; identity columns start ascending because that's the
/// order they read in.
pub fn default_ascending(key: ProcessSortKey) -> bool {
    match key {
        ProcessSortKey::Pid | ProcessSortKey::User | ProcessSortKey::Name => true,
        ProcessSortKey::Cpu | ProcessSortKey::Mem | ProcessSortKey::Rss | ProcessSortKey::Time => {
            false
        }
    }
}

/// The signals the view can send. Deliberately just these two: `x` for the
/// polite one, `X` for the one that can't be ignored. Adding SIGHUP/SIGINT
/// would spend keys on cases a shell handles better.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

impl Signal {
    pub fn label(self) -> &'static str {
        match self {
            Signal::Term => "SIGTERM",
            Signal::Kill => "SIGKILL",
        }
    }

    #[cfg(unix)]
    fn raw(self) -> libc::c_int {
        match self {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        }
    }
}

/// Parses `ps` output produced with `PS_ARGS`. Returns the rows plus a count
/// of lines that couldn't be read at all — normally zero, since the `=`
/// suffixes suppress the header, but a `ps` that emitted one anyway would
/// have its `PID PPID ...` line land in that count rather than on screen.
///
/// Columns are never cut at fixed offsets: `ps` re-computes each column's
/// width from the widest value in *this* run, so an offset that works on one
/// machine is wrong on the next. Instead the eight whitespace-free fields
/// are taken one at a time and whatever remains is the command line — which
/// is the only field that can contain spaces, and is why `args` has to stay
/// last in `PS_ARGS`.
pub fn parse_ps_output(stdout: &str) -> (Vec<ProcessInfo>, usize) {
    let mut procs = Vec::new();
    let mut skipped = 0usize;

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((fields, command)) = take_fields(line, LEADING_FIELDS) else {
            skipped += 1;
            continue;
        };
        // A non-numeric pid means this isn't a process row (a stray header,
        // a warning line). Everything after it is best-effort: a field that
        // won't parse becomes zero rather than costing us the whole row,
        // since the pid and the command are what the view is actually for.
        let Ok(pid) = fields[0].parse::<u32>() else {
            skipped += 1;
            continue;
        };

        let command = truncate_command(command);
        let etime = fields[7].to_string();
        procs.push(ProcessInfo {
            pid,
            ppid: fields[1].parse().unwrap_or(0),
            user: fields[2].to_string(),
            cpu: fields[3].parse().unwrap_or(0.0),
            mem: fields[4].parse().unwrap_or(0.0),
            rss_kib: fields[5].parse().unwrap_or(0),
            state: fields[6].to_string(),
            etime_secs: parse_etime(&etime),
            etime,
            name: command_name(&command),
            command,
        });
    }

    (procs, skipped)
}

/// Splits off the first `n` whitespace-separated fields, returning them and
/// the (trimmed) remainder. `None` when the line has fewer than `n` fields.
///
/// `splitn(n + 1, char::is_whitespace)` would be shorter but wrong: runs of
/// spaces — which `ps` pads its columns with — would come back as empty
/// fields.
fn take_fields(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(n);
    let mut rest = line;
    for _ in 0..n {
        let start = rest.find(|c: char| !c.is_whitespace())?;
        rest = &rest[start..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((fields, rest.trim()))
}

/// Cuts a command line to `MAX_COMMAND_LEN` bytes on a char boundary. Byte
/// length rather than display width on purpose: this is a memory bound, not
/// a layout decision — `ui::text` does the width-aware truncation when it
/// knows how many columns the terminal actually gave it.
fn truncate_command(command: &str) -> String {
    if command.len() <= MAX_COMMAND_LEN {
        return command.to_string();
    }
    let end = (0..=MAX_COMMAND_LEN)
        .rev()
        .find(|i| command.is_char_boundary(*i))
        .unwrap_or(0);
    command[..end].to_string()
}

/// The short name to show and sort by: `argv[0]`'s basename, with the
/// brackets Linux wraps kernel threads in (`[kthreadd]`) removed. `"?"` when
/// there's no command line at all — rare, but `ps` does emit it.
pub fn command_name(command: &str) -> String {
    let argv0 = command.split_whitespace().next().unwrap_or("");
    let argv0 = argv0.trim_start_matches('[').trim_end_matches(']');
    if argv0.is_empty() {
        return "?".to_string();
    }
    match argv0.rsplit('/').next() {
        Some(base) if !base.is_empty() => base.to_string(),
        // A command line ending in `/` has no basename; show it as-is.
        _ => argv0.to_string(),
    }
}

/// Converts `ps`'s `etime` (`[[DD-]HH:]MM:SS`, the one format both BSD `ps`
/// and procps agree on) to seconds. `None` for anything that doesn't fit,
/// which the sort then keeps at the bottom rather than guessing a position
/// for.
pub fn parse_etime(s: &str) -> Option<u64> {
    let (days, hms) = match s.split_once('-') {
        Some((days, rest)) => (days.parse::<u64>().ok()?, rest),
        None => (0, s),
    };

    let mut parts = hms.split(':').rev();
    let secs: u64 = parts.next()?.parse().ok()?;
    let mins: u64 = parts.next()?.parse().ok()?;
    let hours: u64 = match parts.next() {
        Some(hours) => hours.parse().ok()?,
        None => 0,
    };
    // A fourth `:`-separated component isn't a format `ps` produces.
    if parts.next().is_some() || secs >= 60 || mins >= 60 {
        return None;
    }

    Some(((days * 24 + hours) * 60 + mins) * 60 + secs)
}

/// Orders `procs` in place. Ties always break on ascending pid — the sort is
/// stable, but the *input* order is whatever `ps` happened to emit this run,
/// so without an explicit tiebreak equal rows (every idle process shares
/// `0.0` CPU) would shuffle every two seconds.
pub fn sort_processes(procs: &mut [ProcessInfo], key: ProcessSortKey, ascending: bool) {
    procs.sort_by(|a, b| {
        let ord = match key {
            ProcessSortKey::Pid => a.pid.cmp(&b.pid),
            ProcessSortKey::User => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
            // `total_cmp` rather than `partial_cmp().unwrap()`: `ps` won't
            // emit a NaN, but a panic in a sort comparator is not the way to
            // find out if it ever does.
            ProcessSortKey::Cpu => a.cpu.total_cmp(&b.cpu),
            ProcessSortKey::Mem => a.mem.total_cmp(&b.mem),
            ProcessSortKey::Rss => a.rss_kib.cmp(&b.rss_kib),
            ProcessSortKey::Time => {
                // Unparsable elapsed times sink in *both* directions:
                // reversing them would float the rows we know least about to
                // the top, which is the opposite of useful.
                return match (a.etime_secs, b.etime_secs) {
                    (None, None) => a.pid.cmp(&b.pid),
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (Some(x), Some(y)) => {
                        let ord = x.cmp(&y);
                        let ord = if ascending { ord } else { ord.reverse() };
                        ord.then(a.pid.cmp(&b.pid))
                    }
                };
            }
            ProcessSortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        };
        let ord = if ascending { ord } else { ord.reverse() };
        ord.then(a.pid.cmp(&b.pid))
    });
}

/// Where the cursor belongs after a refresh replaced the snapshot: on the
/// same pid if it's still there, otherwise clamped to the new length.
///
/// Following the pid rather than the index is what makes a 2-second refresh
/// usable at all — under a CPU sort, rows reorder constantly, and an
/// index-anchored cursor would drift onto a different process between the
/// moment you aim at one and the moment you press `x`. The clamp (rather
/// than a reset to 0) keeps the cursor near where it was when the process
/// under it exits.
pub fn reanchor_cursor(new: &[ProcessInfo], anchor_pid: Option<u32>, old_cursor: usize) -> usize {
    if new.is_empty() {
        return 0;
    }
    if let Some(pid) = anchor_pid
        && let Some(idx) = new.iter().position(|p| p.pid == pid)
    {
        return idx;
    }
    old_cursor.min(new.len() - 1)
}

/// Sends `signal` to `pid`.
///
/// The `i32::try_from(...).filter(|p| *p > 1)` is not defensive
/// pedantry — `kill(2)` reads non-positive pids as broadcasts. A `u32` above
/// `i32::MAX` cast straight to `pid_t` comes out negative, which means "the
/// whole process group"; `0` means "my own process group", i.e. the shell
/// that launched ozzel and everything in it. `1` is init. None of those are
/// things a file manager's `x` key may do, so they're refused here as well
/// as in `App::execute_kill`, which is the layer that also knows to refuse
/// ozzel's own pid.
#[cfg(unix)]
pub fn send_signal(pid: u32, signal: Signal) -> Result<()> {
    use anyhow::Context;

    let pid_t = i32::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .with_context(|| format!("refusing to signal pid {pid}"))?;

    // SAFETY: `kill` has no preconditions beyond a valid signal number;
    // `pid_t` is checked positive above, so this can only ever address one
    // process. Failure is reported through errno, not memory.
    let rc = unsafe { libc::kill(pid_t, signal.raw()) };
    if rc == 0 {
        return Ok(());
    }
    Err(std::io::Error::last_os_error())
        .with_context(|| format!("failed to send {} to pid {pid}", signal.label()))
}

#[cfg(not(unix))]
pub fn send_signal(_pid: u32, _signal: Signal) -> Result<()> {
    anyhow::bail!("sending signals is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with(
        pid: u32,
        name: &str,
        cpu: f32,
        rss_kib: u64,
        etime_secs: Option<u64>,
    ) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid: 1,
            user: "masaki".to_string(),
            cpu,
            mem: 0.0,
            rss_kib,
            state: "S".to_string(),
            etime: "00:01".to_string(),
            etime_secs,
            command: name.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn parses_a_typical_linux_ps_line_into_every_column() {
        let line = "  1234   1 root      12.5  1.8    294912 Ssl+ 02-18:32:57 /usr/lib/systemd/systemd --user";
        let (procs, skipped) = parse_ps_output(line);
        assert_eq!(skipped, 0);
        assert_eq!(procs.len(), 1);
        let p = &procs[0];
        assert_eq!(p.pid, 1234);
        assert_eq!(p.ppid, 1);
        assert_eq!(p.user, "root");
        assert_eq!(p.cpu, 12.5);
        assert_eq!(p.mem, 1.8);
        assert_eq!(p.rss_kib, 294912);
        assert_eq!(p.state, "Ssl+");
        assert_eq!(p.etime, "02-18:32:57");
        assert_eq!(p.etime_secs, Some(239577));
        assert_eq!(p.command, "/usr/lib/systemd/systemd --user");
        assert_eq!(p.name, "systemd");
    }

    #[test]
    fn parses_a_macos_ps_line_whose_command_is_an_absolute_path() {
        let (procs, skipped) =
            parse_ps_output("    1     0 root  0.4  0.1  21440 Ss   02-18:45:48 /sbin/launchd");
        assert_eq!(skipped, 0);
        assert_eq!(procs[0].pid, 1);
        assert_eq!(procs[0].ppid, 0);
        assert_eq!(procs[0].name, "launchd");
    }

    #[test]
    fn derives_the_display_name_from_argv_zeros_basename() {
        assert_eq!(command_name("/usr/bin/ssh -T git@example.com"), "ssh");
        assert_eq!(command_name("zsh"), "zsh");
        assert_eq!(command_name("/usr/bin/"), "/usr/bin/");
    }

    #[test]
    fn strips_the_brackets_from_a_linux_kernel_thread_name() {
        assert_eq!(command_name("[kthreadd]"), "kthreadd");
        let (procs, _) = parse_ps_output("   2     0 root  0.0  0.0  0 S    01:02:03 [kthreadd]");
        assert_eq!(procs[0].name, "kthreadd");
        assert_eq!(procs[0].command, "[kthreadd]");
    }

    #[test]
    fn keeps_a_command_line_containing_spaces_intact_as_the_last_field() {
        let (procs, _) =
            parse_ps_output("99 1 masaki 0.0 0.0 100 S 00:10 /bin/sh -c 'echo  a   b' && sleep 5");
        assert_eq!(procs[0].command, "/bin/sh -c 'echo  a   b' && sleep 5");
        assert_eq!(procs[0].name, "sh");
    }

    #[test]
    fn accepts_a_line_whose_command_field_is_empty() {
        let (procs, skipped) = parse_ps_output("99 1 masaki 0.0 0.0 100 S 00:10");
        assert_eq!(skipped, 0);
        assert_eq!(procs[0].command, "");
        assert_eq!(procs[0].name, "?");
    }

    #[test]
    fn skips_a_header_line_whose_first_column_is_not_a_numeric_pid() {
        let text = "  PID  PPID USER  %CPU %MEM   RSS STAT     ELAPSED COMMAND\n\
                    99 1 masaki 0.0 0.0 100 S 00:10 zsh";
        let (procs, skipped) = parse_ps_output(text);
        assert_eq!(skipped, 1);
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].pid, 99);
    }

    #[test]
    fn skips_a_line_with_too_few_columns_and_counts_it_as_skipped() {
        let (procs, skipped) = parse_ps_output("99 1 masaki 0.0");
        assert!(procs.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn ignores_blank_and_whitespace_only_lines_without_counting_them() {
        let (procs, skipped) = parse_ps_output("\n   \n99 1 masaki 0.0 0.0 100 S 00:10 zsh\n\n");
        assert_eq!(skipped, 0);
        assert_eq!(procs.len(), 1);
    }

    #[test]
    fn truncates_a_pathologically_long_command_line_to_the_cap() {
        let long = "x".repeat(MAX_COMMAND_LEN * 3);
        let (procs, _) = parse_ps_output(&format!("99 1 masaki 0.0 0.0 100 S 00:10 /bin/{long}"));
        assert_eq!(procs[0].command.len(), MAX_COMMAND_LEN);
        // The name is derived from the already-cut command line, so it's
        // bounded too — nothing downstream sees the original 1536 bytes.
        assert_eq!(procs[0].name.len(), MAX_COMMAND_LEN - "/bin/".len());
    }

    #[test]
    fn truncating_a_command_line_never_splits_a_multibyte_character() {
        // A cut at exactly MAX_COMMAND_LEN would land mid-character here.
        let padded = format!("{}あ", "x".repeat(MAX_COMMAND_LEN - 1));
        let (procs, _) = parse_ps_output(&format!("99 1 masaki 0.0 0.0 100 S 00:10 {padded}"));
        assert_eq!(procs[0].command.len(), MAX_COMMAND_LEN - 1);
    }

    #[test]
    fn parses_etime_in_mm_ss_hh_mm_ss_and_dd_hh_mm_ss_forms() {
        assert_eq!(parse_etime("46:51"), Some(2811));
        assert_eq!(parse_etime("22:46:03"), Some(81963));
        assert_eq!(parse_etime("02-18:32:57"), Some(239577));
        assert_eq!(parse_etime("00:00"), Some(0));
    }

    #[test]
    fn an_unparsable_etime_becomes_none() {
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("-"), None);
        assert_eq!(parse_etime("12"), None);
        assert_eq!(parse_etime("1:2:3:4"), None);
        assert_eq!(parse_etime("00:99"), None);
        assert_eq!(parse_etime("ab:cd"), None);
    }

    #[test]
    fn sorting_by_cpu_descending_puts_the_busiest_process_first() {
        let mut procs = vec![
            proc_with(1, "a", 0.5, 10, Some(1)),
            proc_with(2, "b", 12.0, 10, Some(1)),
            proc_with(3, "c", 3.0, 10, Some(1)),
        ];
        sort_processes(&mut procs, ProcessSortKey::Cpu, false);
        assert_eq!(
            procs.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
    }

    #[test]
    fn sorting_by_rss_orders_numerically_not_lexically() {
        let mut procs = vec![
            proc_with(1, "a", 0.0, 9, Some(1)),
            proc_with(2, "b", 0.0, 100, Some(1)),
        ];
        sort_processes(&mut procs, ProcessSortKey::Rss, true);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn sorting_by_name_is_case_insensitive_and_breaks_ties_by_pid() {
        let mut procs = vec![
            proc_with(9, "Zsh", 0.0, 0, Some(1)),
            proc_with(3, "bash", 0.0, 0, Some(1)),
            proc_with(1, "bash", 0.0, 0, Some(1)),
        ];
        sort_processes(&mut procs, ProcessSortKey::Name, true);
        assert_eq!(
            procs.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![1, 3, 9]
        );
    }

    #[test]
    fn equal_rows_keep_a_stable_pid_order_regardless_of_direction() {
        let mut procs = vec![
            proc_with(7, "a", 0.0, 0, Some(1)),
            proc_with(2, "b", 0.0, 0, Some(1)),
        ];
        sort_processes(&mut procs, ProcessSortKey::Cpu, false);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![2, 7]);
        sort_processes(&mut procs, ProcessSortKey::Cpu, true);
        assert_eq!(procs.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![2, 7]);
    }

    #[test]
    fn processes_with_an_unparsable_etime_sort_last_in_both_directions() {
        let mut procs = vec![
            proc_with(1, "a", 0.0, 0, None),
            proc_with(2, "b", 0.0, 0, Some(50)),
            proc_with(3, "c", 0.0, 0, Some(10)),
        ];
        sort_processes(&mut procs, ProcessSortKey::Time, true);
        assert_eq!(procs.last().unwrap().pid, 1);
        sort_processes(&mut procs, ProcessSortKey::Time, false);
        assert_eq!(procs.last().unwrap().pid, 1);
        assert_eq!(procs[0].pid, 2);
    }

    #[test]
    fn the_default_direction_is_descending_for_usage_columns_only() {
        assert!(default_ascending(ProcessSortKey::Pid));
        assert!(default_ascending(ProcessSortKey::User));
        assert!(default_ascending(ProcessSortKey::Name));
        assert!(!default_ascending(ProcessSortKey::Cpu));
        assert!(!default_ascending(ProcessSortKey::Mem));
        assert!(!default_ascending(ProcessSortKey::Rss));
        assert!(!default_ascending(ProcessSortKey::Time));
    }

    #[test]
    fn reanchor_cursor_follows_the_same_pid_when_rows_move() {
        let procs = vec![
            proc_with(5, "a", 0.0, 0, Some(1)),
            proc_with(6, "b", 0.0, 0, Some(1)),
            proc_with(7, "c", 0.0, 0, Some(1)),
        ];
        assert_eq!(reanchor_cursor(&procs, Some(7), 0), 2);
    }

    #[test]
    fn reanchor_cursor_clamps_to_the_new_length_when_the_anchor_pid_is_gone() {
        let procs = vec![proc_with(5, "a", 0.0, 0, Some(1))];
        assert_eq!(reanchor_cursor(&procs, Some(999), 4), 0);
        assert_eq!(reanchor_cursor(&[], Some(5), 3), 0);
        assert_eq!(reanchor_cursor(&procs, None, 0), 0);
    }

    #[test]
    fn send_signal_refuses_pid_zero_so_it_can_never_hit_the_whole_process_group() {
        assert!(send_signal(0, Signal::Term).is_err());
    }

    #[test]
    fn send_signal_refuses_pid_one() {
        assert!(send_signal(1, Signal::Kill).is_err());
    }

    #[test]
    fn send_signal_refuses_a_pid_that_does_not_fit_in_pid_t() {
        assert!(send_signal(u32::MAX, Signal::Term).is_err());
    }

    /// The only test that actually signals something: spawns a throwaway
    /// child and terminates it. Everything above proves the *guards* work,
    /// which is worth nothing if the call itself doesn't.
    #[test]
    #[cfg(unix)]
    fn send_signal_terminates_a_child_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        send_signal(child.id(), Signal::Term).expect("SIGTERM to our own child");

        let status = child.wait().expect("reap the child");
        assert!(
            !status.success(),
            "a signalled process must not report success: {status:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn signalling_a_pid_that_does_not_exist_is_an_error_rather_than_a_panic() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap the child");

        // Reaped, so the pid is gone (barring an immediate reuse, which
        // would need the whole pid space to wrap between these two lines).
        assert!(send_signal(pid, Signal::Term).is_err());
    }
}
