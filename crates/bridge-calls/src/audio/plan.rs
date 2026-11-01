//! Read-only planning: inventory → explicit routing plan. Pure functions over a
//! [`ServerSnapshot`]; nothing here can touch a server.
//!
//! A plan is selected by Bluetooth *address* (and the exact local device names the caller picked
//! or pinned from the defaults), never by guessing at names. It separates what the server
//! *declares* (endpoints exist, profile is a duplex voice profile) from what has been verified
//! on hardware (always [`HardwareEvidence::NotVerified`] here: no audio sample is inspected).

use serde::Serialize;

use crate::audio::model::{
    BtAddress, CardInfo, EndpointId, PROP_BT_PROFILE, ProfileKind, ServerIdentity, ServerSnapshot,
    SinkInfo, SourceInfo, VoiceCodec, props_address,
};
use crate::audio::spec::{
    DEFAULT_LATENCY_MS, EchoMethod, EchoSpec, LoopbackSpec, MAX_LATENCY_MS, MIN_LATENCY_MS,
    PathDirection, SessionId, VIRTUAL_PREFIX, is_safe_name,
};
use crate::model::{AudioCodecEvidence, TransportSnapshot, TransportState};
use bridge_core::calls::AudioCodec;

/// What the call-control adapter (`AudioGatewayTransport1`) reported. The audio manager does not
/// open the voice link; it requires this evidence that the link is up before connecting a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ControlEvidence {
    pub transport: TransportState,
    pub codec: AudioCodecEvidence,
}

impl From<&TransportSnapshot> for ControlEvidence {
    fn from(t: &TransportSnapshot) -> Self {
        Self {
            transport: t.state,
            codec: t.codec_evidence,
        }
    }
}

/// Which local device a path ends at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "select", content = "name", rename_all = "snake_case")]
pub enum LocalSelection {
    /// An exact sink/source name from the inventory.
    Named(String),
    /// Whatever the server's default is *at prepare time*. It is resolved once and pinned by name
    /// in the plan; a later default change does not move the route, and the default itself is
    /// never changed.
    DefaultAtPrepare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "policy", content = "method", rename_all = "snake_case")]
pub enum EchoPolicy {
    Off,
    /// Use echo cancellation; if it cannot be set up, continue without and report the fallback.
    Preferred(EchoMethod),
    /// Use echo cancellation or do not start at all.
    Required(EchoMethod),
}

