// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl whats-new` prints the notes embedded at build time and the release page, without a service.

use std::process::Command;

#[test]
fn whats_new_prints_the_embedded_notes_and_the_release_page() {
    let state = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_geekbridge-ctl"))
        .args(["--state-dir", state.path().to_str().unwrap(), "whats-new"])
        .env_remove("GEEKBRIDGE_UPDATE_REPO")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        text,
        format!(
            "{}Full release notes: https://github.com/fspecii/geekbridge/releases/tag/v{version}\n",
            bridge_core::whats_new::embedded().render()
        )
    );
    assert!(text.starts_with(&format!("Updated to {version}\n")));
}
