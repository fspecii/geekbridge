//! What the desktop shows for calls: one honest, UI-ready view built from every call source.
//!
//! A phone can be seen by several sources at once: a Bluetooth HFP backend (this computer as the
//! hands-free unit; carries call audio) and the GeekBridge link ([`crate::calls_link`]; signalling
//! only). This module merges them into rows and decides, per row, which controls exist. A control
//! is offered only when the same pre-flight used for execution ([`CallState::check_command`])
//! accepts it, so the UI cannot promise something no backend can do. When both sources can answer,
//! HFP wins because that is the one that brings the audio to this computer.
//!
//! Pure: no I/O, no clocks.
use crate::calls::{
    AudioCodec, AudioLinkState, CallBackend, CallCapabilities, CallCommand, CallDirection,
    CallFeature, CallId, CallPhase, CallState, Sensitive, Support, UnsupportedReason,
};
use serde::{Deserialize, Serialize};

/// One backend's view of one phone.
#[derive(Clone, Debug)]
pub struct SourceSnapshot {
    /// Opaque, stable id of the source (`hfp:...`, `link:...`); echoed back in [`ActionTarget`].
    pub key: String,
    /// Human name of the phone.
    pub label: String,
    pub caps: CallCapabilities,
    pub state: CallState,
}
impl SourceSnapshot {
    fn is_hfp(&self) -> bool {
        matches!(self.caps.backend, CallBackend::BluetoothHfp { .. })
    }
}

/// How a control reaches the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlPath {
    /// Bluetooth hands-free: the audio comes to this computer.
    Hfp,
    /// GeekBridge link: signalling only, the audio stays where the phone has it.
    Link,
}

