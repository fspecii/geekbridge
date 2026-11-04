// SPDX-License-Identifier: GPL-3.0-or-later
//! A stateful view over a [`CallBackend`]: diffs snapshots into the neutral
//! [`bridge_core::calls::CallEvent`] stream, keeps one [`CallState`] per phone, and gates explicit
//! [`CallCommand`]s through the model's pre-flight before dispatch.

use std::collections::BTreeMap;
use std::time::Duration;

use bridge_core::calls::{
    Applied, CallCapabilities, CallCommand, CallEvent, CallId, CallState, EndReason,
    EventRejection, LinkState,
};

use crate::error::{BackendError, ControlError};
use crate::{Action, CallBackend, CallFingerprint, DeviceRef, PhoneSnapshot, ServiceStatus};

/// One event applied (or rejected) for one phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEvent {
    pub device: DeviceRef,
    pub event: CallEvent,
    pub applied: Applied,
}

struct Tracked {
    phone: PhoneSnapshot,
    state: CallState,
    fingerprints: BTreeMap<CallId, CallFingerprint>,
}

pub struct Session {
    backend: Box<dyn CallBackend>,
    phones: BTreeMap<String, Tracked>,
    service: ServiceStatus,
}

impl Session {
    pub fn new(backend: Box<dyn CallBackend>) -> Self {
        Self {
            backend,
            phones: BTreeMap::new(),
            service: ServiceStatus::Absent,
        }
    }

    pub fn service(&self) -> &ServiceStatus {
        &self.service
    }

    /// Phones currently believed connected, with their reduced call state.
    pub fn phones(&self) -> impl Iterator<Item = (&DeviceRef, &CallState, &CallCapabilities)> {
        self.phones
            .values()
            .map(|t| (&t.phone.device, &t.state, &t.phone.capabilities))
    }

    pub fn phone(&self, device: &DeviceRef) -> Option<&PhoneSnapshot> {
        self.phones
            .get(&device.path)
            .filter(|t| &t.phone.device == device)
            .map(|t| &t.phone)
    }

    pub fn state(&self, device: &DeviceRef) -> Option<&CallState> {
        self.phones
            .get(&device.path)
            .filter(|t| &t.phone.device == device)
            .map(|t| &t.state)
    }

