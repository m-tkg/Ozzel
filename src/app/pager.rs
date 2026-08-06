//! The three full-frame "pager" screens — `Mode::Viewer`, `Mode::Help`,
//! `Mode::Log` — and the `less`-style scroll/search machinery they share.
//! Split out of `app/mod.rs` (Phase 6, Step 3).
//!
//! All three bind the same fixed key set (never the configurable keymap):
//! `k`/`j`/arrows, `b`/`f`/Space (page), `u`/`d` (half page), `g`/`G`/
//! Home/End, and `/`/`?`/`n`/`N` search — see `scroll_cmd` and
//! `App::handle_pager_prologue`, which factor that shared shape out of the
//! three screens' own `handle_*_key` methods. Each screen keeps its own
//! scroll field's exact semantics (the viewer/help clamp to a max, the log
//! view counts *up* from the bottom — see `apply_scroll_clamped` /
//! `apply_scroll_inverted`), plus whatever's genuinely unique to it (the
//! viewer's horizontal scroll and hex/text toggle, the log view's
//! width-aware rewrap cache).

use super::*;

/// One of the `less`-style scroll keys common to the viewer, help, and log
/// screens — see `scroll_cmd` (which key codes map to which variant) and
/// `apply_scroll_clamped`/`apply_scroll_inverted` (how each screen turns a
/// variant into a concrete scroll-field update).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollCmd {
    Up,
    Down,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    Top,
    Bottom,
}

