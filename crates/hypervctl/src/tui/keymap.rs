//! Keys to actions, as one table per context (spec 3.3, design 5.8). The key
//! hints in the status bar are generated from the same table, so a binding and
//! its description can never drift apart.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::tui::store::{Action, DetailTab};

/// Which table a key is looked up in (spec 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    List,
    Detail,
}

const NONE: KeyModifiers = KeyModifiers::empty();
const CONTROL: KeyModifiers = KeyModifiers::CONTROL;

/// One row of the keymap: where it applies, which keys trigger it, what it
/// does and how the status bar describes it.
struct Binding {
    contexts: &'static [Context],
    keys: &'static [(KeyCode, KeyModifiers)],
    action: Action,
    description: &'static str,
}

const LIST: &[Context] = &[Context::List];
const DETAIL: &[Context] = &[Context::Detail];
const BOTH: &[Context] = &[Context::List, Context::Detail];

/// The whole keymap of the TUI, in the order the status bar shows the hints.
///
/// The keys of the menu, the confirmation dialog and the help belong here once
/// those exist (M9).
const BINDINGS: &[Binding] = &[
    Binding {
        contexts: BOTH,
        keys: &[(KeyCode::Char('q'), NONE), (KeyCode::Char('c'), CONTROL)],
        action: Action::Quit,
        description: "quit",
    },
    Binding {
        contexts: BOTH,
        keys: &[(KeyCode::Tab, NONE)],
        action: Action::FocusToggle,
        description: "focus",
    },
    Binding {
        contexts: BOTH,
        keys: &[(KeyCode::Char('r'), NONE)],
        action: Action::RefreshNow,
        description: "refresh",
    },
    Binding {
        contexts: BOTH,
        keys: &[(KeyCode::Char('1'), NONE)],
        action: Action::SelectTab(DetailTab::Details),
        description: "details tab",
    },
    Binding {
        contexts: BOTH,
        keys: &[(KeyCode::Char('2'), NONE)],
        action: Action::SelectTab(DetailTab::Checkpoints),
        description: "checkpoints tab",
    },
    Binding {
        contexts: LIST,
        keys: &[(KeyCode::Char('j'), NONE), (KeyCode::Down, NONE)],
        action: Action::SelectNext,
        description: "move",
    },
    Binding {
        contexts: LIST,
        keys: &[(KeyCode::Char('k'), NONE), (KeyCode::Up, NONE)],
        action: Action::SelectPrev,
        description: "move",
    },
    Binding {
        contexts: LIST,
        keys: &[(KeyCode::Char('g'), NONE)],
        action: Action::SelectFirst,
        description: "first",
    },
    Binding {
        contexts: LIST,
        keys: &[(KeyCode::Char('G'), NONE)],
        action: Action::SelectLast,
        description: "last",
    },
    Binding {
        contexts: LIST,
        keys: &[(KeyCode::Enter, NONE)],
        action: Action::FocusDetail,
        description: "detail pane",
    },
    Binding {
        contexts: DETAIL,
        keys: &[(KeyCode::Char('h'), NONE), (KeyCode::Left, NONE)],
        action: Action::TabPrev,
        description: "previous tab",
    },
    Binding {
        contexts: DETAIL,
        keys: &[(KeyCode::Char('l'), NONE), (KeyCode::Right, NONE)],
        action: Action::TabNext,
        description: "next tab",
    },
    Binding {
        contexts: DETAIL,
        keys: &[(KeyCode::Char('j'), NONE), (KeyCode::Down, NONE)],
        action: Action::ScrollDown,
        description: "scroll",
    },
    Binding {
        contexts: DETAIL,
        keys: &[(KeyCode::Char('k'), NONE), (KeyCode::Up, NONE)],
        action: Action::ScrollUp,
        description: "scroll",
    },
    Binding {
        contexts: DETAIL,
        keys: &[(KeyCode::Esc, NONE)],
        action: Action::FocusList,
        description: "back to list",
    },
];

/// The action a key asks for in `context`, or nothing.
///
/// Windows sends a release event after every press; only presses count, or one
/// keypress would act twice (design 5.8, acceptance 12).
pub fn action(context: Context, key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    BINDINGS
        .iter()
        .find(|binding| {
            binding.contexts.contains(&context)
                && binding
                    .keys
                    .iter()
                    .any(|&(code, modifiers)| matches(&key, code, modifiers))
        })
        .map(|binding| binding.action.clone())
}

/// The key hints of `context`, in the order of the table (design 5.8).
pub fn hints(context: Context) -> Vec<String> {
    BINDINGS
        .iter()
        .filter(|binding| binding.contexts.contains(&context))
        .map(|binding| {
            let keys: Vec<&str> = binding
                .keys
                .iter()
                .map(|(code, modifiers)| key_name(*code, *modifiers))
                .collect();
            format!("{} {}", keys.join("/"), binding.description)
        })
        .collect()
}

