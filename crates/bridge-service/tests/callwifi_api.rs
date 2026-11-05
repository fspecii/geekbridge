// SPDX-License-Identifier: GPL-3.0-or-later
//! EXPERIMENTAL Wi-Fi call audio through a real `Daemon`: a stand-in phone that is a real `bridge_kde` endpoint
//! (TLS over loopback) declares the `geekbridge.callaudio.*` packets, sends `ready`, offers a session and sends
//! Opus frames; the sound output is a stand-in so nothing plays on a real speaker.
//!
//! It shows the service side of the protocol works against its own transport: capability negotiation, the
//! consent gate, the press, the offer and answer, frames reaching the output, and every stop path. It says nothing
//! about Android, the helper, or whether any phone lets its call audio be read.

mod support;

use bridge_callwifi::codec::Encoder;
use bridge_callwifi::selftest::Verdict;
use bridge_callwifi::tone::{Tone, rms};
use bridge_callwifi::wire::{self, Dir, Frame, Msg, Offer, Ready, Uplink};
use bridge_core::call_wifi_view::{CallWifiAction, CallWifiView};
use bridge_core::calls_link::{self as link, LinkCallReport, PhoneState};
use bridge_core::ipc::{Feature, Request, Response};
use bridge_kde::{self as kde};
use bridge_service::callwifi::{Audio, Capture, Playback};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{Phone, Rig, pair_phone};
use tokio::time::{sleep, timeout};

#[derive(Default)]
struct Speaker {
    played: Arc<Mutex<Vec<i16>>>,
}
struct Out(Arc<Mutex<Vec<i16>>>);
impl Playback for Out {
    fn write(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        self.0.lock().unwrap().extend_from_slice(pcm);
        Ok(())
    }
    fn close(&mut self) {}
}
impl Audio for Speaker {
    fn supported(&self) -> Result<(), String> {
        Ok(())
    }
    fn describe(&self) -> (Option<String>, Option<String>) {
        (Some("Test headphones".into()), None)
    }
    fn open_playback(&self) -> Result<Box<dyn Playback>, String> {
        Ok(Box::new(Out(self.played.clone())))
    }
    fn open_capture(&self) -> Result<Box<dyn Capture>, String> {
        Err("no microphone in this test".into())
    }
}

fn phone_with_callwifi() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Test Phone",
        kde::DeviceType::Phone,
        // What the phone accepts.
        [link::REQUEST]
            .into_iter()
            .chain(wire::PHONE_INCOMING)
            .chain(["kdeconnect.ping"])
            .map(String::from)
            .collect(),
        // What the phone sends.
        [link::STATE, link::RESULT]
            .into_iter()
            .chain(wire::COMPUTER_INCOMING)
            .chain(["kdeconnect.ping"])
            .map(String::from)
            .collect(),
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

fn packet(m: &Msg) -> kde::Packet {
    kde::Packet::new(m.kind(), wire::body(m))
}

async fn view(rig: &Rig, action: CallWifiAction) -> CallWifiView {
    match rig.call(Request::CallWifi { action }).await.unwrap() {
        Response::CallWifi(v) => *v,
        other => panic!("expected call_wifi, got {other:?}"),
    }
}

async fn wait_view(rig: &Rig, what: &str, cond: impl Fn(&CallWifiView) -> bool) -> CallWifiView {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        let v = view(rig, CallWifiAction::Status).await;
        if cond(&v) {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {v:?}");
        sleep(Duration::from_millis(25)).await;
    }
}

/// The next callaudio packet the service sent the phone.
async fn next_msg(phone_link: &mut kde::TrustedLink) -> Msg {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(kde::LinkEvent::Packet(p)) = phone_link.recv().await
                && wire::is_callaudio_kind(&p.kind)
            {
                return wire::parse(&p.kind, &p.body).expect("a well-formed packet");
            }
        }
    })
    .await
    .expect("no callaudio packet from the service")
}

/// The next packet that is not the once-a-second receiver report.
async fn next_msg_skipping_stats(phone_link: &mut kde::TrustedLink) -> Msg {
    loop {
        let m = next_msg(phone_link).await;
        if !matches!(m, Msg::Stats(_)) {
            return m;
        }
    }
}

fn off_hook() -> kde::Packet {
    kde::Packet::new(
        link::STATE,
        LinkCallReport {
            state: PhoneState::OffHook,
            number: None,
            name: None,
            can_answer: true,
            can_end: true,
            can_dial: false,
            can_tones: false,
        }
        .body(),
    )
}

fn ready() -> Msg {
    Msg::Ready(Ready {
        ready: true,
        listen: true,
        speak: false,
        test: Verdict::Works,
    })
}

