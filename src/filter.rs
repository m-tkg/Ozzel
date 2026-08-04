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

    pub fn matches(&self, name: &str) -> bool {
        match &self.kind {
            FilterKind::Substring(needle) => name.to_lowercase().contains(needle.as_str()),
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

    #[test]
    fn empty_input_means_no_filter() {
        assert!(FilterSpec::parse("").is_none());
    }

    #[test]
    fn substring_is_case_insensitive() {
        let spec = FilterSpec::parse("Report").unwrap();
        assert!(spec.matches("annual_report.txt"));
        assert!(spec.matches("REPORT.CSV"));
        assert!(!spec.matches("summary.txt"));
    }

    #[test]
    fn substring_matches_japanese_text() {
        let spec = FilterSpec::parse("日本語").unwrap();
        assert!(spec.matches("日本語ファイル名.txt"));
        assert!(!spec.matches("english.txt"));
    }

    #[test]
    fn re_prefix_compiles_a_case_sensitive_regex() {
        let spec = FilterSpec::parse("re:^IMG_[0-9]+\\.jpg$").unwrap();
        assert!(spec.matches("IMG_1234.jpg"));
        assert!(
            !spec.matches("img_1234.jpg"),
            "regex mode is case-sensitive as written"
        );
        assert!(!spec.matches("IMG_1234.jpeg"));
    }

    #[test]
    fn invalid_regex_matches_nothing_but_reports_an_error() {
        let spec = FilterSpec::parse("re:(unclosed").unwrap();
        assert!(!spec.matches("anything"));
        assert!(!spec.matches(""));
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
