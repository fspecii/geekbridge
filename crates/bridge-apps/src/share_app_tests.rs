// SPDX-License-Identifier: GPL-3.0-or-later
//! Share window: the transfer-list model (wording, progress, failure codes), file filtering, command
//! building and headless renders of every transfer state, dropped files and a missing service.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use crate::test_support::{Rig, shapes};
use bridge_core::ipc::{LinkRole, LinkStatus, PairedDevice};
use bridge_core::share::ShareDeviceSettings;
use eframe::egui::{DroppedFile, Event, HoveredFile};

fn transfer(
    id: u64,
    kind: TransferKind,
    dir: TransferDirection,
    state: TransferState,
    size: Option<u64>,
    bytes: u64,
) -> TransferInfo {
    TransferInfo {
        transfer_id: id,
        device_id: "a".into(),
        device_name: "Pixel".into(),
        direction: dir,
        kind,
        name: format!("file-{id}.bin"),
        size,
        bytes,
        state,
        code: None,
        path: None,
        text: None,
        expires_in_ms: Some(30_000),
        at_unix_ms: id,
    }
}

fn status(connected: bool, devices: usize) -> Status {
    let paired = (0..devices)
        .map(|i| PairedDevice {
            device_id: if i == 0 { "a".into() } else { format!("x{i}") },
            name: format!("Phone {i}"),
            device_type: "phone".into(),
            cert_sha256: String::new(),
            paired_at_unix: 0,
            consents: vec![],
        })
        .collect();
    let links = if connected {
        vec![LinkStatus {
            device_id: "a".into(),
            name: String::new(),
            phase: LinkPhase::Connected,
            role: Some(LinkRole::Initiator),
            connection_id: None,
            peer_ip: None,
            peer_cert_sha256: None,
            negotiated: vec![],
            since_unix_ms: 0,
            pings_received: 0,
            last_ping_unix_ms: None,
            rejected_packets: 0,
            battery: None,
        }]
    } else {
        vec![]
    };
    Status {
        ipc_version: 1,
        service_version: "1".into(),
        device_id: "me".into(),
        device_name: "Desk".into(),
        own_cert_sha256: String::new(),
        paired,
        links,
        challenges: vec![],
        event_seq: 0,
        inbound_listener: None,
        reconnecting: vec![],
        disconnected: vec![],
    }
}

fn every_transfer() -> Vec<TransferInfo> {
    use TransferDirection::{Incoming, Outgoing};
    use TransferKind::{File, Text, Url};
    use TransferState::*;
    let mut v = Vec::new();
    let mut id = 0;
    for dir in [Incoming, Outgoing] {
        for kind in [File, Text, Url] {
            for state in [Offered, Waiting, Active, Done, Failed, Cancelled, Declined] {
                for size in [None, Some(0), Some(1500), Some(u64::MAX)] {
                    id += 1;
                    let mut t = transfer(id, kind, dir, state, size, 700);
                    if kind != File {
                        t.text = Some("https://example.org/a?b=c d\n\u{202e}text".into());
                    }
                    if state == Failed {
                        t.code = Some(["expired", "timeout", "weird"][id as usize % 3].into());
                    }
                    if state == Done && dir == Incoming && kind == File {
                        t.path = Some("/tmp/geekbridge-test/file.sh".into());
                    }
                    v.push(t);
                }
            }
        }
    }
    v
}

fn app_with(view: View) -> (ShareApp, mpsc::Receiver<Cmd>) {
    let (tx, rx) = mpsc::sync_channel::<Cmd>(16);
    let app = ShareApp {
        worker: Worker {
            tx,
            latest: Arc::new(Mutex::new(None)),
        },
        view,
        chosen: None,
        text: String::new(),
    };
    (app, rx)
}

fn frame(app: &mut ShareApp, rig: &mut Rig, events: Vec<Event>) -> eframe::egui::FullOutput {
    let input = rig.input(events);
    rig.ctx.run(input, |ctx| app.frame(ctx))
}

fn share_status(transfers: Vec<TransferInfo>) -> ShareStatus {
    ShareStatus {
        save_dir: "/home/u/Downloads/GeekBridge".into(),
        max_file_bytes: 1 << 30,
        devices: vec![ShareDeviceSettings {
            device_id: "a".into(),
            receive: true,
            auto_accept: false,
        }],
        transfers,
    }
}

