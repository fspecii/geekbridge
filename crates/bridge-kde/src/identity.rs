//! Device identity packets (`kdeconnect.identity`) and their validation.
//!
//! Everything in an identity is attacker-controlled until it has been checked
//! against the TLS-authenticated certificate; nothing here establishes trust.

use std::fmt;

use serde_json::{Map, Value};

use crate::error::{KdeError, Result};
use crate::packet::Packet;

/// The only protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 8;
pub const IDENTITY_TYPE: &str = "kdeconnect.identity";

const MAX_NAME_CHARS: usize = 32;
const MAX_CAPABILITIES: usize = 256;
const MAX_CAPABILITY_LEN: usize = 128;
/// Characters stripped from device names, as stock peers do. Control characters
/// are also removed so a peer cannot inject terminal escape sequences.
const NAME_STRIP: &[char] = &[
    '"', '\'', ',', ';', ':', '.', '!', '?', '(', ')', '[', ']', '<', '>',
];

/// A validated device identifier: 32 to 38 characters of `[A-Za-z0-9_-]`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn parse(s: &str) -> Result<Self> {
        let ok = (32..=38).contains(&s.len())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(KdeError::InvalidIdentity(
                "deviceId must be 32-38 characters of [A-Za-z0-9_-]".into(),
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.0)
    }
}

/// Strip the characters peers strip, drop control characters, cap the length.
/// Returns `None` when nothing is left.
pub fn sanitize_device_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !NAME_STRIP.contains(c) && !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Longest name a user may choose (what stock KDE Connect keeps).
pub const MAX_USER_NAME_CHARS: usize = MAX_NAME_CHARS;

/// A name typed by the user: refuse control characters and anything longer than
/// [`MAX_USER_NAME_CHARS`] instead of silently cutting it, then apply the same filtering peers
/// apply (so what is saved is exactly what the other device will show).
pub fn validate_device_name(raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        return Err(KdeError::InvalidIdentity(
            "the name contains control characters".into(),
        ));
    }
    if raw.trim().chars().count() > MAX_USER_NAME_CHARS {
        return Err(KdeError::InvalidIdentity(format!(
            "the name is longer than {MAX_USER_NAME_CHARS} characters"
        )));
    }
    sanitize_device_name(raw)
        .ok_or_else(|| KdeError::InvalidIdentity("the name is empty after filtering".into()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceType {
    Desktop,
    Laptop,
    Phone,
    Tablet,
    Tv,
    Unknown,
}

impl DeviceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Laptop => "laptop",
            Self::Phone => "phone",
            Self::Tablet => "tablet",
            Self::Tv => "tv",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "desktop" => Self::Desktop,
            "laptop" => Self::Laptop,
            "phone" => Self::Phone,
            "tablet" => Self::Tablet,
            "tv" => Self::Tv,
            _ => Self::Unknown,
        }
    }
}

/// An identity as sent or received. `target_*` appear only on the plaintext
/// connection packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub device_id: DeviceId,
    pub device_name: String,
    pub protocol_version: u32,
    pub device_type: DeviceType,
    pub incoming_capabilities: Vec<String>,
    pub outgoing_capabilities: Vec<String>,
    pub target_device_id: Option<DeviceId>,
    pub target_protocol_version: Option<u32>,
}

impl Identity {
    pub fn local(
        device_id: DeviceId,
        device_name: &str,
        device_type: DeviceType,
        incoming: Vec<String>,
        outgoing: Vec<String>,
    ) -> Result<Self> {
        let device_name = sanitize_device_name(device_name).ok_or_else(|| {
            KdeError::InvalidIdentity("device name is empty after filtering".into())
        })?;
        for cap in incoming.iter().chain(outgoing.iter()) {
            if cap.is_empty() || cap.len() > MAX_CAPABILITY_LEN {
                return Err(KdeError::InvalidIdentity(
                    "capability name has an invalid length".into(),
                ));
            }
        }
        if incoming.len() > MAX_CAPABILITIES || outgoing.len() > MAX_CAPABILITIES {
            return Err(KdeError::InvalidIdentity("too many capabilities".into()));
        }
        Ok(Self {
            device_id,
            device_name,
            protocol_version: PROTOCOL_VERSION,
            device_type,
            incoming_capabilities: incoming,
            outgoing_capabilities: outgoing,
            target_device_id: None,
            target_protocol_version: None,
        })
    }

