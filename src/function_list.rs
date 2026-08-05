//! Pure filtering logic for the function-list command palette (`F`/`S-f`,
//! `Mode::FunctionList` — dyna's "From Function List"): narrows
//! `Action::ALL` down to whatever matches the user's typed query,
//! case-insensitively, against either the action's `[keys]`/`[bindings]`
//! config name or its help-screen description. Shared by
//! `App::handle_function_list_key` (for cursor-bounds clamping) and
//! `ui::function_list_view` (for rendering), so both always agree on
//! exactly which actions are listed and in what order.

use crate::action::Action;

/// Every action whose config name or description contains `query`
/// (case-insensitive substring match); an empty query matches everything,
/// in `Action::ALL`'s fixed order.
pub fn filter_actions(query: &str) -> Vec<Action> {
    let needle = query.to_lowercase();
    Action::ALL
        .into_iter()
        .filter(|action| {
            needle.is_empty()
                || action.config_name().to_lowercase().contains(&needle)
                || action.description().to_lowercase().contains(&needle)
        })
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
