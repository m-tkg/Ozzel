//! The `settings` action's full-frame configuration screen (raspi-config
//! style: category menu -> item list -> per-item editor), including
//! keybinding editing. `App::handle_settings_key` (app.rs) owns all the
//! actual key-dispatch/orchestration; this module owns the category/item
//! catalog, the pure per-item-type editor logic, and the `toml_edit`-based
//! surgical persistence (`save_*`/`remove_binding`/`add_binding` below).
//!
//! **Persistence model — save-on-change.** Every committed edit (a bool
//! flip, an enum/color/palette selection, `Enter` on a text field, a
//! keybinding add/remove) writes straight to the real config file via
//! `toml_edit` (format/comment-preserving — see each `save_*` function)
//! and then `App` re-runs the existing live-reload path
//! (`App::reload_config`) immediately, the same one `,` (edit_config)
//! already uses. There is no separate "pending changes" state to lose
//! track of or to explicitly discard — what's on screen always reflects
//! what's actually on disk and currently loaded, by construction. Chosen
//! over "save on close" specifically so a mistake is never more than one
//! edit away from being visible and undoable the same way, rather than
//! silently accumulating until the screen closes.
//!
//! **`[keys]` vs `[bindings]` precedence for the keybinding editor.**
//! `build_keymap` (app.rs) applies `[keys]` first, then `[bindings]` on
//! top — so `[bindings]` always wins on any combo both sections mention.
//! This module's `add_binding`/`remove_binding` implement a policy that
//! stays coherent with that:
//! - **Add** a combo to an action: always append it to that action's
//!   `[bindings]` array (creating the array/section if needed, and
//!   de-duplicating if it's somehow already there). `[bindings]`
//!   unconditionally wins, so this is the only way to *guarantee* the new
//!   binding actually takes effect regardless of any pre-existing
//!   `[keys]` entry for that same combo.
//! - **Remove** a combo from an action: if that exact combo is currently
//!   listed in `[bindings]` for this action, delete it from that array
//!   (and the array/key entirely, once empty) — this is "undo what a
//!   previous settings-screen add did." Otherwise, the combo must be
//!   coming from the *default* keymap or from a `[keys]` entry that maps
//!   it to this action — either way, writing `[keys].<combo> = "none"`
//!   unbinds it: a fresh write for that combo overwrites whatever action
//!   string (if any) was already there, which is exactly "prefer
//!   editing/removing that `[keys]` line too" for the case where one
//!   already existed.
//! - **Steal** (resolving a capture conflict): `remove_binding` on the
//!   losing action for that combo, then `add_binding` on the winning one
//!   — two ordinary writes in sequence, reusing the same two primitives
//!   rather than a bespoke third code path.

use std::path::Path;

use anyhow::{Context, Result};
use ratatui::style::Color;

use crate::action::Action;
use crate::config::{self, Config, DeleteBehavior, SizeFormat};
use crate::keymap::{self, KeyCombo, Keymap};

/// Top-level settings categories, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Behavior,
    Colors,
    Startup,
    Viewers,
    Keybindings,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Behavior,
        Category::Colors,
        Category::Startup,
        Category::Viewers,
        Category::Keybindings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Behavior => "Behavior",
            Category::Colors => "Colors",
            Category::Startup => "Startup / integration",
            Category::Viewers => "Extension viewers",
            Category::Keybindings => "Keybindings",
        }
    }
}

/// One editable key in `Category::Behavior`/`Colors`/`Startup`, in the
/// order each category's item list shows them. `Viewers`/`Keybindings`
/// are dynamic (sized by `config.viewers.len()`/`Action::ALL.len()`) and
/// have their own item-list logic in `App::handle_settings_key` instead
/// of going through this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The TOML key name — for `Colors` items, the key *within*
    /// `[colors]`; for everything else, the top-level key.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Bool,
    /// `delete_behavior`'s two variants.
    DeleteBehaviorEnum,
    /// `size_format`'s three variants — like `DeleteBehaviorEnum`, an
    /// explicit per-setting variant rather than a generic "choice" kind
    /// (there are only two enum settings; explicit stays auditable).
    SizeFormatEnum,
    Color,
    /// `home`/`editor` — free text, empty means "unset" (`None`).
    OptionalText,
}

