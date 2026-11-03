// SPDX-License-Identifier: GPL-3.0-or-later
//! Which of two live connections to the same peer survives. Shared by the computer and the phone so both
//! ends decide identically.
//!
//! When both ends dial each other at the same moment (a redial on each side after a drop, a reconnect to
//! re-declare capabilities), each ends up with two connections to the same device. If each side simply
//! kept the newest, each would close the connection the other kept and the link would be gone until the next
//! redial. The rule below picks the same connection on both ends.
use crate::{DeviceId, Role};

/// How long a held connection counts as "just made". Only inside this window are two connections a cross-dial
/// decided by [`prefer_new`]; a held connection older than this that meets a new one is a reconnect after a loss the
/// holder has not noticed yet (a half-open flow: the peer's FIN or RST never arrived), and the newest must win on both
/// ends, or the peer could not get back in until the holder's own dead-link detection fired.
pub const CROSS_DIAL_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// Two trusted connections to `peer`: the one held now (opened with `old`) and one that just arrived
/// (opened with `new`, from the point of view of `local`). True if the new one replaces the old.
///
/// The connection opened by the lexicographically smaller device id wins. Two connections opened by the same
/// side mean the newest is a reconnect and replaces the older one.
pub fn prefer_new(local: &DeviceId, peer: &DeviceId, old: Role, new: Role) -> bool {
    if old == new {
        return true;
    }
    let (initiator, other) = match new {
        Role::Initiator => (local, peer),
        Role::Acceptor => (peer, local),
    };
    initiator < other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> DeviceId {
        DeviceId::parse(&c.to_string().repeat(32)).unwrap()
    }

    #[test]
    fn both_ends_keep_the_same_connection_in_either_arrival_order() {
        let (a, b) = (id('a'), id('b'));
        // Connection X was opened by `a` (the smaller id), Y by `b`.
        // On a: X is Initiator, Y is Acceptor. On b: X is Acceptor, Y is Initiator.
        let a_keeps_x = !prefer_new(&a, &b, Role::Initiator, Role::Acceptor); // old X, new Y
        let b_replaces_y_with_x = prefer_new(&b, &a, Role::Initiator, Role::Acceptor); // old Y, new X
        assert!(a_keeps_x && b_replaces_y_with_x);
        let a_replaces_y_with_x = prefer_new(&a, &b, Role::Acceptor, Role::Initiator); // old Y, new X
        let b_keeps_x = !prefer_new(&b, &a, Role::Acceptor, Role::Initiator); // old X, new Y
        assert!(a_replaces_y_with_x && b_keeps_x);
    }

    #[test]
    fn the_same_direction_twice_is_a_reconnect_and_the_newest_wins() {
        let (a, b) = (id('a'), id('b'));
        for local in [(&a, &b), (&b, &a)] {
            assert!(prefer_new(
                local.0,
                local.1,
                Role::Initiator,
                Role::Initiator
            ));
            assert!(prefer_new(local.0, local.1, Role::Acceptor, Role::Acceptor));
        }
    }
}
