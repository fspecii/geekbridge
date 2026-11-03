// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end call control over the GeekBridge link: a real `Daemon` and a stand-in phone that is a
//! real `bridge_kde` endpoint (TLS over loopback) sending `geekbridge.calls.state` and executing
//! `geekbridge.calls.request`.
//!
//! This shows the service side of the protocol works against its own transport. It says nothing
//! about Android's `TelecomManager`, Bluetooth HFP or call audio.

mod support;

use bridge_core::calls::CallPhase;
use bridge_core::calls_link::{self as link, LinkAction, LinkCallReport, LinkFailure, PhoneState};
use bridge_core::calls_view::ControlPath;
use bridge_core::ipc::{CallAction, ErrorCode, Feature, Request, Response};
use bridge_kde::{self as kde};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{Phone, Rig, pair_phone};
use tokio::time::{sleep, timeout};

fn calls_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        vec!["kdeconnect.ping".into(), link::REQUEST.into()],
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

fn report(state: PhoneState, can_answer: bool) -> kde::Packet {
    let r = LinkCallReport {
        state,
        number: matches!(state, PhoneState::Ringing)
            .then(|| bridge_core::calls::Sensitive::new("+15550100")),
        name: matches!(state, PhoneState::Ringing)
            .then(|| bridge_core::calls::Sensitive::new("Ada")),
        can_answer,
        can_end: can_answer,
        can_dial: false,
    };
    kde::Packet::new(link::STATE, r.body())
}

fn idle_report(can_dial: bool) -> kde::Packet {
    let mut r = LinkCallReport::idle(true, true);
    r.can_dial = can_dial;
    kde::Packet::new(link::STATE, r.body())
}

/// Wait for the next request the service sent the stand-in phone, with its raw body.
async fn next_packet(phone_link: &mut kde::TrustedLink) -> kde::Packet {
    loop {
        let event = timeout(Duration::from_secs(5), phone_link.recv())
            .await
            .expect("request not sent")
            .unwrap();
        if let kde::LinkEvent::Packet(p) = event
            && p.kind == link::REQUEST
            // The "send your state again" ask that follows switching Telephony on is not a control.
            && p.body.get("action").and_then(|a| a.as_str()) != Some("report")
        {
            return p;
        }
    }
}

/// True if the service sends the phone no request within a short while.
async fn sends_nothing(phone_link: &mut kde::TrustedLink) -> bool {
    timeout(Duration::from_millis(400), async {
        loop {
            if let Ok(kde::LinkEvent::Packet(p)) = phone_link.recv().await
                && p.kind == link::REQUEST
                && p.body.get("action").and_then(|a| a.as_str()) != Some("report")
            {
                return;
            }
        }
    })
    .await
    .is_err()
}

async fn calls(rig: &Rig) -> bridge_core::ipc::CallsStatus {
    match rig.call(Request::Calls).await.unwrap() {
        Response::Calls(c) => *c,
        other => panic!("expected calls, got {other:?}"),
    }
}

async fn wait_calls(
    rig: &Rig,
    what: &str,
    cond: impl Fn(&bridge_core::ipc::CallsStatus) -> bool,
) -> bridge_core::ipc::CallsStatus {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let c = calls(rig).await;
        if cond(&c) {
            return c;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {c:?}");
        sleep(Duration::from_millis(25)).await;
    }
}

async fn next_request(phone_link: &mut kde::TrustedLink) -> (String, LinkAction) {
    loop {
        let event = timeout(Duration::from_secs(5), phone_link.recv())
            .await
            .expect("request not sent")
            .unwrap();
        if let kde::LinkEvent::Packet(p) = event
            && p.kind == link::REQUEST
            && p.body.get("action").and_then(|a| a.as_str()) != Some("report")
        {
            return link::parse_request(&p.body).unwrap();
        }
    }
}

