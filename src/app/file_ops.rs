//! Filesystem-mutating operations: mkdir/rename/delete, copy/move
//! (transfer) and duplicate, and zip/unzip/extract — each `begin_*`
//! (builds a prompt/confirmation) paired with the `spawn_*` that hands
//! the actual work to a background task (see `crate::tasks`), plus the
//! `Mode::Confirm`/`PendingOp` machinery (`handle_confirm_key`/
//! `execute_pending`) every one of them can end up going through first.
//! Split out of `app/mod.rs` (Phase 6, Step 5's "if there's room" bullet).

use super::*;
use crate::virtual_dir::ArchiveKind;

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
    pub(super) fn handle_transfer_collision_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Resolved before the `&mut self.mode` borrow below, and as a plain
        // `Copy` value — see `handle_select_key`.
        let nav = self.keymap.menu_nav(code, modifiers);
        let Mode::TransferCollision { state } = &mut self.mode else {
            return;
        };
        let up = |state: &mut CollisionState| {
            state.cursor = state
                .cursor
                .checked_sub(1)
                .unwrap_or(COLLISION_CHOICES.len() - 1);
        };
        let down = |state: &mut CollisionState| {
            state.cursor = (state.cursor + 1) % COLLISION_CHOICES.len();
        };
        match code {
            KeyCode::Up => up(state),
            KeyCode::Down => down(state),
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
            _ => match nav {
                Some(MenuNav::Up) => up(state),
                Some(MenuNav::Down) => down(state),
                None => {}
            },
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

    /// `u`: extracts the archive under the cursor into the other pane.
    /// Accepts every format `virtual_dir::detect_archive_kind` recognizes
    /// (zip, the tar family, and bare `.gz`/`.bz2`), routed through the
    /// same per-format workers the Virtual Directory `C` extraction uses.
    ///
    /// Multi-file archives (zip + tar family) always land in a *newly
    /// created* subdirectory of the other pane's cwd, named after the
    /// archive's stem (`project.tar.gz` -> `project`) and uniquified with
    /// a `-1`/`-2`/... suffix when that name is taken — see
    /// `continue_unzip`. That's what replaced the old top-level-collision
    /// check and overwrite confirm: `u` can no longer overwrite anything,
    /// so there is nothing left to confirm.
    pub(super) fn begin_unzip(&mut self) {
        if self.reject_if_virtual("unzip") {
            return;
        }
        let Some(name) = self.active_pane().selected_entry_name() else {
            self.log_error("no entry selected to unzip");
            return;
        };
        let archive_path = self.active_pane().cwd.join(&name);
        // `detect_archive_kind` classifies by name alone, so a *directory*
        // called `foo.zip` would pass it — reject that before opening
        // anything.
        if !archive_path.is_file() {
            self.log_error(format!("{name}: not a file"));
            return;
        }
        let Some(kind) = virtual_dir::detect_archive_kind(&archive_path) else {
            self.log_error(format!("{name}: not a supported archive format"));
            return;
        };
        if self.other_pane().is_virtual() {
            self.log_error("cannot extract into a virtual directory (archive) pane");
            return;
        }
        let dest_root = self.other_pane().cwd.clone();

        // Only a zip can be encrypted, and `zip_has_encrypted_entries`
        // opens its argument *as a zip* — probing a tar with it (which the
        // old `.zip`-only gate made impossible) would fail with an opaque
        // "not a valid zip archive" before extraction was ever attempted.
        // Same guard `begin_extract` below already uses.
        //
        // An encrypted archive needs the password *before* anything is
        // spawned — collected via the masked prompt and verified there
        // (see `commit_archive_password`, which re-enters
        // `continue_unzip` once, with an already-valid password).
        if matches!(kind, ArchiveKind::Zip) {
            match virtual_dir::zip_has_encrypted_entries(&archive_path) {
                Ok(true) => {
                    self.mode = Mode::Prompt {
                        kind: PromptKind::ArchivePassword {
                            pending: PasswordPending::Unzip {
                                archive_path,
                                dest_root,
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
        self.continue_unzip(archive_path, dest_root, None);
    }

    /// The `u` flow after any needed password is in hand. `dest_root` is
    /// the other pane's cwd; the actual destination is derived from it
    /// here — a freshly created stem-named subdirectory for zip/tar, or
    /// `dest_root` itself for a single-payload `.gz`/`.bz2`, which has no
    /// container worth wrapping — and the task spawns immediately, with no
    /// collision check left to make.
    ///
    /// The directory is created here rather than in `begin_unzip` so that
    /// cancelling the password prompt with Esc doesn't leave a stray empty
    /// directory behind. A *failed* or `C-k`-cancelled extraction still
    /// does leave its (possibly partial) directory, matching how a
    /// cancelled copy leaves partial files.
    pub(super) fn continue_unzip(
        &mut self,
        archive_path: PathBuf,
        dest_root: PathBuf,
        password: Option<String>,
    ) {
        // Re-derived rather than threaded through `PasswordPending`:
        // `detect_archive_kind` is a pure filename match, so this is free.
        let Some(kind) = virtual_dir::detect_archive_kind(&archive_path) else {
            self.log_error(format!(
                "{}: not a supported archive format",
                archive_path.display()
            ));
            return;
        };
        let dest_dir = match kind {
            ArchiveKind::Single(_) => {
                let payload = virtual_dir::single_payload_name(&archive_path);
                if dest_root.join(&payload).exists() {
                    self.log_error(format!("already exists: {payload}"));
                    return;
                }
                dest_root
            }
            _ => {
                let stem = virtual_dir::archive_stem(&archive_path);
                match ops::create_unique_dir(&dest_root, &stem) {
                    Ok(dir) => dir,
                    Err(err) => {
                        self.log_error(err.to_string());
                        return;
                    }
                }
            }
        };
        self.spawn_unzip(archive_path, dest_dir, password);
    }

    /// Hands the actual extraction off to a background task (see
    /// `tasks::archive::run_extract`); see `spawn_transfer` for the
    /// completion story. `dest_dir` here is the *final* destination the
    /// files land in — the freshly created subdirectory, for everything
    /// but a single-payload archive — so the log line names it.
    fn spawn_unzip(&mut self, archive_path: PathBuf, dest_dir: PathBuf, password: Option<String>) {
        self.log_info(format!(
            "unzip: {} -> {}",
            archive_path.display(),
            dest_dir.display()
        ));
        let desc = format!("unzip {} to {}", archive_path.display(), dest_dir.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            archive::run_extract(
                id,
                tx,
                cancel,
                archive_path,
                archive::ExtractSelection::All,
                dest_dir,
                password,
            );
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
                archive::ExtractSelection::Targets(inner_targets),
                dest_dir,
                password,
            );
        });
    }

    /// `@` (symlink): creates one symbolic link per marked (or cursor)
    /// entry in the other pane's directory, each pointing at the source's
    /// absolute path. Same confirm gate as Copy/Move
    /// (`config.confirm_operations`).
    pub(super) fn begin_symlink(&mut self) {
        if self.reject_if_virtual("symlink") {
            return;
        }
        if self.other_pane().is_virtual() {
            self.log_error("cannot create symlinks in a virtual directory (archive) pane");
            return;
        }
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to symlink");
            return;
        }
        let dest_dir = self.other_pane().cwd.clone();
        if dest_dir == self.active_pane().cwd {
            self.log_error("both panes show the same directory — the link name would collide");
            return;
        }
        if !self.config.confirm_operations {
            self.execute_symlink(targets, dest_dir);
            return;
        }
        let message = format!(
            "Symlink {} item(s) -> {}? (y/n)",
            targets.len(),
            dest_dir.display()
        );
        self.confirm(message, PendingOp::Symlink { targets, dest_dir });
    }

    /// The actual (synchronous) symlink creation `begin_symlink`/
    /// `execute_pending` end in: per-source create with individual
    /// success/failure logging, then reload.
    fn execute_symlink(&mut self, targets: Vec<PathBuf>, dest_dir: PathBuf) {
        let mut created = 0usize;
        let mut failed = 0usize;
        for src in &targets {
            match ops::create_symlink(src, &dest_dir) {
                Ok(dest) => {
                    self.log_info(format!("symlink: {} -> {}", dest.display(), src.display()));
                    created += 1;
                }
                Err(err) => {
                    self.log_error(err.to_string());
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            self.log_error(format!("symlink: {created} created, {failed} failed"));
        } else {
            self.log_info(format!("symlink: {created} created"));
        }
        self.reload_both();
        for pane in &mut self.panes {
            pane.clear_marks();
        }
    }

    /// `W` (sync_dirs): opens the sync-mode dialog for syncing the active
    /// pane's whole directory onto the other pane's. Same-directory and
    /// nested (either-way) pane pairs are rejected up front — a mirror of
    /// a directory into its own ancestor/descendant would eat itself.
    pub(super) fn begin_sync_dirs(&mut self) {
        if self.reject_if_virtual("sync_dirs") {
            return;
        }
        if self.other_pane().is_virtual() {
            self.log_error("cannot sync into a virtual directory (archive) pane");
            return;
        }
        let src = self.active_pane().cwd.clone();
        let dest = self.other_pane().cwd.clone();
        if src == dest {
            self.log_error("both panes show the same directory — nothing to sync");
            return;
        }
        if dest.starts_with(&src) || src.starts_with(&dest) {
            self.log_error("cannot sync between a directory and its own subdirectory");
            return;
        }
        self.mode = Mode::SyncSelect {
            src,
            dest,
            cursor: 0,
        };
    }

    /// Fixed keys for the sync-mode dialog: up/down move over
    /// `SYNC_CHOICES`, Enter picks (update goes through the ordinary
    /// `confirm_operations` gate; mirror *always* confirms, spelling out
    /// the deletions), Esc cancels.
    pub(super) fn handle_sync_select_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Resolved before the `&mut self.mode` borrow below, and as a plain
        // `Copy` value — see `handle_select_key`.
        let nav = self.keymap.menu_nav(code, modifiers);
        let Mode::SyncSelect { cursor, .. } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up => *cursor = cursor.checked_sub(1).unwrap_or(SYNC_CHOICES.len() - 1),
            KeyCode::Down => *cursor = (*cursor + 1) % SYNC_CHOICES.len(),
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let Mode::SyncSelect { src, dest, cursor } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    return;
                };
                let mirror = cursor == 1;
                if mirror {
                    // Never gated on confirm_operations: this one deletes.
                    let message = format!(
                        "Mirror sync: entries in {} not present in {} will be DELETED. Proceed? (y/n)",
                        dest.display(),
                        src.display()
                    );
                    self.confirm(message, PendingOp::SyncDirs { src, dest, mirror });
                } else if self.config.confirm_operations {
                    let message = format!(
                        "Sync (update) {} -> {}? (y/n)",
                        src.display(),
                        dest.display()
                    );
                    self.confirm(message, PendingOp::SyncDirs { src, dest, mirror });
                } else {
                    self.spawn_sync(src, dest, mirror);
                }
            }
            _ => match nav {
                Some(MenuNav::Up) => {
                    *cursor = cursor.checked_sub(1).unwrap_or(SYNC_CHOICES.len() - 1)
                }
                Some(MenuNav::Down) => *cursor = (*cursor + 1) % SYNC_CHOICES.len(),
                None => {}
            },
        }
    }

    /// Hands the actual sync off to a background task (see
    /// `tasks::sync::run_sync`); see `spawn_transfer` for the completion
    /// story (the reload both panes get afterward is exactly right here).
    fn spawn_sync(&mut self, src: PathBuf, dest: PathBuf, mirror: bool) {
        let mode_label = if mirror { "mirror" } else { "update" };
        self.log_info(format!(
            "sync ({mode_label}): {} -> {}",
            src.display(),
            dest.display()
        ));
        let behavior = self.config.delete_behavior;
        let desc = format!("sync ({mode_label}) to {}", dest.display());
        self.tasks.spawn(desc, move |id, tx, cancel| {
            crate::tasks::sync::run_sync(id, tx, cancel, src, dest, mirror, behavior);
        });
    }

    /// `A` (chmod): opens the 3x3 rwx toggle dialog for the marked (or
    /// cursor) entries, prefilled with the first target's current mode.
    /// Unix only — the mode bits simply don't exist elsewhere, so on other
    /// platforms this logs and does nothing (rather than a lossy
    /// readonly-flag-only imitation).
    pub(super) fn begin_chmod(&mut self) {
        if self.reject_if_virtual("chmod") {
            return;
        }
        #[cfg(not(unix))]
        {
            self.log_error("chmod is not supported on this platform");
        }
        #[cfg(unix)]
        {
            let targets = self.active_pane().marked_or_cursor();
            if targets.is_empty() {
                self.log_error("no entry selected to chmod");
                return;
            }
            let bits = std::fs::symlink_metadata(&targets[0])
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o777
                })
                .unwrap_or(0o644);
            self.mode = Mode::Chmod {
                state: ChmodState {
                    targets,
                    bits,
                    cursor: 0,
                },
            };
        }
    }

    /// Fixed keys for the chmod dialog: arrows move over the 3x3 grid,
    /// Space toggles the highlighted bit, `0`-`7` set the highlighted
    /// row's class to that octal digit, Enter applies the edited mode to
    /// every target, Esc cancels.
    pub(super) fn handle_chmod_key(&mut self, code: KeyCode) {
        let Mode::Chmod { state } = &mut self.mode else {
            return;
        };
        match code {
            KeyCode::Up => {
                if state.cursor >= 3 {
                    state.cursor -= 3;
                }
            }
            KeyCode::Down => {
                if state.cursor + 3 < 9 {
                    state.cursor += 3;
                }
            }
            KeyCode::Left => {
                if state.cursor % 3 > 0 {
                    state.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if state.cursor % 3 < 2 {
                    state.cursor += 1;
                }
            }
            KeyCode::Char(' ') => {
                state.bits ^= ChmodState::bit_at(state.cursor);
            }
            KeyCode::Char(c @ '0'..='7') => {
                let digit = c as u32 - '0' as u32;
                let row = state.cursor / 3;
                let shift = 3 * (2 - row as u32);
                state.bits = (state.bits & !(0o7 << shift)) | (digit << shift);
            }
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let Mode::Chmod { state } = std::mem::replace(&mut self.mode, Mode::Normal) else {
                    return;
                };
                self.execute_chmod(state);
            }
            _ => {}
        }
    }

    /// Applies the dialog's edited mode to every target. Unix only, like
    /// `begin_chmod` — on other platforms the dialog can't even open, so
    /// this is unreachable there (compiled to a no-op body for
    /// completeness).
    fn execute_chmod(&mut self, state: ChmodState) {
        #[cfg(unix)]
        {
            let mut changed = 0usize;
            let mut failed = 0usize;
            for target in &state.targets {
                match ops::chmod(target, state.bits) {
                    Ok(()) => changed += 1,
                    Err(err) => {
                        self.log_error(err.to_string());
                        failed += 1;
                    }
                }
            }
            let summary = format!(
                "chmod {:03o}: {changed} changed{}",
                state.bits,
                if failed > 0 {
                    format!(", {failed} failed")
                } else {
                    String::new()
                }
            );
            if failed > 0 {
                self.log_error(summary);
            } else {
                self.log_info(summary);
            }
            self.reload_both();
            for pane in &mut self.panes {
                pane.clear_marks();
            }
        }
        #[cfg(not(unix))]
        {
            let _ = state;
        }
    }

    /// `T` (touch): prompts for a timestamp (prefilled with the cursor
    /// entry's mtime) and applies it to the marked (or cursor) entries'
    /// modified/accessed times on commit. Empty input means "now".
    pub(super) fn begin_touch(&mut self) {
        if self.reject_if_virtual("touch") {
            return;
        }
        let targets = self.active_pane().marked_or_cursor();
        if targets.is_empty() {
            self.log_error("no entry selected to touch");
            return;
        }
        let prefill = self
            .active_pane()
            .selected_entry()
            .and_then(|e| e.mtime)
            .map(|t| {
                let dt: DateTime<Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_default();
        self.mode = Mode::Prompt {
            kind: PromptKind::TouchTime { targets },
            input: LineEditor::from_str(&prefill),
        };
    }

    /// The committed touch prompt: parses the typed timestamp (three
    /// formats, shortest first-match wins; empty = now) and applies it to
    /// every target.
    pub(super) fn commit_touch(&mut self, targets: Vec<PathBuf>, value: String) {
        let value = value.trim();
        let time = if value.is_empty() {
            std::time::SystemTime::now()
        } else {
            match parse_touch_time(value) {
                Some(t) => t,
                None => {
                    self.log_error(format!(
                        "invalid time (expected YYYY-MM-DD [HH:MM[:SS]]): {value}"
                    ));
                    return;
                }
            }
        };
        let mut changed = 0usize;
        let mut failed = 0usize;
        for target in &targets {
            match ops::set_times(target, time) {
                Ok(()) => changed += 1,
                Err(err) => {
                    self.log_error(err.to_string());
                    failed += 1;
                }
            }
        }
        let dt: DateTime<Local> = time.into();
        let summary = format!(
            "touch {}: {changed} changed{}",
            dt.format("%Y-%m-%d %H:%M:%S"),
            if failed > 0 {
                format!(", {failed} failed")
            } else {
                String::new()
            }
        );
        if failed > 0 {
            self.log_error(summary);
        } else {
            self.log_info(summary);
        }
        self.reload_both();
        for pane in &mut self.panes {
            pane.clear_marks();
        }
    }

    /// `I` (file_info): builds the metadata listing for the cursor entry —
    /// everything re-read fresh from disk right now, not from the possibly
    /// stale `FsEntry` — and opens the read-only info modal.
    pub(super) fn begin_file_info(&mut self) {
        if self.reject_if_virtual("file_info") {
            return;
        }
        let Some(entry) = self.active_pane().selected_entry() else {
            self.log_error("no entry selected");
            return;
        };
        let path = entry.path.clone();
        let name = entry.name.clone();
        match build_file_info(&path, &name) {
            Ok(mut info) => {
                let marks = self.active_pane().marks.len();
                if marks > 0 {
                    // Shallow sum of the marked *files* (directories
                    // excluded — that's `z`/calc_dir_size's job), straight
                    // from fresh metadata like the rest of the dialog.
                    let total: u64 = self
                        .active_pane()
                        .marks
                        .iter()
                        .filter_map(|p| std::fs::symlink_metadata(p).ok())
                        .filter(|m| m.is_file())
                        .map(|m| m.len())
                        .sum();
                    info.rows.push((String::new(), String::new()));
                    info.rows.push((
                        "marked".to_string(),
                        format!("{marks} item(s), files total {total} bytes"),
                    ));
                }
                self.mode = Mode::FileInfo {
                    info: Box::new(info),
                };
            }
            Err(err) => self.log_error(err.to_string()),
        }
    }

    /// Fixed keys for the file-info modal: any of Esc/`q`/Enter closes it.
    pub(super) fn handle_file_info_key(&mut self, code: KeyCode) {
        if matches!(code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            self.mode = Mode::Normal;
        }
    }

    /// Fixed confirmation keys for `Mode::Confirm`; never consults the
    /// keymap. `y`/`Y` executes the pending op, `n`/`N`/`Esc` cancels,
    /// and every other key is ignored — a stray keystroke (a leftover
    /// navigation key, a typo) must neither trigger nor silently dismiss
    /// a confirmation.
    pub(super) fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y' | 'Y') => {
                if let Mode::Confirm { on_yes, .. } =
                    std::mem::replace(&mut self.mode, Mode::Normal)
                {
                    self.execute_pending(on_yes);
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn execute_pending(&mut self, op: PendingOp) {
        match op {
            PendingOp::Delete { targets } => self.spawn_delete(targets),
            PendingOp::Symlink { targets, dest_dir } => self.execute_symlink(targets, dest_dir),
            PendingOp::SyncDirs { src, dest, mirror } => self.spawn_sync(src, dest, mirror),
            PendingOp::Transfer {
                kind,
                pairs,
                dest_dir,
            } => self.spawn_transfer(kind, pairs, dest_dir),
            PendingOp::ZipOverwrite {
                targets,
                archive_path,
            } => self.spawn_zip(targets, archive_path),
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

/// Parses the touch prompt's timestamp: full date-time first, then the
/// minute- and day-granularity short forms (missing parts zero-filled).
/// Interpreted in local time; an ambiguous local time (DST fold) resolves
/// to the earlier instant.
fn parse_touch_time(value: &str) -> Option<std::time::SystemTime> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone};
    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(local.into())
}

/// Builds the file-info modal's rows from a fresh `symlink_metadata` read
/// (plus `read_link` for symlinks) — never from the pane's cached
/// `FsEntry`, so what the dialog shows is what's on disk right now.
fn build_file_info(path: &Path, name: &str) -> anyhow::Result<crate::mode::FileInfoData> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat: {}", path.display()))?;
    let mut rows: Vec<(String, String)> = Vec::new();

    rows.push(("path".to_string(), path.display().to_string()));
    let kind = if meta.file_type().is_symlink() {
        "symlink"
    } else if meta.is_dir() {
        "directory"
    } else {
        "file"
    };
    rows.push(("type".to_string(), kind.to_string()));

    if meta.file_type().is_symlink() {
        match std::fs::read_link(path) {
            Ok(target) => {
                let dangling = if path.metadata().is_err() {
                    " (dangling)"
                } else {
                    ""
                };
                rows.push((
                    "link to".to_string(),
                    format!("{}{dangling}", target.display()),
                ));
            }
            Err(err) => rows.push(("link to".to_string(), format!("<unreadable: {err}>"))),
        }
    }

    rows.push((
        "size".to_string(),
        format!(
            "{} bytes ({})",
            crate::ui::pane_view::group_thousands(meta.len()),
            crate::ui::pane_view::human_size(meta.len())
        ),
    ));

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let entry_kind = if meta.file_type().is_symlink() {
            EntryKind::Symlink
        } else if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        rows.push((
            "permissions".to_string(),
            format!(
                "{} ({:04o})",
                crate::ui::pane_view::unix_permission_string(entry_kind, meta.mode()),
                meta.mode() & 0o7777
            ),
        ));
        let owner = user_name(meta.uid())
            .map(|n| format!("{n} ({})", meta.uid()))
            .unwrap_or_else(|| meta.uid().to_string());
        let group = group_name(meta.gid())
            .map(|n| format!("{n} ({})", meta.gid()))
            .unwrap_or_else(|| meta.gid().to_string());
        rows.push(("owner".to_string(), format!("{owner} : {group}")));
        rows.push(("links".to_string(), meta.nlink().to_string()));
        rows.push(("inode".to_string(), meta.ino().to_string()));
    }

    fn time_row(label: &str, t: std::io::Result<std::time::SystemTime>) -> (String, String) {
        let value = t
            .map(|t| {
                let dt: DateTime<Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_else(|_| "-".to_string());
        (label.to_string(), value)
    }
    rows.push(time_row("modified", meta.modified()));
    rows.push(time_row("accessed", meta.accessed()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ctime = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::new(meta.ctime().max(0) as u64, meta.ctime_nsec().max(0) as u32);
        let dt: DateTime<Local> = ctime.into();
        rows.push((
            "changed".to_string(),
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        ));
    }
    #[cfg(not(unix))]
    rows.push(time_row("created", meta.created()));

    Ok(crate::mode::FileInfoData {
        title: name.to_string(),
        rows,
    })
}

/// uid -> user name via `getpwuid_r`; `None` on any failure (the caller
/// falls back to the bare number). One fixed-size buffer, no ERANGE
/// retry — a name that doesn't fit in 1 KiB just shows numerically.
#[cfg(unix)]
fn user_name(uid: u32) -> Option<String> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = [0u8; 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// gid -> group name via `getgrgid_r`; same contract as `user_name`.
#[cfg(unix)]
fn group_name(gid: u32) -> Option<String> {
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = [0u8; 1024];
    let mut result: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe {
        libc::getgrgid_r(
            gid,
            &mut grp,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(grp.gr_name) };
    Some(name.to_string_lossy().into_owned())
}
