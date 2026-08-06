//! Tests for the git-status plumbing: how `TaskEvent::GitStatus` results
//! are routed onto panes, and — most importantly — that a probe's
//! `Finished` event has none of the side effects a real file operation's
//! completion has (no reload, no mark clearing, no log line). Never
//! spawns a real `git` process: events are injected directly, with
//! `spawn_detached` no-op workers minting valid `TaskId`s.

use super::super::test_support::*;
use super::super::*;

use crate::git::{GitDirStatus, GitMarker};

fn status_for(branch: &str, dir: &Path, name: &str, marker: GitMarker) -> GitDirStatus {
    let mut statuses = HashMap::new();
    statuses.insert(dir.join(name), marker);
    GitDirStatus {
        branch: branch.to_string(),
        statuses,
    }
}

/// Registers a fake in-flight probe for the left pane and returns its id.
fn register_probe(app: &mut App) -> TaskId {
    let (id, cancel) = app.tasks.spawn_detached(|_, _, _| {});
    app.pending_git_status.insert(id, ActivePane::Left);
    app.latest_git_task[0] = Some((id, cancel));
    id
}

#[test]
fn git_status_result_lands_on_the_pane_that_asked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let id = register_probe(&mut app);

    let status = status_for("main", dir.path(), "a.txt", GitMarker::Modified);
    app.handle_event(AppEvent::Task(TaskEvent::GitStatus {
        id,
        dir: dir.path().to_path_buf(),
        status: Some(status.clone()),
    }));

    assert_eq!(app.panes[0].git.as_ref(), Some(&status));
    assert!(
        app.panes[1].git.is_none(),
        "only the asking pane is stamped"
    );
}

#[test]
fn stale_probe_result_is_dropped_when_a_newer_probe_exists() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let old_id = register_probe(&mut app);
    // A newer probe supersedes the old one.
    let _new_id = register_probe(&mut app);

    app.handle_event(AppEvent::Task(TaskEvent::GitStatus {
        id: old_id,
        dir: dir.path().to_path_buf(),
        status: Some(status_for("main", dir.path(), "a.txt", GitMarker::Added)),
    }));

    assert!(
        app.panes[0].git.is_none(),
        "a superseded probe's result must never be applied"
    );
}

#[test]
fn result_for_a_directory_the_pane_left_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());
    let id = register_probe(&mut app);

    let other_dir = dir.path().join("elsewhere");
    app.handle_event(AppEvent::Task(TaskEvent::GitStatus {
        id,
        dir: other_dir.clone(),
        status: Some(status_for("main", &other_dir, "a.txt", GitMarker::Added)),
    }));

    assert!(app.panes[0].git.is_none());
}

#[test]
fn git_probe_finished_does_not_reload_or_clear_marks_or_log() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.active_pane_mut().reload().unwrap();
    select_entry_named(&mut app, "a.txt");
    app.dispatch(Action::Mark);
    assert_eq!(app.active_pane().marks.len(), 1);
    let log_len_before = app.log.len();

    let id = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id,
        result: Ok("2 changed entr(ies)".to_string()),
    }));

    assert_eq!(
        app.active_pane().marks.len(),
        1,
        "a passive probe's completion must never clear marks"
    );
    assert_eq!(
        app.log.len(),
        log_len_before,
        "a successful probe must not log a completion line"
    );
    // The finished probe's bookkeeping is gone (the end-of-event sweep
    // may have already scheduled a fresh, unrelated probe — that's fine).
    assert!(!app.pending_git_status.contains_key(&id));
    assert_ne!(app.latest_git_task[0].as_ref().map(|(t, _)| *t), Some(id));
}

#[test]
fn git_probe_failure_logs_but_cancellation_stays_silent() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    let cancelled = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id: cancelled,
        result: Err("cancelled".to_string()),
    }));
    assert!(!app.log.iter().any(|l| l.message.contains("git status")));

    let failed = register_probe(&mut app);
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id: failed,
        result: Err("git status failed".to_string()),
    }));
    assert!(
        app.log
            .iter()
            .any(|l| l.is_error && l.message.contains("git status"))
    );
}

#[test]
fn navigation_schedules_a_probe_per_pane_and_show_git_status_off_disables_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(dir.path(), dir.path());

    // Any event (a Tick will do) triggers the initial probe for both panes.
    app.handle_event(AppEvent::Tick);
    assert!(app.latest_git_task[0].is_some());
    assert!(app.latest_git_task[1].is_some());
    assert_eq!(
        app.git_checked_dir[0].as_deref(),
        Some(app.panes[0].cwd.as_path())
    );

    // Same directory again: no new probe (the id is unchanged).
    let (first_id, _) = *app.latest_git_task[0]
        .as_ref()
        .map(|(id, c)| (*id, c.clone()))
        .as_ref()
        .unwrap();
    app.handle_event(AppEvent::Tick);
    assert_eq!(
        app.latest_git_task[0].as_ref().map(|(id, _)| *id),
        Some(first_id)
    );

    // Turning the setting off clears both the bookkeeping and the status.
    app.config.show_git_status = false;
    app.panes[0].set_git_status(Some(status_for(
        "main",
        dir.path(),
        "a.txt",
        GitMarker::Added,
    )));
    app.handle_event(AppEvent::Tick);
    assert!(app.git_checked_dir[0].is_none());
    assert!(app.panes[0].git.is_none());
}

#[test]
fn task_completion_reload_forces_a_git_reprobe() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
    let mut app = test_app(dir.path(), dir.path());
    app.handle_event(AppEvent::Tick);
    let first_id = app.latest_git_task[0].as_ref().map(|(id, _)| *id).unwrap();

    // A real (tracked) task finishing reloads both panes, which must
    // invalidate `git_checked_dir` — the next sweep spawns a fresh probe.
    let id = app.tasks.spawn("noop", |_, _, _| {});
    app.handle_event(AppEvent::Task(TaskEvent::Finished {
        id,
        result: Ok("done".to_string()),
    }));

    let second_id = app.latest_git_task[0].as_ref().map(|(id, _)| *id).unwrap();
    assert_ne!(first_id, second_id, "the reload must force a re-probe");
}
