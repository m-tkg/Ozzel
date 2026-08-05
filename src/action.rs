//! Every command the app knows how to perform, decoupled from any specific
//! key. `keymap.rs` maps configurable key combos onto these; `Deserialize`
//! (snake_case) lets a config `[keys]`/`[bindings]` table's action names
//! parse straight into this enum (e.g. `"C-c" = "copy"`,
//! `rename = ["r", "S-r"]`).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    SwitchPane,
    /// Context-dependent: a directory (or `..`) navigates into it (and
    /// gets recorded in history); anything else opens in the built-in
    /// viewer. Merges what used to be two separate actions, `Enter` and
    /// `View` — see `App::begin_open`.
    Open,
    Parent,
    CycleSort,
    ToggleHidden,
    SwapPanes,
    Refresh,
    Mark,
    MarkAll,
    Rename,
    Mkdir,
    Delete,
    Copy,
    Move,
    /// Copies the cursor entry to a new name in the *same* directory,
    /// prompting for the name first (prefilled with the current one). See
    /// `App::begin_duplicate`.
    Duplicate,
    /// Copies the cursor entry's absolute path to the system clipboard
    /// (via an OSC 52 terminal escape — see `App::begin_copy_path`).
    CopyPath,
    Filter,
    ClearFilter,
    ZipMarked,
    Unzip,
    HistoryJump,
    /// Per-pane "back" in a browser-style history stack — the reverse of
    /// whatever cwd change was made last (see `App::record_history_if_changed`).
    /// Distinct from `HistoryJump`'s persisted MRU menu.
    HistoryBack,
    /// The reverse of `HistoryBack`.
    HistoryForward,
    BookmarkJump,
    BookmarkAdd,
    GoHome,
    CommandLine,
    OpenEditor,
    OpenDefault,
    FocusLeft,
    FocusRight,
    /// Opens the full-frame keybinding help screen (`Mode::Help`); see
    /// `crate::help`.
    Help,
    /// Opens ozzel's own config file in an editor (creating it from a
    /// starter template first if it doesn't exist yet), then reloads the
    /// config live once the editor exits. See `App::begin_edit_config`.
    EditConfig,
    /// Opens the full-frame in-memory log viewer (`Mode::Log`); see
    /// `App::begin_show_log`.
    ShowLog,
    /// Opens the command palette (`Mode::FunctionList`): a filterable
    /// list of every action, executable by name. See
    /// `App::begin_function_list`.
    FunctionList,
    Quit,
}

/// Coarse grouping used to organize the help screen (`crate::help`) into
/// sections. Not part of the config format — purely a display concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    Movement,
    Marks,
    FileOps,
    Filter,
    Jumps,
    External,
    Misc,
}

impl ActionCategory {
    /// Fixed display order for the help screen.
    pub const ALL: [ActionCategory; 7] = [
        ActionCategory::Movement,
        ActionCategory::Marks,
        ActionCategory::FileOps,
        ActionCategory::Filter,
        ActionCategory::Jumps,
        ActionCategory::External,
        ActionCategory::Misc,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActionCategory::Movement => "Movement",
            ActionCategory::Marks => "Marks",
            ActionCategory::FileOps => "File operations",
            ActionCategory::Filter => "Filter / search",
            ActionCategory::Jumps => "History / bookmarks / home",
            ActionCategory::External => "External / viewer",
            ActionCategory::Misc => "Misc",
        }
    }
}

impl Action {
    /// Every action, exactly once, in the order the help screen lists them
    /// within their category. Kept as an explicit array (rather than a
    /// derived "iterate all variants") so adding a variant is a compile
    /// error here *and* in `category`/`description`/`config_name` below
    /// (all exhaustive matches) until every one of them accounts for it.
    pub const ALL: [Action; 42] = [
        Action::CursorUp,
        Action::CursorDown,
        Action::PageUp,
        Action::PageDown,
        Action::Top,
        Action::Bottom,
        Action::SwitchPane,
        Action::FocusLeft,
        Action::FocusRight,
        Action::Open,
        Action::Parent,
        Action::CycleSort,
        Action::ToggleHidden,
        Action::SwapPanes,
        Action::Refresh,
        Action::Mark,
        Action::MarkAll,
        Action::Rename,
        Action::Mkdir,
        Action::Delete,
        Action::Copy,
        Action::Move,
        Action::Duplicate,
        Action::CopyPath,
        Action::ZipMarked,
        Action::Unzip,
        Action::Filter,
        Action::ClearFilter,
        Action::HistoryJump,
        Action::HistoryBack,
        Action::HistoryForward,
        Action::BookmarkJump,
        Action::BookmarkAdd,
        Action::GoHome,
        Action::CommandLine,
        Action::OpenEditor,
        Action::OpenDefault,
        Action::Help,
        Action::EditConfig,
        Action::ShowLog,
        Action::FunctionList,
        Action::Quit,
    ];

    pub fn category(self) -> ActionCategory {
        use Action::*;
        match self {
            CursorUp | CursorDown | PageUp | PageDown | Top | Bottom | SwitchPane | FocusLeft
            | FocusRight | Open | Parent | CycleSort | ToggleHidden | SwapPanes | Refresh => {
                ActionCategory::Movement
            }
            Mark | MarkAll => ActionCategory::Marks,
            Rename | Mkdir | Delete | Copy | Move | Duplicate | CopyPath | ZipMarked | Unzip => {
                ActionCategory::FileOps
            }
            Filter | ClearFilter => ActionCategory::Filter,
            HistoryJump | HistoryBack | HistoryForward | BookmarkJump | BookmarkAdd | GoHome => {
                ActionCategory::Jumps
            }
            CommandLine | OpenEditor | OpenDefault | EditConfig => ActionCategory::External,
            Help | ShowLog | FunctionList | Quit => ActionCategory::Misc,
        }
    }

