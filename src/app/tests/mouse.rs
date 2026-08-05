use super::super::mouse::hit_test_row;
use super::super::test_support::*;
use super::super::*;

#[test]
fn hit_test_row_maps_a_click_inside_rows_area_to_an_entry_index() {
    let layout = test_layout(
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        },
        0,
        0,
    );
    // rows_area starts at (1, 1); the 3rd row down is y=3 -> index 2.
    assert_eq!(hit_test_row(&layout, 5, 3), Some(2));
}

#[test]
fn hit_test_row_honors_the_scroll_start_offset() {
    let layout = test_layout(
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        },
        0,
        5,
    );
    assert_eq!(hit_test_row(&layout, 5, 1), Some(5));
}

#[test]
fn hit_test_row_returns_none_outside_the_rows_area() {
    let layout = test_layout(
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        },
        0,
        0,
    );
    assert_eq!(hit_test_row(&layout, 0, 0), None); // border row
    assert_eq!(hit_test_row(&layout, 5, 9), None); // border row (bottom)
    assert_eq!(hit_test_row(&layout, 100, 3), None); // off to the right
}

#[test]
fn hit_test_row_accounts_for_a_2_row_header() {
    let layout = test_layout(
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 10,
        },
        1, // one extra header content row
        0,
    );
    // rows_area now starts one row lower (y=2), so y=2 is index 0.
    assert_eq!(hit_test_row(&layout, 5, 2), Some(0));
    assert_eq!(hit_test_row(&layout, 5, 1), None); // the header row itself
}

#[test]
fn left_click_on_an_entry_row_focuses_the_pane_and_moves_the_cursor() {
    let (_dir, mut app) = mouse_test_app();
    app.active = ActivePane::Right; // start on the other pane
    // Entries sorted: ".." isn't shown here since dir.path() as both
    // panes' root is being used as a plain (non-fs-root) directory, so
    // row 0 is "..", row 1 is the first real entry.
    click(&mut app, 5, 2); // rows_area y starts at 1 -> row index 1
    assert_eq!(app.active, ActivePane::Left);
    assert_eq!(app.panes[0].cursor, 1);
}

#[test]
fn left_click_does_not_mark_the_row_it_lands_on() {
    // Regression test: a plain click (down, then up, no drag in
    // between) must only move the cursor — marking is drag-only.
    let (_dir, mut app) = mouse_test_app();
    click(&mut app, 5, 2);
    release(&mut app, 5, 2);
    assert!(app.panes[0].marks.is_empty());
}

#[test]
fn click_on_the_header_or_border_only_focuses_without_moving_the_cursor() {
    let (_dir, mut app) = mouse_test_app();
    app.panes[0].cursor = 0;
    app.active = ActivePane::Right;
    click(&mut app, 0, 0); // the border, not inside rows_area
    assert_eq!(app.active, ActivePane::Left);
    assert_eq!(app.panes[0].cursor, 0);
}

#[test]
fn drag_across_multiple_rows_toggles_every_row_swept_over_on() {
    // Screen rows 2/3/4 map to visible indices 1/2/3 = c_dir/a.txt/b.txt
    // (index 0 is the ".." row); rows_area starts at screen y=1.
    let (dir, mut app) = mouse_test_app();
    click(&mut app, 5, 2); // origin: c_dir, not yet toggled (no drag yet)
    drag(&mut app, 5, 4); // sweep down through a.txt to b.txt
    release(&mut app, 5, 4);

    let marks = &app.panes[0].marks;
    assert_eq!(marks.len(), 3, "marks: {marks:?}");
    assert!(marks.contains(&dir.path().join("c_dir")));
    assert!(marks.contains(&dir.path().join("a.txt")));
    assert!(marks.contains(&dir.path().join("b.txt")));
}

#[test]
fn dragging_over_already_marked_rows_toggles_them_back_off() {
    let (_dir, mut app) = mouse_test_app();
    // First gesture: mark c_dir and a.txt.
    click(&mut app, 5, 2);
    drag(&mut app, 5, 3);
    release(&mut app, 5, 3);
    assert_eq!(app.panes[0].marks.len(), 2);

    // Second, independent gesture over the exact same rows: each
    // gesture snapshots the marks fresh at its own mouse-down, so
    // toggling an already-marked row unmarks it — "drag to deselect".
    click(&mut app, 5, 2);
    drag(&mut app, 5, 3);
    release(&mut app, 5, 3);

    assert!(
        app.panes[0].marks.is_empty(),
        "marks: {:?}",
        app.panes[0].marks
    );
}

#[test]
fn drag_retreat_reverts_rows_that_leave_the_range() {
    // The defining behavior of the live rubber-band model: a row
    // toggled by extending the range, then left behind by retreating,
    // must revert to its pre-drag state — not stay toggled forever.
    let (dir, mut app) = mouse_test_app();
    click(&mut app, 5, 2); // origin: c_dir (index 1)
    drag(&mut app, 5, 3); // extend to a.txt (index 2): both toggled ON
    assert_eq!(app.panes[0].marks.len(), 2);

    drag(&mut app, 5, 2); // retreat back onto the origin: a.txt leaves the range
    release(&mut app, 5, 2);

    let marks = &app.panes[0].marks;
    assert_eq!(marks.len(), 1, "marks: {marks:?}");
    assert!(marks.contains(&dir.path().join("c_dir")));
    assert!(
        !marks.contains(&dir.path().join("a.txt")),
        "a.txt must revert to unmarked once it leaves the range"
    );
}

