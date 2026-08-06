//! Unified-diff generation for the `=` (diff) action — a thin wrapper
//! around the `similar` crate, kept as its own module so the output
//! format (headers, context radius) is unit-testable without touching the
//! viewer or the filesystem.

use similar::TextDiff;

/// Context lines shown around each hunk, matching `diff -u`'s default.
const CONTEXT_RADIUS: usize = 3;

/// Builds a unified diff of `left` -> `right`, with `---`/`+++` headers
/// naming the two sides. Identical inputs produce an empty string (the
/// caller treats that as "files are identical" and never opens the
/// viewer).
pub fn unified(left_name: &str, right_name: &str, left: &str, right: &str) -> String {
    let diff = TextDiff::from_lines(left, right);
    if diff.ratio() == 1.0 {
        return String::new();
    }
    diff.unified_diff()
        .context_radius(CONTEXT_RADIUS)
        .header(left_name, right_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_produce_an_empty_diff() {
        assert_eq!(unified("a", "b", "same\ntext\n", "same\ntext\n"), "");
    }

    #[test]
    fn changed_line_produces_headers_hunk_and_both_sides() {
        let out = unified(
            "left/f.txt",
            "right/f.txt",
            "one\ntwo\nthree\n",
            "one\nTWO\nthree\n",
        );
        assert!(
            out.starts_with("--- left/f.txt\n+++ right/f.txt\n"),
            "out: {out}"
        );
        assert!(out.contains("@@"), "out: {out}");
        assert!(out.contains("-two"), "out: {out}");
        assert!(out.contains("+TWO"), "out: {out}");
        // Context lines around the change come through unprefixed-ish
        // (leading space).
        assert!(out.contains(" one"), "out: {out}");
        assert!(out.contains(" three"), "out: {out}");
    }

    #[test]
    fn addition_and_removal_only() {
        let added = unified("a", "b", "x\n", "x\ny\n");
        assert!(added.contains("+y"), "out: {added}");
        let removed = unified("a", "b", "x\ny\n", "x\n");
        assert!(removed.contains("-y"), "out: {removed}");
    }

    #[test]
    fn distant_changes_produce_separate_hunks() {
        let left: String = (0..40).map(|i| format!("line{i}\n")).collect();
        let right = left
            .replace("line2\n", "LINE2\n")
            .replace("line35\n", "LINE35\n");
        let out = unified("a", "b", &left, &right);
        assert_eq!(
            out.matches("@@").count(),
            4, // two hunks, each with a leading and trailing @@ pair marker
            "two separated changes must not merge into one giant hunk: {out}"
        );
    }
}
