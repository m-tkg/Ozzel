//! Filesystem change notification for the two panes, so a file added,
//! changed or removed by Finder / another shell shows up without the user
//! pressing `C-r`.
//!
//! Deliberately thin: [`DirWatcher`] owns a `notify` watcher plus the
//! channel its background thread reports on, and knows nothing about panes
//! or reloading. `App` decides *what* to watch (each pane's cwd — see
//! `App::maybe_resync_watches`), *which* pane an event belongs to, and
//! *when* it is safe to act on one (`App::apply_fs_refresh`). Everything
//! here is best-effort: a watcher that can't be created, or a directory
//! that can't be registered, degrades to "no auto-refresh" with a logged
//! error rather than failing anything.
//!
//! Only *directories* are ever watched, always non-recursively. A pane
//! browsing an archive (Virtual Directory) keeps its `cwd` pointing at the
//! real directory *containing* that archive, so watching `cwd` covers it
//! too: a rewritten `project.zip` lands as an event on its parent
//! directory, and `VirtualDir::list`'s own mtime/len cache check turns the
//! resulting reload into a re-listing.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// A `notify` watcher over a small, explicit set of directories, with the
/// changed paths arriving on a channel the caller drains.
///
/// The `RecommendedWatcher` must be kept alive: dropping it unregisters
/// every watch and stops the backend thread, so this struct owning it is
/// load-bearing, not incidental.
pub struct DirWatcher {
    watcher: RecommendedWatcher,
    /// Exactly the directories currently registered, so `sync` can diff
    /// against a desired set instead of re-registering blindly. Also holds
    /// ones whose registration *failed*, so a directory that can't be
    /// watched (an unsupported filesystem, a hit watch limit) is
    /// complained about once rather than on every event loop pass.
    watched: Vec<Watched>,
    rx: Receiver<Vec<PathBuf>>,
}

/// One registered directory, in both the form the caller asked for and
/// the form events come back in.
struct Watched {
    /// Exactly what the caller passed to [`DirWatcher::sync`] — a pane's
    /// `cwd`. What [`DirWatcher::changed_dirs`] reports back, so the
    /// caller can compare against its own state without knowing anything
    /// about symlink resolution.
    requested: PathBuf,
    /// `requested` run through `fs::canonicalize`, the common form both
    /// sides of the comparison in [`DirWatcher::changed_dirs`] are put
    /// into. A caller's path and the backend's rarely spell the same
    /// directory the same way: on macOS `/var` is a symlink, so a cwd of
    /// `/var/folders/...` is delivered as `/private/var/folders/...`; on
    /// Windows the two differ by `\\?\` prefixing and 8.3 short names
    /// (`RUNNER~1` vs `runneradmin`). Comparing raw paths misses every
    /// event on both, and auto-refresh silently never fires. Falls back to
    /// `requested` when canonicalization fails (a directory that just
    /// vanished).
    canonical: PathBuf,
}