#[tokio::test]
async fn answer_reject_and_hangup_over_the_phone_link() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;

    // Without the per-device consent the phone's report is ignored.
    phone_link
        .send(&report(PhoneState::Ringing, true))
        .await
        .unwrap();
    sleep(Duration::from_millis(150)).await;
    let c = calls(&rig).await;
    assert!(c.view.rows.is_empty());
    assert!(c.sources.iter().any(|l| l.contains("allow Calls")));

    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    // Switching Telephony on asks the phone for the state it sent while the consent was off.
    let ask = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(kde::LinkEvent::Packet(p)) = phone_link.recv().await
                && p.kind == link::REQUEST
            {
                return p;
            }
        }
    })
    .await
    .expect("no report request after the consent was switched on");
    assert_eq!(
        link::parse_request(&ask.body).unwrap().1,
        LinkAction::Report
    );
    phone_link
        .send(&report(PhoneState::Ringing, true))
        .await
        .unwrap();
    let c = wait_calls(&rig, "ringing call", |c| c.view.ringing().is_some()).await;
    let row = c.view.ringing().unwrap().clone();
    assert_eq!(row.caller_label(), "Ada");
    assert_eq!(row.answer.as_ref().unwrap().path, ControlPath::Link);

    // Decline first: the phone is asked to reject, then reports idle.
    rig.ok(Request::CallAction {
        target: row.reject.clone().unwrap(),
        action: CallAction::Reject,
    })
    .await;
    let (id, action) = next_request(&mut phone_link).await;
    assert_eq!(action, LinkAction::Reject);
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Ok(())),
        ))
        .await
        .unwrap();
    phone_link
        .send(&report(PhoneState::Idle, true))
        .await
        .unwrap();
    wait_calls(&rig, "idle", |c| c.view.rows.is_empty()).await;

    // A second call: answer it, then hang up.
    phone_link
        .send(&report(PhoneState::Ringing, true))
        .await
        .unwrap();
    let c = wait_calls(&rig, "second ring", |c| c.view.ringing().is_some()).await;
    let answer = c.view.ringing().unwrap().answer.clone().unwrap();
    rig.ok(Request::CallAction {
        target: answer,
        action: CallAction::Answer,
    })
    .await;
    let (id, action) = next_request(&mut phone_link).await;
    assert_eq!(action, LinkAction::Answer);
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Ok(())),
        ))
        .await
        .unwrap();
    phone_link
        .send(&report(PhoneState::OffHook, true))
        .await
        .unwrap();
    let c = wait_calls(&rig, "live call", |c| c.view.live().is_some()).await;
    assert_eq!(c.view.live().unwrap().phase, CallPhase::Active);
    // No Bluetooth hands-free backend in this rig: the audio honestly stays on the phone.
    assert!(
        c.view
            .audio
            .hint
            .as_deref()
            .unwrap()
            .contains("Answered on the phone")
    );
    assert!(c.view.mute.is_none());
    rig.ok(Request::CallAction {
        target: c.view.live().unwrap().hangup.clone().unwrap(),
        action: CallAction::Hangup,
    })
    .await;
    assert_eq!(next_request(&mut phone_link).await.1, LinkAction::Hangup);
    phone_link
        .send(&report(PhoneState::Idle, true))
        .await
        .unwrap();
    wait_calls(&rig, "idle again", |c| c.view.rows.is_empty()).await;
}

#[tokio::test]
async fn phone_without_permission_gets_no_answer_button_and_refusals_are_shown() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    phone_link
        .send(&report(PhoneState::Ringing, false))
        .await
        .unwrap();
    let c = wait_calls(&rig, "ringing", |c| c.view.ringing().is_some()).await;
    let row = c.view.ringing().unwrap();
    assert!(row.answer.is_none() && row.reject.is_none());
    assert!(row.missing_control.is_some());
    // A forged target is refused without anything reaching the phone.
    let forged = bridge_core::calls_view::ActionTarget {
        source: format!("link:{}", phone.id()),
        call: bridge_core::calls::CallId::new(link::LINK_CALL_ID),
        path: ControlPath::Link,
    };
    assert_eq!(
        rig.refused(Request::CallAction {
            target: forged,
            action: CallAction::Answer
        })
        .await,
        ErrorCode::NotFound
    );

    // The phone later grants the permission; its next report enables the controls.
    phone_link
        .send(&report(PhoneState::Ringing, true))
        .await
        .unwrap();
    let c = wait_calls(&rig, "answer offered", |c| {
        c.view.ringing().is_some_and(|r| r.answer.is_some())
    })
    .await;
    rig.ok(Request::CallAction {
        target: c.view.ringing().unwrap().answer.clone().unwrap(),
        action: CallAction::Answer,
    })
    .await;
    let (id, _) = next_request(&mut phone_link).await;
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Err(LinkFailure::PermissionMissing)),
        ))
        .await
        .unwrap();
    let c = wait_calls(&rig, "notice", |c| c.notice.is_some()).await;
    assert!(c.notice.unwrap().contains("not allowed"));
}

