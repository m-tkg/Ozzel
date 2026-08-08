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

/// The keys of every other mode, which the `Keymap` has no entries for and
/// so have no other way to show up here. All fixed, except that the menus
/// and dialogs additionally accept whatever `cursor_up`/`cursor_down` are
/// bound to (see `Keymap::menu_nav`), and the paged bookmark menu whatever
/// `focus_left`/`focus_right` are (see `Keymap::menu_page`) — noted per
/// line, and in the header `build_lines` puts above them.
const FIXED_KEY_LINES: &[&str] = &[
    "Prompt (rename/mkdir/zip name/touch time/:command): Enter confirm, Esc cancel, Backspace/Delete/Left/Right/Home/End edit",
    "Confirm dialogs: y/Y proceed, n/N/Esc cancel, any other key is ignored",
    "Chmod dialog: arrows move over the rwx grid, Space toggles, 0-7 set the highlighted row, Enter applies, Esc cancels",
    "Sync dialog (W): Up/Down (or your cursor_up/cursor_down keys) choose update copy vs mirror, Enter confirms (mirror always re-confirms its deletions), Esc cancels",
    "Overwrite dialog: Up/Down (or your cursor_up/cursor_down keys) choose, Enter answers for this file, Esc cancels the whole transfer",
    "File info: Esc/Enter/q close",
    "Select menu (history/bookmarks): Up/Down (or your cursor_up/cursor_down keys) move, Enter select, Esc cancel; bookmarks only: 9 per page with 1-9 jumping straight to a row, Left/Right (or your focus_left/focus_right keys) turn the page, d delete, Shift+Up/Shift+Down reorder (saved immediately)",
    "Command palette (F): type to filter, Up/Down move (letter keys type instead, so only modifier combos bound to cursor_up/cursor_down move here), Enter run, Esc cancel",
    "Viewer: Up/Down/j/k, Space/f/PageDown, b/PageUp, d/u (half page), g/Home top, G/End bottom, Left/Right scroll horizontally (text mode), Tab toggle text/hex, /,? search, n/N next/prev match, Esc clears a search then closes, q closes",
    "This help screen: same less-style scrolling and /,?,n/N search as the viewer, q/Esc/h close (Esc clears a search first)",
    "Log viewer (L/S-l): same less-style scrolling and /,?,n/N search as the viewer, q/Esc close (Esc clears a search first)",
    "Process manager (P/S-p, Unix only): Up/Down (or your cursor_up/cursor_down keys) move, PageUp/PageDown, g/Home top, G/End bottom, r refresh now, x SIGTERM, X SIGKILL (both confirm), q/Esc close",
    "Process manager sort keys: p pid, u user, c %cpu, m %mem, s rss, t elapsed, n command — pressing the active one again reverses it. These letters win over any cursor_up/cursor_down you rebound onto them",
    "Process manager mouse (when mouse = true): click a row to put the cursor on it, wheel to scroll (inverted, like the panes': wheel up moves down the list). No double-click action, deliberately — nothing here should be one stray click away from a signal",
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
        "Modal keys (fixed, though the menus below also take your cursor_up/cursor_down keys)"
            .to_string(),
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

    /// The modal key lines are hand-written, so they can silently drift
    /// from the handlers. Pin the two the reorder feature depends on being
    /// documented — nothing else announces Shift+Up/Shift+Down.
    #[test]
    fn fixed_key_lines_document_the_bookmark_reorder_keys() {
        let text = build_display_lines(&Keymap::defaults()).join("\n");
        assert!(text.contains("Shift+Up/Shift+Down reorder"), "{text}");
        assert!(text.contains("cursor_up/cursor_down"), "{text}");
    }

    /// Same deal for the process manager, whose keys exist nowhere in the
    /// keymap — including the warning that its sort letters shadow a
    /// rebound cursor_up/cursor_down.
    #[test]
    fn fixed_key_lines_document_the_process_manager_keys() {
        let text = build_display_lines(&Keymap::defaults()).join("\n");
        assert!(text.contains("x SIGTERM, X SIGKILL"), "{text}");
        assert!(text.contains("p pid, u user, c %cpu"), "{text}");
        assert!(
            text.contains("click a row to put the cursor on it"),
            "{text}"
        );
        assert!(
            text.contains("win over any cursor_up/cursor_down"),
            "{text}"
        );
    }

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
    fn ends_with_the_modal_key_section() {
        let keymap = Keymap::defaults();
        let lines = build_lines(&keymap);
        assert!(matches!(lines.last(), Some(HelpLine::Text(_))));
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, HelpLine::Header(h) if h.contains("Modal keys")))
        );
    }
}
