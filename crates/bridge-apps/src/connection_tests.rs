// SPDX-License-Identifier: GPL-3.0-or-later
//! The service-control model behind the connection details and pairing dialogs: how answers and
//! events fold into what is shown, the worker loop, and headless renders of every link state with a
//! scripted service. A scripted service means no socket and no token file.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use bridge_core::{
    AppKind,
    ipc::{ChallengeDirection, EventPage, PairChallenge, PairQr, PairedDevice},
    pairing_help::{ConnectionTest, TestStep},
};
use eframe::egui::{Event, Modifiers, PointerButton, pos2};

type Log = Arc<Mutex<Vec<Request>>>;

fn link(id: &str, phase: LinkPhase) -> ipc::LinkStatus {
    ipc::LinkStatus {
        device_id: id.into(),
        name: format!("Phone {id}"),
        phase,
        role: None,
        connection_id: Some(1),
        peer_ip: None,
        peer_cert_sha256: None,
        negotiated: vec![],
        since_unix_ms: 0,
        pings_received: 0,
        last_ping_unix_ms: None,
        rejected_packets: 0,
        battery: None,
    }
}

fn paired(id: &str, name: &str) -> PairedDevice {
    PairedDevice {
        device_id: id.into(),
        name: name.into(),
        device_type: "phone".into(),
        cert_sha256: String::new(),
        paired_at_unix: 0,
        consents: vec![],
    }
}

fn challenge(key: &str, expires: u64) -> PairChallenge {
    PairChallenge {
        challenge_id: "c1".into(),
        device_id: "a".repeat(32),
        device_name: "Phone \u{202e}evil".into(),
        direction: ChallengeDirection::Incoming,
        connection_id: 1,
        peer_cert_sha256: "00".repeat(32),
        own_cert_sha256: "11".repeat(32),
        verification_key: key.into(),
        expires_in_ms: expires,
    }
}

fn status() -> Status {
    Status {
        ipc_version: 1,
        service_version: "test".into(),
        device_id: "x".repeat(32),
        device_name: "Desk".into(),
        own_cert_sha256: "00".repeat(32),
        paired: vec![],
        links: vec![],
        challenges: vec![],
        event_seq: 0,
        inbound_listener: None,
        reconnecting: vec![],
        disconnected: vec![],
    }
}

fn busy_status() -> Status {
    let mut s = status();
    s.links = vec![
        link("l1", LinkPhase::Connecting),
        link("l2", LinkPhase::Unpaired),
        link("l3", LinkPhase::Pairing),
        link("l4", LinkPhase::Connected),
    ];
    s.paired = vec![
        paired("l4", "Connected one"),
        paired("off", "Offline one"),
        paired("w", &"Wide name ".repeat(30)),
    ];
    s
}

fn scripted(
    fetch: impl FnMut() -> Result<Status, String> + Send + 'static,
    respond: impl Fn(&Request) -> Result<Response, String> + Send + 'static,
) -> (Connections, Log) {
    let log: Log = Arc::default();
    let sink = log.clone();
    let worker = Worker::spawn_with(
        fetch,
        move |r| {
            sink.lock().unwrap().push(r.clone());
            respond(r)
        },
        || {},
    );
    let conn = Connections {
        worker,
        state: ControlState::default(),
        manual_id: String::new(),
        manual_address: String::new(),
        active_challenge: None,
        entered_code: String::new(),
        forget: None,
        pair: Default::default(),
        name_draft: None,
    };
    (conn, log)
}

fn conn_with(s: Status) -> (Connections, Log) {
    let (mut c, log) = scripted(move || Ok(s.clone()), |_| Ok(Response::Ack));
    wait_for_state(&mut c, |st| st.status.is_some());
    (c, log)
}