    /// The plaintext first packet of a TCP connection: our identity plus the
    /// device and protocol version it is addressed to.
    pub fn addressed_to(&self, target: &DeviceId) -> Self {
        let mut out = self.clone();
        out.target_device_id = Some(target.clone());
        out.target_protocol_version = Some(PROTOCOL_VERSION);
        out
    }

    pub fn to_packet(&self) -> Packet {
        let mut body = Map::new();
        body.insert("deviceId".into(), Value::from(self.device_id.as_str()));
        body.insert("deviceName".into(), Value::from(self.device_name.as_str()));
        body.insert("protocolVersion".into(), Value::from(self.protocol_version));
        body.insert("deviceType".into(), Value::from(self.device_type.as_str()));
        body.insert(
            "incomingCapabilities".into(),
            Value::from(self.incoming_capabilities.clone()),
        );
        body.insert(
            "outgoingCapabilities".into(),
            Value::from(self.outgoing_capabilities.clone()),
        );
        if let Some(t) = &self.target_device_id {
            body.insert("targetDeviceId".into(), Value::from(t.as_str()));
        }
        if let Some(v) = self.target_protocol_version {
            body.insert("targetProtocolVersion".into(), Value::from(v));
        }
        Packet::new(IDENTITY_TYPE, body)
    }

    /// Validate an incoming identity packet. `protocolVersion` and
    /// `targetProtocolVersion` may be JSON integers or decimal strings (stock
    /// Android writes the latter on the TCP line). Version 8 is mandatory.
    pub fn from_packet(packet: &Packet) -> Result<Self> {
        if packet.kind != IDENTITY_TYPE {
            return Err(KdeError::InvalidIdentity(format!(
                "unexpected packet type {:?}",
                truncate(&packet.kind)
            )));
        }
        let body = &packet.body;
        let device_id = DeviceId::parse(str_field(body, "deviceId")?)?;
        let device_name =
            sanitize_device_name(str_field(body, "deviceName")?).ok_or_else(|| {
                KdeError::InvalidIdentity("deviceName is empty after filtering".into())
            })?;
        let protocol_version = lenient_u32(body.get("protocolVersion")).ok_or_else(|| {
            KdeError::InvalidIdentity("protocolVersion missing or not an unsigned integer".into())
        })?;
        if protocol_version != PROTOCOL_VERSION {
            return Err(KdeError::UnsupportedProtocol(protocol_version));
        }
        let device_type = match body.get("deviceType") {
            None => DeviceType::Unknown,
            Some(Value::String(s)) => DeviceType::parse(s),
            Some(_) => {
                return Err(KdeError::InvalidIdentity(
                    "deviceType is not a string".into(),
                ));
            }
        };
        let target_device_id = match body.get("targetDeviceId") {
            None => None,
            Some(Value::String(s)) => Some(DeviceId::parse(s)?),
            Some(_) => {
                return Err(KdeError::InvalidIdentity(
                    "targetDeviceId is not a string".into(),
                ));
            }
        };
        let target_protocol_version = match body.get("targetProtocolVersion") {
            None => None,
            Some(v) => Some(lenient_u32(Some(v)).ok_or_else(|| {
                KdeError::InvalidIdentity("targetProtocolVersion is not an unsigned integer".into())
            })?),
        };
        Ok(Self {
            device_id,
            device_name,
            protocol_version,
            device_type,
            incoming_capabilities: capabilities(body, "incomingCapabilities")?,
            outgoing_capabilities: capabilities(body, "outgoingCapabilities")?,
            target_device_id,
            target_protocol_version,
        })
    }
}

fn truncate(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(48).collect()
}

fn str_field<'a>(body: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    match body.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(KdeError::InvalidIdentity(format!(
            "{key} missing or not a string"
        ))),
    }
}

/// Accept a non-negative JSON integer or a string of 1-10 ASCII digits that
/// fits in `u32`. Floats, negatives, null and anything else are rejected.
pub(crate) fn lenient_u32(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        Value::String(s)
            if !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            s.parse().ok()
        }
        _ => None,
    }
}

