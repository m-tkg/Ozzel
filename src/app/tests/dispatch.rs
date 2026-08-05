use super::super::test_support::*;
use super::super::*;
use std::time::Duration;

#[test]
fn quit_action_confirms_by_default_then_quits_on_y() {
    // confirm_quit defaults to true: with nothing running, Quit must
    // now confirm rather than quit immediately.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    assert!(!app.should_quit);
    app.dispatch(Action::Quit);
    assert!(!app.should_quit, "must confirm before quitting by default");
    match &app.mode {
        Mode::Confirm { message, .. } => assert_eq!(message, "Quit ozzel? (y/n)"),
        other => panic!("expected Mode::Confirm, got {other:?}"),
    }

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(app.should_quit);
}

#[test]
fn quit_confirm_declined_keeps_the_app_running_when_nothing_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::Quit);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(!app.should_quit);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn quit_with_confirm_quit_false_and_no_tasks_quits_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            confirm_quit: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.dispatch(Action::Quit);
    assert!(app.should_quit, "confirm_quit=false must quit immediately");
}

#[test]
fn quit_tasks_running_confirm_is_unaffected_by_confirm_quit_false() {
    // The tasks-running confirm is unconditional — confirm_quit only
    // governs the "nothing running" case.
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            confirm_quit: false,
            ..Config::default()
        },
    )
    .unwrap();
    app.tasks.spawn("noop", |id, tx, _| {
        std::thread::sleep(Duration::from_millis(200));
        let _ = tx.send(TaskEvent::Finished {
            id,
            result: Ok("done".to_string()),
        });
    });

    app.dispatch(Action::Quit);
    assert!(
        !app.should_quit,
        "must still confirm when tasks are running, even with confirm_quit=false"
    );
    assert!(matches!(app.mode, Mode::Confirm { .. }));

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(app.should_quit);
}

#[test]
fn quit_with_running_task_asks_for_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.tasks.spawn("noop", |id, tx, _| {
        std::thread::sleep(Duration::from_millis(200));
        let _ = tx.send(TaskEvent::Finished {
            id,
            result: Ok("done".to_string()),
        });
    });

    app.dispatch(Action::Quit);
    assert!(!app.should_quit, "must not quit while a task is running");
    assert!(matches!(app.mode, Mode::Confirm { .. }));

    app.handle_event(AppEvent::Input(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(app.should_quit, "confirming quit-anyway must still quit");
}

#[test]
fn quit_confirmation_declined_keeps_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.tasks.spawn("noop", |id, tx, _| {
        std::thread::sleep(Duration::from_millis(200));
        let _ = tx.send(TaskEvent::Finished {
            id,
            result: Ok("done".to_string()),
        });
    });

    app.dispatch(Action::Quit);
    app.handle_event(AppEvent::Input(KeyCode::Char('n'), KeyModifiers::NONE));
    assert!(!app.should_quit);
    assert!(matches!(app.mode, Mode::Normal));
    wait_for_tasks_done(&mut app);
}

#[test]
fn cancel_tasks_action_sets_the_cancel_flag_on_every_running_task() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    // Two long-enough-sleeping no-op tasks so both are still in
    // `running` (and their threads haven't observed the flag yet) when
    // asserted below — `TaskManager::spawn` inserts into `running`
    // synchronously before the thread even starts, so there's no race
    // to get right here.
    let mut cancels: Vec<Arc<AtomicBool>> = Vec::new();
    for _ in 0..2 {
        let id = app.tasks.spawn("noop", |id, tx, cancel| {
            for _ in 0..100 {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(TaskEvent::Finished {
                        id,
                        result: Err("cancelled".to_string()),
                    });
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(TaskEvent::Finished {
                id,
                result: Ok("done".to_string()),
            });
        });
        cancels.push(app.tasks.running.get(&id).unwrap().cancel.clone());
    }
    assert_eq!(app.tasks.running.len(), 2);

    app.dispatch(Action::CancelTasks);

    for cancel in &cancels {
        assert!(
            cancel.load(Ordering::Relaxed),
            "every running task's cancel flag must be set"
        );
    }
    assert!(
        app.log
            .iter()
            .any(|l| l.message.contains("cancelling 2 task(s)")),
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );

    wait_for_tasks_done(&mut app);
    // Both worker threads must have actually observed the flag and
    // unwound to a cancelled Finished, not merely had the flag set —
    // the same `Finished(Err("cancelled"))` contract every real
    // `tasks::*` worker (copy/move/delete/zip/unzip/extract) reports.
    assert!(
        app.log
            .iter()
            .filter(|l| l.message.contains("cancelled"))
            .count()
            >= 2,
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[test]
fn cancel_tasks_action_logs_no_running_tasks_when_nothing_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::CancelTasks);

    assert!(
        app.log.iter().any(|l| l.message == "no running tasks"),
        "log: {:?}",
        app.log.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

#[test]
fn cancel_tasks_action_makes_a_real_copy_worker_finish_cancelled() {
    // End-to-end through a real `tasks::copy_move` worker (rather than
    // the synthetic loop above) — proves the flag
    // `App::cancel_running_tasks` sets is the exact same
    // `Arc<AtomicBool>` `copy_move::run_copy` polls, all the way
    // through `TaskManager::spawn`'s plumbing. The cancel flag is set
    // *before* spawning (same "pre-cancelled" pattern
    // `copy_move::tests::cancel_flag_set_before_start_aborts_immediately`
    // uses) rather than raced against a real in-flight copy, so this is
    // fully deterministic: `run_copy` checks the flag before touching
    // any file.
    let src_dir = tempfile::tempdir().unwrap();
    let dest_dir = tempfile::tempdir().unwrap();
    std::fs::write(src_dir.path().join("a.txt"), b"hello").unwrap();

    let mut app = test_app(src_dir.path(), dest_dir.path());
    let sources = vec![src_dir.path().join("a.txt")];
    let dest = dest_dir.path().to_path_buf();
    app.tasks.spawn("copy 1 item", move |id, tx, cancel| {
        // Give `cancel_running_tasks` below a chance to run first.
        std::thread::sleep(Duration::from_millis(50));
        crate::tasks::copy_move::run_copy(id, tx, cancel, sources, dest);
    });
    assert_eq!(app.tasks.running.len(), 1);

    app.dispatch(Action::CancelTasks);
    wait_for_tasks_done(&mut app);

    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("cancelled")),
        "log: {:?}",
        app.log
            .iter()
            .map(|l| (&l.message, l.is_error))
            .collect::<Vec<_>>()
    );
    assert!(
        !dest_dir.path().join("a.txt").exists(),
        "a task cancelled before it started copying must not have written anything"
    );
}

