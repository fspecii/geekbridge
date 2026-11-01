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
    };
    kde::Packet::new(link::STATE, r.body())
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