pub const BEHAVIOR_ITEMS: [Item; 17] = [
    Item {
        key: "confirm_operations",
        label: "confirm_operations (confirm before copy/move)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "confirm_quit",
        label: "confirm_quit (confirm before quitting)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "quit_cd",
        label: "quit_cd (write --cwd-file on quit)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "mouse",
        label: "mouse (mouse support)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "delete_behavior",
        label: "delete_behavior (delete mode)",
        kind: ItemKind::DeleteBehaviorEnum,
    },
    Item {
        key: "show_permissions",
        label: "show_permissions (permissions column)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "dim_inactive",
        label: "dim_inactive (dim the inactive pane)",
        kind: ItemKind::Bool,
    },
    // Appended last (rather than next to its `g`-key relatives) so the
    // longstanding items above keep their positions — tests and muscle
    // memory both index into this array.
    Item {
        key: "file_search_incremental",
        label: "file_search_incremental (re-run the file-name search on every keystroke)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "command_line_interactive",
        label: "command_line_interactive (run : commands in an interactive shell, $SHELL -i)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "natural_sort",
        label: "natural_sort (sort digit runs as numbers: file2 < file10)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "cursor_wrap",
        label: "cursor_wrap (Up/Down wrap around at the list edges)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "size_format",
        label: "size_format (size column display)",
        kind: ItemKind::SizeFormatEnum,
    },
    Item {
        key: "show_git_status",
        label: "show_git_status (git status column + branch tag)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "auto_refresh",
        label: "auto_refresh (reload a pane when its directory changes externally)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "process_auto_refresh",
        label: "process_auto_refresh (re-run ps every 2s while the process manager is open)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "cursor_memory",
        label: "cursor_memory (restore the cursor position when revisiting a directory)",
        kind: ItemKind::Bool,
    },
    Item {
        key: "clear_on_suspend",
        label: "clear_on_suspend (blank the terminal while an external command runs)",
        kind: ItemKind::Bool,
    },
];

pub const COLOR_ITEMS: [Item; 5] = [
    Item {
        key: "cursor",
        label: "cursor (active-pane cursor color)",
        kind: ItemKind::Color,
    },
    Item {
        key: "cursor_inactive",
        label: "cursor_inactive (inactive-pane cursor color)",
        kind: ItemKind::Color,
    },
    Item {
        key: "directory",
        label: "directory (directory color)",
        kind: ItemKind::Color,
    },
    Item {
        key: "hidden",
        label: "hidden (hidden-file color)",
        kind: ItemKind::Color,
    },
    Item {
        key: "executable",
        label: "executable (executable-file color)",
        kind: ItemKind::Color,
    },
];

pub const STARTUP_ITEMS: [Item; 2] = [
    Item {
        key: "home",
        label: "home (where ~ jumps; OS home when unset)",
        kind: ItemKind::OptionalText,
    },
    Item {
        key: "editor",
        label: "editor (editor for e; $EDITOR when unset)",
        kind: ItemKind::OptionalText,
    },
];

/// The curated named-color palette the `Color` editor's select list
/// offers, alongside a free-text hex entry — every name
/// `crate::color::parse_color` itself accepts, so a selection here always
/// round-trips.
pub const COLOR_PALETTE: [(&str, Color); 16] = [
    ("black", Color::Black),
    ("red", Color::Red),
    ("green", Color::Green),
    ("yellow", Color::Yellow),
    ("blue", Color::Blue),
    ("magenta", Color::Magenta),
    ("cyan", Color::Cyan),
    ("gray", Color::Gray),
    ("dark_gray", Color::DarkGray),
    ("light_red", Color::LightRed),
    ("light_green", Color::LightGreen),
    ("light_yellow", Color::LightYellow),
    ("light_blue", Color::LightBlue),
    ("light_magenta", Color::LightMagenta),
    ("light_cyan", Color::LightCyan),
    ("white", Color::White),
];

/// Renders a `Color` the way the settings screen's item list / color
/// editor shows it: the palette's own name for an exact match, otherwise
/// `#RRGGBB` for `Rgb`, otherwise a debug fallback (`Indexed`/`Reset`
/// aren't reachable through `color::parse_color`-driven config values in
/// practice, but this stays total rather than panicking on one).
pub fn display_color(c: Color) -> String {
    if let Some((name, _)) = COLOR_PALETTE.iter().find(|(_, pc)| *pc == c) {
        return (*name).to_string();
    }
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        other => format!("{other:?}"),
    }
}