    /// Read the backend and fold the differences into per-phone state. Only changes are returned.
    ///
    /// A vanished phone, a restarted service (new owner) or an absent service produces
    /// `Link { Down }` for the affected phones, which the model turns into `LinkLost` call
    /// endings. On a backend error nothing is changed.
    pub fn refresh(&mut self) -> Result<Vec<SessionEvent>, BackendError> {
        let snapshot = self.backend.snapshot()?;
        let mut out = Vec::new();

        // Phones that are gone, or whose identity changed under the same path.
        let gone: Vec<String> = self
            .phones
            .iter()
            .filter(|(path, tracked)| {
                !snapshot
                    .phones
                    .iter()
                    .any(|p| &p.device.path == *path && p.device == tracked.phone.device)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in gone {
            if let Some(mut tracked) = self.phones.remove(&path) {
                let device = tracked.phone.device.clone();
                push(
                    &mut out,
                    &device,
                    &mut tracked.state,
                    CallEvent::Link {
                        state: LinkState::Down,
                    },
                );
            }
        }

        for phone in snapshot.phones {
            match self.phones.get_mut(&phone.device.path) {
                None => {
                    let mut state = CallState::default();
                    let device = phone.device.clone();
                    push(
                        &mut out,
                        &device,
                        &mut state,
                        CallEvent::Link {
                            state: LinkState::Up,
                        },
                    );
                    let mut tracked = Tracked {
                        phone: phone.clone(),
                        state,
                        fingerprints: BTreeMap::new(),
                    };
                    diff_phone(&mut out, &mut tracked, &phone);
                    tracked.phone = phone;
                    self.phones.insert(device.path.clone(), tracked);
                }
                Some(tracked) => {
                    diff_phone(&mut out, tracked, &phone);
                    tracked.phone = phone;
                }
            }
        }
        self.service = snapshot.service;
        Ok(out)
    }

    /// Wait for the backend to signal a change (or `timeout`), then [`refresh`](Self::refresh).
    pub fn pump(&mut self, timeout: Duration) -> Result<Vec<SessionEvent>, BackendError> {
        self.backend.wait_for_change(timeout)?;
        self.refresh()
    }

    /// Accept (`false`) or refuse (`true`) the voice link the phone opens. Not a call command:
    /// the call-audio controller uses it to keep the audio where the user wants it.
    pub fn set_reject_sco(&self, device: &DeviceRef, reject: bool) -> Result<(), ControlError> {
        let tracked = self
            .phones
            .get(&device.path)
            .ok_or(ControlError::NoSuchDevice)?;
        if tracked.phone.device != *device {
            return Err(ControlError::StaleDevice);
        }
        self.backend.invoke(&Action::SetRejectSco {
            device: device.clone(),
            reject,
        })
    }

    /// Ask the phone to open the voice link (`Activate`), outside the call-command pre-flight.
    pub fn activate_audio(&self, device: &DeviceRef) -> Result<(), ControlError> {
        if !self.phones.contains_key(&device.path) {
            return Err(ControlError::NoSuchDevice);
        }
        self.backend.invoke(&Action::ActivateAudio {
            device: device.clone(),
        })
    }

    /// Run a neutral command against one phone: model pre-flight (capability, arguments, link,
    /// call state), then backend dispatch with fresh-state revalidation. `Ok` means the service
    /// accepted the request; the effect appears in a later [`refresh`](Self::refresh).
    pub fn execute(&self, device: &DeviceRef, command: &CallCommand) -> Result<(), ControlError> {
        let tracked = self
            .phones
            .get(&device.path)
            .ok_or(ControlError::NoSuchDevice)?;
        if tracked.phone.device.owner != device.owner {
            return Err(ControlError::ServiceRestarted);
        }
        if tracked.phone.device.address != device.address {
            return Err(ControlError::StaleDevice);
        }
        tracked
            .state
            .check_command(&tracked.phone.capabilities, command)
            .map_err(ControlError::Preflight)?;
        let call_ref = |id: &CallId| tracked.phone.call_ref(id).ok_or(ControlError::NoSuchCall);
        let action = match command {
            CallCommand::Answer { call } => Action::Answer {
                call: call_ref(call)?,
            },
            CallCommand::Hangup { call } => Action::Hangup {
                call: call_ref(call)?,
            },
            CallCommand::Reject { call } => Action::Reject {
                call: call_ref(call)?,
            },
            CallCommand::Dial { number } => Action::Dial {
                device: device.clone(),
                number: number.clone(),
            },
            CallCommand::SwapCalls => Action::SwapCalls {
                device: device.clone(),
            },
            CallCommand::SendTones { tones } => Action::SendTones {
                device: device.clone(),
                tones: tones.clone(),
            },
            CallCommand::ConnectAudio => Action::ActivateAudio {
                device: device.clone(),
            },
            CallCommand::DisconnectAudio => {
                return Err(ControlError::Unsupported(
                    "the telephony API has no method to close the audio transport",
                ));
            }
            CallCommand::MuteRinger => {
                return Err(ControlError::Unsupported(
                    "HFP has no ringer-mute procedure",
                ));
            }
            CallCommand::SetMicrophoneMute { .. } => {
                return Err(ControlError::Unsupported(
                    "microphone mute belongs to the host audio graph, not this API",
                ));
            }
        };
        self.backend.invoke(&action)
    }

    /// Reject a ringing call (hang-up restricted to `incoming`/`waiting`). The neutral model has no
    /// reject command, so this goes through the same state gates as [`execute`](Self::execute).
    pub fn reject(&self, device: &DeviceRef, call: &CallId) -> Result<(), ControlError> {
        let tracked = self
            .phones
            .get(&device.path)
            .ok_or(ControlError::NoSuchDevice)?;
        let call = tracked
            .phone
            .call_ref(call)
            .ok_or(ControlError::NoSuchCall)?;
        self.backend.invoke(&Action::Reject { call })
    }

    /// Dispatch a typed [`Action`] directly (hold/swap variants, multiparty). The backend
    /// revalidates it against fresh state.
    pub fn invoke(&self, action: &Action) -> Result<(), ControlError> {
        self.backend.invoke(action)
    }
}

fn push(out: &mut Vec<SessionEvent>, device: &DeviceRef, state: &mut CallState, event: CallEvent) {
    let applied = state.apply(event.clone());
    if applied != Applied::Unchanged {
        out.push(SessionEvent {
            device: device.clone(),
            event,
            applied,
        });
    }
}

fn diff_phone(out: &mut Vec<SessionEvent>, tracked: &mut Tracked, new: &PhoneSnapshot) {
    let device = new.device.clone();
    let audio = new
        .transport
        .as_ref()
        .map_or(Default::default(), |t| t.audio_link());
    push(
        out,
        &device,
        &mut tracked.state,
        CallEvent::Audio { state: audio },
    );

    let mut live_ids = Vec::new();
    for call in &new.calls {
        let Some(phase) = call.observation.phase() else {
            continue; // disconnected or unrecognized: handled below / never acted on
        };
        live_ids.push(call.id.clone());
        // Same path, different caller: the earlier call ended and another took its object path.
        if tracked
            .fingerprints
            .get(&call.id)
            .is_some_and(|fp| fp != &call.fingerprint)
        {
            end(out, &device, tracked, &call.id);
        }
        let update = CallEvent::CallUpdated {
            id: call.id.clone(),
            direction: call.direction(),
            phase,
            party: call.party(),
        };
        let existed = tracked.state.calls.contains_key(&call.id);
        let applied = tracked.state.apply(update.clone());
        match applied {
            Applied::Unchanged => {}
            Applied::Rejected(
                EventRejection::InvalidTransition | EventRejection::InconsistentDirection,
            ) if existed => {
                // The reducer cannot get from the old phase to this one: PipeWire reused the id
                // between two refreshes. Treat as end + new call rather than guessing.
                end(out, &device, tracked, &call.id);
                push(out, &device, &mut tracked.state, update);
            }
            other => out.push(SessionEvent {
                device: device.clone(),
                event: update,
                applied: other,
            }),
        }
        tracked
            .fingerprints
            .insert(call.id.clone(), call.fingerprint.clone());
    }

    let stale: Vec<CallId> = tracked
        .state
        .calls
        .keys()
        .filter(|id| !live_ids.contains(id))
        .cloned()
        .collect();
    for id in stale {
        end(out, &device, tracked, &id);
    }
    tracked.fingerprints.retain(|id, _| live_ids.contains(id));
}

fn end(out: &mut Vec<SessionEvent>, device: &DeviceRef, tracked: &mut Tracked, id: &CallId) {
    tracked.fingerprints.remove(id);
    if tracked.state.calls.contains_key(id) {
        push(
            out,
            device,
            &mut tracked.state,
            CallEvent::CallEnded {
                id: id.clone(),
                reason: EndReason::Unspecified,
            },
        );
    }
}
