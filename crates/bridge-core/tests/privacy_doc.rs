// SPDX-License-Identifier: GPL-3.0-or-later
//! `docs/privacy.md` must list exactly the consent features the code has. Adding a `Feature` without documenting its
//! data flow fails here.
use bridge_core::ipc::Feature;
use std::{collections::BTreeSet, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// First-column backticked names of the table between `<!-- guard:NAME -->` and `<!-- /guard -->`.
fn guarded(doc: &str, name: &str) -> BTreeSet<String> {
    let start = format!("<!-- guard:{name} -->");
    let after = doc
        .split_once(&start)
        .unwrap_or_else(|| panic!("docs/privacy.md lost the marker {start}"))
        .1;
    let body = after
        .split_once("<!-- /guard -->")
        .expect("guard not closed")
        .0;
    body.lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .map(|rest| rest.split('`').next().unwrap().to_owned())
        .collect()
}

fn doc() -> String {
    std::fs::read_to_string(root().join("docs/privacy.md")).expect("docs/privacy.md exists")
}

#[test]
fn privacy_doc_lists_every_consent_feature_and_nothing_else() {
    let in_code: BTreeSet<String> = Feature::ALL
        .iter()
        .map(|f| {
            serde_json::to_value(f)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        guarded(&doc(), "desktop-consent"),
        in_code,
        "docs/privacy.md consent table differs from bridge_core::ipc::Feature::ALL"
    );
}

#[test]
fn every_desktop_feature_variant_is_in_all() {
    // `Feature::ALL` is a hand-kept array: a new variant that is not in it would escape the check above.
    let src = std::fs::read_to_string(root().join("crates/bridge-core/src/ipc.rs")).unwrap();
    let decl = src
        .split("pub enum Feature {")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let variants = decl
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.starts_with("//") && !l.starts_with("#[") && l.ends_with(',') && !l.contains(' ')
        })
        .count();
    assert_eq!(
        variants,
        Feature::ALL.len(),
        "a Feature variant is missing from Feature::ALL"
    );
}

#[test]
fn privacy_doc_lists_the_phone_grants_as_in_code() {
    let permits = std::fs::read_to_string(
        root().join("crates/bridge-android/src/transport/engine/permits.rs"),
    )
    .unwrap();
    let array = permits
        .split("FEATURES: [&str;")
        .nth(1)
        .unwrap()
        .split("];")
        .next()
        .unwrap();
    let in_code: BTreeSet<String> = array
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(!in_code.is_empty());
    assert_eq!(
        guarded(&doc(), "phone-grants"),
        in_code,
        "docs/privacy.md phone-grant table differs from permits.rs FEATURES"
    );
    let kotlin = std::fs::read_to_string(
        root().join("android/app/src/main/java/dev/geekbridge/android/ComputerGrants.kt"),
    )
    .unwrap();
    let body = kotlin
        .split("enum class GrantFeature")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    let kt: BTreeSet<String> = body
        .lines()
        .filter(|l| {
            l.trim_start().starts_with(|c: char| c.is_ascii_uppercase()) && l.contains("(\"")
        })
        .filter_map(|l| l.split('"').nth(1).map(str::to_owned))
        .collect();
    assert_eq!(
        kt, in_code,
        "ComputerGrants.kt GrantFeature differs from permits.rs FEATURES"
    );
}

#[test]
fn privacy_doc_describes_the_call_trace_as_in_code() {
    let doc = doc();
    for needle in [
        "call-trace.log",
        "calls --trace --clear",
        "two digits",
        "128 KiB",
        "256 KiB",
    ] {
        assert!(
            doc.contains(needle),
            "docs/privacy.md lost its call-trace entry ({needle})"
        );
    }
    let src =
        std::fs::read_to_string(root().join("crates/bridge-service/src/call_trace.rs")).unwrap();
    assert!(
        src.contains("MEMORY_CAP: usize = 128 * 1024"),
        "update the cap in docs/privacy.md"
    );
    assert!(
        src.contains("FILE_CAP: usize = 256 * 1024"),
        "update the cap in docs/privacy.md"
    );
    assert!(src.contains("pub const FILE: &str = \"call-trace.log\""));
}

#[test]
fn readme_and_user_guide_link_the_privacy_review() {
    assert!(
        std::fs::read_to_string(root().join("README.md"))
            .unwrap()
            .contains("docs/privacy.md")
    );
    assert!(
        std::fs::read_to_string(root().join("docs/user-guide.md"))
            .unwrap()
            .contains("privacy.md")
    );
}
