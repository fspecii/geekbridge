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

/// Truncate in place to at most `max` characters, on a character boundary.
pub fn limit_chars(text: &mut String, max: usize) {
    if let Some((index, _)) = text.char_indices().nth(max) {
        text.truncate(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
