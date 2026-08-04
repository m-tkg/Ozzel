//! Input mode: `Normal` routes keys through the `Keymap`, `Prompt` and
//! `Confirm` are modal and consume fixed editing keys directly (see
//! `App::handle_prompt_key` / `App::handle_confirm_key`).

use std::path::PathBuf;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// What a `Mode::Prompt` is collecting text for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Rename { orig: String },
    Mkdir,
}

/// Which direction a marked-or-cursor transfer is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    Copy,
    Move,
}

/// The operation a `Mode::Confirm` will perform if the user answers yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOp {
    Delete {
        targets: Vec<PathBuf>,
    },
    Overwrite {
        kind: TransferKind,
        sources: Vec<PathBuf>,
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
    Prompt {
        kind: PromptKind,
        input: LineEditor,
    },
    Confirm {
        message: String,
        on_yes: PendingOp,
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
