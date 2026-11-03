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
    clean(text, max, false)
}

/// First-strong isolate and pop: the pieces [`isolate`] wraps a phone-chosen value in.
const FSI: char = '\u{2068}';
const PDI: char = '\u{2069}';

/// `text` cleaned like [`plain`] and wrapped in a bidirectional isolate, so a right-to-left name, title or
/// number cannot reorder the words around it in a banner ("Ana: 5 min" must not read "5 min :Ana").
/// The isolate characters are the only direction controls [`banner_safe`] lets through.
pub fn isolate(text: &str, max: usize) -> String {
    format!("{FSI}{}{PDI}", plain(text, max))
}

/// Like [`plain`] for a string already assembled from [`isolate`]d parts: keeps their isolates, strips every
/// other direction control, and closes any isolate the length limit cut off.
pub fn banner_safe(text: &str, max: usize) -> String {
    let mut out = clean(text, max, true);
    let open = out.chars().filter(|c| *c == FSI).count();
    let close = out.chars().filter(|c| *c == PDI).count();
    for _ in close..open {
        out.push(PDI);
    }
    out
}

fn clean(text: &str, max: usize, keep_isolates: bool) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && (!is_invisible_format(*c) || (keep_isolates && matches!(*c, FSI | PDI)))
        })
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
    use super::{banner_safe, isolate, plain};

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
    fn isolate_wraps_cleaned_text_and_keeps_the_sentence_in_order() {
        let arabic = "\u{645}\u{643}\u{62a}\u{628}";
        let s = isolate(&format!("\u{202E}{arabic}\u{2069}<b>"), 40);
        assert!(s.starts_with('\u{2068}') && s.ends_with('\u{2069}'));
        // The hostile controls inside are gone: exactly one isolate pair is left, ours.
        assert_eq!(s.chars().filter(|c| *c == '\u{2068}').count(), 1);
        assert_eq!(s.chars().filter(|c| *c == '\u{2069}').count(), 1);
        assert!(!s.contains('\u{202E}') && !s.contains('<'));
        let line = banner_safe(
            &format!("{}: {}", isolate("Ana", 8), isolate(arabic, 40)),
            160,
        );
        assert_eq!(
            line,
            format!("\u{2068}Ana\u{2069}: \u{2068}{arabic}\u{2069}")
        );
    }

    #[test]
    fn a_length_cut_never_leaves_an_isolate_open() {
        let s = banner_safe(&isolate(&"x".repeat(100), 100), 20);
        assert_eq!(s.chars().filter(|c| *c == '\u{2068}').count(), 1);
        assert!(s.ends_with('\u{2069}'));
        // A raw pop or override that was not produced by `isolate` is still stripped by plain().
        assert_eq!(plain("a\u{2068}b\u{2069}c", 10), "abc");
    }

    #[test]
    fn plain_text_is_unchanged_and_bounded() {
        assert_eq!(plain("Ana's phone – ünï", 80), "Ana's phone – ünï");
        assert_eq!(plain("abcdef", 3), "abc");
    }
}
