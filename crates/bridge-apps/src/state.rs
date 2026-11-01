//! Honest view of what the discovery service reported. Discovery is never pairing.
use crate::identity::{capability_prefix, feature_label};
use bridge_core::{AppKind, Snapshot};
use std::io;

const MAX_TEXT_CHARS: usize = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub address: String,
    /// Always false today: the service only discovers.
    pub trusted: bool,
    /// The phone's announcement lists this app's capability family. Not a permission grant.
    pub advertises_feature: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryModel {
    Checking,
    ServiceUnavailable {
        reason: String,
    },
    Ready {
        discovery_error: Option<String>,
        devices: Vec<DeviceInfo>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Headline {
    pub tone: Tone,
    pub title: String,
    pub detail: String,
}

/// Strip control characters and bound length; the API is local but still untrusted input.
pub fn display_text(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control() && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).collect();
    let mut chars = clean.chars();
    let head: String = chars.by_ref().take(MAX_TEXT_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

impl DiscoveryModel {
    pub fn service_unavailable(reason: String) -> Self {
        Self::ServiceUnavailable {
            reason: display_text(&reason),
        }
    }

    pub fn from_snapshot(app: AppKind, snapshot: &Snapshot) -> Self {
        let prefix = capability_prefix(app);
        let devices = snapshot
            .devices
            .iter()
            .map(|d| DeviceInfo {
                id: display_text(&d.id),
                name: display_text(&d.name),
                kind: display_text(&d.device_type),
                address: d.address.to_string(),
                trusted: false,
                advertises_feature: d
                    .incoming_capabilities
                    .iter()
                    .chain(&d.outgoing_capabilities)
                    .any(|c| prefix.is_some_and(|prefix| c.starts_with(prefix))),
            })
            .collect();
        Self::Ready {
            discovery_error: snapshot.discovery_error.as_deref().map(display_text),
            devices,
        }
    }

    pub fn from_fetch(app: AppKind, result: io::Result<Snapshot>) -> Self {
        match result {
            Ok(snapshot) => Self::from_snapshot(app, &snapshot),
            Err(err) => Self::service_unavailable(match err.kind() {
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::TimedOut
                | io::ErrorKind::WouldBlock => err.to_string(),
                _ => format!("unexpected reply: {err}"),
            }),
        }
    }

    pub fn devices(&self) -> &[DeviceInfo] {
        match self {
            Self::Ready { devices, .. } => devices,
            _ => &[],
        }
    }

    pub fn service_reachable(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    pub fn headline(&self) -> Headline {
        let h = |tone, title: &str, detail: String| Headline {
            tone,
            title: title.into(),
            detail,
        };
        match self {
            Self::Checking => h(
                Tone::Neutral,
                "Checking for the GeekBridge service…",
                "Looking for the GeekBridge helper on this computer.".into(),
            ),
            Self::ServiceUnavailable { reason } => h(
                Tone::Error,
                "GeekBridge service isn't running",
                format!(
                    "Start geekbridge-service, or pass --api-port if it uses another port. ({reason})"
                ),
            ),
            Self::Ready {
                discovery_error: Some(error),
                ..
            } => h(
                Tone::Warning,
                "Phone discovery isn't working",
                error.clone(),
            ),
            Self::Ready { devices, .. } if devices.is_empty() => h(
                Tone::Neutral,
                "No phone found yet",
                "Open the phone app and join the same network as this computer.".into(),
            ),
            Self::Ready { devices, .. } if devices.iter().any(|d| d.trusted) => h(
                Tone::Info,
                "Paired phone reported",
                "Phone features are not available in this build yet.".into(),
            ),
            Self::Ready { devices, .. } => {
                let n = devices.len();
                h(
                    Tone::Info,
                    if n == 1 {
                        "Phone found, not paired"
                    } else {
                        "Phones found, none paired"
                    },
                    "GeekBridge can see that it's nearby, nothing more. Pairing isn't available yet."
                        .into(),
                )
            }
        }
    }

    /// Compact status for the connection strip. The strip never says "connected".
    pub fn strip(&self) -> Strip {
        let s = |tone, label: &str, hollow| Strip {
            tone,
            label: label.into(),
            hollow,
        };
        match self {
            Self::Checking => s(Tone::Neutral, "Looking for your phone…", true),
            Self::ServiceUnavailable { .. } => s(Tone::Error, "Helper not running", false),
            Self::Ready {
                discovery_error: Some(_),
                ..
            } => s(Tone::Warning, "Can't search for phones", false),
            Self::Ready { devices, .. } if devices.is_empty() => {
                s(Tone::Neutral, "No phone found", true)
            }
            Self::Ready { devices, .. } if devices.len() == 1 => {
                s(Tone::Info, "Phone nearby · not paired", false)
            }
            Self::Ready { devices, .. } => Strip {
                tone: Tone::Info,
                label: format!("{} phones nearby · none paired", devices.len()),
                hollow: false,
            },
        }
    }

    /// What the person can do next, in plain words.
    pub fn guidance(&self, api_port: u16) -> String {
        match self {
            Self::Checking => "Checking this computer for the GeekBridge helper.".into(),
            Self::ServiceUnavailable { .. } => format!(
                "The GeekBridge helper (geekbridge-service) isn't running, so no phone can be found. \
                 Start it, or open this app with --api-port if it uses a port other than {api_port}."
            ),
            Self::Ready {
                discovery_error: Some(_),
                ..
            } => "The helper is running but can't listen for phones. Another program may be \
                  using the same network port."
                .into(),
            Self::Ready { devices, .. } if devices.is_empty() => {
                "Open the phone app and join the same Wi-Fi network as this computer.".into()
            }
            Self::Ready { .. } => "Your phone can be seen on the network, but it isn't paired. \
                  Pairing isn't available in this version yet, so nothing is read from or sent to it."
                .into(),
        }
    }

    /// Lines for the optional "Technical details" section.
    pub fn technical_lines(&self, app: AppKind, api_port: u16) -> Vec<String> {
        let mut lines = vec![format!("Helper address: 127.0.0.1:{api_port}")];
        match self {
            Self::Checking => lines.push("Helper: waiting for a reply".into()),
            Self::ServiceUnavailable { reason } => {
                lines.push(format!("Helper: not reachable ({reason})"))
            }
            Self::Ready {
                discovery_error,
                devices,
            } => {
                lines.push("Helper: reachable".into());
                if let Some(error) = discovery_error {
                    lines.push(format!("Discovery error: {error}"));
                }
                let feature = feature_label(app);
                for d in devices {
                    let mut line = format!("{} · {} · {} · id {}", d.name, d.kind, d.address, d.id);
                    if let Some(feature) = feature {
                        line.push_str(&format!(
                            " · {feature}: {}",
                            if d.advertises_feature {
                                "announced"
                            } else {
                                "not announced"
                            }
                        ));
                    }
                    lines.push(line);
                }
            }
        }
        lines
    }

    /// Why a phone action can't run, as a short fragment ("no phone found"), or None if the
    /// action may run.
    pub fn action_blocker(&self) -> Option<String> {
        // A discovery result can never authorize a phone operation.
        // Replace this only with per-device, per-operation backend authorization.
        Some(match self {
            Self::Checking => "still looking for a phone".into(),
            Self::ServiceUnavailable { .. } => "the GeekBridge helper isn't running".into(),
            Self::Ready {
                discovery_error: Some(_),
                ..
            } => "phone search isn't working".into(),
            Self::Ready { devices, .. } if devices.is_empty() => "no phone found".into(),
            Self::Ready { .. } => "pairing isn't available yet".into(),
        })
    }

    /// Visible one-line explanation next to a blocked control, e.g. "Can't call yet: no phone found."
    pub fn blocked_line(&self, action: &str) -> Option<String> {
        self.action_blocker()
            .map(|reason| format!("Can't {action} yet: {reason}."))
    }
}

/// Compact connection summary: one dot and a few words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Strip {
    pub tone: Tone,
    pub label: String,
    /// Hollow dot: nothing found/known yet. Filled: something to look at.
    pub hollow: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::{ConnectionState, Device};

    fn device(caps: &[&str]) -> Device {
        Device {
            id: "0123456789abcdef0123456789abcdef".into(),
            name: "Pixel\n7".into(),
            device_type: "phone".into(),
            address: "192.168.1.8".parse().unwrap(),
            tcp_port: 1716,
            protocol_version: 8,
            incoming_capabilities: caps.iter().map(|s| s.to_string()).collect(),
            outgoing_capabilities: vec![],
            state: ConnectionState::Discovered,
            last_seen_unix_ms: 1,
        }
    }

    #[test]
    fn discovered_is_not_paired_and_blocks_actions() {
        let mut snap = Snapshot::empty(1716);
        snap.devices.push(device(&["kdeconnect.sms.request"]));
        let model = DiscoveryModel::from_snapshot(AppKind::Messages, &snap);
        assert_eq!(model.devices()[0].name, "Pixel7");
        assert!(!model.devices()[0].trusted && model.devices()[0].advertises_feature);
        assert_eq!(model.headline().title, "Phone found, not paired");
        assert!(model.action_blocker().unwrap().contains("pairing"));
        assert!(
            !DiscoveryModel::from_snapshot(AppKind::Calls, &snap).devices()[0].advertises_feature
        );
    }

    #[test]
    fn udp_conflict_is_surfaced() {
        let mut snap = Snapshot::empty(1716);
        snap.discovery_error = Some("Cannot listen on UDP 1716: in use".into());
        let h = DiscoveryModel::from_snapshot(AppKind::Agenda, &snap).headline();
        assert_eq!(h.tone, Tone::Warning);
        assert!(h.detail.contains("UDP 1716"));
    }

    #[test]
    fn unreachable_service_and_empty_states() {
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        let model = DiscoveryModel::from_fetch(AppKind::Calls, Err(refused));
        assert_eq!(model.headline().tone, Tone::Error);
        assert!(!model.service_reachable());
        let empty = DiscoveryModel::from_snapshot(AppKind::Calls, &Snapshot::empty(1716));
        assert_eq!(empty.headline().title, "No phone found yet");
        assert!(empty.action_blocker().is_some());
    }

    #[test]
    fn strip_and_blocked_lines_stay_honest() {
        let mut snap = Snapshot::empty(1716);
        let model = DiscoveryModel::from_snapshot(AppKind::Calls, &snap);
        assert_eq!(model.strip().label, "No phone found");
        assert_eq!(
            model.blocked_line("call").as_deref(),
            Some("Can't call yet: no phone found.")
        );
        snap.devices.push(device(&[]));
        let model = DiscoveryModel::from_snapshot(AppKind::Calls, &snap);
        assert_eq!(model.strip().label, "Phone nearby · not paired");
        assert!(model.blocked_line("send").unwrap().contains("pairing"));
        // No state ever reports a connected phone or lets an action through.
        for m in [
            DiscoveryModel::Checking,
            DiscoveryModel::service_unavailable("x".into()),
            model,
        ] {
            assert!(m.blocked_line("send").is_some());
            assert!(!m.strip().label.to_lowercase().contains("connected"));
        }
    }

    #[test]
    fn product_text_avoids_internals_but_details_keep_them() {
        let mut snap = Snapshot::empty(1716);
        snap.devices.push(device(&["kdeconnect.sms.request"]));
        let model = DiscoveryModel::from_snapshot(AppKind::Messages, &snap);
        for text in [
            model.strip().label,
            model.guidance(17380),
            model.headline().detail,
        ] {
            assert!(!text.contains("KDE") && !text.contains("schema") && !text.contains("JNI"));
        }
        let lines = model.technical_lines(AppKind::Messages, 17380).join("\n");
        assert!(lines.contains("127.0.0.1:17380") && lines.contains("messaging: announced"));
    }

    #[test]
    fn display_text_is_bounded() {
        assert_eq!(display_text("a\u{7}b"), "ab");
        assert_eq!(display_text("a\u{202e}b\u{2066}c"), "abc");
        assert_eq!(
            display_text(&"x".repeat(1000)).chars().count(),
            MAX_TEXT_CHARS + 1
        );
    }
}
