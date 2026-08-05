//! Mouse handling for Normal mode: click focus/cursor-move, left-drag
//! range-marking, double-click open, and wheel scroll — plus the pure
//! `hit_test_row` coordinate math and `DragState` they share. Split out
//! of `app/mod.rs` (Phase 6, Step 4).

use super::*;

/// Which pane a left-button drag is constrained to, and the drag's own
/// running state. This is a "live rubber-band" range select: every `Drag`
/// event recomputes marks fresh from `snapshot` (the pane's marks the
/// instant before the drag began) plus the current `[origin_index, row]`
/// range toggled relative to it — see `App::handle_mouse_left_drag`. That
/// means retreating the pointer out of a row it previously swept over
/// automatically reverts that row to whatever it was *before* the drag
/// touched it, rather than leaving it toggled forever — the defining
/// difference from a plain "mark everything swept" or "toggle once and
/// remember" scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragState {
    pub pane: ActivePane,
    /// The entry index (into `visible_entries()`) the drag started on —
    /// its toggle is deliberately deferred until the drag is *confirmed*
    /// (the first `Drag`/movement event arrives), so a plain click that
    /// never moves the pointer never toggles anything (see
    /// `App::handle_mouse_left_down`'s doc comment).
    pub origin_index: usize,
    /// `false` until the first `Drag` event after mouse-down; only
    /// affects whether the origin row's own toggle has materialized yet
    /// (a plain click that never drags must leave marks untouched).
    started: bool,
    /// The pane's marks exactly as they were the instant before this drag
    /// began — the baseline every `Drag` event re-derives the current
    /// marks from, so the range is always computed fresh rather than
    /// accumulated incrementally.
    snapshot: HashSet<PathBuf>,
}

/// Maps a click/drag/wheel screen coordinate to an entry index (into
/// `Pane::visible_entries()`), given the pane's last-drawn `rows_area` and
/// scroll `start` offset — pure and free-standing so it's directly
/// unit-testable without any `App`/`Pane` machinery. Returns `None` when
/// `(x, y)` falls outside `rows_area` entirely (out of range, or over the
/// header/border instead of a row). Does *not* clamp against how many
/// entries actually exist past `start` — callers that need that (a short
/// listing scrolled so its last row doesn't fill the viewport) additionally
/// bound the result against `visible_entries().len()`.
pub(super) fn hit_test_row(layout: &PaneLayout, x: u16, y: u16) -> Option<usize> {
    let area = layout.rows_area;
    if x < area.x || x >= area.x + area.width || y < area.y || y >= area.y + area.height {
        return None;
    }
    Some(layout.start + (y - area.y) as usize)
}

impl App {
    /// Entry point for every mouse event (only ever produced when
    /// `config.mouse` enabled capture — see `event::read_event`). Modal
    /// modes other than Viewer/Log/Help ignore the mouse entirely (per the
    /// plan: "other modals ignore mouse, Esc still by key"); Normal mode
    /// gets the full click/drag/wheel behavior in `handle_mouse_normal`.
    /// Viewer/Log/Help never reach here at all: mouse capture is
    /// dynamically disabled the moment any of them becomes the active
    /// mode (see `App::wants_mouse_capture`/`main.rs`'s `sync_mouse_capture`),
    /// so the terminal never even reports a mouse event while one is
    /// showing — that's the whole point, it hands wheel/selection back to
    /// the terminal's own native scrollback and text selection. Only
    /// `Normal` has anything to do here; every other mode (including the
    /// Function List command palette, which keeps capture on but has no
    /// mouse behavior of its own) ignores mouse input.
    pub(super) fn handle_mouse(&mut self, ev: MouseEvent) {
        if matches!(self.mode, Mode::Normal) {
            self.handle_mouse_normal(ev);
        }
    }

    /// Which pane (if any) `(x, y)` falls inside, by its last-drawn `area`
    /// (see `App::pane_layout`, refreshed every frame by `ui::draw`).
    fn pane_at(&self, x: u16, y: u16) -> Option<ActivePane> {
        for (i, layout) in self.pane_layout.iter().enumerate() {
            let layout = layout.as_ref()?;
            if x >= layout.area.x
                && x < layout.area.x + layout.area.width
                && y >= layout.area.y
                && y < layout.area.y + layout.area.height
            {
                return Some(if i == 0 {
                    ActivePane::Left
                } else {
                    ActivePane::Right
                });
            }
        }
        None
    }

    fn pane_layout_for(&self, pane: ActivePane) -> Option<&PaneLayout> {
        self.pane_layout[pane.index()].as_ref()
    }

