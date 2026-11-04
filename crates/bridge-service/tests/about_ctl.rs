// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl about` and `--version` print the licence facts without a service.

use std::process::Command;

fn run(args: &[&str]) -> String {
    let state = tempfile::tempdir().unwrap();
    let mut all = vec!["--state-dir", state.path().to_str().unwrap()];
    all.extend_from_slice(args);
    let out = Command::new(env!("CARGO_BIN_EXE_geekbridge-ctl"))
        .args(all)
        .env("GEEKBRIDGE_LANG", "en")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn about_lists_version_licence_independence_project_and_documents() {
    let text = run(&["about"]);
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        text.starts_with(&format!("GeekBridge v{version}")),
        "{text}"
    );
    assert!(text.contains("Licence: GPL-3.0-or-later"));
    assert!(text.contains(
        "Independent implementation compatible with the KDE Connect protocol; not affiliated with KDE."
    ));
    assert!(text.contains("Project: https://github.com/fspecii/geekbridge"));
    assert!(text.contains("Third-party notices: "));
    assert!(text.contains("Licence text: "));
    assert!(!text.contains("about."), "{text}");
}

#[test]
fn version_keeps_its_first_line_and_adds_the_same_facts() {
    let text = run(&["--version"]);
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some(format!("geekbridge-ctl {}", env!("CARGO_PKG_VERSION")).as_str())
    );
    assert_eq!(lines.collect::<Vec<_>>().join("\n") + "\n", run(&["about"]));
}
