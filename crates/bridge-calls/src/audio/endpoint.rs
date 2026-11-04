// SPDX-License-Identifier: GPL-3.0-or-later
//! Where call audio that is NOT the Bluetooth voice link plays and is captured: the user's headphones and
//! microphone for calls, the very pick the Bluetooth path routes to (`call-audio.json`, written by
//! `geekbridge-ctl call audio-devices` and the Calls window).
//!
//! Used by the experimental Wi-Fi call audio (the far party's voice played through the chosen output, the
//! computer's microphone for "Speak"). There is one pick and one resolver: [`route::resolve`] decides what
//! the pick means right now (the device, the system default, or the default because the pick is unplugged).

use std::path::Path;

use super::prefs::{self, Choice};
use super::route::{self, Resolved, Role};
use super::{AudioServer, ServerError, ServerSnapshot};

/// A device a stream can be sent to or taken from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// The server's device name (a PipeWire `node.name`): what `pw-cat --target` takes.
    pub name: String,
    /// What to show a person.
    pub description: String,
    /// The pick that was wanted when it is not connected and the default stands in.
    pub missing_pick: Option<String>,
}

/// The device for `role` given the user's `choice`, read from one server snapshot. `None`: no such device at all.
pub fn endpoint_in(snap: &ServerSnapshot, role: Role, choice: &Choice) -> Option<Endpoint> {
    let wanted = match role {
        Role::Output => choice.output.as_deref(),
        Role::Input => choice.input.as_deref(),
    };
    let resolved = route::resolve(snap, role, wanted)?;
    let name = resolved.name().to_owned();
    Some(Endpoint {
        description: route::describe(snap, role, &name),
        missing_pick: match resolved {
            Resolved::Fallback { wanted, .. } => Some(wanted),
            _ => None,
        },
        name,
    })
}

/// The user's pick in `state_dir` resolved against the running sound server (blocking). `Ok(None)` when the
/// computer has no device for `role`.
pub fn endpoint(state_dir: &Path, role: Role) -> Result<Option<Endpoint>, ServerError> {
    let choice = prefs::load_choice(&state_dir.join(prefs::CHOICE_FILE));
    let mut server = super::connect_platform()?;
    let snap = server.snapshot()?;
    Ok(endpoint_in(&snap, role, &choice))
}

#[cfg(test)]
mod tests {
    use super::super::route::fixtures::*;
    use super::*;

    #[test]
    fn the_pick_wins_the_default_stands_in_and_a_missing_pick_is_named() {
        let s = snapshot("a2dp-sink", false);
        let none = Choice::default();
        let d = endpoint_in(&s, Role::Output, &none).expect("a default output");
        assert!(d.missing_pick.is_none());
        let pick = Choice {
            output: Some(HEADSET_OUT.into()),
            input: Some(HEADSET_IN.into()),
        };
        let o = endpoint_in(&s, Role::Output, &pick).unwrap();
        assert_eq!(o.name, HEADSET_OUT);
        assert!(!o.description.is_empty());
        assert_eq!(
            endpoint_in(&s, Role::Input, &pick).unwrap().name,
            HEADSET_IN
        );
        let gone = Choice {
            output: Some("alsa_output.unplugged".into()),
            input: None,
        };
        let f = endpoint_in(&s, Role::Output, &gone).unwrap();
        assert_eq!(f.missing_pick.as_deref(), Some("alsa_output.unplugged"));
        assert_ne!(f.name, "alsa_output.unplugged");
    }
}