/// Maps a key code to the `less`-style scroll command it stands for, if
/// any — the same table for all three pager screens (modifiers are never
/// consulted here; none of the three bind a modified variant of these).
fn scroll_cmd(code: KeyCode) -> Option<ScrollCmd> {
    match code {
        KeyCode::Up | KeyCode::Char('k') => Some(ScrollCmd::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(ScrollCmd::Down),
        KeyCode::PageUp | KeyCode::Char('b') => Some(ScrollCmd::PageUp),
        KeyCode::PageDown | KeyCode::Char('f') | KeyCode::Char(' ') => Some(ScrollCmd::PageDown),
        KeyCode::Char('d') => Some(ScrollCmd::HalfPageDown),
        KeyCode::Char('u') => Some(ScrollCmd::HalfPageUp),
        KeyCode::Home | KeyCode::Char('g') => Some(ScrollCmd::Top),
        KeyCode::End | KeyCode::Char('G') => Some(ScrollCmd::Bottom),
        _ => None,
    }
}

/// Applies `cmd` to a scroll counter that runs `0..=max` top-to-bottom —
/// the viewer's `scroll` and the help screen's `scroll`.
fn apply_scroll_clamped(scroll: &mut usize, cmd: ScrollCmd, max: usize) {
    match cmd {
        ScrollCmd::Up => *scroll = scroll.saturating_sub(1),
        ScrollCmd::Down => *scroll = (*scroll + 1).min(max),
        ScrollCmd::PageUp => *scroll = scroll.saturating_sub(VIEWER_PAGE_SIZE),
        ScrollCmd::PageDown => *scroll = (*scroll + VIEWER_PAGE_SIZE).min(max),
        ScrollCmd::HalfPageUp => *scroll = scroll.saturating_sub(VIEWER_HALF_PAGE_SIZE),
        ScrollCmd::HalfPageDown => *scroll = (*scroll + VIEWER_HALF_PAGE_SIZE).min(max),
        ScrollCmd::Top => *scroll = 0,
        ScrollCmd::Bottom => *scroll = max,
    }
}

/// Applies `cmd` to the log view's `scroll_from_bottom`, whose sense is
/// inverted relative to `apply_scroll_clamped` (see `Mode::Log`'s doc
/// comment): "up" *increases* it, "down" *decreases* it, and there's no
/// upper bound to clamp against here — rendering saturates it down to a
/// meaningful range instead (see `App::log_wrapped`'s callers).
fn apply_scroll_inverted(scroll_from_bottom: &mut usize, cmd: ScrollCmd) {
    match cmd {
        ScrollCmd::Up => *scroll_from_bottom = scroll_from_bottom.saturating_add(1),
        ScrollCmd::Down => *scroll_from_bottom = scroll_from_bottom.saturating_sub(1),
        ScrollCmd::PageUp => {
            *scroll_from_bottom = scroll_from_bottom.saturating_add(VIEWER_PAGE_SIZE)
        }
        ScrollCmd::PageDown => {
            *scroll_from_bottom = scroll_from_bottom.saturating_sub(VIEWER_PAGE_SIZE)
        }
        ScrollCmd::HalfPageUp => {
            *scroll_from_bottom = scroll_from_bottom.saturating_add(VIEWER_HALF_PAGE_SIZE)
        }
        ScrollCmd::HalfPageDown => {
            *scroll_from_bottom = scroll_from_bottom.saturating_sub(VIEWER_HALF_PAGE_SIZE)
        }
        ScrollCmd::Top => *scroll_from_bottom = usize::MAX,
        ScrollCmd::Bottom => *scroll_from_bottom = 0,
    }
}

impl App {
    /// Loads `path` and switches to `Mode::Viewer`. Binary files no longer
    /// get rejected — they simply open in hex mode (see
    /// `viewer::LoadedFile::initial_mode`); only a genuine I/O error is
    /// logged and leaves the mode unchanged.
    pub(super) fn open_viewer(&mut self, path: &Path) {
        match viewer::load(path) {
            Ok(loaded) => {
                self.mode = Mode::Viewer {
                    path: path.to_path_buf(),
                    lines: loaded.lines,
                    bytes: loaded.bytes,
                    view_mode: loaded.initial_mode,
                    scroll: 0,
                    h_scroll: 0,
                    truncated: loaded.truncated,
                    search: ViewerSearch::Idle,
                };
            }
            Err(err) => self.log_error(format!("{}: {err}", path.display())),
        }
    }

    /// The Virtual Directory counterpart of `open_viewer`: extracts
    /// `inner_path` from `archive_path` fully into memory (capped at
    /// `viewer::SIZE_CAP`, same as any other viewer open) rather than
    /// reading from a real file path — a genuinely encrypted entry
    /// surfaces as a plain load error here, same treatment as any other
    /// unreadable entry.
    pub(super) fn open_viewer_virtual(&mut self, archive_path: &Path, inner_path: &Path) {
        let password = self
            .active_pane()
            .virtual_dir
            .as_ref()
            .and_then(|vd| vd.cached_password());
        match virtual_dir::extract_single_to_memory(
            archive_path,
            inner_path,
            viewer::SIZE_CAP,
            password.as_deref(),
        ) {
            Ok((bytes, truncated)) => {
                self.show_virtual_bytes(archive_path, inner_path, bytes, truncated)
            }
            // An encrypted entry (or a stale cached password after the
            // archive changed underneath) prompts instead of just logging
            // — see `commit_archive_password` for the retry.
            Err(err)
                if err
                    .downcast_ref::<virtual_dir::ZipPasswordError>()
                    .is_some() =>
            {
                if let Some(vd) = &self.active_pane().virtual_dir {
                    vd.clear_password();
                }
                self.mode = Mode::Prompt {
                    kind: PromptKind::ArchivePassword {
                        pending: PasswordPending::View {
                            archive_path: archive_path.to_path_buf(),
                            inner_path: inner_path.to_path_buf(),
                        },
                    },
                    input: LineEditor::new(),
                };
            }
            Err(err) => self.log_error(format!("{}: {err}", inner_path.display())),
        }
    }

    /// The shared "bytes are in hand, open the viewer" tail of
    /// `open_viewer_virtual` — also entered from a successful password
    /// prompt (`commit_archive_password`'s View branch).
    pub(super) fn show_virtual_bytes(
        &mut self,
        archive_path: &Path,
        inner_path: &Path,
        bytes: Vec<u8>,
        truncated: bool,
    ) {
        let loaded = viewer::load_bytes(bytes, truncated);
        let archive_name = archive_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| archive_path.display().to_string());
        let label = format!("{archive_name}:{}", virtual_dir::inner_display(inner_path));
        self.mode = Mode::Viewer {
            path: PathBuf::from(label),
            lines: loaded.lines,
            bytes: loaded.bytes,
            view_mode: loaded.initial_mode,
            scroll: 0,
            h_scroll: 0,
            truncated: loaded.truncated,
            search: ViewerSearch::Idle,
        };
    }

    /// Shared prologue for the Viewer/Help/Log screens' fixed keys: the
    /// close key(s) (`q` everywhere, plus `h` for Help), the two-step `Esc`
    /// (clears an active search first, closes on the next press), and
    /// starting a `/`/`?` search or stepping an existing one with `n`/`N`.
    /// Returns `true` once the key is fully handled — callers should
    /// `return` immediately; `false` means "not one of these, keep
    /// dispatching" (typically into that screen's own scroll keys).
    ///
    /// Assumes the caller has already peeled off `ViewerSearch::Editing`
    /// (routing it to `handle_pager_search_input_key` instead) before
    /// calling this — same layering as the three `handle_*_key` methods
    /// below.
    fn handle_pager_prologue(
        &mut self,
        code: KeyCode,
        close_keys: &[KeyCode],
        get_search: impl Fn(&mut Mode) -> Option<&mut ViewerSearch>,
        step: impl FnOnce(&mut Self, bool),
    ) -> bool {
        if close_keys.contains(&code) {
            self.mode = Mode::Normal;
            return true;
        }
        if code == KeyCode::Esc {
            let Some(search) = get_search(&mut self.mode) else {
                return true;
            };
            if matches!(search, ViewerSearch::Active { .. }) {
                // Clear the highlights/match state but stay open — a
                // second Esc (now with nothing left to clear) closes the
                // screen, same as the close key(s).
                *search = ViewerSearch::Idle;
            } else {
                self.mode = Mode::Normal;
            }
            return true;
        }
        if code == KeyCode::Char('/') {
            let Some(search) = get_search(&mut self.mode) else {
                return true;
            };
            search::begin(search, SearchDirection::Forward);
            return true;
        }
        if code == KeyCode::Char('?') {
            let Some(search) = get_search(&mut self.mode) else {
                return true;
            };
            search::begin(search, SearchDirection::Backward);
            return true;
        }
        if code == KeyCode::Char('n') {
            step(self, false);
            return true;
        }
        if code == KeyCode::Char('N') {
            step(self, true);
            return true;
        }
        false
    }

    /// Fixed keys for `Mode::Viewer`; never consults the keymap. A search
    /// input in progress (`ViewerSearch::Editing`) is peeled off first and
    /// routed to `handle_pager_search_input_key` — same "handle the nested
    /// modal layer before anything else" pattern as `handle_prompt_key`.
    /// The shared `q`/Esc/`/`/`?`/`n`/`N` prologue runs next (see
    /// `handle_pager_prologue`), before the main field-destructure, so
    /// assigning `self.mode = Mode::Normal` there never conflicts with the
    /// still-live borrow the scroll arms need.
    pub(super) fn handle_viewer_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let Mode::Viewer {
            search: ViewerSearch::Editing { .. },
            ..
        } = &self.mode
        {
            self.handle_pager_search_input_key(
                code,
                modifiers,
                |mode| match mode {
                    Mode::Viewer { search, .. } => Some(search),
                    _ => None,
                },
                Self::run_viewer_search,
            );
            return;
        }

        if self.handle_pager_prologue(
            code,
            &[KeyCode::Char('q')],
            |mode| match mode {
                Mode::Viewer { search, .. } => Some(search),
                _ => None,
            },
            Self::viewer_search_step,
        ) {
            return;
        }

        let Mode::Viewer {
            lines,
            bytes,
            view_mode,
            scroll,
            h_scroll,
            ..
        } = &mut self.mode
        else {
            return;
        };

        if code == KeyCode::Tab {
            *view_mode = view_mode.toggle();
            *scroll = 0;
            *h_scroll = 0;
            return;
        }

        let max_scroll = match view_mode {
            ViewMode::Text => lines.len().saturating_sub(1),
            ViewMode::Hex => bytes
                .len()
                .div_ceil(viewer::HEX_BYTES_PER_LINE)
                .saturating_sub(1),
        };
        if let Some(cmd) = scroll_cmd(code) {
            apply_scroll_clamped(scroll, cmd, max_scroll);
            return;
        }
        match code {
            KeyCode::Left => *h_scroll = h_scroll.saturating_sub(VIEWER_H_SCROLL_STEP),
            KeyCode::Right => *h_scroll += VIEWER_H_SCROLL_STEP,
            _ => {}
        }
    }

    /// Fixed keys for a pager screen's search input line (`ViewerSearch::
    /// Editing`) — delegates the actual state machine to
    /// `crate::search::handle_input_key`; only `Confirmed` needs any
    /// further work, via `run_search`. Shared by the viewer/help/log
    /// screens, which differ only in how to reach their `&mut
    /// ViewerSearch` (`get_search`) and how to run a confirmed search
    /// (`run_search`).
    fn handle_pager_search_input_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        get_search: impl FnOnce(&mut Mode) -> Option<&mut ViewerSearch>,
        run_search: impl FnOnce(&mut Self, String, SearchDirection),
    ) {
        let Some(search) = get_search(&mut self.mode) else {
            return;
        };
        match search::handle_input_key(search, code, modifiers) {
            search::InputOutcome::Editing | search::InputOutcome::Cancelled => {}
            search::InputOutcome::Confirmed(pattern, direction) => {
                run_search(self, pattern, direction);
            }
        }
    }

    /// Runs a `/`/`?` search once Enter confirms the pattern: builds the
    /// current view mode's haystack (text lines, or formatted hex-dump
    /// rows — see `viewer_search_haystack`) and hands it to
    /// `crate::search::run`, which does the actual matching/jump-with-wrap.
    /// A pattern that matches nothing logs `"pattern not found: ..."`
    /// instead of silently doing nothing (`search` itself is already left
    /// at `Idle` by `search::run` in that case).
    fn run_viewer_search(&mut self, pattern: String, direction: SearchDirection) {
        // Scoped to an immutable borrow of `self.mode`, dropped before the
        // `&mut self.mode` borrow below — building the haystack never
        // needs to mutate anything.
        let haystack: Vec<String> = {
            let Mode::Viewer {
                lines,
                bytes,
                view_mode,
                ..
            } = &self.mode
            else {
                return;
            };
            viewer_search_haystack(lines, bytes, *view_mode).into_owned()
        };

        let Mode::Viewer { scroll, search, .. } = &mut self.mode else {
            return;
        };
        match search::run(search, &haystack, pattern, direction, *scroll) {
            Ok(target) => *scroll = target,
            Err(pattern) => self.log_error(format!("pattern not found: {pattern}")),
        }
    }

    /// `n` (`reverse: false`) / `N` (`reverse: true`) — delegates to
    /// `crate::search::step`; a no-op when there's no active search.
    fn viewer_search_step(&mut self, reverse: bool) {
        let Mode::Viewer { scroll, search, .. } = &mut self.mode else {
            return;
        };
        if let Some(target) = search::step(search, reverse) {
            *scroll = target;
        }
    }

    pub(super) fn begin_help(&mut self) {
        self.mode = Mode::Help {
            scroll: 0,
            search: ViewerSearch::Idle,
        };
    }

    /// The help screen's line listing (`crate::help::build_lines`), from a
    /// cache keyed by `Keymap::generation` — rebuilt only when the keymap
    /// itself changes (config reload / a settings-screen rebind), not on
    /// every scroll/search keystroke against the already-open help screen.
    /// `Keymap::generation`'s own doc comment explains why a
    /// per-`Keymap`-*instance* id is enough of a cache key even though
    /// nothing here ever mutates a `Keymap` in place.
    pub(crate) fn help_lines(&mut self) -> &[crate::help::HelpLine] {
        let generation = self.keymap.generation();
        let stale = match &self.help_lines_cache {
            Some((cached_gen, _)) => *cached_gen != generation,
            None => true,
        };
        if stale {
            self.help_lines_cache = Some((generation, crate::help::build_lines(&self.keymap)));
        }
        &self.help_lines_cache.as_ref().unwrap().1
    }

    /// Fixed keys for `Mode::Help`; never consults the keymap (it would be
    /// circular — this screen exists to document the keymap). Same
    /// `less`-style scroll/search keys as the viewer's text mode (see
    /// `Mode::Help`'s doc comment for the exact set); `q`/Esc/`h` close
    /// back to Normal (a first `Esc` while a search is active clears it
    /// instead). The listing (`self.help_lines()`) only actually gets
    /// rebuilt when the keymap changes, not on every keypress here.
    pub(super) fn handle_help_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let Mode::Help {
            search: ViewerSearch::Editing { .. },
            ..
        } = &self.mode
        {
            self.handle_pager_search_input_key(
                code,
                modifiers,
                |mode| match mode {
                    Mode::Help { search, .. } => Some(search),
                    _ => None,
                },
                Self::run_help_search,
            );
            return;
        }

        if self.handle_pager_prologue(
            code,
            &[KeyCode::Char('q'), KeyCode::Char('h')],
            |mode| match mode {
                Mode::Help { search, .. } => Some(search),
                _ => None,
            },
            Self::help_search_step,
        ) {
            return;
        }

        let max_scroll = self.help_lines().len().saturating_sub(1);
        let Mode::Help { scroll, .. } = &mut self.mode else {
            return;
        };
        if let Some(cmd) = scroll_cmd(code) {
            apply_scroll_clamped(scroll, cmd, max_scroll);
        }
    }

    /// Runs a confirmed Help-screen search against `self.help_lines()`'s
    /// plain-text rendering of the current keymap listing — see
    /// `run_viewer_search`'s doc comment for the shared shape.
    fn run_help_search(&mut self, pattern: String, direction: SearchDirection) {
        let haystack: Vec<String> = self.help_lines().iter().map(HelpLine::text).collect();
        let Mode::Help { scroll, search } = &mut self.mode else {
            return;
        };
        match search::run(search, &haystack, pattern, direction, *scroll) {
            Ok(target) => *scroll = target,
            Err(pattern) => self.log_error(format!("pattern not found: {pattern}")),
        }
    }

    fn help_search_step(&mut self, reverse: bool) {
        let Mode::Help { scroll, search } = &mut self.mode else {
            return;
        };
        if let Some(target) = search::step(search, reverse) {
            *scroll = target;
        }
    }

    /// `S-l`/`L`: opens the full-frame in-memory log viewer, scrolled to
    /// the bottom (the newest content — `scroll_from_bottom: 0`).
    pub(super) fn begin_show_log(&mut self) {
        self.mode = Mode::Log {
            scroll_from_bottom: 0,
            search: ViewerSearch::Idle,
        };
    }

    /// Fixed keys for `Mode::Log`; same `less`-style scroll/search keys as
    /// Help/the viewer, mapped onto `scroll_from_bottom`'s inverted sense
    /// via `apply_scroll_inverted` (see `Mode::Log`'s doc comment) —
    /// `up`/`k`/`b` (page up) and `u` (half page up) all *increase* it
    /// (scroll up, away from the bottom); `down`/`j`/`f`/`space` (page
    /// down) and `d` (half page down) all *decrease* it — exactly
    /// mirroring the pre-existing Up/Down asymmetry this had before search
    /// was added.
    /// `scroll_from_bottom` only grows/shrinks here — it's rendering
    /// (`ui::log_view::render_full`, which knows the terminal width and
    /// therefore the real wrapped row count) that clamps it to a
    /// meaningful range, so `Home`/`g` can just jump to `usize::MAX` and
    /// let the render side saturate it down to "the very top".
    pub(super) fn handle_log_view_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let Mode::Log {
            search: ViewerSearch::Editing { .. },
            ..
        } = &self.mode
        {
            self.handle_pager_search_input_key(
                code,
                modifiers,
                |mode| match mode {
                    Mode::Log { search, .. } => Some(search),
                    _ => None,
                },
                Self::run_log_search,
            );
            return;
        }

        if self.handle_pager_prologue(
            code,
            &[KeyCode::Char('q')],
            |mode| match mode {
                Mode::Log { search, .. } => Some(search),
                _ => None,
            },
            Self::log_search_step,
        ) {
            return;
        }

        let Mode::Log {
            scroll_from_bottom, ..
        } = &mut self.mode
        else {
            return;
        };
        if let Some(cmd) = scroll_cmd(code) {
            apply_scroll_inverted(scroll_from_bottom, cmd);
        }
    }

    /// The entire in-memory log wrapped at `width`, from a cache keyed by
    /// `(log_generation, width)` (see their doc comments) — rebuilt only
    /// when the log actually changed or the caller asks for a different
    /// width, not on every call. `render_full` calls this every frame the
    /// full log view is open; `run_log_search`/`log_search_step` each call
    /// it once per search keystroke — both share the one cache slot, since
    /// they're always asking about the same width (`self.log_view_width`,
    /// the one `render_full` itself just wrote).
    pub(crate) fn log_wrapped(&mut self, width: u16) -> &[(String, bool)] {
        let stale = match &self.log_wrap_cache {
            Some((cached_gen, cached_width, _)) => {
                *cached_gen != self.log_generation || *cached_width != width
            }
            None => true,
        };
        if stale {
            let wrapped = crate::logwrap::wrap_log_lines(self.log.iter(), width as usize);
            self.log_wrap_cache = Some((self.log_generation, width, wrapped));
        }
        &self.log_wrap_cache.as_ref().unwrap().2
    }

    /// Runs a confirmed Log-view search: rewraps the log at
    /// `self.log_view_width` (the width `ui::log_view::render_full` last
    /// actually rendered at — see its doc comment) into the exact same
    /// rows the screen shows, searches those, and converts the matched row
    /// index into `scroll_from_bottom` units — `total - 1 - row_idx`, which
    /// (per `ui::log_view::log_view_start`'s own math) lands the match as
    /// the *last* (bottom-most) visible row, independent of viewport
    /// height (which, unlike width, isn't cached anywhere — this
    /// conversion doesn't need it).
    fn run_log_search(&mut self, pattern: String, direction: SearchDirection) {
        let width = self.log_view_width;
        let haystack: Vec<String> = self
            .log_wrapped(width)
            .iter()
            .map(|(text, _)| text.clone())
            .collect();
        let total = haystack.len();

        let from_index = {
            let Mode::Log {
                scroll_from_bottom, ..
            } = &self.mode
            else {
                return;
            };
            total
                .saturating_sub(1)
                .saturating_sub((*scroll_from_bottom).min(total.saturating_sub(1)))
        };

        let Mode::Log {
            scroll_from_bottom,
            search,
        } = &mut self.mode
        else {
            return;
        };
        match search::run(search, &haystack, pattern, direction, from_index) {
            Ok(row_idx) => {
                *scroll_from_bottom = total.saturating_sub(1).saturating_sub(row_idx);
            }
            Err(pattern) => self.log_error(format!("pattern not found: {pattern}")),
        }
    }

    fn log_search_step(&mut self, reverse: bool) {
        let width = self.log_view_width;
        let total = self.log_wrapped(width).len();
        let Mode::Log {
            scroll_from_bottom,
            search,
        } = &mut self.mode
        else {
            return;
        };
        if let Some(row_idx) = search::step(search, reverse) {
            *scroll_from_bottom = total.saturating_sub(1).saturating_sub(row_idx);
        }
    }
}
