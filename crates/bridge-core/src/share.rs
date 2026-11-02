// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire types of the file / link / text sharing feature on the local control API.
//!
//! The service moves the bytes; clients only start transfers, answer offers and read progress.
//! File *names* from phones are already reduced to one safe path component by the service. Text
//! and URLs are phone-supplied display text: treat them as untrusted, never execute them, and only
//! offer `http`/`https` links to the system opener.

use serde::{Deserialize, Serialize};

/// Longest text accepted by `Request::ShareSendText` (the control request limit is 16 KiB).
pub const MAX_SEND_TEXT_BYTES: usize = 8 * 1024;
/// Longest path accepted by `Request::ShareSendFile`.
pub const MAX_SEND_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareTextKind {
    Text,
    Url,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Incoming,
    Outgoing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    File,
    Text,
    Url,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Incoming file waiting for Accept / Decline.
    Offered,
    /// Outgoing file announced; waiting for the phone to connect.
    Waiting,
    Active,
    Done,
    Failed,
    Cancelled,
    Declined,
}

impl TransferState {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Offered | Self::Waiting | Self::Active)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferInfo {
    pub transfer_id: u64,
    pub device_id: String,
    pub device_name: String,
    pub direction: TransferDirection,
    pub kind: TransferKind,
    /// File name (files only), a single safe path component.
    pub name: String,
    /// Announced size; `None` when unknown.
    pub size: Option<u64>,
    pub bytes: u64,
    pub state: TransferState,
    /// Short machine code when `state` is `failed` (`timeout`, `too_large`, `io_error`, ...).
    pub code: Option<String>,
    /// Where a received file was saved (`done`, incoming files only).
    pub path: Option<String>,
    /// Received text or URL (text/url kinds only), untrusted.
    pub text: Option<String>,
    /// Milliseconds before an unanswered offer expires.
    pub expires_in_ms: Option<u64>,
    pub at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareDeviceSettings {
    pub device_id: String,
    /// The phone may send us files, links and text at all.
    pub receive: bool,
    /// Accept incoming files from this phone without asking.
    pub auto_accept: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareStatus {
    /// Where accepted files are stored (default `~/Downloads/GeekBridge`).
    pub save_dir: String,
    pub max_file_bytes: u64,
    pub devices: Vec<ShareDeviceSettings>,
    /// Newest last, bounded.
    pub transfers: Vec<TransferInfo>,
}
