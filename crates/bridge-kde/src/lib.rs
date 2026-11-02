// SPDX-License-Identifier: GPL-3.0-or-later
//! KDE Connect protocol 8 LAN transport with explicit, certificate-pinned pairing.
//!
//! * [`Endpoint`] performs the TCP/TLS handshake in either role (the TCP
//!   initiator is the TLS *server*), exchanges and cross-checks identities, and
//!   yields a [`Handshake`]: a [`TrustedLink`] for a peer whose exact certificate
//!   was pinned earlier, or a [`PendingLink`] that can only be inspected or paired.
//! * [`PendingLink::pair`] runs an explicit pairing that needs a local approval
//!   from an [`Approver`] and writes the trust record only for the certificate
//!   on that connection.
//! * Discovery is out of scope and never confers trust.
//!
//! Interop with stock KDE Connect Android/desktop devices is documented in
//! `docs/kdeconnect-interop.md`.

pub mod cert;
pub mod error;
pub mod fsutil;
pub mod identity;
mod keepalive;
pub mod link;
pub mod packet;
pub mod pairing;
pub mod payload;
pub mod qrpair;
pub mod share;
pub mod sms;
pub mod store;
pub mod tls;

pub use cert::{CertInfo, DeviceCredentials};
pub use error::{KdeError, PairingFailure, Result};
pub use identity::{DeviceId, DeviceType, Identity, PROTOCOL_VERSION};
pub use link::{
    AddressPolicy, ConnectionId, Endpoint, Handshake, Limits, LinkEvent, LinkInfo, PendingLink,
    Role, Timeouts, TransportConfig, TrustedLink, WireTap,
};
pub use packet::Packet;
pub use pairing::{
    ApprovalRequest, Approver, Direction, PairMode, PairingConfig, verification_key,
};
pub use payload::{CancelToken, PayloadConfig, PayloadContext, PayloadError, PayloadListener};
pub use store::{FileTrustStore, MemoryTrustStore, TrustStore, TrustedPeer};
pub use tls::TlsVersions;
