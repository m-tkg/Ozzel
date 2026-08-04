//! TOML configuration: `~/.config/ozzel/config.toml` (Linux/mac) or
//! `%APPDATA%\ozzel\config.toml` (Windows) via `directories::ProjectDirs`.
//! A missing file falls back to defaults; a *malformed* file is a hard
//! error (never silently ignored) since that usually means the user just
//! made a typo they'd want to know about.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeleteBehavior {
    #[default]
    Trash,
    Permanent,
}

/// `home`/`editor` are parsed now so the config file's shape is stable,
/// even though nothing consumes them until later phases (jump-to-home key,
/// external editor launch).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
#[allow(dead_code, reason = "home/editor land in a later phase")]
pub struct Config {
    pub delete_behavior: DeleteBehavior,
    pub home: Option<PathBuf>,
    pub editor: Option<String>,
    pub keys: HashMap<String, String>,
}

/// Path to the config file, if this platform has a resolvable config dir.
pub fn config_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "ozzel").map(|dirs| dirs.config_dir().join("config.toml"))
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
}
