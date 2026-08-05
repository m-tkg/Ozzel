//! Filesystem-mutating operations: mkdir/rename/delete, copy/move
//! (transfer) and duplicate, and zip/unzip/extract — each `begin_*`
//! (builds a prompt/confirmation) paired with the `spawn_*` that hands
//! the actual work to a background task (see `crate::tasks`), plus the
//! `Mode::Confirm`/`PendingOp` machinery (`handle_confirm_key`/
//! `execute_pending`) every one of them can end up going through first.
//! Split out of `app/mod.rs` (Phase 6, Step 5's "if there's room" bullet).

use super::*;

impl App {
    pub(super) fn begin_mkdir(&mut self) {
        if self.reject_if_virtual("mkdir") {
            return;
        }
        self.mode = Mode::Prompt {
            kind: PromptKind::Mkdir,
            input: LineEditor::new(),
        };
    }

    pub(super) fn begin_rename(&mut self) {
        if self.reject_if_virtual("rename") {
            return;
        }
        match self.active_pane().selected_entry_name() {
            Some(name) => {
                self.mode = Mode::Prompt {
                    kind: PromptKind::Rename { orig: name.clone() },
                    input: LineEditor::from_str(&name),
                };
            }
            None => self.log_error("no entry selected to rename"),
        }
    }