fn capabilities(body: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    let Some(v) = body.get(key) else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = v else {
        return Err(KdeError::InvalidIdentity(format!("{key} is not an array")));
    };
    if items.len() > MAX_CAPABILITIES {
        return Err(KdeError::InvalidIdentity(format!(
            "{key} has too many entries"
        )));
    }
    items
        .iter()
        .map(|i| match i {
            Value::String(s) if !s.is_empty() && s.len() <= MAX_CAPABILITY_LEN => Ok(s.clone()),
            _ => Err(KdeError::InvalidIdentity(format!(
                "{key} contains an invalid entry"
            ))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 32).collect()
    }

    #[test]
    fn user_names_are_validated_not_silently_cut() {
        assert_eq!(
            validate_device_name("  Work laptop ").unwrap(),
            "Work laptop"
        );
        assert_eq!(validate_device_name("Ana's PC").unwrap(), "Anas PC");
        for bad in ["", "   ", "...", "a\u{1b}[31mred", "tab\there", "x\ny"] {
            assert!(validate_device_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_device_name(&"a".repeat(MAX_USER_NAME_CHARS)).is_ok());
        assert!(validate_device_name(&"a".repeat(MAX_USER_NAME_CHARS + 1)).is_err());
    }

    fn packet(body: Value) -> Packet {
        Packet::new(IDENTITY_TYPE, body.as_object().unwrap().clone())
    }

    fn base() -> Value {
        json!({"deviceId": id('a'), "deviceName": "Phone", "protocolVersion": 8, "deviceType": "phone"})
    }

    #[test]
    fn device_id_bounds() {
        assert!(DeviceId::parse(&id('a')).is_ok());
        assert!(DeviceId::parse(&"a".repeat(31)).is_err());
        assert!(DeviceId::parse(&"a".repeat(39)).is_err());
        assert!(DeviceId::parse(&"a".repeat(38)).is_ok());
        assert!(DeviceId::parse("../xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").is_err());
        assert!(DeviceId::parse(&format!("{}\0", "a".repeat(32))).is_err());
        assert!(DeviceId::parse(&format!("{}é", "a".repeat(32))).is_err());
        assert!(DeviceId::parse("ee061a75_e403_4ecc_9261_5ffe2722f698").is_ok());
    }

    #[test]
    fn protocol_version_numeric_and_string() {
        for v in [json!(8), json!("8")] {
            let mut b = base();
            b["protocolVersion"] = v;
            assert_eq!(
                Identity::from_packet(&packet(b)).unwrap().protocol_version,
                8
            );
        }
        for v in [
            json!("abc"),
            json!(null),
            json!(8.5),
            json!(-8),
            json!(1u64 << 40),
            json!(""),
        ] {
            let mut b = base();
            b["protocolVersion"] = v.clone();
            assert!(
                Identity::from_packet(&packet(b)).is_err(),
                "{v} must be rejected"
            );
        }
        let mut b = base();
        b["protocolVersion"] = json!(7);
        assert!(matches!(
            Identity::from_packet(&packet(b)),
            Err(KdeError::UnsupportedProtocol(7))
        ));
    }

    #[test]
    fn target_version_may_be_string() {
        let mut b = base();
        b["targetDeviceId"] = json!(id('b'));
        b["targetProtocolVersion"] = json!("8");
        let i = Identity::from_packet(&packet(b)).unwrap();
        assert_eq!(i.target_protocol_version, Some(8));
    }

    #[test]
    fn name_filtering() {
        assert_eq!(
            sanitize_device_name("My (Phone)!").as_deref(),
            Some("My Phone")
        );
        assert_eq!(
            sanitize_device_name("a\u{1b}[31mb").as_deref(),
            Some("a31mb")
        );
        assert_eq!(sanitize_device_name("()[]<>").as_deref(), None);
        assert_eq!(
            sanitize_device_name(&"x".repeat(10_000))
                .unwrap()
                .chars()
                .count(),
            32
        );
        let mut b = base();
        b["deviceName"] = json!("...");
        assert!(Identity::from_packet(&packet(b)).is_err());
    }

    #[test]
    fn wrong_type_and_missing_fields() {
        let mut p = packet(base());
        p.kind = "kdeconnect.pair".into();
        assert!(Identity::from_packet(&p).is_err());
        let mut b = base();
        b.as_object_mut().unwrap().remove("deviceName");
        assert!(Identity::from_packet(&packet(b)).is_err());
    }

    #[test]
    fn round_trip() {
        let me = Identity::local(
            DeviceId::parse(&id('c')).unwrap(),
            "Desk",
            DeviceType::Desktop,
            vec!["kdeconnect.ping".into()],
            vec![],
        )
        .unwrap();
        let wire = me.addressed_to(&DeviceId::parse(&id('d')).unwrap());
        assert_eq!(Identity::from_packet(&wire.to_packet()).unwrap(), wire);
    }
}
