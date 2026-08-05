//! Shared platform-directory resolution for `config.rs` (the config file's
//! location) and `persist.rs` (the history/bookmarks data directory) — both
//! needed "the ozzel-specific directory under some platform base," and had
//! near-identical resolution logic (env var -> absolute-path check -> home
//! fallback -> `ProjectDirs` on Windows) duplicated between them, differing
//! only in *which* env var/home-fallback-subpath pair (`XDG_CONFIG_HOME`/
//! `.config` vs `XDG_DATA_HOME`/`.local/share`) and *which*
//! `directories::ProjectDirs` accessor (`config_dir()`/`data_dir()`)
//! applies. This module holds the part that's actually identical; each
//! caller still owns its own env var name, fallback subpath, and (for
//! `config.rs`) the final `config.toml` join.

use std::path::PathBuf;

/// Pure XDG-style resolution used on macOS and Linux: `<env_dir>/ozzel`
/// when `env_dir` is set to an *absolute* path (the value `$XDG_CONFIG_HOME`/
/// `$XDG_DATA_HOME` would already have been read into), otherwise
/// `<home_dir>/<home_fallback>/ozzel`; `None` only when neither resolves
/// (no env var and no discoverable home dir). Pure (takes both inputs
/// explicitly rather than reading the environment/home dir itself) so
/// callers' tests can exercise every fallback branch without mutating
/// process-global environment state — `config::unix_config_path` and
/// `persist::unix_data_dir` each call this with their own env var's value
/// already read and their own `home_fallback` (`".config"`/
/// `".local/share"`).
#[cfg_attr(
    windows,
    allow(dead_code, reason = "only used on the unix config/data-dir path")
)]
pub(crate) fn unix_ozzel_dir(
    env_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    home_fallback: &str,
) -> Option<PathBuf> {
    let base = env_dir
        .filter(|p| p.is_absolute())
        .or_else(|| home_dir.map(|home| home.join(home_fallback)))?;
    Some(base.join("ozzel"))
}

/// Windows counterpart: `directories::ProjectDirs`' own base-dir resolution
/// (Windows has no XDG env-var convention to honor, so there's no "unix"
/// fallback logic to share here — just the one `ProjectDirs::from` call
/// both `config::config_path` and `persist::data_dir` used to make
/// separately). Callers pick `config_dir()`/`data_dir()` off the result.
#[cfg(windows)]
pub(crate) fn windows_project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "ozzel")
}

#[cfg(test)]
mod tests {
    // Every test below is `#[cfg(unix)]`, so on Windows this import would
    // itself be flagged unused under `-D warnings` — gate it the same way.
    #[cfg(unix)]
    use super::*;

    // Meaningful only on unix (a path like "/custom/xdg" isn't absolute on
    // Windows, and Windows never calls this function at all — see
    // `windows_project_dirs` above), so these are unix-only, same as the
    // `config`/`persist` tests that exercise this indirectly through their
    // own thin wrappers.
    #[test]
    #[cfg(unix)]
    fn env_dir_takes_priority_when_absolute() {
        let path = unix_ozzel_dir(
            Some(PathBuf::from("/custom/xdg")),
            Some(PathBuf::from("/home/user")),
            ".config",
        );
        assert_eq!(path, Some(PathBuf::from("/custom/xdg/ozzel")));
    }

    #[test]
    #[cfg(unix)]
    fn falls_back_to_home_and_fallback_subpath_when_env_dir_unset() {
        let path = unix_ozzel_dir(None, Some(PathBuf::from("/home/user")), ".config");
        assert_eq!(path, Some(PathBuf::from("/home/user/.config/ozzel")));
    }

    #[test]
    #[cfg(unix)]
    fn falls_back_to_home_when_env_dir_is_relative() {
        // A relative XDG_*_HOME is invalid per the XDG spec; ignore it
        // rather than joining it onto nothing meaningful.
        let path = unix_ozzel_dir(
            Some(PathBuf::from("relative/path")),
            Some(PathBuf::from("/home/user")),
            ".local/share",
        );
        assert_eq!(path, Some(PathBuf::from("/home/user/.local/share/ozzel")));
    }

    #[test]
    #[cfg(unix)]
    fn none_when_neither_env_dir_nor_home_resolve() {
        assert_eq!(unix_ozzel_dir(None, None, ".config"), None);
    }

    #[test]
    #[cfg(unix)]
    fn honors_whichever_home_fallback_subpath_the_caller_passes() {
        // The one parameter that actually differs between `config.rs`'s
        // and `persist.rs`'s callers — both fallback subpaths in one place.
        let home = Some(PathBuf::from("/home/user"));
        assert_eq!(
            unix_ozzel_dir(None, home.clone(), ".config"),
            Some(PathBuf::from("/home/user/.config/ozzel"))
        );
        assert_eq!(
            unix_ozzel_dir(None, home, ".local/share"),
            Some(PathBuf::from("/home/user/.local/share/ozzel"))
        );
    }
}