    /// Normal-mode mouse behavior: left click focuses (and, on an entry
    /// row, moves the cursor there), left drag range-marks within the pane
    /// the drag started in, double-click opens, and wheel scrolls the
    /// cursor of whichever pane it's over without changing focus.
    fn handle_mouse_normal(&mut self, ev: MouseEvent) {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.handle_mouse_left_down(ev),
            MouseEventKind::Drag(MouseButton::Left) => self.handle_mouse_left_drag(ev),
            MouseEventKind::Up(MouseButton::Left) => self.drag = None,
            MouseEventKind::ScrollUp => self.handle_mouse_wheel(ev, -(MOUSE_WHEEL_STEP as isize)),
            MouseEventKind::ScrollDown => self.handle_mouse_wheel(ev, MOUSE_WHEEL_STEP as isize),
            _ => {}
        }
    }

    /// Left-button down: focuses whichever pane was clicked; on an entry
    /// row, also moves that pane's cursor there, starts a potential
    /// range-mark drag, and checks for a double-click (same row, same pane,
    /// within `DOUBLE_CLICK_WINDOW`) — which opens the entry instead of
    /// just moving the cursor a second time.
    fn handle_mouse_left_down(&mut self, ev: MouseEvent) {
        let Some(pane) = self.pane_at(ev.column, ev.row) else {
            return;
        };
        self.active = pane;
        let Some(layout) = self.pane_layout_for(pane) else {
            return;
        };
        let Some(row) = hit_test_row(layout, ev.column, ev.row) else {
            // Clicked the pane's header/border/blank area: focus only.
            self.last_click = None;
            self.drag = None;
            return;
        };
        let len = self.panes[pane.index()].visible_entries().len();
        if row >= len {
            self.last_click = None;
            self.drag = None;
            return;
        }

        let now = Instant::now();
        let is_double_click = matches!(
            self.last_click,
            Some((last_pane, last_row, at))
                if last_pane == pane && last_row == row && now.duration_since(at) < DOUBLE_CLICK_WINDOW
        );
        self.panes[pane.index()].cursor = row;
        if is_double_click {
            self.last_click = None;
            self.drag = None;
            self.begin_open();
            return;
        }
        self.last_click = Some((pane, row, now));
        // Arms a *potential* drag, but doesn't toggle anything yet: a
        // plain click (mouse-down immediately followed by mouse-up, no
        // `Drag` events in between) must only move the cursor, never
        // toggle a mark — only `handle_mouse_left_drag`, once an actual
        // `Drag` event proves the pointer moved while the button was
        // held, applies the range (which, on that very first event,
        // already includes the origin row itself). `snapshot` freezes
        // the pane's marks exactly as they were right now, before any of
        // that happens.
        let snapshot = self.panes[pane.index()].marks.clone();
        self.drag = Some(DragState {
            pane,
            origin_index: row,
            started: false,
            snapshot,
        });
    }

    /// Left-button drag: a live rubber-band range select. Every `Drag`
    /// event recomputes the pane's marks from scratch as `snapshot`
    /// (frozen at mouse-down) with every row in `[origin_index, row]`
    /// toggled relative to it — never accumulated incrementally — so a
    /// row the pointer swept over and then retreated *out* of
    /// automatically reverts to whatever it was before the drag touched
    /// it, and a direction reversal across the origin just recomputes the
    /// (now different) range from the same snapshot. Only active *within*
    /// the pane the drag started in — crossing into the other pane, or
    /// off the entry rows entirely, simply leaves the current range as-is
    /// (focus never changes mid-drag, and nothing reverts just because
    /// the pointer briefly left the rows area).
    fn handle_mouse_left_drag(&mut self, ev: MouseEvent) {
        let Some(drag) = &self.drag else { return };
        let pane = drag.pane;
        let origin = drag.origin_index;

        let Some(layout) = self.pane_layout_for(pane) else {
            return;
        };
        let Some(row) = hit_test_row(layout, ev.column, ev.row) else {
            return;
        };
        let len = self.panes[pane.index()].visible_entries().len();
        if row >= len {
            return;
        }

        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.started = true;
        let snapshot = drag.snapshot.clone();

        let (lo, hi) = if row <= origin {
            (row, origin)
        } else {
            (origin, row)
        };
        self.panes[pane.index()].apply_drag_range(&snapshot, lo, hi);
    }

    /// Mouse wheel over a pane in Normal mode: moves that pane's cursor by
    /// `delta` rows (negative = up) without changing which pane is active —
    /// tried focus-follow while implementing this and found it more
    /// surprising than leaving focus alone, since a stray wheel tick while
    /// reading the other pane would otherwise silently redirect keystrokes.
    fn handle_mouse_wheel(&mut self, ev: MouseEvent, delta: isize) {
        let Some(pane) = self.pane_at(ev.column, ev.row) else {
            return;
        };
        self.panes[pane.index()].move_cursor(delta);
    }
}
