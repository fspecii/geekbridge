use bridge_core::{ConnectionState, Device};
use serde::Deserialize;
use std::net::IpAddr;

pub const MAX_IDENTITY_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
struct Packet {
    #[serde(rename = "type")]
    kind: String,
    body: Identity,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    device_id: String,
    device_name: String,
    #[serde(default)]
    device_type: String,
    #[serde(default = "default_tcp_port")]
    tcp_port: u16,
    protocol_version: u32,
    #[serde(default)]
    incoming_capabilities: Vec<String>,
    #[serde(default)]
    outgoing_capabilities: Vec<String>,
}

fn default_tcp_port() -> u16 {
    1716
}

pub fn parse_identity(data: &[u8], source: IpAddr, now: u64) -> Result<Device, &'static str> {
    if data.len() > MAX_IDENTITY_BYTES {
        return Err("identity too large");
    }
    if source.is_unspecified() || source.is_multicast() {
        return Err("invalid source");
    }
    let packet: Packet = serde_json::from_slice(data).map_err(|_| "invalid identity JSON")?;
    if packet.kind != "kdeconnect.identity" {
        return Err("not an identity");
    }
    let body = packet.body;
    if !(32..=38).contains(&body.device_id.len())
        || !body
            .device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid device ID");
    }
    if body.device_name.trim().is_empty() || body.device_name.chars().any(char::is_control) {
        return Err("invalid device name");
    }
    let device_type = match body.device_type.as_str() {
        "phone" | "smartphone" => "phone",
        "tablet" => "tablet",
        "desktop" => "desktop",
        "laptop" => "laptop",
        "tv" => "tv",
        _ => "unknown",
    }
    .to_owned();
    let name: String = body.device_name.chars().take(64).collect();
    if !(1716..=1764).contains(&body.tcp_port) {
        return Err("invalid KDE Connect TCP port");
    }
    if !(7..=8).contains(&body.protocol_version) {
        return Err("unsupported protocol version");
    }
    for caps in [&body.incoming_capabilities, &body.outgoing_capabilities] {
        if caps.len() > 128
            || caps
                .iter()
                .any(|s| s.len() > 128 || !s.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err("invalid capabilities");
        }
    }
    Ok(Device {
        id: body.device_id,
        name,
        device_type,
        address: source,
        tcp_port: body.tcp_port,
        protocol_version: body.protocol_version,
        incoming_capabilities: body.incoming_capabilities,
        outgoing_capabilities: body.outgoing_capabilities,
        state: ConnectionState::Discovered,
        last_seen_unix_ms: now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> serde_json::Value {
        serde_json::json!({"id": 1, "type":"kdeconnect.identity", "body": {
            "deviceId":"0123456789abcdef0123456789abcdef", "deviceName":"Android phone", "deviceType":"phone",
            "tcpPort":1716, "protocolVersion":8, "incomingCapabilities":["kdeconnect.ping"], "outgoingCapabilities":[]
        }})
    }
    fn parse(value: &serde_json::Value) -> Result<Device, &'static str> {
        parse_identity(
            &serde_json::to_vec(value).unwrap(),
            "192.168.1.8".parse().unwrap(),
            42,
        )
    }
    #[test]
    fn identity_is_always_untrusted() {
        let device = parse(&fixture()).unwrap();
        assert_eq!(device.state, ConnectionState::Discovered);
        assert_eq!(device.last_seen_unix_ms, 42);
    }
    #[test]
    fn rejects_bad_or_hostile_input() {
        for (key, value) in [
            ("deviceId", serde_json::json!("../bad")),
            ("deviceName", serde_json::json!("bad\nname")),
            ("tcpPort", serde_json::json!(22)),
            ("protocolVersion", serde_json::json!(9)),
            ("incomingCapabilities", serde_json::json!(["has space"])),
        ] {
            let mut packet = fixture();
            packet["body"][key] = value;
            assert!(parse(&packet).is_err(), "accepted invalid {key}");
        }
        assert!(
            parse_identity(
                &vec![b' '; MAX_IDENTITY_BYTES + 1],
                "127.0.0.1".parse().unwrap(),
                0
            )
            .is_err()
        );
        let mut packet = fixture();
        packet["type"] = "kdeconnect.pair".into();
        assert!(parse(&packet).is_err());
    }
}
