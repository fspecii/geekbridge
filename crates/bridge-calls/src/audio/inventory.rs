// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only inventory of what can take part in call audio, derived from one snapshot.

use serde::Serialize;

use crate::audio::model::{
    BtAddress, EndpointId, PROP_BT_PROFILE, ProfileKind, ServerIdentity, ServerSnapshot,
    props_address,
};
use crate::audio::spec::{SessionId, session_in_argument};

/// One Bluetooth phone card and what it currently exposes.
///
/// `a2dp_sink` and the voice endpoints are reported separately on purpose: a phone playing music
/// to this computer (A2DP) has an output endpoint and no microphone, which is *not* a call path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PhoneAudio {
    pub address: BtAddress,
    pub card: EndpointId,
    pub active_profile: Option<String>,
    pub active_kind: Option<ProfileKind>,
    /// The card lists an available duplex voice profile (declared, not exercised).
    pub voice_profile_available: bool,
    /// An A2DP sink node (phone media playing here), if the active profile exposes one.
    pub a2dp_sink: Option<EndpointId>,
    pub voice_input: Option<EndpointId>,
    pub voice_output: Option<EndpointId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalEndpoint {
    pub id: EndpointId,
    pub description: String,
    /// Set if this local device is itself Bluetooth.
    pub bluetooth: Option<BtAddress>,
}

/// A module that carries a GeekBridge tag but belongs to no live session known to the manager,
/// typically left behind by a crashed process. Never touched unless the caller passes it to
/// `reap_orphans`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrphanModule {
    pub index: u32,
    pub module: String,
    pub session: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioInventory {
    pub server: ServerIdentity,
    pub default_sink: Option<String>,
    pub default_source: Option<String>,
    pub phones: Vec<PhoneAudio>,
    pub local_outputs: Vec<LocalEndpoint>,
    pub local_inputs: Vec<LocalEndpoint>,
    pub orphans: Vec<OrphanModule>,
}

pub(crate) fn orphans_in(snap: &ServerSnapshot, live: &[SessionId]) -> Vec<OrphanModule> {
    snap.modules
        .iter()
        .filter_map(|m| {
            let session = session_in_argument(&m.name, m.argument.as_deref()?)?;
            (!live.contains(&session)).then(|| OrphanModule {
                index: m.index,
                module: m.name.clone(),
                session,
            })
        })
        .collect()
}

pub fn inventory(snap: &ServerSnapshot, live_sessions: &[SessionId]) -> AudioInventory {
    let mut phones = Vec::new();
    for card in &snap.cards {
        let Some(address) = card.address() else {
            continue;
        };
        let mut p = PhoneAudio {
            address,
            card: EndpointId {
                index: card.index,
                name: card.name.clone(),
            },
            active_profile: card.active_profile.clone(),
            active_kind: card.active_kind(),
            voice_profile_available: card.voice_profile_available(),
            a2dp_sink: None,
            voice_input: None,
            voice_output: None,
        };
        let kind_of = |props: &crate::audio::model::Props| {
            props.get(PROP_BT_PROFILE).map(|v| ProfileKind::classify(v))
        };
        let voice_card = p.active_kind.is_some_and(|k| k.is_voice());
        for s in snap.sinks.iter().filter(|s| s.card == Some(card.index)) {
            match kind_of(&s.props) {
                Some(ProfileKind::A2dpSink) => p.a2dp_sink = Some(s.id()),
                Some(k) if k.is_voice() => p.voice_output = Some(s.id()),
                None if voice_card => p.voice_output = Some(s.id()),
                None if p.active_kind == Some(ProfileKind::A2dpSink) => p.a2dp_sink = Some(s.id()),
                _ => {}
            }
        }
        for s in snap
            .sources
            .iter()
            .filter(|s| s.card == Some(card.index) && s.monitor_of_sink.is_none())
        {
            match kind_of(&s.props) {
                Some(k) if k.is_voice() => p.voice_input = Some(s.id()),
                None if voice_card => p.voice_input = Some(s.id()),
                _ => {}
            }
        }
        phones.push(p);
    }
    let local_outputs = snap
        .sinks
        .iter()
        .map(|s| LocalEndpoint {
            id: s.id(),
            description: s.description.clone(),
            bluetooth: props_address(&s.props),
        })
        .collect();
    let local_inputs = snap
        .sources
        .iter()
        .filter(|s| s.monitor_of_sink.is_none())
        .map(|s| LocalEndpoint {
            id: s.id(),
            description: s.description.clone(),
            bluetooth: props_address(&s.props),
        })
        .collect();
    AudioInventory {
        server: snap.identity.clone(),
        default_sink: snap.default_sink.clone(),
        default_source: snap.default_source.clone(),
        phones,
        local_outputs,
        local_inputs,
        orphans: orphans_in(snap, live_sessions),
    }
}
