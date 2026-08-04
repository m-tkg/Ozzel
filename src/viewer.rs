//! Loads a file for the built-in text viewer (`Mode::Viewer`): capped at a
//! maximum size, lossily decoded as UTF-8, tabs expanded for width math,
//! and binary files rejected outright rather than shown as garbage.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

/// Files larger than this are read only up to the cap; the viewer shows a
/// "truncated" note in its footer in that case rather than silently
/// pretending the file ends there.
const SIZE_CAP: u64 = 10 * 1024 * 1024; // 10 MiB
/// How many leading bytes are sniffed for a NUL byte to decide "this looks
/// like a binary file" before even trying to decode it as text.
const BINARY_SNIFF_LEN: usize = 8 * 1024; // 8 KiB
const TAB_WIDTH: usize = 4;

#[derive(Debug)]
pub struct LoadedFile {
    pub lines: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum LoadError {
    /// A NUL byte turned up in the first `BINARY_SNIFF_LEN` bytes — treated
    /// as "not text", full stop, rather than attempting a lossy decode
    /// that would just show garbage.
    Binary,
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Binary => write!(f, "binary file — viewer supports text only"),
            LoadError::Io(msg) => write!(f, "{msg}"),
        }
    }
}

/// Reads `path` up to `SIZE_CAP` bytes, rejects it as binary if a NUL byte
/// turns up early, and otherwise decodes it (lossily, if necessary) into
/// tab-expanded lines ready to render.
pub fn load(path: &Path) -> Result<LoadedFile, LoadError> {
    let (bytes, truncated) = read_capped(path)?;
    if looks_binary(&bytes) {
        return Err(LoadError::Binary);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines = text.lines().map(expand_tabs).collect();
    Ok(LoadedFile { lines, truncated })
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
    }

    #[test]
    fn loads_japanese_text_lossless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jp.txt");
        std::fs::write(&path, "日本語のテキスト\n二行目です").unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.lines, vec!["日本語のテキスト", "二行目です"]);
    }

    #[test]
    fn non_utf8_bytes_are_decoded_lossily_not_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shiftjis.txt");
        // Shift-JIS bytes for "あい" — not valid UTF-8, but also has no NUL
        // byte, so it should decode lossily rather than being flagged
        // binary.
        std::fs::write(&path, [0x82, 0xa0, 0x82, 0xa2]).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.lines.len(), 1);
        assert!(
            loaded.lines[0].contains('\u{FFFD}'),
            "invalid bytes become U+FFFD"
        );
    }

    #[test]
    fn binary_file_with_nul_byte_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.dat");
        std::fs::write(&path, [b'a', b'b', 0u8, b'c']).unwrap();

        match load(&path) {
            Err(LoadError::Binary) => {}
            other => panic!("expected LoadError::Binary, got {other:?}"),
        }
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

        assert!(load(&path).is_ok());
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
    }

    #[test]
    fn expand_tabs_pads_to_next_tab_stop() {
        assert_eq!(expand_tabs("a\tb"), "a   b"); // 'a' at col 0, tab -> col 4
        assert_eq!(expand_tabs("ab\tc"), "ab  c"); // 'ab' at col 0-1, tab -> col 4
        assert_eq!(expand_tabs("abcd\te"), "abcd    e"); // exactly at a tab stop -> full 4 spaces
        assert_eq!(expand_tabs("no tabs here"), "no tabs here");
    }
}
