// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the source commit and the commit count, so the `dev` update channel can tell whether the
//! rolling dev build is newer than the running binary. Both are optional: a build from a source
//! archive (no `.git`) simply has neither. Set `GEEKBRIDGE_BUILD_COMMIT` / `GEEKBRIDGE_BUILD_COUNT`
//! to override (scripts/release-dev.sh does, because it builds from a copy without `.git`).
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GEEKBRIDGE_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=GEEKBRIDGE_BUILD_COUNT");
    // A new commit moves HEAD (or the branch ref it names).
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    let commit = std::env::var("GEEKBRIDGE_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "--verify", "HEAD"]))
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| (7..=40).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|c| c[..c.len().min(12)].to_owned());
    let count = std::env::var("GEEKBRIDGE_BUILD_COUNT")
        .ok()
        .or_else(|| git(&["rev-list", "--count", "HEAD"]))
        .and_then(|c| c.trim().parse::<u32>().ok());
    if let Some(commit) = commit {
        println!("cargo:rustc-env=GEEKBRIDGE_BUILD_COMMIT={commit}");
    }
    if let Some(count) = count {
        println!("cargo:rustc-env=GEEKBRIDGE_BUILD_COUNT={count}");
    }
}
