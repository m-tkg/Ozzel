//! Key combo parsing and the configurable keymap. Normal mode is the only
//! mode that consults this — Prompt/Confirm use fixed editing keys instead
//! (see `App::handle_prompt_key` / `App::handle_confirm_key`).

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use serde::de::IntoDeserializer;
use thiserror::Error;

use crate::action::Action;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeymapError {
    #[error("invalid key combo: \"{0}\"")]
    InvalidCombo(String),
    #[error("unknown action: \"{0}\"")]
    UnknownAction(String),
}

/// A parsed key + modifier pair, hashable so it can key a `HashMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyCombo {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Parses combo strings like `"up"`, `"space"`, `"tab"`, `"backspace"`,
    /// `"enter"`, `"esc"`, `"C-r"`, `"S-tab"`, or a single literal
    /// character such as `"a"` / `"R"` / `"."`.
    pub fn parse(s: &str) -> Result<Self, KeymapError> {
        let mut modifiers = KeyModifiers::NONE;
        let mut rest = s;
        loop {
            if let Some(stripped) = rest.strip_prefix("C-") {
                modifiers.insert(KeyModifiers::CONTROL);
                rest = stripped;
            } else if let Some(stripped) = rest.strip_prefix("S-") {
                modifiers.insert(KeyModifiers::SHIFT);
                rest = stripped;
            } else if let Some(stripped) = rest.strip_prefix("A-") {
                modifiers.insert(KeyModifiers::ALT);
                rest = stripped;
            } else {
                break;
            }
        }

        if rest.is_empty() {
            return Err(KeymapError::InvalidCombo(s.to_string()));
        }

        let code = match rest.to_ascii_lowercase().as_str() {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "delete" | "del" => KeyCode::Delete,
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return Err(KeymapError::InvalidCombo(s.to_string())),
                }
            }
        };

        // Crossterm's own key parser always sets SHIFT alongside an ASCII
        // uppercase `Char` (see its `normalize_case`), because a basic
        // terminal has no way to send "shift+r" distinctly from "R" — both
        // are just the byte 'R'. Mirror that here so `"R"` (and `"S-r"`)
        // both resolve to exactly what a real keypress reports; without
        // this, `"R"` would parse to `Char('R')` + no modifiers and could
        // never match a real `R` keypress.
        if let KeyCode::Char(c) = code
            && c.is_ascii_uppercase()
        {
            modifiers.insert(KeyModifiers::SHIFT);
        }

        Ok(KeyCombo { code, modifiers })
    }
}

/// Parses an action name (e.g. `"copy"`, `"cycle_sort"`) using `Action`'s
/// own `Deserialize` impl, so the set of valid names can never drift from
/// the enum itself.
fn parse_action(name: &str) -> Result<Action, KeymapError> {
    let deserializer: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
        name.into_deserializer();
    Action::deserialize(deserializer).map_err(|_| KeymapError::UnknownAction(name.to_string()))
}

pub struct Keymap {
    bindings: HashMap<KeyCombo, Action>,
}

