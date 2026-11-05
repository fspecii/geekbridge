// SPDX-License-Identifier: GPL-3.0-or-later
//! Vectors captured from the REAL stock KDE Connect Android app (1.37.0, F-Droid build) talking to
//! `geekbridge-service` in an emulator. See `tests/fixtures/kdeconnect-android-real/README.md`.

use std::path::{Path, PathBuf};

use bridge_core::clipboard_wire::{self, RemoteClip};
use bridge_core::phone_link;
use bridge_kde::identity::Identity;
use bridge_kde::pairing::PairBody;
use bridge_kde::share::{self, ShareContent};
use bridge_kde::{Packet, sms};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kdeconnect-android-real")
}

fn packet(name: &str) -> Packet {
    let bytes = std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(bytes.last(), Some(&b'\n'), "{name}: fixture keeps the LF");
    Packet::from_line(&bytes[..bytes.len() - 1]).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn android_identity_is_a_valid_protocol_8_phone() {
    for name in ["android-udp-identity.json", "android-identity-secure.json"] {
        let id = Identity::from_packet(&packet(name)).unwrap();
        assert_eq!(id.protocol_version, 8, "{name}");
        assert_eq!(id.device_type.as_str(), "phone");
        assert_eq!(id.device_id.as_str().len(), 32);
        for cap in [
            "kdeconnect.ping",
            "kdeconnect.clipboard",
            "kdeconnect.share.request",
            "kdeconnect.sms.request_conversations",
            "kdeconnect.contacts.request_all_uids_timestamps",
        ] {
            assert!(id.incoming_capabilities.iter().any(|c| c == cap), "{cap}");
        }
        for cap in [
            "kdeconnect.sms.messages",
            "kdeconnect.contacts.response_vcards",
        ] {
            assert!(id.outgoing_capabilities.iter().any(|c| c == cap), "{cap}");
        }
    }
    // Android advertises the TCP port in the UDP datagram.
    assert_eq!(packet("android-udp-identity.json").body["tcpPort"], 1716);
}

#[test]
fn android_pair_packets() {
    let accept = PairBody::parse(&packet("android-pair-accept.json")).unwrap();
    assert!(accept.pair && accept.timestamp.is_none());
    assert!(
        !PairBody::parse(&packet("android-pair-unpair.json"))
            .unwrap()
            .pair
    );
    let req = PairBody::parse(&packet("android-pair-request.json")).unwrap();
    assert!(req.pair);
    let ts = req.timestamp.unwrap();
    assert!((ts - packet("android-pair-request.json").id / 1000).abs() < 5);
    // What we send is the same shape.
    let ours = PairBody::parse(&packet("geekbridge-pair-request.json")).unwrap();
    assert!(ours.pair && ours.timestamp.is_some());
}

#[test]
fn android_ping_clipboard_and_share() {
    assert!(packet("android-ping.json").body.is_empty());
    let clip = packet("android-clipboard.json");
    assert!(matches!(
        clipboard_wire::parse(&clip.kind, &clip.body, 1 << 20).unwrap(),
        RemoteClip::Update { .. }
    ));
    let big = 1 << 30;
    match share::parse_share(&packet("android-share-file.json"), big).unwrap() {
        ShareContent::File(f) => {
            assert_eq!(f.name, "d2p.bin");
            assert_eq!(f.size, Some(300_000));
            assert_eq!(f.number_of_files, Some(1));
            assert!((1739..=1764).contains(&f.port));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        share::parse_share(&packet("android-share-text.json"), big).unwrap(),
        ShareContent::Text(t) if t == "shared-text-from-stock-phone"
    ));
    assert!(matches!(
        share::parse_share(&packet("android-share-url.json"), big).unwrap(),
        ShareContent::Url(u) if u == "https://example.org/from-phone"
    ));
    // The file offer we sent was accepted by the app (it saved a byte-identical file).
    assert!(matches!(
        share::parse_share(&packet("geekbridge-share-file.json"), big).unwrap(),
        ShareContent::File(_)
    ));
}

#[test]
fn android_sms_messages_parse() {
    let empty = sms::parse_messages(&packet("android-sms-messages-empty.json").body).unwrap();
    assert!(empty.messages.is_empty() && empty.rejected == 0);
    let p = sms::parse_messages(&packet("android-sms-messages.json").body).unwrap();
    assert_eq!((p.messages.len(), p.rejected), (1, 0));
    let m = &p.messages[0];
    assert_eq!(&*m.body, "Hello from Ada, interop test");
    assert!(!m.read);
}

#[test]
fn android_contacts_parse() {
    let uids = phone_link::parse_uids(&packet("android-contacts-uids.json").body).unwrap();
    assert_eq!(uids.len(), 1);
    let cards = phone_link::parse_vcards(&packet("android-contacts-vcards.json").body).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].0, uids[0].0);
    let c = phone_link::contact_from_vcard(&cards[0].1).unwrap();
    assert_eq!(c.display_name, "Ada Lovelace");
    assert_eq!(c.emails, ["ada@example.org"]);
    assert_eq!(c.phones.len(), 1);
}

#[test]
fn what_geekbridge_sent_the_android_plugins_has_the_keys_they_read() {
    // SMSPlugin: addresses (list of {address}), messageBody, subID optional, sendSms.
    let send = packet("geekbridge-sms-request-send.json");
    assert_eq!(send.body["version"], 2);
    assert_eq!(send.body["sendSms"], true);
    assert_eq!(send.body["messageBody"], "reply from desktop");
    assert_eq!(send.body["addresses"][0]["address"], "+15551230001");
    let conv = packet("geekbridge-sms-request-conversation.json");
    for k in ["threadID", "rangeStartTimestamp", "numberToRequest"] {
        assert!(conv.body.contains_key(k), "{k}");
    }
    assert!(
        packet("geekbridge-sms-request-conversations.json")
            .body
            .is_empty()
    );
    assert!(
        packet("geekbridge-contacts-request-uids.json")
            .body
            .is_empty()
    );
}
