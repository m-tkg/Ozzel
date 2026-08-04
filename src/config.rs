//! TOML configuration. ozzel is a terminal/CLI tool, so on macOS and Linux
//! it follows the CLI-tool convention (`$XDG_CONFIG_HOME/ozzel/config.toml`,
//! falling back to `~/.config/ozzel/config.toml`) rather than Apple's GUI-
//! app convention (`~/Library/Application Support`); Windows still uses
//! `%APPDATA%\ozzel\` via `directories::ProjectDirs`, which already matches
//! CLI-tool norms there. A missing file falls back to defaults; a
//! *malformed* file is a hard error (never silently ignored) since that
//! usually means the user just made a typo they'd want to know about.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeleteBehavior {
    #[default]
    Trash,
    Permanent,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub delete_behavior: DeleteBehavior,
    /// Directory `GoHome` (`~`/`H`) jumps to; falls back to the OS home
    /// directory when unset.
    pub home: Option<PathBuf>,
    /// Editor command `OpenEditor` (`e`) runs; falls back to `$EDITOR`.
    pub editor: Option<String>,
    pub keys: HashMap<String, String>,
}

/// Path to the config file, if this platform has a resolvable config dir.
pub fn config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        directories::ProjectDirs::from("", "", "ozzel")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }
    #[cfg(not(windows))]
    {
        let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home_dir = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
        unix_config_path(xdg_config_home, home_dir)
    }
}

/// Pure XDG-style resolution used on macOS and Linux: `$XDG_CONFIG_HOME`
/// when it's set to an absolute path, otherwise `~/.config`. Kept as a
/// standalone function (rather than inlined env/home lookups) purely so
/// tests can exercise the fallback logic without mutating process-global
/// environment state.
#[cfg_attr(
    windows,
    allow(dead_code, reason = "only used on the unix config_path path")
)]
fn unix_config_path(
    xdg_config_home: Option<PathBuf>,
    home_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    let base = xdg_config_home
        .filter(|p| p.is_absolute())
        .or_else(|| home_dir.map(|home| home.join(".config")))?;
    Some(base.join("ozzel").join("config.toml"))
}

/// Loads the config file, falling back to defaults when it doesn't exist.
/// Returns an error (with the file path and parse detail) if it exists but
/// fails to parse.
pub fn load() -> Result<Config> {
    let Some(path) = config_path() else {
        return Ok(Config::default());
    };
    if !path.exists() {
        return Ok(Config::default());
    }

    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_toml_parses_cleanly() {
        // Guards against README/examples drift: the shipped example must
        // stay valid TOML that deserializes into `Config`, even with every
        // optional line commented out.
        let text =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/config.toml"))
                .expect("examples/config.toml must exist");
        let config: Config = toml::from_str(&text).expect("examples/config.toml must parse");
        assert_eq!(config.delete_behavior, DeleteBehavior::Trash);
    }

    #[test]
    fn defaults_are_trash_and_empty_keys() {
        let config = Config::default();
        assert_eq!(config.delete_behavior, DeleteBehavior::Trash);
        assert!(config.keys.is_empty());
        assert!(config.home.is_none());
        assert!(config.editor.is_none());
    }

    #[test]
    fn parses_a_full_config_toml() {
        let toml_text = r#"
            delete_behavior = "permanent"
            home = "/tmp"
            editor = "vim"

            [keys]
            "C-c" = "copy"
            "C-x" = "none"
        "#;
        let config: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(config.delete_behavior, DeleteBehavior::Permanent);
        assert_eq!(config.home, Some(PathBuf::from("/tmp")));
        assert_eq!(config.editor, Some("vim".to_string()));
        assert_eq!(config.keys.get("C-c"), Some(&"copy".to_string()));
    }

    #[test]
    fn malformed_toml_is_an_error_not_defaults() {
        let bad = "delete_behavior = [not valid";
        let result: Result<Config, _> = toml::from_str(bad);
        assert!(result.is_err());
    }

    #[test]
    fn xdg_config_home_takes_priority_when_absolute() {
        let path = unix_config_path(
            Some(PathBuf::from("/custom/xdg")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(path, Some(PathBuf::from("/custom/xdg/ozzel/config.toml")));
    }

    #[test]
    fn falls_back_to_home_dot_config_when_xdg_unset() {
        let path = unix_config_path(None, Some(PathBuf::from("/home/user")));
        assert_eq!(
            path,
            Some(PathBuf::from("/home/user/.config/ozzel/config.toml"))
        );
    }

    #[test]
    fn falls_back_to_home_dot_config_when_xdg_is_relative() {
        // A relative XDG_CONFIG_HOME is invalid per the XDG spec; ignore it
        // rather than joining it onto nothing meaningful.
        let path = unix_config_path(
            Some(PathBuf::from("relative/path")),
            Some(PathBuf::from("/home/user")),
        );
        assert_eq!(
            path,
            Some(PathBuf::from("/home/user/.config/ozzel/config.toml"))
        );
    }

    #[test]
    fn none_when_neither_xdg_nor_home_resolve() {
        assert_eq!(unix_config_path(None, None), None);
    }
}
