//! The background process-list worker: runs `ps` once, parses it with
//! `crate::process`, and reports one `TaskEvent::ProcessList` per run.
//!
//! Same shape as `git_status` (shell out, parse, send one structured event)
//! with one difference that shows up in the error handling: this worker is
//! re-spawned every couple of seconds while the view is open, so a failure
//! that `git_status` could afford to log once would arrive thirty times a
//! minute here. Failures therefore travel as `Err(String)` inside the
//! snapshot event — `App::apply_process_snapshot` puts them in the view's
//! footer and only logs when the message actually changes.

use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::process::{PS_ARGS, parse_ps_output};

use super::{TaskEvent, TaskId, finish_cancelled};

pub fn run_process_list(id: TaskId, tx: Sender<TaskEvent>, cancel: Arc<AtomicBool>) {
    if cancel.load(Ordering::Relaxed) {
        finish_cancelled(&tx, id);
        return;
    }

    let result = match Command::new("ps").args(PS_ARGS).output() {
        Err(err) => Err(format!("failed to run ps: {err}")),
        Ok(output) if !output.status.success() => Err(format!(
            "ps exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(output) => {
            // Lossy rather than strict UTF-8: a command line is whatever
            // bytes were exec'd, and one process with a mojibake argv is no
            // reason to lose the whole snapshot.
            let text = String::from_utf8_lossy(&output.stdout);
            let (procs, skipped) = parse_ps_output(&text);
            if skipped > 0 {
                // Worth one line: it means this platform's `ps` prints
                // something the parser doesn't know about, which is a bug
                // report rather than a transient condition.
                let _ = tx.send(TaskEvent::Log {
                    id,
                    line: format!("ps: skipped {skipped} unparsable line(s)"),
                });
            }
            Ok(procs)
        }
    };

    // Checked again after the run: a probe superseded while `ps` was
    // executing must not overwrite the newer snapshot. (`App` also drops
    // stale results by task id — this just avoids the wasted event.)
    if cancel.load(Ordering::Relaxed) {
        finish_cancelled(&tx, id);
        return;
    }

    let summary = match &result {
        Ok(procs) => Ok(format!("{} process(es)", procs.len())),
        Err(msg) => Err(msg.clone()),
    };
    let _ = tx.send(TaskEvent::ProcessList { id, result });
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: summary,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    fn task_id() -> TaskId {
        // `TaskId::next` is private to the module; a manager hands one out.
        let (tx, _rx) = channel();
        let manager = super::super::TaskManager::new(tx);
        manager.spawn_detached(|_, _, _| {}).0
    }

    #[test]
    #[cfg(unix)]
    fn running_ps_reports_a_snapshot_containing_this_test_process() {
        let (tx, rx) = channel();
        let id = task_id();
        run_process_list(id, tx, Arc::new(AtomicBool::new(false)));

        let events: Vec<_> = rx.try_iter().collect();
        let snapshot = events
            .iter()
            .find_map(|e| match e {
                TaskEvent::ProcessList { result, .. } => Some(result),
                _ => None,
            })
            .expect("a ProcessList event");
        let procs = snapshot.as_ref().expect("ps to succeed");
        assert!(
            procs.iter().any(|p| p.pid == std::process::id()),
            "the test process itself should be in the snapshot"
        );
        assert!(matches!(
            events.last(),
            Some(TaskEvent::Finished { result: Ok(_), .. })
        ));
        // No `Log` event means `parse_ps_output` read every line this
        // platform's `ps` actually produced — the one check here that
        // exercises real output rather than a hand-written fixture, and so
        // the one that would catch a column layout the parser doesn't know
        // about.
        assert!(
            !events.iter().any(|e| matches!(e, TaskEvent::Log { .. })),
            "every line of real ps output should parse: {events:?}"
        );
        // And the fields actually landed in the right columns: this process
        // is a descendant of something, has a command line, and has been
        // running for a parsable amount of time.
        let me = procs
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("this process");
        assert!(me.ppid > 0, "{me:?}");
        assert!(!me.command.is_empty(), "{me:?}");
        assert!(me.etime_secs.is_some(), "{me:?}");
    }

    #[test]
    fn a_cancelled_probe_reports_cancelled_without_a_snapshot() {
        let (tx, rx) = channel();
        run_process_list(task_id(), tx, Arc::new(AtomicBool::new(true)));

        let events: Vec<_> = rx.try_iter().collect();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TaskEvent::ProcessList { .. }))
        );
        assert!(matches!(
            events.as_slice(),
            [TaskEvent::Finished { result: Err(msg), .. }] if msg == "cancelled"
        ));
    }
}
