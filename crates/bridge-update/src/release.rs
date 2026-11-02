// SPDX-License-Identifier: GPL-3.0-or-later
//! GitHub release JSON, platform asset selection and checksum-file parsing.

use crate::{
    Error,
    http::{Policy, Url},
    version::Version,
};
use serde::{Deserialize, Serialize};

/// Upper bound for a desktop installer we are willing to download.
pub const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_NOTES_CHARS: usize = 4000;

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    /// The API URL of the asset (`/releases/assets/<id>`), used with a token for private repos.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub size: u64,
}

impl Release {
    pub fn parse(bytes: &[u8]) -> Result<Release, Error> {
        let release: Release = serde_json::from_slice(bytes)
            .map_err(|_| Error::Malformed("release JSON is not understood".into()))?;
        if release.tag_name.is_empty() || release.tag_name.len() > 64 {
            return Err(Error::Malformed("release has no usable tag".into()));
        }
        if release.assets.len() > 200 {
            return Err(Error::Malformed("release lists too many assets".into()));
        }
        Ok(release)
    }

    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

impl Platform {
    pub fn current() -> Platform {
        Platform {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

fn os_words(os: &str) -> &'static [&'static str] {
    match os {
        "linux" => &["linux"],
        "windows" => &["windows", "win64"],
        "macos" => &["macos", "darwin", "osx"],
        _ => &[],
    }
}

fn arch_words(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["x86_64", "amd64", "x64"],
        "aarch64" => &["aarch64", "arm64"],
        _ => &[],
    }
}

/// Installer extensions we recognise, best first.
fn ext_rank(os: &str, lower: &str) -> Option<usize> {
    let list: &[&str] = match os {
        "linux" => &[".tar.gz", ".tgz", ".appimage"],
        "windows" => &[".msi", ".zip", ".exe"],
        "macos" => &[".dmg", ".zip", ".pkg"],
        _ => &[],
    };
    list.iter().position(|ext| lower.ends_with(ext))
}

/// Picks the desktop asset for `platform`: its name must carry both an OS and an architecture word
/// and a known archive/installer extension. Android packages, checksum files and anything for
/// another platform are never chosen.
pub fn select_asset<'a>(assets: &'a [Asset], platform: &Platform) -> Option<&'a Asset> {
    let os = os_words(&platform.os);
    let arch = arch_words(&platform.arch);
    assets
        .iter()
        .filter(|a| a.size > 0 && a.size <= MAX_ASSET_BYTES && safe_file_name(&a.name))
        .filter_map(|a| {
            let lower = a.name.to_ascii_lowercase();
            if lower.contains("android") || lower.ends_with(".apk") || lower.contains("sha256") {
                return None;
            }
            let has_os = os.iter().any(|w| lower.contains(w));
            let has_arch = arch.iter().any(|w| lower.contains(w))
                || (platform.os == "macos" && lower.contains("universal"));
            if !(has_os && has_arch) {
                return None;
            }
            Some((ext_rank(&platform.os, &lower)?, a))
        })
        // Same extension: the shortest name is the canonical `geekbridge-<os>-<arch>.<ext>`; a longer
        // one (`geekbridge-0.1.1-linux-x86_64-installer.tar.gz`, `…-debug…`) is an extra the release
        // also carries and must not win on alphabetical luck (a digit sorts before a letter).
        .min_by(|(ra, a), (rb, b)| {
            ra.cmp(rb)
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.cmp(&b.name))
        })
        .map(|(_, a)| a)
}

/// A name that is safe to use as a single path component on every platform we ship on.
pub fn safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
        && !name.contains("..")
}

/// A release URL we may fetch or show: https GitHub hosts, or the one configured test origin.
pub fn url_allowed(policy: &Policy, url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| policy.allows(&u))
}

/// Name of the checksum file that covers every asset.
pub fn find_sums_asset(assets: &[Asset]) -> Option<&Asset> {
    assets.iter().find(|a| {
        let n = a.name.to_ascii_lowercase();
        n == "sha256sums" || n == "sha256sums.txt"
    })
}

pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Looks `name` up in a `sha256sum`-style listing (`<hex>  <name>` or `<hex> *<name>`). Returns
/// the lowercase digest only when every line for that name agrees.
pub fn find_checksum(listing: &str, name: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in listing.lines() {
        let line = line.trim();
        let Some((digest, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim().trim_start_matches('*');
        let rest = rest.strip_prefix("./").unwrap_or(rest);
        if rest != name || !is_hex64(digest) {
            continue;
        }
        let digest = digest.to_ascii_lowercase();
        match &found {
            Some(prev) if *prev != digest => return None,
            _ => found = Some(digest),
        }
    }
    found
}

/// Release notes for display: control characters dropped, length bounded.
pub fn clean_notes(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .map(|c| if c == '\t' { ' ' } else { c })
        .collect();
    let mut out: String = cleaned.trim().chars().take(MAX_NOTES_CHARS).collect();
    if cleaned.trim().chars().count() > MAX_NOTES_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, size: u64) -> Asset {
        Asset {
            name: name.into(),
            browser_download_url: format!("https://github.com/x/y/releases/download/v1/{name}"),
            url: None,
            size,
        }
    }

    fn assets() -> Vec<Asset> {
        [
            "geekbridge-android.apk",
            "SHA256SUMS",
            "geekbridge-linux-x86_64.tar.gz",
            "geekbridge-linux-aarch64.tar.gz",
            "geekbridge-windows-x86_64.zip",
            "geekbridge-macos-aarch64.zip",
            "geekbridge-macos-x86_64.zip",
            "geekbridge-linux-x86_64.tar.gz.sha256",
        ]
        .iter()
        .map(|n| asset(n, 1000))
        .collect()
    }

    fn pick(os: &str, arch: &str) -> Option<String> {
        let p = Platform {
            os: os.into(),
            arch: arch.into(),
        };
        select_asset(&assets(), &p).map(|a| a.name.clone())
    }

    #[test]
    fn selects_per_platform() {
        assert_eq!(
            pick("linux", "x86_64").as_deref(),
            Some("geekbridge-linux-x86_64.tar.gz")
        );
        assert_eq!(
            pick("linux", "aarch64").as_deref(),
            Some("geekbridge-linux-aarch64.tar.gz")
        );
        assert_eq!(
            pick("windows", "x86_64").as_deref(),
            Some("geekbridge-windows-x86_64.zip")
        );
        assert_eq!(
            pick("macos", "aarch64").as_deref(),
            Some("geekbridge-macos-aarch64.zip")
        );
        assert_eq!(pick("windows", "aarch64"), None);
        assert_eq!(pick("freebsd", "x86_64"), None);
    }

    #[test]
    fn never_selects_apk_checksums_or_oversize_or_unsafe_names() {
        let p = Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let only = vec![
            asset("geekbridge-android-linux-x86_64.apk", 10),
            asset("geekbridge-linux-x86_64.tar.gz.sha256", 10),
            asset("geekbridge-linux-x86_64.tar.gz", MAX_ASSET_BYTES + 1),
            asset("geekbridge-linux-x86_64.tar.gz", 0),
            asset("../geekbridge-linux-x86_64.tar.gz", 10),
        ];
        assert!(select_asset(&only, &p).is_none());
    }

    #[test]
    fn prefers_better_extension() {
        let p = Platform {
            os: "macos".into(),
            arch: "aarch64".into(),
        };
        let list = vec![
            asset("geekbridge-macos-arm64.zip", 5),
            asset("geekbridge-macos-arm64.dmg", 5),
            asset("geekbridge-macos-universal.zip", 5),
        ];
        assert_eq!(
            select_asset(&list, &p).unwrap().name,
            "geekbridge-macos-arm64.dmg"
        );
    }

    #[test]
    fn real_release_asset_set_picks_the_canonical_archive() {
        // Exactly what release.yml publishes for a Linux build (package-linux.sh adds the installer
        // tarball and the .deb next to geekbridge-linux-x86_64.tar.gz).
        let list: Vec<Asset> = [
            "geekbridge-0.1.1-linux-x86_64-installer.tar.gz",
            "geekbridge-android.apk",
            "geekbridge-linux-x86_64.tar.gz",
            "geekbridge_0.1.1_amd64.deb",
            "SHA256SUMS",
        ]
        .iter()
        .map(|n| asset(n, 5))
        .collect();
        let p = Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(
            select_asset(&list, &p).unwrap().name,
            "geekbridge-linux-x86_64.tar.gz"
        );
    }

    #[test]
    fn checksum_lookup() {
        let h1 = "a".repeat(64);
        let h2 = "B".repeat(64);
        let listing = format!("{h1}  one.zip\n{h2} *two.zip\n{h1}  ./three.zip\nnot a line\n");
        assert_eq!(find_checksum(&listing, "one.zip"), Some(h1.clone()));
        assert_eq!(find_checksum(&listing, "two.zip"), Some("b".repeat(64)));
        assert_eq!(find_checksum(&listing, "three.zip"), Some(h1.clone()));
        assert_eq!(find_checksum(&listing, "four.zip"), None);
        // Conflicting digests for one name are ambiguous, hence unusable.
        let conflict = format!("{h1}  x.zip\n{}  x.zip\n", "c".repeat(64));
        assert_eq!(find_checksum(&conflict, "x.zip"), None);
        // Short or non-hex digests are ignored.
        assert_eq!(find_checksum("abc  x.zip", "x.zip"), None);
        assert_eq!(
            find_checksum(&format!("{}  x.zip", "g".repeat(64)), "x.zip"),
            None
        );
    }

    #[test]
    fn rejects_bad_release_json() {
        assert!(Release::parse(b"not json").is_err());
        assert!(Release::parse(b"{}").is_err());
        assert!(Release::parse(br#"{"tag_name":""}"#).is_err());
        assert!(Release::parse(br#"{"tag_name":"v1.0.0"}"#).is_ok());
    }

    #[test]
    fn notes_are_bounded_and_clean() {
        let n = clean_notes(&format!("hi\u{1b}[31m\r\nthere{}", "x".repeat(10_000)));
        assert!(n.starts_with("hi[31m\nthere"));
        assert!(n.chars().count() <= MAX_NOTES_CHARS + 1);
        assert!(n.ends_with('…'));
    }
}
