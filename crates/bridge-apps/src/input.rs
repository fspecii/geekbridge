// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure text helpers for the local-only controls.

pub const MAX_DIAL_CHARS: usize = 32;
pub const MAX_DRAFT_CHARS: usize = 4_000;
pub const MAX_CLIPBOARD_CHARS: usize = 65_536;

/// Keep digits, `*`, `#`, and a leading `+`; bounded length.
pub fn sanitize_dial(input: &str) -> String {
    let mut out = String::new();
    for c in input.chars() {
        let ok = c.is_ascii_digit() || c == '*' || c == '#' || (c == '+' && out.is_empty());
        if ok && out.chars().count() < MAX_DIAL_CHARS {
            out.push(c);
        }
    }
    out
}

pub fn push_dial_char(number: &mut String, c: char) {
    let mut candidate = number.clone();
    candidate.push(c);
    *number = sanitize_dial(&candidate);
}

/// A contact search typed into the dialer: letters, digits, spaces and a few name marks; no control or
/// direction characters; bounded. It is only ever a search, never dialed.
pub fn clean_name_query(input: &str, max: usize) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || " -.'’+".contains(*c))
        .take(max)
        .collect()
}

/// Truncate in place to at most `max` characters, on a character boundary.
pub fn limit_chars(text: &mut String, max: usize) {
    if let Some((index, _)) = text.char_indices().nth(max) {
        text.truncate(index);
    }
}

use eframe::egui;

/// Whether an input-method composition (a preedit, as typing Chinese, Japanese or Korean produces) is
/// open. Enter that confirms a composition belongs to the input method: it must neither send a message
/// nor add a line. Platforms differ in whether the confirming Enter reaches the app at all, and in
/// whether it arrives before or after the `Commit`, so every Enter is judged in event order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ime {
    composing: bool,
}

impl Ime {
    pub fn composing(&self) -> bool {
        self.composing
    }

    /// Walk one frame's events in order and remove each Enter key press that happens while a composition
    /// is open or in the same frame as the commit that ends it. Returns how many were removed.
    pub fn drop_composing_enter(&mut self, events: &mut Vec<egui::Event>) -> usize {
        use egui::{Event, ImeEvent};
        let mut removed = 0;
        let mut committed_this_frame = false;
        let mut composing = self.composing;
        events.retain(|e| {
            match e {
                Event::Ime(ImeEvent::Enabled) => {}
                Event::Ime(ImeEvent::Preedit(text)) => composing = !text.is_empty(),
                Event::Ime(ImeEvent::Commit(_)) => {
                    composing = false;
                    committed_this_frame = true;
                }
                Event::Ime(ImeEvent::Disabled) => composing = false,
                Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } if composing || committed_this_frame => {
                    removed += 1;
                    return false;
                }
                _ => {}
            }
            true
        });
        self.composing = composing;
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, ImeEvent, Key, Modifiers};

    fn enter(pressed: bool) -> Event {
        Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }
    fn preedit(t: &str) -> Event {
        Event::Ime(ImeEvent::Preedit(t.into()))
    }

    #[test]
    fn enter_during_a_preedit_is_the_input_methods_and_enter_after_it_sends() {
        let mut ime = Ime::default();
        // Typing "ni" shows a preedit; Enter in a later frame is still inside the composition.
        let mut frame = vec![Event::Ime(ImeEvent::Enabled), preedit("ni")];
        assert_eq!(ime.drop_composing_enter(&mut frame), 0);
        assert!(ime.composing());
        let mut frame = vec![enter(true), enter(false)];
        assert_eq!(ime.drop_composing_enter(&mut frame), 1);
        assert_eq!(
            frame.len(),
            1,
            "only the press is taken; a release is harmless"
        );
        assert!(ime.composing(), "the commit has not arrived yet");
        // The input method commits (empty preedit follows): the next Enter is a real one.
        let mut frame = vec![Event::Ime(ImeEvent::Commit("\u{4f60}".into())), preedit("")];
        ime.drop_composing_enter(&mut frame);
        assert!(!ime.composing());
        let mut frame = vec![enter(true)];
        assert_eq!(ime.drop_composing_enter(&mut frame), 0);
        assert_eq!(frame.len(), 1);
    }

    #[test]
    fn the_enter_that_confirms_is_dropped_whichever_side_of_the_commit_it_arrives() {
        // Platform A: Enter, then Commit, in one frame (Enter pressed while composing).
        let mut ime = Ime::default();
        let mut frame = vec![
            preedit("a"),
            enter(true),
            Event::Ime(ImeEvent::Commit("a".into())),
        ];
        assert_eq!(ime.drop_composing_enter(&mut frame), 1);
        assert!(!frame.iter().any(|e| matches!(e, Event::Key { .. })));
        // Platform B: Commit first, the same Enter right behind it in the same frame.
        let mut ime = Ime::default();
        let mut frame = vec![
            preedit("a"),
            Event::Ime(ImeEvent::Commit("a".into())),
            enter(true),
        ];
        assert_eq!(ime.drop_composing_enter(&mut frame), 1);
        // A frame with an Enter and no composition anywhere is left alone, and so is another key.
        let mut ime = Ime::default();
        let mut frame = vec![enter(true), Event::Text("x".into())];
        assert_eq!(ime.drop_composing_enter(&mut frame), 0);
        assert_eq!(frame.len(), 2);
    }

    #[test]
    fn disabling_the_input_method_ends_a_composition() {
        let mut ime = Ime::default();
        ime.drop_composing_enter(&mut vec![preedit("x")]);
        assert!(ime.composing());
        ime.drop_composing_enter(&mut vec![Event::Ime(ImeEvent::Disabled)]);
        assert!(!ime.composing());
        let mut frame = vec![enter(true)];
        assert_eq!(ime.drop_composing_enter(&mut frame), 0);
    }

    #[test]
    fn dial_input_is_filtered() {
        assert_eq!(sanitize_dial("+1 (555) 010-99#*"), "+155501099#*");
        assert_eq!(sanitize_dial("1+2"), "12");
        assert_eq!(sanitize_dial(&"9".repeat(100)).len(), MAX_DIAL_CHARS);
        let mut n = String::new();
        push_dial_char(&mut n, '+');
        push_dial_char(&mut n, '+');
        push_dial_char(&mut n, '7');
        assert_eq!(n, "+7");
    }

    #[test]
    fn limit_respects_char_boundaries() {
        let mut s = "héllo wörld".to_string();
        limit_chars(&mut s, 4);
        assert_eq!(s, "héll");
        limit_chars(&mut s, 10);
        assert_eq!(s, "héll");
    }
}
