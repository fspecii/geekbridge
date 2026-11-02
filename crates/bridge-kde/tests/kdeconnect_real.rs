//! Cross-implementation vectors captured from a REAL stock KDE Connect desktop daemon
//! (kdeconnectd 26.08.2, Arch package `kdeconnect`), by `scripts/interop-kdeconnect.sh`.
//!
//! `tests/fixtures/kdeconnect-real/` holds the packet lines exactly as they crossed the TLS
//! connection (one JSON line each, trailing LF kept), the two public certificates of that run and
//! the verification keys the daemon displayed. `daemon-*` files are packets the daemon wrote;
//! `geekbridge-*` files are packets GeekBridge wrote that the daemon accepted. No private key or
//! secret is stored; see the README in that directory.

use std::path::{Path, PathBuf};

use base64::Engine;
use bridge_core::clipboard_wire::{self, RemoteClip, WireError};
use bridge_kde::identity::Identity;
use bridge_kde::pairing::{PairBody, verification_key};
use bridge_kde::share::{self, ShareContent};
use bridge_kde::{CertInfo, Packet};
use serde_json::Value;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kdeconnect-real")
}

fn raw(name: &str) -> Vec<u8> {
    std::fs::read(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn packet(name: &str) -> Packet {
    let bytes = raw(name);
    assert_eq!(
        bytes.last(),
        Some(&b'\n'),
        "{name}: fixture keeps the line feed"
    );
    Packet::from_line(&bytes[..bytes.len() - 1]).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn cert(name: &str) -> CertInfo {
    let pem = String::from_utf8(raw(name)).unwrap();
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .unwrap();
    CertInfo::parse(&der).unwrap()
}

fn manifest() -> Value {
    serde_json::from_slice(&raw("manifest.json")).unwrap()
}

#[test]
fn every_captured_line_is_accepted_and_re_emitted_byte_for_byte() {
    let m = manifest();
    let files = m["files"].as_array().unwrap();
    assert!(files.len() >= 25, "fixture set shrank");
    for f in files {
        let name = f["file"].as_str().unwrap();
        let bytes = raw(name);
        let p = packet(name);
        assert_eq!(p.kind, f["type"].as_str().unwrap(), "{name}");
        // Qt's compact JSON sorts keys; serde_json's map does too, so a parsed packet written
        // back must reproduce the stock line exactly, not merely an equivalent one.
        assert_eq!(
            p.to_line(),
            bytes,
            "{name}: re-serialisation differs from the stock bytes"
        );
    }
}

#[test]
fn daemon_identities_are_valid_protocol_8_identities() {
    let secure = Identity::from_packet(&packet("daemon-identity-secure.json")).unwrap();
    assert_eq!(secure.protocol_version, 8);
    assert_eq!(secure.device_type.as_str(), "desktop");
    assert_eq!(secure.device_name, "interop-daemon");
    assert!(secure.target_device_id.is_none());
    for cap in [
        "kdeconnect.ping",
        "kdeconnect.clipboard",
        "kdeconnect.clipboard.connect",
        "kdeconnect.share.request",
    ] {
        assert!(
            secure.incoming_capabilities.iter().any(|c| c == cap),
            "{cap}"
        );
        assert!(
            secure.outgoing_capabilities.iter().any(|c| c == cap),
            "{cap}"
        );
    }
    // The stock desktop does not declare any of our geekbridge.* extension types.
    assert!(
        !secure
            .incoming_capabilities
            .iter()
            .chain(&secure.outgoing_capabilities)
            .any(|c| c.starts_with("geekbridge."))
    );

    let plain = Identity::from_packet(&packet("daemon-identity-plaintext.json")).unwrap();
    let gb = Identity::from_packet(&packet("geekbridge-identity-secure.json")).unwrap();
    assert_eq!(plain.device_id, secure.device_id);
    assert_eq!(plain.target_device_id.as_ref(), Some(&gb.device_id));
    assert_eq!(plain.target_protocol_version, Some(8));
    // Identities we send, as the daemon received them.
    let ours = Identity::from_packet(&packet("geekbridge-identity-plaintext.json")).unwrap();
    assert_eq!(ours.target_device_id.as_ref(), Some(&secure.device_id));
    assert_eq!(ours.target_protocol_version, Some(8));
    assert_eq!(ours.device_id, gb.device_id);
}

#[test]
fn daemon_certificate_matches_its_identity_and_is_valid() {
    let secure = Identity::from_packet(&packet("daemon-identity-secure.json")).unwrap();
    let c = cert("daemon.crt.pem");
    assert_eq!(c.common_name(), secure.device_id.as_str());
    // Valid at the time of capture (the certificate is valid for ~10 years from creation).
    c.check_validity(packet("daemon-ping-empty.json").id / 1000)
        .unwrap();
    let g = cert("geekbridge.crt.pem");
    let gb = Identity::from_packet(&packet("geekbridge-identity-secure.json")).unwrap();
    assert_eq!(g.common_name(), gb.device_id.as_str());
}

#[test]
fn verification_keys_equal_the_ones_the_real_daemon_displayed() {
    let m = manifest();
    let (d, g) = (cert("daemon.crt.pem"), cert("geekbridge.crt.pem"));
    for (case, req) in [
        ("s1_geekbridge_requested", "geekbridge-pair-request.json"),
        ("s2_daemon_requested", "daemon-pair-request.json"),
    ] {
        let v = &m["vectors"][case];
        assert_eq!(req, v["request"].as_str().unwrap());
        let ts = PairBody::parse(&packet(req)).unwrap().timestamp.unwrap();
        let shown = v["key_shown_by_daemon"].as_str().unwrap();
        assert_eq!(
            verification_key(g.spki_der(), d.spki_der(), ts),
            shown,
            "{case}"
        );
        // Order of the two keys does not matter.
        assert_eq!(
            verification_key(d.spki_der(), g.spki_der(), ts),
            shown,
            "{case}"
        );
    }
}

#[test]
fn pair_packets_parse_like_stock_sends_them() {
    let accept = PairBody::parse(&packet("daemon-pair-accept.json")).unwrap();
    assert!(accept.pair && accept.timestamp.is_none());
    let unpair = PairBody::parse(&packet("daemon-pair-unpair.json")).unwrap();
    assert!(!unpair.pair);
    let request = PairBody::parse(&packet("daemon-pair-request.json")).unwrap();
    assert!(request.pair && request.timestamp.is_some());
    // The timestamp is in seconds, close to the packet id (milliseconds).
    let ts = request.timestamp.unwrap();
    assert!((ts - packet("daemon-pair-request.json").id / 1000).abs() < 5);
}

#[test]
fn clipboard_packets_from_the_daemon_are_understood() {
    let max = 1 << 20;
    let upd = packet("daemon-clipboard.json");
    assert!(matches!(
        clipboard_wire::parse(&upd.kind, &upd.body, max),
        Ok(RemoteClip::Update { .. })
    ));
    let conn = packet("daemon-clipboard-connect.json");
    match clipboard_wire::parse(&conn.kind, &conn.body, max) {
        Ok(RemoteClip::Connect {
            content,
            timestamp_ms,
        }) => {
            assert_eq!(content, "hello text");
            assert!(timestamp_ms > 0);
        }
        other => panic!("{other:?}"),
    }
    // Regression for the interop bug this run found: an empty clipboard is `content: null`.
    let empty = packet("daemon-clipboard-connect-empty.json");
    assert!(empty.body["content"].is_null());
    match clipboard_wire::parse(&empty.kind, &empty.body, max) {
        Ok(RemoteClip::Connect {
            content,
            timestamp_ms,
        }) => {
            assert!(content.is_empty());
            assert_eq!(timestamp_ms, 0, "callers never apply timestamp 0");
        }
        other => panic!("{other:?}"),
    }
    // null is only tolerated on `.connect`; a live update without text is still malformed.
    let mut bad = upd.body.clone();
    bad.insert("content".into(), Value::Null);
    assert_eq!(
        clipboard_wire::parse(&upd.kind, &bad, max),
        Err(WireError::MissingContent)
    );
}

#[test]
fn share_packets_from_the_daemon_are_understood() {
    let max = 1 << 30;
    let f = packet("daemon-share-file.json");
    assert!(f.payload_size.is_some() && f.payload_transfer_info.is_some());
    match share::parse_share(&f, max).unwrap() {
        ShareContent::File(o) => {
            assert_eq!(o.name, "from-daemon.bin");
            assert_eq!(o.size, Some(200_000));
            assert!((1716..=1764).contains(&o.port), "payload port {}", o.port);
            assert_eq!(o.number_of_files, Some(1));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        share::parse_share(&packet("daemon-share-text.json"), max).unwrap(),
        ShareContent::Text("text from daemon".into())
    );
    assert_eq!(
        share::parse_share(&packet("daemon-share-url.json"), max).unwrap(),
        ShareContent::Url("https://example.org/from-daemon".into())
    );
}

#[test]
fn ping_packets_from_the_daemon() {
    assert_eq!(
        packet("daemon-ping-message.json").body["message"],
        "hello from daemon"
    );
    assert!(packet("daemon-ping-empty.json").body.is_empty());
}

#[test]
fn what_geekbridge_sends_has_the_shape_stock_peers_read() {
    // Stock `SharePlugin`/`ClipboardPlugin` read exactly these keys (kdeconnect-android 4de397e,
    // kdeconnect-kde 25c0cc7).
    let file = packet("geekbridge-share-file.json");
    for k in ["filename", "numberOfFiles", "totalPayloadSize", "open"] {
        assert!(file.body.contains_key(k), "{k}");
    }
    assert!(file.payload_size.is_some());
    assert!(file.payload_transfer_info.as_ref().unwrap()["port"].is_u64());
    assert!(packet("geekbridge-share-text.json").body["text"].is_string());
    assert!(packet("geekbridge-share-url.json").body["url"].is_string());
    assert!(packet("geekbridge-clipboard.json").body["content"].is_string());
    let c = packet("geekbridge-clipboard-connect.json");
    assert!(c.body["content"].is_string() && c.body["timestamp"].is_i64());
    let pair = PairBody::parse(&packet("geekbridge-pair-request.json")).unwrap();
    assert!(pair.pair && pair.timestamp.is_some());
}

#[test]
fn extension_packets_are_well_formed_and_namespaced() {
    // The real daemon logged `discarding unsupported packet` for each of these and kept the link.
    for n in [
        "geekbridge-ext-calendar-request.json",
        "geekbridge-ext-calendar-write.json",
        "geekbridge-ext-calls-request.json",
        "geekbridge-ext-clipboard-pull.json",
    ] {
        assert!(packet(n).kind.starts_with("geekbridge."), "{n}");
    }
}

/// Field names the stock Android phone reads for requests GeekBridge sends (no stock desktop can
/// answer these, so they are spec-checked against kdeconnect-android 4de397e, not run live).
#[test]
fn sms_and_contacts_requests_use_the_keys_the_android_plugins_read() {
    use bridge_core::messages::ThreadId;
    use bridge_core::phone_link;
    let b = bridge_kde::sms::request_conversation_body(ThreadId(42), Some(1_700_000_000_000), 50);
    // SMSPlugin: packet.getLong("threadID"), "rangeStartTimestamp", "numberToRequest".
    assert_eq!(b["threadID"], 42);
    assert_eq!(b["rangeStartTimestamp"], 1_700_000_000_000u64);
    assert_eq!(b["numberToRequest"], 50);
    // ContactsPlugin: np.getStringList("uids").
    let v = phone_link::request_vcards_body(&["a".into(), "b".into()]);
    assert_eq!(v["uids"], serde_json::json!(["a", "b"]));
}
