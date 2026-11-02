// SPDX-License-Identifier: GPL-3.0-or-later
//! The OS-tool clipboard backends (wl-clipboard, xclip) and the service on top of them, driven through shell stubs on a
//! private PATH instead of a compositor: the stubs record their argv and print canned output, so missing tools, crashes,
//! hangs, huge or binary output, non-UTF-8 text, image MIME negotiation, password-manager hints, the change-event helper
//! dying, and loop suppression are all deterministic. Every test takes `ENV` because PATH is process-wide.
#![cfg(unix)]

use bridge_core::{
    clipboard::{ObserveMode, ReadAccess, Sensitivity},
    clipboard_wire::{self, CLIPBOARD},
};
use bridge_service::clipboard::{
    BackendError, ClipImage, ClipOut, ClipboardBackend, ClipboardService, LinkCaps, detect_backend,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;

static ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Rig {
    dir: tempfile::TempDir,
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nbody";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0jpegbody";

impl Rig {
    fn d(&self) -> &Path {
        self.dir.path()
    }
    fn put(&self, name: &str, data: impl AsRef<[u8]>) {
        fs::write(self.d().join(name), data).unwrap();
    }
    fn rm(&self, name: &str) {
        let _ = fs::remove_file(self.d().join(name));
    }
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        fs::read(self.d().join(name)).ok()
    }
    fn log(&self) -> String {
        String::from_utf8_lossy(&self.get("log").unwrap_or_default()).into_owned()
    }
    fn stub(&self, name: &str, body: &str) {
        let bin = self.d().join("bin");
        let tmp = bin.join(format!(".{name}.new"));
        fs::write(
            &tmp,
            format!(
                "#!/bin/sh\nPATH=/usr/bin:/bin\nD='{}'\n{body}\n",
                self.d().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, bin.join(name)).unwrap();
    }
    fn unstub(&self, name: &str) {
        let _ = fs::remove_file(self.d().join("bin").join(name));
    }
    fn wayland(&self) {
        self.stub(
            "wl-paste",
            r#"echo "$*" >> $D/log
if [ "$1" = --watch ]; then
  [ -f $D/watch_die ] && exit 1
  echo
  while [ ! -f $D/watch_stop ]; do
    sleep 0.05
    [ -f $D/fire ] && rm -f $D/fire && echo
  done
  exit 0
fi
if [ "$1" = --list-types ]; then [ -f $D/types_fail ] && exit 1; cat $D/types; exit 0; fi
case $(cat $D/mode 2>/dev/null) in
  crash) exit 134;;
  hang) exec sleep 30;;
  huge) exec head -c 20000000 /dev/zero;;
  nonutf8) printf '\377\376abc'; exit 0;;
esac
for a; do last=$a; done
case $last in text) f=$D/text;; *) f=$D/$(echo "$last" | tr / _);; esac
[ -f $f ] || exit 1
cat $f"#,
        );
        self.stub(
            "wl-copy",
            r#"echo "copy $*" >> $D/log
[ -f $D/copy_fail ] && exit 1
if [ "$1" = --type ]; then cat > $D/$(echo "$2" | tr / _); else cat > $D/text; fi"#,
        );
        self.put("types", "text/plain\ntext/plain;charset=utf-8\n");
        self.put("text", "hello");
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-stub");
            std::env::remove_var("DISPLAY");
        }
    }
    fn xclip(&self) {
        self.stub(
            "xclip",
            r#"echo "$*" >> $D/log
case " $* " in
  *" -o "*)
    for a; do last=$a; done
    [ "$last" = TARGETS ] && { [ -f $D/types_fail ] && exit 1; cat $D/types; exit 0; }
    case $(cat $D/mode 2>/dev/null) in crash) exit 1;; esac
    f=$D/xc_$(echo "$last" | tr / _)
    [ -f $f ] || exit 1
    cat $f;;
  *)
    [ -f $D/copy_fail ] && exit 1
    t=text
    case " $* " in *" -t "*) t=$(echo "$*" | sed 's/.*-t \([^ ]*\).*/\1/' | tr / _);; esac
    cat > $D/xc_$t;;
esac"#,
        );
        self.put("types", "TARGETS\nUTF8_STRING\nSTRING\n");
        self.put("xc_UTF8_STRING", "hello");
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":99");
        }
    }
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("bin")).unwrap();
    // The xclip event loop is started through `sh -c`.
    std::os::unix::fs::symlink("/bin/sh", dir.path().join("bin/sh")).unwrap();
    // SAFETY: tests that touch the environment hold ENV; nothing else here reads it concurrently.
    unsafe {
        std::env::set_var("PATH", dir.path().join("bin"));
        std::env::remove_var("GEEKBRIDGE_CLIPBOARD");
    }
    Rig { dir }
}

