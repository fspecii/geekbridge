// SPDX-License-Identifier: GPL-3.0-or-later
//! Narrow ports between the testable orchestration and the Apple frameworks.
//!
//! [`NativePort`] is everything the orchestrator needs from IOBluetooth; [`AudioPort`] everything
//! it needs from CoreAudio. The Apple implementations live in `apple/` (macOS only) and the
//! scripted ones in `mock` (tests). Ports are **not** `Send`: Objective-C objects are owned by the
//! thread that created them (the process main thread running a `CFRunLoop`).
use crate::model::{AgFeatures, BluetoothAuth, BtAddress, Generation, PhoneRecord, Tagged};
use crate::plan::NativeOp;
use bridge_core::calls::Sensitive;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::mpsc::Sender;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NativeErrorKind {
    /// The Objective-C layer was used off the thread that owns it.
    WrongThread,
    /// `IOBluetoothDevice` could not be created or is not known to the system.
    DeviceNotFound,
    NotPaired,
    /// `IOBluetoothHandsFreeDevice initWithDevice:delegate:` returned nil.
    InitFailed,
    /// macOS refused access to Bluetooth.
    PermissionDenied,
    NoSuchLink,
    /// Any other framework failure; detail carries no phone numbers.
    Other {
        detail: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeError {
    pub op: String,
    #[serde(flatten)]
    pub kind: NativeErrorKind,
}
impl NativeError {
    pub fn new(op: impl Into<String>, kind: NativeErrorKind) -> Self {
        Self {
            op: op.into(),
            kind,
        }
    }
    pub fn other(op: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(
            op,
            NativeErrorKind::Other {
                detail: detail.into(),
            },
        )
    }
}
impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.op, self.kind)
    }
}
impl std::error::Error for NativeError {}

/// What opening a link tells the caller synchronously. The link is only usable after the
/// `SlcConnected` event arrives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenedLink {
    /// The hands-free feature bitmap in force before the explicit request, for diagnostics.
    pub default_hf_features: Option<u32>,
}

/// IOBluetooth as seen by the orchestrator. Every method must be called on the owning thread.
pub trait NativePort {
    /// False for ports on hosts that have no macOS Bluetooth stack; the controller then reports
    /// `UnsupportedHost` for every operation instead of attempting any.
    fn host_supported(&self) -> bool {
        true
    }
    /// Bluetooth privacy state, without triggering a permission prompt.
    fn authorization(&mut self) -> BluetoothAuth;
    /// Read-only: list paired devices with their cached HFP-gateway classification. Opens no
    /// connection and sends no command.
    fn paired_phones(&mut self) -> Result<Vec<PhoneRecord>, NativeError>;
    /// Create the hands-free object and delegate for `address`, tag every callback with
    /// `generation`, and start the service-level connection (`connect`).
    fn open_link(
        &mut self,
        address: &BtAddress,
        generation: Generation,
        sink: Sender<Tagged>,
    ) -> Result<OpenedLink, NativeError>;
    /// The gateway's reported feature bitmap (`deviceSupportedFeatures`); 0 if not yet known.
    fn gateway_features(&mut self, generation: Generation) -> Result<AgFeatures, NativeError>;
    /// Execute one planned operation on the link of `generation`.
    fn perform(&mut self, generation: Generation, op: &NativeOp) -> Result<(), NativeError>;
    /// Ask for the call list; entries arrive as `NativeEvent::CurrentCall` (enrichment only).
    fn request_call_list(&mut self, generation: Generation) -> Result<(), NativeError>;
    /// Begin an orderly disconnect (`disconnect`); `SlcDisconnected` may follow.
    fn close_link(&mut self, generation: Generation) -> Result<(), NativeError>;
    /// Detach the delegate and stop forwarding callbacks of `generation`. Idempotent.
    fn retire_link(&mut self, generation: Generation);
}

// ---------------------------------------------------------------- audio

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioTransport {
    Bluetooth,
    BluetoothLe,
    BuiltIn,
    Usb,
    Virtual,
    Aggregate,
    Other,
}

/// One CoreAudio device, identified by its stable UID (the numeric `AudioObjectID` can change).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub uid: String,
    pub name: Option<Sensitive>,
    pub transport: AudioTransport,
    pub input_channels: u32,
    pub output_channels: u32,
}
impl AudioDevice {
    pub fn has_input(&self) -> bool {
        self.input_channels > 0
    }
    pub fn has_output(&self) -> bool {
        self.output_channels > 0
    }
    /// Bluetooth devices with both directions are candidates for HFP call audio.
    pub fn is_duplex_bluetooth(&self) -> bool {
        self.transport == AudioTransport::Bluetooth && self.has_input() && self.has_output()
    }
    /// Heuristic only: classic Bluetooth device UIDs usually embed the address. Not proof that
    /// this endpoint belongs to the selected phone.
    pub fn address_hint(&self, address: &BtAddress) -> bool {
        self.uid
            .to_ascii_uppercase()
            .replace('-', ":")
            .contains(address.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioError {
    pub op: String,
    /// CoreAudio `OSStatus`, when the failure came from the framework.
    pub status: Option<i32>,
    pub detail: String,
}
impl AudioError {
    pub fn new(op: impl Into<String>, status: Option<i32>, detail: impl Into<String>) -> Self {
        Self {
            op: op.into(),
            status,
            detail: detail.into(),
        }
    }
}
impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed ({:?}): {}", self.op, self.status, self.detail)
    }
}
impl std::error::Error for AudioError {}

/// CoreAudio as seen by the route manager. Reads are side-effect free.
pub trait AudioPort {
    fn devices(&mut self) -> Result<Vec<AudioDevice>, AudioError>;
    fn default_input(&mut self) -> Result<Option<String>, AudioError>;
    fn default_output(&mut self) -> Result<Option<String>, AudioError>;
    /// Changes the system default for every application; callers own the restore.
    fn set_default_input(&mut self, uid: &str) -> Result<(), AudioError>;
    fn set_default_output(&mut self, uid: &str) -> Result<(), AudioError>;
}
