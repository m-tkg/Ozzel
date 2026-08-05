//! Shared `less`-style incremental search machinery for every full-frame
//! scrollable text view: `Mode::Viewer`, `Mode::Help`, and `Mode::Log` all
//! use this — `/` (forward) / `?` (backward) open a search input, Enter
//! runs it against a caller-supplied haystack of plain display-line
//! strings via `viewer::Matcher` (regex-first, case-insensitive-literal
//! fallback — see `Matcher::build`), `n`/`N` step through the matches with
//! wraparound, and `Esc` is a two-step close everywhere: the first press
//! (while a search is `Active`) clears it back to `Idle` without leaving
//! the view, the second (or an `Esc` with no active search at all) closes
//! the view itself. Each view's own key handler (`App::handle_viewer_key`/
//! `handle_help_key`/`handle_log_view_key`) owns that two-step Esc/`q`
//! dispatch and its own scroll-key mapping (they differ enough — `Mode::Log`'s
//! `scroll_from_bottom` counts up in the *opposite* direction "up" moves in
//! — that a single shared dispatcher would need as much per-view branching
//! as three small ones do, for no real savings) but delegates every bit of
//! the actual search state machine to the functions below, so the matching/
//! stepping/wraparound logic exists exactly once.
//!
//! All state lives in `crate::mode::ViewerSearch` (kept under that name
//! even though it's no longer viewer-only, rather than churning a rename
//! across three views' worth of match arms for a purely cosmetic change).

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::mode::{LineEditor, SearchDirection, ViewerSearch};
use crate::viewer::Matcher;

/// `/` (`Forward`) / `?` (`Backward`): opens the search input, remembering
/// whatever search (if any) was already active so `Esc` on an empty input
/// restores it exactly (see `handle_input_key`'s `Cancelled` case).
pub fn begin(search: &mut ViewerSearch, direction: SearchDirection) {
    let previous = std::mem::replace(search, ViewerSearch::Idle);
    *search = ViewerSearch::Editing {
        input: LineEditor::new(),
        direction,
        previous: Box::new(previous),
    };
}

/// What a keypress did while `search` was `ViewerSearch::Editing` — the
/// caller only needs to act on `Confirmed` (build its haystack and call
/// `run`); `Editing`/`Cancelled` both mean `search` already reflects the
/// right state.
pub enum InputOutcome {
    /// A plain edit keystroke (or a key this doesn't handle) — still
    /// typing.
    Editing,
    /// `Esc`, or `Enter` on an empty pattern: `search` has been restored to
    /// whatever it was before `begin` was called.
    Cancelled,
    /// `Enter` on a non-empty pattern: `search` is `Idle` for the moment
    /// (rebuilt into `Active` by `run`, which the caller must call next
    /// with a haystack built from whatever's currently on screen).
    Confirmed(String, SearchDirection),
}

