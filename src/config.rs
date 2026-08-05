//! TOML configuration. ozzel is a terminal/CLI tool, so on macOS and Linux
//! it follows the CLI-tool convention (`$XDG_CONFIG_HOME/ozzel/config.toml`,
//! falling back to `~/.config/ozzel/config.toml`) rather than Apple's GUI-
//! app convention (`~/Library/Application Support`); Windows still uses
//! `%APPDATA%\ozzel\` via `directories::ProjectDirs`, which already matches
//! CLI-tool norms there. A missing file falls back to defaults; a
//! *malformed* file is a hard error (never silently ignored) since that
//! usually means the user just made a typo they'd want to know about.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::{Deserialize, Deserializer};

use crate::color;

/// The starter template written into a missing config file (see
/// `ensure_config_file_exists`) — literally `examples/config.toml`,
/// embedded via `include_str!` so the shipped example and the one a
/// first-time user gets from `,` (edit_config) can never drift apart.
const CONFIG_TEMPLATE: &str = include_str!("../examples/config.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeleteBehavior {
    #[default]
    Trash,
    Permanent,
}

/// `#90EE90` ("light green" in CSS terms) — a soft pastel that reads
/// clearly as a cursor highlight without the harshness of pure ANSI green.
/// Requires a truecolor-capable terminal; on one that only supports the
/// 16-color ANSI palette, set `cursor = "light_green"` in `[colors]`
/// instead for the closest built-in approximation.
fn default_cursor_color() -> Color {
    Color::Rgb(0x90, 0xEE, 0x90)
}

/// The cursor row in the *inactive* pane uses a neutral white by default,
/// so a glance at the screen shows exactly one green (active) cursor and
/// one white (inactive) cursor rather than two identically-colored ones.
fn default_cursor_inactive_color() -> Color {
    Color::White
}

fn default_dim_inactive() -> bool {
    true
}

/// Cyan (水色) for directories — the dyna-filer-esque convention this app
/// otherwise follows throughout.
fn default_directory_color() -> Color {
    Color::Cyan
}

/// Red for hidden entries — wins over `executable`/`directory` when an
/// entry is more than one of these at once (see `entry_type_color`).
fn default_hidden_color() -> Color {
    Color::Red
}

/// Yellow for executable files. Collides visually with the existing
/// "marked" row color (also yellow) — resolved by giving `marked` priority
/// over any type color (see `ui::pane_view::row_style`), so this is only
/// ever seen on an unmarked row.
fn default_executable_color() -> Color {
    Color::Yellow
}

/// The permissions column (`drwxr-xr-x` / a compact Windows fallback) is
/// shown by default; set `show_permissions = false` to reclaim the columns
/// for a wider name column instead.
fn default_show_permissions() -> bool {
    true
}

/// Mouse capture is enabled by default (click/wheel/drag — see
/// `App::handle_mouse`). Set `mouse = false` to leave the terminal's native
/// text selection usable instead (the standard TUI trade-off: with capture
/// on, hold Shift for native selection).
fn default_mouse() -> bool {
    true
}

/// Copy/Move confirm before spawning by default — same "ask before doing
/// something that touches the filesystem" posture as Delete. A top-level
/// `confirm_operations = false` skips that confirm when there's no
/// collision (a collision always confirms regardless of this setting).
fn default_confirm_operations() -> bool {
    true
}

/// Quit confirms by default when nothing is running ("Quit ozzel?"). A
/// top-level `confirm_quit = false` skips that confirm and quits
/// immediately in that case — but the *tasks-running* confirm ("N task(s)
/// running — quit anyway?") is unconditional and never affected by this
/// setting, since a detached worker thread gets killed outright if the
/// process exits while it's still writing.
fn default_confirm_quit() -> bool {
    true
}

/// `--cwd-file` is written on quit by default (when the flag was given at
/// all — see `main.rs`). A top-level `quit_cd = false` opts out entirely,
/// for a user who passes `--cwd-file` for some other integration and never
/// wants ozzel touching it.
fn default_quit_cd() -> bool {
    true
}

