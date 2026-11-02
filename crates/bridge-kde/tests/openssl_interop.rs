// SPDX-License-Identifier: GPL-3.0-or-later
//! Drives the real `geekbridge-pair` binary against an independent OpenSSL-backed peer
//! (Python `ssl`). Opt-in because it needs `python3` and the `openssl` CLI:
//!
//! `cargo test -p bridge-kde --test openssl_interop -- --ignored`

use std::process::Command;

#[test]
#[ignore = "needs python3 and the openssl CLI; run with --ignored"]
fn cli_against_openssl_peer() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/interop/openssl_peer_e2e.py"
    );
    let out = Command::new("python3")
        .args(["-I", script, env!("CARGO_BIN_EXE_geekbridge-pair")])
        .output()
        .expect("python3 is required for this test");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "interop script failed:\n{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("ALL OK"), "{text}");
}
