// SPDX-License-Identifier: GPL-3.0-or-later
//! What the control API says about phone photos (`Request::Photos`, `Request::PhotoThumb`). Listing and
//! thumbnails only; the full image arrives through the share channel and is reported as a [`FetchInfo`].
//! Names are display text from the phone, so they only ever go through the control channel.
use serde::{Deserialize, Serialize};

/// What to do with a photo once the phone has sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchMode {
    /// Keep it in the save folder (like any received file).
    Save,
    /// Only to put it on the clipboard: it lands in a private scratch folder that is emptied soon after.
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchState {
    /// Asked the phone; waiting for it to offer the file.
    Waiting,
    Receiving,
    Done,
    Failed,
}

/// One full-size request and where it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchInfo {
    pub fetch_id: u64,
    pub device_id: String,
    pub photo_id: String,
    pub name: String,
    pub mode: FetchMode,
    pub state: FetchState,
    /// Where the file is, once `Done`.
    #[serde(default)]
    pub path: Option<String>,
    /// Short reason when `Failed`.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoItem {
    pub device_id: String,
    pub id: String,
    pub name: String,
    pub taken_ms: u64,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// A thumbnail is held and can be fetched with `PhotoThumb`.
    pub has_thumb: bool,
}

/// A phone that may show photos right now (consent on, connected, declared the packets).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoDevice {
    pub device_id: String,
    pub name: String,
    /// The phone only shows the photos the user picked (Android partial access).
    pub partial: bool,
    /// A list has arrived since consent was given.
    pub listed: bool,
}

/// Answer to `Request::Photos`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotosView {
    /// Newest first across the selected phones.
    pub items: Vec<PhotoItem>,
    /// Phones with the `photos` consent on that are connected.
    pub devices: Vec<PhotoDevice>,
    /// Connected phones that declared the photos packets but have no consent yet (the window offers to allow).
    pub offered: Vec<PhotoDevice>,
    pub fetches: Vec<FetchInfo>,
}

/// Answer to `Request::PhotoThumb`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhotoThumbView {
    pub width: u32,
    pub height: u32,
    /// Base64 of a small JPEG (at most 24 KiB decoded).
    pub jpeg_base64: String,
}