fn deserialize_color<'de, D>(deserializer: D) -> std::result::Result<Color, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    color::parse_color(&raw).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ColorsConfig {
    #[serde(
        deserialize_with = "deserialize_color",
        default = "default_cursor_color"
    )]
    pub cursor: Color,
    /// Cursor row color in whichever pane is *not* currently active.
    #[serde(
        deserialize_with = "deserialize_color",
        default = "default_cursor_inactive_color"
    )]
    pub cursor_inactive: Color,
    /// Whether the inactive pane's rows (including its cursor row) and its
    /// border/header render dimmed. The cursor row stays locatable even
    /// dimmed, since it's still the only row with a background fill.
    #[serde(default = "default_dim_inactive")]
    pub dim_inactive: bool,
    /// Non-cursor directory rows' fg color. Precedence when an entry is
    /// more than one of directory/hidden/executable at once: hidden wins
    /// over executable wins over directory (see `entry_type_color`).
    #[serde(
        deserialize_with = "deserialize_color",
        default = "default_directory_color"
    )]
    pub directory: Color,
    /// Non-cursor hidden-entry rows' fg color; highest precedence of the
    /// three type colors.
    #[serde(
        deserialize_with = "deserialize_color",
        default = "default_hidden_color"
    )]
    pub hidden: Color,
    /// Non-cursor executable-file rows' fg color (unix: any `x` bit on a
    /// regular file; Windows: a PATHEXT-ish extension — see
    /// `entry::is_executable`).
    #[serde(
        deserialize_with = "deserialize_color",
        default = "default_executable_color"
    )]
    pub executable: Color,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            cursor: default_cursor_color(),
            cursor_inactive: default_cursor_inactive_color(),
            dim_inactive: default_dim_inactive(),
            directory: default_directory_color(),
            hidden: default_hidden_color(),
            executable: default_executable_color(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub delete_behavior: DeleteBehavior,
    /// Directory `GoHome` (`~`) jumps to; falls back to the OS home
    /// directory when unset.
    pub home: Option<PathBuf>,
    /// Editor command `OpenEditor` (`e`) runs; falls back to `$EDITOR`.
    pub editor: Option<String>,
    /// Whether Copy/Move show a confirm dialog before spawning when there's
    /// no filename collision (a collision always confirms regardless).
    /// Kept top-level next to `delete_behavior` rather than under a
    /// separate `[behavior]` section, so existing configs with
    /// `delete_behavior` at the top level keep working unchanged.
    #[serde(default = "default_confirm_operations")]
    pub confirm_operations: bool,
    /// Whether `Quit` shows a "Quit ozzel?" confirm dialog when nothing is
    /// running (the tasks-running confirm is unconditional — see
    /// `default_confirm_quit`). Kept top-level next to
    /// `confirm_operations` for the same "don't break existing configs"
    /// reason.
    #[serde(default = "default_confirm_quit")]
    pub confirm_quit: bool,
    /// Whether quitting writes the focused pane's cwd to `--cwd-file` (if
    /// that flag was given at all — this only opts *out* when it was).
    /// See `main.rs`'s `write_cwd_file` and the README's shell wrapper.
    #[serde(default = "default_quit_cd")]
    pub quit_cd: bool,
    /// Whether the permissions column (`drwxr-xr-x` / Windows fallback)
    /// renders at all — see `ui::pane_view`. Dropped automatically under a
    /// narrow pane width regardless of this setting; this is the "never
    /// show it even when there's room" opt-out.
    #[serde(default = "default_show_permissions")]
    pub show_permissions: bool,
    /// Whether mouse capture is enabled at startup (`EnableMouseCapture`) —
    /// see `main.rs`'s `TerminalGuard` and `App::handle_mouse`. `false`
    /// leaves the terminal's native text selection usable instead.
    #[serde(default = "default_mouse")]
    pub mouse: bool,
    /// `combo -> action_name`; `"none"` unbinds. Applied to the default
    /// keymap first (see `App::new`).
    pub keys: HashMap<String, String>,
    /// `action_name -> [combo, ...]`; the inverted, ergonomic form for
    /// binding several keys to one action at once (e.g.
    /// `rename = ["r", "S-r"]`). Applied *after* `[keys]`, so a combo
    /// listed here wins if both sections mention it. There's no `"none"`
    /// here — unbinding stays `[keys]`'s job.
    pub bindings: HashMap<String, Vec<String>>,
    pub colors: ColorsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            delete_behavior: DeleteBehavior::default(),
            home: None,
            editor: None,
            confirm_operations: default_confirm_operations(),
            confirm_quit: default_confirm_quit(),
            quit_cd: default_quit_cd(),
            show_permissions: default_show_permissions(),
            mouse: default_mouse(),
            keys: HashMap::new(),
            bindings: HashMap::new(),
            colors: ColorsConfig::default(),
        }
    }
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
    load_from_path(&path)
}

/// Core of `load`, taking the path explicitly — factored out so both
/// `load` (the real, XDG-resolved location) and `App::reload_config`
/// (which needs an injectable path for its own testability, since the
/// live-reload it drives has to point at a tempdir file in tests) share
/// exactly one parse path.
pub fn load_from_path(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }

    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}

