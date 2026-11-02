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
