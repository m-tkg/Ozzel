//! Loads a file for the built-in viewer (`Mode::Viewer`): capped at a
//! maximum size, decoded lossily as UTF-8 for the text view, and always
//! available as a hex dump too (see `format_hex_line`). Files that sniff as
//! binary simply open in hex mode initially rather than being rejected —
//! the user can Tab over to the (possibly garbled) text view if they want.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::mode::ViewMode;

/// Files larger than this are read only up to the cap; the viewer shows a
/// "truncated" note in its footer in that case rather than silently
/// pretending the file ends there.
const SIZE_CAP: u64 = 10 * 1024 * 1024; // 10 MiB
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
    let initial_mode = if looks_binary(&bytes) {
        ViewMode::Hex
    } else {
        ViewMode::Text
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().map(expand_tabs).collect();
    Ok(LoadedFile {
        lines,
        bytes,
        initial_mode,
        truncated,
    })
}

fn read_capped(path: &Path) -> Result<(Vec<u8>, bool), LoadError> {
    let metadata = fs::metadata(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let file_len = metadata.len();
    let to_read = file_len.min(SIZE_CAP) as usize;

    let mut file = File::open(path).map_err(|e| LoadError::Io(e.to_string()))?;
    let mut buf = vec![0u8; to_read];
    file.read_exact(&mut buf)
        .map_err(|e| LoadError::Io(e.to_string()))?;

    Ok((buf, file_len > SIZE_CAP))
}

fn looks_binary(bytes: &[u8]) -> bool {
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
}
