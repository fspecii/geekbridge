// SPDX-License-Identifier: GPL-3.0-or-later
//! `scripts/gen-whats-new.sh`: section extraction from CHANGELOG.md, sanitising, and "the checked-in
//! generated files are not stale". Unix only (the script is bash + awk; releases are cut on Linux).
#![cfg(unix)]

use std::{path::PathBuf, process::Command};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(args: &[&str]) -> (bool, String, String) {
    let out = Command::new("bash")
        .arg(root().join("scripts/gen-whats-new.sh"))
        .args(args)
        .current_dir(root())
        .output()
        .expect("bash is available");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn extract(changelog: &str, version: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("CHANGELOG.md");
    std::fs::write(&f, changelog).unwrap();
    let (ok, out, err) = run(&[
        "--stdout",
        "--changelog",
        f.to_str().unwrap(),
        "--version",
        version,
    ]);
    assert!(ok, "{err}");
    out
}

const LOG: &str = "# Changelog

## [Unreleased]

### Added
- Not yet released.

## [0.3.0-rc.1] - 2026-12-01

### Added
- Release candidate feature.

## [0.3.0] - 2026-12-05

### Added
- **Tray icon** with a menu: Open, Pause sync and Quit. More text follows here.
- See [the guide](docs/pairing.md) and ![pic](x.png) for `ctl pair`.
  A continuation line joins the bullet, and the first sentence wins.

### Changed
- Faster discovery

### Known limitations
- Not a highlight.

## [0.2.0] - 2026-11-20

### Fixed
- Old fix.
";

#[test]
fn a_prerelease_heading_is_matched_exactly() {
    assert_eq!(
        extract(LOG, "0.3.0-rc.1"),
        "version: 0.3.0-rc.1\n- Release candidate feature\n"
    );
}

#[test]
fn the_final_section_is_cleaned_and_skips_limitations_and_other_versions() {
    assert_eq!(
        extract(LOG, "0.3.0"),
        "version: 0.3.0
- Tray icon with a menu: Open, Pause sync and Quit
- See the guide and pic for ctl pair
- Faster discovery
"
    );
}

#[test]
fn a_missing_section_falls_back_to_the_release_notes_line() {
    assert_eq!(
        extract(
            &std::fs::read_to_string(root().join("CHANGELOG.md")).unwrap(),
            "9.9.9"
        ),
        "version: 9.9.9\n- See the release notes\n"
    );
    // Unreleased is not a version of any build.
    assert_eq!(
        extract(LOG, "0.4.0"),
        "version: 0.4.0\n- See the release notes\n"
    );
    // A section with no bullets also falls back.
    assert_eq!(
        extract("## [1.0.0] - x\n\n### Added\n\n", "1.0.0"),
        "version: 1.0.0\n- See the release notes\n"
    );
}

#[test]
fn hostile_text_becomes_short_plain_lines() {
    let long = "word ".repeat(80);
    let log = format!(
        "## [1.0.0] - x\n\n### Added\n- # Head > quote | cell <script>alert(1)</script> \\ \u{7}bell\u{1b}[31m red\n- {long}\n- {}\n- 4\n- 5\n- 6\n- 7\n",
        "x".repeat(500)
    );
    let out = extract(&log, "1.0.0");
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(lines.len(), 5, "{out}");
    assert_eq!(lines[0], "- Head quote cell alert(1) bell 31m red");
    for l in &lines {
        assert!(l.chars().count() <= 142, "{l}");
        assert!(
            !l.chars()
                .skip(2)
                .any(|c| c.is_control() || "`*_<>|\\#".contains(c)),
            "{l}"
        );
    }
    assert!(lines[1].ends_with('…'), "{}", lines[1]);
}

#[test]
fn the_checked_in_files_are_up_to_date() {
    let (ok, _, err) = run(&["--check"]);
    assert!(ok, "generated what's-new files are stale: {err}");
}
