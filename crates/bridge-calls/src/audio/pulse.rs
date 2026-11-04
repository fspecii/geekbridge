// SPDX-License-Identifier: GPL-3.0-or-later
//! PulseAudio / `pipewire-pulse` adapter through `pactl` and `parec` (both from the `libpulse`
//! package, present wherever `pipewire-pulse` is).
//!
//! Why not libpulse in-process: linking it maps libpulse, libpulsecommon, libdbus, libsndfile
//! (and its codecs) and libsystemd into the service at start, +2.2 MB idle RSS for a feature that
//! runs only during a call (`docs/performance.md`). The tools are run only while a call's audio
//! is being handled, with no shell, fixed argument lists, names validated first, and a deadline
//! on every run. Reads use `pactl -f json`; writes are limited to moving a stream, switching a
//! card profile and muting a source; the level meter is a short `parec` capture.
//!
//! Exercised against a private `pipewire` + `pipewire-pulse` + `wireplumber` instance by
//! `scripts/e2e/call-audio-sandbox.sh` (see `tests/audio_sandbox.rs`).

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::audio::model::{
    CardInfo, CardProfile, Props, ServerIdentity, ServerSnapshot, SinkInfo, SourceInfo, StreamInfo,
};
use crate::audio::server::{AudioServer, PeakTarget, ServerError};

#[derive(Clone, Debug)]
pub struct PulseOptions {
    /// `None`: the default server of the user's session (`PULSE_SERVER` is honoured).
    pub server: Option<String>,
    pub client_name: String,
    /// Deadline for the first `pactl info`.
    pub connect_timeout: Duration,
    /// Deadline for every other run.
    pub op_timeout: Duration,
}

impl Default for PulseOptions {
    fn default() -> Self {
        Self {
            server: None,
            client_name: "GeekBridge".into(),
            connect_timeout: Duration::from_secs(3),
            op_timeout: Duration::from_secs(3),
        }
    }
}

pub struct PulseServer {
    opts: PulseOptions,
}

/// Wait for `child` up to `limit`, collecting stdout. Kills it on timeout.
fn finish(mut child: Child, limit: Duration, what: &'static str) -> Result<String, ServerError> {
    let mut out = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = out.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let end = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() >= end => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ServerError::Timeout { what });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(ServerError::Unavailable(format!("{what}: {e}"))),
        }
    };
    let text = reader.join().unwrap_or_default();
    if status.success() {
        Ok(text)
    } else {
        Err(ServerError::Rejected {
            what,
            detail: format!("pactl exited with {status}"),
        })
    }
}

