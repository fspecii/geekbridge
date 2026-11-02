// SPDX-License-Identifier: GPL-3.0-or-later
//! Pairing transitions: the QR window's open/renew/close cycle, one-tap pairing, the "what to do
//! next" card after a new phone appears, and QR encoding limits. A scripted service stands in for
//! the real one; nothing leaves the process.
#![allow(clippy::field_reassign_with_default)]
use super::*;
use bridge_core::{AppKind, ipc::PairedDevice};
use eframe::egui::{Event, Modifiers, PointerButton, pos2};

type Log = Arc<Mutex<Vec<Request>>>;

fn status(paired: &[&str], links: Vec<ipc::LinkStatus>) -> Status {
    Status {
        ipc_version: 1,
        service_version: "t".into(),
        device_id: "x".repeat(32),
        device_name: "Desk".into(),
        own_cert_sha256: "00".repeat(32),
        paired: paired
            .iter()
            .map(|id| PairedDevice {
                device_id: (*id).into(),
                name: format!("Phone {id}"),
                device_type: "phone".into(),
                cert_sha256: String::new(),
                paired_at_unix: 0,
                consents: vec![],
            })
            .collect(),
        links,
        challenges: vec![],
        event_seq: 0,
        inbound_listener: None,
        reconnecting: vec![],
        disconnected: vec![],
    }
}

fn qr(ms: u64) -> ipc::PairQr {
    ipc::PairQr {
        payload: "geekbridge://pair?t=abc&a=192.168.1.2:1716".into(),
        expires_in_ms: ms,
        computer_name: "Desk \u{202e}".into(),
        addresses: vec!["192.168.1.2:1716".into()],
    }
}

fn scripted(s: Arc<Mutex<Status>>, qr_ms: u64) -> (Connections, Log) {
    scripted_with(move || Ok(s.lock().unwrap().clone()), qr_ms)
}

fn scripted_with(
    fetch: impl FnMut() -> Result<Status, String> + Send + 'static,
    qr_ms: u64,
) -> (Connections, Log) {
    let log: Log = Arc::default();
    let sink = log.clone();
    let worker = Worker::spawn_with(
        fetch,
        move |r| {
            sink.lock().unwrap().push(r.clone());
            match r {
                Request::PairQr => Ok(Response::PairQr(Box::new(qr(qr_ms)))),
                _ => Ok(Response::Ack),
            }
        },
        || {},
    );
    (
        Connections {
            worker,
            state: ControlState::default(),
            manual_id: String::new(),
            manual_address: String::new(),
            active_challenge: None,
            entered_code: String::new(),
            forget: None,
            pair: Default::default(),
            name_draft: None,
        },
        log,
    )
}

fn pump(
    c: &mut Connections,
    ctx: &egui::Context,
    size: egui::Vec2,
    events: Vec<Event>,
) -> Option<NextStep> {
    c.update();
    let pal = theme::palette(AppKind::Calls, true);
    let mut step = None;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            step = step.or(c.pair_card(ui, &pal, &DiscoveryModel::Checking));
        });
        c.dialogs(ctx, &pal);
    });
    step
}

fn click(
    c: &mut Connections,
    ctx: &egui::Context,
    size: egui::Vec2,
    at: egui::Pos2,
) -> Option<NextStep> {
    let mut step = pump(c, ctx, size, vec![Event::PointerMoved(at)]);
    for pressed in [true, false] {
        step = step.or(pump(
            c,
            ctx,
            size,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }],
        ));
    }
    step
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    theme::apply(&ctx, AppKind::Calls);
    ctx
}