impl EchoPolicy {
    pub fn method(&self) -> Option<EchoMethod> {
        match self {
            Self::Off => None,
            Self::Preferred(m) | Self::Required(m) => Some(*m),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouteRequest {
    pub phone: BtAddress,
    /// Where the caller's voice is played.
    pub local_output: LocalSelection,
    /// Whose voice is sent to the caller.
    pub local_input: LocalSelection,
    pub echo: EchoPolicy,
    pub latency_ms: u32,
    pub control: ControlEvidence,
}

impl RouteRequest {
    pub fn new(phone: BtAddress, control: ControlEvidence) -> Self {
        Self {
            phone,
            local_output: LocalSelection::DefaultAtPrepare,
            local_input: LocalSelection::DefaultAtPrepare,
            echo: EchoPolicy::Off,
            latency_ms: DEFAULT_LATENCY_MS,
            control,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRole {
    Output,
    Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalProblem {
    /// A sink monitor is not a microphone.
    IsMonitor,
    /// It belongs to the phone being routed.
    IsThePhone,
    /// A virtual node created by GeekBridge (this or an earlier session).
    IsGeekBridgeVirtual,
    /// The name cannot be passed safely in a module argument.
    UnsafeName,
}

/// Why a route cannot be built right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "blocker", rename_all = "snake_case")]
pub enum Blocker {
    InvalidLatency {
        ms: u32,
    },
    /// The audio server has no card for this phone (not connected, or not yet exposed).
    PhoneCardAbsent,
    AmbiguousPhoneCard,
    /// The card is not in a duplex voice profile. Typical while only A2DP media is up; the call
    /// layer must activate the SCO link and WirePlumber must switch the profile. This crate does
    /// not switch card profiles (WirePlumber persists that choice).
    CardNotInVoiceProfile {
        active: Option<ProfileKind>,
        voice_profile_available: bool,
    },
    /// The card is in an audio-gateway profile: this computer would be the phone.
    CardWrongRole,
    /// The call layer reports the voice transport is not active. Transport activation does not
    /// itself yield microphone audio, but its absence means there is no SCO link at all.
    TransportNotActive {
        state: TransportState,
    },
    /// Transport state `error` or an unrecognised string: needs a control action, not waiting.
    TransportFailed {
        state: TransportState,
    },
    /// The transport's codec and the card's voice profile name disagree.
    CodecMismatch {
        transport: AudioCodec,
        profile: VoiceCodec,
    },
    VoiceEndpointsMissing {
        source: bool,
        sink: bool,
    },
    /// A node of the phone's card advertises a non-voice profile (typically a leftover A2DP
    /// sink) while the card is in a voice profile. It is never used.
    NonVoiceEndpoint {
        endpoint: EndpointId,
        reported: ProfileKind,
    },
    /// A node's Bluetooth address differs from its card's.
    EndpointAddressMismatch {
        endpoint: EndpointId,
    },
    AmbiguousVoiceEndpoints,
    UnsafeName {
        name: String,
    },
    LocalEndpointMissing {
        role: LocalRole,
        name: String,
    },
    NoDefaultLocalEndpoint {
        role: LocalRole,
    },
    LocalEndpointInvalid {
        role: LocalRole,
        name: String,
        problem: LocalProblem,
    },
}

impl Blocker {
    /// `true` if waiting and re-assessing can clear it without any action by the caller.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::PhoneCardAbsent
                | Self::TransportNotActive { .. }
                | Self::CodecMismatch { .. }
                | Self::VoiceEndpointsMissing { .. }
                | Self::NonVoiceEndpoint { .. }
                | Self::CardNotInVoiceProfile {
                    voice_profile_available: true,
                    ..
                }
        )
    }
}

/// Non-blocking observations the caller should surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "warning", rename_all = "snake_case")]
pub enum Warning {
    /// The server did not label a phone node with `api.bluez5.profile`; the card's active
    /// profile was the only profile evidence.
    NodeProfileNotReported,
    /// The transport reported no codec, so the card's codec could not be cross-checked.
    CodecEvidenceMissing,
    /// The chosen local device is itself a Bluetooth device.
    LocalEndpointIsBluetooth { role: LocalRole },
}

/// Hardware verification is separate from the declared state and is never inferred.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HardwareEvidence {
    /// No phone call audio was heard or measured in either direction. Declared inventory and
    /// loaded modules prove nothing about samples.
    NotVerified { reason: &'static str },
}

impl HardwareEvidence {
    pub const fn unverified() -> Self {
        Self::NotVerified {
            reason: "audio samples are not inspected; two-way audibility needs a real phone and a second listener",
        }
    }
}

/// The phone side of a plan, all taken from the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PhoneVoice {
    pub address: BtAddress,
    pub card: EndpointId,
    pub profile: String,
    pub codec: VoiceCodec,
    /// Where the caller's voice comes from.
    pub source: EndpointId,
    /// Where the user's voice goes.
    pub sink: EndpointId,
}

/// One direction of the logical route, before any echo-cancel substitution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PathPlan {
    pub direction: PathDirection,
    pub from: EndpointId,
    pub to: EndpointId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RoutePlan {
    /// The request with both local selections pinned to concrete names.
    pub request: RouteRequest,
    pub server: ServerIdentity,
    pub phone: PhoneVoice,
    pub local_output: EndpointId,
    pub local_input: EndpointId,
    /// RX: `phone.source` → `local_output`. With echo cancellation the sink is the cancel
    /// module's playback node, which itself plays to `local_output`.
    pub rx: PathPlan,
    /// TX: `local_input` → `phone.sink`. With echo cancellation the source is the cancel
    /// module's capture node, which itself records `local_input`.
    pub tx: PathPlan,
    pub warnings: Vec<Warning>,
    pub hardware_verified: HardwareEvidence,
}

/// The module specs a start will load, in load order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RouteSpecs {
    pub echo: Option<EchoSpec>,
    pub rx: LoopbackSpec,
    pub tx: LoopbackSpec,
}

impl RoutePlan {
    /// `with_echo`: route through the echo-cancel nodes of `session`.
    pub fn specs(&self, session: &SessionId, with_echo: bool) -> RouteSpecs {
        let latency_ms = self.request.latency_ms;
        let echo = match (with_echo, self.request.echo.method()) {
            (true, Some(method)) => Some(EchoSpec {
                method,
                source_name: session.echo_source_name(),
                sink_name: session.echo_sink_name(),
                source_master: self.local_input.name.clone(),
                sink_master: self.local_output.name.clone(),
            }),
            _ => None,
        };
        let rx_sink = echo
            .as_ref()
            .map_or_else(|| self.local_output.name.clone(), |e| e.sink_name.clone());
        let tx_source = echo
            .as_ref()
            .map_or_else(|| self.local_input.name.clone(), |e| e.source_name.clone());
        RouteSpecs {
            rx: LoopbackSpec {
                direction: PathDirection::Rx,
                source: self.phone.source.name.clone(),
                sink: rx_sink,
                latency_ms,
            },
            tx: LoopbackSpec {
                direction: PathDirection::Tx,
                source: tx_source,
                sink: self.phone.sink.name.clone(),
                latency_ms,
            },
            echo,
        }
    }

    /// Same concrete devices (the part a start must re-confirm).
    pub fn same_targets(&self, other: &Self) -> bool {
        self.phone.address == other.phone.address
            && self.phone.card == other.phone.card
            && self.phone.profile == other.phone.profile
            && self.phone.source == other.phone.source
            && self.phone.sink == other.phone.sink
            && self.local_output == other.local_output
            && self.local_input == other.local_input
            && self.server == other.server
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "declared", rename_all = "snake_case")]
pub enum DeclaredFeasibility {
    /// Everything the server declares is in place.
    Ready { plan: Box<RoutePlan> },
    /// Only transient blockers: wait and assess again.
    Waiting { blockers: Vec<Blocker> },
    /// At least one blocker will not clear by itself.
    Refused { blockers: Vec<Blocker> },
}

/// Declared and hardware-verified feasibility, kept apart on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub declared: DeclaredFeasibility,
    pub hardware_verified: HardwareEvidence,
}

impl Assessment {
    pub fn blockers(&self) -> &[Blocker] {
        match &self.declared {
            DeclaredFeasibility::Ready { .. } => &[],
            DeclaredFeasibility::Waiting { blockers }
            | DeclaredFeasibility::Refused { blockers } => blockers,
        }
    }
    pub fn plan(&self) -> Option<&RoutePlan> {
        match &self.declared {
            DeclaredFeasibility::Ready { plan } => Some(plan),
            _ => None,
        }
    }
}

struct CardEndpoints<'a> {
    sources: Vec<&'a SourceInfo>,
    sinks: Vec<&'a SinkInfo>,
    non_voice: Vec<(EndpointId, ProfileKind)>,
    address_mismatch: Vec<EndpointId>,
    unlabelled: bool,
}

fn node_profile(props: &crate::audio::model::Props) -> Option<ProfileKind> {
    props.get(PROP_BT_PROFILE).map(|p| ProfileKind::classify(p))
}

fn find_card(snap: &ServerSnapshot, addr: BtAddress) -> Vec<&CardInfo> {
    snap.cards
        .iter()
        .filter(|c| c.address() == Some(addr))
        .collect()
}

fn card_endpoints<'a>(
    snap: &'a ServerSnapshot,
    card: &CardInfo,
    addr: BtAddress,
) -> CardEndpoints<'a> {
    let mut out = CardEndpoints {
        sources: vec![],
        sinks: vec![],
        non_voice: vec![],
        address_mismatch: vec![],
        unlabelled: false,
    };
    let mut classify = |id: EndpointId, props: &crate::audio::model::Props| -> bool {
        if props_address(props).is_some_and(|a| a != addr) {
            out.address_mismatch.push(id);
            return false;
        }
        match node_profile(props) {
            Some(k) if !k.is_voice() => {
                out.non_voice.push((id, k));
                false
            }
            Some(_) => true,
            None => {
                out.unlabelled = true;
                true
            }
        }
    };
    let mut sinks = vec![];
    for s in snap.sinks.iter().filter(|s| s.card == Some(card.index)) {
        if classify(s.id(), &s.props) {
            sinks.push(s);
        }
    }
    let mut sources = vec![];
    for s in snap
        .sources
        .iter()
        .filter(|s| s.card == Some(card.index) && s.monitor_of_sink.is_none())
    {
        if classify(s.id(), &s.props) {
            sources.push(s);
        }
    }
    out.sinks = sinks;
    out.sources = sources;
    out
}