pub fn display_bool(value: bool) -> &'static str {
    if value { "ON" } else { "OFF" }
}

pub fn display_delete_behavior(value: DeleteBehavior) -> &'static str {
    match value {
        DeleteBehavior::Trash => "trash",
        DeleteBehavior::Permanent => "permanent",
    }
}

pub fn display_size_format(value: SizeFormat) -> &'static str {
    value.as_str()
}

/// Private — only `item_value_display` (this module) calls it.
fn display_optional_text(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "(unset)".to_string())
}

/// Reads `item`'s current value out of `config` as a display string, for
/// the item list (not the editor, which has its own richer per-type
/// state). `item.kind` alone is enough to know where to look
/// (`ItemKind::Color` is always a `[colors]` key, everything else is
/// top-level) — `category` isn't actually needed to disambiguate, but
/// every call site already has it on hand from the item list it's
/// rendering, so it's taken anyway for a symmetrical signature.
pub fn item_value_display(category: Category, item: &Item, config: &Config) -> String {
    let _ = category;
    match item.kind {
        ItemKind::Color => {
            let c = match item.key {
                "cursor" => config.colors.cursor,
                "cursor_inactive" => config.colors.cursor_inactive,
                "directory" => config.colors.directory,
                "hidden" => config.colors.hidden,
                "executable" => config.colors.executable,
                _ => unreachable!("color item key {:?} not in [colors]", item.key),
            };
            display_color(c)
        }
        ItemKind::Bool => {
            let value = match item.key {
                "confirm_operations" => config.confirm_operations,
                "confirm_quit" => config.confirm_quit,
                "quit_cd" => config.quit_cd,
                "mouse" => config.mouse,
                "file_search_incremental" => config.file_search_incremental,
                "command_line_interactive" => config.command_line_interactive,
                "natural_sort" => config.natural_sort,
                "cursor_wrap" => config.cursor_wrap,
                "cursor_memory" => config.cursor_memory,
                "clear_on_suspend" => config.clear_on_suspend,
                "show_permissions" => config.show_permissions,
                "show_git_status" => config.show_git_status,
                "auto_refresh" => config.auto_refresh,
                "process_auto_refresh" => config.process_auto_refresh,
                "dim_inactive" => config.colors.dim_inactive,
                _ => unreachable!("bool item key {:?}", item.key),
            };
            display_bool(value).to_string()
        }
        ItemKind::DeleteBehaviorEnum => display_delete_behavior(config.delete_behavior).to_string(),
        ItemKind::SizeFormatEnum => display_size_format(config.size_format).to_string(),
        ItemKind::OptionalText => {
            let value = match item.key {
                "home" => config.home.as_ref().map(|p| p.display().to_string()),
                "editor" => config.editor.clone(),
                _ => unreachable!("text item key {:?}", item.key),
            };
            display_optional_text(&value)
        }
    }
}

/// Which top-level TOML table a `save_*`/`remove_*` write targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Table {
    Root,
    Colors,
}

fn table_for(category: Category) -> Table {
    match category {
        Category::Colors => Table::Colors,
        _ => Table::Root,
    }
}

/// Loads `path` as a `toml_edit::DocumentMut`, creating it from the
/// template first if it doesn't exist yet (via `config::ensure_config_file_exists`,
/// the exact same first-run bootstrap `,`/edit_config already uses, so a
/// settings-screen edit on a fresh install produces the same starting
/// point either path would).
fn load_document(path: &Path) -> Result<toml_edit::DocumentMut> {
    config::ensure_config_file_exists(path)
        .with_context(|| format!("failed to create config file: {}", path.display()))?;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    text.parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}

fn write_document(path: &Path, doc: &toml_edit::DocumentMut) -> Result<()> {
    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write config file: {}", path.display()))
}

/// `doc[table]`, creating an empty inline-able table first if `table` is
/// `Colors` and `[colors]` doesn't exist yet in this file.
fn table_mut(doc: &mut toml_edit::DocumentMut, table: Table) -> &mut toml_edit::Item {
    match table {
        Table::Root => doc.as_item_mut(),
        Table::Colors => {
            if doc.get("colors").is_none() {
                doc["colors"] = toml_edit::table();
            }
            &mut doc["colors"]
        }
    }
}