impl Keymap {
    /// The dyna-filer-style defaults: arrows/PageUp/PageDown/Home/End move
    /// the cursor, `Left`/`Right` additionally jump focus straight to that
    /// pane, Tab switches panes, Enter/Backspace navigate, Space marks,
    /// `a` marks all, `s`/`.` cycle sort/hidden, `C`/`M`/`D`(or `d`)/`R`(or
    /// `r`)/`K` are the copy/move/delete/rename/mkdir commands (`r`/`d` are
    /// this keymap's showcase of binding more than one combo to the same
    /// action — `R`/`D` still work too, they're the same key with Shift
    /// implied), `w` swaps panes, `f`/`/` start an incremental filter (Esc
    /// clears one that's active), `p` zips the marked-or-cursor selection,
    /// `u` unzips the file under the cursor, `h`/`?` open the keybinding
    /// help screen, `S-h` (capital `H`) opens the history jump menu (moved
    /// off plain `h` to make room for help), `b` opens the bookmark jump
    /// menu, `B` adds a bookmark, `~` jumps to home (no longer also on
    /// `S-h`/`H`, which now means history), `:` runs a shell command (TUI
    /// suspended), `e` opens the cursor file in an editor (suspended, no
    /// pause), `x`/`o` open the cursor entry in the built-in text viewer,
    /// and `q`/Ctrl+C quit. `S-up`/`S-down` jump straight to the top/bottom
    /// of the pane (same as Home/End, kept bound too), and `S-enter` opens
    /// the cursor entry with the OS default-app handler -- most terminals
    /// can only report Shift+Enter distinctly from Enter when the kitty
    /// keyboard protocol is active (see `main.rs`'s `keyboard_enhancement`
    /// handling); without it, `S-enter` arrives as plain Enter and falls
    /// back to the built-in viewer. `OpenDefault` is otherwise a valid
    /// action but unbound on any other key by default — rebind it via
    /// `[keys]` to get the old behavior back.
    pub fn default_dyna() -> Self {
        use Action::*;
        let pairs: &[(&str, Action)] = &[
            ("up", CursorUp),
            ("down", CursorDown),
            ("left", FocusLeft),
            ("right", FocusRight),
            ("pageup", PageUp),
            ("pagedown", PageDown),
            ("home", Top),
            ("end", Bottom),
            ("S-up", Top),
            ("S-down", Bottom),
            ("tab", SwitchPane),
            ("enter", Enter),
            ("S-enter", OpenDefault),
            ("backspace", Parent),
            ("s", CycleSort),
            (".", ToggleHidden),
            ("w", SwapPanes),
            ("C-r", Refresh),
            ("space", Mark),
            ("a", MarkAll),
            ("r", Rename),
            ("R", Rename),
            ("K", Mkdir),
            ("d", Delete),
            ("D", Delete),
            ("C", Copy),
            ("M", Move),
            ("f", Filter),
            ("/", Filter),
            ("esc", ClearFilter),
            ("p", ZipMarked),
            ("u", Unzip),
            ("h", Help),
            ("?", Help),
            ("H", HistoryJump),
            ("b", BookmarkJump),
            ("B", BookmarkAdd),
            ("~", GoHome),
            (":", CommandLine),
            ("e", OpenEditor),
            ("x", View),
            ("o", View),
            ("q", Quit),
            ("C-c", Quit),
        ];

        let mut bindings = HashMap::new();
        for (combo, action) in pairs {
            let combo = KeyCombo::parse(combo).expect("built-in combo must parse");
            bindings.insert(combo, *action);
        }
        Self { bindings }
    }

    /// Applies a config `[keys]` table on top of the defaults: a normal
    /// value rebinds/adds a combo, and the literal string `"none"` unbinds
    /// it. Returns the first parse error encountered, if any.
    pub fn merge_overrides(
        &mut self,
        overrides: &HashMap<String, String>,
    ) -> Result<(), KeymapError> {
        for (combo_str, action_str) in overrides {
            let combo = KeyCombo::parse(combo_str)?;
            if action_str == "none" {
                self.bindings.remove(&combo);
            } else {
                let action = parse_action(action_str)?;
                self.bindings.insert(combo, action);
            }
        }
        Ok(())
    }

    /// Applies a config `[bindings]` table (action name -> array of key
    /// combos) on top of whatever `[keys]`/the defaults produced: every
    /// combo listed gets bound to that action, overriding whatever it
    /// previously held. There's no `"none"` sentinel here — unlike
    /// `[keys]`, `[bindings]`'s values are key combos, not action names, so
    /// unbinding a key stays `[keys]`'s job. `App::new` applies this
    /// strictly after `merge_overrides`, so `[bindings]` wins on any
    /// combo both sections happen to mention.
    pub fn apply_bindings(
        &mut self,
        bindings: &HashMap<String, Vec<String>>,
    ) -> Result<(), KeymapError> {
        for (action_str, combos) in bindings {
            let action = parse_action(action_str)?;
            for combo_str in combos {
                let combo = KeyCombo::parse(combo_str)?;
                self.bindings.insert(combo, action);
            }
        }
        Ok(())
    }

    pub fn resolve(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.bindings.get(&KeyCombo::new(code, modifiers)).copied()
    }

    /// Every key combo currently bound to `action`, formatted for display
    /// (see `format_combo`) and sorted for a deterministic order. Used only
    /// by the help screen (`crate::help`) — never round-tripped back
    /// through `KeyCombo::parse`.
    pub fn combos_for(&self, action: Action) -> Vec<String> {
        let mut combos: Vec<String> = self
            .bindings
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(combo, _)| format_combo(combo))
            .collect();
        combos.sort();
        combos
    }
}

