// SPDX-License-Identifier: GPL-3.0-or-later
//! The only way call-audio code touches an audio server.
//!
//! The trait is deliberately narrow. Besides reading, it can: move one stream to a named device,
//! switch one card's profile, mute/unmute one source, and measure a peak level. It has no method
//! to change the default sink/source, load modules or write configuration, so call audio can
//! never leave anything behind that outlives the call except what the controller records in its
//! restore file (a card profile and a source mute) and undoes.

use std::time::Duration;

use crate::audio::model::ServerSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServerError {
    /// No server could be reached, or this platform has no audio adapter.
    #[error("audio server unavailable: {0}")]
    Unavailable(String),
    /// The connection to a running server dropped.
    #[error("audio server connection lost: {0}")]
    ConnectionLost(String),
    /// A bounded wait expired. For a write the outcome is unknown: the server may still act.
    #[error("audio server did not answer within the time limit ({what})")]
    Timeout { what: &'static str },
    /// The server answered and refused.
    #[error("audio server refused {what}: {detail}")]
    Rejected { what: &'static str, detail: String },
}

impl ServerError {
    pub fn is_connection_loss(&self) -> bool {
        matches!(self, Self::ConnectionLost(_) | Self::Unavailable(_))
    }
}

/// What to measure the level of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeakTarget {
    /// A playback stream, measured through its sink's monitor (only that stream's samples).
    SinkInput { index: u32, sink: String },
    /// A source (microphone), by name.
    Source(String),
}

/// Blocking audio-server access. Every method is bounded in time by the implementation.
pub trait AudioServer: Send {
    /// Read everything call audio needs in one pass.
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError>;
    /// Move a playback stream to the sink called `sink`.
    fn move_sink_input(&mut self, index: u32, sink: &str) -> Result<(), ServerError>;
    /// Move a capture stream to the source called `source`.
    fn move_source_output(&mut self, index: u32, source: &str) -> Result<(), ServerError>;
    /// Activate `profile` on card `card` (by index).
    fn set_card_profile(&mut self, card: u32, profile: &str) -> Result<(), ServerError>;
    /// Mute or unmute the source called `source`.
    fn set_source_mute(&mut self, source: &str, mute: bool) -> Result<(), ServerError>;
    /// Highest absolute sample level (0.0..=1.0) seen on `target` within `window`. `Ok(None)` if
    /// this server cannot measure it; `Ok(Some(0.0))` is digital silence.
    fn peak(&mut self, target: &PeakTarget, window: Duration) -> Result<Option<f32>, ServerError> {
        let _ = (target, window);
        Ok(None)
    }
    /// Drop the old connection and establish a new one. Never starts a server.
    fn reconnect(&mut self) -> Result<(), ServerError>;
}