    /// A short English one-liner for the help screen.
    pub fn description(self) -> &'static str {
        use Action::*;
        match self {
            CursorUp => "Move cursor up",
            CursorDown => "Move cursor down",
            PageUp => "Move cursor up a page",
            PageDown => "Move cursor down a page",
            Top => "Jump to the top of the list",
            Bottom => "Jump to the bottom of the list",
            SwitchPane => "Toggle the active pane",
            FocusLeft => "Focus the left pane",
            FocusRight => "Focus the right pane",
            Open => "Open directory (navigate in), or view file",
            Parent => "Go to the parent directory",
            CycleSort => "Cycle the sort key",
            ToggleHidden => "Toggle hidden files",
            SwapPanes => "Swap the left and right panes",
            Refresh => "Reload both panes",
            Mark => "Mark/unmark the cursor entry",
            MarkAll => "Toggle marks on all visible entries",
            Rename => "Rename the cursor entry",
            Mkdir => "Create a new directory",
            Delete => "Delete marked entries (or the cursor entry)",
            Copy => "Copy marked entries (or the cursor entry) to the other pane",
            Move => "Move marked entries (or the cursor entry) to the other pane",
            Duplicate => "Copy the cursor entry to a new name in the same directory",
            CopyPath => "Copy the cursor entry's absolute path to the clipboard",
            ZipMarked => "Zip marked entries (or the cursor entry)",
            Unzip => "Unzip the cursor .zip file",
            Filter => "Start an incremental filter",
            ClearFilter => "Clear the active filter",
            HistoryJump => "Open the directory history menu",
            HistoryBack => "Go back to the previous directory in this pane",
            HistoryForward => "Go forward to the next directory in this pane",
            BookmarkJump => "Open the bookmarks menu",
            BookmarkAdd => "Bookmark the current directory",
            GoHome => "Jump to the home directory",
            CommandLine => "Run a shell command (TUI suspended)",
            OpenEditor => "Open the cursor file in an editor",
            OpenDefault => "Open the cursor entry with the OS default app",
            Help => "Show this help screen",
            EditConfig => "Edit the config file (created from a template if missing)",
            ShowLog => "Show the full in-memory log",
            FunctionList => "Open the command palette (search and run any action)",
            Quit => "Quit ozzel",
        }
    }

    /// The exact string this action is named by in `[keys]`/`[bindings]`
    /// (matches the `Deserialize` derive's `rename_all = "snake_case"`
    /// exactly — `config_name_round_trips_through_deserialize_for_every_action`
    /// below guards against the two drifting apart).
    pub fn config_name(self) -> &'static str {
        use Action::*;
        match self {
            CursorUp => "cursor_up",
            CursorDown => "cursor_down",
            PageUp => "page_up",
            PageDown => "page_down",
            Top => "top",
            Bottom => "bottom",
            SwitchPane => "switch_pane",
            Open => "open",
            Parent => "parent",
            CycleSort => "cycle_sort",
            ToggleHidden => "toggle_hidden",
            SwapPanes => "swap_panes",
            Refresh => "refresh",
            Mark => "mark",
            MarkAll => "mark_all",
            Rename => "rename",
            Mkdir => "mkdir",
            Delete => "delete",
            Copy => "copy",
            Move => "move",
            Duplicate => "duplicate",
            CopyPath => "copy_path",
            Filter => "filter",
            ClearFilter => "clear_filter",
            ZipMarked => "zip_marked",
            Unzip => "unzip",
            HistoryJump => "history_jump",
            HistoryBack => "history_back",
            HistoryForward => "history_forward",
            BookmarkJump => "bookmark_jump",
            BookmarkAdd => "bookmark_add",
            GoHome => "go_home",
            CommandLine => "command_line",
            OpenEditor => "open_editor",
            OpenDefault => "open_default",
            FocusLeft => "focus_left",
            FocusRight => "focus_right",
            Quit => "quit",
            Help => "help",
            EditConfig => "edit_config",
            ShowLog => "show_log",
            FunctionList => "function_list",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::IntoDeserializer;

    #[test]
    fn all_contains_every_variant_exactly_once() {
        let mut seen = std::collections::HashSet::new();
        for action in Action::ALL {
            assert!(seen.insert(action), "duplicate in Action::ALL: {action:?}");
        }
        assert_eq!(Action::ALL.len(), seen.len());
    }

    #[test]
    fn config_name_round_trips_through_deserialize_for_every_action() {
        for action in Action::ALL {
            let deserializer: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
                action.config_name().into_deserializer();
            let parsed = Action::deserialize(deserializer)
                .unwrap_or_else(|_| panic!("config_name() {:?} must deserialize", action));
            assert_eq!(
                parsed, action,
                "config_name() for {action:?} must round-trip"
            );
        }
    }

    #[test]
    fn every_action_has_a_non_empty_description() {
        for action in Action::ALL {
            assert!(!action.description().is_empty(), "{action:?}");
        }
    }
}
