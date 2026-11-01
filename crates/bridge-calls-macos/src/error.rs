//! Errors surfaced by the adapter to its caller (the daemon or the helper protocol).
use crate::model::{BluetoothAuth, Generation};
use crate::plan::Refusal;
use crate::port::NativeError;
use crate::route::RouteError;
use bridge_core::calls::CommandRejection;
use serde::Serialize;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum AdapterError {
    /// This build has no macOS Bluetooth stack.
    UnsupportedHost {
        os: &'static str,
    },
    /// macOS refuses (or restricts) Bluetooth access; nothing native is attempted.
    PermissionDenied {
        authorization: BluetoothAuth,
    },
    /// The user has not been asked yet and the caller did not allow a prompt.
    PermissionNotDetermined,
    InvalidAddress,
    /// The address is not among the paired devices.
    UnknownPhone,
    NotPaired,
    /// Paired, but macOS's cached SDP data does not list the Hands-Free Audio Gateway role.
    NotHandsFreeGateway,
    /// A link is already open or opening; only one phone is driven at a time.
    LinkBusy {
        generation: Generation,
    },
    NoActiveLink,
    /// The service-level connection is not established yet.
    LinkNotReady,
    /// Another operation of the same plane is awaiting confirmation.
    Busy,
    /// The per-process limit on retired delegates was reached; restart the helper.
    TooManyLinks,
    /// The core capability/state model refused the command.
    Rejected {
        rejection: CommandRejection,
    },
    /// The adapter refused the command as unsafe or ambiguous.
    Refused {
        refusal: Refusal,
    },
    Native {
        native: NativeError,
    },
    Route {
        route: RouteError,
    },
    /// Malformed or oversized helper protocol input.
    Protocol {
        detail: String,
    },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHost { os } => {
                write!(f, "macOS Bluetooth calling is not available on {os}")
            }
            Self::PermissionDenied { authorization } => {
                write!(
                    f,
                    "Bluetooth access is {authorization:?}; grant it in System Settings"
                )
            }
            Self::PermissionNotDetermined => {
                f.write_str("Bluetooth permission has not been requested yet")
            }
            Self::InvalidAddress => f.write_str("invalid Bluetooth address"),
            Self::UnknownPhone => f.write_str("no such paired device"),
            Self::NotPaired => f.write_str("device is not paired"),
            Self::NotHandsFreeGateway => {
                f.write_str("device does not advertise the Hands-Free Audio Gateway role")
            }
            Self::LinkBusy { generation } => write!(f, "link {} is already open", generation.0),
            Self::NoActiveLink => f.write_str("no active phone link"),
            Self::LinkNotReady => f.write_str("phone link is still connecting"),
            Self::Busy => f.write_str("another operation is awaiting confirmation"),
            Self::TooManyLinks => f.write_str("too many link attempts in this process"),
            Self::Rejected { rejection } => write!(f, "command rejected: {rejection:?}"),
            Self::Refused { refusal } => write!(f, "command refused: {refusal}"),
            Self::Native { native } => write!(f, "native failure: {native}"),
            Self::Route { route } => write!(f, "audio route failure: {route}"),
            Self::Protocol { detail } => write!(f, "protocol error: {detail}"),
        }
    }
}
impl std::error::Error for AdapterError {}

impl From<Refusal> for AdapterError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused { refusal }
    }
}
impl From<NativeError> for AdapterError {
    fn from(native: NativeError) -> Self {
        Self::Native { native }
    }
}
impl From<RouteError> for AdapterError {
    fn from(route: RouteError) -> Self {
        Self::Route { route }
    }
}
impl From<CommandRejection> for AdapterError {
    fn from(rejection: CommandRejection) -> Self {
        Self::Rejected { rejection }
    }
}
