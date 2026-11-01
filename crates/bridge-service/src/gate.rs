//! Which packet types may cross a trusted link, and when.
//!
//! The service declares exactly the capabilities it has working code for: `kdeconnect.ping` and the
//! clipboard packets. The phone is therefore never told it may send SMS or contacts, and anything it
//! sends anyway is counted and dropped, never routed. Outgoing
//! feature requests additionally need per-device consent (default off) and an integrated adapter.

use bridge_core::ipc::Feature;
use bridge_kde::Identity;

pub use bridge_core::clipboard_wire::{
    CLIPBOARD, CLIPBOARD_CONNECT, ITEM as CLIPBOARD_ITEM, NONE as CLIPBOARD_NONE,
    PULL as CLIPBOARD_PULL,
};

pub use bridge_core::calls_link::{
    REQUEST as CALLS_REQUEST, RESULT as CALLS_RESULT, STATE as CALLS_STATE,
};

pub use bridge_core::phone_link::{
    CALENDAR_REQUEST, CALENDAR_SYNC, CONTACTS_REQUEST_UIDS, CONTACTS_REQUEST_VCARDS, CONTACTS_UIDS,
    CONTACTS_VCARDS, SMS_STATUS,
};
pub use bridge_kde::sms::{
    MESSAGES as SMS_MESSAGES, REQUEST as SMS_REQUEST,
    REQUEST_CONVERSATION as SMS_REQUEST_CONVERSATION,
    REQUEST_CONVERSATIONS as SMS_REQUEST_CONVERSATIONS,
};

pub const PING: &str = "kdeconnect.ping";
/// What this build declares in its identity packet.
/// File, link and text sharing (`crate::share`).
pub const SHARE_REQUEST: &str = "kdeconnect.share.request";
pub const LOCAL_INCOMING: [&str; 13] = [
    PING,
    CLIPBOARD,
    CLIPBOARD_CONNECT,
    CLIPBOARD_ITEM,
    CLIPBOARD_NONE,
    SHARE_REQUEST,
    CALLS_STATE,
    CALLS_RESULT,
    CONTACTS_UIDS,
    CONTACTS_VCARDS,
    CALENDAR_SYNC,
    SMS_MESSAGES,
    SMS_STATUS,
];
pub const LOCAL_OUTGOING: [&str; 13] = [
    PING,
    CLIPBOARD,
    CLIPBOARD_CONNECT,
    CLIPBOARD_PULL,
    CLIPBOARD_ITEM,
    SHARE_REQUEST,
    CALLS_REQUEST,
    CONTACTS_REQUEST_UIDS,
    CONTACTS_REQUEST_VCARDS,
    CALENDAR_REQUEST,
    SMS_REQUEST_CONVERSATIONS,
    SMS_REQUEST_CONVERSATION,
    SMS_REQUEST,
];

/// Packet types the peer may send us: its outgoing list intersected with what we declared.
pub fn negotiated_incoming(peer: &Identity) -> Vec<String> {
    LOCAL_INCOMING
        .iter()
        .filter(|kind| peer.outgoing_capabilities.iter().any(|c| c == *kind))
        .map(|s| (*s).to_owned())
        .collect()
}

/// Packet types we may send the peer: our outgoing list intersected with what it accepts.
pub fn negotiated_outgoing(peer: &Identity) -> Vec<String> {
    LOCAL_OUTGOING
        .iter()
        .filter(|kind| peer.incoming_capabilities.iter().any(|c| c == *kind))
        .map(|s| (*s).to_owned())
        .collect()
}

/// Feature a packet type belongs to, if it is consent-gated.
pub fn feature_of(kind: &str) -> Option<Feature> {
    if bridge_core::clipboard_wire::is_pull_protocol_kind(kind) {
        return Some(Feature::Clipboard);
    }
    if bridge_core::calls_link::is_calls_kind(kind) {
        return Some(Feature::Telephony);
    }
    if bridge_core::phone_link::is_calendar_kind(kind) {
        return Some(Feature::Calendar);
    }
    if kind == SMS_STATUS {
        return Some(Feature::Sms);
    }
    let ns = kind.strip_prefix("kdeconnect.")?;
    let head = ns.split('.').next()?;
    Some(match head {
        "sms" => Feature::Sms,
        "contacts" => Feature::Contacts,
        "clipboard" => Feature::Clipboard,
        "notification" => Feature::Notifications,
        "telephony" => Feature::Telephony,
        _ => return None,
    })
}

/// Whether the service has a protocol adapter that handles `feature` packets.
pub fn adapter_integrated(feature: Feature) -> bool {
    matches!(
        feature,
        Feature::Clipboard
            | Feature::Telephony
            | Feature::Sms
            | Feature::Contacts
            | Feature::Calendar
    )
}

#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Ping,
    /// A clipboard packet. Whether it is acted on still depends on the per-device consent.
    Clipboard,
    /// A `geekbridge.calls.*` packet from the phone. Acted on only for devices with Telephony consent.
    Calls,
    /// A contacts, calendar or SMS packet from the phone. Acted on only for devices that allowed
    /// that feature (`phone_data`); otherwise it is dropped unread.
    PhoneData,
    Reject,
}

