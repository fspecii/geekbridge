// SPDX-License-Identifier: GPL-3.0-or-later
//! "What's new": the few lines shown once after an update (the AFTER view; the updater's release
//! notes are the BEFORE view and are unrelated).
//!
//! The text is generated at release time from `CHANGELOG.md` by `scripts/gen-whats-new.sh` into
//! `assets/whats_new.txt` and embedded here, so no network or file lookup is needed. This module
//! parses and re-sanitises it (plain text only, hard size limits), decides from the stored
//! "last seen" version whether to show it, and persists that version in `whats-new.json` in the
//! service state directory (a versioned document, docs/state-formats.md).

use crate::statefile::{Corrupt, PlainIo, Spec, StateFile};
use serde_json::Value;
use std::path::Path;

/// The generated file. `assets/whats_new.txt` must stay under [`MAX_EMBEDDED_BYTES`] (a test pins it).
const EMBEDDED: &str = include_str!("../assets/whats_new.txt");
pub const MAX_EMBEDDED_BYTES: usize = 4096;
pub const MAX_HIGHLIGHTS: usize = 6;
pub const MAX_LINE_CHARS: usize = 140;
/// Shown when the changelog has no section for this version.
pub const FALLBACK: &str = "See the release notes";
const STATE_FILE: &str = "whats-new.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notes {
    pub version: String,
    pub highlights: Vec<String>,
}

impl Notes {
    /// Plain text for the CLI: a title line, then one bullet per highlight.
    pub fn render(&self) -> String {
        let mut out = format!("Updated to {}\n", self.version);
        for h in &self.highlights {
            out.push_str("  - ");
            out.push_str(h);
            out.push('\n');
        }
        out
    }
}

/// The notes of this build.
pub fn embedded() -> Notes {
    parse(EMBEDDED, env!("CARGO_PKG_VERSION"))
}

/// Parses the generated format (`version: X` then `- text` lines). Anything unexpected is dropped,
/// every line is sanitised again, and an empty result becomes the [`FALLBACK`] line. The version
/// is always `build_version`: the notes can never claim to be for another release.
pub fn parse(text: &str, build_version: &str) -> Notes {
    let mut highlights: Vec<String> = text
        .lines()
        .take(64)
        .filter_map(|l| l.strip_prefix("- "))
        .map(sanitise_line)
        .filter(|l| !l.is_empty())
        .take(MAX_HIGHLIGHTS)
        .collect();
    if highlights.is_empty() {
        highlights.push(FALLBACK.to_owned());
    }
    Notes {
        version: build_version.to_owned(),
        highlights,
    }
}

/// One line of plain text: control characters and markdown punctuation removed, whitespace
/// collapsed, at most [`MAX_LINE_CHARS`] characters (cut with an ellipsis).
pub fn sanitise_line(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len().min(512));
    let mut in_tag = false;
    let raw: String = raw.chars().take(4096).collect();
    for c in strip_links(&raw).chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '`' | '*' | '_' | '~' | '#' | '>' | '|' | '\\' | '[' | ']' => {}
            // Bidi overrides and zero-width characters can disguise text; they are dropped.
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => {}
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => s.push(' '),
            c => s.push(c),
        }
    }
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_LINE_CHARS {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(MAX_LINE_CHARS - 1).collect();
    if let Some(i) = cut.rfind(' ') {
        cut.truncate(i);
    }
    cut.push('…');
    cut
}

