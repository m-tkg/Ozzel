//! Background task infrastructure: plain OS threads (no tokio — file I/O is
//! blocking anyway) that report progress over an `mpsc::Sender<TaskEvent>`,
//! tracked here so the UI can render a gauge per running task and the app
//! can offer a quit-while-busy confirmation.

pub mod archive;
pub mod copy_move;
pub mod delete;
pub mod dir_size;
pub mod git_status;
pub mod sync;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

/// A message from a background task's worker thread back to the main
/// thread, sent over the `mpsc::Sender<TaskEvent>` every `TaskManager::spawn`
/// closure receives a clone of. Lives here (rather than `event.rs`, where
/// it originally sat alongside `AppEvent`) so `event.rs` — the terminal-
/// input-normalization module — doesn't have to depend on `tasks` just for
/// this enum's own `TaskId` field; `event.rs` still depends on `tasks` for
/// `AppEvent::Task`'s payload (this type), which is unavoidable, but no
/// longer for anything this enum's *definition* needed.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskEvent {
    Progress {
        id: TaskId,
        done: u64,
        total: u64,
        detail: String,
    },
    Log {
        id: TaskId,
        line: String,
    },
    /// One directory's recursive size, from a `calc_dir_size` worker
    /// (`dir_size.rs`) — sent per directory as each finishes, so results
    /// land on screen incrementally. Structured (rather than stuffed into
    /// `Finished`'s summary string) because the receiver needs the actual
    /// `(path, bytes)` to stamp onto the pane's entry list.
    DirSize {
        id: TaskId,
        path: std::path::PathBuf,
        bytes: u64,
    },
    /// One directory's git status, from a `git_status` worker — `None`
    /// when `dir` isn't inside a git work tree (or `git` isn't
    /// installed), which clears any stale status off the pane. Routed by
    /// `App::handle_task_event` onto the pane whose refresh spawned it,
    /// same shape as `DirSize`.
    GitStatus {
        id: TaskId,
        dir: std::path::PathBuf,
        status: Option<crate::git::GitDirStatus>,
    },
    Finished {
        id: TaskId,
        result: Result<String, String>,
    },
}

