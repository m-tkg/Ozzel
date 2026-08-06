//! Library half of ozzel, split out from `main.rs` so that external
//! targets (criterion benches in `benches/`) can reach internal APIs
//! (`Pane`, `logwrap::wrap_log_lines`, `virtual_dir`, ...) without going
//! through a spawned binary. `main.rs` re-exports nothing back from here
//! beyond `use ozzel::...` at its own call sites — it holds only `main`
//! itself and the top-level event loop (`run`), thin enough that every
//! other module, including CLI parsing (`cli`), terminal lifecycle
//! (`terminal`), and the `ozzel update` subcommand (`update`), lives here
//! instead: `terminal`'s escape-sequence helpers are also reused by
//! `external::run_suspended`, another lib module, which is what actually
//! forces those three across the bin/lib boundary rather than leaving them
//! as `main.rs`-only plumbing.

pub mod action;
pub mod app;
pub mod cli;
pub mod color;
pub mod config;
pub mod diff;
pub mod entry;
pub mod event;
pub mod external;
pub mod file_search;
pub mod filter;
pub mod function_list;
pub mod git;
pub mod help;
pub mod keymap;
pub mod logwrap;
pub mod mode;
pub mod ops;
pub mod pane;
pub mod persist;
pub mod search;
pub mod settings;
pub mod tasks;
pub mod terminal;
pub mod ui;
pub mod update;
pub mod viewer;
pub mod virtual_dir;
mod xdg;
