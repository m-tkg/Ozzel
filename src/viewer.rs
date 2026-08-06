//! Loads a file for the built-in viewer (`Mode::Viewer`): capped at a
//! maximum size, decoded lossily as UTF-8 for the text view, and always
//! available as a hex dump too (see `format_hex_line`). Files that sniff as
//! binary simply open in hex mode initially rather than being rejected —
//! the user can Tab over to the (possibly garbled) text view if they want.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use regex::{Regex, RegexBuilder};

use crate::mode::ViewMode;

/// Files larger than this are read only up to the cap; the viewer shows a
/// "truncated" note in its footer in that case rather than silently
/// pretending the file ends there. `pub` so `virtual_dir::extract_single_to_memory`
/// (a virtual-directory file open) can honor the exact same cap without
/// this module needing to know anything about zip archives.
pub const SIZE_CAP: u64 = 10 * 1024 * 1024; // 10 MiB
/// How many leading bytes are sniffed for a NUL byte to decide "this looks
/// like a binary file" and therefore should default to opening in hex mode.
const BINARY_SNIFF_LEN: usize = 8 * 1024; // 8 KiB
const TAB_WIDTH: usize = 4;
/// Bytes per hex-dump row, matching `xxd`'s default layout (two groups of 8).
pub const HEX_BYTES_PER_LINE: usize = 16;