/// Chunked read/write buffer size shared by every worker that streams file
/// bytes through a manual `Read`/`Write` loop: copy/move's chunked-file
/// path (`copy_move.rs`) and zip/tar create/extract (`archive.rs`). Was
/// declared identically in both files; one value now.
pub(crate) const CHUNK_SIZE: usize = 1024 * 1024; // 1 MiB
/// `Throttle` interval shared by every worker's `Progress` events — how
/// often the UI's gauge actually needs to move, not how often a worker's
/// inner loop makes forward progress. Was declared identically in
/// `copy_move.rs` and `archive.rs`; one value now.
pub(crate) const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Identifies one spawned task. Ordered by creation order (an incrementing
/// counter), so a `BTreeMap<TaskId, _>` iterates tasks oldest-first —
/// keeping gauge rows in a stable order across frames instead of the
/// arbitrary order a `HashMap` would give.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u64);

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

impl TaskId {
    fn next() -> Self {
        TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// What the UI needs to know about a task that's still running.
pub struct RunningTask {
    pub desc: String,
    pub progress: (u64, u64),
    pub detail: String,
    /// Set by `App::cancel_running_tasks` (`Action::CancelTasks`); every
    /// worker (copy/move/delete/zip/unzip/extract) polls this between
    /// files/chunks and unwinds to `Finished(Err("cancelled"))` when it's
    /// set.
    pub cancel: Arc<AtomicBool>,
}

pub struct TaskManager {
    pub running: BTreeMap<TaskId, RunningTask>,
    tx: Sender<TaskEvent>,
}

impl TaskManager {
    pub fn new(tx: Sender<TaskEvent>) -> Self {
        Self {
            running: BTreeMap::new(),
            tx,
        }
    }

    /// Spawns `work` on a new OS thread and tracks it as running. `work`
    /// receives its own `TaskId`, a clone of the event sender, and a cancel
    /// flag it should check between files/chunks and abort on — no UI
    /// binds the flag to a key yet, but every worker already honors it.
    pub fn spawn<F>(&mut self, desc: impl Into<String>, work: F) -> TaskId
    where
        F: FnOnce(TaskId, Sender<TaskEvent>, Arc<AtomicBool>) + Send + 'static,
    {
        let id = TaskId::next();
        let cancel = Arc::new(AtomicBool::new(false));
        self.running.insert(
            id,
            RunningTask {
                desc: desc.into(),
                progress: (0, 0),
                detail: String::new(),
                cancel: cancel.clone(),
            },
        );

        let tx = self.tx.clone();
        thread::spawn(move || work(id, tx, cancel));

        id
    }

    /// Like `spawn`, but the task is *not* tracked in `running`: no
    /// progress gauge, no "N running" in the status bar, no
    /// quit-while-busy confirmation, and `cancel_tasks` (C-k) never
    /// touches it. For passive background probes (git status) whose
    /// lifecycle the user never manages — the caller keeps the returned
    /// cancel flag itself if it ever wants to abort one.
    pub fn spawn_detached<F>(&self, work: F) -> (TaskId, Arc<AtomicBool>)
    where
        F: FnOnce(TaskId, Sender<TaskEvent>, Arc<AtomicBool>) + Send + 'static,
    {
        let id = TaskId::next();
        let cancel = Arc::new(AtomicBool::new(false));
        let tx = self.tx.clone();
        let worker_cancel = cancel.clone();
        thread::spawn(move || work(id, tx, worker_cancel));
        (id, cancel)
    }

    /// Applies a `Progress`/`Finished` event to `running` (a `Finished`
    /// removes the task). Returns a one-line summary and whether it's an
    /// error, for the caller to log — `None` for anything that doesn't
    /// warrant a log line (`Progress`, and `Log` which the caller logs
    /// directly from the event itself).
    pub fn apply_event(&mut self, event: &TaskEvent) -> Option<(String, bool)> {
        match event {
            TaskEvent::Progress {
                id,
                done,
                total,
                detail,
            } => {
                if let Some(task) = self.running.get_mut(id) {
                    task.progress = (*done, *total);
                    task.detail = detail.clone();
                }
                None
            }
            TaskEvent::Log { .. } => None,
            // Routed by `App::handle_task_event` onto the pane it belongs
            // to; nothing to update in `running` and nothing to log here.
            TaskEvent::DirSize { .. } => None,
            TaskEvent::GitStatus { .. } => None,
            TaskEvent::Finished { id, result } => {
                let desc = self
                    .running
                    .remove(id)
                    .map(|t| t.desc)
                    .unwrap_or_else(|| "task".to_string());
                match result {
                    Ok(summary) => Some((format!("{desc}: {summary}"), false)),
                    Err(summary) => Some((format!("{desc}: {summary}"), true)),
                }
            }
        }
    }
}

/// Rate-limits how often progress is reported: `allow` returns `true` (and
/// resets the window) the first time it's called and any time at least
/// `interval` has passed since the last `true`, keeping the event channel
/// from being flooded by a tight per-chunk copy loop.
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.duration_since(last) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// Sends a `Log` event for `id`, silently dropping the vanishingly rare
/// send failure (the receiver hanging up mid-task, e.g. the UI thread
/// exiting) the same way every other event send in `tasks/*` already does.
/// Every worker's non-fatal per-entry warning (a skipped symlink, an unsafe
/// archive path, a target missing from an archive, ...) goes through this
/// rather than each call site re-spelling `tx.send(TaskEvent::Log { .. })`.
pub(crate) fn send_log(tx: &Sender<TaskEvent>, id: TaskId, line: String) {
    let _ = tx.send(TaskEvent::Log { id, line });
}

/// Sends the fixed `Finished(Err("cancelled"))` every worker reports the
/// same way the moment its cancel flag is observed set.
pub(crate) fn finish_cancelled(tx: &Sender<TaskEvent>, id: TaskId) {
    let _ = tx.send(TaskEvent::Finished {
        id,
        result: Err("cancelled".to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_allows_first_call_then_blocks_within_interval() {
        let mut throttle = Throttle::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert!(throttle.allow(t0), "first call is always allowed");
        assert!(!throttle.allow(t0 + Duration::from_millis(50)));
        assert!(!throttle.allow(t0 + Duration::from_millis(99)));
    }

    #[test]
    fn throttle_allows_again_once_interval_elapses() {
        let mut throttle = Throttle::new(Duration::from_millis(100));
        let t0 = Instant::now();
        assert!(throttle.allow(t0));
        assert!(throttle.allow(t0 + Duration::from_millis(100)));
        // The window resets from the last *allowed* instant, not the first.
        assert!(!throttle.allow(t0 + Duration::from_millis(150)));
        assert!(throttle.allow(t0 + Duration::from_millis(201)));
    }

    #[test]
    fn apply_event_progress_updates_running_task() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut manager = TaskManager::new(tx);
        let id = manager.spawn("noop", |_, _, _| {});
        // Give the spawned thread a moment; its body is a no-op so this is
        // only to avoid flakiness on slow CI, not correctness-critical.
        std::thread::sleep(Duration::from_millis(10));

        manager.apply_event(&TaskEvent::Progress {
            id,
            done: 5,
            total: 10,
            detail: "file.txt".to_string(),
        });
        assert_eq!(manager.running.get(&id).unwrap().progress, (5, 10));
        assert_eq!(manager.running.get(&id).unwrap().detail, "file.txt");
    }

    #[test]
    fn apply_event_finished_removes_task_and_summarizes() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut manager = TaskManager::new(tx);
        let id = manager.spawn("copy 1 item", |_, _, _| {});

        let (summary, is_error) = manager
            .apply_event(&TaskEvent::Finished {
                id,
                result: Ok("done".to_string()),
            })
            .unwrap();
        assert_eq!(summary, "copy 1 item: done");
        assert!(!is_error);
        assert!(!manager.running.contains_key(&id));
    }

    #[test]
    fn apply_event_finished_err_is_flagged_as_error() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut manager = TaskManager::new(tx);
        let id = manager.spawn("delete 1 item", |_, _, _| {});

        let (summary, is_error) = manager
            .apply_event(&TaskEvent::Finished {
                id,
                result: Err("permission denied".to_string()),
            })
            .unwrap();
        assert_eq!(summary, "delete 1 item: permission denied");
        assert!(is_error);
    }

    #[test]
    fn task_ids_order_by_creation() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut manager = TaskManager::new(tx);
        let first = manager.spawn("a", |_, _, _| {});
        let second = manager.spawn("b", |_, _, _| {});
        assert!(first < second);
    }
}
