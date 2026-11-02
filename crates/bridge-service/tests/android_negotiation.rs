//! What `gate` negotiates with the REAL stock KDE Connect Android app (identity captured from
//! 1.37.0, F-Droid build, see `crates/bridge-kde/tests/fixtures/kdeconnect-android-real/`).

use bridge_kde::{Identity, Packet};
use bridge_service::gate::{self, Incoming};

fn stock_android() -> Identity {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../bridge-kde/tests/fixtures/kdeconnect-android-real/android-identity-secure.json"
    );
    let line = std::fs::read(path).unwrap();
    Identity::from_packet(&Packet::from_line(&line[..line.len() - 1]).unwrap()).unwrap()
}

#[test]
fn negotiated_with_stock_android_is_exactly_what_was_verified_live() {
    let p = stock_android();
    // Observed live in `geekbridge-ctl status` after pairing.
    assert_eq!(
        gate::negotiated_incoming(&p),
        [
            "kdeconnect.ping",
            "kdeconnect.clipboard",
            "kdeconnect.clipboard.connect",
            "kdeconnect.share.request",
            "kdeconnect.contacts.response_uids_timestamps",
            "kdeconnect.contacts.response_vcards",
            "kdeconnect.sms.messages",
        ]
    );
    let out = gate::negotiated_outgoing(&p);
    for kind in [
        "kdeconnect.ping",
        "kdeconnect.clipboard",
        "kdeconnect.share.request",
        "kdeconnect.sms.request",
        "kdeconnect.sms.request_conversations",
        "kdeconnect.sms.request_conversation",
        "kdeconnect.contacts.request_all_uids_timestamps",
        "kdeconnect.contacts.request_vcards_by_uid",
    ] {
        assert!(out.iter().any(|k| k == kind), "{kind}");
    }
    // GeekBridge-only extensions are never offered to a stock phone.
    assert!(!out.iter().any(|k| k.starts_with("geekbridge.")));
}

#[test]
fn stock_android_battery_notification_and_telephony_packets_are_dropped() {
    let neg = gate::negotiated_incoming(&stock_android());
    for kind in [
        "kdeconnect.battery",
        "kdeconnect.notification",
        "kdeconnect.telephony",
        "kdeconnect.connectivity_report",
    ] {
        assert_eq!(
            gate::classify_incoming(kind, &neg),
            Incoming::Reject,
            "{kind}"
        );
    }
    assert_eq!(
        gate::classify_incoming("kdeconnect.sms.messages", &neg),
        Incoming::PhoneData
    );
}