/// Writes a single bool key (`save_change`'s entry point for
/// `ItemKind::Bool`).
pub fn save_bool(path: &Path, category: Category, key: &str, value: bool) -> Result<()> {
    let mut doc = load_document(path)?;
    table_mut(&mut doc, table_for(category))[key] = toml_edit::value(value);
    write_document(path, &doc)
}

/// Writes `delete_behavior` as its lowercase `snake_case` string (matching
/// `DeleteBehavior`'s own `#[serde(rename_all = "snake_case")]`).
pub fn save_delete_behavior(path: &Path, value: DeleteBehavior) -> Result<()> {
    let mut doc = load_document(path)?;
    doc["delete_behavior"] = toml_edit::value(display_delete_behavior(value));
    write_document(path, &doc)
}

/// Writes `size_format` as its `snake_case` string (matching
/// `SizeFormat`'s `#[serde(rename_all = "snake_case")]`). Used both by
/// the settings screen's editor and the `v` (toggle_size_format) action.
pub fn save_size_format(path: &Path, value: SizeFormat) -> Result<()> {
    let mut doc = load_document(path)?;
    doc["size_format"] = toml_edit::value(value.as_str());
    write_document(path, &doc)
}

/// Writes a color as its `#RRGGBB` hex form — always hex rather than
/// whatever named form the user picked it by, so the file's roundtrip
/// through this screen never depends on `color::parse_color`'s alias
/// table staying exactly in sync with `COLOR_PALETTE`'s. Still fully
/// valid input either way, per `parse_color`'s own hex support.
pub fn save_color(path: &Path, key: &str, value: Color) -> Result<()> {
    let mut doc = load_document(path)?;
    let hex = match value {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        other => display_color(other), // a named palette entry formats to its own name, also valid
    };
    table_mut(&mut doc, Table::Colors)[key] = toml_edit::value(hex);
    write_document(path, &doc)
}

/// Writes (or clears, for an empty `value`) an optional top-level string
/// key (`home`/`editor`) — an empty string removes the key entirely
/// rather than writing `""`, so the field genuinely goes back to "unset"
/// (`None`) rather than an empty-but-present string.
pub fn save_optional_text(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    if value.is_empty() {
        doc.as_table_mut().remove(key);
    } else {
        doc[key] = toml_edit::value(value);
    }
    write_document(path, &doc)
}

/// Writes (or removes, for an empty `command`) one `[viewers]` entry.
pub fn save_viewer_entry(path: &Path, extension: &str, command: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    if doc.get("viewers").is_none() {
        doc["viewers"] = toml_edit::table();
    }
    let viewers = doc["viewers"].as_table_mut().expect("just ensured a table");
    if command.is_empty() {
        viewers.remove(extension);
    } else {
        viewers[extension] = toml_edit::value(command);
    }
    write_document(path, &doc)
}

/// Removes a `[viewers]` entry outright (the settings screen's delete
/// action — distinct from `save_viewer_entry`'s empty-command case in
/// name only, same effect).
pub fn remove_viewer_entry(path: &Path, extension: &str) -> Result<()> {
    save_viewer_entry(path, extension, "")
}

/// If `extension` changed (a rename via the edit flow), removes the old
/// key before writing the new one — used when the settings screen's
/// viewer-entry editor commits with a different extension than it
/// started with.
pub fn save_viewer_entry_renaming(
    path: &Path,
    old_extension: Option<&str>,
    new_extension: &str,
    command: &str,
) -> Result<()> {
    if let Some(old) = old_extension
        && old != new_extension
    {
        remove_viewer_entry(path, old)?;
    }
    save_viewer_entry(path, new_extension, command)
}

