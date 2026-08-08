//! Tests for the process manager's plumbing: how `TaskEvent::ProcessList`
//! snapshots reach the view, that a probe's `Finished` has none of the side
//! effects a real file operation's completion has, and the view's own keys
//! (sorting, cursor, kill confirmation).
//!
//! Almost none of these run `ps`: the mode is built directly and events are
//! injected, with `spawn_detached` no-op workers minting valid `TaskId`s —
//! so the whole file compiles and passes on Windows too, where the feature
//! itself doesn't exist. The exceptions are the two tests that go through
//! `begin_process_manager`/`r`, which are `#[cfg(unix)]`.

use super::super::test_support::*;
use super::super::*;

use crate::mode::{PendingKill, ProcessManagerState};
use crate::process::{ProcessInfo, ProcessSortKey, Signal};

fn sample(pid: u32, name: &str, cpu: f32) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: 1,
        user: "masaki".to_string(),
        cpu,
        mem: 0.0,
        rss_kib: 1024,
        state: "S".to_string(),
        etime: "00:10".to_string(),
        etime_secs: Some(10),
        command: format!("/bin/{name}"),
        name: name.to_string(),
    }
}

/// Opens the view directly, without going through `begin_process_manager`
/// (which would run a real `ps`) — and with auto-refresh off, so the
/// end-of-event sweep doesn't spawn probes underneath the assertions.
fn open_view(app: &mut App, processes: Vec<ProcessInfo>) {
    app.config.process_auto_refresh = false;
    app.mode = Mode::ProcessManager {
        state: Box::new(ProcessManagerState {
            processes,
            sort_key: ProcessSortKey::Cpu,
            ascending: false,
            cursor: 0,
            loading: false,
            error: None,
            updated_at: None,
            pending_kill: None,
        }),
    };
}

fn state_of(app: &App) -> &ProcessManagerState {
    match &app.mode {
        Mode::ProcessManager { state } => state,
        other => panic!("expected Mode::ProcessManager, got {other:?}"),
    }
}

/// Registers a fake in-flight probe and returns its id plus its cancel flag.
fn register_probe(app: &mut App) -> (TaskId, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let (id, cancel) = app.tasks.spawn_detached(|_, _, _| {});
    app.pending_process_list.insert(id);
    app.latest_process_probe = Some((id, cancel.clone()));
    app.process_probed_at = Some(std::time::Instant::now());
    (id, cancel)
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(AppEvent::Input(code, KeyModifiers::NONE));
}

/// Stands in for what `ui::process_view::render` reports back: rows start
/// at y=2 (under the title and column header) and the window starts at
/// `start`.
fn set_process_layout(app: &mut App, start: usize, height: u16) {
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 80,
        height: height + 3,
    };
    app.process_layout = Some(PaneLayout {
        area,
        rows_area: ratatui::layout::Rect {
            x: 0,
            y: 2,
            width: 80,
            height,
        },
        start,
    });
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    app.handle_event(AppEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
}

fn pids(app: &App) -> Vec<u32> {
    state_of(app).processes.iter().map(|p| p.pid).collect()
}

#[test]
#[cfg(unix)]
fn opening_the_process_manager_shows_the_loading_state_and_spawns_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.config.process_auto_refresh = false;

    app.dispatch(Action::ProcessManager);

    assert!(state_of(&app).loading);
    assert!(state_of(&app).processes.is_empty());
    assert!(
        app.latest_process_probe.is_some(),
        "opening must not wait for the refresh interval"
    );
}

#[test]
#[cfg(not(unix))]
fn the_process_manager_action_logs_unsupported_and_stays_in_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::ProcessManager);

    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("not supported"))
    );
    assert!(app.latest_process_probe.is_none());
}

#[test]
fn a_process_list_event_replaces_the_snapshot_and_marks_the_frame_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    let (id, _) = register_probe(&mut app);
    app.needs_redraw = false;

    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Ok(vec![sample(1, "a", 1.0), sample(2, "b", 9.0)]),
    }));

    // Sorted by the state's %CPU-descending default on the way in.
    assert_eq!(pids(&app), vec![2, 1]);
    assert!(!state_of(&app).loading);
    assert!(state_of(&app).updated_at.is_some());
    assert!(app.needs_redraw, "a new snapshot has to reach the screen");
}