    pub(super) fn begin_delete(&mut self) {
        if self.reject_if_virtual("delete") {
            return;
        }
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to delete");
            return;
        }
        let message = format!("Delete {} item(s)? (y/n)", targets.len());
        self.confirm(message, PendingOp::Delete { targets });
    }

    /// Copy/Move always confirm by default (`config.confirm_operations`);
    /// with it set to `false`, a transfer with no filename collision skips
    /// straight to `spawn_transfer` — but a collision *always* confirms
    /// regardless, and when both apply it's a single combined dialog
    /// (`Copy 3 item(s) -> /dest? (2 will be overwritten) (y/n)`) rather
    /// than two sequential ones.
    pub(super) fn begin_transfer(&mut self, kind: TransferKind) {
        // Virtual Directory: `C` (Copy) is repurposed as extract (partial
        // extraction of the marked/cursor entries — see `begin_extract`);
        // `M` (Move) has no virtual-mode meaning at all ("move INTO/OUT-as-
        // move" is explicitly rejected) and always bails. Copying/moving
        // INTO a virtual pane (the *other* pane is the one browsing an
        // archive) is rejected the same way regardless of direction —
        // there's no real directory there to write into.
        if self.active_pane().is_virtual() {
            match kind {
                TransferKind::Copy => self.begin_extract(),
                TransferKind::Move => {
                    self.reject_if_virtual("move");
                }
            }
            return;
        }
        if self.other_pane().is_virtual() {
            self.log_error("cannot copy/move into a virtual directory (archive) pane");
            return;
        }

        let sources = self.active_pane().marked_or_cursor();
        if sources.is_empty() {
            self.log_error("no entry selected");
            return;
        }
        let dest_dir = self.other_pane().cwd.clone();
        if sources.iter().any(|src| dest_dir.starts_with(src)) {
            self.log_error("cannot copy/move a directory into itself or a descendant");
            return;
        }

        let collisions = copy_move::find_collisions(&sources, &dest_dir);
        if collisions.is_empty() && !self.config.confirm_operations {
            self.spawn_transfer(kind, sources, dest_dir);
            return;
        }

        let verb = match kind {
            TransferKind::Copy => "Copy",
            TransferKind::Move => "Move",
        };
        let mut message = format!(
            "{verb} {} item(s) -> {}?",
            sources.len(),
            dest_dir.display()
        );
        if !collisions.is_empty() {
            message.push_str(&format!(" ({} will be overwritten)", collisions.len()));
        }
        message.push_str(" (y/n)");

        self.confirm(
            message,
            PendingOp::Overwrite {
                kind,
                sources,
                dest_dir,
            },
        );
    }

    /// Hands the actual copy/move off to a background task (see
    /// `tasks::copy_move`); `dispatch`/`execute_pending` return immediately,
    /// and completion arrives later as a `TaskEvent::Finished` drained by
    /// `drain_tasks`.
    ///
    /// Logs one `{verb}: {src} -> {dest}` line per source *before*
    /// spawning — enumerated up front (rather than per-file as the task
    /// itself progresses) so the log is an atomic, complete record of
    /// exactly what was asked for at the moment the operation started,
    /// never a partial list if something in the batch later fails or gets
    /// cancelled mid-flight. For a large batch this can add many lines to
    /// the (capacity-capped) log — intentional; `L` opens the full
    /// scrollable log view for exactly this case.
    fn spawn_transfer(&mut self, kind: TransferKind, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        let verb = match kind {
            TransferKind::Copy => "copy",
            TransferKind::Move => "move",
        };
        for src in &sources {
            let dest = src
                .file_name()
                .map(|name| dest_dir.join(name))
                .unwrap_or_else(|| dest_dir.clone());
            self.log_info(format!("{verb}: {} -> {}", src.display(), dest.display()));
        }
        let desc = format!("{verb} {} item(s) to {}", sources.len(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| match kind {
            TransferKind::Copy => copy_move::run_copy(id, tx, cancel, sources, dest_dir),
            TransferKind::Move => copy_move::run_move(id, tx, cancel, sources, dest_dir),
        });
    }

    /// Hands the actual delete off to a background task (see
    /// `tasks::delete`); see `spawn_transfer` for the completion story.
    /// Captures the post-delete cursor anchor (see `Pane::anchor_above`)
    /// from the active pane *before* the delete runs — `targets` are all
    /// still present in `visible_entries()` at this point, which is
    /// exactly what `anchor_above` needs — then spawns the task and
    /// remembers the anchor against its own `TaskId` for
    /// `handle_task_event` to apply once that specific task finishes.
    fn spawn_delete(&mut self, targets: Vec<PathBuf>) {
        let anchor = self.active_pane().anchor_above(&targets);
        let pane = self.active;
        let behavior = self.config.delete_behavior;
        for target in &targets {
            self.log_info(format!("delete: {}", target.display()));
        }
        let desc = format!("delete {} item(s)", targets.len());
        let id = self.tasks.spawn(desc, move |id, tx, cancel| {
            delete_task::run_delete(id, tx, cancel, targets, behavior);
        });
        self.pending_delete_anchor.insert(id, (pane, anchor));
    }

    /// `c` (duplicate): prompts for a new name (prefilled with the current
    /// one) and, on commit, copies the cursor entry to that name in the
    /// *same* directory — via the same background-task machinery as
    /// Copy/Move, so a large directory duplicates asynchronously with
    /// progress too.
    pub(super) fn begin_duplicate(&mut self) {
        if self.reject_if_virtual("duplicate") {
            return;
        }
        let Some(selected) = self.active_pane().selected_entry() else {
            self.log_error("no entry selected to duplicate");
            return;
        };
        let name = selected.name.clone();
        let source = selected.path.clone();
        self.mode = Mode::Prompt {
            kind: PromptKind::Duplicate { source },
            input: LineEditor::from_str(&name),
        };
    }

    /// Validates and spawns the actual duplicate once the prompt commits:
    /// the name must be non-empty, must not contain a path separator (it
    /// stays in the same directory — this isn't a move), and must differ
    /// from the source's own name (a same-name "duplicate" would just
    /// collide with itself).
    pub(super) fn commit_duplicate(&mut self, source: PathBuf, name: String) {
        if name.is_empty() {
            self.log_error("name cannot be empty");
            return;
        }
        if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
            self.log_error("name cannot contain a path separator");
            return;
        }
        let Some(parent) = source.parent() else {
            self.log_error("cursor entry has no parent directory");
            return;
        };
        let dest = parent.join(&name);
        if dest == source {
            self.log_error("new name must differ from the current name");
            return;
        }
        if dest.exists() {
            self.log_error(format!("{} already exists", dest.display()));
            return;
        }
        self.spawn_duplicate(source, dest);
    }

    /// Hands the actual duplicate off to a background task (see
    /// `tasks::copy_move::run_duplicate`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_duplicate(&mut self, source: PathBuf, dest: PathBuf) {
        self.log_info(format!(
            "duplicate: {} -> {}",
            source.display(),
            dest.display()
        ));
        let desc = format!("duplicate {} to {}", source.display(), dest.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            copy_move::run_duplicate(id, tx, cancel, source, dest);
        });
    }

    pub(super) fn commit_zip_name(&mut self, targets: Vec<PathBuf>, name: String) {
        if name.is_empty() {
            self.log_error("name cannot be empty");
            return;
        }
        if name.contains('/') || name.contains(std::path::MAIN_SEPARATOR) {
            self.log_error("name cannot contain a path separator");
            return;
        }

        let dest_dir = self.other_pane().cwd.clone();
        let archive_path = dest_dir.join(&name);
        if archive_path.exists() {
            let message = format!("Overwrite {}? (y/n)", archive_path.display());
            self.confirm(
                message,
                PendingOp::ZipOverwrite {
                    targets,
                    archive_path,
                },
            );
        } else {
            self.spawn_zip(targets, archive_path);
        }
    }

    /// Opens the zip-name prompt for the active pane's marked-or-cursor
    /// selection, pre-filled with `<first-target-stem>.zip`.
    pub(super) fn begin_zip(&mut self) {
        if self.reject_if_virtual("zip_marked") {
            return;
        }
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to zip");
            return;
        }
        let stem = targets[0]
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_string());
        let default_name = format!("{stem}.zip");
        self.mode = Mode::Prompt {
            kind: PromptKind::ZipName { targets },
            input: LineEditor::from_str(&default_name),
        };
    }

    /// Hands the actual zip creation off to a background task (see
    /// `tasks::archive::run_zip`); see `spawn_transfer` for the completion
    /// story.
    fn spawn_zip(&mut self, targets: Vec<PathBuf>, archive_path: PathBuf) {
        for target in &targets {
            self.log_info(format!("zip: {}", target.display()));
        }
        let desc = format!(
            "zip {} item(s) to {}",
            targets.len(),
            archive_path.display()
        );
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_zip(id, tx, cancel, targets, archive_path);
        });
    }

    /// The cursor entry must be a `.zip` file; extracts into the other
    /// pane's cwd, confirming first if any top-level entry would collide.
    pub(super) fn begin_unzip(&mut self) {
        if self.reject_if_virtual("unzip") {
            return;
        }
        let Some(name) = self.active_pane().selected_entry_name() else {
            self.log_error("no entry selected to unzip");
            return;
        };
        if !name.to_lowercase().ends_with(".zip") {
            self.log_error("selected entry is not a .zip file");
            return;
        }
        let archive_path = self.active_pane().cwd.join(&name);
        let dest_dir = self.other_pane().cwd.clone();

        match archive::top_level_collisions(&archive_path, &dest_dir) {
            Ok(collisions) if !collisions.is_empty() => {
                let message = format!("Overwrite {} existing item(s)? (y/n)", collisions.len());
                self.confirm(
                    message,
                    PendingOp::UnzipOverwrite {
                        archive_path,
                        dest_dir,
                    },
                );
            }
            Ok(_) => self.spawn_unzip(archive_path, dest_dir),
            Err(err) => self.log_error(err.to_string()),
        }
    }

    /// Hands the actual extraction off to a background task (see
    /// `tasks::archive::run_unzip`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_unzip(&mut self, archive_path: PathBuf, dest_dir: PathBuf) {
        self.log_info(format!(
            "unzip: {} -> {}",
            archive_path.display(),
            dest_dir.display()
        ));
        let desc = format!("unzip {} to {}", archive_path.display(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_unzip(id, tx, cancel, archive_path, dest_dir);
        });
    }

    /// `C` while the active pane is a Virtual Directory: extracts the
    /// marked (or cursor) entries out of the archive. `inner_targets` are
    /// archive-internal paths, extracted into the *other* pane's real
    /// cwd — already confirmed by `begin_transfer` to not itself be
    /// virtual before this is ever called. Same confirm-before-overwrite
    /// posture as a real Copy/Move (`config.confirm_operations`, always
    /// confirming on an actual collision).
    fn begin_extract(&mut self) {
        let inner_targets = self.active_pane().marked_or_cursor();
        if inner_targets.is_empty() {
            self.log_error("no entry selected to extract");
            return;
        }
        let Some(archive_path) = self
            .active_pane()
            .virtual_dir
            .as_ref()
            .map(|vd| vd.archive_path.clone())
        else {
            return; // defensive: begin_transfer already checked is_virtual()
        };
        let dest_dir = self.other_pane().cwd.clone();

        let collisions = archive::extract_collisions(&inner_targets, &dest_dir);
        if collisions.is_empty() && !self.config.confirm_operations {
            self.spawn_extract(archive_path, inner_targets, dest_dir);
            return;
        }

        let mut message = format!(
            "Extract {} item(s) -> {}?",
            inner_targets.len(),
            dest_dir.display()
        );
        if !collisions.is_empty() {
            message.push_str(&format!(" ({} will be overwritten)", collisions.len()));
        }
        message.push_str(" (y/n)");

        self.confirm(
            message,
            PendingOp::Extract {
                archive_path,
                inner_targets,
                dest_dir,
            },
        );
    }

    /// Hands the actual extraction off to a background task (see
    /// `tasks::archive::run_extract`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_extract(
        &mut self,
        archive_path: PathBuf,
        inner_targets: Vec<PathBuf>,
        dest_dir: PathBuf,
    ) {
        for target in &inner_targets {
            let dest = target
                .file_name()
                .map(|name| dest_dir.join(name))
                .unwrap_or_else(|| dest_dir.clone());
            self.log_info(format!(
                "extract: {}{} -> {}",
                archive_path.display(),
                virtual_dir::inner_display(target),
                dest.display()
            ));
        }
        let desc = format!(
            "extract {} item(s) to {}",
            inner_targets.len(),
            dest_dir.display()
        );
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_extract(id, tx, cancel, archive_path, inner_targets, dest_dir);
        });
    }

    /// Fixed confirmation keys for `Mode::Confirm`; never consults the
    /// keymap. `y`/`Y` executes the pending op, anything else (including
    /// Esc) cancels.
    pub(super) fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') => {
                if let Mode::Confirm { on_yes, .. } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                {
                    self.execute_pending(on_yes);
                }
            }
            _ => self.mode = Mode::Normal,
        }
    }

    fn execute_pending(&mut self, op: PendingOp) {
        match op {
            PendingOp::Delete { targets } => self.spawn_delete(targets),
            PendingOp::Overwrite {
                kind,
                sources,
                dest_dir,
            } => self.spawn_transfer(kind, sources, dest_dir),
            PendingOp::ZipOverwrite {
                targets,
                archive_path,
            } => self.spawn_zip(targets, archive_path),
            PendingOp::UnzipOverwrite {
                archive_path,
                dest_dir,
            } => self.spawn_unzip(archive_path, dest_dir),
            PendingOp::Extract {
                archive_path,
                inner_targets,
                dest_dir,
            } => self.spawn_extract(archive_path, inner_targets, dest_dir),
            PendingOp::Quit => self.should_quit = true,
        }
    }
}