/// Formats a combo back into (approximately) the same notation
/// `KeyCombo::parse` accepts — e.g. `Char('r')`+`NONE` -> `"r"`,
/// `Char('R')`+`SHIFT` -> `"R"` (no redundant `S-`, since Shift is implied
/// for any uppercase letter), `Enter`+`SHIFT` -> `"S-Enter"`. Display-only.
fn format_combo(combo: &KeyCombo) -> String {
    let implied_shift = matches!(combo.code, KeyCode::Char(c) if c.is_ascii_uppercase());
    let mut prefix = String::new();
    if combo.modifiers.contains(KeyModifiers::CONTROL) {
        prefix.push_str("C-");
    }
    if combo.modifiers.contains(KeyModifiers::ALT) {
        prefix.push_str("A-");
    }
    if combo.modifiers.contains(KeyModifiers::SHIFT) && !implied_shift {
        prefix.push_str("S-");
    }
    format!("{prefix}{}", format_key_code(combo.code))
}

fn format_key_code(code: KeyCode) -> String {
    match code {
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_keys() {
        assert_eq!(
            KeyCombo::parse("up").unwrap(),
            KeyCombo::new(KeyCode::Up, KeyModifiers::NONE)
        );
        assert_eq!(
            KeyCombo::parse("backspace").unwrap(),
            KeyCombo::new(KeyCode::Backspace, KeyModifiers::NONE)
        );
        assert_eq!(
            KeyCombo::parse("space").unwrap(),
            KeyCombo::new(KeyCode::Char(' '), KeyModifiers::NONE)
        );
    }

    #[test]
    fn parses_modifier_prefixes() {
        assert_eq!(
            KeyCombo::parse("C-r").unwrap(),
            KeyCombo::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            KeyCombo::parse("S-tab").unwrap(),
            KeyCombo::new(KeyCode::Tab, KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn parses_single_chars_preserving_case() {
        // Uppercase letters always carry SHIFT: a real terminal has no way
        // to send "shift+r" distinctly from "R", and crossterm's own
        // parser sets SHIFT for every uppercase `Char` it reports, so the
        // keymap must store the same bit or it can never match.
        assert_eq!(
            KeyCombo::parse("R").unwrap(),
            KeyCombo::new(KeyCode::Char('R'), KeyModifiers::SHIFT)
        );
        assert_eq!(
            KeyCombo::parse("r").unwrap(),
            KeyCombo::new(KeyCode::Char('r'), KeyModifiers::NONE)
        );
        assert_eq!(
            KeyCombo::parse(".").unwrap(),
            KeyCombo::new(KeyCode::Char('.'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn uppercase_combo_matches_a_real_terminal_keypress() {
        // This is the scenario the fix above exists for: a genuine
        // crossterm event for an uppercase letter always carries SHIFT.
        let combo_from_config = KeyCombo::parse("R").unwrap();
        let combo_from_real_keypress = KeyCombo::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(combo_from_config, combo_from_real_keypress);
    }

    #[test]
    fn rejects_empty_and_multi_char_garbage() {
        assert!(KeyCombo::parse("").is_err());
        assert!(KeyCombo::parse("xyz").is_err());
    }

    #[test]
    fn default_keymap_resolves_core_bindings() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(
            km.resolve(KeyCode::Char(' '), KeyModifiers::NONE),
            Some(Action::Mark)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('C'), KeyModifiers::SHIFT),
            Some(Action::Copy)
        );
        assert_eq!(km.resolve(KeyCode::Char('c'), KeyModifiers::NONE), None);
    }

    #[test]
    fn default_keymap_binds_w_to_swap_panes_and_u_to_unzip() {
        // `u` used to be SwapPanes; Phase 4 rebinds it to Unzip (dyna-filer's
        // unpack key) and moves SwapPanes to `w`. Pin both here so a future
        // change can't silently swap them back without a test failing.
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('w'), KeyModifiers::NONE),
            Some(Action::SwapPanes)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('u'), KeyModifiers::NONE),
            Some(Action::Unzip)
        );
    }

    #[test]
    fn default_keymap_binds_filter_and_zip_keys() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('f'), KeyModifiers::NONE),
            Some(Action::Filter)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('/'), KeyModifiers::NONE),
            Some(Action::Filter)
        );
        assert_eq!(
            km.resolve(KeyCode::Esc, KeyModifiers::NONE),
            Some(Action::ClearFilter)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('p'), KeyModifiers::NONE),
            Some(Action::ZipMarked)
        );
    }

    #[test]
    fn default_keymap_binds_history_bookmark_home_and_external_keys() {
        let km = Keymap::default_dyna();
        // `h` moved to Help (see the dedicated test below); history is now
        // on `S-h` (capital `H`) only.
        assert_eq!(
            km.resolve(KeyCode::Char('b'), KeyModifiers::NONE),
            Some(Action::BookmarkJump)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('B'), KeyModifiers::SHIFT),
            Some(Action::BookmarkAdd)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('~'), KeyModifiers::NONE),
            Some(Action::GoHome)
        );
        assert_eq!(
            km.resolve(KeyCode::Char(':'), KeyModifiers::NONE),
            Some(Action::CommandLine)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('e'), KeyModifiers::NONE),
            Some(Action::OpenEditor)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(Action::View)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('o'), KeyModifiers::NONE),
            Some(Action::View)
        );
    }

    #[test]
    fn default_keymap_binds_r_and_shift_r_both_to_rename() {
        // The showcase multi-binding pair: `r` and `R`/`S-r` (the same real
        // keypress) both resolve to Rename.
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('r'), KeyModifiers::NONE),
            Some(Action::Rename)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('R'), KeyModifiers::SHIFT),
            Some(Action::Rename)
        );
    }

    #[test]
    fn default_keymap_binds_d_and_shift_d_both_to_delete() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('d'), KeyModifiers::NONE),
            Some(Action::Delete)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('D'), KeyModifiers::SHIFT),
            Some(Action::Delete)
        );
    }

    #[test]
    fn default_keymap_binds_h_and_question_mark_to_help_and_shift_h_to_history() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('h'), KeyModifiers::NONE),
            Some(Action::Help)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('?'), KeyModifiers::NONE),
            Some(Action::Help)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('H'), KeyModifiers::SHIFT),
            Some(Action::HistoryJump)
        );
    }

    #[test]
    fn default_keymap_go_home_is_bound_only_to_tilde() {
        // `H`/`S-h` moved to HistoryJump; GoHome no longer has a second key.
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Char('~'), KeyModifiers::NONE),
            Some(Action::GoHome)
        );
        assert_ne!(
            km.resolve(KeyCode::Char('H'), KeyModifiers::SHIFT),
            Some(Action::GoHome)
        );
    }

    #[test]
    fn open_default_action_is_unbound_by_default_but_rebindable() {
        // OS-default-app opening is still a valid action — it just isn't
        // on any key out of the box, since `x`/`o` now open the built-in
        // viewer instead. A user can still rebind it via `[keys]`.
        let mut km = Keymap::default_dyna();
        assert_ne!(
            km.resolve(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(Action::OpenDefault)
        );
        assert_ne!(
            km.resolve(KeyCode::Char('o'), KeyModifiers::NONE),
            Some(Action::OpenDefault)
        );

        let mut overrides = HashMap::new();
        overrides.insert("g".to_string(), "open_default".to_string());
        km.merge_overrides(&overrides).unwrap();
        assert_eq!(
            km.resolve(KeyCode::Char('g'), KeyModifiers::NONE),
            Some(Action::OpenDefault)
        );
    }

    #[test]
    fn default_keymap_binds_left_right_arrows_to_pane_focus() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Left, KeyModifiers::NONE),
            Some(Action::FocusLeft)
        );
        assert_eq!(
            km.resolve(KeyCode::Right, KeyModifiers::NONE),
            Some(Action::FocusRight)
        );
    }

    #[test]
    fn default_keymap_binds_shift_up_down_to_top_bottom_and_keeps_home_end() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Up, KeyModifiers::SHIFT),
            Some(Action::Top)
        );
        assert_eq!(
            km.resolve(KeyCode::Down, KeyModifiers::SHIFT),
            Some(Action::Bottom)
        );
        assert_eq!(
            km.resolve(KeyCode::Home, KeyModifiers::NONE),
            Some(Action::Top)
        );
        assert_eq!(
            km.resolve(KeyCode::End, KeyModifiers::NONE),
            Some(Action::Bottom)
        );
    }

    #[test]
    fn default_keymap_binds_shift_enter_to_open_default_and_keeps_plain_enter() {
        let km = Keymap::default_dyna();
        assert_eq!(
            km.resolve(KeyCode::Enter, KeyModifiers::SHIFT),
            Some(Action::OpenDefault)
        );
        assert_eq!(
            km.resolve(KeyCode::Enter, KeyModifiers::NONE),
            Some(Action::Enter)
        );
    }

    #[test]
    fn merge_overrides_rebinds_and_unbinds() {
        let mut km = Keymap::default_dyna();
        let mut overrides = HashMap::new();
        overrides.insert("C-c".to_string(), "copy".to_string());
        overrides.insert("q".to_string(), "none".to_string());
        km.merge_overrides(&overrides).unwrap();

        assert_eq!(
            km.resolve(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Copy)
        );
        assert_eq!(km.resolve(KeyCode::Char('q'), KeyModifiers::NONE), None);
    }

    #[test]
    fn merge_overrides_rejects_unknown_action_name() {
        let mut km = Keymap::default_dyna();
        let mut overrides = HashMap::new();
        overrides.insert("x".to_string(), "not_a_real_action".to_string());
        assert!(km.merge_overrides(&overrides).is_err());
    }

    #[test]
    fn apply_bindings_binds_every_listed_combo_to_the_action() {
        let mut km = Keymap::default_dyna();
        let mut bindings = HashMap::new();
        bindings.insert("quit".to_string(), vec!["z".to_string(), "S-z".to_string()]);
        km.apply_bindings(&bindings).unwrap();

        assert_eq!(
            km.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
        assert_eq!(
            km.resolve(KeyCode::Char('z'), KeyModifiers::SHIFT),
            Some(Action::Quit)
        );
    }

    #[test]
    fn apply_bindings_overrides_whatever_the_combo_previously_held() {
        let mut km = Keymap::default_dyna();
        // "q" defaults to Quit; rebind it via [bindings] to Copy.
        let mut bindings = HashMap::new();
        bindings.insert("copy".to_string(), vec!["q".to_string()]);
        km.apply_bindings(&bindings).unwrap();

        assert_eq!(
            km.resolve(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Copy)
        );
    }

    #[test]
    fn apply_bindings_is_applied_after_keys_and_wins_on_overlap() {
        // Mirrors App::new's actual order: [keys] first, then [bindings].
        let mut km = Keymap::default_dyna();
        let mut keys = HashMap::new();
        keys.insert("z".to_string(), "quit".to_string());
        km.merge_overrides(&keys).unwrap();

        let mut bindings = HashMap::new();
        bindings.insert("copy".to_string(), vec!["z".to_string()]);
        km.apply_bindings(&bindings).unwrap();

        assert_eq!(
            km.resolve(KeyCode::Char('z'), KeyModifiers::NONE),
            Some(Action::Copy),
            "[bindings] must win over [keys] on the same combo"
        );
    }

    #[test]
    fn apply_bindings_rejects_unknown_action_name() {
        let mut km = Keymap::default_dyna();
        let mut bindings = HashMap::new();
        bindings.insert("not_a_real_action".to_string(), vec!["z".to_string()]);
        assert!(km.apply_bindings(&bindings).is_err());
    }

    #[test]
    fn apply_bindings_rejects_a_bad_combo() {
        let mut km = Keymap::default_dyna();
        let mut bindings = HashMap::new();
        bindings.insert("quit".to_string(), vec!["not-a-combo".to_string()]);
        assert!(km.apply_bindings(&bindings).is_err());
    }

    #[test]
    fn combos_for_lists_every_key_bound_to_an_action_sorted() {
        let km = Keymap::default_dyna();
        let combos = km.combos_for(Action::Rename);
        assert_eq!(combos, vec!["R".to_string(), "r".to_string()]);
    }

    #[test]
    fn combos_for_an_unbound_action_is_empty() {
        let mut km = Keymap::default_dyna();
        let mut overrides = HashMap::new();
        overrides.insert("g".to_string(), "open_default".to_string());
        km.merge_overrides(&overrides).unwrap();
        // OpenDefault is bound to S-enter by default; unbind that too so it
        // ends up with zero combos for this test.
        let mut unbind = HashMap::new();
        unbind.insert("S-enter".to_string(), "none".to_string());
        unbind.insert("g".to_string(), "none".to_string());
        km.merge_overrides(&unbind).unwrap();
        assert!(km.combos_for(Action::OpenDefault).is_empty());
    }
}
