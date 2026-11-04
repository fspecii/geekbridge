// SPDX-License-Identifier: GPL-3.0-or-later
//! The "To" field of a new message, by contact name. What the user types is either a phone number
//! (nothing changes: it is used as typed) or a name: then the contacts the phone shared are searched
//! and a list is offered. A contact with one number is chosen in one step; a contact with several
//! asks which number. Pure logic, no drawing, so keyboard use (Up, Down, Enter, Escape) is tested.
use crate::i18n::{tr, tr_args};
use bridge_core::{collate, phone_view::Person};

/// Most contacts listed at once.
pub const MAX_ROWS: usize = 6;
/// Longest text the To field takes while it holds a name.
pub const MAX_NAME_CHARS: usize = 64;

/// A letter anywhere means a name (a number holds digits, `+`, `*`, `#` only).
pub fn is_name_query(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

/// A number the phone can be asked to text.
pub fn dialable(text: &str) -> bool {
    !is_name_query(text) && text.chars().any(|c| c.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    /// Distinct numbers (by digits), in the phone's order.
    pub numbers: Vec<String>,
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Contacts with a number whose name contains every word of `query` (case and accents ignored), best
/// first: names that start with the query, then the rest in name order. The same person on two
/// phones, or the same number twice, is listed once.
pub fn candidates(people: &[Person], query: &str) -> Vec<Candidate> {
    let words: Vec<String> = query.split_whitespace().map(collate::fold).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let whole = collate::fold(query.trim());
    let mut out: Vec<(bool, String, Candidate)> = Vec::new();
    for p in people {
        let folded = collate::fold(&p.name);
        if p.name.trim().is_empty() || !words.iter().all(|w| folded.contains(w.as_str())) {
            continue;
        }
        let mut numbers: Vec<String> = Vec::new();
        for n in &p.phones {
            let d = digits(n);
            if !d.is_empty() && !numbers.iter().any(|o| digits(o) == d) {
                numbers.push(n.trim().to_owned());
            }
        }
        if numbers.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(_, _, c)| c.name == p.name) {
            Some((_, _, c)) => {
                for n in numbers {
                    if !c.numbers.iter().any(|o| digits(o) == digits(&n)) {
                        c.numbers.push(n);
                    }
                }
            }
            None => out.push((
                folded.starts_with(&whole),
                collate::key(&p.name),
                Candidate {
                    name: p.name.clone(),
                    numbers,
                },
            )),
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.into_iter().map(|(_, _, c)| c).take(MAX_ROWS).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKey {
    Up,
    Down,
    Enter,
    Escape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing to do (the key was not for the list).
    Ignored,
    /// The list changed (highlight moved, or it now asks for a number).
    Moved,
    /// A name and one of its numbers was chosen.
    Picked { name: String, number: String },
    /// Escape with nothing to go back to: close the list.
    Dismissed,
}

/// Highlight and, for a contact with several numbers, which contact is being asked about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picker {
    pub highlight: usize,
    pub asking: Option<Candidate>,
}

/// One drawn row: what it says and what a screen reader says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub text: String,
    pub spoken: String,
}

impl Picker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn len(&self, cands: &[Candidate]) -> usize {
        self.asking
            .as_ref()
            .map_or(cands.len(), |c| c.numbers.len())
    }

    /// Rows for the list now on screen (numbers of one contact while asking, else the contacts).
    pub fn rows(&self, cands: &[Candidate]) -> Vec<Row> {
        match &self.asking {
            Some(c) => c
                .numbers
                .iter()
                .map(|n| Row {
                    text: n.clone(),
                    spoken: tr_args(
                        "compose.to.number_spoken",
                        &[("name", &c.name), ("number", n)],
                    ),
                })
                .collect(),
            None => cands
                .iter()
                .map(|c| match c.numbers.as_slice() {
                    [one] => Row {
                        text: format!("{}  {one}", c.name),
                        spoken: tr_args(
                            "compose.to.contact_spoken",
                            &[("name", &c.name), ("number", one)],
                        ),
                    },
                    many => Row {
                        text: format!(
                            "{}  {}",
                            c.name,
                            tr_args("compose.to.numbers", &[("n", &many.len())])
                        ),
                        spoken: tr_args(
                            "compose.to.several_spoken",
                            &[("name", &c.name), ("n", &many.len())],
                        ),
                    },
                })
                .collect(),
        }
    }

    pub fn heading(&self) -> Option<String> {
        self.asking
            .as_ref()
            .map(|c| tr_args("compose.to.choose", &[("name", &c.name)]))
    }

    /// Choose row `index` (Enter on it, or a click).
    pub fn choose(&mut self, index: usize, cands: &[Candidate]) -> Step {
        if let Some(c) = &self.asking {
            return match c.numbers.get(index) {
                Some(n) => Step::Picked {
                    name: c.name.clone(),
                    number: n.clone(),
                },
                None => Step::Ignored,
            };
        }
        let Some(c) = cands.get(index) else {
            return Step::Ignored;
        };
        if let [only] = c.numbers.as_slice() {
            return Step::Picked {
                name: c.name.clone(),
                number: only.clone(),
            };
        }
        self.asking = Some(c.clone());
        self.highlight = 0;
        Step::Moved
    }

    pub fn key(&mut self, key: PickKey, cands: &[Candidate]) -> Step {
        let n = self.len(cands);
        if n == 0 {
            return Step::Ignored;
        }
        self.highlight = self.highlight.min(n - 1);
        match key {
            PickKey::Down => {
                self.highlight = (self.highlight + 1) % n;
                Step::Moved
            }
            PickKey::Up => {
                self.highlight = (self.highlight + n - 1) % n;
                Step::Moved
            }
            PickKey::Enter => self.choose(self.highlight, cands),
            PickKey::Escape => {
                if self.asking.take().is_some() {
                    self.highlight = 0;
                    Step::Moved
                } else {
                    Step::Dismissed
                }
            }
        }
    }
}

/// The line under the field when a name matches nobody.
pub fn none_found() -> String {
    tr("compose.to.none").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str, phones: &[&str]) -> Person {
        Person {
            device_id: "d".into(),
            id: name.into(),
            name: name.into(),
            phones: phones.iter().map(|s| (*s).to_owned()).collect(),
            emails: vec![],
            organization: None,
        }
    }

    fn people() -> Vec<Person> {
        vec![
            person("Ann Lee", &["+44 20 7946 0001"]),
            person(
                "Joanna Smith",
                &["+1 555 0100", "+1 555 0101", "+1 (555) 0100"],
            ),
            person("Émile Zola", &["+33 1 23 45 67 89"]),
            person("No Number", &[]),
            person("Ann Lee", &["+44 20 7946 0001", "+44 7700 900123"]),
        ]
    }

    #[test]
    fn a_letter_means_a_name_and_digits_alone_stay_a_number() {
        assert!(is_name_query("ann"));
        assert!(is_name_query("1-800-FLOWERS"));
        assert!(!is_name_query("+49 151 2345678"));
        assert!(dialable("+49 151 2345678"));
        assert!(dialable("112"));
        assert!(!dialable("ann"));
        assert!(!dialable("+"));
        assert!(!dialable(""));
    }

    #[test]
    fn names_match_by_every_word_ignoring_case_and_accents_and_duplicates_collapse() {
        let c = candidates(&people(), "ann");
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Ann Lee", "Joanna Smith"],
            "prefix first, then the rest"
        );
        assert_eq!(
            c[0].numbers,
            ["+44 20 7946 0001", "+44 7700 900123"],
            "the same person on two phones is one"
        );
        assert_eq!(
            c[1].numbers,
            ["+1 555 0100", "+1 555 0101"],
            "the same number written twice is one"
        );
        assert_eq!(candidates(&people(), "emile")[0].name, "Émile Zola");
        assert_eq!(
            candidates(&people(), "lee ann").len(),
            1,
            "words in any order"
        );
        assert!(
            candidates(&people(), "no number").is_empty(),
            "a contact without a number is not offered"
        );
        assert!(candidates(&people(), "   ").is_empty());
        assert!(candidates(&people(), "zzz").is_empty());
        let many: Vec<Person> = (0..30)
            .map(|i| person(&format!("Sam {i}"), &["1"]))
            .collect();
        assert_eq!(candidates(&many, "sam").len(), MAX_ROWS);
    }

    #[test]
    fn up_down_wrap_and_enter_picks_a_single_number_at_once() {
        let c = candidates(&people(), "a");
        assert!(c.len() >= 3, "{c:?}");
        let mut p = Picker::default();
        assert_eq!(p.key(PickKey::Up, &c), Step::Moved);
        assert_eq!(p.highlight, c.len() - 1, "Up from the top wraps to the end");
        assert_eq!(p.key(PickKey::Down, &c), Step::Moved);
        assert_eq!(p.highlight, 0);
        let one = candidates(&people(), "emile");
        let mut p = Picker::default();
        assert_eq!(
            p.key(PickKey::Enter, &one),
            Step::Picked {
                name: "Émile Zola".into(),
                number: "+33 1 23 45 67 89".into()
            }
        );
    }

    #[test]
    fn a_contact_with_several_numbers_asks_which_and_escape_goes_back() {
        let c = candidates(&people(), "joanna");
        let mut p = Picker::default();
        assert_eq!(
            p.key(PickKey::Enter, &c),
            Step::Moved,
            "asks instead of guessing"
        );
        assert!(p.heading().unwrap().contains("Joanna Smith"));
        let rows = p.rows(&c);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].spoken.contains("Joanna Smith") && rows[1].spoken.contains("+1 555 0101"));
        assert_eq!(p.key(PickKey::Down, &c), Step::Moved);
        assert_eq!(
            p.key(PickKey::Enter, &c),
            Step::Picked {
                name: "Joanna Smith".into(),
                number: "+1 555 0101".into()
            }
        );
        // Escape: from the numbers back to the contacts, then close.
        let mut p = Picker::default();
        p.key(PickKey::Enter, &c);
        assert_eq!(p.key(PickKey::Escape, &c), Step::Moved);
        assert!(p.asking.is_none());
        assert_eq!(p.key(PickKey::Escape, &c), Step::Dismissed);
        // Nothing listed: the keys belong to the text field.
        assert_eq!(Picker::default().key(PickKey::Enter, &[]), Step::Ignored);
    }

    #[test]
    fn rows_say_the_number_or_how_many_and_a_screen_reader_gets_both() {
        let c = candidates(&people(), "a");
        let p = Picker::default();
        let rows = p.rows(&c);
        let several = rows.iter().find(|r| r.text.starts_with("Joanna")).unwrap();
        assert!(several.text.contains("2 numbers"), "{several:?}");
        assert!(several.spoken.contains("2 numbers"));
        let emile = Picker::default().rows(&candidates(&people(), "emile"));
        assert!(emile[0].text.contains("+33") && emile[0].spoken.contains("+33"));
    }
}
