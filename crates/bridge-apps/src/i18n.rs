// SPDX-License-Identifier: GPL-3.0-or-later
//! The one catalogue of user-visible desktop text.
//!
//! English is the source: `i18n/en.properties` holds every string under a stable key. A
//! translation is another file in the same format (`i18n/de.properties`, `i18n/pt-BR.properties`)
//! registered in [`BUNDLED`]. Lookup falls back from the exact tag (`pt-BR`) to the language
//! (`pt`) to English, and a key missing everywhere shows the key itself (a test catches that).
//!
//! * `tr("key")` is plain text, `tr_args("key", &[("name", &x)])` fills `{name}` placeholders and
//!   `tr_n("key", n, &[..])` picks `key.one` / `key.other` (and `zero`/`two`/`few`/`many` where the
//!   language needs them) by the plural rule of the active language; `{n}` is always filled in.
//! * The language comes from `GEEKBRIDGE_LANG`, then `LC_ALL`, `LC_MESSAGES`, `LANG`.
//!   `GEEKBRIDGE_LANG=en-XA` is a pseudo-locale: accented letters, ~40 % longer, so hard-coded
//!   (un-accented) text and clipping are easy to spot. `GEEKBRIDGE_I18N_DIR` loads `<tag>.properties`
//!   from a directory instead of the bundled files, to try a translation without rebuilding.
//!
//! File format (Java `.properties` subset, understood by Weblate): `key = value` per line, `#`
//! comments, `\n` for a newline, `\\` for a backslash, `{{` / `}}` for literal braces.
use std::collections::HashMap;
use std::sync::OnceLock;

const EN: &str = include_str!("../i18n/en.properties");

/// Translations compiled into the binary: `(tag, file contents)`. Add a line when adding a file.
pub const BUNDLED: &[(&str, &str)] = &[];

type Table = HashMap<String, String>;

/// Parses the `.properties` subset described in the module docs.
pub fn parse_catalogue(src: &str) -> Table {
    let mut out = Table::new();
    for line in src.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        out.insert(k.trim().to_owned(), unescape(v.trim_start()));
    }
    out
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Language tag from the environment values in precedence order; `pt_BR.UTF-8@x` gives `pt-BR`.
pub fn tag_from_env_values(
    geekbridge: Option<&str>,
    all: Option<&str>,
    messages: Option<&str>,
    lang: Option<&str>,
) -> String {
    let raw = [geekbridge, all, messages, lang]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
        .unwrap_or("");
    let base = raw.split(['.', '@']).next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return "en".to_owned();
    }
    base.replace('_', "-")
}

/// `pt-BR` gives `["pt-BR", "pt"]`; the order is the fallback order before English.
fn candidates(tag: &str) -> Vec<String> {
    let mut v = vec![tag.to_owned()];
    if let Some((lang, _)) = tag.split_once('-') {
        v.push(lang.to_owned());
    }
    v
}

fn is_pseudo(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("en-XA")
}

/// CLDR plural category for `n` in `lang` (language part of the tag).
pub fn plural_category(lang: &str, n: u64) -> &'static str {
    let lang = lang.split('-').next().unwrap_or("en");
    match lang {
        "ja" | "zh" | "ko" | "vi" | "th" | "id" | "ms" => "other",
        "fr" | "pt" if n <= 1 => "one",
        "ru" | "uk" | "be" => match (n % 10, n % 100) {
            (1, r) if r != 11 => "one",
            (2..=4, r) if !(12..=14).contains(&r) => "few",
            _ => "many",
        },
        "pl" => match (n, n % 10, n % 100) {
            (1, _, _) => "one",
            (_, 2..=4, r) if !(12..=14).contains(&r) => "few",
            _ => "many",
        },
        "cs" | "sk" => match n {
            1 => "one",
            2..=4 => "few",
            _ => "other",
        },
        "ar" => match (n, n % 100) {
            (0, _) => "zero",
            (1, _) => "one",
            (2, _) => "two",
            (_, 3..=10) => "few",
            (_, 11..=99) => "many",
            _ => "other",
        },
        "fr" | "pt" => "other",
        _ if n == 1 => "one",
        _ => "other",
    }
}