/// Fate of a packet received on a trusted link. Negotiation is the only way in.
pub fn classify_incoming(kind: &str, negotiated: &[String]) -> Incoming {
    if kind == PING && negotiated.iter().any(|k| k == PING) {
        Incoming::Ping
    } else if feature_of(kind) == Some(Feature::Clipboard) && negotiated.iter().any(|k| k == kind) {
        Incoming::Clipboard
    } else if matches!(kind, CALLS_STATE | CALLS_RESULT) && negotiated.iter().any(|k| k == kind) {
        Incoming::Calls
    } else if matches!(
        kind,
        CONTACTS_UIDS | CONTACTS_VCARDS | CALENDAR_SYNC | SMS_MESSAGES | SMS_STATUS
    ) && negotiated.iter().any(|k| k == kind)
    {
        Incoming::PhoneData
    } else {
        Incoming::Reject
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    Allowed,
    NotNegotiated,
    ConsentRequired(Feature),
    NotIntegrated(Feature),
}

/// Decide whether we may send `kind`. `consent` answers for the device the link belongs to.
pub fn check_outgoing(
    kind: &str,
    negotiated: &[String],
    consent: impl Fn(Feature) -> bool,
) -> Outgoing {
    if !negotiated.iter().any(|k| k == kind) {
        return Outgoing::NotNegotiated;
    }
    match feature_of(kind) {
        None if kind == PING => Outgoing::Allowed,
        None => Outgoing::NotNegotiated,
        Some(f) if !consent(f) => Outgoing::ConsentRequired(f),
        Some(f) if !adapter_integrated(f) => Outgoing::NotIntegrated(f),
        Some(_) => Outgoing::Allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_kde::{DeviceId, DeviceType};

    fn peer(incoming: &[&str], outgoing: &[&str]) -> Identity {
        Identity::local(
            DeviceId::parse(&"p".repeat(32)).unwrap(),
            "Phone",
            DeviceType::Phone,
            incoming.iter().map(|s| s.to_string()).collect(),
            outgoing.iter().map(|s| s.to_string()).collect(),
        )
        .unwrap()
    }

    #[test]
    fn only_declared_and_advertised_types_are_negotiated() {
        let p = peer(
            &["kdeconnect.ping", "kdeconnect.notification.request"],
            &[
                "kdeconnect.ping",
                "kdeconnect.notification",
                "kdeconnect.contacts.response_uids_timestamps",
            ],
        );
        // Only the intersection with what this build declares: the notification packets are not ours.
        assert_eq!(
            negotiated_incoming(&p),
            vec![PING.to_string(), CONTACTS_UIDS.to_string()]
        );
        assert_eq!(negotiated_outgoing(&p), vec![PING.to_string()]);
        let silent = peer(&[], &[]);
        assert!(negotiated_incoming(&silent).is_empty());
        assert!(negotiated_outgoing(&silent).is_empty());
    }

    #[test]
    fn unnegotiated_packets_are_rejected_even_for_known_features() {
        let negotiated = vec![PING.to_string()];
        assert_eq!(classify_incoming(PING, &negotiated), Incoming::Ping);
        assert_eq!(classify_incoming(PING, &[]), Incoming::Reject);
        // Clipboard packets are only routed when negotiated, and only the negotiated kinds.
        let clip = vec![CLIPBOARD.to_string(), CLIPBOARD_ITEM.to_string()];
        assert_eq!(classify_incoming(CLIPBOARD, &clip), Incoming::Clipboard);
        assert_eq!(
            classify_incoming(CLIPBOARD_ITEM, &clip),
            Incoming::Clipboard
        );
        assert_eq!(
            classify_incoming(CLIPBOARD_CONNECT, &clip),
            Incoming::Reject
        );
        assert_eq!(classify_incoming(CLIPBOARD, &[]), Incoming::Reject);
        // Phone data is routed only when negotiated, and only the negotiated kinds.
        let data = vec![SMS_MESSAGES.to_string()];
        assert_eq!(classify_incoming(SMS_MESSAGES, &data), Incoming::PhoneData);
        assert_eq!(classify_incoming(SMS_STATUS, &data), Incoming::Reject);
        assert_eq!(classify_incoming(SMS_MESSAGES, &[]), Incoming::Reject);
        for kind in [
            "kdeconnect.sms.messages",
            "kdeconnect.contacts.response_vcards",
            "kdeconnect.notification",
            "kdeconnect.telephony",
            "kdeconnect.share.request",
            "kdeconnect.runcommand",
            "kdeconnect.pair",
            "kdeconnect.identity",
            "",
        ] {
            assert_eq!(
                classify_incoming(kind, &negotiated),
                Incoming::Reject,
                "{kind}"
            );
        }
    }

    #[test]
    fn outgoing_feature_requests_need_negotiation_consent_and_an_adapter() {
        let all = vec![
            PING.to_string(),
            "kdeconnect.sms.request_conversations".to_string(),
            "kdeconnect.contacts.request_all_uids_timestamps".to_string(),
            "kdeconnect.clipboard".to_string(),
        ];
        assert_eq!(check_outgoing(PING, &all, |_| false), Outgoing::Allowed);
        assert_eq!(check_outgoing(PING, &[], |_| true), Outgoing::NotNegotiated);
        for (kind, feature) in [
            ("kdeconnect.sms.request_conversations", Feature::Sms),
            (
                "kdeconnect.contacts.request_all_uids_timestamps",
                Feature::Contacts,
            ),
            ("kdeconnect.clipboard", Feature::Clipboard),
        ] {
            assert_eq!(
                check_outgoing(kind, &all, |_| false),
                Outgoing::ConsentRequired(feature)
            );
            // With consent, only features that have an adapter may leave.
            assert_eq!(
                check_outgoing(kind, &all, |_| true),
                if adapter_integrated(feature) {
                    Outgoing::Allowed
                } else {
                    Outgoing::NotIntegrated(feature)
                }
            );
            assert_eq!(check_outgoing(kind, &[], |_| true), Outgoing::NotNegotiated);
        }
        assert_eq!(
            check_outgoing("kdeconnect.unknown", &["kdeconnect.unknown".into()], |_| {
                true
            }),
            Outgoing::NotNegotiated
        );
    }
}