#[derive(Debug)]
pub struct LoadedFile {
    pub lines: Vec<String>,
    pub bytes: Vec<u8>,
    /// Which view mode the viewer should open in: `Hex` when the file
    /// sniffed as binary, `Text` otherwise. The user can always Tab to the
    /// other mode regardless of this choice.
    pub initial_mode: ViewMode,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum LoadError {
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

/// Reads `path` up to `SIZE_CAP` bytes and returns both the tab-expanded
/// text lines (decoded lossily where necessary) and the raw bytes, so the
/// viewer can show either representation and toggle between them with Tab.
pub fn load(path: &Path) -> Result<LoadedFile, LoadError> {
    let (bytes, truncated) = read_capped(path)?;
    Ok(load_bytes(bytes, truncated))
}

/// The non-fs half of `load`: builds a `LoadedFile` from bytes already
/// read from wherever (a plain file for `load`, or a zip entry extracted
/// to memory for a virtual-directory file open — see
/// `virtual_dir::extract_single_to_memory`) plus whether the caller
/// already truncated them at some size cap. Infallible — sniffing/
/// decoding bytes that are already in hand can't fail the way reading
/// from disk can.
pub fn load_bytes(bytes: Vec<u8>, truncated: bool) -> LoadedFile {
    let initial_mode = if looks_binary(&bytes) {
        ViewMode::Hex
    } else {
        ViewMode::Text
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().map(expand_tabs).collect();
    LoadedFile {
        lines,
        bytes,
        initial_mode,
        truncated,
    }
}

// `pub(crate)`: the `=` (diff) action reads both sides through the exact
// same cap the viewer itself uses (see `App::begin_diff`).
pub(crate) fn read_capped(path: &Path) -> Result<(Vec<u8>, bool), LoadError> {
    let metadata = fs::metadata(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let file_len = metadata.len();
    let to_read = file_len.min(SIZE_CAP) as usize;

    let mut file = File::open(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf)
        .map_err(|e| LoadError::Io(e.to_string()))?;

    Ok((buf, file_len > SIZE_CAP))
}

// `pub(crate)`: the `=` (diff) action refuses to diff binary files using
// the same sniff the viewer uses to pick its initial mode.
pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    let sniff_len = bytes.len().min(BINARY_SNIFF_LEN);
    bytes[..sniff_len].contains(&0)
}

/// Expands tabs to the next multiple-of-`TAB_WIDTH` display column (not
/// just "insert 4 spaces"), so horizontal-scroll math downstream lines up
/// the same way a real tab stop would.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for ch in line.chars() {
        if ch == '\t' {
            let spaces = TAB_WIDTH - (col % TAB_WIDTH);
            out.push_str(&" ".repeat(spaces));
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Formats one `xxd`-style hex-dump line for `chunk` (up to
/// `HEX_BYTES_PER_LINE` bytes), starting at byte offset `offset` into the
/// file: an 8-digit hex offset, up to 16 bytes as 2-digit hex grouped 8+8
/// (a partial final chunk pads with blank space so the ASCII gutter still
/// lines up in a fixed-width font), and a `|...|` ASCII gutter where
/// non-printable bytes (anything outside the printable ASCII range) show as
/// `.`.
pub fn format_hex_line(chunk: &[u8], offset: usize) -> String {
    let mut out = format!("{offset:08x}  ");
    for i in 0..HEX_BYTES_PER_LINE {
        if i == 8 {
            out.push(' ');
        }
        if let Some(byte) = chunk.get(i) {
            out.push_str(&format!("{byte:02x} "));
        } else {
            out.push_str("   ");
        }
    }
    out.push_str(" |");
    for &byte in chunk {
        let ch = if (0x20..=0x7e).contains(&byte) {
            byte as char
        } else {
            '.'
        };
        out.push(ch);
    }
    out.push('|');
    out
}

/// Formats every row of `bytes` as `format_hex_line` would, for hex-mode
/// search (`Mode::Viewer`'s `/`/`?`, see `Matcher`) — the spec's "search
/// the formatted line strings" for hex mode, so a search for e.g. `48 65`
/// matches the same text a user reading the hex dump actually sees. Not
/// used for rendering (the viewer only ever formats the rows currently on
/// screen — see `ui::viewer_view::render`); this materializes every row up
/// front, which is fine as a one-shot, search-triggered cost even at the
/// 10 MiB `SIZE_CAP`, but would be wasteful to do every frame.
pub fn format_hex_lines(bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(HEX_BYTES_PER_LINE)
        .enumerate()
        .map(|(row, chunk)| format_hex_line(chunk, row * HEX_BYTES_PER_LINE))
        .collect()
}

/// A compiled `less`-style search pattern (`Mode::Viewer`'s `/`/`?`): tried
/// as a case-insensitive regex first, matching `less`'s own regex-first
/// search — falling back to a plain case-insensitive substring match when
/// the typed text isn't valid regex syntax, so a literal search
/// containing `(`, `[`, `.` etc. degrades gracefully instead of erroring
/// or matching nothing. Never stored on `Mode` itself (see
/// `crate::mode::ViewerSearch::Active`'s doc comment for why) — built on
/// demand from the plain `pattern: String` it carries instead, both when
/// running/advancing a search and when rendering highlights. Compiled once
/// by `crate::search::run` and cached on `ViewerSearch::Active` behind an
/// `Rc` (see its doc comment) rather than rebuilt every render frame.
#[derive(Debug)]
pub enum Matcher {
    Regex(Regex),
    /// Already lowercased, so `is_match`/`find_ranges` don't have to
    /// re-lowercase it on every call.
    Literal(String),
}

impl Matcher {
    pub fn build(pattern: &str) -> Self {
        if pattern.is_empty() {
            // An empty pattern is technically valid regex syntax (it
            // matches a zero-width string at every position), which would
            // make `is_match`/`find_ranges` claim "the whole line matched"
            // — never useful, and not what an empty search box should
            // mean. Force the literal path instead, where an empty needle
            // already means "no matches" (see `find_literal_ranges`).
            return Matcher::Literal(String::new());
        }
        match RegexBuilder::new(pattern).case_insensitive(true).build() {
            Ok(re) => Matcher::Regex(re),
            Err(_) => Matcher::Literal(pattern.to_lowercase()),
        }
    }

    pub fn is_match(&self, line: &str) -> bool {
        match self {
            Matcher::Regex(re) => re.is_match(line),
            // An empty needle is trivially "contained" by every string —
            // explicitly excluded so this agrees with `find_ranges`
            // (empty pattern -> no matches, not "matches everywhere").
            Matcher::Literal(needle) => {
                !needle.is_empty() && line.to_lowercase().contains(needle.as_str())
            }
        }
    }

    /// Byte ranges (into `line`) of every match, for highlighting — see
    /// `ui::viewer_view`'s use of this to build styled spans. Empty for an
    /// empty literal needle (an empty pattern never reaches here in
    /// practice — `App::handle_viewer_search_input_key` treats an empty
    /// `Enter` as a cancel — but an unconditional substring match at every
    /// position would be a nonsensical "highlight everything" result if it
    /// ever did).
    pub fn find_ranges(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Matcher::Regex(re) => re.find_iter(line).map(|m| (m.start(), m.end())).collect(),
            Matcher::Literal(needle) => find_literal_ranges(line, needle),
        }
    }
}

/// Case-insensitive literal substring search returning *byte* ranges into
/// the original (not lowercased) `line`, char by char rather than via a
/// whole-string `to_lowercase` + byte-offset re-mapping — the latter would
/// drift out of sync whenever lowercasing changes a character's UTF-8
/// byte length (rare, but real: e.g. Turkish İ → "i̇" is 1 byte → 2
/// bytes). `needle_lower` must already be lowercased.
fn find_literal_ranges(line: &str, needle_lower: &str) -> Vec<(usize, usize)> {
    if needle_lower.is_empty() {
        return Vec::new();
    }
    let needle_chars: Vec<char> = needle_lower.chars().collect();
    let line_chars: Vec<(usize, char)> = line.char_indices().collect();
    if line_chars.len() < needle_chars.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for start in 0..=(line_chars.len() - needle_chars.len()) {
        let is_match = needle_chars
            .iter()
            .enumerate()
            .all(|(k, &nc)| chars_match_ci(line_chars[start + k].1, nc));
        if is_match {
            let start_byte = line_chars[start].0;
            let end_byte = line_chars
                .get(start + needle_chars.len())
                .map(|&(b, _)| b)
                .unwrap_or(line.len());
            out.push((start_byte, end_byte));
        }
    }
    out
}

/// Whether `a` and `b` are the same character ignoring case, via full
/// Unicode case folding (`char::to_lowercase`) rather than just ASCII —
/// e.g. "É" matches "é". Compares each side as a single `char`, so it
/// doesn't handle the rare case where lowercasing one of them yields more
/// than one `char` (e.g. Turkish İ) — an intentional, documented
/// simplification; Japanese (no case distinction at all) and ASCII/Latin
/// text are unaffected.
fn chars_match_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_plain_text_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "line one\nline two\nline three").unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.lines, vec!["line one", "line two", "line three"]);
        assert!(!loaded.truncated);
        assert_eq!(loaded.initial_mode, ViewMode::Text);
    }

