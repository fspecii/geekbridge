// SPDX-License-Identifier: GPL-3.0-or-later
//! `RestartIfIdle`: the service decides, under the call hub's lock, whether it may leave for an update.
//! Sandbox services on private ports only; the supervisor check is replaced so nothing here can touch systemd.
#![cfg(target_os = "linux")]

mod support;

use bridge_core::calls_link::{self as link, LinkCallReport, PhoneState};
use bridge_core::ipc::{ErrorCode, Feature, Hello, IpcError, Request, Response, Token};
use bridge_kde as kde;
use bridge_update::install::{Error, IpcProbe, Probe};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use support::{Phone, Rig, pair_phone};
use tokio::time::timeout;

/// A phone that speaks the calls link and accepts shared files.
fn phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        vec![
            "kdeconnect.ping".into(),
            "kdeconnect.share.request".into(),
            link::REQUEST.into(),
        ],
        vec![
            "kdeconnect.ping".into(),
            link::STATE.into(),
            link::RESULT.into(),
        ],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn under_a_unit() -> Result<(), String> {
    Ok(())
}

async fn leaves(rig: &Rig) -> bool {
    timeout(Duration::from_millis(900), rig.daemon.restart_wanted())
        .await
        .is_ok()
}

async fn ask(rig: &Rig, force: bool) -> Result<Response, IpcError> {
    rig.call(Request::RestartIfIdle { force }).await
}

fn refusal(r: Result<Response, IpcError>) -> (ErrorCode, String) {
    match r {
        Err(IpcError::Service(e)) => (e.code, e.message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn state(s: PhoneState) -> kde::Packet {
    let mut r = LinkCallReport::idle(true, true);
    r.state = s;
    kde::Packet::new(link::STATE, r.body())
}

#[tokio::test]
async fn idle_service_leaves_a_ringing_call_refuses_and_a_launch_without_a_unit_is_left_alone() {
    // Started by hand (not the systemd unit): never leaves, says to restart it yourself.
    let manual = Rig::start().await;
    let (code, why) = refusal(ask(&manual, false).await);
    assert_eq!(code, ErrorCode::Unsupported);
    assert!(why.contains("restart it yourself"), "{why}");
    assert!(
        !leaves(&manual).await,
        "a manual launch must not be asked to exit"
    );

    let rig = Rig::start_with(|o| o.supervisor = under_a_unit).await;
    let phone = phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    // Connected, Calls allowed, no report yet: the call state is unknown, so no restart.
    let (code, why) = refusal(ask(&rig, false).await);
    assert_eq!(code, ErrorCode::Busy);
    assert!(why.contains("not reported"), "{why}");
    phone_link.send(&state(PhoneState::Idle)).await.unwrap();
    // A ring: refused, and force does not override a call.
    phone_link.send(&state(PhoneState::Ringing)).await.unwrap();
    wait_rows(&rig, true).await;
    for force in [false, true] {
        let (code, why) = refusal(ask(&rig, force).await);
        assert_eq!(
            (code, why.contains("call")),
            (ErrorCode::Busy, true),
            "{why}"
        );
    }
    // An active call: refused.
    phone_link.send(&state(PhoneState::OffHook)).await.unwrap();
    assert_eq!(refusal(ask(&rig, false).await).0, ErrorCode::Busy);
    assert!(
        !leaves(&rig).await,
        "a refused request must not stop the service"
    );
    // Idle again: the service accepts and leaves.
    phone_link.send(&state(PhoneState::Idle)).await.unwrap();
    wait_rows(&rig, false).await;
    assert!(matches!(ask(&rig, false).await, Ok(Response::Ack)));
    assert!(leaves(&rig).await);
}

async fn wait_rows(rig: &Rig, want: bool) {
    for _ in 0..200 {
        if let Ok(Response::Calls(c)) = rig.call(Request::Calls).await
            && c.view.rows.is_empty() != want
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the call view never became {want}");
}

#[tokio::test]
async fn a_file_in_flight_stops_a_restart_unless_forced() {
    let rig = Rig::start_with(|o| o.supervisor = under_a_unit).await;
    let phone = phone();
    let _link = pair_phone(&rig, &phone).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.bin");
    std::fs::write(&file, vec![7_u8; 100_000]).unwrap();
    let sent = rig
        .call(Request::ShareSendFile {
            device_id: phone.id(),
            path: file.display().to_string(),
        })
        .await;
    assert!(
        matches!(sent, Ok(Response::TransferStarted { .. })),
        "{sent:?}"
    );
    let (code, why) = refusal(ask(&rig, false).await);
    assert_eq!(code, ErrorCode::Busy);
    assert!(why.contains("file"), "{why}");
    assert!(!leaves(&rig).await);
    assert!(matches!(ask(&rig, true).await, Ok(Response::Ack)));
    assert!(leaves(&rig).await);
}

#[test]
fn requests_survive_old_and_new_peers() {
    // An older client never sends it; a newer one may leave `force` out.
    let r: Request = serde_json::from_str(r#"{"op":"restart_if_idle"}"#).unwrap();
    assert_eq!(r, Request::RestartIfIdle { force: false });
    let r: Request = serde_json::from_str(r#"{"op":"restart_if_idle","force":true}"#).unwrap();
    assert_eq!(r, Request::RestartIfIdle { force: true });
    // What an older service does with it: an unrecognised op fails to parse (it answers BadRequest).
    assert!(serde_json::from_str::<Request>(r#"{"op":"restart_if_idle_v9"}"#).is_err());
    assert!(serde_json::from_str::<Request>(r#"{"op":"restart_if_idle","nope":1}"#).is_err());
}

/// A service from before `RestartIfIdle`: answers it with `BadRequest`, and `Calls` with an error (so the
/// two-step fallback stops at "cannot tell" and never reaches systemctl).
fn old_service(token: Token) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (n, conn) in listener.incoming().take(2).enumerate() {
            let mut conn = conn.unwrap();
            let nonce = format!("{n:064x}");
            let hello = bridge_core::ipc::format_hello(&Hello::Ready {
                nonce: nonce.clone(),
            });
            conn.write_all(hello.as_bytes()).unwrap();
            let mut line = Vec::new();
            BufReader::new(conn.try_clone().unwrap())
                .read_until(b'\n', &mut line)
                .unwrap();
            line.pop();
            let (mac, body) = bridge_core::ipc::split_frame(&line).unwrap();
            let body_text = String::from_utf8_lossy(body).into_owned();
            let (code, message) = if body_text.contains("restart_if_idle") {
                ("bad_request", "unrecognised request")
            } else {
                ("failed", "no calls here")
            };
            let json = format!(r#"{{"kind":"error","code":"{code}","message":"{message}"}}"#);
            let reply = token.response_mac(&nonce, &mac, json.as_bytes());
            writeln!(conn, "{} {json}", bridge_core::ipc::encode_hex(&reply)).unwrap();
        }
    });
    port
}

#[test]
fn a_new_ctl_falls_back_to_the_two_step_form_against_an_old_service() {
    let token = Token::from_bytes([9; 32]);
    let port = old_service(token.clone());
    let probe = IpcProbe {
        port,
        token,
        force: false,
    };
    match probe.restart_if_idle() {
        Err(Error::CallInProgress(m)) => assert!(m.contains("cannot tell whether a call"), "{m}"),
        other => panic!("expected the safe refusal, got {other:?}"),
    }
}
