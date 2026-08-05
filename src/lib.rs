//! Library half of ozzel, split out from `main.rs` so that external
//! targets (criterion benches in `benches/`) can reach internal APIs
//! (`Pane`, `ui::log_view::wrap_log_lines`, `virtual_dir`, ...) without
//! going through a spawned binary. `main.rs` re-exports nothing back from
//! here beyond `use ozzel::...` at its own call sites — this crate holds
//! every module that isn't CLI/terminal/process-lifecycle plumbing.

pub mod action;
pub mod app;
pub mod color;
pub mod config;
pub mod entry;
pub mod event;
pub mod external;
pub mod filter;
pub mod function_list;
pub mod help;
pub mod keymap;
pub mod mode;
pub mod ops;
pub mod pane;
pub mod persist;
pub mod search;
pub mod settings;
pub mod tasks;
pub mod ui;
pub mod viewer;
pub mod virtual_dir;
mod xdg;