    #[test]
    fn loads_japanese_text_lossless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jp.txt");
        std::fs::write(&path, "日本語のテキスト\n二行目です").unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.lines, vec!["日本語のテキスト", "二行目です"]);
        assert_eq!(loaded.initial_mode, ViewMode::Text);
    }

    #[test]
    fn non_utf8_bytes_are_decoded_lossily_not_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shiftjis.txt");
        // Shift-JIS bytes for "あい" — not valid UTF-8, but also has no NUL
        // byte, so it should decode lossily and still default to text mode.
        std::fs::write(&path, [0x82, 0xa0, 0x82, 0xa2]).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.lines.len(), 1);
        assert!(
            loaded.lines[0].contains('\u{FFFD}'),
            "invalid bytes become U+FFFD"
        );
        assert_eq!(loaded.initial_mode, ViewMode::Text);
    }

    #[test]
    fn binary_file_with_nul_byte_opens_in_hex_mode_not_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [b'a', b'b', 0u8, b'c']).unwrap();

        let loaded = load(&path).expect("binary files must load successfully now");
        assert_eq!(loaded.initial_mode, ViewMode::Hex);
        assert_eq!(loaded.bytes, vec![b'a', b'b', 0u8, b'c']);
    }

    #[test]
    fn nul_byte_beyond_sniff_window_is_not_detected() {
        // Documents the heuristic's limit: only the first BINARY_SNIFF_LEN
        // bytes are checked, so a NUL byte further in is missed. This is
        // an intentional MVP trade-off (cheap check), not a bug.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mostly_text.dat");
        let mut content = vec![b'a'; BINARY_SNIFF_LEN + 100];
        content[BINARY_SNIFF_LEN + 50] = 0;
        std::fs::write(&path, &content).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.initial_mode, ViewMode::Text);
    }

    #[test]
    fn large_file_is_truncated_at_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // One byte over the cap, all non-newline so it reads as one giant
        // line — content doesn't matter, only that truncation is flagged
        // and the read stops at the cap rather than reading everything.
        let content = vec![b'x'; (SIZE_CAP + 1) as usize];
        std::fs::write(&path, &content).unwrap();

        let loaded = load(&path).unwrap();
        assert!(loaded.truncated);
        let total_len: usize = loaded.lines.iter().map(|l| l.len()).sum();
        assert_eq!(total_len as u64, SIZE_CAP);
        assert_eq!(loaded.bytes.len() as u64, SIZE_CAP);
    }

    #[test]
    fn expand_tabs_pads_to_next_tab_stop() {
        assert_eq!(expand_tabs("a\tb"), "a   b"); // 'a' at col 0, tab -> col 4
        assert_eq!(expand_tabs("ab\tc"), "ab  c"); // 'ab' at col 0-1, tab -> col 4
        assert_eq!(expand_tabs("abcd\te"), "abcd    e"); // exactly at a tab stop -> full 4 spaces
        assert_eq!(expand_tabs("no tabs here"), "no tabs here");
    }

    #[test]
    fn format_hex_line_full_row_matches_xxd_style_layout() {
        let chunk = b"Hello world!\x0a\x01\x02\x03";
        assert_eq!(chunk.len(), 16);
        let line = format_hex_line(chunk, 0x10);
        assert_eq!(
            line,
            "00000010  48 65 6c 6c 6f 20 77 6f  72 6c 64 21 0a 01 02 03  |Hello world!....|"
        );
    }

    #[test]
    fn format_hex_line_partial_last_line_pads_hex_but_not_ascii_gutter() {
        let chunk = b"Hi!";
        let line = format_hex_line(chunk, 0);
        assert_eq!(
            line,
            "00000000  48 69 21                                          |Hi!|"
        );
    }

    #[test]
    fn format_hex_line_non_printable_bytes_show_as_dot() {
        // Only 0x20 and 0x7e (space and '~') are printable ASCII; the rest
        // must show as '.' in the gutter.
        let chunk = &[0x00, 0x1f, 0x20, 0x7e, 0x7f, 0xff];
        let line = format_hex_line(chunk, 0);
        let gutter = line.split('|').nth(1).unwrap();
        assert_eq!(gutter, ".. ~..");
    }

    #[test]
    fn format_hex_lines_matches_format_hex_line_row_by_row() {
        let bytes = b"Hello, world! This spans two rows of hex.";
        let lines = format_hex_lines(bytes);
        assert_eq!(lines.len(), bytes.len().div_ceil(HEX_BYTES_PER_LINE));
        for (row, chunk) in bytes.chunks(HEX_BYTES_PER_LINE).enumerate() {
            assert_eq!(lines[row], format_hex_line(chunk, row * HEX_BYTES_PER_LINE));
        }
    }

    // --- `Matcher`: regex-first with a literal fallback ------------------

    #[test]
    fn matcher_build_prefers_a_valid_regex() {
        // "foo.bar" is valid regex syntax (`.` means "any char"), so it
        // must be compiled as a regex, not treated as the literal string
        // "foo.bar" (which wouldn't match "fooXbar").
        let matcher = Matcher::build("foo.bar");
        assert!(matches!(matcher, Matcher::Regex(_)));
        assert!(matcher.is_match("fooXbar"));
    }

    #[test]
    fn matcher_build_falls_back_to_literal_on_invalid_regex() {
        // An unclosed group is invalid regex syntax; it must still be
        // usable as a plain (case-insensitive) substring search.
        let matcher = Matcher::build("foo(bar");
        assert!(matches!(matcher, Matcher::Literal(_)));
        assert!(matcher.is_match("xx foo(bar yy"));
        assert!(!matcher.is_match("no match here"));
    }

    #[test]
    fn matcher_is_match_is_case_insensitive_for_both_kinds() {
        assert!(Matcher::build("hello").is_match("HELLO world"));
        assert!(Matcher::build("HELLO").is_match("hello world"));
        assert!(Matcher::build("h.llo").is_match("HELLO world")); // regex path
    }

    #[test]
    fn matcher_find_ranges_locates_every_match_by_byte_offset() {
        let matcher = Matcher::build("ab");
        let ranges = matcher.find_ranges("ab cd AB ef");
        assert_eq!(ranges, vec![(0, 2), (6, 8)]);
    }

    #[test]
    fn matcher_find_ranges_on_japanese_text() {
        // "語" is a 3-byte UTF-8 character; the returned range must cover
        // exactly those 3 bytes, not a truncated/miscounted slice.
        let matcher = Matcher::build("本語");
        let line = "日本語のテキスト";
        let ranges = matcher.find_ranges(line);
        assert_eq!(ranges.len(), 1);
        let (start, end) = ranges[0];
        assert_eq!(&line[start..end], "本語");
    }

    #[test]
    fn matcher_find_ranges_empty_needle_matches_nothing() {
        // Never actually reached through `App` (an empty `Enter` cancels
        // instead of searching), but must not behave as "match everywhere"
        // if it ever were — an empty regex, notably, IS valid syntax and
        // would otherwise zero-width-match every position.
        let matcher = Matcher::build("");
        assert!(matches!(matcher, Matcher::Literal(_)));
        assert!(!matcher.is_match("anything"));
        assert!(matcher.find_ranges("anything").is_empty());
    }

    #[test]
    fn matcher_hex_dump_search_matches_the_formatted_line() {
        let bytes = b"Hi!";
        let lines = format_hex_lines(bytes);
        let matcher = Matcher::build("48 69"); // "Hi" in hex
        assert!(matcher.is_match(&lines[0]));
        let matcher = Matcher::build("nope");
        assert!(!matcher.is_match(&lines[0]));
    }
}
