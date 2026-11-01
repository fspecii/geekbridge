//! Translation between this adapter's Windows vocabulary and `bridge-core::calls`.
//!
//! `bridge-core` is the neutral call domain; this module feeds it ([`CoreProjector`] turns
//! successive observations of one device into [`CallEvent`]s, [`capabilities_for`] declares what
//! may be attempted) and reads commands from it ([`plan_core_command`]). Nothing here invents a
//! D-Bus-style identity: call ids are `w<epoch>.<windows call id>`, so a call id from before a
//! line removal can never match a call after it.
//!
//! Support is only ever [`Support::Declared`]. Verifying a feature requires a recorded hardware
//! exercise, which this crate has not had.
use crate::model::*;
use crate::readiness::{AudioReadiness, Readiness};
use bridge_core::calls::*;

/// Core call id for a Windows call on one device epoch.
pub fn core_call_id(epoch: u64, native: &NativeCallId) -> CallId {
    let plain = format!("w{epoch:x}.{}", native.as_str());
    if plain.len() <= MAX_FIELD_LEN {
        CallId::new(plain)
    } else {
        CallId::new(format!("w{epoch:x}.#{}", native.fingerprint()))
    }
}

pub fn core_direction(d: NativeDirection) -> CallDirection {
    match d {
        NativeDirection::Incoming => CallDirection::Incoming,
        NativeDirection::Outgoing => CallDirection::Outgoing,
        NativeDirection::Unknown => CallDirection::Unknown,
    }
}

/// Phase of one call given the others on the line. Windows has no "alerting" or "waiting" state:
/// a ringing call is [`CallPhase::Waiting`] when another call is already live, and a dialing call
/// stays [`CallPhase::Dialing`] until it talks. `None` for calls that are over.
pub fn core_phase(call: &CallRecord, all: &[CallRecord]) -> Option<CallPhase> {
    match call.status {
        NativeCallStatus::Incoming => {
            let other_live = all.iter().any(|c| {
                c.id != call.id
                    && matches!(
                        c.status,
                        NativeCallStatus::Talking
                            | NativeCallStatus::Held
                            | NativeCallStatus::Dialing
                    )
            });
            Some(if other_live {
                CallPhase::Waiting
            } else {
                CallPhase::Incoming
            })
        }
        NativeCallStatus::Dialing => Some(CallPhase::Dialing),
        NativeCallStatus::Talking => Some(CallPhase::Active),
        NativeCallStatus::Held => Some(CallPhase::Held),
        NativeCallStatus::Lost | NativeCallStatus::Ended | NativeCallStatus::Other(_) => None,
    }
}

fn bounded(s: &Option<Sensitive>) -> Option<Sensitive> {
    s.as_ref()
        .filter(|v| v.expose().len() <= MAX_FIELD_LEN)
        .cloned()
}

/// What may be attempted for one device, derived from readiness. Never `Verified`.
pub fn capabilities_for(readiness: &Readiness) -> CallCapabilities {
    let backend = CallBackend::BluetoothHfp {
        host: HfpHost::Windows,
    };
    let mut caps = CallCapabilities::unsupported(backend, UnsupportedReason::NotImplemented);
    let declared = |ok: bool| {
        if ok {
            Support::Declared
        } else {
            Support::Unknown
        }
    };
    let control = readiness.control_ready();
    let dial = readiness.dial_blocker().is_none();
    let route = control && readiness.audio == AudioReadiness::Routable;
    caps.set(CallFeature::CallStateEvents, declared(control));
    caps.set(CallFeature::CallerIdentity, declared(control));
    caps.set(CallFeature::Answer, declared(control));
    caps.set(CallFeature::Hangup, declared(control));
    caps.set(CallFeature::Dial, declared(dial));
    caps.set(CallFeature::SwapCalls, declared(control));
    caps.set(CallFeature::SendTones, declared(control));
    caps.set(
        CallFeature::MuteRinger,
        Support::Unsupported {
            reason: UnsupportedReason::NotInProtocol,
        },
    );
    caps.set(CallFeature::MuteMicrophone, declared(control));
    // Audio is carried by the operating system's HFP stack. It is declared only while Windows
    // itself reports that the call can be routed here; "Unreported" stays Unknown.
    caps.set(CallFeature::AudioDownlink, declared(route));
    caps.set(CallFeature::AudioUplink, declared(route));
    caps.set(CallFeature::AudioLinkControl, declared(control));
    caps
}