#[tokio::test]
async fn malformed_call_packets_do_not_create_calls() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    for body in [
        serde_json::json!({"state":"bogus"}),
        serde_json::json!({"state":"ringing","number": 7}),
        serde_json::json!({}),
    ] {
        let map = body.as_object().unwrap().clone();
        phone_link
            .send(&kde::Packet::new(link::STATE, map))
            .await
            .unwrap();
    }
    sleep(Duration::from_millis(200)).await;
    assert!(calls(&rig).await.view.rows.is_empty());
}

/// Dialing from the PC over the phone link only. The stand-in phone is a loopback TLS endpoint, so
/// no call is ever placed anywhere; the numbers below are fictional.
#[tokio::test]
async fn dial_needs_consent_the_phones_permission_and_a_plain_number() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    let source = format!("link:{}", phone.id());
    let dial = |number: &str, source: &str| Request::CallDial {
        source: source.to_owned(),
        number: bridge_core::calls::Sensitive::new(number),
    };

    // No Telephony consent: the report is ignored, nothing is offered, nothing is sent.
    phone_link.send(&idle_report(true)).await.unwrap();
    sleep(Duration::from_millis(150)).await;
    assert!(calls(&rig).await.view.dial.is_none());
    assert_eq!(
        rig.refused(dial("+15550100", &source)).await,
        ErrorCode::NotFound
    );
    assert!(sends_nothing(&mut phone_link).await);

    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    // Consent on, but the phone has not allowed placing calls (canDial false).
    phone_link.send(&idle_report(false)).await.unwrap();
    let c = wait_calls(&rig, "idle report", |c| !c.sources.is_empty()).await;
    assert!(c.view.dial.is_none());
    assert!(c.sources.iter().any(|l| l.contains("to dial from here")));
    assert_eq!(
        rig.refused(dial("+15550100", &source)).await,
        ErrorCode::NotFound
    );
    assert!(sends_nothing(&mut phone_link).await);

    // The phone allows it.
    phone_link.send(&idle_report(true)).await.unwrap();
    let c = wait_calls(&rig, "dial offered", |c| c.view.dial.is_some()).await;
    let offered = c.view.dial.unwrap();
    assert_eq!(
        (offered.source.as_str(), offered.path),
        (source.as_str(), ControlPath::Link)
    );

    // MMI/USSD-style and malformed numbers are refused before anything leaves the computer.
    for bad in [
        "*#06#",
        "*21*5550100#",
        "555 0100",
        "",
        "++1",
        "12345678901234567890123",
    ] {
        assert_eq!(
            rig.refused(dial(bad, &source)).await,
            ErrorCode::BadRequest,
            "{bad}"
        );
    }
    // A source the view did not offer is refused.
    assert_eq!(
        rig.refused(dial("5550100", "link:someone-else")).await,
        ErrorCode::NotFound
    );
    assert!(sends_nothing(&mut phone_link).await);

    // The real thing: the phone is asked to dial exactly this number.
    rig.ok(dial("+15550100", &source)).await;
    let p = next_packet(&mut phone_link).await;
    let (id, action) = link::parse_request(&p.body).unwrap();
    assert_eq!(action, LinkAction::Dial);
    assert_eq!(
        link::parse_dial_number(&p.body).unwrap().expose(),
        "+15550100"
    );
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Ok(())),
        ))
        .await
        .unwrap();
    phone_link
        .send(&report(PhoneState::OffHook, true))
        .await
        .unwrap();
    let c = wait_calls(&rig, "outgoing call is live", |c| c.view.live().is_some()).await;
    assert!(c.view.dial.is_none(), "no second call on top of a live one");
    assert_eq!(
        rig.refused(dial("5550100", &source)).await,
        ErrorCode::NotFound
    );
    assert!(sends_nothing(&mut phone_link).await);

    // The phone refuses (permission revoked meanwhile): the reason is shown.
    phone_link
        .send(&report(PhoneState::Idle, true))
        .await
        .unwrap();
    wait_calls(&rig, "idle", |c| c.view.rows.is_empty()).await;
    phone_link.send(&idle_report(true)).await.unwrap();
    wait_calls(&rig, "dial offered again", |c| c.view.dial.is_some()).await;
    rig.ok(dial("5550101", &source)).await;
    let p = next_packet(&mut phone_link).await;
    let (id, _) = link::parse_request(&p.body).unwrap();
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Err(LinkFailure::PermissionMissing)),
        ))
        .await
        .unwrap();
    let c = wait_calls(&rig, "refusal shown", |c| c.notice.is_some()).await;
    assert!(c.notice.unwrap().contains("not allowed"));
}