impl DirWatcher {
    /// Starts the backend watcher thread. Fails only when the platform
    /// backend itself can't be created (inotify instance limits, an
    /// unsupported environment) — the caller logs that and carries on
    /// without auto-refresh.
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            // A send failure means `App` (and so the receiver) is gone —
            // nothing to do but drop the event; the watcher is about to be
            // dropped too. Errors reported by the backend itself are
            // likewise dropped rather than surfaced: they are per-event
            // and non-fatal, and this closure runs on the watcher's own
            // thread, with no access to the log.
            if let Ok(event) = res {
                let _ = tx.send(event.paths);
            }
        })
        .context("failed to start the filesystem watcher")?;
        Ok(Self {
            watcher,
            watched: Vec::new(),
            rx,
        })
    }

    /// Registers exactly `desired` (deduplicated by the caller or not —
    /// this dedups), dropping any watch no longer wanted. Returns the
    /// paths that could not be registered, for the caller to log; those
    /// are still recorded as watched, so the failure isn't re-reported on
    /// every pass.
    pub fn sync(&mut self, desired: &[PathBuf]) -> Vec<PathBuf> {
        self.watched.retain(|w| {
            let keep = desired.contains(&w.requested);
            if !keep {
                // An unwatch failure only means the path is already gone
                // from the backend (a deleted directory, typically) — the
                // outcome we wanted anyway.
                let _ = self.watcher.unwatch(&w.requested);
            }
            keep
        });

        let mut failed = Vec::new();
        for path in desired {
            if self.watched.iter().any(|w| &w.requested == path) {
                continue;
            }
            if self
                .watcher
                .watch(path, RecursiveMode::NonRecursive)
                .is_err()
            {
                failed.push(path.clone());
            }
            self.watched.push(Watched {
                canonical: std::fs::canonicalize(path).unwrap_or_else(|_| path.clone()),
                requested: path.clone(),
            });
        }
        failed
    }

    /// Which watched directories have changed since the last call, named
    /// the way the caller asked for them (see `Watched::requested`),
    /// deduplicated, and drained without blocking.
    ///
    /// Each reported path is reduced to the directory the change happened
    /// *in* — its parent for the usual per-file event, or the path itself
    /// for a backend that reports the directory (FSEvents does) — and that
    /// directory is canonicalized before being compared, since neither
    /// side spells a path the same way (see `Watched::canonical`). Only
    /// the directory is canonicalized, never the changed entry itself: a
    /// deletion names something that no longer exists.
    ///
    /// A change *deeper* than one level below a watched directory belongs
    /// to no pane: watches are non-recursive, so anything deeper is only
    /// ever coalesced noise from a backend that watches by tree.
    pub fn changed_dirs(&self) -> Vec<PathBuf> {
        let mut changed: Vec<PathBuf> = Vec::new();
        for path in self.rx.try_iter().flatten() {
            for dir in [path.parent().map(Path::to_path_buf), Some(path)]
                .into_iter()
                .flatten()
            {
                let resolved = std::fs::canonicalize(&dir).unwrap_or(dir);
                for w in &self.watched {
                    if resolved == w.canonical && !changed.contains(&w.requested) {
                        changed.push(w.requested.clone());
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one test that actually exercises the OS backend, so the
    /// wiring (handler closure -> channel -> `changed_dirs`) can't silently
    /// rot.
    /// Everything above the `DirWatcher` boundary is tested without a real
    /// watcher — see `app::tests::auto_refresh`'s module comment.
    ///
    /// Polls for up to five seconds rather than sleeping a fixed amount:
    /// delivery latency differs per backend (inotify is near-instant,
    /// FSEvents coalesces), and a fixed sleep would be either flaky or
    /// needlessly slow.
    #[test]
    fn a_real_watch_reports_a_file_created_in_the_watched_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut watcher = DirWatcher::new().unwrap();
        assert!(
            watcher.sync(&[dir.path().to_path_buf()]).is_empty(),
            "a plain tempdir must be watchable"
        );

        std::fs::write(dir.path().join("created.txt"), b"hi").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            // Reported under the path that was *asked* for, not the
            // canonicalized one the backend uses — on macOS a tempdir is
            // `/var/folders/...` while events arrive as `/private/var/...`.
            if watcher.changed_dirs().contains(&dir.path().to_path_buf()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("no event for the created file arrived within 5s");
    }

    /// The premise `App`'s git-directory watch rests on: a file written
    /// one level inside a watched directory is reported as that
    /// *directory*. `git add`/`commit`/`checkout` write `.git/index` and
    /// `.git/HEAD`, and what has to come back is `.git` itself.
    #[test]
    fn a_file_written_inside_a_watched_directory_is_reported_as_that_directory() {
        let root = tempfile::tempdir().unwrap();
        let inner = root.path().join(".git");
        std::fs::create_dir(&inner).unwrap();
        let mut watcher = DirWatcher::new().unwrap();
        assert!(watcher.sync(std::slice::from_ref(&inner)).is_empty());

        std::fs::write(inner.join("index"), b"pretend index").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if watcher.changed_dirs().contains(&inner) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("no event for the file written inside the watched directory arrived within 5s");
    }

    #[test]
    fn sync_reports_a_path_that_cannot_be_watched_and_does_not_retry_it() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let mut watcher = DirWatcher::new().unwrap();

        assert_eq!(
            watcher.sync(std::slice::from_ref(&missing)),
            vec![missing.clone()]
        );
        assert!(
            watcher.sync(&[missing]).is_empty(),
            "a failure must be reported once, not on every pass"
        );
    }

    #[test]
    fn a_real_watch_ignores_a_change_in_a_sibling_directory() {
        let root = tempfile::tempdir().unwrap();
        let watched = root.path().join("watched");
        let other = root.path().join("other");
        std::fs::create_dir(&watched).unwrap();
        std::fs::create_dir(&other).unwrap();
        let mut watcher = DirWatcher::new().unwrap();
        assert!(watcher.sync(std::slice::from_ref(&watched)).is_empty());

        std::fs::write(other.join("created.txt"), b"hi").unwrap();
        // Then a change in the watched directory, as a barrier: once it
        // has been reported, anything the sibling was going to produce has
        // had at least as long to arrive.
        std::fs::write(watched.join("created.txt"), b"hi").unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let changed = watcher.changed_dirs();
            assert!(
                !changed.contains(&other),
                "a sibling directory is not watched"
            );
            if changed.contains(&watched) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("no event for the watched directory arrived within 5s");
    }
}