fn wait_for_state(c: &mut Connections, ok: impl Fn(&ControlState) -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        c.update();
        if ok(&c.state) {
            return;
        }
        assert!(
            std::time::Instant::now() < end,
            "state never arrived: {:?}",
            c.state
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn nearby() -> DiscoveryModel {
    DiscoveryModel::Ready {
        discovery_error: None,
        devices: vec![crate::state::DeviceInfo {
            id: "near".into(),
            name: "Nearby phone".into(),
            kind: "phone".into(),
            address: "192.168.1.9".into(),
            trusted: false,
            advertises_feature: true,
        }],
    }
}

fn frame(
    c: &mut Connections,
    ctx: &egui::Context,
    size: egui::Vec2,
    discovery: &DiscoveryModel,
    events: Vec<Event>,
) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..Default::default()
    };
    let pal = theme::palette(AppKind::Calls, true);
    c.update();
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                c.name_row(ui, &pal);
                let _ = c.pair_card(ui, &pal, discovery);
            });
        });
        c.dialogs(ctx, &pal);
    });
}

fn click(
    c: &mut Connections,
    ctx: &egui::Context,
    size: egui::Vec2,
    d: &DiscoveryModel,
    at: egui::Pos2,
) {
    frame(c, ctx, size, d, vec![Event::PointerMoved(at)]);
    for pressed in [true, false] {
        frame(
            c,
            ctx,
            size,
            d,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }],
        );
    }
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    theme::apply(&ctx, AppKind::Calls);
    ctx
}

// --------------------------------------------------------------------------------- pure model

#[test]
fn every_service_error_is_a_plain_sentence_that_leaks_nothing() {
    let cases = [
        ipc::IpcError::Io(std::io::Error::other("secret path /home/u/.token")),
        ipc::IpcError::Unauthorized,
        ipc::IpcError::Protocol("bad frame".into()),
        ipc::IpcError::Unavailable {
            code: "no_key".into(),
            message: "no key".into(),
        },
    ];
    for e in cases {
        let line = service_error(e);
        assert!(
            !line.contains("/home") && !line.contains("bad frame") && !line.contains("no key"),
            "{line}"
        );
        assert!(line.len() > 20);
    }
}

#[test]
fn results_fold_into_state_exactly_once_per_request_kind() {
    let mut st = ControlState::default();
    let qr = PairQr {
        payload: "geekbridge://pair?x=1".into(),
        expires_in_ms: 60_000,
        computer_name: "Desk".into(),
        addresses: vec!["192.168.1.2:1716".into()],
    };
    apply_result(
        &mut st,
        &Request::PairQr,
        Ok(Response::PairQr(Box::new(qr.clone()))),
    );
    assert_eq!(st.pair_qr.as_ref(), Some(&qr));
    st.notice = Some("old".into());
    apply_result(&mut st, &Request::PairQrCancel, Ok(Response::Ack));
    assert!(st.pair_qr.is_none() && st.notice.is_none());
    let test = ConnectionTest {
        ok: true,
        headline: "ok".into(),
        steps: vec![],
        advice: vec![],
    };
    apply_result(
        &mut st,
        &Request::TestConnection {
            device_id: None,
            endpoint: None,
        },
        Ok(Response::ConnectionTest(Box::new(test.clone()))),
    );
    assert_eq!(st.test.as_ref(), Some(&test));
    for req in [
        Request::Connect {
            device_id: "a".into(),
            endpoint: None,
        },
        Request::Pair {
            device_id: "a".into(),
            mode: ipc::PairMode::Request,
        },
        Request::Approve {
            challenge_id: "c".into(),
        },
        Request::Deny {
            challenge_id: "c".into(),
        },
        Request::Disconnect {
            device_id: "a".into(),
        },
        Request::Unpair {
            device_id: "a".into(),
        },
        Request::SetDeviceName { name: "n".into() },
        Request::Ping {
            device_id: "a".into(),
        },
        Request::Status,
    ] {
        apply_result(&mut st, &req, Ok(Response::Ack));
        assert!(
            st.notice.as_deref().is_some_and(|n| !n.is_empty()),
            "{req:?}"
        );
        apply_result(&mut st, &req, Err("refused: busy".into()));
        assert_eq!(st.notice.as_deref(), Some("refused: busy"));
    }
    // A reply of the wrong shape to a QR request is a plain acknowledgement, never a stale code.
    st.pair_qr = None;
    apply_result(&mut st, &Request::PairQr, Ok(Response::Ack));
    assert!(st.pair_qr.is_none());
}

