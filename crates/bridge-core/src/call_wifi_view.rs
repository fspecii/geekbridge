// SPDX-License-Identifier: GPL-3.0-or-later
//! What the control API says about "Call audio over Wi-Fi (ADB helper)" (`Request::CallWifi`).
//!
//! EXPERIMENTAL and off by default; Bluetooth hands-free stays the primary call-audio path. The view carries no call
//! content and no audio: only whether each paired phone is ready, what its self-test found, and the numbers of the
//! live session (`docs/design/call-audio-over-wifi.md`).
use serde::{Deserialize, Serialize};

/// Where the long explanation lives.
pub const DOC: &str = "docs/design/call-audio-over-wifi.md";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum CallWifiAction {
    /// Read the state.
    Status,
    /// Press "Listen here (Wi-Fi, experimental)" for the phone's current call: needs this computer's consent for the
    /// phone, the phone's switch and grant, a ready helper and a live call. Answers once the request is sent.
    Listen { device_id: String },
    /// Stop listening (and speaking) at once.
    Stop,
    /// Turn "Speak (experimental)" on or off for the live session.
    Speak { on: bool },
    /// The echo guard while speaking: `auto` (on unless the output is a headset or the microphone already cancels
    /// echo), `on` (always), `off` (never). Kept until the service restarts.
    Echo { mode: EchoMode },
}

/// Without echo cancellation, the far party's voice from the speakers goes back into the microphone and they hear
/// themselves. The guard closes the microphone while the far party is heard (half duplex, like a speakerphone).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EchoMode {
    #[default]
    Auto,
    On,
    Off,
}

impl EchoMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

/// One paired phone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallWifiPhone {
    pub device_id: String,
    pub name: String,
    pub connected: bool,
    /// This computer's consent for this phone (`Feature::CallAudioWifi`), off by default.
    pub allowed: bool,
    /// The phone says its helper is running and its self-test has a verdict.
    pub ready: bool,
    /// The phone may be listened to: the self-test found the far party's audio.
    pub listen: bool,
    /// Speaking back is available: the phone's own opt-in is on and the self-test found an uplink route.
    pub speak: bool,
    /// `not_run`, `works`, `silent`, `refused` or `unsure` (the phone's last self-test).
    pub test: String,
    /// A call is live on this phone (from the call state link).
    pub in_call: bool,
}

/// The live listening session, if any. Numbers only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallWifiSession {
    pub device_id: String,
    pub name: String,
    /// `starting` (request sent, waiting for the phone), `live`, or `stopping`.
    pub state: String,
    pub speaking: bool,
    pub frames: u64,
    pub lost: u64,
    pub depth_ms: u32,
    pub seconds: u64,
    /// Far party: frames that came too late to play, gaps filled with concealment, and the arrival jitter
    /// (RFC 3550 estimate, ms). Together with `depth_ms` this is the delay added on this computer.
    #[serde(default)]
    pub late: u64,
    #[serde(default)]
    pub concealed: u64,
    #[serde(default)]
    pub jitter_ms: u32,
    /// Speaking: microphone frames sent, and the phone's last receiver report for them (got, missing, dropped
    /// because the phone's helper did not take them in time, queued on the phone in ms).
    #[serde(default)]
    pub up_sent: u64,
    #[serde(default)]
    pub up_got: u64,
    #[serde(default)]
    pub up_lost: u64,
    #[serde(default)]
    pub up_dropped: u64,
    #[serde(default)]
    pub up_depth_ms: u32,
    /// The echo guard is closing the microphone while the far party is heard, and the share of sent frames it
    /// silenced (percent).
    #[serde(default)]
    pub echo_guard: bool,
    #[serde(default)]
    pub echo_held_pct: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallWifiView {
    /// This computer can play the audio (Linux with PipeWire's `pw-cat`); otherwise `problem` says why not.
    pub supported: bool,
    pub problem: Option<String>,
    pub phones: Vec<CallWifiPhone>,
    pub session: Option<CallWifiSession>,
    /// The headphones and microphone a session would use (the Bluetooth path's choice), by description.
    pub output: Option<String>,
    pub input: Option<String>,
    /// The echo guard setting (`auto`, `on`, `off`) and, for `auto`, what it decided and why.
    #[serde(default)]
    pub echo: EchoMode,
    #[serde(default)]
    pub echo_note: Option<String>,
}

impl CallWifiView {
    /// The one-line state for `geekbridge-ctl` and `doctor`.
    pub fn summary(&self) -> String {
        if !self.supported {
            return format!(
                "not available on this computer: {}",
                self.problem.as_deref().unwrap_or("unsupported system")
            );
        }
        if let Some(s) = &self.session {
            return format!(
                "{} with {}{}",
                s.state,
                s.name,
                if s.speaking { " (speaking)" } else { "" }
            );
        }
        let allowed = self.phones.iter().filter(|p| p.allowed).count();
        let ready = self.phones.iter().filter(|p| p.allowed && p.ready).count();
        format!(
            "off for {} of {} phones; {} ready",
            self.phones.len() - allowed,
            self.phones.len(),
            ready
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_plainly() {
        let mut v = CallWifiView {
            supported: true,
            ..Default::default()
        };
        assert_eq!(v.summary(), "off for 0 of 0 phones; 0 ready");
        v.phones.push(CallWifiPhone {
            device_id: "a".into(),
            allowed: true,
            ready: true,
            ..Default::default()
        });
        assert_eq!(v.summary(), "off for 0 of 1 phones; 1 ready");
        v.session = Some(CallWifiSession {
            name: "Fold".into(),
            state: "live".into(),
            speaking: true,
            ..Default::default()
        });
        assert_eq!(v.summary(), "live with Fold (speaking)");
        v.supported = false;
        v.problem = Some("no pw-cat".into());
        assert!(v.summary().contains("no pw-cat"));
    }

    #[test]
    fn actions_round_trip() {
        for a in [
            CallWifiAction::Status,
            CallWifiAction::Listen {
                device_id: "d".into(),
            },
            CallWifiAction::Stop,
            CallWifiAction::Speak { on: true },
            CallWifiAction::Echo {
                mode: EchoMode::Off,
            },
        ] {
            let j = serde_json::to_string(&a).unwrap();
            assert_eq!(serde_json::from_str::<CallWifiAction>(&j).unwrap(), a);
        }
    }
}