/// Fixed editing keys for `ViewerSearch::Editing`'s input line — same set
/// `Filter`/`JumpSearch`/`Prompt` use (Ctrl+char never inserts, so e.g.
/// Ctrl+C still reaches whatever it means at the terminal level). No-op
/// (`InputOutcome::Editing`) if `search` isn't actually `Editing`.
pub fn handle_input_key(
    search: &mut ViewerSearch,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> InputOutcome {
    match code {
        KeyCode::Esc => {
            let ViewerSearch::Editing { previous, .. } =
                std::mem::replace(search, ViewerSearch::Idle)
            else {
                return InputOutcome::Editing;
            };
            *search = *previous;
            InputOutcome::Cancelled
        }
        KeyCode::Enter => {
            let ViewerSearch::Editing {
                input, direction, ..
            } = search
            else {
                return InputOutcome::Editing;
            };
            let pattern = input.value();
            if pattern.is_empty() {
                let ViewerSearch::Editing { previous, .. } =
                    std::mem::replace(search, ViewerSearch::Idle)
                else {
                    unreachable!("just matched Editing above");
                };
                *search = *previous;
                return InputOutcome::Cancelled;
            }
            let direction = *direction;
            InputOutcome::Confirmed(pattern, direction)
        }
        _ => {
            let ViewerSearch::Editing { input, .. } = search else {
                return InputOutcome::Editing;
            };
            match code {
                KeyCode::Backspace => input.backspace(),
                KeyCode::Delete => input.delete(),
                KeyCode::Left => input.move_left(),
                KeyCode::Right => input.move_right(),
                KeyCode::Home => input.move_home(),
                KeyCode::End => input.move_end(),
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => input.insert(c),
                _ => {}
            }
            InputOutcome::Editing
        }
    }
}

/// Runs a confirmed `/`/`?` search (see `InputOutcome::Confirmed`): scans
/// `haystack` (built by the caller — e.g. `viewer_search_haystack`,
/// `help::build_display_lines`, or a log view's wrapped rows) for every
/// case-insensitive match, and jumps to the nearest one to `from_index` in
/// `direction`, wrapping around if nothing qualifies before the end (see
/// `nearest_match`). On success, sets `*search` to `Active` and returns the
/// matched haystack index to scroll to. A pattern matching nothing leaves
/// `*search` at `Idle` and returns `Err(pattern)` instead — deliberately
/// not logged here (this module has no `App` to log through); the caller
/// logs `"pattern not found: {pattern}"` itself.
pub fn run(
    search: &mut ViewerSearch,
    haystack: &[String],
    pattern: String,
    direction: SearchDirection,
    from_index: usize,
) -> Result<usize, String> {
    let matcher = Matcher::build(&pattern);
    let matches: Vec<usize> = haystack
        .iter()
        .enumerate()
        .filter(|(_, line)| matcher.is_match(line))
        .map(|(i, _)| i)
        .collect();

    if matches.is_empty() {
        *search = ViewerSearch::Idle;
        return Err(pattern);
    }

    let (current, wrapped) = nearest_match(&matches, from_index, direction);
    let target = matches[current];
    *search = ViewerSearch::Active {
        pattern,
        direction,
        matches,
        current,
        wrapped,
    };
    Ok(target)
}

/// `n` (`reverse: false`) / `N` (`reverse: true`): advances to the
/// next/previous match in the active search, wrapping around (recorded in
/// `wrapped`, for the footer's "search wrapped" notice). Returns the new
/// target index to scroll to, or `None` when there's no active search
/// (`n`/`N` are then a no-op).
pub fn step(search: &mut ViewerSearch, reverse: bool) -> Option<usize> {
    let ViewerSearch::Active {
        direction,
        matches,
        current,
        wrapped,
        ..
    } = search
    else {
        return None;
    };
    let effective_direction = if reverse {
        direction.reversed()
    } else {
        *direction
    };
    let len = matches.len();
    let (next, did_wrap) = match effective_direction {
        SearchDirection::Forward => {
            if *current + 1 < len {
                (*current + 1, false)
            } else {
                (0, true)
            }
        }
        SearchDirection::Backward => {
            if *current > 0 {
                (*current - 1, false)
            } else {
                (len - 1, true)
            }
        }
    };
    *current = next;
    *wrapped = did_wrap;
    Some(matches[*current])
}

/// The index into `matches` (sorted ascending) that a fresh `/`/`?` search
/// should land on: the nearest match at-or-after `from` for a forward
/// search, at-or-before `from` for a backward one — wrapping around to the
/// other end (and reporting that it did, for the footer's "search wrapped"
/// notice) when nothing qualifies in that direction. `matches` must be
/// non-empty — `run` never calls this otherwise. (Moved here from `app.rs`,
/// unchanged, when the viewer's search machinery became shared with
/// help/log.)
pub fn nearest_match(matches: &[usize], from: usize, direction: SearchDirection) -> (usize, bool) {
    match direction {
        SearchDirection::Forward => match matches.iter().position(|&m| m >= from) {
            Some(i) => (i, false),
            None => (0, true),
        },
        SearchDirection::Backward => match matches.iter().rposition(|&m| m <= from) {
            Some(i) => (i, false),
            None => (matches.len() - 1, true),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn begin_opens_editing_and_remembers_the_previous_search() {
        let mut search = ViewerSearch::Idle;
        begin(&mut search, SearchDirection::Forward);
        assert!(matches!(
            search,
            ViewerSearch::Editing {
                direction: SearchDirection::Forward,
                ..
            }
        ));
    }

    #[test]
    fn handle_input_key_esc_restores_the_previous_search() {
        let mut search = ViewerSearch::Active {
            pattern: "old".to_string(),
            direction: SearchDirection::Forward,
            matches: vec![0],
            current: 0,
            wrapped: false,
        };
        begin(&mut search, SearchDirection::Backward);
        let outcome = handle_input_key(&mut search, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(outcome, InputOutcome::Cancelled));
        assert!(matches!(
            search,
            ViewerSearch::Active { pattern, .. } if pattern == "old"
        ));
    }

    #[test]
    fn handle_input_key_enter_on_empty_pattern_cancels() {
        let mut search = ViewerSearch::Idle;
        begin(&mut search, SearchDirection::Forward);
        let outcome = handle_input_key(&mut search, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(outcome, InputOutcome::Cancelled));
        assert!(matches!(search, ViewerSearch::Idle));
    }

    #[test]
    fn handle_input_key_types_then_confirms_on_enter() {
        let mut search = ViewerSearch::Idle;
        begin(&mut search, SearchDirection::Forward);
        for c in "hi".chars() {
            let outcome = handle_input_key(&mut search, KeyCode::Char(c), KeyModifiers::NONE);
            assert!(matches!(outcome, InputOutcome::Editing));
        }
        let outcome = handle_input_key(&mut search, KeyCode::Enter, KeyModifiers::NONE);
        match outcome {
            InputOutcome::Confirmed(pattern, direction) => {
                assert_eq!(pattern, "hi");
                assert_eq!(direction, SearchDirection::Forward);
            }
            _ => panic!("expected Confirmed"),
        }
    }

    #[test]
    fn run_finds_matches_and_activates_the_search() {
        let mut search = ViewerSearch::Idle;
        let haystack = lines(&["alpha", "beta", "gamma beta"]);
        let target = run(
            &mut search,
            &haystack,
            "beta".to_string(),
            SearchDirection::Forward,
            0,
        )
        .unwrap();
        assert_eq!(target, 1);
        assert!(matches!(
            search,
            ViewerSearch::Active { matches, .. } if matches == vec![1, 2]
        ));
    }

    #[test]
    fn run_reports_no_matches_as_err_and_leaves_idle() {
        let mut search = ViewerSearch::Idle;
        let haystack = lines(&["alpha", "beta"]);
        let err = run(
            &mut search,
            &haystack,
            "zzz".to_string(),
            SearchDirection::Forward,
            0,
        )
        .unwrap_err();
        assert_eq!(err, "zzz");
        assert!(matches!(search, ViewerSearch::Idle));
    }

    #[test]
    fn step_advances_and_wraps() {
        let mut search = ViewerSearch::Active {
            pattern: "x".to_string(),
            direction: SearchDirection::Forward,
            matches: vec![2, 5, 9],
            current: 2,
            wrapped: false,
        };
        let target = step(&mut search, false).unwrap();
        assert_eq!(target, 2); // wrapped back to matches[0]
        assert!(matches!(search, ViewerSearch::Active { wrapped: true, .. }));
    }

    #[test]
    fn step_reverse_steps_backward() {
        let mut search = ViewerSearch::Active {
            pattern: "x".to_string(),
            direction: SearchDirection::Forward,
            matches: vec![2, 5, 9],
            current: 2,
            wrapped: false,
        };
        let target = step(&mut search, true).unwrap(); // N: reverse of Forward = Backward
        assert_eq!(target, 5);
    }

    #[test]
    fn step_on_idle_search_is_a_no_op() {
        let mut search = ViewerSearch::Idle;
        assert_eq!(step(&mut search, false), None);
    }

    #[test]
    fn nearest_match_forward_wraps_to_the_first_match() {
        assert_eq!(
            nearest_match(&[10, 20, 30], 25, SearchDirection::Forward),
            (2, false)
        );
        assert_eq!(
            nearest_match(&[10, 20, 30], 40, SearchDirection::Forward),
            (0, true)
        );
    }

    #[test]
    fn nearest_match_backward_wraps_to_the_last_match() {
        assert_eq!(
            nearest_match(&[10, 20, 30], 25, SearchDirection::Backward),
            (1, false)
        );
        assert_eq!(
            nearest_match(&[10, 20, 30], 5, SearchDirection::Backward),
            (2, true)
        );
    }
}