#[tokio::test]
async fn a_press_an_offer_and_frames_play_and_every_stop_path_ends_it() {
    let speaker = Arc::new(Speaker::default());
    let played = speaker.played.clone();
    let rig = Rig::start_with(|o| {
        *o.callwifi_audio.get_mut().unwrap() = Some(speaker.clone());
    })
    .await;
    let phone = phone_with_callwifi();
    let mut phone_link = pair_phone(&rig, &phone).await;
    let id = phone.id();

    // Off by default: listed, not allowed, and its ready packet is dropped unread.
    phone_link.send(&packet(&ready())).await.unwrap();
    sleep(Duration::from_millis(150)).await;
    let v = view(&rig, CallWifiAction::Status).await;
    assert!(v.supported);
    assert_eq!(v.phones.len(), 1);
    assert!(!v.phones[0].allowed && !v.phones[0].ready);
    // Pressing anyway is refused with the way to allow it.
    let e = rig
        .call(Request::CallWifi {
            action: CallWifiAction::Listen {
                device_id: id.clone(),
            },
        })
        .await;
    assert!(
        format!("{e:?}").contains("call_audio_wifi on"),
        "refusal names the consent: {e:?}"
    );

    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::CallAudioWifi,
        enabled: true,
    })
    .await;
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::Telephony,
        enabled: true,
    })
    .await;
    phone_link.send(&packet(&ready())).await.unwrap();
    phone_link.send(&off_hook()).await.unwrap();
    let v = wait_view(&rig, "ready and in a call", |v| {
        v.phones.first().is_some_and(|p| p.listen && p.in_call)
    })
    .await;
    assert!(!v.phones[0].speak, "no uplink was reported");
    // The call card's one status poll carries it too.
    match rig.call(Request::Calls).await.unwrap() {
        Response::Calls(c) => assert!(c.wifi.is_some_and(|w| w.phones[0].listen)),
        other => panic!("{other:?}"),
    }

    // Press: the phone is asked, offers, the service accepts.
    let v = view(
        &rig,
        CallWifiAction::Listen {
            device_id: id.clone(),
        },
    )
    .await;
    assert_eq!(v.session.as_ref().unwrap().state, "starting");
    assert_eq!(v.output.as_deref(), Some("Test headphones"));
    let Msg::Request { session } = next_msg(&mut phone_link).await else {
        panic!("expected request")
    };
    phone_link
        .send(&packet(&Msg::Offer(Offer {
            session,
            down: true,
            up: false,
            uplink: Uplink::None,
        })))
        .await
        .unwrap();
    assert!(
        matches!(next_msg(&mut phone_link).await, Msg::Answer(a) if a.accept && !a.up),
        "offer accepted for listening only"
    );
    wait_view(&rig, "live", |v| {
        v.session.as_ref().is_some_and(|s| s.state == "live")
    })
    .await;

    // The far party's voice reaches the output.
    let (mut enc, mut tone) = (Encoder::new().unwrap(), Tone::new(440.0, 8000.0));
    for seq in 0..40u32 {
        let opus = enc.encode(&tone.frame()).unwrap();
        phone_link
            .send(&packet(&Msg::Frame(Frame {
                session,
                dir: Dir::Down,
                seq,
                ts: seq * 320,
                cap: 0,
                opus,
            })))
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
    }
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let loud = {
            let p = played.lock().unwrap();
            p.len() > 3200 && p.windows(1600).step_by(1600).any(|c| rms(c) > 1000.0)
        };
        if loud {
            break;
        }
        assert!(Instant::now() < end, "the tone never reached the output");
        sleep(Duration::from_millis(20)).await;
    }
    assert!(
        wait_view(&rig, "frames counted", |v| v
            .session
            .as_ref()
            .is_some_and(|s| s.frames >= 30))
        .await
        .session
        .is_some()
    );

    // Stop from the computer reaches the phone.
    view(&rig, CallWifiAction::Stop).await;
    assert!(
        matches!(next_msg_skipping_stats(&mut phone_link).await, Msg::Stop { session: s, .. } if s == session)
    );
    assert!(view(&rig, CallWifiAction::Status).await.session.is_none());

    // Consent withdrawn mid-session also reaches the phone and ends it.
    view(
        &rig,
        CallWifiAction::Listen {
            device_id: id.clone(),
        },
    )
    .await;
    let Msg::Request { session } = next_msg(&mut phone_link).await else {
        panic!("expected request")
    };
    phone_link
        .send(&packet(&Msg::Offer(Offer {
            session,
            down: true,
            up: false,
            uplink: Uplink::None,
        })))
        .await
        .unwrap();
    assert!(matches!(next_msg(&mut phone_link).await, Msg::Answer(a) if a.accept));
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::CallAudioWifi,
        enabled: false,
    })
    .await;
    assert!(matches!(
        next_msg_skipping_stats(&mut phone_link).await,
        Msg::Stop { .. }
    ));
    let v = view(&rig, CallWifiAction::Status).await;
    assert!(v.session.is_none() && !v.phones[0].allowed && !v.phones[0].ready);
}

#[tokio::test]
async fn a_phone_that_does_not_declare_the_packets_is_not_listed() {
    let rig = Rig::start_with(|o| {
        *o.callwifi_audio.get_mut().unwrap() = Some(Arc::new(Speaker::default()));
    })
    .await;
    let stock = Phone::new();
    let _link = pair_phone(&rig, &stock).await;
    let v = view(&rig, CallWifiAction::Status).await;
    assert!(v.phones.is_empty() && v.session.is_none());
    // And a Calls poll stays quiet about the experiment.
    match rig.call(Request::Calls).await.unwrap() {
        Response::Calls(c) => assert!(c.wifi.is_none()),
        other => panic!("{other:?}"),
    }
}
