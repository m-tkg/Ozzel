//! Pure filtering logic for the function-list command palette (`F`/`S-f`,
//! `Mode::FunctionList`): narrows `Action::ALL` down to whatever matches
//! the user's typed query, case-insensitively, against either the
//! action's `[keys]`/`[bindings]`
//! config name or its help-screen description. Shared by
//! `App::handle_function_list_key` (for cursor-bounds clamping) and
//! `ui::function_list_view` (for rendering), so both always agree on
//! exactly which actions are listed and in what order.

use std::sync::OnceLock;

use crate::action::Action;

/// `Action::ALL`'s config names/descriptions, lowercased once and cached —
/// both are `&'static str`s baked into the binary, so there's nothing to
/// invalidate. Without this, `filter_actions` re-lowercased all ~44
/// actions' worth of strings on every single call, including calls that
/// only re-render the palette (a cursor Up/Down, a blinking cursor) rather
/// than actually changing the query.
fn lowercase_haystacks() -> &'static [(String, String)] {
    static TABLE: OnceLock<Vec<(String, String)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        Action::ALL
            .iter()
            .map(|a| {
                (
                    a.config_name().to_lowercase(),
                    a.description().to_lowercase(),
                )
            })
            .collect()
    })
}

/// Every action whose config name or description contains `query`
/// (case-insensitive substring match); an empty query matches everything,
/// in `Action::ALL`'s fixed order.
pub fn filter_actions(query: &str) -> Vec<Action> {
    let needle = query.to_lowercase();
    Action::ALL
        .into_iter()
        .zip(lowercase_haystacks())
        .filter(|(_, (name_lower, description_lower))| {
            needle.is_empty() || name_lower.contains(&needle) || description_lower.contains(&needle)
        })
        .map(|(action, _)| action)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_every_action_in_order() {
        assert_eq!(filter_actions(""), Action::ALL.to_vec());
    }

    #[test]
    fn matches_config_name_case_insensitively() {
        let results = filter_actions("REN");
        assert!(results.contains(&Action::Rename));
    }

    #[test]
    fn matches_description_case_insensitively() {
        // OpenEditor's description ("Open the cursor file in an editor")
        // contains "editor", but its config name ("open_editor") is the
        // only thing that would match if only config_name were checked —
        // this specifically exercises the description side of the match
        // by searching for a word that's only in the description.
        let results = filter_actions("CURSOR ENTRY");
        assert!(results.contains(&Action::Rename)); // "Rename the cursor entry"
    }

    #[test]
    fn narrows_to_nothing_when_no_match() {
        assert!(filter_actions("zzzznotarealaction").is_empty());
    }
}