#[test]
fn a_stale_probes_snapshot_is_dropped_once_a_newer_probe_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    let (old_id, _) = register_probe(&mut app);
    let (_new_id, _) = register_probe(&mut app);

    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id: old_id,
        result: Ok(vec![sample(1, "a", 1.0)]),
    }));

    assert!(
        state_of(&app).processes.is_empty(),
        "a superseded probe's snapshot must never be applied"
    );
}

#[test]
fn a_process_probe_finishing_never_reloads_the_panes_or_clears_marks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");
    app.dispatch(Action::Mark);
    assert_eq!(app.active_pane().marks.len(), 1);
    open_view(&mut app, Vec::new());
    let log_len_before = app.log.len();

    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id,
        result: Ok("412 process(es)".to_string()),
    }));

    assert_eq!(
        app.active_pane().marks.len(),
        1,
        "a passive probe's completion must never clear marks"
    );
    assert_eq!(
        app.log.len(),
        log_len_before,
        "a probe's completion must not log a line"
    );
    assert!(!app.pending_process_list.contains(&id));
    assert!(app.latest_process_probe.is_none());
}

#[test]
fn a_probe_failing_is_not_logged_a_second_time_by_its_finished_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    let log_len_before = app.log.len();

    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id,
        result: Err("failed to run ps: No such file or directory".to_string()),
    }));

    assert_eq!(
        app.log.len(),
        log_len_before,
        "the snapshot event already reported this; Finished must stay silent"
    );
}

#[test]
fn a_refresh_keeps_the_cursor_on_the_same_pid_even_when_rows_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(
        &mut app,
        vec![sample(9, "busy", 9.0), sample(5, "idle", 0.0)],
    );
    // Cursor onto pid 5, currently the second row.
    press(&mut app, KeyCode::Down);
    assert_eq!(state_of(&app).cursor, 1);

    let (id, _) = register_probe(&mut app);
    // The two swap places: 5 is now the busy one.
    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Ok(vec![sample(9, "busy", 0.0), sample(5, "idle", 9.0)]),
    }));

    assert_eq!(pids(&app), vec![5, 9]);
    assert_eq!(
        state_of(&app).cursor,
        0,
        "the cursor follows its process, not its row number"
    );
}

#[test]
fn a_refresh_clamps_the_cursor_when_the_process_under_it_has_exited() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(
        &mut app,
        vec![
            sample(1, "a", 3.0),
            sample(2, "b", 2.0),
            sample(3, "c", 1.0),
        ],
    );
    press(&mut app, KeyCode::End);
    assert_eq!(state_of(&app).cursor, 2);

    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Ok(vec![sample(1, "a", 3.0)]),
    }));

    assert_eq!(state_of(&app).cursor, 0);
}

#[test]
fn a_ps_failure_lands_in_the_footer_and_only_logs_once_per_distinct_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 1.0)]);

    for _ in 0..3 {
        let (id, _) = register_probe(&mut app);
        app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
            id,
            result: Err("ps exited with 1".to_string()),
        }));
    }

    assert_eq!(state_of(&app).error.as_deref(), Some("ps exited with 1"));
    assert_eq!(
        state_of(&app).processes.len(),
        1,
        "the last good snapshot stays on screen"
    );
    let logged = app
        .log
        .iter()
        .filter(|l| l.message.contains("ps exited with 1"))
        .count();
    assert_eq!(logged, 1, "a repeating failure must not flood the log");

    // A *different* message is news again.
    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Err("failed to run ps".to_string()),
    }));
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("failed to run ps"))
    );
}

#[test]
fn a_successful_snapshot_clears_a_previous_failure_from_the_footer() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());

    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Err("ps exited with 1".to_string()),
    }));
    let (id, _) = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::ProcessList {
        id,
        result: Ok(vec![sample(1, "a", 1.0)]),
    }));

    assert!(state_of(&app).error.is_none());
}

#[test]
fn pressing_a_sort_key_reorders_the_list_and_pressing_it_again_reverses_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(9, "a", 1.0), sample(2, "b", 5.0)]);

    press(&mut app, KeyCode::Char('p'));
    assert_eq!(pids(&app), vec![2, 9], "pid ascending by default");
    assert!(state_of(&app).ascending);

    press(&mut app, KeyCode::Char('p'));
    assert_eq!(pids(&app), vec![9, 2], "the same key reverses");
    assert!(!state_of(&app).ascending);

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(pids(&app), vec![2, 9], "%cpu starts descending");
    assert_eq!(state_of(&app).sort_key, ProcessSortKey::Cpu);
}

