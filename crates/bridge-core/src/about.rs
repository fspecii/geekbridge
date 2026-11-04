// SPDX-License-Identifier: GPL-3.0-or-later
//! What the desktop "About GeekBridge" section and `geekbridge-ctl about` / `--version` both show:
//! the version, the build commit and update channel when known, the licence, the independence
//! statement, the project address, and where the licence and third-party notices live.
//!
//! The words come from the i18n catalogue (`about.*`); this module only holds the facts and the
//! lookup of the documents installed next to the program, so the window and the command line
//! cannot drift apart.
use std::path::{Path, PathBuf};

/// SPDX identifier of the project licence.
pub const LICENCE_ID: &str = "GPL-3.0-or-later";
/// The project page and source.
pub const PROJECT_URL: &str = "https://github.com/fspecii/geekbridge";
/// Online copies, used when no installed copy is found.
pub const LICENSE_URL: &str = "https://github.com/fspecii/geekbridge/blob/main/LICENSE";
pub const NOTICES_URL: &str =
    "https://github.com/fspecii/geekbridge/blob/main/THIRD_PARTY_NOTICES.md";
pub const LICENSE_FILE: &str = "LICENSE";
pub const NOTICES_FILE: &str = "THIRD_PARTY_NOTICES.md";

/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A document that is either a local file or, failing that, a web page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocTarget {
    File(PathBuf),
    Url(&'static str),
}

impl DocTarget {
    /// What to hand to the system opener.
    pub fn open_arg(&self) -> String {
        match self {
            DocTarget::File(p) => p.display().to_string(),
            DocTarget::Url(u) => (*u).to_owned(),
        }
    }
}

/// Directories that may hold the documents, best first: next to the binary (a portable copy),
/// the prefix the binary was installed under (`<prefix>/bin/..` then `share/doc/geekbridge`), and
/// the usual system locations (Debian and Arch layouts).
pub fn doc_dirs(exe: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(bin) = exe.and_then(Path::parent) {
        dirs.push(bin.to_path_buf());
        if let Some(prefix) = bin.parent() {
            dirs.push(prefix.join("share/doc/geekbridge"));
            dirs.push(prefix.join("share/licenses/geekbridge"));
        }
    }
    for d in [
        "/usr/share/doc/geekbridge",
        "/usr/share/licenses/geekbridge",
        "/usr/local/share/doc/geekbridge",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// First existing `name` in `dirs`, or the web page.
pub fn find_doc(dirs: &[PathBuf], name: &str, url: &'static str) -> DocTarget {
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .map_or(DocTarget::Url(url), DocTarget::File)
}

/// The licence text for the running program.
pub fn license_target() -> DocTarget {
    let exe = std::env::current_exe().ok();
    find_doc(&doc_dirs(exe.as_deref()), LICENSE_FILE, LICENSE_URL)
}

/// The third-party notices for the running program.
pub fn notices_target() -> DocTarget {
    let exe = std::env::current_exe().ok();
    find_doc(&doc_dirs(exe.as_deref()), NOTICES_FILE, NOTICES_URL)
}

/// The facts of one build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct About {
    pub commit: Option<String>,
    /// `stable` or `dev`; unknown when the service has not been asked.
    pub channel: Option<String>,
}

/// Text with `{name}` placeholders filled: `(key, [(name, value)])`.
pub type Fill<'a> = &'a dyn Fn(&str, &[(&str, &str)]) -> String;

/// The lines shared by the window and the command line. `tr(key)` returns plain text and
/// `fill(key, args)` text with `{name}` placeholders filled.
pub fn lines(about: &About, tr: &dyn Fn(&str) -> String, fill: Fill) -> Vec<String> {
    let mut out = vec![match &about.commit {
        Some(c) => fill(
            "about.version_commit",
            &[("version", VERSION), ("commit", c)],
        ),
        None => fill("about.version", &[("version", VERSION)]),
    }];
    if let Some(ch) = &about.channel {
        out.push(fill("about.channel", &[("channel", ch)]));
    }
    out.push(fill("about.licence", &[("licence", LICENCE_ID)]));
    out.push(tr("about.independent"));
    out.push(fill("about.project", &[("url", PROJECT_URL)]));
    for (key, target) in [
        ("about.notices_at", notices_target()),
        ("about.license_at", license_target()),
    ] {
        out.push(fill(key, &[("where", &target.open_arg())]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(key: &str, args: &[(&str, &str)]) -> String {
        let mut s = key.to_owned();
        for (k, v) in args {
            s.push_str(&format!(" {k}={v}"));
        }
        s
    }

    #[test]
    fn finds_next_to_binary_then_prefix_then_falls_back_to_url() {
        let t = tempfile::tempdir().unwrap();
        let bin = t.path().join("bin");
        let docs = t.path().join("share/doc/geekbridge");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&docs).unwrap();
        let exe = bin.join("geekbridge");
        let dirs = doc_dirs(Some(&exe));
        assert_eq!(dirs[0], bin);
        assert_eq!(
            find_doc(&dirs, "NOPE-not-here.md", NOTICES_URL),
            DocTarget::Url(NOTICES_URL)
        );
        std::fs::write(docs.join("X.md"), "x").unwrap();
        assert_eq!(
            find_doc(&dirs, "X.md", NOTICES_URL),
            DocTarget::File(docs.join("X.md"))
        );
        std::fs::write(bin.join("X.md"), "y").unwrap();
        assert_eq!(
            find_doc(&dirs, "X.md", NOTICES_URL),
            DocTarget::File(bin.join("X.md"))
        );
    }

    #[test]
    fn system_locations_are_searched_last() {
        let dirs = doc_dirs(None);
        assert_eq!(dirs[0], PathBuf::from("/usr/share/doc/geekbridge"));
        assert!(dirs.contains(&PathBuf::from("/usr/share/licenses/geekbridge")));
    }

    #[test]
    fn lines_carry_every_fact() {
        let tr = |k: &str| k.to_owned();
        let l = lines(
            &About {
                commit: Some("abc123def456".into()),
                channel: Some("dev".into()),
            },
            &tr,
            &fill,
        );
        let all = l.join("\n");
        assert!(all.contains(&format!("version={VERSION}")) && all.contains("commit=abc123def456"));
        assert!(all.contains("channel=dev"));
        assert!(all.contains("licence=GPL-3.0-or-later"));
        assert!(all.contains("about.independent"));
        assert!(all.contains(PROJECT_URL));
        assert!(all.contains("about.notices_at") && all.contains("about.license_at"));
    }

    #[test]
    fn unknown_commit_and_channel_are_left_out() {
        let tr = |k: &str| k.to_owned();
        let l = lines(&About::default(), &tr, &fill);
        assert!(l[0].starts_with("about.version version="));
        assert!(!l.iter().any(|s| s.contains("about.channel")));
    }
}
