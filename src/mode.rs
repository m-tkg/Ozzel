//! Input mode: `Normal` routes keys through the `Keymap`, `Prompt` and
//! `Confirm` are modal and consume fixed editing keys directly (see
//! `App::handle_prompt_key` / `App::handle_confirm_key`).

use std::path::PathBuf;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// What a `Mode::Prompt` is collecting text for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Rename {
        orig: String,
    },
    Mkdir,
    /// Collecting the archive file name for a zip-create; `targets` is
    /// captured at prompt-open time (the marks/cursor selection that
    /// triggered it), not re-read when the prompt commits.
    ZipName {
        targets: Vec<PathBuf>,
    },
    /// Collecting a `:`-command line to run with the TUI suspended.
    Command,
    /// Collecting the new name for `duplicate` (`c`): prefilled with the
    /// cursor entry's current name, committed as a copy of `source` under
    /// the typed name in the *same* directory. `source` is captured at
    /// prompt-open time, same story as `ZipName`'s `targets`.
    Duplicate {
        source: PathBuf,
    },
}

/// Which jump menu a `Mode::Select` is showing — `d` (delete) only makes
/// sense for `Bookmark`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    History,
    Bookmark,
}

/// Which direction a marked-or-cursor transfer is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Copy,
    Move,
}

/// Which representation the built-in viewer is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Text,
    /// An `xxd`-style hex dump of the raw bytes (see `viewer::format_hex_line`).
    Hex,
}

impl ViewMode {
    pub fn toggle(self) -> Self {
        match self {
            ViewMode::Text => ViewMode::Hex,
            ViewMode::Hex => ViewMode::Text,
        }
    }
}

/// Which way a viewer search (`/` vs `?`) reads the file — `n` repeats the
/// last search in this direction, `N` in the reverse of it, exactly like
/// `less`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    /// `/` — toward the end of the file.
    Forward,
    /// `?` — toward the start of the file.
    Backward,
}

impl SearchDirection {
    pub fn reversed(self) -> Self {
        match self {
            SearchDirection::Forward => SearchDirection::Backward,
            SearchDirection::Backward => SearchDirection::Forward,
        }
    }
}

/// `less`-style incremental search state for `Mode::Viewer`. `Idle` is the
/// steady state outside of any search; pressing `/` or `?` moves to
/// `Editing` (a bottom input line, same UI pattern as `Filter`/
/// `JumpSearch`); `Enter` there runs the search and — if it matched
/// anything — moves to `Active`, which drives both the highlighted matches
/// on screen and `n`/`N` navigation until `Esc` (in plain, non-editing
/// viewer state) clears it back to `Idle`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ViewerSearch {
    #[default]
    Idle,
    Editing {
        input: LineEditor,
        direction: SearchDirection,
        /// The search state (if any) that was active before `/`/`?` was
        /// pressed, restored verbatim on `Esc` — canceling an in-progress
        /// search must never lose a previous search's highlights, the same
        /// way `less` leaves you exactly where you were before pressing
        /// `/` again if you back out of it.
        previous: Box<ViewerSearch>,
    },
    Active {
        /// The raw text as typed — re-compiled into a `viewer::Matcher` on
        /// demand (by `n`/`N` navigation and by rendering, for
        /// highlighting) rather than stored compiled, since a compiled
        /// `regex::Regex` doesn't implement `PartialEq`/`Eq` and every
        /// other `Mode` variant does.
        pattern: String,
        direction: SearchDirection,
        /// Line indices (text mode) or hex-dump row indices (hex mode)
        /// containing at least one match, ascending. Always non-empty —
        /// a search with zero matches never produces `Active` (see
        /// `App::run_viewer_search`).
        matches: Vec<usize>,
        /// Index into `matches` of the line the viewer is currently
        /// parked on — what `n`/`N` advance and the footer's `i/N` counter
        /// reads.
        current: usize,
        /// Set by `App::run_viewer_search`/`App::viewer_search_step`
        /// whenever the *most recent* jump had to wrap around the start or
        /// end of the file to find a match — drives the footer's `less`-
        /// style "search wrapped" notice. Cleared by the next jump that
        /// *doesn't* wrap, not time-based.
        wrapped: bool,
    },
}

