// SPDX-License-Identifier: GPL-3.0-or-later
//! The wl-clipboard backend against a real Wayland compositor. Opt-in: it reads and overwrites the
//! clipboard of whatever `WAYLAND_DISPLAY` points at, so it only runs when `GEEKBRIDGE_TEST_WAYLAND=1`
//! and should be pointed at a throwaway headless compositor:
//!
//! ```text
//! RT=$(mktemp -d); chmod 700 $RT; export XDG_RUNTIME_DIR=$RT
//! WLR_BACKENDS=headless WLR_RENDERER=pixman sway -c /dev/null &   # then WAYLAND_DISPLAY=wayland-1
//! GEEKBRIDGE_TEST_WAYLAND=1 cargo test -p bridge-service --test wayland_clipboard
//! ```
use bridge_core::clipboard::{ObserveMode, ReadAccess, Sensitivity};
use bridge_service::clipboard::{ClipImage, detect_backend};
use std::process::{Command, Stdio};

fn enabled() -> bool {
    std::env::var("GEEKBRIDGE_TEST_WAYLAND").as_deref() == Ok("1")
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn wl_copy(args: &[&str], data: &[u8]) {
    use std::io::Write;
    let mut c = Command::new("wl-copy")
        .args(args)
        .stdin(Stdio::piped())
        // wl-copy leaves a daemon holding the selection; it must not inherit the test's pipes.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(data).unwrap();
    assert!(c.wait().unwrap().success());
}

#[test]
fn text_image_primary_and_events_on_wayland() {
    if !enabled() {
        eprintln!("skipped: set GEEKBRIDGE_TEST_WAYLAND=1 with a throwaway WAYLAND_DISPLAY");
        return;
    }
    let b = detect_backend().expect("wl-clipboard backend");
    assert_eq!(b.name(), "wl-clipboard");
    let caps = b.capabilities();
    assert_eq!(caps.read, ReadAccess::Background);
    assert_eq!(caps.observe, ObserveMode::Event);
    assert!(b.change_events().is_some(), "wl-paste --watch starts");

    // The regular clipboard, not the primary selection, is what is read.
    wl_copy(&["--primary"], b"selected text");
    b.write_text("from the clipboard").unwrap();
    assert_eq!(
        b.read_text(1024).unwrap().as_deref(),
        Some("from the clipboard")
    );

    // Images: both MIME types round-trip, and the magic bytes decide the type.
    let png = b"\x89PNG\r\n\x1a\nbody".to_vec();
    b.write_image(&ClipImage {
        mime: "image/png",
        bytes: png.clone(),
    })
    .unwrap();
    let got = b.read_image(1 << 20).unwrap().expect("png");
    assert_eq!((got.mime, got.bytes), ("image/png", png));
    assert_eq!(b.read_text(1024).unwrap(), None, "an image is not text");
    let jpeg = b"\xFF\xD8\xFF\xE0jpegbody".to_vec();
    b.write_image(&ClipImage {
        mime: "image/jpeg",
        bytes: jpeg.clone(),
    })
    .unwrap();
    assert_eq!(b.read_image(1 << 20).unwrap().unwrap().bytes, jpeg);
    // Too large is refused, not truncated.
    assert!(b.read_image(4).is_err());

    // Password managers mark their copies; those are never sent.
    wl_copy(&["--type", "x-kde-passwordManagerHint"], b"secret");
    assert_eq!(b.sensitivity(), Sensitivity::Sensitive);
    b.write_text("plain").unwrap();
    assert_eq!(b.sensitivity(), Sensitivity::Normal);
}

/// Wrap `wl-paste` in a script that logs every invocation, so the test can count reads.
fn count_wl_paste_calls() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v wl-paste"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls.log");
    let shim = dir.path().join("wl-paste");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            real.trim()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![dir.path().to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    // SAFETY: only this test changes PATH, before it starts any thread that reads it.
    unsafe { std::env::set_var("PATH", std::env::join_paths(dirs).unwrap()) };
    (dir, log)
}

