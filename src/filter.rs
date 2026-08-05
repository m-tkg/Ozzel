//! `Pane`'s incremental filter: case-insensitive substring by default, or a
//! (case-sensitive) regex when the input starts with `re:`.

use regex::Regex;

#[derive(Debug, Clone)]
pub struct FilterSpec {
    /// The exact text the user typed, kept around for the pane header's
    /// `[flt: ...]` tag and for round-tripping through `LineEditor`.
    pub raw: String,
    kind: FilterKind,
}

#[derive(Debug, Clone)]
enum FilterKind {
    /// Lowercased needle; matched against the lowercased candidate.
    Substring(String),
    /// Compiled as-typed (case-sensitive).
    Regex(Regex),
    /// The `re:` pattern failed to compile. Matches nothing, and carries
    /// the compiler's error message for display in the filter line.
    Invalid(String),
}

impl FilterSpec {
    /// Parses live filter input. An empty string means "no filter" (so
    /// backspacing to nothing clears it), represented as `None` rather than
    /// a variant of this type.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        let kind = match raw.strip_prefix("re:") {
            Some(pattern) => match Regex::new(pattern) {
                Ok(re) => FilterKind::Regex(re),
                Err(err) => FilterKind::Invalid(err.to_string()),
            },
            None => FilterKind::Substring(raw.to_lowercase()),
        };
        Some(Self {
            raw: raw.to_string(),
            kind,
        })
    }

    /// `name_lower` must be `name.to_lowercase()` — callers with a
    /// precomputed lowercase key (`FsEntry::name_lower`) pass it straight
    /// through so substring matching never re-allocates it per call; the
    /// regex path always matches against the original-case `name` instead
    /// (`FilterKind::Regex` is deliberately case-sensitive, see `parse`'s
    /// doc comment).
    pub fn matches(&self, name: &str, name_lower: &str) -> bool {
        match &self.kind {
            FilterKind::Substring(needle) => name_lower.contains(needle.as_str()),
            FilterKind::Regex(re) => re.is_match(name),
            FilterKind::Invalid(_) => false,
        }
    }

    /// `Some(message)` when this is an invalid `re:` pattern — surfaced in
    /// the filter input line instead of crashing or silently matching
    /// nothing without explanation.
    pub fn error(&self) -> Option<&str> {
        match &self.kind {
            FilterKind::Invalid(msg) => Some(msg),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls `matches` with `name`'s lowercase computed on the fly, since
    /// the tests below aren't exercising `FsEntry::name_lower` itself.
    fn m(spec: &FilterSpec, name: &str) -> bool {
        spec.matches(name, &name.to_lowercase())
    }

    #[test]
    fn empty_input_means_no_filter() {
        assert!(FilterSpec::parse("").is_none());
    }

    #[test]
    fn substring_is_case_insensitive() {
        let spec = FilterSpec::parse("Report").unwrap();
        assert!(m(&spec, "annual_report.txt"));
        assert!(m(&spec, "REPORT.CSV"));
        assert!(!m(&spec, "summary.txt"));
    }

    #[test]
    fn substring_matches_japanese_text() {
        let spec = FilterSpec::parse("日本語").unwrap();
        assert!(m(&spec, "日本語ファイル名.txt"));
        assert!(!m(&spec, "english.txt"));
    }

    #[test]
    fn re_prefix_compiles_a_case_sensitive_regex() {
        let spec = FilterSpec::parse("re:^IMG_[0-9]+\\.jpg$").unwrap();
        assert!(m(&spec, "IMG_1234.jpg"));
        assert!(
            !m(&spec, "img_1234.jpg"),
            "regex mode is case-sensitive as written"
        );
        assert!(!m(&spec, "IMG_1234.jpeg"));
    }

    #[test]
    fn invalid_regex_matches_nothing_but_reports_an_error() {
        let spec = FilterSpec::parse("re:(unclosed").unwrap();
        assert!(!m(&spec, "anything"));
        assert!(!m(&spec, ""));
        assert!(spec.error().is_some());
    }

    #[test]
    fn valid_filters_have_no_error() {
        assert!(FilterSpec::parse("foo").unwrap().error().is_none());
        assert!(FilterSpec::parse("re:^a").unwrap().error().is_none());
    }

    #[test]
    fn raw_preserves_original_typed_text() {
        let spec = FilterSpec::parse("re:Foo").unwrap();
        assert_eq!(spec.raw, "re:Foo");
    }
}
