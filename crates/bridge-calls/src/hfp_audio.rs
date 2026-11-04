// SPDX-License-Identifier: GPL-3.0-or-later
//! Glue between the telephony [`Session`] and the call-audio [`CallAudio`] controller: per poll,
//! turn each phone's call state into a [`PhoneInput`], and send the controller's telephony
//! requests (`RejectSCO`, `Activate`) back through the session. Used by the service's Linux
//! hands-free host and by the sandbox tests, so both run the same code.

use bridge_core::calls::CallPhase;

use crate::audio::{AudioServer, BtAddress, CallAudio, PhoneInput, Request, Want};
use crate::{DeviceRef, Session};

/// The phones the session knows, with their controller input.
pub fn phone_inputs(session: &Session) -> Vec<(DeviceRef, PhoneInput)> {
    session
        .phones()
        .filter_map(|(device, state, _)| {
            let address = BtAddress::from_sensitive(&device.address)?;
            let phases: Vec<CallPhase> = state.calls.values().map(|c| c.phase).collect();
            let transport = session.phone(device).and_then(|p| p.transport.as_ref());
            Some((
                device.clone(),
                PhoneInput {
                    address,
                    ringing: phases
                        .iter()
                        .any(|p| matches!(p, CallPhase::Incoming | CallPhase::Waiting)),
                    live: phases.iter().any(|p| {
                        matches!(
                            p,
                            CallPhase::Active
                                | CallPhase::Held
                                | CallPhase::Dialing
                                | CallPhase::Alerting
                        )
                    }),
                    transport: transport.map(|t| t.state),
                    reject_sco: transport.and_then(|t| t.reject_sco),
                },
            ))
        })
        .collect()
}

/// Send the controller's requests. Returns one line per request that failed.
pub fn send(
    session: &Session,
    phones: &[(DeviceRef, PhoneInput)],
    requests: Vec<Request>,
) -> Vec<String> {
    let mut errors = Vec::new();
    for r in requests {
        let addr = match &r {
            Request::SetRejectSco { address, .. } | Request::Activate { address } => *address,
        };
        let Some((device, _)) = phones.iter().find(|(_, p)| p.address == addr) else {
            continue;
        };
        let result = match r {
            Request::SetRejectSco { reject, .. } => session.set_reject_sco(device, reject),
            Request::Activate { .. } => session.activate_audio(device),
        };
        if let Err(e) = result {
            errors.push(match r {
                Request::SetRejectSco { reject: true, .. } => {
                    format!("Could not keep the call audio on the phone: {e}")
                }
                Request::SetRejectSco { reject: false, .. } => {
                    format!("Could not let the phone send the call audio here: {e}")
                }
                Request::Activate { .. } => {
                    format!("Could not ask the phone for the call audio: {e}")
                }
            });
        }
    }
    errors
}

/// One poll: feed the controller and send what it asks for. Returns request failures.
pub fn tick<S: AudioServer>(session: &Session, audio: &mut CallAudio<S>) -> Vec<String> {
    let phones = phone_inputs(session);
    let inputs: Vec<PhoneInput> = phones.iter().map(|(_, p)| p.clone()).collect();
    let requests = audio.tick(&inputs);
    send(session, &phones, requests)
}

/// The user wants this phone's call audio at `want` (answer/dial here, "use computer", "use
/// phone"). Sends the `RejectSCO` change right away, before the call command that follows.
pub fn want<S: AudioServer>(
    session: &Session,
    audio: &mut CallAudio<S>,
    device: &DeviceRef,
    want: Want,
) -> Vec<String> {
    let phones = phone_inputs(session);
    let Some((_, p)) = phones.iter().find(|(d, _)| d == device) else {
        return Vec::new();
    };
    let requests = audio.want(p.address, want, p.reject_sco);
    send(session, &phones, requests)
}

/// Undo everything (service shutdown).
pub fn shutdown<S: AudioServer>(session: &Session, audio: &mut CallAudio<S>) -> Vec<String> {
    let phones = phone_inputs(session);
    let inputs: Vec<PhoneInput> = phones.iter().map(|(_, p)| p.clone()).collect();
    let requests = audio.shutdown(&inputs);
    send(session, &phones, requests)
}
