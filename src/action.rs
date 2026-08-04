//! Every command the app knows how to perform, decoupled from any specific
//! key. `keymap.rs` maps configurable key combos onto these; `Deserialize`
//! (snake_case) lets a config `[keys]` table's action names parse straight
//! into this enum (e.g. `"C-c" = "copy"`).

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
    SwitchPane,
    Enter,
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
    Filter,
    ClearFilter,
    ZipMarked,
    Unzip,
    HistoryJump,
    BookmarkJump,
    BookmarkAdd,
    GoHome,
    CommandLine,
    OpenEditor,
    OpenDefault,
    Quit,
}