#[test]
fn re_sorting_keeps_the_cursor_on_the_same_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(9, "a", 5.0), sample(2, "b", 1.0)]);
    assert_eq!(state_of(&app).cursor, 0); // pid 9

    press(&mut app, KeyCode::Char('p'));

    assert_eq!(pids(&app), vec![2, 9]);
    assert_eq!(state_of(&app).cursor, 1, "still on pid 9");
}

#[test]
fn the_sort_keys_are_matched_before_the_keymaps_cursor_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    // `c` now means cursor_down everywhere the keymap is consulted.
    app.config
        .keys
        .insert("c".to_string(), "cursor_down".to_string());
    app.keymap = build_keymap(&app.config).unwrap();
    open_view(&mut app, vec![sample(9, "a", 1.0), sample(2, "b", 5.0)]);

    press(&mut app, KeyCode::Char('c'));

    assert_eq!(state_of(&app).cursor, 0, "the view's own key wins");
    assert_eq!(state_of(&app).sort_key, ProcessSortKey::Cpu);
}

#[test]
fn cursor_movement_follows_a_rebound_cursor_down_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);

    // `k` is cursor_down in the default ijkl layout, and isn't one of the
    // view's own keys.
    press(&mut app, KeyCode::Char('k'));

    assert_eq!(state_of(&app).cursor, 1);
}

#[test]
fn the_cursor_clamps_at_both_ends_instead_of_wrapping() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);

    press(&mut app, KeyCode::Up);
    assert_eq!(state_of(&app).cursor, 0);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(state_of(&app).cursor, 1);
    press(&mut app, KeyCode::Down);
    assert_eq!(state_of(&app).cursor, 1);
}

#[test]
fn x_opens_a_kill_confirmation_naming_the_process_and_esc_cancels_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(4242, "sleep", 0.0)]);

    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        state_of(&app).pending_kill,
        Some(PendingKill {
            pid: 4242,
            name: "sleep".to_string(),
            signal: Signal::Term,
        })
    );

    // While the question is up, the list's own keys are inert.
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(state_of(&app).sort_key, ProcessSortKey::Cpu);

    press(&mut app, KeyCode::Esc);
    assert!(state_of(&app).pending_kill.is_none());
    assert!(
        matches!(app.mode, Mode::ProcessManager { .. }),
        "cancelling the kill must not close the view"
    );
}

#[test]
fn capital_x_asks_for_sigkill_instead() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(4242, "sleep", 0.0)]);

    press(&mut app, KeyCode::Char('X'));

    assert_eq!(
        state_of(&app).pending_kill.as_ref().map(|k| k.signal),
        Some(Signal::Kill)
    );
}

#[test]
fn refusing_to_signal_ozzels_own_pid_logs_an_error_and_leaves_the_view_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(std::process::id(), "ozzel", 0.0)]);

    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));

    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("refusing to kill ozzel itself"))
    );
    assert!(matches!(app.mode, Mode::ProcessManager { .. }));
    assert!(state_of(&app).pending_kill.is_none());
}

#[test]
fn refusing_to_signal_pid_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "init", 0.0)]);

    press(&mut app, KeyCode::Char('X'));
    press(&mut app, KeyCode::Char('y'));

    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("refusing to signal pid 1"))
    );
}

#[test]
fn q_and_esc_both_close_the_process_manager_back_to_normal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    open_view(&mut app, Vec::new());
    press(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.mode, Mode::Normal));

    open_view(&mut app, Vec::new());
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn closing_the_process_manager_cancels_the_in_flight_probe() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    let (_, cancel) = register_probe(&mut app);

    press(&mut app, KeyCode::Char('q'));

    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
    assert!(app.latest_process_probe.is_none());
}

#[test]
fn no_probe_is_spawned_while_the_process_manager_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    assert!(app.config.process_auto_refresh, "the default is on");

    for _ in 0..3 {
        app.handle_event(AppEvent::Tick);
    }

    assert!(app.latest_process_probe.is_none());
    assert!(app.process_probed_at.is_none());
}

#[test]
fn no_second_probe_is_spawned_while_one_is_still_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    app.config.process_auto_refresh = true;
    let (id, _) = register_probe(&mut app);
    // Long past due, so only the in-flight check can hold the next one back.
    app.process_probed_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));

    app.handle_event(AppEvent::Tick);

    assert_eq!(app.latest_process_probe.as_ref().map(|(t, _)| *t), Some(id));
}