#[test]
fn failure_events_become_one_friendly_problem_and_pairing_clears_it() {
    let mut st = ControlState::default();
    let ev = |kind| ipc::Event {
        seq: 1,
        at_unix_ms: 0,
        device_id: None,
        kind,
    };
    for reason in [
        "token expired",
        "",
        "\u{202e}weird",
        "TLS handshake failed",
        "connection refused",
    ] {
        for kind in [
            ipc::EventKind::PairFailed {
                reason: reason.into(),
            },
            ipc::EventKind::QrRefused {
                reason: reason.into(),
            },
            ipc::EventKind::ConnectFailed {
                reason: reason.into(),
            },
        ] {
            st.problem = None;
            absorb_events(&mut st, &[ev(kind)]);
            let p = st.problem.clone().expect("a problem is shown");
            assert!(!p.headline.is_empty() && !p.advice.is_empty());
        }
    }
    absorb_events(&mut st, &[ev(ipc::EventKind::Connected)]);
    assert!(st.problem.is_some(), "unrelated events keep the problem");
    absorb_events(&mut st, &[ev(ipc::EventKind::Paired)]);
    assert!(st.problem.is_none());
    absorb_events(&mut st, &[]);
}

#[test]
fn computer_names_and_manual_endpoints_reject_hostile_input() {
    for bad in ["", "  ", "a\nb", "a\u{0}b", "\u{202e}x", &"é".repeat(40)] {
        let _ = name_problem(bad);
    }
    assert_eq!(name_problem("Study PC"), None);
    assert!(name_problem("\u{1b}[31m").is_some());
    let id = "a".repeat(32);
    for addr in [
        "",
        "nonsense",
        "1.2.3.4",
        "1.2.3.4:99999",
        "[::]:1716",
        "[ff02::1]:1716",
        "999.1.1.1:1",
        "1.2.3.4:1716 extra",
    ] {
        assert!(manual_endpoint(&id, addr).is_err(), "{addr}");
    }
    assert!(manual_endpoint(&id, "[fe80::1]:1716").is_ok());
    assert!(
        manual_endpoint(&format!(" {id} "), " 10.0.0.2:1716 ").is_ok(),
        "whitespace is trimmed"
    );
    assert!(manual_endpoint("", "10.0.0.2:1716").is_err());
    assert!(
        code_matches("  ABCD1234 ", "abcd1234"),
        "padding around the typed code is ignored"
    );
    assert!(!code_matches("ABCD1234", "short"));
    assert!(!code_matches("ABCD1234", "ABCD123G"));
    assert!(
        !code_matches("ＡＢＣＤ１２３４", "ABCD1234"),
        "full-width digits are not hex"
    );
}

// --------------------------------------------------------------------------------- the worker

#[test]
fn the_worker_publishes_status_errors_and_events_in_order() {
    let calls = Arc::new(Mutex::new(0u32));
    let c2 = calls.clone();
    let (mut c, log) = scripted(
        move || {
            let mut n = c2.lock().unwrap();
            *n += 1;
            let mut s = status();
            // First status sets the baseline; later polls announce events.
            s.event_seq = if *n == 1 { 5 } else { 9 };
            Ok(s)
        },
        |r| match r {
            Request::Events { after } => Ok(Response::Events(EventPage {
                events: vec![ipc::Event {
                    seq: after + 1,
                    at_unix_ms: 0,
                    device_id: None,
                    kind: ipc::EventKind::PairFailed {
                        reason: "token expired".into(),
                    },
                }],
                next_after: 9,
                gap: false,
            })),
            _ => Ok(Response::Ack),
        },
    );
    wait_for_state(&mut c, |s| s.status.is_some());
    c.refresh();
    wait_for_state(&mut c, |s| s.problem.is_some());
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, Request::Events { after: 5 })),
        "history before the first status is not replayed"
    );
}