fn props_of(v: &Value) -> Props {
    v.get("properties")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn u32_of(v: &Value, k: &str) -> Option<u32> {
    v.get(k)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

fn str_of(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// `"cb0e:172e"` (pactl's two 16-bit halves) as one number; 0 when absent.
fn cookie(s: &str) -> u32 {
    let mut it = s.split(':').map(|h| u32::from_str_radix(h, 16).ok());
    match (it.next().flatten(), it.next().flatten()) {
        (Some(hi), Some(lo)) => (hi << 16) | (lo & 0xffff),
        _ => 0,
    }
}

fn list(json: &str, what: &'static str) -> Result<Vec<Value>, ServerError> {
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Array(a)) => Ok(a),
        _ => Err(ServerError::Rejected {
            what,
            detail: "unreadable pactl output".into(),
        }),
    }
}

/// Build a snapshot from the six `pactl -f json` outputs. Pure, so it is tested on canned
/// output from a real `pipewire-pulse`.
pub(crate) fn parse_snapshot(
    info: &str,
    cards: &str,
    sinks: &str,
    sources: &str,
    sink_inputs: &str,
    source_outputs: &str,
) -> Result<ServerSnapshot, ServerError> {
    let info: Value = serde_json::from_str(info).map_err(|_| ServerError::Rejected {
        what: "server info",
        detail: "unreadable pactl output".into(),
    })?;
    let opt = |k: &str| {
        info.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != "@DEFAULT_SINK@" && *s != "@DEFAULT_SOURCE@")
            .map(str::to_owned)
    };
    let identity = ServerIdentity {
        server_name: str_of(&info, "server_name"),
        server_version: str_of(&info, "server_version"),
        host_name: str_of(&info, "host_name"),
        cookie: cookie(&str_of(&info, "cookie")),
    };
    let cards_v = list(cards, "card list")?;
    // Devices name their card by PipeWire object id (`device.id`); cards carry it as `object.id`.
    let card_by_object: Vec<(String, u32)> = cards_v
        .iter()
        .filter_map(|c| {
            let p = props_of(c);
            Some((p.get("object.id")?.clone(), u32_of(c, "index")?))
        })
        .collect();
    let card_of = |p: &Props| {
        p.get("device.id").and_then(|id| {
            card_by_object
                .iter()
                .find(|(o, _)| o == id)
                .map(|(_, i)| *i)
        })
    };
    let cards = cards_v
        .iter()
        .filter_map(|c| {
            Some(CardInfo {
                index: u32_of(c, "index")?,
                name: str_of(c, "name"),
                props: props_of(c),
                profiles: c
                    .get("profiles")
                    .and_then(Value::as_object)
                    .map(|o| {
                        o.iter()
                            .map(|(name, p)| CardProfile {
                                name: name.clone(),
                                available: p
                                    .get("available")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                active_profile: c
                    .get("active_profile")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect();
    let sinks_v = list(sinks, "sink list")?;
    let sinks: Vec<SinkInfo> = sinks_v
        .iter()
        .filter_map(|s| {
            let props = props_of(s);
            Some(SinkInfo {
                index: u32_of(s, "index")?,
                name: str_of(s, "name"),
                description: str_of(s, "description"),
                card: card_of(&props),
                mute: s.get("mute").and_then(Value::as_bool).unwrap_or(false),
                props,
            })
        })
        .collect();
    let monitor_of = |name: &str| {
        sinks_v
            .iter()
            .find(|s| s.get("monitor_source").and_then(Value::as_str) == Some(name))
            .and_then(|s| u32_of(s, "index"))
    };
    let sources = list(sources, "source list")?
        .iter()
        .filter_map(|s| {
            let props = props_of(s);
            let name = str_of(s, "name");
            let monitor = monitor_of(&name).or_else(|| {
                // A monitor whose sink is gone from the list still is not a microphone.
                (props.get("device.class").map(String::as_str) == Some("monitor"))
                    .then_some(u32::MAX)
            });
            Some(SourceInfo {
                index: u32_of(s, "index")?,
                description: str_of(s, "description"),
                card: card_of(&props),
                mute: s.get("mute").and_then(Value::as_bool).unwrap_or(false),
                monitor_of_sink: monitor,
                name,
                props,
            })
        })
        .collect();
    let stream = |v: &Value, dev: &str| {
        let props = props_of(v);
        Some(StreamInfo {
            index: u32_of(v, "index")?,
            name: props
                .get(crate::audio::model::PROP_NODE_NAME)
                .cloned()
                .unwrap_or_else(|| str_of(v, "name")),
            device: u32_of(v, dev).filter(|i| *i != u32::MAX),
            corked: v.get("corked").and_then(Value::as_bool).unwrap_or(false),
            mute: v.get("mute").and_then(Value::as_bool).unwrap_or(false),
            props,
        })
    };
    Ok(ServerSnapshot {
        identity,
        default_sink: opt("default_sink_name"),
        default_source: opt("default_source_name"),
        cards,
        sinks,
        sources,
        sink_inputs: list(sink_inputs, "playback stream list")?
            .iter()
            .filter_map(|v| stream(v, "sink"))
            .collect(),
        source_outputs: list(source_outputs, "capture stream list")?
            .iter()
            .filter_map(|v| stream(v, "source"))
            .collect(),
    })
}

impl PulseServer {
    /// Check the server answers (`pactl info`). Never starts one.
    pub fn connect(opts: PulseOptions) -> Result<Self, ServerError> {
        let s = Self { opts };
        s.pactl(&["info"], s.opts.connect_timeout, "connect")
            .map_err(|e| ServerError::Unavailable(e.to_string()))?;
        Ok(s)
    }

    fn command(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        if let Some(server) = &self.opts.server {
            c.arg(format!("--server={server}"));
        }
        c.arg(format!("--client-name={}", self.opts.client_name));
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        c
    }

    fn pactl(
        &self,
        args: &[&str],
        limit: Duration,
        what: &'static str,
    ) -> Result<String, ServerError> {
        let child = self.command("pactl").args(args).spawn().map_err(|e| {
            ServerError::Unavailable(format!("pactl cannot run ({e}); install libpulse"))
        })?;
        finish(child, limit, what)
    }

    fn json(&self, args: &[&str], what: &'static str) -> Result<String, ServerError> {
        let mut a = vec!["-f", "json"];
        a.extend_from_slice(args);
        self.pactl(&a, self.opts.op_timeout, what)
    }

    fn checked(name: &str, what: &'static str) -> Result<(), ServerError> {
        if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_control) {
            return Err(ServerError::Rejected {
                what,
                detail: "not a device name".into(),
            });
        }
        Ok(())
    }

    /// Record float32 mono from `device` (optionally only one sink input) for `window`; the
    /// highest absolute sample.
    fn measure(
        &self,
        device: &str,
        monitor_of: Option<u32>,
        window: Duration,
    ) -> Result<f32, ServerError> {
        let mut c = self.command("parec");
        c.args([
            "--raw",
            "--format=float32le",
            "--channels=1",
            "--rate=8000",
            "--latency-msec=20",
            "--property=media.role=abstract",
        ]);
        if let Some(i) = monitor_of {
            c.arg(format!("--monitor-stream={i}"));
        }
        c.arg(format!("--device={device}"));
        let mut child = c
            .spawn()
            .map_err(|e| ServerError::Unavailable(format!("parec cannot run ({e})")))?;
        let mut out = child.stdout.take().ok_or(ServerError::Rejected {
            what: "level meter",
            detail: "no output".into(),
        })?;
        let reader = std::thread::spawn(move || {
            let mut peak = 0f32;
            let mut buf = [0u8; 4096];
            let mut carry: Vec<u8> = Vec::new();
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..n]);
                let whole = carry.len() / 4 * 4;
                for c in carry[..whole].as_chunks::<4>().0 {
                    let v = f32::from_le_bytes(*c).abs();
                    if v.is_finite() && v > peak {
                        peak = v;
                    }
                }
                carry.drain(..whole);
            }
            peak
        });
        std::thread::sleep(window);
        let exited_early = matches!(child.try_wait(), Ok(Some(s)) if !s.success());
        let _ = child.kill();
        let _ = child.wait();
        let peak = reader.join().unwrap_or(0.0);
        if exited_early {
            return Err(ServerError::Rejected {
                what: "level meter",
                detail: "parec could not record".into(),
            });
        }
        Ok(peak.min(1.0))
    }
}

impl AudioServer for PulseServer {
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError> {
        let info = self.json(&["info"], "server info")?;
        let cards = self.json(&["list", "cards"], "card list")?;
        let sinks = self.json(&["list", "sinks"], "sink list")?;
        let sources = self.json(&["list", "sources"], "source list")?;
        let inputs = self.json(&["list", "sink-inputs"], "playback stream list")?;
        let outputs = self.json(&["list", "source-outputs"], "capture stream list")?;
        parse_snapshot(&info, &cards, &sinks, &sources, &inputs, &outputs)
    }

    fn move_sink_input(&mut self, index: u32, sink: &str) -> Result<(), ServerError> {
        const W: &str = "moving the call's playback stream";
        Self::checked(sink, W)?;
        self.pactl(
            &["move-sink-input", &index.to_string(), sink],
            self.opts.op_timeout,
            W,
        )
        .map(|_| ())
    }

    fn move_source_output(&mut self, index: u32, source: &str) -> Result<(), ServerError> {
        const W: &str = "moving the call's capture stream";
        Self::checked(source, W)?;
        self.pactl(
            &["move-source-output", &index.to_string(), source],
            self.opts.op_timeout,
            W,
        )
        .map(|_| ())
    }

    fn set_card_profile(&mut self, card: u32, profile: &str) -> Result<(), ServerError> {
        const W: &str = "switching the card profile";
        Self::checked(profile, W)?;
        self.pactl(
            &["set-card-profile", &card.to_string(), profile],
            self.opts.op_timeout,
            W,
        )
        .map(|_| ())
    }

    fn set_source_mute(&mut self, source: &str, mute: bool) -> Result<(), ServerError> {
        const W: &str = "muting the microphone";
        Self::checked(source, W)?;
        self.pactl(
            &["set-source-mute", source, if mute { "1" } else { "0" }],
            self.opts.op_timeout,
            W,
        )
        .map(|_| ())
    }

    fn peak(&mut self, target: &PeakTarget, window: Duration) -> Result<Option<f32>, ServerError> {
        match target {
            PeakTarget::SinkInput { index, sink } => {
                Self::checked(sink, "level meter")?;
                self.measure(&format!("{sink}.monitor"), Some(*index), window)
                    .map(Some)
            }
            PeakTarget::Source(name) => {
                Self::checked(name, "level meter")?;
                self.measure(name, None, window).map(Some)
            }
        }
    }

    fn reconnect(&mut self) -> Result<(), ServerError> {
        self.pactl(&["info"], self.opts.connect_timeout, "connect")
            .map(|_| ())
            .map_err(|e| ServerError::Unavailable(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from `pactl -f json` against pipewire-pulse 1.6.9 (the owner's machine plus a
    // phone card as the sandbox shows it).
    const INFO: &str = r#"{"server_name":"PulseAudio (on PipeWire 1.6.9)","server_version":"15.0.0","host_name":"omarchy","default_sink_name":"hx_out","default_source_name":"hx_in","cookie":"cb0e:172e"}"#;
    const CARDS: &str = r#"[{"index":9644,"name":"alsa_card.usb-HyperX","properties":{"object.id":"74"},"profiles":{"off":{"available":true}},"active_profile":"output:analog-stereo+input:mono-fallback"},
        {"index":76,"name":"bluez_card.AA_BB_CC_00_11_22","properties":{"object.id":"80","api.bluez5.address":"AA:BB:CC:00:11:22","device.api":"bluez5"},"profiles":{"off":{"available":true},"audio-gateway":{"available":true}},"active_profile":"audio-gateway"}]"#;
    const SINKS: &str = r#"[{"index":65,"name":"hx_out","description":"HyperX","mute":false,"monitor_source":"hx_out.monitor","properties":{"device.id":"74","device.form_factor":"headset"}}]"#;
    const SOURCES: &str = r#"[{"index":66,"name":"hx_out.monitor","description":"Monitor of HyperX","mute":false,"properties":{"device.id":"74","device.class":"monitor"}},
        {"index":68,"name":"hx_in","description":"HyperX Mono","mute":true,"properties":{"device.id":"74","device.class":"sound"}}]"#;
    const INPUTS: &str = r#"[{"index":201,"sink":65,"corked":false,"mute":false,"properties":{"node.name":"bluez_input.AA_BB_CC_00_11_22.0","api.bluez5.address":"AA:BB:CC:00:11:22","api.bluez5.profile":"headset-audio-gateway","api.bluez5.codec":"msbc","media.class":"Stream/Output/Audio"}}]"#;
    const OUTPUTS: &str = r#"[{"index":202,"source":68,"corked":false,"mute":false,"properties":{"node.name":"bluez_output.AA_BB_CC_00_11_22.1","api.bluez5.address":"AA:BB:CC:00:11:22","api.bluez5.profile":"headset-audio-gateway","media.class":"Stream/Input/Audio"}}]"#;

    #[test]
    fn pactl_json_becomes_the_same_snapshot_libpulse_gave() {
        let s = parse_snapshot(INFO, CARDS, SINKS, SOURCES, INPUTS, OUTPUTS).unwrap();
        assert!(s.identity.is_pipewire());
        assert_eq!(s.identity.cookie, 0xcb0e_172e);
        assert_eq!(s.default_sink.as_deref(), Some("hx_out"));
        assert_eq!(s.cards[1].active_profile.as_deref(), Some("audio-gateway"));
        assert_eq!(s.cards[1].gateway_profile().unwrap().name, "audio-gateway");
        assert_eq!(
            s.sinks[0].card,
            Some(9644),
            "device.id -> the card's object.id"
        );
        assert_eq!(s.sources[0].monitor_of_sink, Some(65));
        assert_eq!(s.sources[1].monitor_of_sink, None);
        assert!(s.sources[1].mute);
        let v = crate::audio::route::phone_voice(
            &s,
            crate::audio::BtAddress::parse("AA:BB:CC:00:11:22").unwrap(),
        );
        assert_eq!(v.rx.unwrap().device, Some(65));
        assert_eq!(v.tx.unwrap().device, Some(68));
        assert_eq!(v.card.unwrap().index, 76);
        let l = crate::audio::route::devices(&s);
        assert_eq!(l.inputs.len(), 1, "the monitor is not a microphone");
    }

    #[test]
    fn odd_output_is_an_error_not_a_panic() {
        assert!(parse_snapshot("{}", "[]", "[]", "[]", "[]", "[]").is_ok());
        assert!(parse_snapshot("nope", "[]", "[]", "[]", "[]", "[]").is_err());
        assert!(parse_snapshot("{}", "{}", "[]", "[]", "[]", "[]").is_err());
        assert_eq!(cookie("zz"), 0);
        assert!(PulseServer::checked("-x", "t").is_err());
        assert!(PulseServer::checked("a\nb", "t").is_err());
        assert!(PulseServer::checked("alsa_output.x", "t").is_ok());
    }
}