/// `geekbridge-ctl call dial` against the in-process daemon: private state dir, port and HOME, the
/// stand-in phone as the only "network". Fictional numbers; nothing is ever placed.
#[tokio::test]
async fn ctl_call_dial_explains_asks_to_confirm_and_then_dials_through_the_link() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    let (state, port) = (rig.state_dir.clone(), rig.control_port);
    let home = tempfile::tempdir().unwrap();
    let home_path = home.path().to_owned();
    let ctl = move |args: Vec<&'static str>| {
        let (state, home_path) = (state.clone(), home_path.clone());
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_geekbridge-ctl"))
                .args(args)
                .env("GEEKBRIDGE_STATE_DIR", &state)
                .env("GEEKBRIDGE_CONTROL_PORT", port.to_string())
                .env("HOME", &home_path)
                .env("XDG_CONFIG_HOME", home_path.join(".config"))
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        })
    };

    // No consent yet: the command says why and what to do, and sends nothing.
    phone_link.send(&idle_report(true)).await.unwrap();
    let (ok, text) = ctl(vec!["call", "dial", "+15550100", "--yes"])
        .await
        .unwrap();
    assert!(
        !ok && text.contains("not available") && text.contains("consent"),
        "{text}"
    );
    assert!(sends_nothing(&mut phone_link).await);

    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    phone_link.send(&idle_report(true)).await.unwrap();
    wait_calls(&rig, "dial offered", |c| c.view.dial.is_some()).await;

    // Bad numbers never reach the service; without a terminal, no --yes means no dial.
    let (ok, text) = ctl(vec!["call", "dial", "*#06#", "--yes"]).await.unwrap();
    assert!(!ok && text.contains("digits"), "{text}");
    let (ok, text) = ctl(vec!["call", "dial", "+15550100"]).await.unwrap();
    assert!(!ok && text.contains("without confirmation"), "{text}");
    assert!(sends_nothing(&mut phone_link).await);

    let (ok, text) = ctl(vec!["call", "dial", "+15550100", "--yes"])
        .await
        .unwrap();
    assert!(ok, "{text}");
    let p = next_packet(&mut phone_link).await;
    assert_eq!(link::parse_request(&p.body).unwrap().1, LinkAction::Dial);
    assert_eq!(
        link::parse_dial_number(&p.body).unwrap().expose(),
        "+15550100"
    );
}

// ---- test-gaps-4: the remaining `geekbridge-ctl calls` / `call ...` commands against a real service

