// SPDX-License-Identifier: GPL-3.0-or-later
//! Locale-neutral name ordering for the Contacts list, without a collation database.
//!
//! Plain `to_lowercase()` order puts "Émile" after "Zoë" and "ñandú" after "z". This folds the
//! Latin letters that carry accents or ligatures to their base letters (the way every phone book
//! groups them), then orders: symbols / digits / emoji first, Latin names next, every other
//! script (Greek, Cyrillic, Hebrew, Arabic, CJK, ...) last in code point order. Code point order
//! is the honest limit for CJK: pinyin / kana readings need a dictionary this app does not ship.
//! The same classification decides the A-Z rail letter, so list order and sections always agree.

/// What kind of script a name starts with; also the primary sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Digits, punctuation, emoji, anything that is not a letter.
    Symbol,
    Latin,
    /// Letters of any other script.
    Other,
}

/// Base letter(s) of a Latin letter with diacritics or a ligature; `None` for letters that fold to
/// themselves (and for anything non-Latin).
fn fold_latin(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ǻ' | 'ạ' | 'ả' | 'ấ' | 'ầ'
        | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => "a",
        'æ' | 'ǣ' | 'ǽ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' | 'ḍ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế' | 'ề' | 'ể'
        | 'ễ' | 'ệ' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ỉ' | 'ị' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ṇ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' | 'ǒ' | 'ọ' | 'ỏ' | 'ố' | 'ồ'
        | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ǔ' | 'ụ' | 'ủ' | 'ứ'
        | 'ừ' | 'ử' | 'ữ' | 'ự' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// True for the Latin script (basic, supplement, extended, additional).
fn is_latin_letter(c: char) -> bool {
    c.is_alphabetic()
        && matches!(c as u32,
            0x41..=0x24F | 0x1E00..=0x1EFF | 0x2C60..=0x2C7F | 0xA720..=0xA7FF | 0xFF21..=0xFF3A | 0xFF41..=0xFF5A)
}

/// The first character that is not a space or mark decides the class.
fn first_significant(name: &str) -> Option<char> {
    name.trim().chars().next()
}

/// Class of a name (see the module docs).
pub fn class_of(name: &str) -> Class {
    match first_significant(name) {
        Some(c) if is_latin_letter(c) => Class::Latin,
        Some(c) if c.is_alphabetic() => Class::Other,
        _ => Class::Symbol,
    }
}

/// Lower-case, accent-folded text for searching: "emile" finds "Émile".
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        // Combining marks (NFD input) fold away with their base letter.
        if matches!(c as u32, 0x300..=0x36F) {
            continue;
        }
        match fold_latin(c) {
            Some(base) => out.push_str(base),
            None => out.push(c),
        }
    }
    out
}

/// Sort key: class, then the accent-folded lower-case name. Equal keys fall back to the caller's
/// tie-breaker (the contact id).
pub fn key(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 1);
    out.push(match class_of(name) {
        Class::Symbol => '0',
        Class::Latin => '1',
        Class::Other => '2',
    });
    out.push_str(&fold(name.trim()));
    out
}

/// Rail / section letter: `A`-`Z` for Latin names (accents folded), `#` for symbols and digits,
/// `…` for every other script.
pub fn section_letter(name: &str) -> char {
    match class_of(name) {
        Class::Symbol => '#',
        Class::Other => '…',
        Class::Latin => key(name)
            .chars()
            .nth(1)
            .filter(char::is_ascii_alphabetic)
            .map_or('#', |c| c.to_ascii_uppercase()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_sort_with_their_base_letter() {
        let mut names = vec![
            "Zoë", "Émile", "Eve", "ñandú", "Nora", "Çelik", "Cem", "Straße", "Strasse",
        ];
        names.sort_by_key(|n| key(n));
        assert_eq!(
            names,
            [
                "Çelik", "Cem", "Émile", "Eve", "ñandú", "Nora", "Straße", "Strasse", "Zoë"
            ]
        );
    }

    #[test]
    fn classes_order_symbols_latin_then_other_scripts() {
        let mut names = vec!["山田太郎", "Ада", "Ann", "🎉 Party", "42 Pizza", "Émile"];
        names.sort_by_key(|n| key(n));
        assert_eq!(
            names,
            ["42 Pizza", "🎉 Party", "Ann", "Émile", "Ада", "山田太郎"]
        );
    }

    #[test]
    fn rail_letters_agree_with_the_order() {
        assert_eq!(section_letter("Émile"), 'E');
        assert_eq!(section_letter("ñandú"), 'N');
        assert_eq!(section_letter("Łukasz"), 'L');
        assert_eq!(section_letter("42 Pizza"), '#');
        assert_eq!(section_letter("🎉"), '#');
        assert_eq!(section_letter("山田"), '…');
        assert_eq!(section_letter("Ада"), '…');
        assert_eq!(section_letter(""), '#');
    }

    #[test]
    fn combining_marks_fold_like_precomposed_letters() {
        assert_eq!(key("E\u{301}mile"), key("Émile"));
    }
}