/// One device as seen by the projector.
pub struct ProjectedDevice<'a> {
    pub epoch: u64,
    pub link_up: bool,
    pub calls: &'a [CallRecord],
}

/// Folds observations of one device into core events. Owns the [`CallState`] it feeds, so the
/// events it returns are exactly those that keep a consumer's state equal to it.
#[derive(Debug, Default)]
pub struct CoreProjector {
    state: CallState,
    /// Events that the reducer refused, for diagnostics. Bounded counter, not a log.
    pub rejected: u32,
}

impl CoreProjector {
    pub fn state(&self) -> &CallState {
        &self.state
    }

    fn push(&mut self, out: &mut Vec<CallEvent>, event: CallEvent) {
        match self.state.apply(event.clone()) {
            Applied::Changed => out.push(event),
            Applied::Unchanged => {}
            Applied::Rejected(_) => self.rejected = self.rejected.saturating_add(1),
        }
    }

    /// Observe the device (or `None` when no device is selected or it vanished) and return the
    /// events needed to bring the core state in line.
    pub fn observe(&mut self, device: Option<ProjectedDevice<'_>>) -> Vec<CallEvent> {
        let mut out = Vec::new();
        let Some(dev) = device.filter(|d| d.link_up) else {
            self.push(
                &mut out,
                CallEvent::Link {
                    state: LinkState::Down,
                },
            );
            return out;
        };
        self.push(
            &mut out,
            CallEvent::Link {
                state: LinkState::Up,
            },
        );

        let mut seen: Vec<CallId> = Vec::new();
        for call in dev.calls {
            let id = core_call_id(dev.epoch, &call.id);
            seen.push(id.clone());
            match core_phase(call, dev.calls) {
                Some(phase) => {
                    let update = CallEvent::CallUpdated {
                        id: id.clone(),
                        direction: core_direction(call.direction),
                        phase,
                        party: CallParty {
                            number: bounded(&call.number),
                            name: bounded(&call.name),
                        },
                    };
                    // A phase change the core machine does not allow (for example a dialing call
                    // that is reported held) is expressed as the end of one call and the start of
                    // another rather than being dropped.
                    let before = self.state.clone();
                    match self.state.apply(update.clone()) {
                        Applied::Changed => out.push(update),
                        Applied::Unchanged => {}
                        Applied::Rejected(EventRejection::InvalidTransition)
                        | Applied::Rejected(EventRejection::InconsistentDirection) => {
                            self.state = before;
                            self.push(
                                &mut out,
                                CallEvent::CallEnded {
                                    id: id.clone(),
                                    reason: EndReason::Unspecified,
                                },
                            );
                            self.push(&mut out, update);
                        }
                        Applied::Rejected(_) => {
                            self.state = before;
                            self.rejected = self.rejected.saturating_add(1);
                        }
                    }
                }
                None => {
                    let reason = if call.status == NativeCallStatus::Lost {
                        EndReason::LinkLost
                    } else {
                        EndReason::Unspecified
                    };
                    if self.state.calls.contains_key(&id) {
                        self.push(&mut out, CallEvent::CallEnded { id, reason });
                    }
                }
            }
        }
        let gone: Vec<CallId> = self
            .state
            .calls
            .keys()
            .filter(|id| !seen.contains(id))
            .cloned()
            .collect();
        for id in gone {
            self.push(
                &mut out,
                CallEvent::CallEnded {
                    id,
                    reason: EndReason::Unspecified,
                },
            );
        }

        let active: Vec<&CallRecord> = dev
            .calls
            .iter()
            .filter(|c| c.status == NativeCallStatus::Talking)
            .collect();
        if active.iter().any(|c| c.muted.is_some()) {
            self.push(
                &mut out,
                CallEvent::MuteChanged {
                    scope: MuteScope::HostMicrophone,
                    muted: active.iter().any(|c| c.muted == Some(true)),
                },
            );
        }
        // "Connected" here is Windows' reported route for a talking call; the codec is not
        // observable through these APIs and no audio has been measured.
        let routed = active.iter().any(|c| c.audio_device == AudioDevice::Local);
        let audio = if routed {
            AudioLinkState::Connected {
                codec: AudioCodec::Unknown,
            }
        } else {
            AudioLinkState::Disconnected
        };
        self.push(&mut out, CallEvent::Audio { state: audio });
        out
    }
}

