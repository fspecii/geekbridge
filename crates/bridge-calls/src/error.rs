// SPDX-License-Identifier: GPL-3.0-or-later
//! Failure types. Control failures are separated by *why* the command was not (or may not have
//! been) executed, so a caller can tell "nothing happened" from "outcome unknown".

use bridge_core::calls::CommandRejection;
use serde::Serialize;

/// Reading backend state failed. An absent service is *not* an error; see
/// [`crate::ServiceStatus`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    /// No call backend exists for this platform or build.
    #[error("call backend unavailable: {0}")]
    Unavailable(String),
    /// The message bus itself failed (connection closed, permission, timeout).
    #[error("message bus error: {0}")]
    Bus(String),
    /// The service answered with data that does not match the declared API. The data is dropped,
    /// never guessed at.
    #[error("malformed reply from {context}: {detail}")]
    Malformed { context: String, detail: String },
}

/// The error vocabulary of the PipeWire telephony service (`org.pipewire.Telephony.Error.*`,
/// `telephony.c`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteErrorKind {
    Failed,
    NotSupported,
    InvalidFormat,
    InvalidState,
    InProgress,
    /// `CME ERROR` from the phone; the numeric code is not carried over D-Bus (the description
    /// text is).
    Cme,
    /// A D-Bus or other error name this crate does not know.
    Other,
}

impl RemoteErrorKind {
    pub fn from_dbus_name(name: &str) -> Self {
        match name.strip_prefix(crate::declared::ERROR_PREFIX) {
            Some("Failed") => Self::Failed,
            Some("NotSupported") => Self::NotSupported,
            Some("InvalidFormat") => Self::InvalidFormat,
            Some("InvalidState") => Self::InvalidState,
            Some("InProgress") => Self::InProgress,
            Some("CME") => Self::Cme,
            _ => Self::Other,
        }
    }
}

/// A control request was not carried out, or its outcome is not known.
///
/// Variants other than [`ControlError::Timeout`] and [`ControlError::Bus`] mean the request was
/// refused *before* any method call was sent, or the service replied with an error; in both cases
/// the phone did not act on it as far as this crate can tell. `Timeout` and `Bus` mean it may have
/// been delivered: re-read state instead of retrying blindly (a retried `Dial` could place two
/// calls).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("call control unavailable: {0}")]
    Unavailable(String),
    /// Refused by the platform-neutral model's pre-flight (capability, argument, link, call state).
    #[error("command rejected by pre-flight check: {0:?}")]
    Preflight(CommandRejection),
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    /// The backend has no such operation at all (as opposed to "not declared by this phone").
    #[error("not supported by this backend: {0}")]
    Unsupported(&'static str),
    /// The telephony service is not on the bus any more.
    #[error("telephony service is no longer on the bus")]
    ServiceGone,
    /// The service restarted since the handle was issued; every earlier handle is void.
    #[error("telephony service restarted since this device handle was issued")]
    ServiceRestarted,
    #[error("no such phone: it is not (or no longer) connected")]
    NoSuchDevice,
    /// The object path now belongs to a different phone.
    #[error("device handle is stale: the object path now belongs to a different phone")]
    StaleDevice,
    #[error("no such call: it ended or was never observed")]
    NoSuchCall,
    /// The call object path was reused by a different call.
    #[error("call handle is stale: the object path now belongs to a different call")]
    StaleCall,
    /// The call reports a state string this crate does not know; nothing is sent.
    #[error("call is in an unrecognized state {0:?}; refusing to act")]
    UnrecognizedCallState(String),
    /// The bus object does not declare this method with the expected signature.
    #[error("{interface}.{member} is not declared by the service ({detail})")]
    NotDeclared {
        interface: &'static str,
        member: &'static str,
        detail: String,
    },
    /// The current phone/call state does not allow this action (checked locally against fresh
    /// data, mirroring the service's own checks).
    #[error("current state does not allow this action: {0}")]
    WrongState(&'static str),
    /// The service replied with a telephony error.
    #[error("service refused ({kind:?}): {message}")]
    Remote {
        kind: RemoteErrorKind,
        name: String,
        message: String,
    },
    /// No reply within the method timeout. **Outcome unknown.**
    #[error("no reply within the method timeout; the outcome is unknown")]
    Timeout,
    /// Transport failure while sending or waiting. **Outcome may be unknown.**
    #[error("message bus error: {0}")]
    Bus(String),
}

impl ControlError {
    /// True when the request may nonetheless have reached the phone.
    pub fn outcome_unknown(&self) -> bool {
        matches!(self, Self::Timeout | Self::Bus(_))
    }
}
