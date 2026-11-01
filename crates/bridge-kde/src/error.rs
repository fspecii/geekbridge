//! Error type shared by every layer of the transport.
//!
//! Messages never contain key material. Certificate problems carry fingerprints
//! (public data) only.

use std::io;
use std::net::IpAddr;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum KdeError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("peer closed the connection")]
    UnexpectedEof,
    #[error("line exceeds the {limit} byte limit; connection is not resynchronised")]
    LineTooLong { limit: usize },
    #[error("outgoing packet of {size} bytes exceeds the {limit} byte limit")]
    OutgoingTooLarge { size: usize, limit: usize },
    #[error("malformed packet: {0}")]
    MalformedPacket(String),
    #[error("invalid identity: {0}")]
    InvalidIdentity(String),
    #[error("unsupported protocol version {0} (only 8 is accepted)")]
    UnsupportedProtocol(u32),
    #[error("connection is not addressed to this device: {0}")]
    TargetMismatch(String),
    #[error("protocol downgrade refused: stored {stored}, claimed {claimed}")]
    ProtocolDowngrade { stored: u32, claimed: u32 },
    #[error("TLS handshake failed: {0}")]
    Tls(String),
    #[error("invalid certificate: {0}")]
    Certificate(String),
    #[error("certificate does not match the pinned certificate for {device_id}")]
    PinMismatch { device_id: String },
    #[error("peer identity does not match what was verified: {0}")]
    IdentityMismatch(String),
    #[error("peer address {0} is not permitted by the address policy")]
    AddressRejected(IpAddr),
    #[error("too many unpaired connections")]
    TooManyConnections,
    #[error("pairing failed: {0}")]
    Pairing(#[from] PairingFailure),
    #[error("trust store error: {0}")]
    Store(String),
    #[error("trust store at {path} is corrupt: {reason}")]
    StoreCorrupt { path: PathBuf, reason: String },
    #[error("{what} is not private enough: {detail}")]
    InsecurePermissions { what: String, detail: String },
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(&'static str),
}

/// Why a pairing attempt ended without establishing trust.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingFailure {
    #[error("the pairing request timed out")]
    Timeout,
    #[error("the peer rejected or cancelled pairing")]
    RejectedByPeer,
    #[error("pairing was rejected locally")]
    RejectedLocally,
    #[error("peer timestamp is outside the permitted +-1800 s window (clocks out of sync)")]
    ClockSkew,
    #[error("malformed pairing packet: {0}")]
    Malformed(&'static str),
    #[error("too many unexpected or duplicate packets while unpaired")]
    TooManyUnexpectedPackets,
    #[error("the connection or certificate changed during pairing")]
    BindingChanged,
    #[error("the link closed before pairing completed")]
    LinkClosed,
}

pub type Result<T> = std::result::Result<T, KdeError>;
