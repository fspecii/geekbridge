// SPDX-License-Identifier: GPL-3.0-or-later
//! The only way the route manager touches an audio server.
//!
//! The trait is deliberately narrow: one consistent read, load a module, unload a module,
//! reconnect. There is *no* method to set a default sink/source, move a stream, change a card
//! profile, or write configuration, so the manager cannot perform those mutations.

use crate::audio::model::ServerSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ServerError {
    /// No server could be reached, or this platform has no audio adapter.
    #[error("audio server unavailable: {0}")]
    Unavailable(String),
    /// The connection to a running server dropped. Modules this client loaded may still be
    /// loaded on the server.
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
    /// The request may have taken effect even though it reported failure.
    pub fn outcome_unknown(&self) -> bool {
        matches!(self, Self::ConnectionLost(_) | Self::Timeout { .. })
    }
}

/// Blocking audio-server access. Every method is bounded in time by the implementation.
///
/// Implementations run on the calling thread; a service on an async runtime must call the
/// manager from a blocking context (a dedicated thread or `spawn_blocking`).
pub trait AudioServer: Send {
    /// Read everything the planner needs in one pass.
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError>;
    /// `pa_context_load_module`. Returns the new module index.
    fn load_module(&mut self, name: &str, argument: &str) -> Result<u32, ServerError>;
    /// `pa_context_unload_module`.
    fn unload_module(&mut self, index: u32) -> Result<(), ServerError>;
    /// Drop the old connection and establish a new one. Never starts a server.
    fn reconnect(&mut self) -> Result<(), ServerError>;
}
