// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared domain types and a bounded, read-only local discovery API.
pub mod agenda;
pub mod battery_link;
pub mod calls;
pub mod calls_link;
pub mod calls_view;
pub mod clipboard;
pub mod clipboard_wire;
pub mod collate;
pub mod debug_link;
pub mod heartbeat;
pub mod hotkey;
pub mod ipc;
pub mod messages;
pub mod pairing_help;
pub mod phone_link;
pub mod phone_view;
pub mod report;
pub mod share;
pub mod statefile;
pub mod whats_new;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::{Duration, Instant},
};

pub const DEFAULT_API_PORT: u16 = 17380;
pub const API_VERSION: u32 = 1;
pub const MAX_API_BYTES: usize = 256 * 1024;
pub const MAX_DEVICES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    Calls,
    Messages,
    /// The Calendar app. The variant keeps its original name (and `geekbridge-agenda` its binary
    /// name) so installs, scripts and desktop entries keep working.
    Agenda,
    Contacts,
    Clipboard,
}
impl AppKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Calls => "Calls",
            Self::Messages => "Messages",
            Self::Agenda => "Calendar",
            Self::Contacts => "Contacts",
            Self::Clipboard => "Clipboard",
        }
    }
    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Calls => "Your phone, within reach.",
            Self::Messages => "A little more room for conversation.",
            Self::Agenda => "Your day, shared with Google Calendar.",
            Self::Contacts => "Your phone's phonebook.",
            Self::Clipboard => "Pick up where you copied.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Discovered,
}

/// Discovery is untrusted metadata, never authorization for a feature.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub address: std::net::IpAddr,
    pub tcp_port: u16,
    pub protocol_version: u32,
    pub incoming_capabilities: Vec<String>,
    pub outgoing_capabilities: Vec<String>,
    pub state: ConnectionState,
    pub last_seen_unix_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// True if oversized discovery capability lists were shortened for local display.
    #[serde(default)]
    pub capabilities_truncated: bool,
    pub api_version: u32,
    pub discovery_port: u16,
    pub discovery_error: Option<String>,
    pub devices: Vec<Device>,
}
impl Snapshot {
    pub fn empty(port: u16) -> Self {
        Self {
            capabilities_truncated: false,
            api_version: API_VERSION,
            discovery_port: port,
            discovery_error: None,
            devices: vec![],
        }
    }
}

pub fn api_address(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

/// This API deliberately exposes only public discovery data. No phone content or commands.
pub fn fetch_snapshot(port: u16) -> io::Result<Snapshot> {
    let mut stream = TcpStream::connect_timeout(&api_address(port), Duration::from_millis(500))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(b"GET /v1/discovery HTTP/1.0\r\n\r\n")?;
    let mut bytes = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut chunk = [0_u8; 4096];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "service response deadline exceeded",
                )
            })?;
        stream.set_read_timeout(Some(remaining))?;
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MAX_API_BYTES {
            return Err(io::Error::other("service response too large"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    parse_response(&bytes)
}

fn parse_response(bytes: &[u8]) -> io::Result<Snapshot> {
    let mut reader = BufReader::new(bytes);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim_end() != "HTTP/1.0 200 OK" {
        return Err(io::Error::other("service rejected discovery request"));
    }
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::other("incomplete service headers"));
        }
        if line == "\r\n" {
            break;
        }
    }
    let snapshot: Snapshot = serde_json::from_reader(reader)?;
    if snapshot.api_version != API_VERSION {
        return Err(io::Error::other("incompatible GeekBridge service version"));
    }
    if snapshot.devices.len() > MAX_DEVICES {
        return Err(io::Error::other("too many discovery records"));
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_deadline_is_total_even_when_a_service_trickles_bytes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..100 {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let start = Instant::now();
        assert!(fetch_snapshot(port).is_err());
        assert!(start.elapsed() < Duration::from_millis(2500));
        peer.join().unwrap();
    }

    #[test]
    fn refuses_wrong_service_and_version() {
        assert!(parse_response(b"HTTP/1.0 404 Not Found\r\n\r\n{}").is_err());
        let mut state = Snapshot::empty(1716);
        state.api_version += 1;
        let response = format!(
            "HTTP/1.0 200 OK\r\n\r\n{}",
            serde_json::to_string(&state).unwrap()
        );
        assert!(parse_response(response.as_bytes()).is_err());
    }
}