/// The operation a `Mode::Confirm` will perform if the user answers yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    Delete {
        targets: Vec<PathBuf>,
    },
    /// A confirmed Copy/Move ready to spawn. Despite the name, this covers
    /// both cases that lead to a confirm dialog: an actual filename
    /// collision, and (when `config.confirm_operations` is true, the
    /// default) a plain transfer with no collision at all — the confirm
    /// message itself distinguishes the two (see `App::begin_transfer`).
    Overwrite {
        kind: TransferKind,
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    /// The archive path already existed; overwrite it.
    ZipOverwrite {
        targets: Vec<PathBuf>,
        archive_path: PathBuf,
    },
    /// One or more top-level entries in the archive already exist in the
    /// destination directory; overwrite them.
    UnzipOverwrite {
        archive_path: PathBuf,
        dest_dir: PathBuf,
    },
    /// A confirmed extraction from a Virtual Directory (`C` while the
    /// active pane is browsing inside a `.zip`): a partial extraction of
    /// the marked/cursor entries. `inner_targets` are archive-internal paths
    /// (marks/cursor from the virtual pane), extracted into `dest_dir`
    /// (the *other*, necessarily real, pane's cwd).
    Extract {
        archive_path: PathBuf,
        inner_targets: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    /// Confirmed by the quit-while-busy guard: tasks are still running but
    /// the user wants out anyway.
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    /// Incremental filter/search: every edit keystroke live-applies to the
    /// active pane's `Pane.filter` (see `App::handle_filter_key`).
    Filter {
        input: LineEditor,
    },
    /// Prefix-jump incremental search (`\`): pure cursor movement, never
    /// hides/filters the listing (that's `Filter`'s job) — every keystroke
    /// moves the active pane's cursor to the first visible entry whose
    /// name starts with what's typed so far. See
    /// `App::handle_jump_search_key`.
    JumpSearch {
        input: LineEditor,
        /// The cursor position when the search was opened, restored
        /// verbatim on `Esc` — `Enter` leaves the cursor wherever the
        /// search left it instead.
        original_cursor: usize,
    },
    /// A centered jump menu (history or bookmarks): up/down move, Enter
    /// selects (the active pane cd's there), Esc cancels, and `d` deletes
    /// the highlighted entry when `kind` is `Bookmark`.
    Select {
        kind: SelectKind,
        title: String,
        items: Vec<(String, PathBuf)>,
        cursor: usize,
    },
    Prompt {
        kind: PromptKind,
        input: LineEditor,
    },
    Confirm {
        message: String,
        on_yes: PendingOp,
    },
    /// The built-in full-frame text viewer (`x`/`o`/Enter-on-file),
    /// `less`-like. Fixed keys only (see `App::handle_viewer_key`):
    /// Up/Down/PageUp/PageDown/Home/End (`g`/`G`, `j`/`k`, `f`/`b`/`Space`,
    /// `d`/`u` too) scroll vertically, Left/Right scroll horizontally, Tab
    /// toggles text/hex mode, `/`/`?` open a forward/backward search input
    /// (see `ViewerSearch`), `n`/`N` repeat it forward/backward, `q`/Esc
    /// closes back to the filer (Esc clears an active search's highlights
    /// first instead, if there is one).
    Viewer {
        path: PathBuf,
        lines: Vec<String>,
        /// The raw bytes backing `lines` (lossily decoded from these),
        /// retained so hex mode can render the exact on-disk bytes.
        bytes: Vec<u8>,
        view_mode: ViewMode,
        /// Index of the first visible line (text mode) or first visible
        /// 16-byte row (hex mode) — reset to 0 on every Tab toggle, since
        /// the two modes don't share a scroll position.
        scroll: usize,
        /// Display-column offset of the first visible column (text mode
        /// only; hex mode ignores this and always shows full rows).
        h_scroll: usize,
        /// The file was larger than the viewer's size cap and got cut off.
        truncated: bool,
        /// `less`-style `/`/`?` search state — see `ViewerSearch`.
        search: ViewerSearch,
    },
    /// The full-frame keybinding help screen (`h`/`?`). Fixed keys only
    /// (see `App::handle_help_key`): Up/Down/PageUp/PageDown/Home/End
    /// (`g`/`G` too) scroll, `q`/Esc/`h` closes back to the filer. The
    /// listing itself (`crate::help::build_lines`) is computed on demand
    /// from the live `Keymap`, never stored here, so it always reflects
    /// the current effective bindings.
    Help {
        /// Index of the first visible line.
        scroll: usize,
    },
    /// The full-frame in-memory log viewer (`S-l`/`L`). Fixed keys only
    /// (see `App::handle_log_view_key`), same scroll keys as the viewer's
    /// text mode. `scroll_from_bottom` is measured in wrapped display rows
    /// *up from the newest content* (0 = pinned to the bottom, which is
    /// where this mode always opens) rather than a raw line index, since
    /// how many rows the log wraps into depends on terminal width — a
    /// width `App` has no access to; `ui::log_view::render_full` (which
    /// does have it) is what actually turns this into a display offset and
    /// clamps it, so this field can grow past the real maximum here with
    /// no ill effect (see also `Home`'s use of `usize::MAX`).
    Log {
        scroll_from_bottom: usize,
    },
    /// The command palette (`F`/`S-f`): a filterable, scrollable list of
    /// every action. Typing narrows the
    /// list (see `crate::function_list::filter_actions`); `cursor` indexes
    /// into that *filtered* list, not `Action::ALL`, so it's clamped
    /// relative to whatever the current `input` matches.
    FunctionList {
        input: LineEditor,
        cursor: usize,
    },
}

/// A grapheme-safe single-line text editor: every unit the cursor moves
/// over, inserts before, or deletes is a whole grapheme cluster, so
/// Japanese (and other multi-byte) filenames edit correctly instead of
/// getting split mid-character.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineEditor {
    graphemes: Vec<String>,
    /// Index into `graphemes` (0..=len), not a byte offset.
    cursor: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_str(s: &str) -> Self {
        let graphemes: Vec<String> = s.graphemes(true).map(str::to_string).collect();
        let cursor = graphemes.len();
        Self { graphemes, cursor }
    }

    pub fn value(&self) -> String {
        self.graphemes.concat()
    }

    /// Inserts `ch` immediately before the cursor and advances past it.
    pub fn insert(&mut self, ch: char) {
        self.graphemes.insert(self.cursor, ch.to_string());
        self.cursor += 1;
    }

    /// Deletes the grapheme before the cursor (classic backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.graphemes.remove(self.cursor);
        }
    }

    /// Deletes the grapheme under/after the cursor (forward delete).
    pub fn delete(&mut self) {
        if self.cursor < self.graphemes.len() {
            self.graphemes.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.graphemes.len() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.graphemes.len();
    }

    /// Display-column offset of the cursor from the start of the line,
    /// accounting for wide (e.g. full-width Japanese) graphemes.
    pub fn cursor_display_col(&self) -> usize {
        self.graphemes[..self.cursor]
            .iter()
            .map(|g| UnicodeWidthStr::width(g.as_str()))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_value_round_trip_ascii() {
        let mut editor = LineEditor::new();
        for c in "hello".chars() {
            editor.insert(c);
        }
        assert_eq!(editor.value(), "hello");
    }

    #[test]
    fn backspace_removes_one_grapheme_not_one_byte() {
        let mut editor = LineEditor::from_str("日本語ファイル名.txt");
        editor.backspace();
        assert_eq!(editor.value(), "日本語ファイル名.tx");
    }

    #[test]
    fn insert_in_the_middle_of_japanese_text() {
        let mut editor = LineEditor::from_str("日本語.txt");
        editor.move_home();
        editor.move_right();
        editor.move_right();
        editor.insert('X');
        assert_eq!(editor.value(), "日本X語.txt");
    }

    #[test]
    fn delete_removes_grapheme_after_cursor() {
        let mut editor = LineEditor::from_str("日本語");
        editor.move_home();
        editor.delete();
        assert_eq!(editor.value(), "本語");
    }

    #[test]
    fn move_left_right_home_end_stay_in_bounds() {
        let mut editor = LineEditor::from_str("ab");
        editor.move_right(); // already at end, no-op
        assert_eq!(editor.cursor, 2);
        editor.move_home();
        editor.move_left(); // already at start, no-op
        assert_eq!(editor.cursor, 0);
        editor.move_end();
        assert_eq!(editor.cursor, 2);
    }

    #[test]
    fn cursor_display_col_accounts_for_wide_graphemes() {
        let mut editor = LineEditor::from_str("日本");
        assert_eq!(editor.cursor_display_col(), 4); // two width-2 graphemes
        editor.move_home();
        assert_eq!(editor.cursor_display_col(), 0);
    }
}
