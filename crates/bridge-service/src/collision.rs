// SPDX-License-Identifier: GPL-3.0-or-later
//! A phone and this computer dialing each other at the same moment (our redial against the phone's redial, or a
//! reconnect the phone makes to re-declare its capabilities). Both ends then hold two connections to one device
//! and must agree on which one lives; see `bridge_kde::tiebreak`. The rule is only applied between GeekBridge
//! ends that both follow it, and only while the connection already held is brand new or still being dialed: a
//! stock KDE Connect peer keeps the newest connection, and a long-lived connection that is replaced is a
//! reconnect after a silent loss, where the newest must win or the phone could never get back in.
use bridge_core::ipc::{LinkPhase, LinkRole, LinkStatus};
use bridge_kde::{DeviceId, Identity, Role};

/// How long a connection counts as "just made" for the purpose of telling a cross-dial from a reconnect.
pub const WINDOW_MS: u64 = 5_000;

/// An authenticated connection from `peer` just arrived while `held` is the link we have for it. True when
/// the arrival must be turned away and `held` kept.
pub fn keep_held(
    local: &DeviceId,
    peer: &Identity,
    held: &LinkStatus,
    arrived: Role,
    now_ms: u64,
) -> bool {
    if !bridge_core::heartbeat::is_geekbridge_peer(
        &peer.incoming_capabilities,
        &peer.outgoing_capabilities,
    ) {
        return false;
    }
    let fresh = match held.phase {
        LinkPhase::Connecting => true,
        LinkPhase::Connected => now_ms.saturating_sub(held.since_unix_ms) < WINDOW_MS,
        _ => false,
    };
    let Some(held_role) = held.role else {
        return false;
    };
    let held_role = match held_role {
        LinkRole::Initiator => Role::Initiator,
        LinkRole::Acceptor => Role::Acceptor,
    };
    fresh && !bridge_kde::tiebreak::prefer_new(local, &peer.device_id, held_role, arrived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_kde::DeviceType;

    fn id(c: char) -> DeviceId {
        DeviceId::parse(&c.to_string().repeat(32)).unwrap()
    }
    fn peer(c: char, geekbridge: bool) -> Identity {
        let caps = if geekbridge {
            vec![bridge_core::heartbeat::MARKER.to_string()]
        } else {
            vec!["kdeconnect.ping".to_string()]
        };
        Identity::local(id(c), "Phone", DeviceType::Phone, caps.clone(), caps).unwrap()
    }
    fn held(phase: LinkPhase, role: LinkRole, since: u64) -> LinkStatus {
        LinkStatus {
            device_id: String::new(),
            name: String::new(),
            phase,
            role: Some(role),
            connection_id: None,
            peer_ip: None,
            peer_cert_sha256: None,
            negotiated: vec![],
            since_unix_ms: since,
            pings_received: 0,
            last_ping_unix_ms: None,
            rejected_packets: 0,
            battery: None,
        }
    }

    #[test]
    fn the_smaller_id_keeps_the_connection_it_opened() {
        // we are `a`, the phone `b`: our dial wins, the phone's arrival is turned away
        let dial = held(LinkPhase::Connecting, LinkRole::Initiator, 0);
        assert!(keep_held(
            &id('a'),
            &peer('b', true),
            &dial,
            Role::Acceptor,
            10
        ));
        // we are `b`, the phone `a`: the phone's connection replaces our dial
        assert!(!keep_held(
            &id('b'),
            &peer('a', true),
            &dial,
            Role::Acceptor,
            10
        ));
        // and the other way round for a connection the phone opened first
        let theirs = held(LinkPhase::Connected, LinkRole::Acceptor, 0);
        assert!(!keep_held(
            &id('a'),
            &peer('b', true),
            &theirs,
            Role::Initiator,
            10
        ));
        assert!(keep_held(
            &id('b'),
            &peer('a', true),
            &theirs,
            Role::Initiator,
            10
        ));
    }

    #[test]
    fn an_old_link_or_a_stock_peer_is_replaced_by_the_newest_as_before() {
        let old = held(LinkPhase::Connected, LinkRole::Initiator, 0);
        assert!(!keep_held(
            &id('a'),
            &peer('b', true),
            &old,
            Role::Acceptor,
            WINDOW_MS + 1
        ));
        let dial = held(LinkPhase::Connecting, LinkRole::Initiator, 0);
        assert!(!keep_held(
            &id('a'),
            &peer('b', false),
            &dial,
            Role::Acceptor,
            10
        ));
        let unpaired = held(LinkPhase::Unpaired, LinkRole::Initiator, 0);
        assert!(!keep_held(
            &id('a'),
            &peer('b', true),
            &unpaired,
            Role::Acceptor,
            10
        ));
    }

    #[test]
    fn the_same_direction_twice_is_a_reconnect() {
        let dial = held(LinkPhase::Connecting, LinkRole::Acceptor, 0);
        assert!(!keep_held(
            &id('a'),
            &peer('b', true),
            &dial,
            Role::Acceptor,
            10
        ));
    }
}