/// Adds `combo` to `action`'s `[bindings]` array — see this module's doc
/// comment for the full precedence policy. A no-op (not a duplicate
/// write) if `combo` is already listed there.
pub fn add_binding(path: &Path, action: Action, combo: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    if doc.get("bindings").is_none() {
        doc["bindings"] = toml_edit::table();
    }
    let bindings = doc["bindings"]
        .as_table_mut()
        .expect("just ensured a table");
    let key = action.config_name();
    let already_present = bindings
        .get(key)
        .and_then(|item| item.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some(combo)));
    if !already_present {
        let entry = bindings
            .entry(key)
            .or_insert_with(|| toml_edit::value(toml_edit::Array::new()));
        let array = entry
            .as_array_mut()
            .expect("[bindings] values are always arrays");
        array.push(combo);
    }
    write_document(path, &doc)
}

/// Removes `combo` from `action`'s ownership — see this module's doc
/// comment for the full precedence policy (tries `[bindings]` first,
/// falls back to `[keys].<combo> = "none"`).
pub fn remove_binding(path: &Path, action: Action, combo: &str) -> Result<()> {
    let mut doc = load_document(path)?;
    let key = action.config_name();

    let removed_from_bindings = 'remove: {
        let Some(bindings) = doc.get_mut("bindings").and_then(|b| b.as_table_mut()) else {
            break 'remove false;
        };
        let Some(array) = bindings.get_mut(key).and_then(|item| item.as_array_mut()) else {
            break 'remove false;
        };
        let Some(index) = array.iter().position(|v| v.as_str() == Some(combo)) else {
            break 'remove false;
        };
        array.remove(index);
        if array.is_empty() {
            bindings.remove(key);
        }
        true
    };

    if !removed_from_bindings {
        if doc.get("keys").is_none() {
            doc["keys"] = toml_edit::table();
        }
        doc["keys"][combo] = toml_edit::value("none");
    }

    write_document(path, &doc)
}

/// Every combo currently bound to `action`, for the keybinding editor's
/// per-action combo list — thin wrapper over `Keymap::combos_for` kept
/// here so callers only need to import `crate::settings`, not also reach
/// into `crate::keymap` for this one call.
pub fn combos_for(keymap: &Keymap, action: Action) -> Vec<String> {
    keymap.combos_for(action)
}

/// Which action (if any, and if different from `for_action`) already owns
/// `combo` — the keybinding editor's conflict check, run right after a
/// capture. `None` means either nothing owns it yet, or `for_action`
/// already does (re-adding a combo it already has is never a conflict).
pub fn conflicting_action(keymap: &Keymap, combo: KeyCombo, for_action: Action) -> Option<Action> {
    keymap
        .resolve(combo.code, combo.modifiers)
        .filter(|&owner| owner != for_action)
}

/// Formats `combo` for display — re-exported from `keymap` so the
/// keybinding editor (and its tests) don't need a second import for one
/// function.
pub fn format_combo(combo: &KeyCombo) -> String {
    keymap::format_combo(combo)
}

