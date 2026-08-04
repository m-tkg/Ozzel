//! Background task infrastructure: plain OS threads (no tokio — file I/O is
//! blocking anyway) that report progress over an `mpsc::Sender<TaskEvent>`,
//! tracked here so the UI can render a gauge per running task and the app
//! can offer a quit-while-busy confirmation.

pub mod archive;
pub mod copy_move;
pub mod delete;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crate::event::TaskEvent;

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
    #[allow(
        dead_code,
        reason = "cancel UI is explicitly post-MVP per the plan; every worker already checks this flag, so wiring a keybinding to it later needs no further threading changes"
    )]
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