#[test]
fn process_auto_refresh_false_never_spawns_a_periodic_probe() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new()); // sets process_auto_refresh = false

    for _ in 0..3 {
        app.handle_event(AppEvent::Tick);
    }

    assert!(app.latest_process_probe.is_none());
}

#[test]
fn clicking_a_row_moves_the_cursor_onto_that_process() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(
        &mut app,
        vec![
            sample(1, "a", 3.0),
            sample(2, "b", 2.0),
            sample(3, "c", 1.0),
        ],
    );
    set_process_layout(&mut app, 0, 6);

    // Rows start at y=2, so the third row is y=4.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 4);

    assert_eq!(state_of(&app).cursor, 2);
}

#[test]
fn clicking_a_row_honors_the_scroll_offset_the_frame_drew_with() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let procs: Vec<_> = (0..50)
        .map(|i| sample(100 + i, "p", 50.0 - i as f32))
        .collect();
    open_view(&mut app, procs);
    set_process_layout(&mut app, 20, 6);

    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 2);

    assert_eq!(state_of(&app).cursor, 20, "the first drawn row is index 20");
}

#[test]
fn clicking_the_title_header_or_footer_leaves_the_cursor_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);
    set_process_layout(&mut app, 0, 6);
    press(&mut app, KeyCode::Down);
    assert_eq!(state_of(&app).cursor, 1);

    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 0); // title
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 1); // header
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 9); // below the rows

    assert_eq!(state_of(&app).cursor, 1);
}

#[test]
fn clicking_the_blank_space_past_the_last_process_leaves_the_cursor_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);
    set_process_layout(&mut app, 0, 6);

    // Inside rows_area, but past the second (and last) process.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 6);

    assert_eq!(state_of(&app).cursor, 0);
}

/// Inverted, matching a pane's wheel: up moves down the list. See
/// `App::handle_mouse_wheel`.
#[test]
fn the_wheel_scrolls_the_cursor_and_clamps_at_both_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let procs: Vec<_> = (0..10)
        .map(|i| sample(100 + i, "p", 10.0 - i as f32))
        .collect();
    open_view(&mut app, procs);
    set_process_layout(&mut app, 0, 6);

    mouse(&mut app, MouseEventKind::ScrollUp, 10, 4);
    assert_eq!(state_of(&app).cursor, 3);
    mouse(&mut app, MouseEventKind::ScrollDown, 10, 4);
    assert_eq!(state_of(&app).cursor, 0);
    mouse(&mut app, MouseEventKind::ScrollDown, 10, 4);
    assert_eq!(state_of(&app).cursor, 0, "clamped at the top");

    for _ in 0..10 {
        mouse(&mut app, MouseEventKind::ScrollUp, 10, 4);
    }
    assert_eq!(state_of(&app).cursor, 9, "clamped at the bottom");
}

#[test]
fn the_wheel_over_an_empty_list_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());
    set_process_layout(&mut app, 0, 6);

    mouse(&mut app, MouseEventKind::ScrollUp, 10, 4);
    mouse(&mut app, MouseEventKind::ScrollDown, 10, 4);

    assert_eq!(state_of(&app).cursor, 0);
}

#[test]
fn the_mouse_is_ignored_while_a_kill_confirmation_is_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);
    set_process_layout(&mut app, 0, 6);
    press(&mut app, KeyCode::Char('x'));

    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 3);
    mouse(&mut app, MouseEventKind::ScrollUp, 10, 4);

    assert_eq!(
        state_of(&app).cursor,
        0,
        "the question owns the screen until it is answered"
    );
    assert_eq!(state_of(&app).pending_kill.as_ref().map(|k| k.pid), Some(1));
}

#[test]
fn a_double_click_never_kills_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, vec![sample(1, "a", 3.0), sample(2, "b", 2.0)]);
    set_process_layout(&mut app, 0, 6);

    for _ in 0..2 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, 3);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 10, 3);
    }

    assert_eq!(state_of(&app).cursor, 1, "it only ever moves the cursor");
    assert!(state_of(&app).pending_kill.is_none());
}

#[test]
fn mouse_capture_stays_on_in_the_process_manager() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new());

    assert!(
        app.wants_mouse_capture(),
        "unlike the viewer/log/help reading modes, this one is clickable"
    );
}

#[test]
#[cfg(unix)]
fn r_refreshes_even_with_auto_refresh_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    open_view(&mut app, Vec::new()); // sets process_auto_refresh = false

    press(&mut app, KeyCode::Char('r'));

    assert!(app.latest_process_probe.is_some());
}