#[tokio::test]
async fn detection_depends_on_the_session_and_the_tools_present() {
    let _g = ENV.lock().await;
    let r = rig();
    unsafe {
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
    }
    assert!(detect_backend().is_none(), "no session, no tools");
    r.wayland();
    assert_eq!(detect_backend().unwrap().name(), "wl-clipboard");
    r.unstub("wl-copy");
    assert!(detect_backend().is_none(), "wl-paste alone is not enough");
    r.xclip();
    assert_eq!(detect_backend().unwrap().name(), "xclip");
    unsafe { std::env::set_var("GEEKBRIDGE_CLIPBOARD", "off") };
    assert!(detect_backend().is_none());
    unsafe { std::env::set_var("GEEKBRIDGE_CLIPBOARD", "memory") };
    assert_eq!(detect_backend().unwrap().name(), "memory");
    unsafe {
        std::env::remove_var("GEEKBRIDGE_CLIPBOARD");
        std::env::set_var("WAYLAND_DISPLAY", "");
        std::env::set_var("DISPLAY", "");
    }
    assert!(
        detect_backend().is_none(),
        "empty variables are not a session"
    );
}

#[tokio::test]
async fn wl_clipboard_text_round_trips_with_exact_argv_and_odd_unicode() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    let b = detect_backend().unwrap();
    let caps = b.capabilities();
    assert_eq!(caps.read, ReadAccess::Background);
    assert_eq!(caps.observe, ObserveMode::Event);
    assert!(caps.reports_sensitivity && caps.write);

    assert_eq!(b.read_text(1024).unwrap().as_deref(), Some("hello"));
    assert!(r.log().contains("--no-newline --type text"));
    for text in ["שלום עולם", "🙂 \u{202e}rtl", "multi\nline\r\n", "x\u{0}y"] {
        b.write_text(text).unwrap();
        assert_eq!(r.get("text").unwrap(), text.as_bytes());
    }
    // Empty clipboard (tool prints nothing) and no clipboard owner (tool fails) both read as "nothing".
    r.put("text", "");
    assert_eq!(b.read_text(1024).unwrap(), None);
    r.rm("text");
    assert_eq!(b.read_text(1024).unwrap(), None);
    // Writing fails when the tool does.
    r.put("copy_fail", "");
    assert_eq!(b.write_text("x"), Err(BackendError::Failed));
}

#[tokio::test]
async fn wl_clipboard_tool_failures_are_typed_and_bounded() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    let b = detect_backend().unwrap();

    r.put("mode", "crash");
    assert_eq!(
        b.read_text(1024).unwrap(),
        None,
        "a crashing tool yields nothing"
    );
    r.put("mode", "nonutf8");
    assert_eq!(b.read_text(1024), Err(BackendError::NotUtf8));
    r.put("mode", "huge");
    assert_eq!(b.read_text(1024), Err(BackendError::TooLarge));
    assert_eq!(b.read_text(1 << 20), Err(BackendError::TooLarge));
    r.put("text", "x".repeat(1025));
    r.put("mode", "");
    assert_eq!(b.read_text(1024), Err(BackendError::TooLarge));
    assert_eq!(
        b.read_text(1025).unwrap().unwrap().len(),
        1025,
        "exactly at the limit is fine"
    );

    // A hung tool is killed at the 2 s ceiling and reported as a failure, not waited for.
    r.put("mode", "hang");
    let started = std::time::Instant::now();
    assert_eq!(b.read_text(16), Err(BackendError::Failed));
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_secs(6),
        "{took:?}"
    );

    // The tool vanishing after detection: Unavailable.
    r.put("mode", "");
    r.unstub("wl-paste");
    assert_eq!(b.read_text(16), Err(BackendError::Unavailable));
    assert_eq!(b.sensitivity(), Sensitivity::Unknown);
    r.unstub("wl-copy");
    assert_eq!(b.write_text("x"), Err(BackendError::Unavailable));
}

