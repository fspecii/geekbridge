// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeps the desktop catalogue honest: every key used in code exists in English, every English
//! key is used, translations only use known keys and placeholders, and UI source files contain no
//! user-visible literal outside the catalogue (files not yet migrated are on a shrinking allowlist).
use bridge_apps::i18n::{self, BUNDLED};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// Code before the first `#[cfg(test)] mod ...` (test modules sit at the end of each file). A
/// `#[cfg(test)]` on an `impl` or `fn` mid-file is a test helper, not the start of the tests.
fn non_test(src: &str) -> &str {
    let mut from = 0;
    while let Some(i) = src[from..].find("#[cfg(test)]") {
        let at = from + i;
        let mut rest = src[at + "#[cfg(test)]".len()..].trim_start();
        while rest.starts_with("#[") {
            rest = rest.split_once('\n').map_or("", |(_, r)| r.trim_start());
        }
        if rest.starts_with("mod ") {
            return &src[..at];
        }
        from = at + 1;
    }
    src
}

fn english() -> BTreeMap<String, String> {
    i18n::parse_catalogue(include_str!("../i18n/en.properties"))
        .into_iter()
        .collect()
}

/// Plain string literals (no raw strings) on one line.
fn literals(line: &str) -> Vec<String> {
    let b: Vec<char> = line.chars().collect();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            '\'' if i + 2 < b.len() && b[i + 2] == '\'' => i += 3, // 'x'
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                out.push(s);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

/// Looks like text a person reads: a capitalised word followed by lowercase letters, or 3+ words.
fn looks_visible(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 3
        || s.starts_with('/')
        || s.contains(":\\")
        || s.contains("::")
        || s.contains("://")
        || (s.contains('_') && !s.contains(' '))
    {
        return false;
    }
    let first = s
        .chars()
        .find(|c| !matches!(c, '{' | '}' | '\\'))
        .unwrap_or(' ');
    let words = s.split_whitespace().count();
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3
        && ((first.is_uppercase() && s.chars().skip(1).any(|c| c.is_lowercase())) || words >= 3)
}

const SKIP_LINE: &[&str] = &[
    "assert",
    "panic!",
    "expect(",
    "unreachable",
    "eprintln!",
    "#[",
    "include_str",
    "id_salt",
    "Id::new",
    "env::var",
    "env!(",
    "debug_assert",
    "i18n-ok",
    "//",
    "println!",
    "memstats::",
];

/// Visible-looking literals per file in non-test code.
fn offenders() -> BTreeMap<String, Vec<(usize, String)>> {
    let mut out = BTreeMap::new();
    for (rel, src) in sources() {
        // Terminal output (usage, --status, diagnostics counters), date tables and screen-reader
        // plumbing are outside the window UI catalogue.
        if matches!(
            rel.as_str(),
            "i18n.rs" | "localtime.rs" | "cli.rs" | "memstats.rs" | "memtrim.rs" | "lib.rs"
        ) || rel.starts_with("bin/")
            || rel.contains("a11y")
            || rel.ends_with("_tests.rs")
            || rel == "test_support.rs"
        {
            continue;
        }
        for (n, line) in non_test(&src).lines().enumerate() {
            if SKIP_LINE.iter().any(|s| line.contains(s)) {
                continue;
            }
            for lit in literals(line) {
                if looks_visible(&lit) {
                    out.entry(rel.clone())
                        .or_insert_with(Vec::new)
                        .push((n + 1, lit));
                }
            }
        }
    }
    out
}

/// Files that still hold English literals, with the most they may hold. Migrating a file means
/// moving its text to `i18n/en.properties` and deleting its line (or lowering the number); adding
/// a user-visible literal anywhere fails `no_new_literals_outside_the_catalogue`.
fn allowlist() -> BTreeMap<String, usize> {
    include_str!("i18n_allowlist.txt")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let (f, n) = l.split_once(' ').expect("`file count` lines");
            (f.to_owned(), n.trim().parse().expect("count"))
        })
        .collect()
}

