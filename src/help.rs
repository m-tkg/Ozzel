//! Builds the keybinding help screen's (`Mode::Help`) line listing from the
//! *current* effective keymap — after `[keys]`/`[bindings]` config merges —
//! so a user's override shows up correctly, rather than the compiled-in
//! defaults. Shared by `App::handle_help_key` (for scroll clamping) and
//! `ui::help_view` (for rendering), so both always agree on the exact line
//! count.

use crate::action::ActionCategory;
use crate::keymap::Keymap;

/// One renderable row of the help screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    /// A category (or section) header.
    Header(String),
    /// One action's row: its comma-joined keys, its `[keys]`/`[bindings]`
    /// config name, and its one-line description.
    Binding {
        keys: String,
        action: &'static str,
        description: &'static str,
    },
    /// A line of free-form static text (the fixed-key section, which isn't
    /// part of the keymap at all).
    Text(String),
    Blank,
}

impl HelpLine {
    /// This row's plain display text — no styling (that's `ui::help_view`'s
    /// job, wrapping this in a `Line` and bolding `Header`s), and no
    /// leading indent (also `ui::help_view`'s job) — just the exact
    /// characters a `/`/`?` search matches against, so the search haystack
    /// (`build_display_lines`) and the rendered screen can never drift
    /// apart from each other.
    pub fn text(&self) -> String {
        match self {
            HelpLine::Header(title) => title.clone(),
            HelpLine::Binding {
                keys,
                action,
                description,
            } => format!("{keys:<16} {action:<16} {description}"),
            HelpLine::Text(text) => text.clone(),
            HelpLine::Blank => String::new(),
        }
    }
}

/// `build_lines`, flattened to each row's plain display text — the search
/// haystack for `Mode::Help`'s `/`/`?` (see `crate::search`), built fresh
/// per search rather than cached, the same "cheap enough, never goes
/// stale" reasoning `App::handle_help_key`'s doc comment already gives for
/// `build_lines` itself.
pub fn build_display_lines(keymap: &Keymap) -> Vec<String> {
    build_lines(keymap).iter().map(HelpLine::text).collect()
}

/// The fixed (non-remappable) keys of every other mode, since none of them
/// consult the `Keymap` and so have no other way to show up here.
const FIXED_KEY_LINES: &[&str] = &[
    "Prompt (rename/mkdir/zip name/:command): Enter confirm, Esc cancel, Backspace/Delete/Left/Right/Home/End edit",
    "Confirm dialogs: y/Y proceed, any other key (including Esc) cancels",
    "Select menu (history/bookmarks): Up/Down move, Enter select, Esc cancel, d delete (bookmarks only)",
    "Viewer: Up/Down/j/k, Space/f/PageDown, b/PageUp, d/u (half page), g/Home top, G/End bottom, Left/Right scroll horizontally (text mode), Tab toggle text/hex, /,? search, n/N next/prev match, Esc clears a search then closes, q closes",
    "This help screen: same less-style scrolling and /,?,n/N search as the viewer, q/Esc/h close (Esc clears a search first)",
    "Log viewer (L/S-l): same less-style scrolling and /,?,n/N search as the viewer, q/Esc close (Esc clears a search first)",
];

/// Builds the full listing: every bound action grouped by category (skipping
/// actions with zero combos bound, e.g. after a `[keys]` "none" unbind),
/// then a static section of fixed keys that live outside the keymap
/// entirely. Driven by `Keymap::ordered_bindings` — the same source
/// `Keymap::to_bindings_toml` uses — so this screen's grouping/order can
/// never drift from the generated `[bindings]` TOML's.
pub fn build_lines(keymap: &Keymap) -> Vec<HelpLine> {
    let mut lines = Vec::new();
    let mut current_category: Option<ActionCategory> = None;

    for (action, keys) in keymap.ordered_bindings() {
        let category = action.category();
        if current_category != Some(category) {
            if current_category.is_some() {
                lines.push(HelpLine::Blank);
            }
            lines.push(HelpLine::Header(category.label().to_string()));
            current_category = Some(category);
        }
        lines.push(HelpLine::Binding {
            keys: keys.join(", "),
            action: action.config_name(),
            description: action.description(),
        });
    }
    if current_category.is_some() {
        lines.push(HelpLine::Blank);
    }

    lines.push(HelpLine::Header(
        "Fixed keys (not remappable via [keys]/[bindings])".to_string(),
    ));
    for text in FIXED_KEY_LINES {
        lines.push(HelpLine::Text((*text).to_string()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn includes_a_multi_bound_action_with_comma_joined_keys() {
        let keymap = Keymap::defaults();
        let lines = build_lines(&keymap);
        let rename_row = lines
            .iter()
            .find(|l| matches!(l, HelpLine::Binding { action, .. } if *action == "rename"));
        match rename_row {
            Some(HelpLine::Binding { keys, .. }) => {
                assert!(keys.contains('r'), "keys: {keys}");
                assert!(keys.contains('R'), "keys: {keys}");
            }
            other => panic!("expected a rename binding row, got {other:?}"),
        }
    }

    #[test]
    fn reflects_a_keys_override() {
        let mut keymap = Keymap::defaults();
        let mut overrides = HashMap::new();
        overrides.insert("z".to_string(), "quit".to_string());
        overrides.insert("q".to_string(), "none".to_string());
        keymap.merge_overrides(&overrides).unwrap();

        let lines = build_lines(&keymap);
        let quit_row = lines
            .iter()
            .find(|l| matches!(l, HelpLine::Binding { action, .. } if *action == "quit"));
        match quit_row {
            Some(HelpLine::Binding { keys, .. }) => {
                assert!(keys.contains('z'), "keys: {keys}");
                assert!(
                    !keys.contains('q'),
                    "q must no longer be bound; keys: {keys}"
                );
            }
            other => panic!("expected a quit binding row, got {other:?}"),
        }
    }

    #[test]
    fn reflects_a_bindings_override() {
        let mut keymap = Keymap::defaults();
        let mut bindings = HashMap::new();
        bindings.insert("open".to_string(), vec!["v".to_string()]);
        keymap.apply_bindings(&bindings).unwrap();

        let lines = build_lines(&keymap);
        let open_row = lines
            .iter()
            .find(|l| matches!(l, HelpLine::Binding { action, .. } if *action == "open"));
        match open_row {
            Some(HelpLine::Binding { keys, .. }) => assert!(keys.contains('v'), "keys: {keys}"),
            other => panic!("expected an open binding row, got {other:?}"),
        }
    }

    #[test]
    fn skips_an_action_with_zero_combos_bound() {
        let mut keymap = Keymap::defaults();
        // OpenDefault defaults to S-enter only; unbind that so it has none.
        let mut overrides = HashMap::new();
        overrides.insert("S-enter".to_string(), "none".to_string());
        keymap.merge_overrides(&overrides).unwrap();

        let lines = build_lines(&keymap);
        assert!(
            !lines.iter().any(
                |l| matches!(l, HelpLine::Binding { action, .. } if *action == "open_default")
            ),
            "an unbound action must not get a row"
        );
    }

    #[test]
    fn ends_with_the_fixed_key_section() {
        let keymap = Keymap::defaults();
        let lines = build_lines(&keymap);
        assert!(matches!(lines.last(), Some(HelpLine::Text(_))));
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, HelpLine::Header(h) if h.contains("Fixed keys")))
        );
    }
}