#[test]
fn an_unreachable_service_shows_its_error_and_never_a_stale_status() {
    let ok = Arc::new(Mutex::new(true));
    let o = ok.clone();
    let (mut c, _) = scripted(
        move || {
            if *o.lock().unwrap() {
                Ok(status())
            } else {
                Err("down".into())
            }
        },
        |_| Ok(Response::Ack),
    );
    wait_for_state(&mut c, |s| s.status.is_some());
    *ok.lock().unwrap() = false;
    c.refresh();
    wait_for_state(&mut c, |s| s.error.is_some());
    assert!(c.state.status.is_none() && !c.state.connected());
    // Rendering with an error offers a retry and does not panic.
    let cx = ctx();
    frame(
        &mut c,
        &cx,
        egui::vec2(500.0, 500.0),
        &DiscoveryModel::Checking,
        vec![],
    );
}

#[test]
fn a_full_request_queue_says_so_instead_of_blocking() {
    let gate = Arc::new(Mutex::new(()));
    let held = gate.clone();
    let (mut c, _) = scripted(
        move || Ok(status()),
        move |_| {
            let _wait = held.lock().unwrap();
            Ok(Response::Ack)
        },
    );
    wait_for_state(&mut c, |s| s.status.is_some());
    let _hold = gate.lock().unwrap();
    for _ in 0..40 {
        c.submit(Request::Ping {
            device_id: "a".into(),
        });
    }
    assert_eq!(
        c.state.notice.as_deref(),
        Some("Another request is in progress. Try again shortly.")
    );
}

// --------------------------------------------------------------------------------- the dialogs

#[test]
fn details_and_pair_card_render_every_link_state_at_every_width() {
    let cx = ctx();
    for width in [900.0, 360.0, 220.0] {
        for s in [status(), busy_status()] {
            let (mut c, _) = conn_with(s);
            c.state.problem = Some(Problem::from_reason("token expired"));
            c.state.test = Some(ConnectionTest {
                ok: false,
                headline: "Can't reach the phone".into(),
                steps: vec![
                    TestStep {
                        label: "Network".into(),
                        ok: true,
                        detail: "fine".into(),
                    },
                    TestStep {
                        label: "Port".into(),
                        ok: false,
                        detail: "closed \u{202e}".into(),
                    },
                ],
                advice: vec!["Same Wi-Fi?".into(), "x".repeat(400)],
            });
            c.state.notice = Some("Connection requested.".into());
            for d in [DiscoveryModel::Checking, nearby()] {
                for _ in 0..3 {
                    frame(&mut c, &cx, egui::vec2(width, 800.0), &d, vec![]);
                }
            }
        }
    }
}

#[test]
fn approve_is_only_sent_after_the_full_code_is_typed_and_never_after_expiry() {
    let cx = ctx();
    let size = egui::vec2(800.0, 700.0);
    for (typed, expires, expect_approve) in [
        ("", 60_000u64, false),
        ("ABCD123", 60_000, false),
        ("ABCD1235", 60_000, false),
        ("abcd1234", 60_000, true),
        ("ABCD1234", 0, false),
    ] {
        let mut s = busy_status();
        s.challenges = vec![challenge("ABCD1234", expires)];
        let (mut c, log) = conn_with(s);
        frame(&mut c, &cx, size, &DiscoveryModel::Checking, vec![]);
        c.entered_code = typed.into();
        for y in (150..650).step_by(14) {
            for x in (220..620).step_by(40) {
                click(
                    &mut c,
                    &cx,
                    size,
                    &DiscoveryModel::Checking,
                    pos2(x as f32, y as f32),
                );
                c.entered_code = typed.into();
            }
        }
        // The worker records requests on its own thread: give it time to show what it was asked.
        let seen = |l: &Log| {
            l.lock()
                .unwrap()
                .iter()
                .any(|r| matches!(r, Request::Approve { .. }))
        };
        let end = std::time::Instant::now()
            + Duration::from_millis(if expect_approve { 3000 } else { 400 });
        while !seen(&log) && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            seen(&log),
            expect_approve,
            "typed {typed:?}, expires {expires}"
        );
    }
}