fn ctl_runner(
    rig: &Rig,
) -> impl Fn(Vec<&'static str>) -> tokio::task::JoinHandle<(bool, String)> + use<> {
    let (state, port) = (rig.state_dir.clone(), rig.control_port);
    let home = Arc::new(tempfile::tempdir().unwrap());
    move |args: Vec<&'static str>| {
        let (state, home) = (state.clone(), home.clone());
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_geekbridge-ctl"))
                .args(args)
                .env("GEEKBRIDGE_STATE_DIR", &state)
                .env("GEEKBRIDGE_CONTROL_PORT", port.to_string())
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join(".config"))
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            (
                out.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        })
    }
}

#[tokio::test]
async fn ctl_calls_masks_numbers_and_each_control_says_plainly_when_it_is_missing() {
    let rig = Rig::start().await;
    let phone = calls_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    let ctl = ctl_runner(&rig);

    // Nothing going on: every control explains itself and sends nothing.
    let (ok, text) = ctl(vec!["calls"]).await.unwrap();
    assert!(ok && text.starts_with("no calls"), "{text}");
    for (args, said) in [
        (
            vec!["call", "answer"],
            "nothing can answer a call right now",
        ),
        (
            vec!["call", "reject"],
            "nothing can decline a call right now",
        ),
        (vec!["call", "hangup"], "nothing can end a call right now"),
        (vec!["call", "mute", "on"], "muting is not available"),
        (
            vec!["call", "audio"],
            "moving the audio here is not available",
        ),
        (vec!["call", "bogus"], "unknown call command"),
        (vec!["call", "dial"], "unknown call command"),
        (vec!["call", "dial", "1", "2"], "unknown call command"),
        (vec!["nonsense"], "unknown"),
    ] {
        let (ok, text) = ctl(args.clone()).await.unwrap();
        assert!(!ok && text.contains(said), "{args:?}: {text}");
    }
    assert!(sends_nothing(&mut phone_link).await);

    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    phone_link
        .send(&report(PhoneState::Ringing, true))
        .await
        .unwrap();
    wait_calls(&rig, "ringing", |c| c.view.ringing().is_some()).await;

    // Numbers are masked unless asked for; names are never printed.
    let (ok, text) = ctl(vec!["calls"]).await.unwrap();
    assert!(ok && text.contains("ringing | number: *******00"), "{text}");
    assert!(
        !text.contains("+15550100") && !text.contains("Ada"),
        "{text}"
    );
    assert!(
        text.contains("answer: phone-link reject: phone-link"),
        "{text}"
    );
    let (_, text) = ctl(vec!["calls", "--reveal"]).await.unwrap();
    assert!(
        text.contains("number: +15550100") && !text.contains("Ada"),
        "{text}"
    );
    // Mute and audio stay unavailable on the link path; ending a ringing call is not "hang up".
    let (ok, text) = ctl(vec!["call", "hangup"]).await.unwrap();
    assert!(!ok && text.contains("nothing can end a call"), "{text}");
    let (ok, text) = ctl(vec!["call", "mute", "maybe"]).await.unwrap();
    assert!(!ok && text.contains("muting is not available"), "{text}");
    assert!(sends_nothing(&mut phone_link).await);

    // Reject reaches the phone as exactly one request, and the command says ok.
    let (ok, text) = ctl(vec!["call", "reject"]).await.unwrap();
    assert!(ok && text.trim() == "ok", "{text}");
    let (id, action) = next_request(&mut phone_link).await;
    assert_eq!(action, LinkAction::Reject);
    assert!(sends_nothing(&mut phone_link).await);
    // The phone's refusal comes back as the next `calls` notice.
    phone_link
        .send(&kde::Packet::new(
            link::RESULT,
            link::result_body(&id, Err(LinkFailure::NoSuchCall)),
        ))
        .await
        .unwrap();
    wait_calls(&rig, "notice", |c| c.notice.is_some()).await;
    let (_, text) = ctl(vec!["calls"]).await.unwrap();
    assert!(
        text.contains("notice: The call had already changed on the phone."),
        "{text}"
    );
    assert!(text.contains("source: Phone link ("), "{text}");
}