fn calls(log: &std::path::Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |s| s.lines().count())
}

/// The service end to end on a real compositor: event-driven watching (no polling while idle),
/// primary selection ignored, HTML copies still arrive as text, apply-from-phone lands in
/// `wl-paste` with no echo back, and a password-manager copy is never sent.
#[tokio::test]
async fn service_watches_by_event_and_suppresses_its_own_writes() {
    use bridge_core::clipboard_wire::{self, CLIPBOARD};
    use bridge_service::clipboard::{ClipOut, ClipboardService, LinkCaps};
    use std::time::Duration;
    if !enabled() {
        eprintln!("skipped: set GEEKBRIDGE_TEST_WAYLAND=1 with a throwaway WAYLAND_DISPLAY");
        return;
    }
    let (_shim, log) = count_wl_paste_calls();
    let backend = detect_backend().expect("wl-clipboard backend");
    // A 20 ms poll would show up as hundreds of reads in the idle window below.
    let svc = ClipboardService::with_poll(backend, "desktop-1", Duration::from_millis(20));
    let caps = LinkCaps {
        stock: true,
        pull: true,
    };
    wl_copy(&[], b"baseline");
    let phone = "phone-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let mut rx = svc.link_up(phone, "Phone", caps, true);
    let sent = |rx: &mut tokio::sync::mpsc::Receiver<ClipOut>| {
        let mut out = Vec::new();
        while let Ok(ClipOut::Packet { body, .. }) = rx.try_recv() {
            out.push(
                body.get("content")
                    .or_else(|| body.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
            );
        }
        out
    };
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(sent(&mut rx).is_empty(), "the baseline is not a change");

    // Steady state: nothing runs while the clipboard is idle.
    let before = calls(&log);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(calls(&log), before, "idle watcher must not spawn wl-paste");

    // A normal copy: one event, a handful of reads, one packet.
    wl_copy(&[], "κείμενο from desktop".as_bytes());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(sent(&mut rx), vec!["κείμενο from desktop".to_owned()]);
    let reads = calls(&log) - before;
    assert!(reads <= 6, "one change should cost few reads, got {reads}");

    // The primary selection (middle-click paste) is never sent.
    wl_copy(&["--primary"], b"selected only");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(sent(&mut rx).is_empty(), "primary must not be sent");

    // wl-copy offers text/plain next to text/html (as browsers do): the plain text goes out, and
    // the explicit push reads the HTML too.
    wl_copy(&["--type", "text/html"], b"<b>rich</b>");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(sent(&mut rx), vec!["<b>rich</b>".to_owned()]);

    // Password-manager copies are never sent.
    wl_copy(&["--type", "x-kde-passwordManagerHint"], b"secret");
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(sent(&mut rx).is_empty());

    // Apply from the phone: it reaches the real clipboard and is not echoed back.
    svc.on_packet(
        phone,
        CLIPBOARD,
        &clipboard_wire::update_body("from the phone"),
        None,
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let out = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from the phone");
    assert!(sent(&mut rx).is_empty(), "applied text must not echo back");
}

/// Images over the cap are refused (never truncated).
#[test]
fn oversize_image_is_refused_not_truncated_on_wayland() {
    if !enabled() {
        eprintln!("skipped: set GEEKBRIDGE_TEST_WAYLAND=1 with a throwaway WAYLAND_DISPLAY");
        return;
    }
    let b = detect_backend().expect("wl-clipboard backend");
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.resize(3 << 20, 7);
    b.write_image(&ClipImage {
        mime: "image/png",
        bytes: png.clone(),
    })
    .unwrap();
    assert!(b.read_image(1 << 20).is_err(), "3 MiB over a 1 MiB cap");
    assert_eq!(
        b.read_image(4 << 20).unwrap().unwrap().bytes.len(),
        png.len()
    );
}
