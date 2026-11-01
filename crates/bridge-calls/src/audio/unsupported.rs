//! The audio "server" of platforms without a native call-audio adapter yet. Every operation
//! fails with an explicit reason; the Windows (WASAPI/virtual-device) and macOS (CoreAudio)
//! adapters will implement [`AudioServer`] in their place.

use crate::audio::model::ServerSnapshot;
use crate::audio::server::{AudioServer, ServerError};

#[derive(Clone, Debug)]
pub struct UnsupportedAudioServer {
    reason: String,
}

impl UnsupportedAudioServer {
    pub fn new(platform: &str) -> Self {
        Self {
            reason: format!("call-audio routing is not implemented for {platform} yet"),
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
    fn load_module(&mut self, _: &str, _: &str) -> Result<u32, ServerError> {
        Err(self.err())
    }
    fn unload_module(&mut self, _: u32) -> Result<(), ServerError> {
        Err(self.err())
    }
    fn reconnect(&mut self) -> Result<(), ServerError> {
        Err(self.err())
    }
}
