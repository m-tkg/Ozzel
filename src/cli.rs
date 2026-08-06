//! Command-line surface: `clap`'s `Cli`/`Command` definitions, resolving the
//! two positional directory arguments, and the `--cwd-file` write-back
//! decision/write itself.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};

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
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    /// Starting directory for the left pane (defaults to the current directory)
    pub left_dir: Option<PathBuf>,
    /// Starting directory for the right pane (defaults to the current directory)
    pub right_dir: Option<PathBuf>,
    /// Path to write the focused pane's directory to on quit (see
    /// `write_cwd_file`) — pair with a shell wrapper function that `cd`s
    /// into it afterward (README documents one for zsh/bash and
    /// PowerShell). Gated by `config.quit_cd` (default on): when this flag
    /// is absent, nothing is ever written, regardless of that setting.
    #[arg(long)]
    pub cwd_file: Option<PathBuf>,
}

// A `command` token consumes what would otherwise be `left_dir`'s
// positional slot — see `Cli`'s comment above — which is why `ozzel
// update` always means "run the update subcommand," never "open a
// directory named `update`" (use `./update` for that). Plain comment for
// the same --help-leakage reason as `Cli`'s.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Update ozzel to the latest version on GitHub (`cargo install --git`)
    #[command(after_help = "\
Compares against the version on GitHub's main branch and, if it is newer,
runs `cargo install --git https://github.com/m-tkg/Ozzel --force`.
The build takes a minute or two. Requires cargo.")]
    Update {
        /// Reinstall even if the remote version matches the current one
        #[arg(long)]
        force: bool,
    },
}

/// Resolves a startup directory argument, falling back to the current
/// working directory when omitted, and failing loudly when an explicitly
/// requested directory does not exist.
pub fn resolve_startup_dir(dir: Option<PathBuf>) -> anyhow::Result<PathBuf> {
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

/// Whether `main` should write `--cwd-file` at all: only when both the flag
/// was given (`cwd_file.is_some()`) *and* `quit_cd` didn't opt out. Kept as
/// a pure, standalone predicate (rather than inlined into the `if` at the
/// call site) purely so it's directly unit-testable without going through
/// `Cli::parse`/a real filesystem write.
pub fn should_write_cwd_file(quit_cd: bool, cwd_file: Option<&PathBuf>) -> bool {
    quit_cd && cwd_file.is_some()
}

/// Writes `cwd`'s path (as `Path::display` would render it) to `path`,
/// overwriting any existing content — the shell wrapper function
/// (README's `oz()`) reads this back and `cd`s into it. A plain
/// `std::fs::write`, no atomic-rename dance: this is a single small write
/// to a fresh `mktemp` file the wrapper itself owns for the run's
/// lifetime, not a file anything else could be concurrently reading.
pub fn write_cwd_file(path: &Path, cwd: &Path) -> io::Result<()> {
    std::fs::write(path, cwd.display().to_string())
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