/// Substitutes `{name}` from `args`; `{{` and `}}` are literal braces; unknown names stay as written.
pub fn interpolate(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
        } else if let Some(end) = tail.find('}').filter(|_| tail.starts_with('{')) {
            let name = &tail[1..end];
            match args.iter().find(|(k, _)| *k == name) {
                Some((_, v)) => out.push_str(&v.to_string()),
                None => out.push_str(&tail[..=end]),
            }
            rest = &tail[end + 1..];
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Accents the letters and lengthens the text by about 40 % by repeating its own words, keeping
/// `{placeholders}` and brace escapes intact. Brackets mark the ends so clipping is visible.
pub fn pseudo(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut in_ph = false;
    for c in text.chars() {
        match c {
            '{' => {
                in_ph = true;
                out.push(c);
            }
            '}' => {
                in_ph = false;
                out.push(c);
            }
            _ if in_ph => out.push(c),
            _ => out.push(accent(c)),
        }
    }
    let visible = out.chars().filter(|c| *c != '\n').count();
    let want = visible * 7 / 5;
    let words: Vec<String> = out
        .split_whitespace()
        .filter(|w| !w.contains(['{', '}']))
        .map(str::to_owned)
        .collect();
    let mut extra = String::new();
    let mut i = 0;
    while !words.is_empty() && visible + extra.chars().count() < want {
        extra.push(' ');
        extra.push_str(&words[i % words.len()]);
        i += 1;
    }
    format!("[{out}{extra}]")
}

fn accent(c: char) -> char {
    match c {
        'a' => 'á',
        'b' => 'ƀ',
        'c' => 'ç',
        'd' => 'ď',
        'e' => 'é',
        'f' => 'ƒ',
        'g' => 'ğ',
        'h' => 'ĥ',
        'i' => 'í',
        'j' => 'ĵ',
        'k' => 'ķ',
        'l' => 'ĺ',
        'm' => 'ɱ',
        'n' => 'ñ',
        'o' => 'ö',
        'p' => 'þ',
        'q' => 'ǫ',
        'r' => 'ŕ',
        's' => 'š',
        't' => 'ţ',
        'u' => 'ü',
        'v' => 'ṽ',
        'w' => 'ŵ',
        'x' => 'ẋ',
        'y' => 'ý',
        'z' => 'ž',
        'A' => 'Á',
        'B' => 'Ɓ',
        'C' => 'Ç',
        'D' => 'Ď',
        'E' => 'É',
        'F' => 'Ƒ',
        'G' => 'Ĝ',
        'H' => 'Ĥ',
        'I' => 'Í',
        'J' => 'Ĵ',
        'K' => 'Ķ',
        'L' => 'Ĺ',
        'M' => 'Ṁ',
        'N' => 'Ñ',
        'O' => 'Ö',
        'P' => 'Þ',
        'Q' => 'Ǫ',
        'R' => 'Ŕ',
        'S' => 'Š',
        'T' => 'Ţ',
        'U' => 'Ü',
        'V' => 'Ṽ',
        'W' => 'Ŵ',
        'X' => 'Ẋ',
        'Y' => 'Ý',
        'Z' => 'Ž',
        other => other,
    }
}

/// The active catalogue: language tag plus its lookup tables, most specific first.
pub struct Catalogue {
    tag: String,
    tables: Vec<Table>,
    english: Table,
    pseudo: bool,
}

impl Catalogue {
    pub fn for_tag(tag: &str) -> Self {
        let dir = std::env::var_os("GEEKBRIDGE_I18N_DIR");
        let tables = if is_pseudo(tag) {
            Vec::new()
        } else {
            candidates(tag)
                .into_iter()
                .filter_map(|t| {
                    if let Some(d) = &dir
                        && let Ok(s) = std::fs::read_to_string(
                            std::path::Path::new(d).join(format!("{t}.properties")),
                        )
                    {
                        return Some(parse_catalogue(&s));
                    }
                    BUNDLED
                        .iter()
                        .find(|(b, _)| b.eq_ignore_ascii_case(&t))
                        .map(|(_, s)| parse_catalogue(s))
                })
                .collect()
        };
        Self {
            tag: tag.to_owned(),
            tables,
            english: parse_catalogue(EN),
            pseudo: is_pseudo(tag),
        }
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The template for `key`: translation, else English, else the key itself.
    fn template(&self, key: &str) -> String {
        let found = self
            .tables
            .iter()
            .find_map(|t| t.get(key))
            .or_else(|| self.english.get(key));
        match found {
            Some(s) if self.pseudo => pseudo(s),
            Some(s) => s.clone(),
            None => key.to_owned(),
        }
    }

    pub fn text(&self, key: &str) -> String {
        self.template(key)
    }

    pub fn args(&self, key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
        interpolate(&self.template(key), args)
    }

    pub fn plural(&self, key: &str, n: u64, args: &[(&str, &dyn std::fmt::Display)]) -> String {
        let lang = if self.pseudo { "en" } else { &self.tag };
        let cat = plural_category(lang, n);
        let has =
            |k: &str| self.tables.iter().any(|t| t.contains_key(k)) || self.english.contains_key(k);
        let chosen = [format!("{key}.{cat}"), format!("{key}.other")]
            .into_iter()
            .find(|k| has(k))
            .unwrap_or_else(|| format!("{key}.other"));
        let mut all: Vec<(&str, &dyn std::fmt::Display)> = vec![("n", &n)];
        all.extend_from_slice(args);
        interpolate(&self.template(&chosen), &all)
    }
}

/// The catalogue for this process, chosen once from the environment. Tests always see English.
pub fn current() -> &'static Catalogue {
    static CURRENT: OnceLock<Catalogue> = OnceLock::new();
    CURRENT.get_or_init(|| {
        #[cfg(test)]
        let tag = "en".to_owned();
        #[cfg(not(test))]
        let tag = {
            let var = |k: &str| std::env::var(k).ok();
            tag_from_env_values(
                var("GEEKBRIDGE_LANG").as_deref(),
                var("LC_ALL").as_deref(),
                var("LC_MESSAGES").as_deref(),
                var("LANG").as_deref(),
            )
        };
        Catalogue::for_tag(&tag)
    })
}

/// Plain text for `key`. Interned once per key so callers can hold a `&'static str`.
pub fn tr(key: &str) -> &'static str {
    static CACHE: std::sync::Mutex<Option<HashMap<String, &'static str>>> =
        std::sync::Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(s) = map.get(key) {
        return s;
    }
    let s: &'static str = Box::leak(current().text(key).into_boxed_str());
    map.insert(key.to_owned(), s);
    s
}

