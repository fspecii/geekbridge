// SPDX-License-Identifier: GPL-3.0-or-later
//! Text from a phone, made safe to hand to `notify-send` (docs/reports/security-review-2.md S2-3).
//!
//! Names, titles and caller labels are chosen by the other side. Notification daemons render some
//! markup (`<b>`, `<a href>`, `<img>`) and a hostile string can also reorder what is displayed with
//! bidirectional controls, so those are neutralised, not only the ASCII control characters. The
//! callers also put `--` before the positional arguments, so a text that starts with `-` cannot
//! become an option.

/// `text` without control, zero-width and bidirectional-format characters, with `<`, `>` and `&`
/// replaced by look-alikes that no markup parser treats as syntax, cut to `max` characters.
pub fn plain(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .map(|c| match c {
            '<' => '\u{2039}',
            '>' => '\u{203A}',
            '&' => '\u{FF06}',
            other => other,
        })
        .take(max)
        .collect()
}

fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'   // zero-width space/joiners, LRM, RLM
            | '\u{202A}'..='\u{202E}' // embeddings and overrides
            | '\u{2060}'..='\u{206F}' // word joiner, isolates, deprecated formatting
            | '\u{061C}'            // Arabic letter mark
            | '\u{FEFF}' // byte-order mark / zero-width no-break space
    )
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn markup_and_direction_tricks_are_neutralised() {
        let s = plain(
            "<b>Bank</b> <a href=\"http://x\">pay</a> & \u{202E}live\u{200B}",
            80,
        );
        assert!(!s.contains('<') && !s.contains('>') && !s.contains('&'));
        assert!(!s.contains('\u{202E}') && !s.contains('\u{200B}'));
        assert!(s.contains("Bank") && s.contains("live"));
    }

    #[test]
    fn plain_text_is_unchanged_and_bounded() {
        assert_eq!(plain("Ana's phone – ünï", 80), "Ana's phone – ünï");
        assert_eq!(plain("abcdef", 3), "abc");
    }
}