/// A concrete place to send a command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionTarget {
    pub source: String,
    pub call: CallId,
    pub path: ControlPath,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRow {
    /// Stable within one call, for UI state.
    pub key: String,
    pub phone: String,
    pub direction: CallDirection,
    pub phase: CallPhase,
    pub number: Option<Sensitive>,
    pub name: Option<Sensitive>,
    pub answer: Option<ActionTarget>,
    pub reject: Option<ActionTarget>,
    pub hangup: Option<ActionTarget>,
    /// Why a control this state would normally have is missing. Plain words.
    pub missing_control: Option<String>,
}
impl std::fmt::Debug for CallRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallRow")
            .field("key", &self.key)
            .field("phase", &self.phase)
            .field("answer", &self.answer.is_some())
            .field("reject", &self.reject.is_some())
            .field("hangup", &self.hangup.is_some())
            .finish_non_exhaustive()
    }
}
impl CallRow {
    pub fn ringing(&self) -> bool {
        self.phase.is_ringing()
    }
    /// The caller as text for a window title or notification: name, else number, else "Unknown".
    pub fn caller_label(&self) -> String {
        match (&self.name, &self.number) {
            (Some(n), _) => n.expose().to_owned(),
            (None, Some(n)) => n.expose().to_owned(),
            _ => "Unknown caller".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum AudioRoute {
    /// No call is live, so there is nothing to route.
    NoCall,
    /// The call audio is on this computer.
    Computer {
        codec: AudioCodec,
    },
    Connecting,
    /// The call is live but its audio is on the phone.
    Phone,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioRouteView {
    pub route: AudioRoute,
    /// Shown next to the route when it is not obvious, e.g. "Answered on the phone".
    pub hint: Option<String>,
    /// Source whose audio link can be asked to open (`CallCommand::ConnectAudio`), offered only
    /// while the call's audio is on the phone and that source declares audio-link control.
    #[serde(default)]
    pub connect: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteView {
    pub source: String,
    pub muted: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallsView {
    /// Ringing calls first.
    pub rows: Vec<CallRow>,
    pub audio: AudioRouteView,
    /// Present only when microphone mute can really be done for the live call.
    pub mute: Option<MuteView>,
    /// Number of sources that reported at least a link; zero means "no phone available for calls".
    pub sources_up: usize,
}
impl CallsView {
    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            audio: AudioRouteView {
                route: AudioRoute::NoCall,
                hint: None,
                connect: None,
            },
            mute: None,
            sources_up: 0,
        }
    }
    pub fn ringing(&self) -> Option<&CallRow> {
        self.rows.iter().find(|r| r.ringing())
    }
    pub fn live(&self) -> Option<&CallRow> {
        self.rows.iter().find(|r| !r.ringing())
    }
}

struct Part<'a> {
    source: &'a SourceSnapshot,
    call: CallId,
}

/// Merge every source into one view. See the module docs for the rules.
pub fn build(sources: &[SourceSnapshot]) -> CallsView {
    let sources_up = sources
        .iter()
        .filter(|s| s.state.link == crate::calls::LinkState::Up)
        .count();
    // HFP candidates first so they become the primary part of a row.
    let mut rows: Vec<Vec<Part>> = Vec::new();
    for hfp in [true, false] {
        for s in sources.iter().filter(|s| s.is_hfp() == hfp) {
            for (id, info) in &s.state.calls {
                let part = Part {
                    source: s,
                    call: id.clone(),
                };
                // A non-HFP observation of the same phone joins the HFP row in the same class
                // (ringing or live) instead of showing the call twice.
                let slot = (!hfp)
                    .then(|| {
                        rows.iter_mut().find(|r| {
                            r.iter().all(|p| p.source.is_hfp())
                                && r[0].source.state.calls[&r[0].call].phase.is_ringing()
                                    == info.phase.is_ringing()
                        })
                    })
                    .flatten();
                match slot {
                    Some(row) => row.push(part),
                    None => rows.push(vec![part]),
                }
            }
        }
    }
    let mut out: Vec<CallRow> = rows.iter().map(|parts| row_for(parts, sources)).collect();
    out.sort_by_key(|r| !r.ringing());

    let (audio, mute) = audio_and_mute(&rows);
    CallsView {
        rows: out,
        audio,
        mute,
        sources_up,
    }
}

fn row_for(parts: &[Part], sources: &[SourceSnapshot]) -> CallRow {
    let primary = &parts[0];
    let info = &primary.source.state.calls[&primary.call];
    let mut number = None;
    let mut name = None;
    for p in parts {
        let party = &p.source.state.calls[&p.call].party;
        number = number.or_else(|| party.number.clone());
        // A name equal to the number is a fallback, not a contact.
        if name.is_none() {
            name = party
                .name
                .clone()
                .filter(|n| party.number.as_ref() != Some(n));
        }
    }
    let direction = parts
        .iter()
        .map(|p| p.source.state.calls[&p.call].direction)
        .find(|d| *d != CallDirection::Unknown)
        .unwrap_or(CallDirection::Unknown);
    let ringing = info.phase.is_ringing();
    let target = |make: &dyn Fn(CallId) -> CallCommand| -> Option<ActionTarget> {
        parts.iter().find_map(|p| {
            let cmd = make(p.call.clone());
            p.source
                .state
                .check_command(&p.source.caps, &cmd)
                .ok()
                .map(|()| ActionTarget {
                    source: p.source.key.clone(),
                    call: p.call.clone(),
                    path: if p.source.is_hfp() {
                        ControlPath::Hfp
                    } else {
                        ControlPath::Link
                    },
                })
        })
    };
    let answer = ringing
        .then(|| target(&|call| CallCommand::Answer { call }))
        .flatten();
    let reject = ringing
        .then(|| target(&|call| CallCommand::Reject { call }))
        .flatten();
    let hangup = (!ringing)
        .then(|| target(&|call| CallCommand::Hangup { call }))
        .flatten();
    let missing_control = if ringing && answer.is_none() {
        Some(why_cannot_answer(sources))
    } else if !ringing && hangup.is_none() {
        Some("This computer can't end the call. Use the phone.".to_owned())
    } else {
        None
    };
    CallRow {
        key: format!("{}|{}", primary.source.key, primary.call.as_str()),
        phone: primary.source.label.clone(),
        direction,
        phase: info.phase,
        number,
        name,
        answer,
        reject,
        hangup,
        missing_control,
    }
}

fn why_cannot_answer(sources: &[SourceSnapshot]) -> String {
    let link_declined = sources.iter().any(|s| {
        s.caps.backend == CallBackend::GeekBridgeLink
            && s.caps.support(CallFeature::Answer)
                == Support::Unsupported {
                    reason: UnsupportedReason::RemoteDoesNotAdvertise,
                }
    });
    if link_declined {
        "Answer on the phone. To answer from here, allow GeekBridge to answer calls in the phone app's setup."
            .to_owned()
    } else if sources.iter().any(|s| s.is_hfp()) {
        "Answer on the phone. Bluetooth calling on this computer can't answer this call.".to_owned()
    } else {
        "Answer on the phone. Nothing connected to this computer can answer calls yet.".to_owned()
    }
}

fn audio_and_mute(rows: &[Vec<Part>]) -> (AudioRouteView, Option<MuteView>) {
    let live = rows.iter().find(|parts| {
        !parts[0].source.state.calls[&parts[0].call]
            .phase
            .is_ringing()
    });
    let Some(parts) = live else {
        return (
            AudioRouteView {
                route: AudioRoute::NoCall,
                hint: None,
                connect: None,
            },
            None,
        );
    };
    let hfp = parts.iter().find(|p| p.source.is_hfp());
    let (route, hint) = match hfp.map(|p| p.source.state.audio) {
        Some(AudioLinkState::Connected { codec }) => (AudioRoute::Computer { codec }, None),
        Some(AudioLinkState::Connecting) => (AudioRoute::Connecting, None),
        Some(AudioLinkState::Disconnected) => (
            AudioRoute::Phone,
            Some("The call audio is still on the phone.".to_owned()),
        ),
        None => (
            AudioRoute::Phone,
            Some(
                "Answered on the phone. Call audio stays there until this computer is paired \
                 with the phone as its Bluetooth hands-free device."
                    .to_owned(),
            ),
        ),
    };
    let mute = hfp.and_then(|p| {
        matches!(route, AudioRoute::Computer { .. })
            .then(|| {
                p.source
                    .state
                    .check_command(
                        &p.source.caps,
                        &CallCommand::SetMicrophoneMute { muted: true },
                    )
                    .ok()
                    .map(|()| MuteView {
                        source: p.source.key.clone(),
                        muted: p.source.state.microphone_muted,
                    })
            })
            .flatten()
    });
    let connect = hfp
        .filter(|_| route == AudioRoute::Phone)
        .filter(|p| {
            p.source
                .state
                .check_command(&p.source.caps, &CallCommand::ConnectAudio)
                .is_ok()
        })
        .map(|p| p.source.key.clone());
    (
        AudioRouteView {
            route,
            hint,
            connect,
        },
        mute,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::{
        CallEvent, CallParty, EndReason, HfpEvidence, HfpHost, LinkState, MuteScope,
    };
    use crate::calls_link::{LinkCallReport, PhoneState};

    fn hfp(ringing: bool, audio: bool) -> SourceSnapshot {
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::LinuxPipeWire,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                caller_id_notification: true,
                host_call_control_api: true,
                audio_codec: audio.then_some(AudioCodec::Msbc),
                ..Default::default()
            },
        );
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        state.apply(CallEvent::CallUpdated {
            id: CallId::new("c1"),
            direction: CallDirection::Incoming,
            phase: if ringing {
                CallPhase::Incoming
            } else {
                CallPhase::Active
            },
            party: CallParty {
                number: Some(Sensitive::new("+15550100")),
                name: None,
            },
        });
        if audio {
            state.apply(CallEvent::Audio {
                state: AudioLinkState::Connected {
                    codec: AudioCodec::Msbc,
                },
            });
        }
        SourceSnapshot {
            key: "hfp:ag1".into(),
            label: "Pixel".into(),
            caps,
            state,
        }
    }

    fn link(phone: PhoneState, can_answer: bool, name: Option<&str>) -> SourceSnapshot {
        let report = LinkCallReport {
            state: phone,
            number: Some(Sensitive::new("+15550100")),
            name: name.map(Sensitive::new),
            can_answer,
            can_end: can_answer,
        };
        let mut state = CallState::default();
        report.apply(&mut state);
        SourceSnapshot {
            key: "link:dev".into(),
            label: "Pixel".into(),
            caps: report.capabilities(),
            state,
        }
    }

    #[test]
    fn nothing_connected_offers_nothing() {
        let v = build(&[]);
        assert_eq!(v, CallsView::empty());
        assert!(v.ringing().is_none());
    }

    #[test]
    fn link_only_ringing_offers_answer_and_says_audio_stays_on_phone() {
        let v = build(&[link(PhoneState::Ringing, true, Some("Ada"))]);
        let row = v.ringing().unwrap();
        assert_eq!(row.answer.as_ref().unwrap().path, ControlPath::Link);
        assert!(row.reject.is_some());
        assert!(row.hangup.is_none());
        assert_eq!(row.caller_label(), "Ada");

        let live = build(&[link(PhoneState::OffHook, true, None)]);
        assert_eq!(live.audio.route, AudioRoute::Phone);
        assert!(
            live.audio
                .hint
                .as_ref()
                .unwrap()
                .contains("Answered on the phone")
        );
        assert!(live.mute.is_none());
        assert!(live.live().unwrap().hangup.is_some());
    }

    #[test]
    fn answer_is_not_offered_without_the_phones_permission() {
        let v = build(&[link(PhoneState::Ringing, false, None)]);
        let row = v.ringing().unwrap();
        assert!(row.answer.is_none() && row.reject.is_none());
        assert!(
            row.missing_control
                .as_ref()
                .unwrap()
                .contains("allow GeekBridge")
        );
    }

    #[test]
    fn hfp_wins_when_both_can_answer_and_the_name_comes_from_the_phone() {
        let v = build(&[
            link(PhoneState::Ringing, true, Some("Ada")),
            hfp(true, false),
        ]);
        assert_eq!(v.rows.len(), 1);
        let row = &v.rows[0];
        assert_eq!(row.answer.as_ref().unwrap().path, ControlPath::Hfp);
        assert_eq!(row.name.as_ref().unwrap().expose(), "Ada");
        assert_eq!(row.number.as_ref().unwrap().expose(), "+15550100");
    }

    #[test]
    fn link_is_the_fallback_when_hfp_cannot_control() {
        let mut h = hfp(true, false);
        h.caps = CallCapabilities::unprobed(CallBackend::BluetoothHfp {
            host: HfpHost::LinuxPipeWire,
        });
        let v = build(&[h, link(PhoneState::Ringing, true, None)]);
        assert_eq!(v.rows.len(), 1);
        assert_eq!(v.rows[0].answer.as_ref().unwrap().path, ControlPath::Link);
    }

    #[test]
    fn live_call_with_hfp_audio_reports_computer_route_and_mute_only_if_supported() {
        let mut h = hfp(false, true);
        let v = build(std::slice::from_ref(&h));
        assert_eq!(
            v.audio.route,
            AudioRoute::Computer {
                codec: AudioCodec::Msbc
            }
        );
        assert!(v.audio.hint.is_none());
        // HFP evidence declares microphone mute when audio is up.
        assert_eq!(
            v.mute,
            Some(MuteView {
                source: "hfp:ag1".into(),
                muted: None
            })
        );
        h.caps.set(
            CallFeature::MuteMicrophone,
            Support::Unsupported {
                reason: UnsupportedReason::NoPlatformApi,
            },
        );
        assert!(build(&[h]).mute.is_none());
    }

    #[test]
    fn muted_state_is_reported_and_audio_off_means_phone() {
        let mut h = hfp(false, true);
        h.state.apply(CallEvent::MuteChanged {
            scope: MuteScope::HostMicrophone,
            muted: true,
        });
        assert_eq!(build(&[h.clone()]).mute.unwrap().muted, Some(true));
        h.state.apply(CallEvent::Audio {
            state: AudioLinkState::Disconnected,
        });
        let v = build(&[h.clone()]);
        assert_eq!(v.audio.route, AudioRoute::Phone);
        assert!(v.mute.is_none());
        // Not declared by this source, so no "use computer audio" is offered.
        assert_eq!(v.audio.connect, None);
        h.caps.set(CallFeature::AudioLinkControl, Support::Declared);
        assert_eq!(build(&[h]).audio.connect.as_deref(), Some("hfp:ag1"));
    }

    #[test]
    fn ended_calls_leave_no_rows() {
        let mut l = link(PhoneState::Ringing, true, None);
        l.state.apply(CallEvent::CallEnded {
            id: CallId::new(crate::calls_link::LINK_CALL_ID),
            reason: EndReason::Unspecified,
        });
        assert!(build(&[l]).rows.is_empty());
    }

    #[test]
    fn view_survives_json_and_never_prints_numbers_in_debug() {
        let v = build(&[link(PhoneState::Ringing, true, Some("Ada"))]);
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(serde_json::from_str::<CallsView>(&json).unwrap(), v);
        assert!(!format!("{v:?}").contains("5550100"));
    }
}