#[test]
fn drag_direction_reversal_across_the_origin_recomputes_the_range() {
    let (dir, mut app) = mouse_test_app();
    click(&mut app, 5, 3); // origin: a.txt (index 2)
    drag(&mut app, 5, 4); // extend down to b.txt (index 3)
    assert_eq!(
        app.panes[0].marks.len(),
        2,
        "a.txt+b.txt should be marked: {:?}",
        app.panes[0].marks
    );

    // Reverse direction, sweeping past the origin up to c_dir.
    drag(&mut app, 5, 2);
    release(&mut app, 5, 2);

    let marks = &app.panes[0].marks;
    assert_eq!(marks.len(), 2, "marks: {marks:?}");
    assert!(
        marks.contains(&dir.path().join("c_dir")),
        "newly entered the range"
    );
    assert!(
        marks.contains(&dir.path().join("a.txt")),
        "the origin row, still in range on both sides of the reversal"
    );
    assert!(
        !marks.contains(&dir.path().join("b.txt")),
        "left the range on the reversal, must revert"
    );
}

#[test]
fn drag_over_a_row_marked_before_the_drag_toggles_off_then_reverts_on_retreat() {
    let (dir, mut app) = mouse_test_app();
    app.panes[0].marks.insert(dir.path().join("b.txt")); // pre-marked, index 3

    click(&mut app, 5, 2); // origin: c_dir (index 1)
    drag(&mut app, 5, 4); // extend down through b.txt (index 3)

    let marks = &app.panes[0].marks;
    assert!(
        !marks.contains(&dir.path().join("b.txt")),
        "a row marked before the drag must flip off when swept over: {marks:?}"
    );
    assert!(marks.contains(&dir.path().join("c_dir")));
    assert!(marks.contains(&dir.path().join("a.txt")));

    drag(&mut app, 5, 3); // retreat: b.txt (index 3) leaves the range
    release(&mut app, 5, 3);

    let marks = &app.panes[0].marks;
    assert!(
        marks.contains(&dir.path().join("b.txt")),
        "b.txt must revert to its pre-drag marked state once it leaves the range: {marks:?}"
    );
    assert!(marks.contains(&dir.path().join("c_dir")));
    assert!(marks.contains(&dir.path().join("a.txt")));
}

#[test]
fn plain_click_never_touches_existing_marks() {
    let (dir, mut app) = mouse_test_app();
    app.panes[0].marks.insert(dir.path().join("a.txt"));
    click(&mut app, 5, 2); // a plain click elsewhere, no drag event at all
    release(&mut app, 5, 2);
    assert_eq!(app.panes[0].marks.len(), 1);
    assert!(app.panes[0].marks.contains(&dir.path().join("a.txt")));
}

#[test]
fn drag_crossing_into_the_other_pane_does_not_mark_there_or_change_focus() {
    let (_dir, mut app) = mouse_test_app();
    click(&mut app, 5, 2); // start the drag in the left pane
    assert_eq!(app.active, ActivePane::Left);

    drag(&mut app, 25, 2); // pointer crosses into the right pane's area
    assert_eq!(
        app.active,
        ActivePane::Left,
        "focus must not change mid-drag"
    );
    assert!(
        app.panes[1].marks.is_empty(),
        "the pane the drag didn't start in must never get marked"
    );
}

#[test]
fn double_click_on_a_directory_navigates_into_it() {
    let (dir, mut app) = mouse_test_app();
    // Find c_dir's row by scanning visible entries.
    let idx = app.panes[0]
        .visible_entries()
        .iter()
        .position(|item| matches!(item, crate::pane::VisibleItem::Entry(e) if e.name == "c_dir"))
        .unwrap();
    let row_y = 1 + idx as u16;
    click(&mut app, 5, row_y);
    release(&mut app, 5, row_y);
    click(&mut app, 5, row_y); // second click, same row, immediately after
    assert_eq!(app.panes[0].cwd, dir.path().join("c_dir"));
}

#[test]
fn wheel_scroll_moves_the_cursor_of_the_pane_under_the_pointer_without_changing_focus() {
    let (_dir, mut app) = mouse_test_app();
    app.active = ActivePane::Left;
    app.panes[0].cursor = 0;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 5,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.panes[0].cursor, MOUSE_WHEEL_STEP.min(3));
    assert_eq!(app.active, ActivePane::Left);

    // Scroll over the *other* (currently inactive) pane: must move
    // that pane's cursor, and must still not change focus.
    app.panes[1].cursor = 0;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 25,
        row: 2,
        modifiers: KeyModifiers::NONE,
    });
    assert!(app.panes[1].cursor > 0);
    assert_eq!(app.active, ActivePane::Left);
}
