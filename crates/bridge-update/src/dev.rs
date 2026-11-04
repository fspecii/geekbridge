// SPDX-License-Identifier: GPL-3.0-or-later
//! The rolling `dev` channel: a GitHub prerelease with the fixed tag `dev` that always carries the
//! newest main build, described by a small `dev-manifest.json` asset (docs/updates.md#dev-channel).
//!
//! ```json
//! {"commit":"<40 hex>","versionCode":20161,"builtAt":"2026-11-04T10:00:00Z","notes":"..."}
//! ```
//!
//! A dev build is "newer" than the running one when its commit differs and its `versionCode`
//! (`DEV_CODE_BASE` + number of commits) is higher. A running build that does not know its own
//! commit count (a stable release, a source archive) is offered the dev build whenever the commit
//! differs, because it cannot tell.

use crate::Error;
use serde::Deserialize;

/// The Git tag of the rolling prerelease.
pub const DEV_TAG: &str = "dev";
/// The manifest asset name.
pub const MANIFEST_ASSET: &str = "dev-manifest.json";
/// `versionCode` of a dev build = this + `git rev-list --count HEAD` (android/app/build.gradle.kts,
/// scripts/version-code.sh --dev).
pub const DEV_CODE_BASE: u64 = 20_000;
pub const MAX_MANIFEST_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevManifest {
    /// Lowercase hex, 7..=40 digits.
    pub commit: String,
    pub version_code: u64,
    pub built_at: String,
    pub notes: String,
}

#[derive(Deserialize)]
struct Raw {
    commit: String,
    #[serde(rename = "versionCode")]
    version_code: u64,
    #[serde(rename = "builtAt", default)]
    built_at: String,
    #[serde(default)]
    notes: String,
}

impl DevManifest {
    pub fn parse(bytes: &[u8]) -> Result<DevManifest, Error> {
        let bad = || Error::Malformed("dev-manifest.json is not understood".into());
        let raw: Raw = serde_json::from_slice(bytes).map_err(|_| bad())?;
        let commit = raw.commit.trim().to_ascii_lowercase();
        if !(7..=40).contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        if raw.version_code <= DEV_CODE_BASE || raw.version_code > 2_000_000_000 {
            return Err(bad());
        }
        let built_at: String = raw
            .built_at
            .chars()
            .filter(|c| !c.is_control() && !crate::release::is_invisible_format(*c))
            .take(40)
            .collect();
        Ok(DevManifest {
            commit,
            version_code: raw.version_code,
            built_at,
            notes: raw.notes,
        })
    }

    /// `dev-<12 hex>`: what the UI and `skip` call this build.
    pub fn label(&self) -> String {
        format!("dev-{}", &self.commit[..self.commit.len().min(12)])
    }

    /// Is this build newer than the one that knows `own_commit` / `own_count` (both optional)?
    pub fn newer_than(&self, own_commit: Option<&str>, own_count: Option<u32>) -> bool {
        if let Some(own) = own_commit
            && same_commit(own, &self.commit)
        {
            return false;
        }
        match own_count {
            Some(n) => self.version_code > DEV_CODE_BASE + u64::from(n),
            None => true,
        }
    }
}

/// Two commit ids denote the same commit when one is a prefix of the other (abbreviated ids).
pub fn same_commit(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    !a.is_empty() && !b.is_empty() && (a.starts_with(&b) || b.starts_with(&a))
}

/// `dev-<7..40 hex>`, as produced by [`DevManifest::label`].
pub fn valid_label(text: &str) -> bool {
    text.strip_prefix("dev-")
        .is_some_and(|h| (7..=40).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn manifest(code: u64) -> DevManifest {
        DevManifest::parse(
            format!(
                r#"{{"commit":"{COMMIT}","versionCode":{code},"builtAt":"2026-11-04T10:00:00Z","notes":"x"}}"#
            )
            .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn parses_a_realistic_manifest_and_labels_it() {
        let m = manifest(20161);
        assert_eq!(m.label(), "dev-0123456789ab");
        assert!(valid_label(&m.label()));
        assert_eq!(m.built_at, "2026-11-04T10:00:00Z");
    }

    #[test]
    fn built_at_loses_direction_overrides() {
        let m = DevManifest::parse(
            br#"{"commit":"0123456789abcdef","versionCode":20161,"builtAt":"2026\u202e-11-04\u200b"}"#,
        )
        .unwrap();
        assert_eq!(m.built_at, "2026-11-04");
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            &b"{}"[..],
            b"[]",
            br#"{"commit":"zzzzzzz","versionCode":20161}"#,
            br#"{"commit":"abc","versionCode":20161}"#,
            br#"{"commit":"0123456","versionCode":5}"#,
            br#"{"commit":"0123456","versionCode":-1}"#,
            br#"{"commit":"0123456","versionCode":99999999999}"#,
        ] {
            assert!(
                DevManifest::parse(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn newer_by_code_and_commit() {
        let m = manifest(20200);
        assert!(m.newer_than(Some("aaaaaaaaaaaa"), Some(150))); // 20150 < 20200
        assert!(!m.newer_than(Some("aaaaaaaaaaaa"), Some(200))); // same code, other commit: not newer
        assert!(!m.newer_than(Some("aaaaaaaaaaaa"), Some(300))); // we are ahead
        assert!(!m.newer_than(Some("0123456789ab"), Some(10))); // same commit, abbreviated
        assert!(m.newer_than(Some("aaaaaaaaaaaa"), None)); // unknown count: differing commit is enough
        assert!(m.newer_than(None, None));
    }

    #[test]
    fn labels() {
        assert!(!valid_label("dev"));
        assert!(!valid_label("dev-xyz1234"));
        assert!(!valid_label("v0.1.0"));
    }
}
