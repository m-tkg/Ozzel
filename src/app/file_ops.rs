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

    /// `m` (rename_marks): renames each *visible* marked entry through
    /// its own prompt, in display order, `(n/total)` in the title.
    /// Deliberately no cursor fallback — that's `r`'s job; an empty mark
    /// set here is treated as "nothing to do", not "rename the cursor".
    /// Marks are cleared up front: the names are captured into the prompt
    /// queue, and each rename would invalidate its path-keyed mark anyway.
    pub(super) fn begin_rename_marks(&mut self) {
        if self.reject_if_virtual("rename_marks") {
            return;
        }
        let (names, hidden) = self.active_pane().marked_names_in_display_order();
        if hidden > 0 {
            self.log_info(format!(
                "{hidden} marked entr{} hidden by the filter — not included",
                if hidden == 1 { "y is" } else { "ies are" }
            ));
        }
        let Some((first, rest)) = names.split_first() else {
            self.log_error("no marked entries to rename");
            return;
        };
        let dir = self.active_pane().cwd.clone();
        let total = names.len();
        self.active_pane_mut().clear_marks();
        self.mode = Mode::Prompt {
            input: LineEditor::from_str(first),
            kind: PromptKind::RenameMany {
                dir,
                current: first.clone(),
                queue: rest.iter().cloned().collect(),
                done: 0,
                total,
            },
        };
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

    /// `z` (calc_dir_size): spawns a background task summing each target
    /// directory's recursive size, remembering which pane asked
    /// (`pending_dir_size`) so the per-directory `TaskEvent::DirSize`
    /// results land back on it — see `Pane::set_dir_size` for how results
    /// arriving after further navigation are dropped.
    pub(super) fn begin_calc_dir_size(&mut self) {
        if self.reject_if_virtual("calc_dir_size") {
            return;
        }
        let targets: Vec<PathBuf> = self
            .active_pane()
            .marked_or_cursor()
            .into_iter()
            .filter(|p| p.is_dir())
            .collect();
        if targets.is_empty() {
            self.log_error("no directory selected to size");
            return;
        }
        let desc = format!("dir size ({} dir(s))", targets.len());
        let id = self.tasks.spawn(desc, move |id, tx, cancel| {
            crate::tasks::dir_size::run_dir_size(id, tx, cancel, targets)
        });
        self.pending_dir_size.insert(id, self.active);
    }

    /// Copy/Move with no filename collision confirms by default
    /// (`config.confirm_operations`; `false` skips straight to
    /// `spawn_transfer`). Same-name collisions never go through that y/n —
    /// they open the per-file collision dialog (`Mode::TransferCollision`,
    /// Overwrite/Rename/Skip/…-All per conflict), which is itself the
    /// confirmation and spawns directly once every conflict is answered.
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

        // Partition up front: non-colliding sources become ready `(src,
        // dest)` pairs; colliding ones are queued for the per-file
        // dialog. A source with no file name (a bare root path) can't be
        // transferred into a directory at all — logged and dropped here,
        // where there's still a log-worthy moment for it.
        let mut resolved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut colliding: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
        for src in sources {
            let Some(name) = src.file_name() else {
                self.log_error(format!("{}: has no file name, skipped", src.display()));
                continue;
            };
            let dest = dest_dir.join(name);
            if dest.exists() {
                colliding.push_back(src);
            } else {
                resolved.push((src, dest));
            }
        }
        if resolved.is_empty() && colliding.is_empty() {
            self.log_error("no entry selected");
            return;
        }

        // Same-name conflicts go through the per-file dialog (which is
        // itself the confirmation); a conflict-free transfer keeps the
        // old single confirm gated on `confirm_operations`.
        if let Some(first) = colliding.pop_front() {
            let total = colliding.len() + 1;
            let current = self.collision_info(first, &dest_dir);
            self.mode = Mode::TransferCollision {
                state: CollisionState {
                    kind,
                    dest_dir,
                    resolved,
                    pending: colliding,
                    current,
                    index: 1,
                    total,
                    cursor: 0,
                },
            };
            return;
        }

        if !self.config.confirm_operations {
            self.spawn_transfer(kind, resolved, dest_dir);
            return;
        }

        let verb = match kind {
            TransferKind::Copy => "Copy",
            TransferKind::Move => "Move",
        };
        let message = format!(
            "{verb} {} item(s) -> {}? (y/n)",
            resolved.len(),
            dest_dir.display()
        );
        self.confirm(
            message,
            PendingOp::Transfer {
                kind,
                pairs: resolved,
                dest_dir,
            },
        );
    }

    /// Builds the display facts for one collision (`Mode::TransferCollision`'s
    /// `current`): size + mtime for both sides, freshly `symlink_metadata`ed
    /// (the dest may not even be listed in the other pane — filtered out,
    /// or the pane not reloaded), with ` [New]` marking the newer side.
    fn collision_info(&self, src: PathBuf, dest_dir: &Path) -> CollisionInfo {
        fn facts(path: &Path) -> (Option<u64>, Option<std::time::SystemTime>) {
            match std::fs::symlink_metadata(path) {
                Ok(meta) => (Some(meta.len()), meta.modified().ok()),
                Err(_) => (None, None),
            }
        }
        fn line(
            label: &str,
            size: Option<u64>,
            mtime: Option<std::time::SystemTime>,
            newer: bool,
        ) -> String {
            let size = size.map_or_else(|| "?".to_string(), |s| format!("{s} bytes"));
            let mtime = mtime.map_or_else(String::new, |t| {
                format!("  {}", crate::ui::pane_view::format_mtime(t))
            });
            let new_tag = if newer { "  [New]" } else { "" };
            format!("{label}: {size}{mtime}{new_tag}")
        }

        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dest = dest_dir.join(&name);
        let (src_size, src_mtime) = facts(&src);
        let (dest_size, dest_mtime) = facts(&dest);
        let (src_newer, dest_newer) = match (src_mtime, dest_mtime) {
            (Some(s), Some(d)) if s > d => (true, false),
            (Some(s), Some(d)) if d > s => (false, true),
            _ => (false, false),
        };
        CollisionInfo {
            src,
            name,
            src_line: line("src ", src_size, src_mtime, src_newer),
            dest_line: line("dest", dest_size, dest_mtime, dest_newer),
        }
    }

    /// Key handling for the collision dialog: `↑`/`↓` move over
    /// `COLLISION_CHOICES`, Enter answers for the current entry, Esc
    /// cancels the *whole* transfer (nothing has been spawned yet).
    pub(super) fn handle_transfer_collision_key(&mut self, code: KeyCode) {
        let Mode::TransferCollision { state } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up => {
                state.cursor = state
                    .cursor
                    .checked_sub(1)
                    .unwrap_or(COLLISION_CHOICES.len() - 1);
            }
            KeyCode::Down => state.cursor = (state.cursor + 1) % COLLISION_CHOICES.len(),
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.log_info("transfer cancelled");
            }
            KeyCode::Enter => {
                let Mode::TransferCollision { state } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    return;
                };
                self.answer_collision(state);
            }
            _ => {}
        }
    }

    /// Applies the highlighted answer to `state.current`, then either
    /// asks about the next conflict or spawns the transfer.
    fn answer_collision(&mut self, mut state: CollisionState) {
        match state.cursor {
            // Overwrite
            0 => {
                let dest = state.dest_dir.join(&state.current.name);
                state.resolved.push((state.current.src.clone(), dest));
                self.advance_collision(state);
            }
            // Rename: collect the new name through a prompt; the state
            // travels inside the PromptKind and comes back through
            // `commit_collision_rename`.
            1 => {
                let input = LineEditor::from_str(&state.current.name);
                self.mode = Mode::Prompt {
                    kind: PromptKind::CollisionRename {
                        state: Box::new(state),
                    },
                    input,
                };
            }
            // Skip
            2 => self.advance_collision(state),
            // Overwrite All: current + everything still pending
            3 => {
                let dest = state.dest_dir.join(&state.current.name);
                state.resolved.push((state.current.src.clone(), dest));
                while let Some(src) = state.pending.pop_front() {
                    let Some(name) = src.file_name() else {
                        continue;
                    };
                    let dest = state.dest_dir.join(name);
                    state.resolved.push((src, dest));
                }
                self.finish_collisions(state);
            }
            // Skip All
            _ => {
                state.pending.clear();
                self.finish_collisions(state);
            }
        }
    }

    /// Moves the dialog on to the next pending conflict, or finishes.
    pub(super) fn advance_collision(&mut self, mut state: CollisionState) {
        match state.pending.pop_front() {
            Some(next) => {
                state.current = self.collision_info(next, &state.dest_dir.clone());
                state.index += 1;
                state.cursor = 0;
                self.mode = Mode::TransferCollision { state };
            }
            None => self.finish_collisions(state),
        }
    }

    /// Every conflict has an answer: spawn whatever survived. The dialog
    /// itself was the confirmation, so there's no second y/n here.
    fn finish_collisions(&mut self, state: CollisionState) {
        if state.resolved.is_empty() {
            self.log_info("nothing to transfer (all conflicts skipped)");
            return;
        }
        self.spawn_transfer(state.kind, state.resolved, state.dest_dir);
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
    fn spawn_transfer(
        &mut self,
        kind: TransferKind,
        pairs: Vec<(PathBuf, PathBuf)>,
        dest_dir: PathBuf,
    ) {
        let verb = match kind {
            TransferKind::Copy => "copy",
            TransferKind::Move => "move",
        };
        for (src, dest) in &pairs {
            self.log_info(format!("{verb}: {} -> {}", src.display(), dest.display()));
        }
        let desc = format!("{verb} {} item(s) to {}", pairs.len(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| match kind {
            TransferKind::Copy => copy_move::run_copy(id, tx, cancel, pairs, dest_dir),
            TransferKind::Move => copy_move::run_move(id, tx, cancel, pairs, dest_dir),
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

        // An encrypted archive needs the password *before* anything is
        // spawned — collected via the masked prompt, verified there, and
        // carried through the overwrite confirm (see
        // `commit_archive_password`, which re-enters `continue_unzip`).
        match virtual_dir::zip_has_encrypted_entries(&archive_path) {
            Ok(true) => {
                self.mode = Mode::Prompt {
                    kind: PromptKind::ArchivePassword {
                        pending: PasswordPending::Unzip {
                            archive_path,
                            dest_dir,
                        },
                    },
                    input: LineEditor::new(),
                };
            }
            Ok(false) => self.continue_unzip(archive_path, dest_dir, None),
            Err(err) => self.log_error(err.to_string()),
        }
    }

    /// The unzip flow after any needed password is in hand: the same
    /// collision-check/confirm/spawn sequence `begin_unzip` always had.
    pub(super) fn continue_unzip(
        &mut self,
        archive_path: PathBuf,
        dest_dir: PathBuf,
        password: Option<String>,
    ) {
        match archive::top_level_collisions(&archive_path, &dest_dir) {
            Ok(collisions) if !collisions.is_empty() => {
                let message = format!("Overwrite {} existing item(s)? (y/n)", collisions.len());
                self.confirm(
                    message,
                    PendingOp::UnzipOverwrite {
                        archive_path,
                        dest_dir,
                        password,
                    },
                );
            }
            Ok(_) => self.spawn_unzip(archive_path, dest_dir, password),
            Err(err) => self.log_error(err.to_string()),
        }
    }

    /// Hands the actual extraction off to a background task (see
    /// `tasks::archive::run_unzip`); see `spawn_transfer` for the
    /// completion story.
    fn spawn_unzip(&mut self, archive_path: PathBuf, dest_dir: PathBuf, password: Option<String>) {
        self.log_info(format!(
            "unzip: {} -> {}",
            archive_path.display(),
            dest_dir.display()
        ));
        let desc = format!("unzip {} to {}", archive_path.display(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_unzip(id, tx, cancel, archive_path, dest_dir, password);
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
        let Some(vd) = self.active_pane().virtual_dir.as_ref() else {
            return; // defensive: begin_transfer already checked is_virtual()
        };
        let archive_path = vd.archive_path.clone();
        let cached_password = vd.cached_password();
        let dest_dir = self.other_pane().cwd.clone();

        // An encrypted zip without a session-cached password prompts
        // first; the flow re-enters `continue_extract` once verified. A
        // cached password was already verified when it was cached.
        if cached_password.is_none()
            && matches!(
                virtual_dir::detect_archive_kind(&archive_path),
                Some(virtual_dir::ArchiveKind::Zip)
            )
        {
            match virtual_dir::zip_has_encrypted_entries(&archive_path) {
                Ok(true) => {
                    self.mode = Mode::Prompt {
                        kind: PromptKind::ArchivePassword {
                            pending: PasswordPending::Extract {
                                archive_path,
                                inner_targets,
                                dest_dir,
                            },
                        },
                        input: LineEditor::new(),
                    };
                    return;
                }
                Ok(false) => {}
                Err(err) => {
                    self.log_error(err.to_string());
                    return;
                }
            }
        }
        self.continue_extract(archive_path, inner_targets, dest_dir, cached_password);
    }

    /// The extract flow after any needed password is in hand: the same
    /// collision-check/confirm/spawn sequence `begin_extract` always had.
    pub(super) fn continue_extract(
        &mut self,
        archive_path: PathBuf,
        inner_targets: Vec<PathBuf>,
        dest_dir: PathBuf,
        password: Option<String>,
    ) {
        let collisions = archive::extract_collisions(&inner_targets, &dest_dir);
        if collisions.is_empty() && !self.config.confirm_operations {
            self.spawn_extract(archive_path, inner_targets, dest_dir, password);
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
                password,
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
        password: Option<String>,
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
            archive::run_extract(
                id,
                tx,
                cancel,
                archive_path,
                inner_targets,
                dest_dir,
                password,
            );
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
            PendingOp::Transfer {
                kind,
                pairs,
                dest_dir,
            } => self.spawn_transfer(kind, pairs, dest_dir),
            PendingOp::ZipOverwrite {
                targets,
                archive_path,
            } => self.spawn_zip(targets, archive_path),
            PendingOp::UnzipOverwrite {
                archive_path,
                dest_dir,
                password,
            } => self.spawn_unzip(archive_path, dest_dir, password),
            PendingOp::Extract {
                archive_path,
                inner_targets,
                dest_dir,
                password,
            } => self.spawn_extract(archive_path, inner_targets, dest_dir, password),
            PendingOp::Quit => self.should_quit = true,
        }
    }
}
