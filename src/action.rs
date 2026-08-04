//! Every command the app knows how to perform, decoupled from any specific
//! key. `keymap.rs` (Phase 2) will map configurable key combos onto these;
//! for now `app::action_for_input` hardcodes the dyna-filer-style defaults.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Quit,
}
