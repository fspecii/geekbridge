// SPDX-License-Identifier: GPL-3.0-or-later
//! What the share manager accepts as a file to send, and its caps on concurrent transfers. A refused request
//! must leave no transfer slot behind, and a running transfer cannot be dismissed out from under its link.

use bridge_core::ipc::ErrorCode;
use bridge_service::share::{MAX_ACTIVE, MAX_ACTIVE_PER_DEVICE, ShareManager, ShareOptions};
use std::{path::Path, sync::Arc, time::Duration};

fn manager(dir: &Path, max_file_bytes: u64) -> Arc<ShareManager> {
    ShareManager::new(ShareOptions {
        save_dir: dir.join("downloads"),
        max_file_bytes,
        settings_path: None,
        offer_ttl: Duration::from_secs(3),
        keep_finished: Duration::from_secs(60),
    })
}

const DEV: &str = "dddddddddddddddddddddddddddddddd";

fn code<T: std::fmt::Debug>(r: Result<T, bridge_core::ipc::ErrorBody>) -> ErrorCode {
    r.expect_err("must be refused").code
}

#[test]
fn unsuitable_paths_are_refused_with_a_typed_error_and_no_slot_is_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), 1000);
    let ok = tmp.path().join("ok.bin");
    std::fs::write(&ok, b"hello").unwrap();
    let empty = tmp.path().join("empty.bin");
    std::fs::write(&empty, b"").unwrap();
    let big = tmp.path().join("big.bin");
    std::fs::write(&big, vec![0_u8; 1001]).unwrap();
    let dir = tmp.path().join("a-directory");
    std::fs::create_dir(&dir).unwrap();

    let s = |p: &str| m.begin_send_file(DEV, "Phone", p);
    assert_eq!(code(s("")), ErrorCode::BadRequest);
    assert_eq!(code(s("/tmp/bad\0name")), ErrorCode::BadRequest);
    assert_eq!(
        code(s(&format!("/{}", "a/".repeat(3000)))),
        ErrorCode::BadRequest
    );
    assert_eq!(code(s("relative/path.txt")), ErrorCode::BadRequest);
    assert_eq!(
        code(s(&tmp.path().join("missing").display().to_string())),
        ErrorCode::NotFound
    );
    assert_eq!(code(s(&dir.display().to_string())), ErrorCode::BadRequest);
    assert_eq!(code(s(&empty.display().to_string())), ErrorCode::BadRequest);
    assert_eq!(code(s(&big.display().to_string())), ErrorCode::BadRequest);
    assert!(
        m.status(&[]).transfers.is_empty(),
        "refusals must not register transfers"
    );

    // The boundary itself is accepted.
    let exact = tmp.path().join("exact.bin");
    std::fs::write(&exact, vec![1_u8; 1000]).unwrap();
    assert!(s(&exact.display().to_string()).is_ok());
    assert!(s(&ok.display().to_string()).is_ok());
}

#[cfg(unix)]
#[test]
fn a_fifo_and_a_dangling_symlink_are_not_sendable_files() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), 1000);
    let fifo = tmp.path().join("pipe");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status();
    if status.map(|s| s.success()).unwrap_or(false) {
        // Must refuse without opening (and blocking on) the pipe.
        assert_eq!(
            code(m.begin_send_file(DEV, "Phone", &fifo.display().to_string())),
            ErrorCode::BadRequest
        );
    }
    let dangling = tmp.path().join("dangling");
    symlink(tmp.path().join("nowhere"), &dangling).unwrap();
    assert_eq!(
        code(m.begin_send_file(DEV, "Phone", &dangling.display().to_string())),
        ErrorCode::NotFound
    );
    assert!(m.status(&[]).transfers.is_empty());
}

#[test]
fn concurrent_transfer_caps_hold_per_device_and_overall() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manager(tmp.path(), 1000);
    let f = tmp.path().join("f.bin");
    std::fs::write(&f, b"x").unwrap();
    let path = f.display().to_string();
    let other = |n: char| n.to_string().repeat(32);

    let mut ids = Vec::new();
    for _ in 0..MAX_ACTIVE_PER_DEVICE {
        ids.push(m.begin_send_file(DEV, "Phone", &path).unwrap());
    }
    assert_eq!(
        code(m.begin_send_file(DEV, "Phone", &path)),
        ErrorCode::Busy,
        "per-device cap"
    );
    // Other devices still have room until the global cap.
    let mut started = MAX_ACTIVE_PER_DEVICE;
    let mut n = 0;
    while started < MAX_ACTIVE {
        ids.push(
            m.begin_send_file(&other((b'a' + n) as char), "Other", &path)
                .unwrap(),
        );
        started += 1;
        n += 1;
    }
    assert_eq!(
        code(m.begin_send_file(&other('z'), "Other", &path)),
        ErrorCode::Busy,
        "global cap"
    );

    // A transfer that is still live cannot be dismissed; an unknown id is NotFound.
    assert_eq!(code(m.dismiss(ids[0])), ErrorCode::Busy);
    assert_eq!(code(m.dismiss(9_999)), ErrorCode::NotFound);
    assert_eq!(m.status(&[]).transfers.len(), MAX_ACTIVE);
}