#[test]
fn a_changed_challenge_clears_the_typed_code() {
    let cx = ctx();
    let mut s = status();
    s.challenges = vec![challenge("AAAA1111", 60_000)];
    let (mut c, _) = conn_with(s);
    frame(
        &mut c,
        &cx,
        egui::vec2(800.0, 700.0),
        &DiscoveryModel::Checking,
        vec![],
    );
    c.entered_code = "AAAA1111".into();
    frame(
        &mut c,
        &cx,
        egui::vec2(800.0, 700.0),
        &DiscoveryModel::Checking,
        vec![],
    );
    assert_eq!(c.entered_code, "AAAA1111", "same challenge keeps it");
    c.state.status.as_mut().unwrap().challenges[0].challenge_id = "c2".into();
    frame(
        &mut c,
        &cx,
        egui::vec2(800.0, 700.0),
        &DiscoveryModel::Checking,
        vec![],
    );
    assert!(c.entered_code.is_empty(), "a new challenge starts empty");
}

#[test]
fn forgetting_a_phone_needs_the_confirmation_and_escape_keeps_the_pairing() {
    let cx = ctx();
    let size = egui::vec2(800.0, 700.0);
    let mut s = status();
    s.paired = vec![paired("p1", "Pixel")];
    let (mut c, log) = conn_with(s);
    c.forget = Some(("p1".into(), "Pixel".into()));
    frame(&mut c, &cx, size, &DiscoveryModel::Checking, vec![]);
    assert!(
        !log.lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, Request::Unpair { .. })),
        "opening the dialog forgets nothing"
    );
    frame(
        &mut c,
        &cx,
        size,
        &DiscoveryModel::Checking,
        vec![Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(c.forget.is_none(), "Escape cancels");
    assert!(
        !log.lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, Request::Unpair { .. }))
    );
}

#[test]
fn rename_row_saves_only_valid_changed_names() {
    let cx = ctx();
    let size = egui::vec2(800.0, 400.0);
    let (mut c, log) = conn_with(status());
    frame(&mut c, &cx, size, &DiscoveryModel::Checking, vec![]);
    for draft in ["", "Desk", "bad\u{1b}name", &"x".repeat(50), "New name"] {
        c.name_draft = Some(draft.into());
        for y in (20..120).step_by(6) {
            for x in (200..700).step_by(40) {
                click(
                    &mut c,
                    &cx,
                    size,
                    &DiscoveryModel::Checking,
                    pos2(x as f32, y as f32),
                );
                if c.name_draft.is_none() {
                    break;
                }
            }
        }
    }
    for r in log.lock().unwrap().iter() {
        if let Request::SetDeviceName { name } = r {
            assert!(
                name_problem(name).is_none() && name != "Desk",
                "sent {name:?}"
            );
        }
    }
}

#[test]
fn manual_connect_rejects_bad_text_with_a_notice_and_sends_nothing() {
    let cx = ctx();
    let size = egui::vec2(800.0, 900.0);
    let (mut c, log) = conn_with(status());
    c.manual_address = "nonsense".into();
    c.manual_id = "../x".into();
    for y in (20..900).step_by(8) {
        for x in [60.0, 200.0, 400.0] {
            click(
                &mut c,
                &cx,
                size,
                &DiscoveryModel::Checking,
                pos2(x, y as f32),
            );
            c.manual_address = "nonsense".into();
            c.manual_id = "../x".into();
        }
    }
    assert!(!log.lock().unwrap().iter().any(|r| matches!(
        r,
        Request::Connect {
            endpoint: Some(_),
            ..
        }
    )));
}

// ------------------------------------------------------------- reinstalled phone (same name, new id)

fn paired_at(id: &str, name: &str, at: i64) -> PairedDevice {
    PairedDevice {
        paired_at_unix: at,
        ..paired(id, name)
    }
}