#[tokio::test]
async fn wl_clipboard_image_mime_negotiation_trusts_magic_bytes() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    let b = detect_backend().unwrap();
    let read = |max| b.read_image(max);

    // Text only: no image.
    assert!(read(1 << 20).unwrap().is_none());
    // PNG advertised and real.
    r.put("types", "text/plain\nimage/png\n");
    r.put("image_png", PNG);
    let got = read(1 << 20).unwrap().unwrap();
    assert_eq!((got.mime, got.bytes.as_slice()), ("image/png", PNG));
    // JPEG only.
    r.put("types", "image/jpeg\n");
    r.put("image_jpeg", JPEG);
    assert_eq!(read(1 << 20).unwrap().unwrap().mime, "image/jpeg");
    // Both advertised: PNG preferred.
    r.put("types", "image/jpeg\nIMAGE/PNG\n");
    assert_eq!(read(1 << 20).unwrap().unwrap().mime, "image/png");
    // Advertised PNG that is really a JPEG is reported as what it is; garbage is no image at all.
    r.put("types", "image/png\n");
    r.put("image_png", JPEG);
    assert_eq!(read(1 << 20).unwrap().unwrap().mime, "image/jpeg");
    r.put("image_png", b"not an image at all");
    assert!(read(1 << 20).unwrap().is_none());
    r.put("image_png", "");
    assert!(read(1 << 20).unwrap().is_none());
    // Over the cap: refused, never truncated.
    let mut big = PNG.to_vec();
    big.resize(5000, 1);
    r.put("image_png", &big);
    assert_eq!(read(4096).err(), Some(BackendError::TooLarge));
    assert_eq!(read(5000).unwrap().unwrap().bytes.len(), 5000);
    // Type listing unavailable: no image, unknown sensitivity.
    r.put("types_fail", "");
    assert!(read(1 << 20).unwrap().is_none());
    assert_eq!(b.sensitivity(), Sensitivity::Unknown);
    r.rm("types_fail");

    // Writing names the MIME type to the tool.
    b.write_image(&ClipImage {
        mime: "image/png",
        bytes: PNG.to_vec(),
    })
    .unwrap();
    assert_eq!(r.get("image_png").unwrap(), PNG);
    assert!(r.log().contains("copy --type image/png"));
    b.write_image(&ClipImage {
        mime: "image/jpeg",
        bytes: JPEG.to_vec(),
    })
    .unwrap();
    assert_eq!(r.get("image_jpeg").unwrap(), JPEG);
    r.put("copy_fail", "");
    assert_eq!(
        b.write_image(&ClipImage {
            mime: "image/png",
            bytes: PNG.to_vec()
        }),
        Err(BackendError::Failed)
    );
}

#[tokio::test]
async fn html_and_password_manager_hints_come_from_the_type_list() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    let b = detect_backend().unwrap();
    assert_eq!(b.sensitivity(), Sensitivity::Normal);
    assert_eq!(b.read_html(1024).unwrap(), None, "no html target offered");
    r.put("types", "text/plain\ntext/html\n");
    r.put("text_html", "<b>é</b>");
    assert_eq!(b.read_html(1024).unwrap().as_deref(), Some("<b>é</b>"));
    r.put("text_html", vec![b'<'; 4000]);
    assert_eq!(b.read_html(1024), Err(BackendError::TooLarge));
    r.put("text_html", b"\xff\xfe");
    assert_eq!(b.read_html(1024), Err(BackendError::NotUtf8));
    // Case and padding in the hint's line do not hide it; a longer name does not match.
    for (types, want) in [
        (
            "text/plain\nx-kde-passwordManagerHint\n",
            Sensitivity::Sensitive,
        ),
        (
            "text/plain\n  X-KDE-PASSWORDMANAGERHINT  \n",
            Sensitivity::Sensitive,
        ),
        (
            "text/plain\r\nx-kde-passwordManagerHint\r\n",
            Sensitivity::Sensitive,
        ),
        ("x-kde-passwordManagerHint-not\n", Sensitivity::Normal),
        ("", Sensitivity::Normal),
    ] {
        r.put("types", types);
        assert_eq!(b.sensitivity(), want, "{types:?}");
    }
}

#[tokio::test]
async fn xclip_backend_reads_utf8_then_string_and_writes_images() {
    let _g = ENV.lock().await;
    let r = rig();
    r.xclip();
    let b = detect_backend().unwrap();
    assert_eq!(b.name(), "xclip");
    // No clipnotify: polling.
    assert_eq!(b.capabilities().observe, ObserveMode::Polling);
    assert!(b.change_events().is_none());

    assert_eq!(b.read_text(64).unwrap().as_deref(), Some("hello"));
    assert!(r.log().contains("-selection clipboard -o -t UTF8_STRING"));
    // UTF8_STRING missing: STRING is tried next; both missing is an empty clipboard.
    r.rm("xc_UTF8_STRING");
    r.put("xc_STRING", "latin");
    assert_eq!(b.read_text(64).unwrap().as_deref(), Some("latin"));
    r.rm("xc_STRING");
    assert_eq!(b.read_text(64).unwrap(), None);
    // Too large and non-UTF-8 stop the search at once.
    r.put("xc_UTF8_STRING", "y".repeat(100));
    assert_eq!(b.read_text(10), Err(BackendError::TooLarge));
    r.put("xc_UTF8_STRING", b"\xc3\x28");
    assert_eq!(b.read_text(10), Err(BackendError::NotUtf8));

    b.write_text("ünï").unwrap();
    assert_eq!(r.get("xc_text").unwrap(), "ünï".as_bytes());
    assert!(r.log().contains("-selection clipboard -i"));
    r.put("copy_fail", "");
    assert_eq!(b.write_text("x"), Err(BackendError::Failed));
    r.rm("copy_fail");

    b.write_image(&ClipImage {
        mime: "image/png",
        bytes: PNG.to_vec(),
    })
    .unwrap();
    assert_eq!(r.get("xc_image_png").unwrap(), PNG);
    r.put("types", "TARGETS\nimage/png\nx-kde-passwordManagerHint\n");
    r.put("xc_image_png", PNG);
    assert_eq!(b.read_image(1 << 20).unwrap().unwrap().mime, "image/png");
    assert_eq!(b.sensitivity(), Sensitivity::Sensitive);

    // clipnotify on PATH switches xclip to event mode with a shell loop.
    r.stub("clipnotify", "exit 0");
    assert_eq!(b.capabilities().observe, ObserveMode::Event);
    assert!(b.change_events().is_some());
}