/// Text with `{name}` placeholders filled from `args`.
pub fn tr_args(key: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    current().args(key, args)
}

/// Text for a count: picks `key.<category>` for `n`, with `{n}` available.
pub fn tr_n(key: &str, n: u64, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    current().plural(key, n, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_follows_posix_precedence_and_normalises() {
        assert_eq!(
            tag_from_env_values(None, None, None, Some("pt_BR.UTF-8")),
            "pt-BR"
        );
        assert_eq!(
            tag_from_env_values(None, Some("de_DE"), None, Some("fr")),
            "de-DE"
        );
        assert_eq!(
            tag_from_env_values(Some("en-XA"), Some("de"), None, None),
            "en-XA"
        );
        assert_eq!(
            tag_from_env_values(None, Some(""), Some("es@euro"), None),
            "es"
        );
        assert_eq!(tag_from_env_values(None, None, None, Some("C")), "en");
        assert_eq!(tag_from_env_values(None, None, None, None), "en");
    }

    #[test]
    fn parses_comments_escapes_and_equals_in_values() {
        let t = parse_catalogue("# c\n\na = one\\ntwo\nb=x = y\n");
        assert_eq!(t["a"], "one\ntwo");
        assert_eq!(t["b"], "x = y");
    }

    #[test]
    fn named_arguments_and_brace_escapes() {
        let s = interpolate("{a} and {{b}} {c}", &[("a", &1)]);
        assert_eq!(s, "1 and {b} {c}");
    }

    #[test]
    fn plural_rules() {
        assert_eq!(plural_category("en", 1), "one");
        assert_eq!(plural_category("en", 0), "other");
        assert_eq!(plural_category("fr", 0), "one");
        assert_eq!(plural_category("ja", 1), "other");
        assert_eq!(plural_category("ru", 21), "one");
        assert_eq!(plural_category("ru", 12), "many");
        assert_eq!(plural_category("ru", 3), "few");
        assert_eq!(plural_category("ar", 0), "zero");
        assert_eq!(plural_category("ar", 7), "few");
    }

    #[test]
    fn pseudo_accents_lengthens_and_keeps_placeholders() {
        let p = pseudo("Send {name} a file now");
        assert!(p.starts_with('[') && p.ends_with(']'));
        assert!(p.contains("{name}"));
        assert!(!p.contains("Send"));
        let plain = "Send {name} a file now".chars().count();
        assert!(p.chars().count() >= plain * 7 / 5, "{p}");
    }

    #[test]
    fn fallback_chain_then_key() {
        let c = Catalogue::for_tag("xx-YY");
        assert_eq!(c.text("report.copy"), "Copy");
        assert_eq!(c.text("no.such.key"), "no.such.key");
    }

    #[test]
    fn plural_picks_category_and_fills_n() {
        let c = Catalogue::for_tag("en");
        assert_eq!(c.plural("phone.events", 1, &[]), "1 event");
        assert_eq!(c.plural("phone.events", 5, &[]), "5 events");
    }

    #[test]
    fn pseudo_catalogue_transforms_everything() {
        let c = Catalogue::for_tag("en-XA");
        assert_ne!(c.text("report.copy"), "Copy");
        assert!(c.plural("phone.events", 2, &[]).contains('2'));
    }
}
