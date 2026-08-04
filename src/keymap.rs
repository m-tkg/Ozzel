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
    /// the cursor, Tab switches panes, Enter/Backspace navigate, Space
    /// marks, `a` marks all, `s`/`.` cycle sort/hidden, `C`/`M`/`D`/`R`/`K`
    /// are the copy/move/delete/rename/mkdir commands, `w` swaps panes,
    /// `f`/`/` start an incremental filter (Esc clears one that's active),
    /// `p` zips the marked-or-cursor selection, `u` unzips the file under
    /// the cursor, and `q`/Ctrl+C quit.
    pub fn default_dyna() -> Self {
        use Action::*;
        let pairs: &[(&str, Action)] = &[
            ("up", CursorUp),
            ("down", CursorDown),
            ("pageup", PageUp),
            ("pagedown", PageDown),
            ("home", Top),
            ("end", Bottom),
            ("tab", SwitchPane),
            ("enter", Enter),
            ("backspace", Parent),
            ("s", CycleSort),
            (".", ToggleHidden),
            ("w", SwapPanes),
            ("C-r", Refresh),
            ("space", Mark),
            ("a", MarkAll),
            ("R", Rename),
            ("K", Mkdir),
            ("D", Delete),
            ("C", Copy),
            ("M", Move),
            ("f", Filter),
            ("/", Filter),
            ("esc", ClearFilter),
            ("p", ZipMarked),
            ("u", Unzip),
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

    pub fn resolve(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.bindings.get(&KeyCombo::new(code, modifiers)).copied()
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
}