#[test]
fn no_new_literals_outside_the_catalogue() {
    let found = offenders();
    let allow = allowlist();
    let mut problems = Vec::new();
    for (file, lits) in &found {
        match allow.get(file) {
            None => problems.push(format!(
                "{file}: {} user-visible literal(s) outside the catalogue, first: {:?} (line {}). Move text to i18n/en.properties and use tr()/tr_args()/tr_n(), or mark a technical string with `// i18n-ok`.",
                lits.len(),
                lits[0].1,
                lits[0].0
            )),
            Some(max) if lits.len() > *max => problems.push(format!(
                "{file}: {} literals, allowlist permits {max}; newest: {:?} (line {})",
                lits.len(),
                lits.last().unwrap().1,
                lits.last().unwrap().0
            )),
            Some(max) if lits.len() < *max => problems.push(format!(
                "{file}: only {} literals left, lower its tests/i18n_allowlist.txt entry from {max}",
                lits.len()
            )),
            _ => {}
        }
    }
    for file in allow.keys() {
        if !found.contains_key(file) {
            problems.push(format!(
                "{file}: clean now, delete it from tests/i18n_allowlist.txt"
            ));
        }
    }
    assert!(problems.is_empty(), "\n{}", problems.join("\n"));
}

fn keys_used(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for pat in ["tr(\"", "tr_args(\"", "tr_n(\""] {
        let mut rest = src;
        while let Some(i) = rest.find(pat) {
            let tail = &rest[i + pat.len()..];
            // `push_str("` and friends end in `tr(`; a key call is not preceded by an identifier.
            let ident = rest[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !ident && let Some(j) = tail.find('"') {
                out.insert(tail[..j].to_owned());
            }
            rest = tail;
        }
    }
    out
}

/// Strips a `.lower` or plural suffix so run-time-built keys map to their base.
fn base_key(k: &str) -> &str {
    let k = k.strip_suffix(".lower").unwrap_or(k);
    for s in [".zero", ".one", ".two", ".few", ".many", ".other"] {
        if let Some(b) = k.strip_suffix(s) {
            return b;
        }
    }
    k
}

#[test]
fn every_key_used_in_code_is_in_english() {
    let en = english();
    let bases: BTreeSet<_> = en.keys().map(|k| base_key(k).to_owned()).collect();
    for (file, src) in sources() {
        if file == "i18n.rs" {
            continue;
        }
        for k in keys_used(non_test(&src)) {
            assert!(
                en.contains_key(&k) || bases.contains(&k),
                "{file}: key {k:?} is not in i18n/en.properties"
            );
        }
    }
}

#[test]
fn every_english_key_is_used() {
    let all: String = sources().into_iter().map(|(_, s)| s).collect();
    for k in english().keys() {
        let base = base_key(k);
        assert!(
            all.contains(&format!("\"{k}\"")) || all.contains(&format!("\"{base}\"")),
            "{k} is in en.properties but no code uses it"
        );
    }
}

#[test]
fn translations_use_known_keys_and_the_same_placeholders() {
    fn placeholders(s: &str) -> BTreeSet<String> {
        let s = s.replace("{{", "").replace("}}", "");
        s.split('{')
            .skip(1)
            .filter_map(|t| t.split_once('}'))
            .map(|(n, _)| n.to_owned())
            .collect()
    }
    let en = english();
    for (tag, src) in BUNDLED {
        for (k, v) in i18n::parse_catalogue(src) {
            let known = en
                .get(&k)
                .or_else(|| en.get(&format!("{}.other", base_key(&k))));
            let Some(known) = known else {
                panic!("{tag}: {k} is not an English key");
            };
            // zero/two/few/many forms may drop `{n}` ("one", "two"), so only compare the rest.
            let relaxed = [".zero", ".two", ".few", ".many"]
                .iter()
                .any(|s| k.ends_with(s));
            if !relaxed {
                assert_eq!(placeholders(&v), placeholders(known), "{tag}: {k}");
            }
        }
    }
}

#[test]
fn plural_keys_have_an_other_form() {
    let en = english();
    for k in en.keys() {
        if let Some(b) = k.strip_suffix(".one") {
            assert!(
                en.contains_key(&format!("{b}.other")),
                "{k} needs {b}.other"
            );
        }
    }
}