fn key_name(code: KeyCode, modifiers: KeyModifiers) -> &'static str {
    match (code, modifiers) {
        (KeyCode::Char('c'), CONTROL) => "Ctrl+C",
        (KeyCode::Char('q'), _) => "q",
        (KeyCode::Char('r'), _) => "r",
        (KeyCode::Char('1'), _) => "1",
        (KeyCode::Char('2'), _) => "2",
        (KeyCode::Char('j'), _) => "j",
        (KeyCode::Char('k'), _) => "k",
        (KeyCode::Char('g'), _) => "g",
        (KeyCode::Char('G'), _) => "G",
        (KeyCode::Char('h'), _) => "h",
        (KeyCode::Char('l'), _) => "l",
        (KeyCode::Tab, _) => "Tab",
        (KeyCode::Enter, _) => "Enter",
        (KeyCode::Esc, _) => "Esc",
        (KeyCode::Up, _) => "Up",
        (KeyCode::Down, _) => "Down",
        (KeyCode::Left, _) => "Left",
        (KeyCode::Right, _) => "Right",
        _ => "?",
    }
}

/// Whether a key event asks for `code` with `modifiers`.
///
/// `Shift` is folded into the character: an uppercase char already says that
/// the shift key was down.
fn matches(key: &KeyEvent, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if key.code != code {
        return false;
    }
    let mut pressed = key.modifiers;
    pressed.remove(KeyModifiers::SHIFT);
    pressed == modifiers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, NONE)
    }

    fn press_with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn the_list_keys_match_the_specification_table() {
        assert_eq!(
            action(Context::List, press(KeyCode::Char('j'))),
            Some(Action::SelectNext)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Char('k'))),
            Some(Action::SelectPrev)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Down)),
            Some(Action::SelectNext)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Up)),
            Some(Action::SelectPrev)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Char('g'))),
            Some(Action::SelectFirst)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Char('G'))),
            Some(Action::SelectLast)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Enter)),
            Some(Action::FocusDetail)
        );
        assert_eq!(
            action(Context::List, press(KeyCode::Tab)),
            Some(Action::FocusToggle)
        );
    }

    #[test]
    fn the_detail_keys_match_the_specification_table() {
        assert_eq!(
            action(Context::Detail, press(KeyCode::Char('h'))),
            Some(Action::TabPrev)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Char('l'))),
            Some(Action::TabNext)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Left)),
            Some(Action::TabPrev)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Right)),
            Some(Action::TabNext)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Char('j'))),
            Some(Action::ScrollDown)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Char('k'))),
            Some(Action::ScrollUp)
        );
        assert_eq!(
            action(Context::Detail, press(KeyCode::Esc)),
            Some(Action::FocusList)
        );
    }

    #[test]
    fn the_keys_of_both_contexts_work_in_either() {
        for context in [Context::List, Context::Detail] {
            assert_eq!(
                action(context, press(KeyCode::Char('q'))),
                Some(Action::Quit)
            );
            assert_eq!(
                action(context, press_with(KeyCode::Char('c'), CONTROL)),
                Some(Action::Quit)
            );
            assert_eq!(
                action(context, press(KeyCode::Char('r'))),
                Some(Action::RefreshNow)
            );
            assert_eq!(
                action(context, press(KeyCode::Char('1'))),
                Some(Action::SelectTab(DetailTab::Details))
            );
            assert_eq!(
                action(context, press(KeyCode::Char('2'))),
                Some(Action::SelectTab(DetailTab::Checkpoints))
            );
        }
    }

    #[test]
    fn a_release_is_ignored_so_one_keypress_is_one_action() {
        let mut release = press(KeyCode::Char('j'));
        release.kind = KeyEventKind::Release;
        assert_eq!(action(Context::List, release), None);

        let mut repeat = press(KeyCode::Char('j'));
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(action(Context::List, repeat), None);
    }

    #[test]
    fn a_key_of_the_other_context_does_nothing() {
        assert_eq!(action(Context::List, press(KeyCode::Char('l'))), None);
        assert_eq!(action(Context::Detail, press(KeyCode::Char('g'))), None);
        assert_eq!(action(Context::Detail, press(KeyCode::Enter)), None);
        assert_eq!(action(Context::List, press(KeyCode::Esc)), None);
    }

    #[test]
    fn the_hints_are_generated_from_the_table() {
        let list = hints(Context::List);
        assert_eq!(list[0], "q/Ctrl+C quit");
        assert!(list.contains(&"j/Down move".to_string()), "{list:?}");
        assert!(list.contains(&"Enter detail pane".to_string()), "{list:?}");
        assert!(!list.iter().any(|hint| hint.contains("scroll")), "{list:?}");

        let detail = hints(Context::Detail);
        assert!(
            detail.contains(&"h/Left previous tab".to_string()),
            "{detail:?}"
        );
        assert!(
            detail.contains(&"Esc back to list".to_string()),
            "{detail:?}"
        );
        assert!(
            !detail.iter().any(|hint| hint.contains("first")),
            "{detail:?}"
        );
    }
}