fn codec_of_evidence(e: AudioCodecEvidence) -> Option<AudioCodec> {
    match e {
        AudioCodecEvidence::Known(c) if c != AudioCodec::Unknown => Some(c),
        _ => None,
    }
}

fn codecs_agree(transport: AudioCodec, profile: VoiceCodec) -> bool {
    matches!(
        (transport, profile),
        (AudioCodec::Cvsd, VoiceCodec::Cvsd)
            | (AudioCodec::Msbc, VoiceCodec::Msbc)
            | (AudioCodec::Lc3Swb, VoiceCodec::Lc3)
            | (_, VoiceCodec::Unspecified)
    )
}

enum Resolved<'a> {
    Sink(&'a SinkInfo),
    Source(&'a SourceInfo),
}

fn resolve_local(
    snap: &ServerSnapshot,
    sel: &LocalSelection,
    role: LocalRole,
    phone_addr: BtAddress,
    phone_card: Option<u32>,
    blockers: &mut Vec<Blocker>,
    warnings: &mut Vec<Warning>,
) -> Option<(EndpointId, LocalSelection)> {
    let name = match sel {
        LocalSelection::Named(n) => n.clone(),
        LocalSelection::DefaultAtPrepare => {
            let d = match role {
                LocalRole::Output => snap.default_sink.clone(),
                LocalRole::Input => snap.default_source.clone(),
            };
            match d {
                Some(n) if !n.is_empty() => n,
                _ => {
                    blockers.push(Blocker::NoDefaultLocalEndpoint { role });
                    return None;
                }
            }
        }
    };
    let found = match role {
        LocalRole::Output => snap
            .sinks
            .iter()
            .find(|s| s.name == name)
            .map(Resolved::Sink),
        LocalRole::Input => snap
            .sources
            .iter()
            .find(|s| s.name == name)
            .map(Resolved::Source),
    };
    let Some(found) = found else {
        blockers.push(Blocker::LocalEndpointMissing { role, name });
        return None;
    };
    let (id, card, props, monitor) = match found {
        Resolved::Sink(s) => (s.id(), s.card, &s.props, false),
        Resolved::Source(s) => (s.id(), s.card, &s.props, s.monitor_of_sink.is_some()),
    };
    let problem = if !is_safe_name(&id.name) {
        Some(LocalProblem::UnsafeName)
    } else if id.name.starts_with(VIRTUAL_PREFIX) {
        Some(LocalProblem::IsGeekBridgeVirtual)
    } else if monitor {
        Some(LocalProblem::IsMonitor)
    } else if (card.is_some() && card == phone_card) || props_address(props) == Some(phone_addr) {
        Some(LocalProblem::IsThePhone)
    } else {
        None
    };
    if let Some(problem) = problem {
        blockers.push(Blocker::LocalEndpointInvalid {
            role,
            name: id.name,
            problem,
        });
        return None;
    }
    if props_address(props).is_some() {
        warnings.push(Warning::LocalEndpointIsBluetooth { role });
    }
    Some((id.clone(), LocalSelection::Named(id.name)))
}

/// Build an [`Assessment`] for `req` against one read of the server.
pub fn assess(snap: &ServerSnapshot, req: &RouteRequest) -> Assessment {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if !(MIN_LATENCY_MS..=MAX_LATENCY_MS).contains(&req.latency_ms) {
        blockers.push(Blocker::InvalidLatency { ms: req.latency_ms });
    }

    match req.control.transport {
        TransportState::Active => {}
        s @ (TransportState::Idle | TransportState::Pending) => {
            blockers.push(Blocker::TransportNotActive { state: s });
        }
        s @ (TransportState::Error | TransportState::Unrecognized) => {
            blockers.push(Blocker::TransportFailed { state: s });
        }
    }

    // ---- phone card and its voice endpoints
    let cards = find_card(snap, req.phone);
    let mut phone: Option<PhoneVoice> = None;
    let mut phone_card_index = None;
    match cards.as_slice() {
        [] => blockers.push(Blocker::PhoneCardAbsent),
        [card] => {
            phone_card_index = Some(card.index);
            let kind = card.active_kind();
            match kind {
                Some(ProfileKind::Voice(codec)) => {
                    if let Some(tc) = codec_of_evidence(req.control.codec) {
                        if !codecs_agree(tc, codec) {
                            blockers.push(Blocker::CodecMismatch {
                                transport: tc,
                                profile: codec,
                            });
                        }
                    } else if req.control.transport == TransportState::Active {
                        warnings.push(Warning::CodecEvidenceMissing);
                    }
                    let ep = card_endpoints(snap, card, req.phone);
                    for (endpoint, reported) in &ep.non_voice {
                        blockers.push(Blocker::NonVoiceEndpoint {
                            endpoint: endpoint.clone(),
                            reported: *reported,
                        });
                    }
                    for endpoint in &ep.address_mismatch {
                        blockers.push(Blocker::EndpointAddressMismatch {
                            endpoint: endpoint.clone(),
                        });
                    }
                    match (ep.sources.as_slice(), ep.sinks.as_slice()) {
                        ([src], [snk]) => {
                            if !is_safe_name(&src.name) {
                                blockers.push(Blocker::UnsafeName {
                                    name: src.name.clone(),
                                });
                            }
                            if !is_safe_name(&snk.name) {
                                blockers.push(Blocker::UnsafeName {
                                    name: snk.name.clone(),
                                });
                            }
                            if ep.unlabelled {
                                warnings.push(Warning::NodeProfileNotReported);
                            }
                            phone = Some(PhoneVoice {
                                address: req.phone,
                                card: EndpointId {
                                    index: card.index,
                                    name: card.name.clone(),
                                },
                                profile: card.active_profile.clone().unwrap_or_default(),
                                codec,
                                source: src.id(),
                                sink: snk.id(),
                            });
                        }
                        (srcs, snks) if srcs.len() > 1 || snks.len() > 1 => {
                            blockers.push(Blocker::AmbiguousVoiceEndpoints);
                        }
                        (srcs, snks) => blockers.push(Blocker::VoiceEndpointsMissing {
                            source: srcs.is_empty(),
                            sink: snks.is_empty(),
                        }),
                    }
                }
                Some(ProfileKind::AudioGateway) => blockers.push(Blocker::CardWrongRole),
                other => blockers.push(Blocker::CardNotInVoiceProfile {
                    active: other,
                    voice_profile_available: card.voice_profile_available(),
                }),
            }
        }
        _ => blockers.push(Blocker::AmbiguousPhoneCard),
    }

    // ---- local endpoints
    let out = resolve_local(
        snap,
        &req.local_output,
        LocalRole::Output,
        req.phone,
        phone_card_index,
        &mut blockers,
        &mut warnings,
    );
    let inp = resolve_local(
        snap,
        &req.local_input,
        LocalRole::Input,
        req.phone,
        phone_card_index,
        &mut blockers,
        &mut warnings,
    );

    let hardware_verified = HardwareEvidence::unverified();
    if !blockers.is_empty() {
        let declared = if blockers.iter().all(Blocker::is_transient) {
            DeclaredFeasibility::Waiting { blockers }
        } else {
            DeclaredFeasibility::Refused { blockers }
        };
        return Assessment {
            declared,
            hardware_verified,
        };
    }
    // No blockers imply every part resolved.
    let (Some(phone), Some((local_output, out_sel)), Some((local_input, in_sel))) =
        (phone, out, inp)
    else {
        unreachable!("resolution without blockers always yields all endpoints");
    };
    let mut pinned = req.clone();
    pinned.local_output = out_sel;
    pinned.local_input = in_sel;
    let plan = RoutePlan {
        rx: PathPlan {
            direction: PathDirection::Rx,
            from: phone.source.clone(),
            to: local_output.clone(),
        },
        tx: PathPlan {
            direction: PathDirection::Tx,
            from: local_input.clone(),
            to: phone.sink.clone(),
        },
        request: pinned,
        server: snap.identity.clone(),
        phone,
        local_output,
        local_input,
        warnings,
        hardware_verified: HardwareEvidence::unverified(),
    };
    Assessment {
        declared: DeclaredFeasibility::Ready {
            plan: Box::new(plan),
        },
        hardware_verified,
    }
}