fn shape_text(shapes: &[egui::epaint::ClippedShape]) -> String {
    fn walk(s: &egui::epaint::Shape, out: &mut String) {
        match s {
            egui::epaint::Shape::Text(t) => {
                out.push_str(&t.galley.job.text);
                out.push('\n');
            }
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

fn details_text(c: &mut Connections) -> String {
    let cx = ctx();
    let pal = theme::palette(AppKind::Calls, true);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 700.0),
        )),
        ..Default::default()
    };
    let out = cx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            c.details(ui, &pal, &DiscoveryModel::Checking);
        });
    });
    shape_text(&out.shapes)
}

const STALE_HINT: &str = "A newer link has the same name";

#[test]
fn an_older_offline_entry_with_the_same_name_is_flagged_as_probably_replaced() {
    let list = [
        paired_at("old", "Pixel", 100),
        paired_at("new", " pixel ", 200),
        paired_at("other", "Galaxy", 50),
    ];
    assert!(looks_replaced(&list, &list[0], false));
    assert!(
        !looks_replaced(&list, &list[1], false),
        "the newest is never the stale one"
    );
    assert!(!looks_replaced(&list, &list[2], false), "different name");
    assert!(
        !looks_replaced(&list, &list[0], true),
        "an online entry is in use, not stale"
    );
    assert!(!looks_replaced(
        &[paired_at("a", "", 1), paired_at("b", "", 2)],
        &paired_at("a", "", 1),
        false
    ));
}

#[test]
fn the_phone_list_hints_at_the_old_entry_but_never_unlinks_or_merges_by_itself() {
    let mut s = status();
    s.paired = vec![
        paired_at("old", "Pixel", 100),
        paired_at("new", "Pixel", 200),
    ];
    let (mut c, log) = conn_with(s);
    let text = details_text(&mut c);
    assert_eq!(
        text.matches(STALE_HINT).count(),
        1,
        "one hint, on the old entry only: {text}"
    );
    assert!(text.contains("Unlink…"));
    assert!(c.forget.is_none());
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !log.lock().unwrap().iter().any(|r| matches!(
            r,
            Request::Unpair { .. } | Request::Approve { .. } | Request::SetConsent { .. }
        )),
        "showing the hint changes nothing"
    );
    let mut s = status();
    s.paired = vec![paired_at("a", "Pixel", 1), paired_at("b", "Galaxy", 2)];
    let (mut c, _) = conn_with(s);
    assert!(!details_text(&mut c).contains(STALE_HINT));
}

#[test]
fn a_pairing_prompt_for_a_known_name_explains_the_reinstall_case_and_still_needs_the_typed_code() {
    let cx = ctx();
    let size = egui::vec2(800.0, 700.0);
    let mut s = busy_status();
    let mut ch = challenge("ABCD1234", 60_000);
    ch.device_name = "Pixel".into();
    s.paired = vec![paired_at("old", "Pixel", 100)];
    s.challenges = vec![ch];
    let (mut c, log) = conn_with(s);
    let pal = theme::palette(AppKind::Calls, true);
    let out = dialog_frames(&mut c, &cx, &pal, size);
    let text = shape_text(&out.shapes);
    assert!(text.contains("already linked"), "{text}");
    assert!(text.contains("every sharing option off"));
    // The note is not a shortcut: no code typed, no approval.
    assert!(!code_matches(&c.entered_code, "ABCD1234"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !log.lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, Request::Approve { .. }))
    );
    // A challenge from a name nobody has linked carries no note.
    let mut s = busy_status();
    s.challenges = vec![challenge("ABCD1234", 60_000)];
    let (mut c, _) = conn_with(s);
    let out = dialog_frames(&mut c, &cx, &pal, size);
    assert!(!shape_text(&out.shapes).contains("already linked"));
}

fn dialog_frames(
    c: &mut Connections,
    cx: &egui::Context,
    pal: &Palette,
    size: egui::Vec2,
) -> egui::FullOutput {
    let mut out = None;
    for _ in 0..3 {
        out = Some(cx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            },
            |ctx| c.dialogs(ctx, pal),
        ));
    }
    out.unwrap()
}
