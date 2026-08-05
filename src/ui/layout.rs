//! One pane's on-screen geometry as of the last frame — an `ui`-owned type
//! (rather than living in `app.rs`, which `pane_view::render` would then
//! have to import from) so the dependency runs only `app -> ui`, never the
//! other way: `ui/*` otherwise only ever takes `&App`/`&mut App` as a
//! read/write argument, never a type it has to import from `app` — see
//! `ui::LayoutFeedback`'s doc comment for the other half of untangling this
//! same app<->ui cycle.

use ratatui::layout::Rect;

/// Just enough for mouse hit-testing to map a click's `(x, y)` back to
/// "row N of this pane's visible entries" (see `App`'s `hit_test_row`).
#[derive(Debug, Clone, Copy)]
pub struct PaneLayout {
    /// The pane's full drawn area, borders included.
    pub area: Rect,
    /// The entry-list rows' area specifically (inside the border and any
    /// header rows) — what `hit_test_row` actually maps `y` against.
    pub rows_area: Rect,
    /// Index of the first visible entry (`Pane::visible_entries()[start]`
    /// is whatever's drawn at `rows_area`'s first row) — mirrors
    /// `ui::pane_view`'s own `scroll_offset` so hit-testing agrees with
    /// what's actually on screen.
    pub start: usize,
}