/// Ensures `path`'s parent directories and the file itself exist, writing
/// `CONFIG_TEMPLATE` if the file is missing (never overwrites an existing
/// one). Called by `App::begin_edit_config` right before opening the
/// config file in an editor, so a first-time user gets a real, documented
/// starting point instead of an empty file (or an error from the editor
/// failing to open a nonexistent path whose parent directory doesn't even
/// exist yet).
pub fn ensure_config_file_exists(path: &Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, CONFIG_TEMPLATE)
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
    fn example_config_bindings_section_matches_the_generated_defaults() {
        // examples/config.toml's [bindings] block is meant to be the exact,
        // hand-committed output of `Keymap::to_bindings_toml()` (see its
        // doc comment) — this guards against it ever drifting from
        // whatever `Keymap::default_dyna()` actually builds, comparing
        // parsed maps (not raw text) so comments/whitespace/line-order in
        // the checked-in file are free to differ.
        #[derive(Deserialize)]
        struct BindingsOnly {
            bindings: HashMap<String, Vec<String>>,
        }

        let text =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/config.toml"))
                .expect("examples/config.toml must exist");
        let from_file: BindingsOnly =
            toml::from_str(&text).expect("examples/config.toml must parse");

        let generated_text = crate::keymap::Keymap::default_dyna().to_bindings_toml();
        let generated: BindingsOnly = toml::from_str(&generated_text)
            .expect("Keymap::to_bindings_toml()'s own output must parse");

        assert_eq!(
            from_file.bindings, generated.bindings,
            "examples/config.toml's [bindings] section has drifted from \
             Keymap::default_dyna() — regenerate it from \
             Keymap::default_dyna().to_bindings_toml()'s real output"
        );
    }

    #[test]
    fn defaults_are_trash_and_empty_keys() {
        let config = Config::default();
        assert_eq!(config.delete_behavior, DeleteBehavior::Trash);
        assert!(config.keys.is_empty());
        assert!(config.home.is_none());
        assert!(config.editor.is_none());
        assert_eq!(config.colors.cursor, Color::Rgb(0x90, 0xEE, 0x90));
        assert_eq!(config.colors.cursor_inactive, Color::White);
        assert!(config.colors.dim_inactive);
        assert!(config.confirm_operations);
        assert!(config.confirm_quit);
        assert!(config.quit_cd);
        assert!(config.show_permissions);
        assert!(config.mouse);
        assert_eq!(config.colors.directory, Color::Cyan);
        assert_eq!(config.colors.hidden, Color::Red);
        assert_eq!(config.colors.executable, Color::Yellow);
        assert!(config.bindings.is_empty());
    }

    #[test]
    fn show_permissions_can_be_set_to_false() {
        let config: Config = toml::from_str("show_permissions = false").unwrap();
        assert!(!config.show_permissions);
    }

    #[test]
    fn mouse_can_be_set_to_false() {
        let config: Config = toml::from_str("mouse = false").unwrap();
        assert!(!config.mouse);
    }

    #[test]
    fn colors_directory_hidden_executable_accept_named_colors() {
        let config: Config = toml::from_str(
            "[colors]\ndirectory = \"blue\"\nhidden = \"magenta\"\nexecutable = \"green\"",
        )
        .unwrap();
        assert_eq!(config.colors.directory, Color::Blue);
        assert_eq!(config.colors.hidden, Color::Magenta);
        assert_eq!(config.colors.executable, Color::Green);
    }

    #[test]
    fn confirm_quit_can_be_set_to_false() {
        let config: Config = toml::from_str("confirm_quit = false").unwrap();
        assert!(!config.confirm_quit);
    }

    #[test]
    fn confirm_quit_absent_defaults_to_true() {
        let config: Config = toml::from_str("delete_behavior = \"trash\"").unwrap();
        assert!(config.confirm_quit);
    }

    #[test]
    fn quit_cd_can_be_set_to_false() {
        let config: Config = toml::from_str("quit_cd = false").unwrap();
        assert!(!config.quit_cd);
    }

    #[test]
    fn quit_cd_absent_defaults_to_true() {
        let config: Config = toml::from_str("delete_behavior = \"trash\"").unwrap();
        assert!(config.quit_cd);
    }

    #[test]
    fn confirm_operations_can_be_set_to_false() {
        let config: Config = toml::from_str("confirm_operations = false").unwrap();
        assert!(!config.confirm_operations);
    }

    #[test]
    fn confirm_operations_absent_defaults_to_true() {
        let config: Config = toml::from_str("delete_behavior = \"trash\"").unwrap();
        assert!(config.confirm_operations);
    }

    #[test]
    fn bindings_section_parses_action_to_key_array() {
        let toml_text = r#"
            [bindings]
            rename = ["r", "S-r"]
            quit = ["q", "C-c"]
        "#;
        let config: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(
            config.bindings.get("rename"),
            Some(&vec!["r".to_string(), "S-r".to_string()])
        );
        assert_eq!(
            config.bindings.get("quit"),
            Some(&vec!["q".to_string(), "C-c".to_string()])
        );
    }

    #[test]
    fn bindings_section_absent_is_empty() {
        let config: Config = toml::from_str("delete_behavior = \"trash\"").unwrap();
        assert!(config.bindings.is_empty());
    }

    #[test]
    fn colors_section_absent_falls_back_to_default_cursor_color() {
        let config: Config = toml::from_str("delete_behavior = \"trash\"").unwrap();
        assert_eq!(config.colors.cursor, Color::Rgb(0x90, 0xEE, 0x90));
        assert_eq!(config.colors.cursor_inactive, Color::White);
        assert!(config.colors.dim_inactive);
    }

    #[test]
    fn colors_cursor_inactive_accepts_named_color() {
        let config: Config = toml::from_str("[colors]\ncursor_inactive = \"blue\"").unwrap();
        assert_eq!(config.colors.cursor_inactive, Color::Blue);
    }

    #[test]
    fn colors_cursor_inactive_accepts_hex_color() {
        let config: Config = toml::from_str("[colors]\ncursor_inactive = \"#abcdef\"").unwrap();
        assert_eq!(config.colors.cursor_inactive, Color::Rgb(0xab, 0xcd, 0xef));
    }

    #[test]
    fn colors_cursor_inactive_invalid_value_is_a_hard_parse_error() {
        let result: Result<Config, _> =
            toml::from_str("[colors]\ncursor_inactive = \"not-a-color\"");
        assert!(result.is_err());
    }

    #[test]
    fn colors_dim_inactive_can_be_set_to_false() {
        let config: Config = toml::from_str("[colors]\ndim_inactive = false").unwrap();
        assert!(!config.colors.dim_inactive);
    }

    #[test]
    fn colors_dim_inactive_true_round_trips() {
        let config: Config = toml::from_str("[colors]\ndim_inactive = true").unwrap();
        assert!(config.colors.dim_inactive);
    }

    #[test]
    fn colors_cursor_accepts_named_color() {
        let config: Config = toml::from_str("[colors]\ncursor = \"red\"").unwrap();
        assert_eq!(config.colors.cursor, Color::Red);
    }

    #[test]
    fn colors_cursor_accepts_hex_color() {
        let config: Config = toml::from_str("[colors]\ncursor = \"#123456\"").unwrap();
        assert_eq!(config.colors.cursor, Color::Rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn colors_cursor_invalid_value_is_a_hard_parse_error() {
        let result: Result<Config, _> = toml::from_str("[colors]\ncursor = \"not-a-color\"");
        assert!(
            result.is_err(),
            "an invalid color must fail to parse, not silently fall back"
        );
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

    #[test]
    fn load_from_path_missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let config = load_from_path(&path).unwrap();
        assert_eq!(config.delete_behavior, DeleteBehavior::Trash);
    }

    #[test]
    fn load_from_path_parses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "delete_behavior = \"permanent\"").unwrap();
        let config = load_from_path(&path).unwrap();
        assert_eq!(config.delete_behavior, DeleteBehavior::Permanent);
    }

    #[test]
    fn load_from_path_malformed_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "delete_behavior = [not valid").unwrap();
        assert!(load_from_path(&path).is_err());
    }

    #[test]
    fn ensure_config_file_exists_creates_parent_dirs_and_writes_the_template() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path: the parent directory doesn't exist yet either,
        // exercising the create_dir_all half of the fix.
        let path = dir.path().join("nested").join("ozzel").join("config.toml");
        assert!(!path.exists());

        ensure_config_file_exists(&path).unwrap();

        assert!(path.exists());
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, CONFIG_TEMPLATE);
        // The template itself must still be valid, parseable config, same
        // guarantee `example_config_toml_parses_cleanly` checks for the
        // file on disk — this checks the *embedded* copy specifically.
        let _: Config = toml::from_str(&written).expect("template must parse as valid Config");
    }

    #[test]
    fn ensure_config_file_exists_does_not_overwrite_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "delete_behavior = \"permanent\"").unwrap();

        ensure_config_file_exists(&path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "delete_behavior = \"permanent\"");
    }
}