#[test]
fn the_transfer_list_model_words_every_combination_without_leaking_codes() {
    for t in every_transfer() {
        let line = state_line(&t);
        assert!(!line.is_empty());
        assert!(
            !line.contains("expired") || line.contains("nobody"),
            "{line}"
        );
        if let Some(f) = progress_fraction(&t) {
            assert!((0.0..=1.0).contains(&f), "{f} for {t:?}");
        }
    }
    let mut running = transfer(
        1,
        TransferKind::File,
        TransferDirection::Outgoing,
        TransferState::Active,
        Some(100),
        250,
    );
    assert_eq!(
        progress_fraction(&running),
        Some(1.0),
        "bytes beyond the size are clamped"
    );
    running.size = Some(0);
    assert_eq!(progress_fraction(&running), None);
    running.size = None;
    assert_eq!(progress_fraction(&running), None);
    running.state = TransferState::Done;
    assert_eq!(progress_fraction(&running), Some(1.0));
    for code in [
        "expired",
        "timeout",
        "too_large",
        "truncated",
        "cancelled",
        "pin_mismatch",
        "tls_failed",
        "unreadable",
        "io_error",
        "mystery",
    ] {
        assert!(!failure_words(Some(code)).is_empty());
    }
    assert_eq!(failure_words(None), failure_words(Some("mystery")));
    assert_eq!(human_size(1023), "1023 B");
    assert_eq!(human_size(1024), "1.0 KB");
    assert!(
        human_size(u64::MAX).ends_with("TB"),
        "{}",
        human_size(u64::MAX)
    );
}

#[test]
fn only_readable_files_are_sendable_and_picker_output_is_tolerant() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, b"x").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("nope"), &dangling).unwrap();
        let got = sendable([
            link,
            dangling,
            sub.clone(),
            file.clone(),
            dir.path().join("missing"),
        ]);
        assert_eq!(
            got.len(),
            2,
            "symlink to a file resolves, folders/dangling/missing dropped: {got:?}"
        );
        assert!(
            got.iter().all(|p| !p.contains("link")),
            "paths are canonical"
        );
    }
    assert!(sendable([sub]).is_empty());
    assert!(sendable(Vec::new()).is_empty());
    assert_eq!(parse_picker_output("/a b\r\n\n  \n/c\n").len(), 2);
    assert!(parse_picker_output("").is_empty());
    for risky in [
        "x.EXE",
        "x.sh",
        "x.tar.gz.ps1",
        "x.exe.",
        "x.exe  ",
        "y.AppImage",
        "x.msc",
        "x.Scpt",
        "x.webloc",
        "x.mobileconfig",
        "x.application",
    ] {
        assert!(is_risky_to_open(Path::new(risky)), "{risky}");
    }
    for safe in ["x.txt", "exe", ".bashrc", "", "dir/"] {
        assert!(!is_risky_to_open(Path::new(safe)), "{safe}");
    }
    let (_, open) = open_command(Path::new("/tmp/x/a.txt"), false);
    let (_, reveal) = open_command(Path::new("/tmp/x/a.txt"), true);
    if cfg!(target_os = "linux") {
        assert_eq!(open, vec!["/tmp/x/a.txt".to_owned()]);
        assert_eq!(reveal, vec!["/tmp/x".to_owned()]);
    }
    // These spawn nothing harmful: a missing program is ignored, a refused URL never reaches a program.
    open_link("javascript:alert(1)");
    open_link("file:///etc/passwd");
    spawn_quiet("/nonexistent/geekbridge-test-program", &[]);
}

#[test]
fn errors_from_the_service_are_described_in_plain_bounded_words() {
    let io = ipc::IpcError::Io(std::io::Error::other("refused"));
    assert!(describe(io).contains("isn't reachable"));
    // Nothing is listening on port 1: the batch result names the failure rather than "Sending".
    let token = Token::parse(&[7u8; 32]).or_else(|_| Token::parse(b"0".repeat(64).as_slice()));
    if let Ok(token) = token {
        let msg = send_files(1, &token, "a", &["/x".into(), "/y".into()]);
        assert!(!msg.starts_with("Sending"), "{msg}");
        assert_eq!(send_files(1, &token, "a", &[]), "Nothing to send.");
    }
}