fn wait(c: &mut Connections, what: &str, ok: impl Fn(&Connections) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !ok(c) {
        c.update();
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn asked(log: &Log, f: impl Fn(&Request) -> bool) -> usize {
    log.lock().unwrap().iter().filter(|r| f(r)).count()
}

#[test]
fn qr_encoding_has_a_size_limit_and_decodes_for_odd_text() {
    assert!(qr_modules("").is_some());
    let (m, w) = qr_modules("héllo 🎉 \u{202e}").unwrap();
    assert_eq!(m.len(), w * w);
    assert!(
        qr_modules(&"x".repeat(10_000)).is_none(),
        "too long is refused, not truncated"
    );
    let (_, small) = qr_modules("a").unwrap();
    let (_, big) = qr_modules(&"a".repeat(800)).unwrap();
    assert!(big > small, "longer payloads use a larger symbol");
}

#[test]
fn the_qr_window_opens_renews_before_expiry_and_closes_on_cancel() {
    let cx = ctx();
    let size = egui::vec2(700.0, 800.0);
    let s = Arc::new(Mutex::new(status(&[], vec![])));
    // A code that is already within the renew margin is replaced instead of shown until it dies.
    let (mut c, log) = scripted(s, 5_000);
    wait(&mut c, "status", |c| c.state.status.is_some());
    for _ in 0..3 {
        pump(&mut c, &cx, size, vec![]);
    }
    // Open by the button the user sees; sweep because the layout is not pinned here.
    'sweep: for y in (30..400).step_by(6) {
        for x in [80.0, 160.0, 240.0] {
            click(&mut c, &cx, size, pos2(x, y as f32));
            if c.pair.qr_open {
                break 'sweep;
            }
        }
    }
    assert!(c.pair.qr_open, "the QR button opens the window");
    wait(&mut c, "qr", |c| c.state.pair_qr.is_some());
    for _ in 0..4 {
        pump(&mut c, &cx, size, vec![]);
    }
    wait(&mut c, "renewal", |_| {
        asked(&log, |r| matches!(r, Request::PairQr)) >= 2
    });
    // Escape cancels the code on the service.
    pump(
        &mut c,
        &cx,
        size,
        vec![Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(!c.pair.qr_open);
    wait(&mut c, "cancel", |_| {
        asked(&log, |r| matches!(r, Request::PairQrCancel)) >= 1
    });
}

#[test]
fn a_new_phone_closes_the_qr_window_and_offers_next_steps_once() {
    let cx = ctx();
    let size = egui::vec2(700.0, 800.0);
    let shared = Arc::new(Mutex::new(status(&["old"], vec![])));
    let (mut c, log) = scripted(shared.clone(), 600_000);
    wait(&mut c, "status", |c| c.state.status.is_some());
    pump(&mut c, &cx, size, vec![]);
    assert!(
        c.pair.known_paired.is_some(),
        "the first status is the baseline, not news"
    );
    assert!(c.pair.next_steps.is_none());
    c.pair.qr_open = true;
    *shared.lock().unwrap() = status(&["old", "new"], vec![]);
    c.refresh();
    wait(&mut c, "new phone", |c| {
        c.state.status.as_ref().is_some_and(|s| s.paired.len() == 2)
    });
    pump(&mut c, &cx, size, vec![]);
    assert_eq!(c.pair.next_steps.as_deref(), Some("Phone new"));
    assert!(!c.pair.qr_open);
    wait(&mut c, "cancel", |_| {
        asked(&log, |r| matches!(r, Request::PairQrCancel)) >= 1
    });
    // The card's three buttons: Clipboard, Share, Done.
    let mut got = Vec::new();
    for y in (30..600).step_by(6) {
        for x in [60.0, 180.0, 300.0] {
            if let Some(step) = click(&mut c, &cx, size, pos2(x, y as f32)) {
                got.push(step);
            }
            if c.pair.next_steps.is_none() {
                break;
            }
        }
        if c.pair.next_steps.is_none() {
            break;
        }
    }
    assert!(
        c.pair.next_steps.is_none() || !got.is_empty(),
        "some button of the card was reached"
    );
}

#[test]
fn one_tap_pair_connects_first_then_pairs_when_the_link_reports_unpaired() {
    let cx = ctx();
    let size = egui::vec2(700.0, 800.0);
    let shared = Arc::new(Mutex::new(status(&[], vec![])));
    let (mut c, log) = scripted(shared.clone(), 600_000);
    wait(&mut c, "status", |c| c.state.status.is_some());
    c.one_tap_pair("near");
    wait(&mut c, "connect", |_| {
        asked(
            &log,
            |r| matches!(r, Request::Connect { device_id, .. } if device_id == "near"),
        ) == 1
    });
    // Still connecting: nothing else is sent.
    pump(&mut c, &cx, size, vec![]);
    assert_eq!(asked(&log, |r| matches!(r, Request::Pair { .. })), 0);
    let link = ipc::LinkStatus {
        device_id: "near".into(),
        name: "Nearby".into(),
        phase: LinkPhase::Unpaired,
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
    };
    *shared.lock().unwrap() = status(&[], vec![link]);
    c.refresh();
    wait(&mut c, "unpaired link", |c| {
        c.state.status.as_ref().is_some_and(|s| !s.links.is_empty())
    });
    for _ in 0..3 {
        pump(&mut c, &cx, size, vec![]);
        c.update();
    }
    wait(&mut c, "pair", |_| {
        asked(
            &log,
            |r| matches!(r, Request::Pair { device_id, .. } if device_id == "near"),
        ) >= 1
    });
    // It is a one-shot: the phone is not asked again.
    for _ in 0..4 {
        pump(&mut c, &cx, size, vec![]);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(asked(&log, |r| matches!(r, Request::Pair { .. })), 1);
}

#[test]
fn the_card_with_no_service_disables_pairing_buttons() {
    let cx = ctx();
    let size = egui::vec2(500.0, 600.0);
    // The service never answers, so no status can arrive between clicks (a status that did
    // arrive on a fast poll made this flaky on slower runners).
    let (mut c, log) = scripted_with(|| Err("no service".into()), 1);
    c.state.status = None;
    for y in (20..300).step_by(10) {
        for x in [60.0, 150.0, 260.0] {
            click(&mut c, &cx, size, pos2(x, y as f32));
            c.state.status = None;
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        asked(&log, |r| matches!(
            r,
            Request::PairQr | Request::TestConnection { .. }
        )),
        0
    );
}
