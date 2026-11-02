// SPDX-License-Identifier: GPL-3.0-or-later
//! Module names, argument strings and ownership tags for the loopback / echo-cancel modules.
//!
//! The module set is the usual PipeWire/PulseAudio call-audio graph (two `module-loopback`, optional
//! `module-echo-cancel`), written here as typed specs. Every argument string is derived from validated names, so no quoting
//! or escaping is ever needed.

use serde::Serialize;

pub const MODULE_LOOPBACK: &str = "module-loopback";
pub const MODULE_ECHO_CANCEL: &str = "module-echo-cancel";

pub const MIN_LATENCY_MS: u32 = 10;
pub const MAX_LATENCY_MS: u32 = 500;
pub const DEFAULT_LATENCY_MS: u32 = 50;

const TAG_KEY: &str = "geekbridge.session=";
const AEC_PREFIX: &str = "geekbridge_";
pub(crate) const SESSION_ID_LEN: usize = 16;

/// Names given to nodes created by a session start with this prefix. Local endpoints with this
/// prefix are refused so a route can never be built onto another session's (or an orphan's)
/// virtual node.
pub(crate) const VIRTUAL_PREFIX: &str = AEC_PREFIX;

/// A node or device name that is safe to put into a module argument string unquoted.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | ':' | '@'))
}

/// Identifies the resources of one route session on the server.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == SESSION_ID_LEN && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
            .then(|| Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn from_u64(v: u64) -> Self {
        Self(format!("{v:016x}"))
    }
    fn loopback_token(&self) -> String {
        format!("{TAG_KEY}{}", self.0)
    }
    pub fn echo_source_name(&self) -> String {
        format!("{AEC_PREFIX}{}_aec_src", self.0)
    }
    pub fn echo_sink_name(&self) -> String {
        format!("{AEC_PREFIX}{}_aec_snk", self.0)
    }

    /// Does `argument` carry this session's tag? Loopbacks are tagged through their stream
    /// properties, echo-cancel modules through their (session-unique) node names.
    pub fn tags(&self, module: &str, argument: &str) -> bool {
        match module {
            MODULE_LOOPBACK => argument.contains(&self.loopback_token()),
            MODULE_ECHO_CANCEL => {
                argument.contains(&format!("source_name={}", self.echo_source_name()))
                    && argument.contains(&format!("sink_name={}", self.echo_sink_name()))
            }
            _ => false,
        }
    }
}

/// If `argument` of `module` carries a GeekBridge tag from *any* session, return that session.
pub(crate) fn session_in_argument(module: &str, argument: &str) -> Option<SessionId> {
    let needle = match module {
        MODULE_LOOPBACK => TAG_KEY,
        MODULE_ECHO_CANCEL => "source_name=geekbridge_",
        _ => return None,
    };
    let pos = argument.find(needle)? + needle.len();
    let id = argument.get(pos..pos + SESSION_ID_LEN)?;
    let id = SessionId::parse(id)?;
    id.tags(module, argument).then_some(id)
}

/// Which way audio flows, from the user's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDirection {
    /// Downlink: the caller's voice, phone → this computer's speakers (phone *source*).
    Rx,
    /// Uplink: the user's voice, this computer's microphone → phone (phone *sink*).
    Tx,
}

impl PathDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Rx => "rx",
            Self::Tx => "tx",
        }
    }
}

/// One `module-loopback`: record `source`, play to `sink`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoopbackSpec {
    pub direction: PathDirection,
    pub source: String,
    pub sink: String,
    pub latency_ms: u32,
}

impl LoopbackSpec {
    /// `*_dont_move=true` pins both ends so session managers cannot re-route the streams, and
    /// the stream properties carry the ownership tag.
    pub fn module_argument(&self, session: &SessionId) -> String {
        let props = format!(
            "'{} geekbridge.path={}'",
            session.loopback_token(),
            self.direction.label()
        );
        format!(
            "source={} sink={} latency_msec={} source_dont_move=true sink_dont_move=true \
             sink_input_properties={props} source_output_properties={props}",
            self.source, self.sink, self.latency_ms
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EchoMethod {
    /// `aec_method=webrtc`.
    WebRtc,
    /// Do not pass `aec_method`; use the server's configured default.
    ServerDefault,
}

/// One `module-echo-cancel`: it creates a virtual source (cleaned microphone) and sink (the
/// playback reference) layered over the user's chosen local devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EchoSpec {
    pub method: EchoMethod,
    pub source_name: String,
    pub sink_name: String,
    pub source_master: String,
    pub sink_master: String,
}

impl EchoSpec {
    pub fn module_argument(&self) -> String {
        let method = match self.method {
            EchoMethod::WebRtc => " aec_method=webrtc",
            EchoMethod::ServerDefault => "",
        };
        format!(
            "source_name={} sink_name={} source_master={} sink_master={}{method}",
            self.source_name, self.sink_name, self.source_master, self.sink_master
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId::parse("0123456789abcdef").unwrap()
    }

    #[test]
    fn names_with_quoting_hazards_are_unsafe() {
        for bad in ["", "a b", "a'b", "a\"b", "a\\b", "a=b", "a\nb", "päivä"] {
            assert!(!is_safe_name(bad), "{bad:?}");
        }
        assert!(is_safe_name("alsa_output.pci-0000_00_1f.3.analog-stereo"));
        assert!(is_safe_name("bluez_input.AA_BB_CC_00_11_22.1"));
    }

    #[test]
    fn loopback_argument_pins_both_ends_and_tags_the_streams() {
        let arg = LoopbackSpec {
            direction: PathDirection::Rx,
            source: "bluez_input.X.1".into(),
            sink: "alsa_out".into(),
            latency_ms: 50,
        }
        .module_argument(&sid());
        assert!(arg.starts_with("source=bluez_input.X.1 sink=alsa_out latency_msec=50 "));
        assert!(arg.contains("source_dont_move=true") && arg.contains("sink_dont_move=true"));
        assert!(arg.contains(
            "sink_input_properties='geekbridge.session=0123456789abcdef geekbridge.path=rx'"
        ));
        assert!(sid().tags(MODULE_LOOPBACK, &arg));
        assert!(
            !SessionId::parse("fedcba9876543210")
                .unwrap()
                .tags(MODULE_LOOPBACK, &arg)
        );
        assert_eq!(session_in_argument(MODULE_LOOPBACK, &arg), Some(sid()));
    }

    #[test]
    fn echo_argument_is_tagged_by_its_node_names() {
        let s = sid();
        let arg = EchoSpec {
            method: EchoMethod::WebRtc,
            source_name: s.echo_source_name(),
            sink_name: s.echo_sink_name(),
            source_master: "mic".into(),
            sink_master: "spk".into(),
        }
        .module_argument();
        assert_eq!(
            arg,
            "source_name=geekbridge_0123456789abcdef_aec_src sink_name=geekbridge_0123456789abcdef_aec_snk \
             source_master=mic sink_master=spk aec_method=webrtc"
        );
        assert!(s.tags(MODULE_ECHO_CANCEL, &arg));
        assert_eq!(session_in_argument(MODULE_ECHO_CANCEL, &arg), Some(s));
        // Other module names are never ours.
        assert!(!sid().tags("module-null-sink", &arg));
        assert_eq!(session_in_argument("module-null-sink", &arg), None);
    }

    #[test]
    fn a_users_own_loopback_is_not_tagged() {
        let user = "source=mic sink=spk latency_msec=20";
        assert_eq!(session_in_argument(MODULE_LOOPBACK, user), None);
        assert!(!sid().tags(MODULE_LOOPBACK, user));
    }
}
