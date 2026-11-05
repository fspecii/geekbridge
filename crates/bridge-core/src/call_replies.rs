// SPDX-License-Identifier: GPL-3.0-or-later
//! The short texts offered by "Decline with message": which they are by default, and what a saved
//! list has to look like. Pure data; the service stores the list and sends the chosen text through
//! the ordinary Messages path.

/// At most this many texts are offered (the card has room for three).
pub const MAX_REPLIES: usize = 3;
/// Longest text, in characters. Short on purpose: one SMS segment, and it has to fit a small card.
pub const MAX_REPLY_CHARS: usize = 70;

/// What is offered until the person writes their own.
pub const DEFAULT_REPLIES: [&str; MAX_REPLIES] = [
    "Can't talk now, I'll call you back",
    "I'm in a meeting",
    "Call me in a few minutes",
];

pub fn defaults() -> Vec<String> {
    DEFAULT_REPLIES.iter().map(|s| (*s).to_owned()).collect()
}

/// Why a list was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    TooMany,
    TooLong,
    Control,
    /// The list could not be written to disk.
    Save,
}

impl ReplyError {
    pub fn message(self) -> &'static str {
        match self {
            Self::TooMany => "at most three replies can be saved",
            Self::TooLong => "a reply is too long (70 characters at most)",
            Self::Control => "a reply may not contain line breaks or control characters",
            Self::Save => "the replies could not be saved",
        }
    }
}

/// Clean a list the person typed: trim, drop blanks, check length. An empty result means "use the
/// defaults" and is returned as an empty list, never stored as such.
pub fn clean(texts: &[String]) -> Result<Vec<String>, ReplyError> {
    if texts.len() > MAX_REPLIES {
        return Err(ReplyError::TooMany);
    }
    let mut out = Vec::new();
    for t in texts {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if t.chars().any(char::is_control) {
            return Err(ReplyError::Control);
        }
        if t.chars().count() > MAX_REPLY_CHARS {
            return Err(ReplyError::TooLong);
        }
        out.push(t.to_owned());
    }
    Ok(out)
}

/// The texts in effect for a saved list (possibly empty): the saved ones, else the defaults.
pub fn effective(saved: &[String]) -> Vec<String> {
    if saved.is_empty() {
        defaults()
    } else {
        saved.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_defaults_are_three_short_texts_that_pass_their_own_rules() {
        let d = defaults();
        assert_eq!(d.len(), MAX_REPLIES);
        assert_eq!(clean(&d).unwrap(), d);
    }

    #[test]
    fn blanks_are_dropped_and_text_is_trimmed() {
        assert_eq!(
            clean(&v(&["  In a meeting ", "", "   "])).unwrap(),
            v(&["In a meeting"])
        );
    }

    #[test]
    fn an_empty_list_means_the_defaults() {
        let none = clean(&v(&["", " "])).unwrap();
        assert!(none.is_empty());
        assert_eq!(effective(&none), defaults());
        assert_eq!(effective(&v(&["Busy"])), v(&["Busy"]));
    }

    #[test]
    fn too_many_too_long_and_control_characters_are_refused() {
        assert_eq!(clean(&v(&["a", "b", "c", "d"])), Err(ReplyError::TooMany));
        assert_eq!(
            clean(&["x".repeat(MAX_REPLY_CHARS + 1)]),
            Err(ReplyError::TooLong)
        );
        assert!(clean(&["x".repeat(MAX_REPLY_CHARS)]).is_ok());
        assert_eq!(clean(&v(&["a\nb"])), Err(ReplyError::Control));
        assert_eq!(clean(&v(&["a\u{0}"])), Err(ReplyError::Control));
    }

    #[test]
    fn length_counts_characters_not_bytes() {
        assert!(clean(&["é".repeat(MAX_REPLY_CHARS)]).is_ok());
    }
}
