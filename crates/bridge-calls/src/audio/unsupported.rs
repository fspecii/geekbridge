// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio "server" of platforms without a native call-audio adapter here. Every operation
//! fails with an explicit reason; Windows and macOS route call audio through their own crates.

use crate::audio::model::ServerSnapshot;
use crate::audio::server::{AudioServer, ServerError};

#[derive(Clone, Debug)]
pub struct UnsupportedAudioServer {
    reason: String,
}

impl UnsupportedAudioServer {
    pub fn new(platform: &str) -> Self {
        Self {
            reason: format!("call-audio routing is not implemented for {platform} here"),
        }
    }
    fn err(&self) -> ServerError {
        ServerError::Unavailable(self.reason.clone())
    }
}

impl AudioServer for UnsupportedAudioServer {
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError> {
        Err(self.err())
    }
    fn move_sink_input(&mut self, _: u32, _: &str) -> Result<(), ServerError> {
        Err(self.err())
    }
    fn move_source_output(&mut self, _: u32, _: &str) -> Result<(), ServerError> {
        Err(self.err())
    }
    fn set_card_profile(&mut self, _: u32, _: &str) -> Result<(), ServerError> {
        Err(self.err())
    }
    fn set_source_mute(&mut self, _: &str, _: bool) -> Result<(), ServerError> {
        Err(self.err())
    }
    fn reconnect(&mut self) -> Result<(), ServerError> {
        Err(self.err())
    }
}