// ------------------------------------------------------------------ commands from core

/// A Windows-side action planned from a core command, in terms of calls on the observed device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlannedAction {
    Dial { number: String },
    Call { id: NativeCallId, op: CallOp },
    SendTones { id: NativeCallId, tones: String },
}

/// Why a core command has no Windows mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    Rejected(CommandRejection),
    /// No call of the required kind (active, held) is present.
    NoMatchingCall,
}

/// Map a core command to a Windows action using the calls currently observed.
///
/// `Hangup` of a ringing call is a reject; of any other call, an end. `SwapCalls` resumes the
/// held call and lets the phone swap (HFP `CHLD` semantics, decided by the phone). `ConnectAudio`
/// moves the active call's audio to this computer and `DisconnectAudio` back to the phone.
/// Microphone mute applies to the active call. Hold and resume have no core command; use the
/// adapter's own `WinCommand`.
pub fn plan_core_command(
    command: &CallCommand,
    epoch: u64,
    calls: &[CallRecord],
) -> Result<PlannedAction, PlanError> {
    let find = |id: &CallId| {
        calls
            .iter()
            .find(|c| &core_call_id(epoch, &c.id) == id)
            .ok_or(PlanError::Rejected(CommandRejection::NoSuchCall))
    };
    let status = |s: NativeCallStatus| {
        calls
            .iter()
            .find(|c| c.status == s)
            .ok_or(PlanError::NoMatchingCall)
    };
    Ok(match command {
        CallCommand::Answer { call } => PlannedAction::Call {
            id: find(call)?.id.clone(),
            op: CallOp::Accept,
        },
        CallCommand::Hangup { call } => {
            let c = find(call)?;
            PlannedAction::Call {
                id: c.id.clone(),
                op: if c.status.is_ringing() {
                    CallOp::Reject
                } else {
                    CallOp::End
                },
            }
        }
        CallCommand::Dial { number } => PlannedAction::Dial {
            number: number.expose().to_owned(),
        },
        CallCommand::SwapCalls => PlannedAction::Call {
            id: status(NativeCallStatus::Held)?.id.clone(),
            op: CallOp::Resume,
        },
        CallCommand::SendTones { tones } => PlannedAction::SendTones {
            id: status(NativeCallStatus::Talking)?.id.clone(),
            tones: tones.expose().to_owned(),
        },
        CallCommand::SetMicrophoneMute { muted } => PlannedAction::Call {
            id: status(NativeCallStatus::Talking)?.id.clone(),
            op: if *muted { CallOp::Mute } else { CallOp::Unmute },
        },
        CallCommand::ConnectAudio => PlannedAction::Call {
            id: status(NativeCallStatus::Talking)?.id.clone(),
            op: CallOp::Route(AudioDevice::Local),
        },
        CallCommand::DisconnectAudio => PlannedAction::Call {
            id: status(NativeCallStatus::Talking)?.id.clone(),
            op: CallOp::Route(AudioDevice::Remote),
        },
        CallCommand::MuteRinger => {
            return Err(PlanError::Rejected(CommandRejection::Unsupported {
                reason: UnsupportedReason::NotInProtocol,
            }));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, status: NativeCallStatus) -> CallRecord {
        CallRecord {
            id: NativeCallId::new(id).unwrap(),
            status,
            direction: match status {
                NativeCallStatus::Incoming => NativeDirection::Incoming,
                NativeCallStatus::Dialing => NativeDirection::Outgoing,
                _ => NativeDirection::Unknown,
            },
            audio_device: AudioDevice::Remote,
            muted: Some(false),
            number: Some(Sensitive::new("+15550100")),
            name: None,
            hold_supported: Some(true),
        }
    }
    fn dev<'a>(epoch: u64, calls: &'a [CallRecord]) -> Option<ProjectedDevice<'a>> {
        Some(ProjectedDevice {
            epoch,
            link_up: true,
            calls,
        })
    }

    #[test]
    fn ringing_then_answered_then_gone() {
        let mut p = CoreProjector::default();
        let ev = p.observe(dev(1, &[call("a", NativeCallStatus::Incoming)]));
        assert!(matches!(
            ev[0],
            CallEvent::Link {
                state: LinkState::Up
            }
        ));
        assert!(ev.iter().any(|e| matches!(
            e,
            CallEvent::CallUpdated {
                phase: CallPhase::Incoming,
                direction: CallDirection::Incoming,
                ..
            }
        )));
        let ev = p.observe(dev(1, &[call("a", NativeCallStatus::Talking)]));
        assert!(ev.iter().any(|e| matches!(
            e,
            CallEvent::CallUpdated {
                phase: CallPhase::Active,
                ..
            }
        )));
        let ev = p.observe(dev(1, &[]));
        assert_eq!(
            ev,
            vec![CallEvent::CallEnded {
                id: core_call_id(1, &NativeCallId::new("a").unwrap()),
                reason: EndReason::Unspecified
            }]
        );
        assert_eq!(p.state().recent_ended[0].reason, EndReason::Completed);
    }

    #[test]
    fn second_ringing_call_is_waiting() {
        let calls = [
            call("a", NativeCallStatus::Talking),
            call("b", NativeCallStatus::Incoming),
        ];
        assert_eq!(core_phase(&calls[1], &calls), Some(CallPhase::Waiting));
        assert_eq!(core_phase(&calls[0], &calls), Some(CallPhase::Active));
    }

    #[test]
    fn unchanged_observation_emits_nothing() {
        let mut p = CoreProjector::default();
        let calls = [call("a", NativeCallStatus::Talking)];
        p.observe(dev(1, &calls));
        assert!(p.observe(dev(1, &calls)).is_empty());
    }

    #[test]
    fn line_loss_ends_calls_with_link_lost() {
        let mut p = CoreProjector::default();
        p.observe(dev(1, &[call("a", NativeCallStatus::Talking)]));
        let ev = p.observe(None);
        assert_eq!(
            ev,
            vec![CallEvent::Link {
                state: LinkState::Down
            }]
        );
        assert_eq!(p.state().recent_ended[0].reason, EndReason::LinkLost);
    }

    #[test]
    fn new_epoch_never_matches_old_call_ids() {
        let n = NativeCallId::new("same").unwrap();
        assert_ne!(core_call_id(1, &n), core_call_id(2, &n));
    }

    #[test]
    fn phase_jump_the_core_machine_forbids_is_end_then_start() {
        let mut p = CoreProjector::default();
        p.observe(dev(1, &[call("a", NativeCallStatus::Dialing)]));
        let ev = p.observe(dev(1, &[call("a", NativeCallStatus::Held)]));
        assert!(matches!(ev[0], CallEvent::CallEnded { .. }));
        assert!(matches!(
            ev[1],
            CallEvent::CallUpdated {
                phase: CallPhase::Held,
                ..
            }
        ));
        assert_eq!(p.rejected, 0);
    }

    #[test]
    fn lost_status_is_link_lost_not_completed() {
        let mut p = CoreProjector::default();
        p.observe(dev(1, &[call("a", NativeCallStatus::Talking)]));
        p.observe(dev(1, &[call("a", NativeCallStatus::Lost)]));
        assert_eq!(p.state().recent_ended[0].reason, EndReason::LinkLost);
    }

    #[test]
    fn local_route_on_talking_call_is_reported_connected_unknown_codec() {
        let mut p = CoreProjector::default();
        let mut c = call("a", NativeCallStatus::Talking);
        c.audio_device = AudioDevice::Local;
        p.observe(dev(1, &[c]));
        assert_eq!(
            p.state().audio,
            AudioLinkState::Connected {
                codec: AudioCodec::Unknown
            }
        );
    }

    #[test]
    fn hangup_maps_to_reject_for_ringing_and_end_otherwise() {
        let calls = [
            call("r", NativeCallStatus::Incoming),
            call("t", NativeCallStatus::Talking),
        ];
        let id = |n: &str| core_call_id(3, &NativeCallId::new(n).unwrap());
        assert_eq!(
            plan_core_command(&CallCommand::Hangup { call: id("r") }, 3, &calls),
            Ok(PlannedAction::Call {
                id: NativeCallId::new("r").unwrap(),
                op: CallOp::Reject
            })
        );
        assert_eq!(
            plan_core_command(&CallCommand::Hangup { call: id("t") }, 3, &calls),
            Ok(PlannedAction::Call {
                id: NativeCallId::new("t").unwrap(),
                op: CallOp::End
            })
        );
    }

    #[test]
    fn command_with_stale_epoch_has_no_such_call() {
        let calls = [call("t", NativeCallStatus::Talking)];
        let old = core_call_id(2, &NativeCallId::new("t").unwrap());
        assert_eq!(
            plan_core_command(&CallCommand::Hangup { call: old }, 3, &calls),
            Err(PlanError::Rejected(CommandRejection::NoSuchCall))
        );
    }

    #[test]
    fn audio_commands_target_the_talking_call() {
        let calls = [call("t", NativeCallStatus::Talking)];
        assert_eq!(
            plan_core_command(&CallCommand::ConnectAudio, 1, &calls),
            Ok(PlannedAction::Call {
                id: NativeCallId::new("t").unwrap(),
                op: CallOp::Route(AudioDevice::Local)
            })
        );
        assert_eq!(
            plan_core_command(&CallCommand::DisconnectAudio, 1, &[]),
            Err(PlanError::NoMatchingCall)
        );
    }

    #[test]
    fn ringer_mute_is_unsupported() {
        assert!(matches!(
            plan_core_command(&CallCommand::MuteRinger, 1, &[]),
            Err(PlanError::Rejected(CommandRejection::Unsupported { .. }))
        ));
    }

    #[test]
    fn capabilities_are_declared_never_verified() {
        use crate::readiness::{AudioReadiness, Readiness};
        let ready = Readiness {
            blockers: vec![],
            audio: AudioReadiness::Routable,
        };
        let caps = capabilities_for(&ready);
        for f in CallFeature::ALL {
            assert_ne!(caps.support(f), Support::Verified, "{f:?}");
        }
        assert_eq!(caps.support(CallFeature::Answer), Support::Declared);
        assert_eq!(caps.support(CallFeature::AudioUplink), Support::Declared);
        assert!(matches!(
            caps.support(CallFeature::MuteRinger),
            Support::Unsupported { .. }
        ));

        let blocked = Readiness {
            blockers: vec![],
            audio: AudioReadiness::Blocked,
        };
        let caps = capabilities_for(&blocked);
        assert_eq!(caps.support(CallFeature::AudioDownlink), Support::Unknown);
        assert_eq!(caps.support(CallFeature::Answer), Support::Declared);
    }

    #[test]
    fn blocked_readiness_declares_nothing() {
        use crate::readiness::{AudioReadiness, Blocker, Readiness};
        let r = Readiness {
            blockers: vec![Blocker::NotRegistered],
            audio: AudioReadiness::NotApplicable,
        };
        let caps = capabilities_for(&r);
        for f in [
            CallFeature::CallStateEvents,
            CallFeature::Answer,
            CallFeature::Dial,
            CallFeature::AudioLinkControl,
        ] {
            assert_eq!(caps.support(f), Support::Unknown, "{f:?}");
        }
    }
}