fn sent(rx: &mut mpsc::Receiver<ClipOut>) -> Vec<String> {
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
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out: {what}");
}

const PHONE: &str = "phone-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CAPS: LinkCaps = LinkCaps {
    stock: true,
    pull: true,
};

fn start(r: &Rig, poll_ms: u64) -> (Arc<ClipboardService>, mpsc::Receiver<ClipOut>) {
    let _ = r;
    let backend: Arc<dyn ClipboardBackend> = detect_backend().unwrap();
    let svc = ClipboardService::with_poll(backend, "desktop-1", Duration::from_millis(poll_ms));
    let rx = svc.link_up(PHONE, "Phone", CAPS, true);
    (svc, rx)
}

#[tokio::test]
async fn the_service_sends_changes_by_event_skips_secrets_and_does_not_echo_its_own_writes() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    let (svc, mut rx) = start(&r, 20);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(sent(&mut rx).is_empty(), "the baseline is not a change");

    // An event wakes the watcher; the new text goes out once.
    r.put("text", "first copy");
    r.put("fire", "");
    eventually("first copy sent", || {
        sent(&mut rx).contains(&"first copy".to_owned())
    })
    .await;

    // A password-manager copy is observed but never sent.
    r.put("types", "text/plain\nx-kde-passwordManagerHint\n");
    r.put("text", "hunter2");
    r.put("fire", "");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        sent(&mut rx).is_empty(),
        "secret must not leave the machine"
    );
    r.put("types", "text/plain\n");

    // Remote text is written through wl-copy and the watcher's reading of it is not echoed back.
    svc.on_packet(
        PHONE,
        CLIPBOARD,
        &clipboard_wire::update_body("from the phone"),
        None,
    );
    eventually("applied", || {
        r.get("text").as_deref() == Some(b"from the phone")
    })
    .await;
    r.put("fire", "");
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(sent(&mut rx).is_empty(), "applied text echoed back: loop");

    // An oversize clipboard is skipped, counted once, and the next normal copy is sent again.
    r.put("text", "z".repeat(2 << 20));
    r.put("fire", "");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sent(&mut rx).is_empty());
    r.put("text", "small again");
    r.put("fire", "");
    eventually("recovered", || {
        sent(&mut rx).contains(&"small again".to_owned())
    })
    .await;
    r.put("watch_stop", "");
}

#[tokio::test]
async fn when_the_event_helper_dies_the_service_falls_back_to_polling() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    r.put("watch_die", "");
    let (_svc, mut rx) = start(&r, 30);
    tokio::time::sleep(Duration::from_millis(300)).await;
    sent(&mut rx);
    r.put("text", "found by polling");
    eventually("polled", || {
        sent(&mut rx).contains(&"found by polling".to_owned())
    })
    .await;
    assert!(
        r.log().contains("--watch"),
        "the helper was started and ended"
    );
}

#[tokio::test]
async fn tool_trouble_is_surfaced_as_a_problem_and_clears_when_the_tool_recovers() {
    let _g = ENV.lock().await;
    let r = rig();
    r.wayland();
    r.put("watch_die", "");
    let (svc, mut rx) = start(&r, 30);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let problem = |svc: &ClipboardService| svc.status(Vec::new()).problem;
    r.put("mode", "nonutf8");
    eventually("not utf8 reported", || problem(&svc).is_some()).await;
    r.put("mode", "");
    r.put("text", "recovered text");
    eventually("recovered", || problem(&svc).is_none()).await;
    eventually("sent", || {
        sent(&mut rx).contains(&"recovered text".to_owned())
    })
    .await;
    r.unstub("wl-paste");
    eventually("unavailable reported", || problem(&svc).is_some()).await;
}