/// Whether `combo` can actually be captured as a new binding: it must
/// round-trip through `format_combo` -> `KeyCombo::parse` back to itself,
/// the same representability check `to_bindings_toml`'s drift-prevention
/// test relies on implicitly. Most `KeyCode` variants this app's `Entry`
/// point ever sees do (arrows, function-adjacent named keys, plain
/// chars); a handful of exotic ones (bare `F5`, media keys, ...) don't —
/// captured, they'd write a combo string nothing could ever parse back
/// into a real binding, silently breaking it. Rejected up front instead.
pub fn combo_is_representable(combo: KeyCombo) -> bool {
    KeyCombo::parse(&format_combo(&combo)) == Ok(combo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn test_keymap() -> Keymap {
        Keymap::defaults()
    }

    // --- display helpers ---------------------------------------------------

    /// Every item in the three static tables has to have a matching arm in
    /// `item_value_display`, which is an `unreachable!` on a miss — so a
    /// setting added to a table but not to the lookup would panic the
    /// settings screen the first time it scrolled into view. Rendering
    /// every one of them here catches that at test time instead.
    #[test]
    fn every_static_item_renders_a_value() {
        let config = Config::default();
        for (category, items) in [
            (Category::Behavior, &BEHAVIOR_ITEMS[..]),
            (Category::Colors, &COLOR_ITEMS[..]),
            (Category::Startup, &STARTUP_ITEMS[..]),
        ] {
            for item in items {
                let value = item_value_display(category, item, &config);
                assert!(!value.is_empty(), "{} rendered nothing", item.key);
            }
        }
    }

    #[test]
    fn display_bool_reads_on_off() {
        assert_eq!(display_bool(true), "ON");
        assert_eq!(display_bool(false), "OFF");
    }

    #[test]
    fn display_color_prefers_the_palette_name_over_hex() {
        assert_eq!(display_color(Color::Red), "red");
        assert_eq!(display_color(Color::Rgb(0x90, 0xEE, 0x90)), "#90EE90");
    }

    #[test]
    fn display_optional_text_shows_a_placeholder_when_unset() {
        assert_eq!(display_optional_text(&None), "(unset)");
        assert_eq!(display_optional_text(&Some("vim".to_string())), "vim");
    }

    #[test]
    fn item_value_display_reads_every_behavior_item_from_config() {
        let config = Config {
            confirm_operations: false,
            delete_behavior: DeleteBehavior::Permanent,
            ..Config::default()
        };
        let confirm_ops = &BEHAVIOR_ITEMS[0];
        assert_eq!(
            item_value_display(Category::Behavior, confirm_ops, &config),
            "OFF"
        );
        let del_behavior = &BEHAVIOR_ITEMS[4];
        assert_eq!(
            item_value_display(Category::Behavior, del_behavior, &config),
            "permanent"
        );
    }

    #[test]
    fn item_value_display_reads_colors_from_the_colors_table() {
        let mut config = Config::default();
        config.colors.directory = Color::Magenta;
        let directory_item = &COLOR_ITEMS[2];
        assert_eq!(directory_item.key, "directory");
        assert_eq!(
            item_value_display(Category::Colors, directory_item, &config),
            "magenta"
        );
    }

    #[test]
    fn item_value_display_reads_startup_optional_text() {
        let config = Config {
            editor: Some("nvim".to_string()),
            ..Config::default()
        };
        let editor_item = &STARTUP_ITEMS[1];
        assert_eq!(editor_item.key, "editor");
        assert_eq!(
            item_value_display(Category::Startup, editor_item, &config),
            "nvim"
        );
        let home_item = &STARTUP_ITEMS[0];
        assert_eq!(
            item_value_display(Category::Startup, home_item, &config),
            "(unset)"
        );
    }

    // --- toml_edit persistence: comment/layout preservation -----------------

    #[test]
    fn save_bool_preserves_comments_and_only_changes_the_target_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# a comment above delete_behavior\n\
             delete_behavior = \"trash\"\n\
             # a comment above mouse\n\
             mouse = true\n",
        )
        .unwrap();

        save_bool(&path, Category::Behavior, "mouse", false).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# a comment above delete_behavior"), "{text}");
        assert!(text.contains("# a comment above mouse"), "{text}");
        assert!(text.contains("delete_behavior = \"trash\""), "{text}");
        assert!(text.contains("mouse = false"), "{text}");

        let config: Config = toml::from_str(&text).unwrap();
        assert!(!config.mouse);
        assert_eq!(config.delete_behavior, DeleteBehavior::Trash);
    }

    #[test]
    fn save_color_creates_the_colors_table_if_missing_and_preserves_other_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# top of file\nmouse = true\n").unwrap();

        save_color(&path, "directory", Color::Rgb(0x11, 0x22, 0x33)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top of file"), "{text}");
        assert!(text.contains("mouse = true"), "{text}");
        assert!(text.contains("[colors]"), "{text}");
        assert!(text.contains("directory = \"#112233\""), "{text}");

        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.colors.directory, Color::Rgb(0x11, 0x22, 0x33));
        assert!(config.mouse);
    }

    #[test]
    fn save_color_by_palette_name_writes_the_name_not_hex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        save_color(&path, "hidden", Color::Magenta).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hidden = \"magenta\""), "{text}");
    }

    #[test]
    fn save_optional_text_writes_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        save_optional_text(&path, "editor", "nvim").unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.editor, Some("nvim".to_string()));

        save_optional_text(&path, "editor", "").unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.editor, None);
    }

    #[test]
    fn save_viewer_entry_adds_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        save_viewer_entry(&path, "md", "glow {}").unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.viewers.get("md"), Some(&"glow {}".to_string()));

        remove_viewer_entry(&path, "md").unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!config.viewers.contains_key("md"));
    }

    #[test]
    fn save_viewer_entry_renaming_removes_the_old_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        save_viewer_entry(&path, "md", "glow {}").unwrap();

        save_viewer_entry_renaming(&path, Some("md"), "markdown", "glow {}").unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!config.viewers.contains_key("md"));
        assert_eq!(config.viewers.get("markdown"), Some(&"glow {}".to_string()));
    }

    // --- keybinding [bindings]/[keys] precedence ----------------------------

    #[test]
    fn add_binding_appends_to_the_bindings_array_creating_it_if_needed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        add_binding(&path, Action::Rename, "z").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[bindings]"), "{text}");
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.bindings.get("rename"), Some(&vec!["z".to_string()]));
    }

    #[test]
    fn add_binding_does_not_duplicate_an_already_present_combo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[bindings]\nrename = [\"z\"]\n").unwrap();

        add_binding(&path, Action::Rename, "z").unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.bindings.get("rename"), Some(&vec!["z".to_string()]));
    }

    #[test]
    fn remove_binding_removes_from_bindings_array_when_present_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[bindings]\nrename = [\"z\", \"x\"]\n").unwrap();

        remove_binding(&path, Action::Rename, "z").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.bindings.get("rename"), Some(&vec!["x".to_string()]));
        assert!(
            !text.contains("[keys]"),
            "must not have fallen through to [keys] when found in [bindings]: {text}"
        );
    }

    #[test]
    fn remove_binding_deletes_the_array_entirely_once_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[bindings]\nrename = [\"z\"]\n").unwrap();

        remove_binding(&path, Action::Rename, "z").unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.bindings.get("rename"), None);
    }

    #[test]
    fn remove_binding_falls_back_to_keys_none_for_a_default_combo() {
        // "r" is not in any [bindings] array here (it's a compiled-in
        // default) — removing it must write [keys]."r" = "none".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        remove_binding(&path, Action::Rename, "r").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[keys]"), "{text}");
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.keys.get("r"), Some(&"none".to_string()));
    }

    #[test]
    fn remove_binding_overwrites_an_existing_keys_entry_for_the_same_combo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[keys]\nz = \"rename\"\n").unwrap();

        remove_binding(&path, Action::Rename, "z").unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.keys.get("z"), Some(&"none".to_string()));
    }

    #[test]
    fn steal_sequence_moves_a_combo_from_one_action_to_another() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();

        // "r" defaults to Rename; simulate stealing it for Mkdir.
        remove_binding(&path, Action::Rename, "r").unwrap();
        add_binding(&path, Action::Mkdir, "r").unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.keys.get("r"), Some(&"none".to_string()));
        assert_eq!(config.bindings.get("mkdir"), Some(&vec!["r".to_string()]));
    }

    // --- conflict detection / combo representability ------------------------

    #[test]
    fn conflicting_action_finds_the_current_owner_of_a_default_combo() {
        let keymap = test_keymap();
        let combo = KeyCombo::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(
            conflicting_action(&keymap, combo, Action::Mkdir),
            Some(Action::Rename)
        );
    }

    #[test]
    fn conflicting_action_is_none_when_the_action_already_owns_it() {
        let keymap = test_keymap();
        let combo = KeyCombo::new(KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(conflicting_action(&keymap, combo, Action::Rename), None);
    }

    #[test]
    fn conflicting_action_is_none_for_an_unbound_combo() {
        let keymap = test_keymap();
        let combo = KeyCombo::new(KeyCode::Char('9'), KeyModifiers::NONE);
        assert_eq!(conflicting_action(&keymap, combo, Action::Mkdir), None);
    }

    #[test]
    fn combo_is_representable_accepts_ordinary_keys() {
        assert!(combo_is_representable(KeyCombo::new(
            KeyCode::Char('z'),
            KeyModifiers::NONE
        )));
        assert!(combo_is_representable(KeyCombo::new(
            KeyCode::Char('r'),
            KeyModifiers::SHIFT
        )));
        assert!(combo_is_representable(KeyCombo::new(
            KeyCode::Up,
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn combo_is_representable_rejects_keys_format_combo_cannot_round_trip() {
        assert!(!combo_is_representable(KeyCombo::new(
            KeyCode::F(5),
            KeyModifiers::NONE
        )));
    }
}