#[test]
fn renders_every_state_at_wide_and_narrow_sizes() {
    for (w, h) in [(800.0, 700.0), (320.0, 480.0), (200.0, 260.0)] {
        for (connected, devices) in [(true, 1), (false, 1), (true, 3), (false, 0)] {
            let mut view = View {
                status: Some(status(connected, devices)),
                share: Some(share_status(every_transfer())),
                error: None,
                notice: Some("Sending 2 files…".into()),
            };
            if devices == 0 {
                view.share = None;
            }
            let (mut app, _rx) = app_with(view);
            let mut rig = Rig::new(AppKind::Clipboard, w, h);
            let mut out = frame(&mut app, &mut rig, vec![]);
            for _ in 0..3 {
                out = frame(&mut app, &mut rig, vec![]);
            }
            assert!(
                shapes(&out) > 5,
                "{w}x{h} connected={connected} devices={devices}"
            );
        }
    }
}

#[test]
fn a_missing_service_shows_its_reason_and_nothing_else_breaks() {
    for err in [
        "Start the GeekBridge service first.",
        "The service settings couldn't be read securely.",
        &"x".repeat(5000),
    ] {
        let (mut app, _rx) = app_with(View {
            error: Some(err.into()),
            ..View::default()
        });
        let mut rig = Rig::new(AppKind::Clipboard, 500.0, 500.0);
        for _ in 0..3 {
            frame(&mut app, &mut rig, vec![]);
        }
    }
}

#[test]
fn dropped_files_go_to_the_connected_phone_only_and_folders_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("photo.jpg");
    std::fs::write(&file, b"jpg").unwrap();
    let drop = |app: &mut ShareApp, rig: &mut Rig, paths: Vec<PathBuf>| {
        let mut input = rig.input(vec![]);
        input.hovered_files = vec![HoveredFile::default()];
        input.dropped_files = paths
            .into_iter()
            .map(|p| DroppedFile {
                path: Some(p),
                ..Default::default()
            })
            .collect();
        let _ = rig.ctx.run(input, |ctx| app.frame(ctx));
    };

    // Connected: a file is queued, a folder is refused with a plain notice.
    let (mut app, rx) = app_with(View {
        status: Some(status(true, 1)),
        ..View::default()
    });
    let mut rig = Rig::new(AppKind::Clipboard, 600.0, 600.0);
    drop(&mut app, &mut rig, vec![file.clone()]);
    match rx.try_recv() {
        Ok(Cmd::SendFiles { device_id, paths }) => {
            assert_eq!(device_id, "a");
            assert_eq!(paths.len(), 1);
        }
        _ => panic!("the dropped file should be queued"),
    }
    drop(&mut app, &mut rig, vec![dir.path().to_path_buf()]);
    assert!(rx.try_recv().is_err());
    assert_eq!(
        app.view.notice.as_deref(),
        Some("Only files can be sent, not folders.")
    );

    // Not connected: nothing is queued.
    let (mut app, rx) = app_with(View {
        status: Some(status(false, 1)),
        ..View::default()
    });
    drop(&mut app, &mut rig, vec![file.clone()]);
    assert!(rx.try_recv().is_err());
    assert!(
        app.view
            .notice
            .as_deref()
            .unwrap()
            .contains("Connect a phone")
    );

    // No phone at all.
    let (mut app, rx) = app_with(View::default());
    drop(&mut app, &mut rig, vec![file]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn clicking_through_the_window_only_sends_well_formed_commands() {
    let (mut app, rx) = app_with(View {
        status: Some(status(true, 2)),
        share: Some(share_status(every_transfer())),
        ..View::default()
    });
    app.text = "https://example.org/".into();
    let mut rig = Rig::new(AppKind::Clipboard, 700.0, 900.0);
    for _ in 0..3 {
        frame(&mut app, &mut rig, vec![]);
    }
    for y in (30..900).step_by(18) {
        for x in [40.0, 160.0, 300.0, 450.0, 600.0] {
            let at = eframe::egui::pos2(x, y as f32);
            frame(&mut app, &mut rig, vec![Event::PointerMoved(at)]);
            for pressed in [true, false] {
                frame(
                    &mut app,
                    &mut rig,
                    vec![Event::PointerButton {
                        pos: at,
                        button: eframe::egui::PointerButton::Primary,
                        pressed,
                        modifiers: eframe::egui::Modifiers::NONE,
                    }],
                );
            }
        }
    }
    let mut seen = 0;
    while let Ok(cmd) = rx.try_recv() {
        seen += 1;
        match cmd {
            Cmd::Call(Request::ShareSendText { text, .. }) => assert!(!text.is_empty()),
            Cmd::Call(_) | Cmd::Pick { .. } | Cmd::SendFiles { .. } => {}
        }
    }
    assert!(seen > 0, "some control was activated by the sweep");
}
