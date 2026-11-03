// SPDX-License-Identifier: GPL-3.0-or-later
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
            "kdeconnect.battery",
            "kdeconnect.notification",
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
fn stock_android_telephony_packets_are_dropped() {
    let neg = gate::negotiated_incoming(&stock_android());
    for kind in ["kdeconnect.telephony", "kdeconnect.connectivity_report"] {
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

#[test]
fn stock_android_battery_is_negotiated_but_only_acted_on_with_consent() {
    use bridge_core::ipc::Feature;
    let p = stock_android();
    let neg = gate::negotiated_incoming(&p);
    // The packet is routed to the battery handler (which drops it unless the `battery` consent is on).
    assert_eq!(
        gate::classify_incoming("kdeconnect.battery", &neg),
        Incoming::Battery
    );
    assert_eq!(
        gate::feature_of("kdeconnect.battery"),
        Some(Feature::Battery)
    );
    assert_eq!(
        gate::feature_of("kdeconnect.battery.request"),
        Some(Feature::Battery)
    );
    assert!(gate::adapter_integrated(Feature::Battery));
    // This stock build lists only `kdeconnect.battery` as accepted, so no request is offered to it; it sends
    // its state on connect by itself.
    assert!(
        !gate::negotiated_outgoing(&p)
            .iter()
            .any(|k| k == "kdeconnect.battery.request")
    );
    // A peer that declares neither direction never gets a battery packet through.
    assert_eq!(
        gate::classify_incoming("kdeconnect.battery", &["kdeconnect.ping".to_owned()]),
        Incoming::Reject
    );
}

#[test]
fn stock_android_notifications_are_negotiated_without_replies_and_only_acted_on_with_consent() {
    use bridge_core::ipc::Feature;
    let p = stock_android();
    let neg = gate::negotiated_incoming(&p);
    // Routed to the notification handler, which drops the packet unless the `notifications` consent is on.
    assert_eq!(
        gate::classify_incoming("kdeconnect.notification", &neg),
        Incoming::Notification
    );
    assert!(gate::adapter_integrated(Feature::Notifications));
    // The stock phone accepts the request and action packets; the reply packet it also lists is never offered.
    let out = gate::negotiated_outgoing(&p);
    assert!(out.iter().any(|k| k == "kdeconnect.notification.request"));
    assert!(out.iter().any(|k| k == "kdeconnect.notification.action"));
    assert!(!out.iter().any(|k| k == "kdeconnect.notification.reply"));
    assert_eq!(
        gate::feature_of("kdeconnect.notification.action"),
        Some(Feature::Notifications)
    );
    // Without the consent nothing leaves.
    assert_eq!(
        gate::check_outgoing("kdeconnect.notification.request", &out, |_| false),
        gate::Outgoing::ConsentRequired(Feature::Notifications)
    );
}