#[test]
fn switch_pane_toggles_active() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    assert_eq!(app.active, ActivePane::Left);
    app.dispatch(Action::SwitchPane);
    assert_eq!(app.active, ActivePane::Right);
}

#[test]
fn swap_panes_swaps_cwd() {
    let left_dir = tempfile::tempdir().unwrap();
    let right_dir = tempfile::tempdir().unwrap();
    let mut app = test_app(left_dir.path(), right_dir.path());
    app.dispatch(Action::SwapPanes);
    assert_eq!(app.panes[0].cwd, right_dir.path());
    assert_eq!(app.panes[1].cwd, left_dir.path());
}

#[test]
fn keymap_resolves_q_and_ctrl_c_to_quit() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path(), dir.path());
    assert_eq!(
        app.keymap.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
        Some(Action::Quit)
    );
    assert_eq!(
        app.keymap
            .resolve(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Some(Action::Quit)
    );
}

#[test]
fn wants_mouse_capture_is_true_in_normal_mode_when_mouse_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path(), dir.path());
    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.wants_mouse_capture());
}

#[test]
fn wants_mouse_capture_is_false_when_mouse_is_off_regardless_of_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        dir.path().to_path_buf(),
        dir.path().to_path_buf(),
        Config {
            mouse: false,
            ..Config::default()
        },
    )
    .unwrap();
    assert!(!app.wants_mouse_capture());
    app.dispatch(Action::Help);
    assert!(matches!(app.mode, Mode::Help { .. }));
    assert!(!app.wants_mouse_capture());
}

#[test]
fn wants_mouse_capture_is_false_in_viewer_log_and_help() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("readme.txt"), b"hello").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    move_cursor_onto(app.active_pane_mut(), "readme.txt");
    app.dispatch(Action::Open);
    assert!(matches!(app.mode, Mode::Viewer { .. }));
    assert!(!app.wants_mouse_capture(), "Viewer must disable capture");
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Normal));
    assert!(
        app.wants_mouse_capture(),
        "returning to Normal must re-enable capture"
    );

    app.dispatch(Action::ShowLog);
    assert!(matches!(app.mode, Mode::Log { .. }));
    assert!(!app.wants_mouse_capture(), "Log view must disable capture");
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(app.wants_mouse_capture());

    app.dispatch(Action::Help);
    assert!(matches!(app.mode, Mode::Help { .. }));
    assert!(!app.wants_mouse_capture(), "Help must disable capture");
    app.handle_event(AppEvent::Input(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(app.wants_mouse_capture());
}

#[test]
fn wants_mouse_capture_stays_true_in_the_function_list_palette() {
    // An interactive picker, not a text-reading mode — explicitly
    // excluded from the disable list.
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.dispatch(Action::FunctionList);
    assert!(matches!(app.mode, Mode::FunctionList { .. }));
    assert!(app.wants_mouse_capture());
}

#[test]
fn wants_mouse_capture_stays_true_in_prompt_confirm_and_select_modes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"hi").unwrap();
    let mut app = test_app(dir.path(), dir.path());

    app.dispatch(Action::Mkdir);
    assert!(matches!(app.mode, Mode::Prompt { .. }));
    assert!(app.wants_mouse_capture());
    app.handle_event(AppEvent::Input(KeyCode::Esc, KeyModifiers::NONE));

    move_cursor_onto(app.active_pane_mut(), "a.txt");
    app.dispatch(Action::Delete);
    assert!(matches!(app.mode, Mode::Confirm { .. }));
    assert!(app.wants_mouse_capture());
}
