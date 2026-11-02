// SPDX-License-Identifier: GPL-3.0-or-later
//! State orchestration for one phone link: connect/disconnect lifecycle, generation binding of
//! native callbacks, command planning and dispatch, confirmation by observed effect, timeouts,
//! link loss, and the neutral `bridge_core::calls` event stream.
//!
//! The controller is single-threaded and clock-driven: the owner calls [`Controller::pump`]
//! regularly (the helper does so between run-loop slices) and reads [`Controller::drain`]. It
//! never blocks and never touches Objective-C directly; that is the [`NativePort`]'s business.
//!
//! Invariants enforced here (each has a test):
//! * Only the active generation's callbacks are processed; everything else is counted and dropped
//!   ([`Stats::stale_events`]), including callbacks arriving after `disconnect`.
//! * Call control is refused unless the phone has reported call indicators, and ambiguous or
//!   unidentifiable operations are refused (see [`crate::plan`]).
//! * A command is "confirmed" only by a later observed effect; absence of one yields
//!   [`Outcome::NotObserved`] and non-idempotent commands (dial) are never retried.
//! * Link loss, disconnect and connect failure always end every pending command and every live
//!   call, mark audio disconnected and restore any audio route this controller owns.
use crate::caps::{self, MacCapabilities};
use crate::error::AdapterError;
use crate::model::{
    AgClass, AgFeatures, BluetoothAuth, BtAddress, Clock, Generation, Indicator, Inventory,
    IoStatus, Millis, NativeEvent, PROTOCOL_VERSION, PhoneRecord, Tagged,
};
use crate::plan::{AudioStrategy, Expect, MacCommand, OpKind, PlanInput, plan};
use crate::port::{AudioPort, NativePort};
use crate::route::{AudioProof, RestoreReport, RouteManager, RouteReport, RouteRequest};
use crate::tracker::CallTracker;
use bridge_core::calls::{
    Applied, AudioCodec, AudioLinkState, CallCommand, CallEvent, CallFeature, CallState,
    EventRejection, HfpEvidence, LinkState, Support,
};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub connect_timeout_ms: u64,
    pub command_timeout_ms: u64,
    pub dial_timeout_ms: u64,
    pub audio_timeout_ms: u64,
    pub close_timeout_ms: u64,
    /// Delegates are retired, never freed early; this bounds how many a process may create.
    pub max_generations: u64,
    /// Allow connecting to a paired device whose cached SDP data lacks the AG role.
    pub allow_unadvertised: bool,
    /// Minimum spacing between `currentCallList` requests.
    pub call_list_interval_ms: u64,
    /// Permit macOS to show its Bluetooth permission prompt.
    pub allow_permission_prompt: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 20_000,
            command_timeout_ms: 8_000,
            dial_timeout_ms: 15_000,
            audio_timeout_ms: 10_000,
            close_timeout_ms: 5_000,
            max_generations: 256,
            allow_unadvertised: false,
            call_list_interval_ms: 2_000,
            allow_permission_prompt: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Ticket(pub u64);