/// `[text](url)` and `![alt](url)` become `text`; a bracket without a following `(...)` is left alone.
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find(']').and_then(|close| {
            let tail = after[close + 1..].strip_prefix('(')?;
            let end = tail.find(')')?;
            Some((&after[..close], close + 1 + 1 + end + 1))
        });
        match link {
            Some((text, used)) => {
                out.push_str(rest[..open].trim_end_matches('!'));
                out.push_str(text);
                rest = &after[used..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The release page of `version` on the configured releases repository (`GEEKBRIDGE_UPDATE_REPO`, else
/// the default), the same repository the updater and "Report a problem" use.
pub fn release_url(version: &str) -> String {
    format!(
        "https://github.com/{}/releases/tag/v{}",
        crate::report::repo_from_env(),
        version.trim_start_matches('v')
    )
}

// ------------------------------------------------------------------ version change detection

/// What to do at startup, given the stored "last seen" version and the running one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Newer than last time: show the card, remember the version when it is dismissed.
    Show,
    /// Nothing to show; record the running version now (a fresh install, a downgrade, damaged or
    /// unreadable state).
    Record,
    /// Same version: nothing to do.
    Same,
}

/// `last_seen`: the stored version (`None` when there is no state or it cannot be read).
/// `had_prior_use`: the program was used before this file existed, so a missing value means "updated
/// from a build that did not record it", not a fresh install.
pub fn decide(last_seen: Option<&str>, current: &str, had_prior_use: bool) -> Decision {
    let Some(last) = last_seen else {
        return if had_prior_use && key(current).is_some() {
            Decision::Show
        } else {
            Decision::Record
        };
    };
    match (key(last), key(current)) {
        (Some(a), Some(b)) if a < b => Decision::Show,
        (Some(a), Some(b)) if a == b => Decision::Same,
        // Downgrade, or a value that is not a version (damaged): repair silently.
        _ => Decision::Record,
    }
}

/// Orderable form of a semver string; `None` when it is not one.
type VersionKey = (u64, u64, u64, bool, Vec<(bool, u64, String)>);

fn key(v: &str) -> Option<VersionKey> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut it = core.split('.');
    let n = |s: Option<&str>| s?.parse::<u64>().ok();
    let (a, b, c) = (n(it.next())?, n(it.next())?, n(it.next())?);
    if it.next().is_some() {
        return None;
    }
    let ids = match pre {
        None => vec![],
        Some(p) => p
            .split('.')
            .map(|id| match id.parse::<u64>() {
                Ok(n) => (false, n, String::new()),
                Err(_) => (true, 0, id.to_owned()),
            })
            .collect(),
    };
    // A final release sorts after any prerelease of the same version.
    Some((a, b, c, pre.is_none(), ids))
}

// ------------------------------------------------------------------ persisted "last seen"

/// `whats-new.json`: `{"version": 1, "last_seen": "X.Y.Z"}`. It only suppresses a card, so a damaged
/// file without backup starts empty (and the card is not shown).
pub static STATE_SPEC: Spec = Spec {
    name: STATE_FILE,
    current: 1,
    migrations: &[],
    validate: None,
    on_corrupt: Corrupt::Reset,
};

fn file(dir: &Path) -> StateFile<PlainIo> {
    StateFile::new(PlainIo, dir.join(STATE_FILE), &STATE_SPEC)
}

/// The stored version: `None` when there is no file; an empty string when the file is damaged,
/// newer, or holds something that is not a short string (all of which [`decide`] repairs silently).
pub fn last_seen(dir: &Path) -> Option<String> {
    let Ok(loaded) = file(dir).load() else {
        return Some(String::new());
    };
    use crate::statefile::Status;
    match (&loaded.status, loaded.body.get("last_seen")) {
        (Status::Missing, _) => None,
        (Status::Current, Some(Value::String(s))) if s.len() <= 64 => Some(s.clone()),
        _ => Some(String::new()),
    }
}

/// Remembers `version` as seen. Errors (read-only newer file, IO) are returned for the caller to ignore.
pub fn mark_seen(dir: &Path, version: &str) -> Result<(), String> {
    file(dir)
        .update(|body| {
            body.insert("last_seen".into(), Value::String(version.to_owned()));
            true
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Startup: decide, and record silently when that is all there is to do. Returns the notes to show.
/// `had_prior_use` is evaluated by the caller (for the Home app: a paired-device record exists).
pub fn check(dir: &Path, current: &str, had_prior_use: bool) -> Option<Notes> {
    let seen = last_seen(dir);
    match decide(seen.as_deref(), current, had_prior_use) {
        Decision::Show => Some(parse(EMBEDDED, current)),
        Decision::Record => {
            let _ = mark_seen(dir, current);
            None
        }
        Decision::Same => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_file_is_small_and_matches_this_build() {
        assert!(EMBEDDED.len() <= MAX_EMBEDDED_BYTES, "{}", EMBEDDED.len());
        let first = EMBEDDED.lines().next().unwrap();
        assert_eq!(
            first,
            format!("version: {}", env!("CARGO_PKG_VERSION")),
            "run scripts/gen-whats-new.sh"
        );
        let n = embedded();
        assert!((1..=MAX_HIGHLIGHTS).contains(&n.highlights.len()));
        assert!(n.highlights.iter().all(|h| !h.is_empty()));
    }

    #[test]
    fn detection_covers_fresh_upgrade_downgrade_same_and_damage() {
        // fresh install: no state, no prior use -> record silently
        assert_eq!(decide(None, "0.2.0", false), Decision::Record);
        // updated from a build that never recorded a version
        assert_eq!(decide(None, "0.2.0", true), Decision::Show);
        assert_eq!(decide(Some("0.1.0"), "0.2.0", true), Decision::Show);
        assert_eq!(decide(Some("0.2.0"), "0.2.0", true), Decision::Same);
        assert_eq!(decide(Some("0.3.0"), "0.2.0", true), Decision::Record);
        assert_eq!(decide(Some("0.2.0-rc.1"), "0.2.0", true), Decision::Show);
        assert_eq!(decide(Some("0.2.0"), "0.2.0-rc.1", true), Decision::Record);
        assert_eq!(decide(Some("0.9.0"), "0.10.0", true), Decision::Show);
        assert_eq!(decide(Some("garbage"), "0.2.0", true), Decision::Record);
        assert_eq!(decide(Some(""), "0.2.0", true), Decision::Record);
    }

    #[test]
    fn state_round_trips_and_survives_damage() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        // fresh install: nothing shown, version recorded
        assert!(check(p, "0.1.0", false).is_none());
        assert_eq!(last_seen(p).as_deref(), Some("0.1.0"));
        assert!(check(p, "0.1.0", false).is_none());
        // upgrade: shown until marked
        assert!(check(p, "0.2.0", false).is_some());
        assert!(check(p, "0.2.0", false).is_some());
        mark_seen(p, "0.2.0").unwrap();
        assert!(check(p, "0.2.0", false).is_none());
        // downgrade is silent and re-arms the card for the next upgrade
        assert!(check(p, "0.1.0", false).is_none());
        assert_eq!(last_seen(p).as_deref(), Some("0.1.0"));
        assert!(check(p, "0.2.0", false).is_some());
        // corrupt: no card, and the file is repaired
        std::fs::write(p.join(STATE_FILE), b"{not json").unwrap();
        std::fs::remove_file(p.join("whats-new.json.bak")).ok();
        assert!(check(p, "0.2.0", true).is_none());
        assert_eq!(last_seen(p).as_deref(), Some("0.2.0"));
        // a wrongly typed value is also just ignored
        std::fs::write(p.join(STATE_FILE), br#"{"version":1,"last_seen":7}"#).unwrap();
        assert!(check(p, "0.2.0", false).is_none());
    }

    #[test]
    fn a_newer_state_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let raw = br#"{"version":9,"last_seen":"9.9.9","extra":true}"#;
        std::fs::write(dir.path().join(STATE_FILE), raw).unwrap();
        assert!(check(dir.path(), "0.2.0", true).is_none());
        assert_eq!(std::fs::read(dir.path().join(STATE_FILE)).unwrap(), raw);
    }

    #[test]
    fn unknown_fields_are_kept_when_the_version_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(STATE_FILE),
            br#"{"version":1,"last_seen":"0.1.0","extra":"keep"}"#,
        )
        .unwrap();
        mark_seen(dir.path(), "0.2.0").unwrap();
        let text = std::fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(
            text.contains("\"extra\"") && text.contains("0.2.0"),
            "{text}"
        );
    }

    #[test]
    fn sanitising_removes_markdown_controls_and_long_lines() {
        assert_eq!(
            sanitise_line(
                "**Bold** `code` [link](https://x) ![img](y.png) <b>tag</b>\u{7}\u{1b}[31m ok\t\n"
            ),
            "Bold code link img tag 31m ok"
        );
        assert_eq!(
            sanitise_line("# head > quote | cell \\ esc"),
            "head quote cell esc"
        );
        assert_eq!(sanitise_line("a\u{202e}b\u{200b}c"), "abc");
        let long = "word ".repeat(100);
        let cut = sanitise_line(&long);
        assert!(
            cut.chars().count() <= MAX_LINE_CHARS && cut.ends_with('…'),
            "{cut}"
        );
        let one_word = "x".repeat(1000);
        assert!(sanitise_line(&one_word).chars().count() <= MAX_LINE_CHARS);
    }

    #[test]
    fn parse_limits_lines_and_falls_back() {
        let many: String = (0..20).map(|i| format!("- item {i}\n")).collect();
        assert_eq!(parse(&many, "1.0.0").highlights.len(), MAX_HIGHLIGHTS);
        assert_eq!(parse("", "1.0.0").highlights, vec![FALLBACK.to_owned()]);
        assert_eq!(
            parse("version: 9.9.9\n- ok\nnot a bullet\n-\n- \n", "1.2.3"),
            Notes {
                version: "1.2.3".into(),
                highlights: vec!["ok".into()]
            }
        );
        assert_eq!(
            embedded().render().lines().next().unwrap(),
            format!("Updated to {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