/// Result of a command, as far as can be known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// The expected effect was observed.
    Confirmed,
    /// Issued; this operation has no observable effect (DTMF), so nothing more can be said.
    Dispatched,
    /// Issued, but no effect was seen before the deadline. The gateway may still have acted.
    /// `retry_safe` is false for dial.
    NotObserved { after_ms: u64, retry_safe: bool },
    /// The link ended before the effect was seen.
    Aborted { reason: DownReason },
    /// The SCO open attempt was reported as failed.
    AudioFailed { status: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DownReason {
    Requested,
    ConnectFailed {
        status: i32,
        permission_suspected: bool,
    },
    ConnectTimeout,
    /// Link dropped while up (phone out of range, Bluetooth off, sleep). Outcome of calls unknown.
    Lost {
        status: i32,
    },
    CloseTimeout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkNotice {
    Connecting,
    Up,
    Closing,
    Down(DownReason),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "diag", rename_all = "snake_case")]
pub enum Diag {
    /// A derived call event was refused by the core state machine.
    CoreRejected {
        rejection: &'static str,
    },
    UnhandledResult {
        len: usize,
    },
    Malformed {
        what: &'static str,
    },
    DuplicateConnected,
    /// The hands-free feature bitmap the framework used before our explicit request; recorded so
    /// a hardware run can see what the undocumented default was.
    HostFeatureDefault {
        default_hf_features: u32,
    },
    RouteRestoreFailed,
    NativeFailure {
        op: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// Feed to the daemon's `CallState::apply`.
    Call {
        event: CallEvent,
    },
    Link {
        generation: Generation,
        notice: LinkNotice,
    },
    Capabilities {
        capabilities: Box<MacCapabilities>,
    },
    Command {
        ticket: Ticket,
        op: OpKind,
        outcome: Outcome,
    },
    RouteApplied {
        report: RouteReport,
    },
    RouteRestored {
        report: RestoreReport,
    },
    Diagnostic {
        diag: Diag,
    },
}

/// Result of the read-only probe.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProbeReport {
    pub protocol: u32,
    pub authorization: BluetoothAuth,
    /// Why no inventory was produced, if that is the case.
    pub blocked: Option<AdapterError>,
    pub phones: Vec<PhoneRecord>,
    pub ag_capable: usize,
    pub capabilities: MacCapabilities,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PhoneStatus {
    pub service: Option<bool>,
    pub signal: Option<u8>,
    pub roaming: Option<bool>,
    pub battery: Option<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub stale_events: u64,
    pub malformed_events: u64,
    pub unhandled_results: u64,
    pub core_rejections: u64,
    pub duplicate_connected: u64,
    pub unmatched_list_entries: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPhase {
    Connecting,
    Up,
    Closing,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Snapshot {
    pub generation: Option<Generation>,
    pub phase: Option<LinkPhase>,
    pub core: CallState,
    pub capabilities: MacCapabilities,
    pub phone: PhoneStatus,
    pub stats: Stats,
    pub pending: usize,
    pub identity_uncertain: bool,
}

#[derive(Debug)]
struct Pending {
    ticket: Ticket,
    kind: OpKind,
    expect: Expect,
    deadline: Millis,
    issued: Millis,
    retry_safe: bool,
}

struct Link {
    generation: Generation,
    address: BtAddress,
    phase: LinkPhase,
    deadline: Millis,
    tracker: CallTracker,
    evidence: HfpEvidence,
    ag: AgFeatures,
    phone: PhoneStatus,
    sco_open: bool,
    audio_strategy: Option<AudioStrategy>,
    pending: Vec<Pending>,
    /// Call events derived before the service-level connection completed.
    backlog: Vec<CallEvent>,
    last_list_request: Option<Millis>,
    caller_id_seen: bool,
}

pub struct Controller<N: NativePort, A: AudioPort, C: Clock> {
    native: N,
    routes: RouteManager<A>,
    clock: C,
    cfg: Config,
    tx: Sender<Tagged>,
    rx: Receiver<Tagged>,
    generations_used: u64,
    link: Option<Link>,
    core: CallState,
    caps: MacCapabilities,
    auth: BluetoothAuth,
    routing_support: Support,
    proof: AudioProof,
    out: VecDeque<AdapterEvent>,
    next_ticket: u64,
    stats: Stats,
}

impl<N: NativePort, A: AudioPort, C: Clock> Controller<N, A, C> {
    pub fn new(native: N, audio: A, clock: C, cfg: Config) -> Self {
        let (tx, rx) = channel();
        Self {
            native,
            routes: RouteManager::new(audio),
            clock,
            cfg,
            tx,
            rx,
            generations_used: 0,
            link: None,
            core: CallState::default(),
            caps: caps::unprobed(BluetoothAuth::Unknown),
            auth: BluetoothAuth::Unknown,
            routing_support: Support::Unknown,
            proof: AudioProof::None,
            out: VecDeque::new(),
            next_ticket: 1,
            stats: Stats::default(),
        }
    }

    pub fn native_mut(&mut self) -> &mut N {
        &mut self.native
    }
    pub fn routes_mut(&mut self) -> &mut RouteManager<A> {
        &mut self.routes
    }
    pub fn config(&self) -> &Config {
        &self.cfg
    }
    pub fn capabilities(&self) -> &MacCapabilities {
        &self.caps
    }
    pub fn generation(&self) -> Option<Generation> {
        self.link.as_ref().map(|l| l.generation)
    }
    pub fn phase(&self) -> Option<LinkPhase> {
        self.link.as_ref().map(|l| l.phase)
    }

    pub fn snapshot(&self) -> Snapshot {
        let (pending, uncertain, phone, unmatched) = match &self.link {
            Some(l) => (
                l.pending.len(),
                l.tracker.identity_uncertain(),
                l.phone,
                l.tracker.unmatched_list_entries,
            ),
            None => (0, false, PhoneStatus::default(), 0),
        };
        Snapshot {
            generation: self.generation(),
            phase: self.phase(),
            core: self.core.clone(),
            capabilities: self.caps.clone(),
            phone,
            stats: Stats {
                unmatched_list_entries: unmatched,
                ..self.stats
            },
            pending,
            identity_uncertain: uncertain,
        }
    }

    pub fn drain(&mut self) -> Vec<AdapterEvent> {
        self.out.drain(..).collect()
    }

    pub fn now(&self) -> Millis {
        self.clock.now()
    }

    // ------------------------------------------------------------ inventory and lifecycle

    /// The default read-only action: authorization, paired devices and the (unprobed) capability
    /// picture. Never connects, never sends a command, and does not trigger a permission prompt
    /// unless [`Config::allow_permission_prompt`] is set. Problems are reported in the result
    /// instead of failing, so a denied permission is still a useful answer.
    pub fn probe(&mut self) -> ProbeReport {
        if let Err(e) = self.check_host() {
            return ProbeReport {
                protocol: PROTOCOL_VERSION,
                authorization: BluetoothAuth::Unknown,
                blocked: Some(e),
                phones: Vec::new(),
                ag_capable: 0,
                capabilities: caps::unsupported_host(),
            };
        }
        let authorization = self.native.authorization();
        self.auth = authorization;
        let mut blocked = match authorization {
            a if a.blocks_use() => Some(AdapterError::PermissionDenied { authorization: a }),
            BluetoothAuth::NotDetermined if !self.cfg.allow_permission_prompt => {
                Some(AdapterError::PermissionNotDetermined)
            }
            _ => None,
        };
        let mut phones = Vec::new();
        if blocked.is_none() {
            match self.native.paired_phones() {
                Ok(p) => phones = p,
                Err(e) => blocked = Some(e.into()),
            }
        }
        let ag_capable = phones
            .iter()
            .filter(|p| p.paired && p.hands_free_gateway == AgClass::Advertised)
            .count();
        ProbeReport {
            protocol: PROTOCOL_VERSION,
            authorization,
            blocked,
            phones,
            ag_capable,
            capabilities: caps::unprobed(authorization),
        }
    }

    fn check_host(&self) -> Result<(), AdapterError> {
        if self.native.host_supported() {
            Ok(())
        } else {
            Err(AdapterError::UnsupportedHost {
                os: std::env::consts::OS,
            })
        }
    }

    fn check_auth(&mut self) -> Result<(), AdapterError> {
        self.check_host()?;
        self.auth = self.native.authorization();
        self.caps.authorization = self.auth;
        match self.auth {
            a if a.blocks_use() => Err(AdapterError::PermissionDenied { authorization: a }),
            BluetoothAuth::NotDetermined if !self.cfg.allow_permission_prompt => {
                Err(AdapterError::PermissionNotDetermined)
            }
            _ => Ok(()),
        }
    }

    /// Read-only listing of paired devices. Sends no command and opens no connection.
    pub fn inventory(&mut self) -> Result<Inventory, AdapterError> {
        self.check_auth()?;
        let phones = self.native.paired_phones()?;
        Ok(Inventory {
            authorization: self.auth,
            phones,
        })
    }

    /// Start connecting to the explicitly selected phone. Completion is reported by
    /// [`AdapterEvent::Link`]; the link is usable only after `Up`.
    pub fn connect(&mut self, address: &BtAddress) -> Result<Generation, AdapterError> {
        if let Some(l) = &self.link {
            return Err(AdapterError::LinkBusy {
                generation: l.generation,
            });
        }
        if self.generations_used >= self.cfg.max_generations {
            return Err(AdapterError::TooManyLinks);
        }
        self.check_auth()?;
        let phones = self.native.paired_phones()?;
        let phone = phones
            .iter()
            .find(|p| &p.address == address)
            .ok_or(AdapterError::UnknownPhone)?;
        if !phone.paired {
            return Err(AdapterError::NotPaired);
        }
        if phone.hands_free_gateway != AgClass::Advertised && !self.cfg.allow_unadvertised {
            return Err(AdapterError::NotHandsFreeGateway);
        }
        self.generations_used += 1;
        let generation = Generation(self.generations_used);
        let opened = self
            .native
            .open_link(address, generation, self.tx.clone())?;
        let now = self.clock.now();
        // A new generation starts from a clean core state; stale calls must not leak across links.
        self.core = CallState::default();
        self.proof = AudioProof::None;
        self.link = Some(Link {
            generation,
            address: address.clone(),
            phase: LinkPhase::Connecting,
            deadline: now.saturating_add(self.cfg.connect_timeout_ms),
            tracker: CallTracker::new(format!("mac-g{}", generation.0)),
            evidence: HfpEvidence::default(),
            ag: AgFeatures(0),
            phone: PhoneStatus::default(),
            sco_open: false,
            audio_strategy: None,
            pending: Vec::new(),
            backlog: Vec::new(),
            last_list_request: None,
            caller_id_seen: false,
        });
        self.out.push_back(AdapterEvent::Link {
            generation,
            notice: LinkNotice::Connecting,
        });
        if let Some(default_hf_features) = opened.default_hf_features {
            self.out.push_back(AdapterEvent::Diagnostic {
                diag: Diag::HostFeatureDefault {
                    default_hf_features,
                },
            });
        }
        Ok(generation)
    }

    /// Orderly disconnect of the selected phone. The link is `Down` once the framework confirms
    /// or [`Config::close_timeout_ms`] elapses, whichever comes first.
    pub fn disconnect(&mut self) -> Result<(), AdapterError> {
        let now = self.clock.now();
        let close_timeout = self.cfg.close_timeout_ms;
        let Some(link) = self.link.as_mut() else {
            return Err(AdapterError::NoActiveLink);
        };
        if link.phase == LinkPhase::Closing {
            return Ok(());
        }
        let generation = link.generation;
        link.phase = LinkPhase::Closing;
        link.deadline = now.saturating_add(close_timeout);
        self.out.push_back(AdapterEvent::Link {
            generation,
            notice: LinkNotice::Closing,
        });
        if let Err(e) = self.native.close_link(generation) {
            self.out.push_back(AdapterEvent::Diagnostic {
                diag: Diag::NativeFailure { op: e.op },
            });
            // The framework never heard us; do not wait for a callback that will not come.
            self.finalize(DownReason::Requested);
        }
        Ok(())
    }

    // ------------------------------------------------------------ commands

    pub fn execute_core(&mut self, command: &CallCommand) -> Result<Ticket, AdapterError> {
        let mapped = MacCommand::from_core(command)?;
        self.execute(mapped)
    }

    pub fn execute(&mut self, command: MacCommand) -> Result<Ticket, AdapterError> {
        let now = self.clock.now();
        let cfg = self.cfg;
        let link = match &self.link {
            None => return Err(AdapterError::NoActiveLink),
            Some(l) if l.phase != LinkPhase::Up => {
                return Err(if l.phase == LinkPhase::Connecting {
                    AdapterError::LinkNotReady
                } else {
                    AdapterError::NoActiveLink
                });
            }
            Some(l) => l,
        };
        let kind = command.kind();
        if kind != OpKind::SendTones
            && link
                .pending
                .iter()
                .any(|p| p.kind.is_audio() == kind.is_audio())
        {
            return Err(AdapterError::Busy);
        }
        // Core pre-flight: capability support and state preconditions the core model knows.
        if let Some(core_cmd) = to_core(&command) {
            self.core.check_command(&self.caps.core, &core_cmd)?;
        } else if matches!(kind, OpKind::Hold | OpKind::Resume) {
            let s = self.caps.core.support(CallFeature::SwapCalls);
            match s {
                Support::Unknown => {
                    return Err(bridge_core::calls::CommandRejection::NotProbed.into());
                }
                Support::Unsupported { reason } => {
                    return Err(bridge_core::calls::CommandRejection::Unsupported { reason }.into());
                }
                Support::Declared | Support::Verified => {}
            }
        }
        let input = PlanInput {
            slots: link.tracker.slots(),
            identity_uncertain: link.tracker.identity_uncertain(),
            indicators_observed: link.tracker.indicators_observed(),
            ag: link.ag,
            audio: self.core.audio,
            last_audio_strategy: link.audio_strategy,
        };
        let planned = plan(&command, &input)?;
        let generation = link.generation;
        self.native.perform(generation, &planned.op).map_err(|e| {
            self.out.push_back(AdapterEvent::Diagnostic {
                diag: Diag::NativeFailure { op: e.op.clone() },
            });
            AdapterError::from(e)
        })?;

        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        let Some(link) = self.link.as_mut() else {
            return Ok(ticket);
        };
        if planned.expect == Expect::Unobservable {
            self.out.push_back(AdapterEvent::Command {
                ticket,
                op: kind,
                outcome: Outcome::Dispatched,
            });
            return Ok(ticket);
        }
        if let MacCommand::ConnectAudio { strategy } = &command {
            link.audio_strategy = Some(*strategy);
            self.apply_core(CallEvent::Audio {
                state: AudioLinkState::Connecting,
            });
        }
        let timeout = match kind {
            OpKind::Dial => cfg.dial_timeout_ms,
            k if k.is_audio() => cfg.audio_timeout_ms,
            _ => cfg.command_timeout_ms,
        };
        if let Some(link) = self.link.as_mut() {
            link.pending.push(Pending {
                ticket,
                kind,
                expect: planned.expect,
                deadline: now.saturating_add(timeout),
                issued: now,
                retry_safe: !planned.not_idempotent,
            });
        }
        Ok(ticket)
    }

    // ------------------------------------------------------------ audio routing (explicit only)

    /// Enumerate CoreAudio endpoints (read-only).
    pub fn audio_devices(&mut self) -> Result<Vec<crate::port::AudioDevice>, AdapterError> {
        let devices = self.routes.devices()?;
        self.routing_support = Support::Declared;
        self.refresh_proof_from_devices(&devices);
        self.refresh_caps();
        Ok(devices)
    }

    /// Move the system default input/output to the named endpoints for this call session.
    /// Requires an established phone link so the change can be tied to, and undone with, it.
    pub fn route_apply(&mut self, request: &RouteRequest) -> Result<RouteReport, AdapterError> {
        match self.link.as_ref().map(|l| l.phase) {
            Some(LinkPhase::Up) => {}
            Some(LinkPhase::Connecting) => return Err(AdapterError::LinkNotReady),
            _ => return Err(AdapterError::NoActiveLink),
        }
        let report = self.routes.apply(request)?;
        self.routing_support = Support::Declared;
        self.proof = self.proof.max(AudioProof::RouteApplied);
        self.out.push_back(AdapterEvent::RouteApplied {
            report: report.clone(),
        });
        self.refresh_caps();
        Ok(report)
    }

    /// Undo the route this controller applied (a no-op error if it owns none).
    pub fn route_restore(&mut self) -> Result<RestoreReport, AdapterError> {
        let report = self.routes.restore()?;
        if !report.complete() {
            self.out.push_back(AdapterEvent::Diagnostic {
                diag: Diag::RouteRestoreFailed,
            });
        }
        self.out.push_back(AdapterEvent::RouteRestored {
            report: report.clone(),
        });
        Ok(report)
    }

    fn refresh_proof_from_devices(&mut self, devices: &[crate::port::AudioDevice]) {
        let sco_open = self.link.as_ref().is_some_and(|l| l.sco_open);
        if sco_open && devices.iter().any(|d| d.is_duplex_bluetooth()) {
            self.proof = self.proof.max(AudioProof::RouteDeviceVisible);
        }
    }

    // ------------------------------------------------------------ pumping

    /// Drain queued native callbacks, then advance timers.
    pub fn pump(&mut self) {
        while let Ok(tagged) = self.rx.try_recv() {
            self.ingest(tagged);
        }
        self.tick();
    }

    /// Process one native callback. Callbacks from any generation but the active one are stale.
    pub fn ingest(&mut self, tagged: Tagged) {
        let Some(link) = self.link.as_mut() else {
            self.stats.stale_events += 1;
            return;
        };
        if tagged.generation != link.generation {
            self.stats.stale_events += 1;
            return;
        }
        let now = self.clock.now();
        let phase = link.phase;
        match tagged.event {
            NativeEvent::SlcConnected(status) => self.on_slc_connected(status),
            NativeEvent::SlcDisconnected(status) => {
                let reason = match phase {
                    LinkPhase::Closing => DownReason::Requested,
                    LinkPhase::Connecting => DownReason::ConnectFailed {
                        status: status.0,
                        permission_suspected: status.suggests_permission_denied(),
                    },
                    LinkPhase::Up => DownReason::Lost { status: status.0 },
                };
                self.finalize(reason);
            }
            // While closing, only the disconnect callback matters.
            _ if phase == LinkPhase::Closing => {}
            NativeEvent::ScoOpened(status) => self.on_sco_opened(status),
            NativeEvent::ScoClosed(_) => self.on_sco_closed(),
            NativeEvent::Indicator(ind) => self.on_indicator(ind, now),
            NativeEvent::CallerId(n) => {
                link.caller_id_seen = n.is_some() || link.caller_id_seen;
                let events = link.tracker.on_caller_id(n, now);
                self.apply_derived(events);
                self.refresh_caps();
            }
            NativeEvent::Ring => {}
            NativeEvent::CurrentCall(entry) => {
                let events = link.tracker.on_list_entry(&entry);
                self.apply_derived(events);
            }
            NativeEvent::UnhandledResult(text) => {
                self.stats.unhandled_results += 1;
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::UnhandledResult {
                        len: text.expose().len(),
                    },
                });
            }
            NativeEvent::Malformed(what) => {
                self.stats.malformed_events += 1;
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::Malformed { what },
                });
            }
        }
    }

    /// Advance timers: connect/close deadlines, deferred call endings, command confirmation.
    pub fn tick(&mut self) {
        let now = self.clock.now();
        let Some(link) = self.link.as_mut() else {
            return;
        };
        match link.phase {
            LinkPhase::Connecting if now >= link.deadline => {
                let g = link.generation;
                // Best effort: the attempt may still complete in the framework; retiring the
                // delegate makes sure any late callback is discarded.
                let _ = self.native.close_link(g);
                self.finalize(DownReason::ConnectTimeout);
                return;
            }
            LinkPhase::Closing if now >= link.deadline => {
                self.finalize(DownReason::CloseTimeout);
                return;
            }
            _ => {}
        }
        let events = link.tracker.tick(now);
        self.apply_derived(events);
        self.expire_pending(now);
    }

    // ------------------------------------------------------------ handlers

    fn on_slc_connected(&mut self, status: IoStatus) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        match link.phase {
            LinkPhase::Up => {
                self.stats.duplicate_connected += 1;
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::DuplicateConnected,
                });
                return;
            }
            LinkPhase::Closing => return,
            LinkPhase::Connecting => {}
        }
        if !status.is_success() {
            self.finalize(DownReason::ConnectFailed {
                status: status.0,
                permission_suspected: status.suggests_permission_denied(),
            });
            return;
        }
        let generation = link.generation;
        link.phase = LinkPhase::Up;
        link.evidence.service_level_connection = true;
        link.evidence.host_call_control_api = true;
        link.evidence.host_audio_control_api = true;
        match self.native.gateway_features(generation) {
            Ok(f) => {
                if let Some(l) = self.link.as_mut() {
                    l.ag = f;
                    l.evidence.three_way_calling = f.has(AgFeatures::THREE_WAY_CALLING);
                }
            }
            Err(e) => self.out.push_back(AdapterEvent::Diagnostic {
                diag: Diag::NativeFailure { op: e.op },
            }),
        }
        self.apply_core(CallEvent::Link {
            state: LinkState::Up,
        });
        self.out.push_back(AdapterEvent::Link {
            generation,
            notice: LinkNotice::Up,
        });
        let backlog = self
            .link
            .as_mut()
            .map(|l| std::mem::take(&mut l.backlog))
            .unwrap_or_default();
        for ev in backlog {
            self.apply_core(ev);
        }
        self.refresh_caps();
        self.evaluate_pending();
    }

    fn on_indicator(&mut self, ind: Indicator, now: Millis) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        match ind {
            Indicator::Service(v) => link.phone.service = Some(v),
            Indicator::Signal(v) => link.phone.signal = Some(v),
            Indicator::Roam(v) => link.phone.roaming = Some(v),
            Indicator::Battery(v) => link.phone.battery = Some(v),
            _ => {}
        }
        let events = link.tracker.on_indicator(ind, now);
        self.apply_derived(events);
        self.refresh_caps();
    }

    fn on_sco_opened(&mut self, status: IoStatus) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if !status.is_success() {
            link.sco_open = false;
            self.resolve_where(
                |p| matches!(p.expect, Expect::ScoOpened),
                Outcome::AudioFailed { status: status.0 },
            );
            self.apply_core(CallEvent::Audio {
                state: AudioLinkState::Disconnected,
            });
            return;
        }
        link.sco_open = true;
        // The framework reports no codec; do not guess one.
        link.evidence.audio_codec = Some(AudioCodec::Unknown);
        self.proof = self.proof.max(AudioProof::ScoLinkReported);
        self.apply_core(CallEvent::Audio {
            state: AudioLinkState::Connected {
                codec: AudioCodec::Unknown,
            },
        });
        self.refresh_caps();
        self.evaluate_pending();
    }

    fn on_sco_closed(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        link.sco_open = false;
        self.apply_core(CallEvent::Audio {
            state: AudioLinkState::Disconnected,
        });
        self.evaluate_pending();
    }

    // ------------------------------------------------------------ plumbing

    /// Apply tracker-derived events, buffering them until the service-level connection is up.
    fn apply_derived(&mut self, events: Vec<CallEvent>) {
        if events.is_empty() {
            return;
        }
        let up = self.link.as_ref().is_some_and(|l| l.phase == LinkPhase::Up);
        if !up {
            if let Some(l) = self.link.as_mut() {
                l.backlog.extend(events);
            }
            return;
        }
        for ev in events {
            self.apply_core(ev);
        }
        self.evaluate_pending();
        self.maybe_request_call_list();
    }

    fn apply_core(&mut self, event: CallEvent) {
        match self.core.apply(event.clone()) {
            Applied::Changed => self.out.push_back(AdapterEvent::Call { event }),
            Applied::Unchanged => {}
            // A stray event for a link the core already considers down carries no information.
            Applied::Rejected(EventRejection::LinkDown) => {}
            Applied::Rejected(why) => {
                self.stats.core_rejections += 1;
                if let Some(l) = self.link.as_mut() {
                    l.tracker.mark_uncertain();
                }
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::CoreRejected {
                        rejection: rejection_name(why),
                    },
                });
            }
        }
    }

    fn maybe_request_call_list(&mut self) {
        let now = self.clock.now();
        let interval = self.cfg.call_list_interval_ms;
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let wants = link.phase == LinkPhase::Up
            && self.core.calls.values().any(|c| c.party.number.is_none());
        let due = link
            .last_list_request
            .is_none_or(|t| now.since(t) >= interval);
        if wants && due {
            link.last_list_request = Some(now);
            let g = link.generation;
            if let Err(e) = self.native.request_call_list(g) {
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::NativeFailure { op: e.op },
                });
            }
        }
    }

    fn evaluate_pending(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let slots = link.tracker.slots().clone();
        let sco = link.sco_open;
        let mut done = Vec::new();
        link.pending.retain(|p| {
            if p.expect.satisfied(&slots, sco) {
                done.push((p.ticket, p.kind));
                false
            } else {
                true
            }
        });
        for (ticket, op) in done {
            self.out.push_back(AdapterEvent::Command {
                ticket,
                op,
                outcome: Outcome::Confirmed,
            });
        }
    }

    fn expire_pending(&mut self, now: Millis) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let mut expired = Vec::new();
        link.pending.retain(|p| {
            if now >= p.deadline {
                expired.push((p.ticket, p.kind, now.since(p.issued), p.retry_safe));
                false
            } else {
                true
            }
        });
        for (ticket, op, after_ms, retry_safe) in expired {
            if op == OpKind::ConnectAudio && self.core.audio == AudioLinkState::Connecting {
                self.apply_core(CallEvent::Audio {
                    state: AudioLinkState::Disconnected,
                });
            }
            self.out.push_back(AdapterEvent::Command {
                ticket,
                op,
                outcome: Outcome::NotObserved {
                    after_ms,
                    retry_safe,
                },
            });
        }
    }

    fn resolve_where(&mut self, pick: impl Fn(&Pending) -> bool, outcome: Outcome) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        let mut done = Vec::new();
        link.pending.retain(|p| {
            if pick(p) {
                done.push((p.ticket, p.kind));
                false
            } else {
                true
            }
        });
        for (ticket, op) in done {
            self.out.push_back(AdapterEvent::Command {
                ticket,
                op,
                outcome: outcome.clone(),
            });
        }
    }

    fn refresh_caps(&mut self) {
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let mut evidence = link.evidence.clone();
        evidence.call_indicators = link.tracker.indicators_observed();
        evidence.caller_id_notification = link.caller_id_seen;
        let next = caps::derive(
            &evidence,
            link.ag,
            self.routing_support,
            self.proof,
            self.auth,
        );
        if next != self.caps {
            self.caps = next.clone();
            self.out.push_back(AdapterEvent::Capabilities {
                capabilities: Box::new(next),
            });
        }
    }

    /// End the link of the current generation, whatever state it is in.
    fn finalize(&mut self, reason: DownReason) {
        let Some(link) = self.link.take() else {
            return;
        };
        // Stop forwarding callbacks first; anything already queued is dropped as stale.
        self.native.retire_link(link.generation);
        for p in link.pending {
            self.out.push_back(AdapterEvent::Command {
                ticket: p.ticket,
                op: p.kind,
                outcome: Outcome::Aborted {
                    reason: reason.clone(),
                },
            });
        }
        // Link loss ends every call and the audio link in the core model. The link just left the
        // `Up` phase, so a direct fold is used.
        let was_up = self.core.link == LinkState::Up;
        if was_up {
            self.apply_core(CallEvent::Link {
                state: LinkState::Down,
            });
        }
        self.out.push_back(AdapterEvent::Link {
            generation: link.generation,
            notice: LinkNotice::Down(reason),
        });
        if self.routes.has_owned_changes()
            && let Ok(report) = self.routes.restore()
        {
            if !report.complete() {
                self.out.push_back(AdapterEvent::Diagnostic {
                    diag: Diag::RouteRestoreFailed,
                });
            }
            self.out.push_back(AdapterEvent::RouteRestored { report });
        }
        self.proof = AudioProof::None;
        self.caps = caps::unprobed(self.auth);
        self.out.push_back(AdapterEvent::Capabilities {
            capabilities: Box::new(self.caps.clone()),
        });
        let _ = link.address;
    }
}

/// Core command equivalent of an adapter command, where one exists.
fn to_core(cmd: &MacCommand) -> Option<CallCommand> {
    Some(match cmd {
        MacCommand::Dial { number } => CallCommand::Dial {
            number: number.clone(),
        },
        MacCommand::Answer { call } => CallCommand::Answer { call: call.clone() },
        MacCommand::Hangup { call } => CallCommand::Hangup { call: call.clone() },
        MacCommand::Swap => CallCommand::SwapCalls,
        MacCommand::SendTones { tones } => CallCommand::SendTones {
            tones: tones.clone(),
        },
        MacCommand::ConnectAudio { .. } => CallCommand::ConnectAudio,
        MacCommand::DisconnectAudio => CallCommand::DisconnectAudio,
        MacCommand::Hold | MacCommand::Resume => return None,
    })
}

fn rejection_name(r: EventRejection) -> &'static str {
    match r {
        EventRejection::LinkDown => "link_down",
        EventRejection::FieldTooLong => "field_too_long",
        EventRejection::TooManyCalls => "too_many_calls",
        EventRejection::UnknownCall => "unknown_call",
        EventRejection::InvalidTransition => "invalid_transition",
        EventRejection::InconsistentDirection => "inconsistent_direction",
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
