//! The calling engine: selection, refresh, gating and command dispatch over a [`CallsPlatform`].
//!
//! The engine is single-threaded (`&mut self`); [`crate::service`] runs it on one actor thread
//! with the COM apartment. Rules it enforces:
//!
//! * **Refresh is read-only.** [`Engine::refresh`] and selection call only the read methods of the
//!   port. Nothing registers, connects or asks for access without an explicit call to
//!   [`Engine::request_access`], [`Engine::register`], [`Engine::connect`],
//!   [`Engine::unregister`] or [`Engine::execute`].
//! * **Refuse before touching Windows.** Every explicit action first re-reads state and refuses with
//!   a typed [`Refusal`] if the environment, device or call does not allow it. A refusal means no
//!   mutating port method was called.
//! * **No stale identities.** Each device carries an epoch that changes whenever its line
//!   disappears, is replaced or the phone leaves the transport list. A [`CallRef`] records the
//!   session, device, epoch and Windows call id it was issued for and is refused when any differ or
//!   the call is no longer in a fresh listing.
//! * **Timeouts are uncertain, not failures.** When a deadline passes during a mutating operation
//!   the result is [`Outcome::Uncertain`]; the engine never retries a mutation.
//! * **Other apps and phones are left alone.** The engine only unregisters the explicitly selected
//!   device, and only when asked.
use crate::model::*;
use crate::platform::{CallsPlatform, Deadline};
use crate::project::*;
use crate::readiness::*;
use bridge_core::calls::{
    CallCapabilities, CallCommand, CallEvent, CallId, CallPhase, CommandRejection, Sensitive,
    UnsupportedReason,
};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::Duration;

/// Consecutive refreshes with unreadable state before the projector reports the link down.
const UNKNOWN_STREAK_LIMIT: u32 = 3;
/// Most core events buffered between [`Engine::take_core_events`] calls.
const MAX_PENDING_EVENTS: usize = 256;

// ------------------------------------------------------------------ configuration

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Budget for any single port read.
    pub step: Duration,
    /// Budget for one whole refresh.
    pub refresh: Duration,
    /// `RequestAccessAsync`, which may wait on the operating system.
    pub access: Duration,
    /// `ConnectAsync`.
    pub connect: Duration,
    /// One call operation or dial.
    pub command: Duration,
}
impl Default for Timeouts {
    fn default() -> Self {
        Self {
            step: Duration::from_secs(5),
            refresh: Duration::from_secs(20),
            access: Duration::from_secs(60),
            connect: Duration::from_secs(45),
            command: Duration::from_secs(20),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub policy: Policy,
    pub timeouts: Timeouts,
    /// Identifies this engine instance inside every [`CallRef`], so references from an earlier
    /// process or restart are refused.
    pub session: u64,
}
impl Default for EngineConfig {
    fn default() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self {
            policy: Policy::default(),
            timeouts: Timeouts::default(),
            session: nanos ^ (u64::from(std::process::id()) << 32),
        }
    }
}

// ------------------------------------------------------------------ views

/// A reference to one call on one device epoch. Only the engine creates these.
#[derive(Clone, PartialEq, Eq)]
pub struct CallRef {
    session: u64,
    device: TransportDeviceId,
    epoch: u64,
    call: NativeCallId,
}
impl CallRef {
    pub fn device(&self) -> &TransportDeviceId {
        &self.device
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn native_id(&self) -> &NativeCallId {
        &self.call
    }
    pub fn fingerprint(&self) -> String {
        self.call.fingerprint()
    }
}
impl fmt::Debug for CallRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CallRef(call #{} on device #{} epoch {})",
            self.call.fingerprint(),
            self.device.fingerprint(),
            self.epoch
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallView {
    pub reference: CallRef,
    pub record: CallRecord,
    /// Identifier in the `bridge-core` call domain.
    pub core_id: CallId,
    pub phase: Option<CallPhase>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceView {
    pub id: TransportDeviceId,
    pub epoch: u64,
    pub selected: bool,
    pub name: Option<Sensitive>,
    pub address: Option<Sensitive>,
    pub enabled: bool,
    pub info: Step<TransportInfo>,
    pub line: Option<LineRecord>,
    /// More than one visible line carries this transport id; the dialable one was chosen.
    pub line_ambiguous: bool,
    pub calls: Vec<CallView>,
    /// Reading calls failed: `calls` is empty because it is unknown, not because there are none.
    pub calls_error: Option<PlatformError>,
    pub access: AccessState,
    pub connect: ConnectHistory,
    pub readiness: Readiness,
}
impl DeviceView {
    pub fn call(&self, reference: &CallRef) -> Option<&CallView> {
        self.calls.iter().find(|c| &c.reference == reference)
    }
    fn state_unknown(&self) -> bool {
        self.calls_error.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Increases with every refresh of this engine.
    pub seq: u64,
    pub env: Step<EnvironmentFacts>,
    pub env_blockers: Vec<Blocker>,
    pub devices: Vec<DeviceView>,
    pub transports_error: Option<PlatformError>,
    pub lines_error: Option<PlatformError>,
    /// More paired phones than [`MAX_DEVICES`] exist; the rest are not tracked.
    pub truncated: bool,
}
impl Snapshot {
    pub fn device(&self, id: &TransportDeviceId) -> Option<&DeviceView> {
        self.devices.iter().find(|d| &d.id == id)
    }
    pub fn selected(&self) -> Option<&DeviceView> {
        self.devices.iter().find(|d| d.selected)
    }
}

// ------------------------------------------------------------------ commands and results

/// A typed request to act on the selected device. Constructing one has no effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WinCommand {
    /// Place a call. `route_to_local` additionally asks Windows to move the call audio to this
    /// computer right after the dial is accepted (a separate, separately reported operation).
    Dial {
        number: Sensitive,
        route_to_local: bool,
    },
    Answer(CallRef),
    Reject(CallRef),
    End(CallRef),
    Hold(CallRef),
    Resume(CallRef),
    SetMute {
        call: CallRef,
        muted: bool,
    },
    SendTones {
        call: CallRef,
        tones: Sensitive,
    },
    RouteAudio {
        call: CallRef,
        to: AudioDevice,
    },
}
impl WinCommand {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Dial { .. } => "dial",
            Self::Answer(_) => "answer",
            Self::Reject(_) => "reject",
            Self::End(_) => "end",
            Self::Hold(_) => "hold",
            Self::Resume(_) => "resume",
            Self::SetMute { muted: true, .. } => "mute",
            Self::SetMute { muted: false, .. } => "unmute",
            Self::SendTones { .. } => "send_tones",
            Self::RouteAudio { .. } => "route_audio",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertainReason {
    /// This crate's deadline passed. The operation may still complete.
    DeadlineExceeded,
    /// Windows itself reported `TimedOut`.
    OsTimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum OpFailure {
    /// Windows answered with a failure status.
    Status { status: OperationStatus },
    /// The call into Windows failed.
    Platform { error: PlatformError },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Outcome {
    /// Windows reported success. This is the OS verdict, not a confirmed effect on the phone.
    Succeeded,
    Failed {
        failure: OpFailure,
    },
    /// The effect is unknown; re-read state before acting again.
    Uncertain {
        reason: UncertainReason,
    },
}
impl Outcome {
    fn classify(result: Step<OperationStatus>) -> Self {
        match result {
            Ok(OperationStatus::Succeeded) => Self::Succeeded,
            Ok(OperationStatus::TimedOut) => Self::Uncertain {
                reason: UncertainReason::OsTimedOut,
            },
            Ok(status) => Self::Failed {
                failure: OpFailure::Status { status },
            },
            Err(e) if e.is_timeout() => Self::Uncertain {
                reason: UncertainReason::DeadlineExceeded,
            },
            Err(error) => Self::Failed {
                failure: OpFailure::Platform { error },
            },
        }
    }
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandReport {
    pub action: &'static str,
    pub outcome: Outcome,
    /// For tone sequences, how many tones Windows accepted before the outcome.
    pub steps_done: usize,
    /// The call created by a dial, when Windows returned one.
    pub dialed: Option<CallRef>,
    /// Result of the audio-route step of a dial with `route_to_local`.
    pub route: Option<Outcome>,
    /// The targeted call as re-read after the operation, if it still exists.
    pub after: Option<CallView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterOutcome {
    AlreadyRegistered,
    /// `RegisterApp` returned and `IsRegistered` now reads true.
    Registered,
    /// `RegisterApp` returned but registration could not be confirmed.
    NotConfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnregisterOutcome {
    WasNotRegistered,
    Unregistered,
    NotConfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectReport {
    pub history: ConnectHistory,
    /// After the attempt, a line for this transport is visible. The only evidence of a link that
    /// Windows offers.
    pub line_visible: bool,
}

/// Why an action was refused before anything was sent to Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    Blocked {
        blocker: Blocker,
    },
    NoDeviceSelected,
    /// The selected device is not in the current transport list.
    UnknownDevice,
    /// The reference belongs to another session, device epoch or a call that is gone.
    StaleCall,
    InvalidState {
        status: NativeCallStatus,
    },
    HoldUnsupported,
    InvalidNumber,
    InvalidTones,
    /// DTMF `A`-`D` have no Windows key.
    UnsupportedTone,
    /// Windows reports `CannotRouteToLocalDevice` (for example a Bluetooth headset holds the
    /// audio), so moving call audio to this computer is not attempted.
    AudioRouteBlocked,
    /// The core command was refused by the core call model.
    Core {
        rejection: CommandRejection,
    },
    /// The core command needs a call that is not present.
    NoMatchingCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "category", rename_all = "snake_case")]
pub enum CallsError {
    /// Nothing was sent to Windows.
    Refused { refusal: Refusal },
    /// A call into Windows failed. For mutating steps that timed out see [`Outcome::Uncertain`];
    /// this variant is used for definite failures and for reads.
    Platform { error: PlatformError },
}
impl From<Refusal> for CallsError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused { refusal }
    }
}
impl From<PlatformError> for CallsError {
    fn from(error: PlatformError) -> Self {
        Self::Platform { error }
    }
}
impl fmt::Display for CallsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { refusal } => write!(f, "refused: {refusal:?}"),
            Self::Platform { error } => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for CallsError {}

// ------------------------------------------------------------------ engine

struct Record {
    epoch: u64,
    line_id: Option<LineId>,
    access: AccessState,
    connect: ConnectHistory,
}

pub struct Engine<P> {
    platform: P,
    cfg: EngineConfig,
    seq: u64,
    epoch_counter: u64,
    records: BTreeMap<String, Record>,
    selected: Option<TransportDeviceId>,
    last: Option<Snapshot>,
    projector: CoreProjector,
    pending: VecDeque<CallEvent>,
    events_dropped: u32,
    unknown_streak: u32,
}

impl<P: CallsPlatform> Engine<P> {
    pub fn new(platform: P, cfg: EngineConfig) -> Self {
        Self {
            platform,
            cfg,
            seq: 0,
            epoch_counter: 0,
            records: BTreeMap::new(),
            selected: None,
            last: None,
            projector: CoreProjector::default(),
            pending: VecDeque::new(),
            events_dropped: 0,
            unknown_streak: 0,
        }
    }

    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }
    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }
    pub fn last_snapshot(&self) -> Option<&Snapshot> {
        self.last.as_ref()
    }
    pub fn selected_id(&self) -> Option<&TransportDeviceId> {
        self.selected.as_ref()
    }
    pub fn core_state(&self) -> &bridge_core::calls::CallState {
        self.projector.state()
    }
    /// Core events dropped because they were not drained (a consumer must resynchronise).
    pub fn events_dropped(&self) -> u32 {
        self.events_dropped
    }

    fn next_epoch(&mut self) -> u64 {
        self.epoch_counter += 1;
        self.epoch_counter
    }

    // ---- read-only ---------------------------------------------------------------------------

    /// Re-read everything. Read-only; the default diagnose path.
    pub fn refresh(&mut self) -> Snapshot {
        let deadline = Deadline::after(self.cfg.timeouts.refresh);
        let step = self.cfg.timeouts.step;
        let env = self.platform.environment(deadline.capped(step));
        let env_blockers = match &env {
            Ok(facts) => environment_blockers(facts, &self.cfg.policy),
            Err(e) => vec![Blocker::EnvironmentUnreadable { kind: e.kind }],
        };
        let skip_devices = env_blockers.iter().any(|b| {
            matches!(
                b,
                Blocker::NotWindows
                    | Blocker::EnvironmentUnreadable { .. }
                    | Blocker::UnsupportedOs { .. }
                    | Blocker::ContractMissing { .. }
            )
        });
        let mut snapshot = Snapshot {
            seq: 0,
            env: env.clone(),
            env_blockers,
            devices: Vec::new(),
            transports_error: None,
            lines_error: None,
            truncated: false,
        };
        if let (false, Ok(facts)) = (skip_devices, &env) {
            self.read_devices(facts, &mut snapshot, deadline);
        } else {
            // APIs absent or unsupported: no device can exist, so forget identities.
            self.records.clear();
        }
        self.seq += 1;
        snapshot.seq = self.seq;
        self.project(&snapshot);
        self.last = Some(snapshot.clone());
        snapshot
    }

    fn read_devices(&mut self, env: &EnvironmentFacts, out: &mut Snapshot, deadline: Deadline) {
        let step = self.cfg.timeouts.step;
        let transports = self.platform.list_transports(deadline.capped(step));
        let lines = self.platform.list_lines(deadline.capped(step));
        let transports = match transports {
            Ok(list) => list,
            Err(e) => {
                out.transports_error = Some(e);
                return;
            }
        };
        if let Err(e) = &lines {
            out.lines_error = Some(*e);
        }
        out.truncated = transports.len() > MAX_DEVICES;

        // A phone that left the transport list takes its identity with it.
        let present: Vec<String> = transports
            .iter()
            .take(MAX_DEVICES)
            .map(|t| t.id.key().to_owned())
            .collect();
        self.records.retain(|key, _| present.contains(key));

        for transport in transports.into_iter().take(MAX_DEVICES) {
            let info = self
                .platform
                .transport_info(&transport.id, deadline.capped(step));
            let mut matching: Vec<&LineRecord> = match &lines {
                Ok(all) => all
                    .iter()
                    .filter(|l| {
                        l.transport == NativeTransport::Bluetooth
                            && l.transport_device_id.as_ref() == Some(&transport.id)
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
            matching.sort_by(|a, b| {
                (b.can_dial == Some(true))
                    .cmp(&(a.can_dial == Some(true)))
                    .then_with(|| a.id.cmp(&b.id))
            });
            let line = matching.first().map(|l| (*l).clone());
            let line_ambiguous = matching.len() > 1;

            let key = transport.id.key().to_owned();
            if !self.records.contains_key(&key) {
                let epoch = self.next_epoch();
                self.records.insert(
                    key.clone(),
                    Record {
                        epoch,
                        line_id: None,
                        access: AccessState::NotRequested,
                        connect: ConnectHistory::NotAttempted,
                    },
                );
            }
            // The line vanished or was replaced: everything issued for the old one is stale.
            // When the listing itself failed nothing is known, so nothing is invalidated.
            if lines.is_ok() {
                let new_line = line.as_ref().map(|l| l.id.clone());
                let changed = self
                    .records
                    .get(&key)
                    .is_some_and(|r| r.line_id.is_some() && r.line_id != new_line);
                if changed {
                    let epoch = self.next_epoch();
                    if let Some(r) = self.records.get_mut(&key) {
                        r.epoch = epoch;
                        r.connect = ConnectHistory::NotAttempted;
                    }
                }
                if let Some(r) = self.records.get_mut(&key) {
                    r.line_id = new_line;
                }
            }
            let (epoch, access, connect) = {
                let r = &self.records[&key];
                (r.epoch, r.access, r.connect)
            };

            let lines_error = lines.as_ref().err().map(|e| e.kind);
            let obs = DeviceObservation {
                enabled: transport.enabled,
                info: &info,
                line: line.as_ref(),
                lines_error,
                access,
            };
            let readiness = assess(env, &self.cfg.policy, Some(&obs));

            let mut calls = Vec::new();
            let mut calls_error = None;
            if let (Some(line), true) = (&line, readiness.control_ready()) {
                match self.platform.list_calls(&line.id, deadline.capped(step)) {
                    Ok(listing) => {
                        for record in listing.calls.into_iter().take(MAX_CALLS_PER_DEVICE) {
                            calls.push(CallView {
                                reference: CallRef {
                                    session: self.cfg.session,
                                    device: transport.id.clone(),
                                    epoch,
                                    call: record.id.clone(),
                                },
                                core_id: core_call_id(epoch, &record.id),
                                phase: None,
                                record,
                            });
                        }
                        let records: Vec<CallRecord> =
                            calls.iter().map(|c| c.record.clone()).collect();
                        for call in &mut calls {
                            call.phase = core_phase(&call.record, &records);
                        }
                    }
                    Err(e) => calls_error = Some(e),
                }
            }

            out.devices.push(DeviceView {
                selected: self.selected.as_ref() == Some(&transport.id),
                id: transport.id,
                epoch,
                name: transport.name,
                address: transport.address,
                enabled: transport.enabled,
                info,
                line,
                line_ambiguous,
                calls,
                calls_error,
                access,
                connect,
                readiness,
            });
        }
    }

    fn project(&mut self, snapshot: &Snapshot) {
        let selected = snapshot.selected();
        let unknown = snapshot.transports_error.is_some()
            || snapshot.lines_error.is_some()
            || selected.is_some_and(DeviceView::state_unknown);
        // State that cannot be read is not state that is gone: hold the last projection for a few
        // refreshes, then report the link down rather than keep showing calls nobody can confirm.
        let mut treat_as_down = false;
        if unknown && self.selected.is_some() {
            self.unknown_streak += 1;
            if self.unknown_streak < UNKNOWN_STREAK_LIMIT {
                return;
            }
            treat_as_down = true;
        } else {
            self.unknown_streak = 0;
        }
        let records: Vec<CallRecord> = selected
            .map(|d| d.calls.iter().map(|c| c.record.clone()).collect())
            .unwrap_or_default();
        let events =
            self.projector.observe(
                selected
                    .filter(|_| !treat_as_down)
                    .map(|d| ProjectedDevice {
                        epoch: d.epoch,
                        link_up: d.readiness.control_ready(),
                        calls: &records,
                    }),
            );
        for event in events {
            if self.pending.len() >= MAX_PENDING_EVENTS {
                self.pending.pop_front();
                self.events_dropped = self.events_dropped.saturating_add(1);
            }
            self.pending.push_back(event);
        }
    }

    /// Drain the `bridge-core` events produced by refreshes since the last call.
    pub fn take_core_events(&mut self) -> Vec<CallEvent> {
        self.pending.drain(..).collect()
    }

    /// Declared capabilities of the selected device from the last refresh. Never `Verified`.
    pub fn capabilities(&self) -> CallCapabilities {
        use bridge_core::calls::{CallBackend, HfpHost};
        let backend = CallBackend::BluetoothHfp {
            host: HfpHost::Windows,
        };
        let Some(snapshot) = &self.last else {
            return CallCapabilities::unprobed(backend);
        };
        if snapshot.env_blockers.iter().any(|b| {
            matches!(
                b,
                Blocker::NotWindows
                    | Blocker::UnsupportedOs { .. }
                    | Blocker::ContractMissing { .. }
            )
        }) {
            return CallCapabilities::unsupported(backend, UnsupportedReason::NoPlatformApi);
        }
        match snapshot.selected() {
            Some(d) => capabilities_for(&d.readiness),
            None => CallCapabilities::unprobed(backend),
        }
    }

    // ---- selection (no effect on Windows) ----------------------------------------------------

    /// Choose the device that later explicit actions apply to. Registers nothing.
    pub fn select(&mut self, id: &TransportDeviceId) -> Result<Snapshot, CallsError> {
        let snapshot = self.refresh();
        if snapshot.device(id).is_none() {
            return Err(Refusal::UnknownDevice.into());
        }
        if self.selected.as_ref() != Some(id) {
            self.selected = Some(id.clone());
            // Start the core state afresh for the new device.
            let events = self.projector.observe(None);
            self.pending.extend(events);
        }
        // Re-run so the snapshot, projection and `selected` flags reflect the choice.
        Ok(self.refresh())
    }

    pub fn deselect(&mut self) {
        self.selected = None;
        let events = self.projector.observe(None);
        self.pending.extend(events);
    }

    // ---- explicit actions on the device ------------------------------------------------------

    fn fresh_selected(&mut self) -> Result<(Snapshot, usize), CallsError> {
        let snapshot = self.refresh();
        if let Some(blocker) = snapshot.env_blockers.first() {
            return Err(Refusal::Blocked { blocker: *blocker }.into());
        }
        let Some(id) = self.selected.clone() else {
            return Err(Refusal::NoDeviceSelected.into());
        };
        let Some(index) = snapshot.devices.iter().position(|d| d.id == id) else {
            return Err(Refusal::UnknownDevice.into());
        };
        Ok((snapshot, index))
    }

    /// Disabled or unreadable devices cannot be acted on at all.
    fn device_unusable(dev: &DeviceView) -> Option<Blocker> {
        dev.readiness.blockers.iter().copied().find(|b| {
            matches!(
                b,
                Blocker::DeviceDisabled | Blocker::DeviceUnreadable { .. }
            )
        })
    }

    fn registered(dev: &DeviceView) -> bool {
        matches!(
            &dev.info,
            Ok(TransportInfo {
                registered: Ok(true),
                ..
            })
        )
    }

    fn set_access(&mut self, id: &TransportDeviceId, state: AccessState) {
        if let Some(r) = self.records.get_mut(id.key()) {
            r.access = state;
        }
    }
    fn set_connect(&mut self, id: &TransportDeviceId, history: ConnectHistory) {
        if let Some(r) = self.records.get_mut(id.key()) {
            r.connect = history;
        }
    }

    /// Explicitly ask Windows for access to the selected device's transport
    /// (`RequestAccessAsync`). Never part of a probe.
    pub fn request_access(&mut self) -> Result<AccessState, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        let dev = &snapshot.devices[index];
        if let Some(blocker) = Self::device_unusable(dev) {
            return Err(Refusal::Blocked { blocker }.into());
        }
        let id = dev.id.clone();
        self.ask_access(&id)
    }

    fn ask_access(&mut self, id: &TransportDeviceId) -> Result<AccessState, CallsError> {
        let deadline = Deadline::after(self.cfg.timeouts.access);
        let result = match self.platform.request_access(id, deadline) {
            Ok(status) => {
                let state = AccessState::from(status);
                self.set_access(id, state);
                Ok(state)
            }
            Err(e) => {
                self.set_access(id, AccessState::Failed(e.kind));
                Err(e.into())
            }
        };
        // Read-only: make the retained snapshot show the access state just learned.
        self.refresh();
        result
    }

    /// Register this app for the selected phone's transport.
    ///
    /// Order: refresh and gate, return early if already registered, `RequestAccessAsync` (must be
    /// `Allowed`, otherwise a typed refusal and `RegisterApp` is *not* called), `RegisterApp`,
    /// then confirm with `IsRegistered`. Other apps' and phones' registrations are untouched.
    pub fn register(&mut self) -> Result<RegisterOutcome, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        let dev = &snapshot.devices[index];
        if let Some(blocker) = Self::device_unusable(dev) {
            return Err(Refusal::Blocked { blocker }.into());
        }
        if Self::registered(dev) {
            return Ok(RegisterOutcome::AlreadyRegistered);
        }
        let id = dev.id.clone();
        let access = self.ask_access(&id)?;
        if access != AccessState::Allowed {
            return Err(Refusal::Blocked {
                blocker: Blocker::AccessNotGranted { access },
            }
            .into());
        }
        self.platform.register(&id)?;
        let deadline = Deadline::after(self.cfg.timeouts.step);
        let confirmed = matches!(
            self.platform.transport_info(&id, deadline),
            Ok(TransportInfo {
                registered: Ok(true),
                ..
            })
        );
        self.refresh();
        Ok(if confirmed {
            RegisterOutcome::Registered
        } else {
            RegisterOutcome::NotConfirmed
        })
    }

    /// Unregister this app from the selected phone only.
    pub fn unregister(&mut self) -> Result<UnregisterOutcome, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        let dev = &snapshot.devices[index];
        if let Some(blocker) = Self::device_unusable(dev) {
            return Err(Refusal::Blocked { blocker }.into());
        }
        if !Self::registered(dev) {
            return Ok(UnregisterOutcome::WasNotRegistered);
        }
        let id = dev.id.clone();
        self.platform.unregister(&id)?;
        let deadline = Deadline::after(self.cfg.timeouts.step);
        let still = matches!(
            self.platform.transport_info(&id, deadline),
            Ok(TransportInfo {
                registered: Ok(true),
                ..
            })
        );
        self.refresh();
        Ok(if still {
            UnregisterOutcome::NotConfirmed
        } else {
            UnregisterOutcome::Unregistered
        })
    }

    /// Ask Windows to connect to the selected, registered phone (`ConnectAsync`). A deadline is
    /// reported as [`ConnectHistory::Uncertain`]; look for the line afterwards.
    pub fn connect(&mut self) -> Result<ConnectReport, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        let dev = &snapshot.devices[index];
        if let Some(blocker) = Self::device_unusable(dev) {
            return Err(Refusal::Blocked { blocker }.into());
        }
        if !Self::registered(dev) {
            return Err(Refusal::Blocked {
                blocker: Blocker::NotRegistered,
            }
            .into());
        }
        let id = dev.id.clone();
        let deadline = Deadline::after(self.cfg.timeouts.connect);
        let history = match self.platform.connect(&id, deadline) {
            Ok(true) => ConnectHistory::ReturnedTrue,
            Ok(false) => ConnectHistory::ReturnedFalse,
            Err(e) if e.is_timeout() => ConnectHistory::Uncertain,
            Err(e) => {
                self.set_connect(&id, ConnectHistory::Failed(e.kind));
                return Err(e.into());
            }
        };
        self.set_connect(&id, history);
        let after = self.refresh();
        let line_visible = after.device(&id).is_some_and(|d| d.line.is_some());
        Ok(ConnectReport {
            history,
            line_visible,
        })
    }

    // ---- call commands -----------------------------------------------------------------------

    /// Execute a typed command against freshly read state.
    pub fn execute(&mut self, command: &WinCommand) -> Result<CommandReport, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        self.dispatch(&snapshot, index, command)
    }

    /// Execute a `bridge-core` command: checked against declared capabilities and the core call
    /// state first, then mapped (see [`plan_core_command`]) and dispatched.
    pub fn execute_core(&mut self, command: &CallCommand) -> Result<CommandReport, CallsError> {
        let (snapshot, index) = self.fresh_selected()?;
        let dev = &snapshot.devices[index];
        let caps = capabilities_for(&dev.readiness);
        self.projector
            .state()
            .check_command(&caps, command)
            .map_err(|rejection| Refusal::Core { rejection })?;
        let records: Vec<CallRecord> = dev.calls.iter().map(|c| c.record.clone()).collect();
        let planned = plan_core_command(command, dev.epoch, &records).map_err(|e| match e {
            PlanError::Rejected(rejection) => Refusal::Core { rejection },
            PlanError::NoMatchingCall => Refusal::NoMatchingCall,
        })?;
        let reference = |id: NativeCallId| CallRef {
            session: self.cfg.session,
            device: dev.id.clone(),
            epoch: dev.epoch,
            call: id,
        };
        let win = match planned {
            PlannedAction::Dial { number } => WinCommand::Dial {
                number: Sensitive::new(number),
                route_to_local: false,
            },
            PlannedAction::SendTones { id, tones } => WinCommand::SendTones {
                call: reference(id),
                tones: Sensitive::new(tones),
            },
            PlannedAction::Call { id, op } => {
                let call = reference(id);
                match op {
                    CallOp::Accept => WinCommand::Answer(call),
                    CallOp::Reject => WinCommand::Reject(call),
                    CallOp::End => WinCommand::End(call),
                    CallOp::Hold => WinCommand::Hold(call),
                    CallOp::Resume => WinCommand::Resume(call),
                    CallOp::Mute => WinCommand::SetMute { call, muted: true },
                    CallOp::Unmute => WinCommand::SetMute { call, muted: false },
                    CallOp::Route(to) => WinCommand::RouteAudio { call, to },
                    CallOp::Dtmf(_) => unreachable!("tones are planned as SendTones"),
                }
            }
        };
        self.dispatch(&snapshot, index, &win)
    }

    /// Validate `reference` against a fresh snapshot and return the matching call.
    fn live_call<'a>(
        &self,
        dev: &'a DeviceView,
        reference: &CallRef,
    ) -> Result<&'a CallView, Refusal> {
        if reference.session != self.cfg.session
            || reference.device != dev.id
            || reference.epoch != dev.epoch
        {
            return Err(Refusal::StaleCall);
        }
        let call = dev.call(reference).ok_or(Refusal::StaleCall)?;
        if !call.record.status.is_live() {
            return Err(Refusal::StaleCall);
        }
        Ok(call)
    }

    fn dispatch(
        &mut self,
        snapshot: &Snapshot,
        index: usize,
        command: &WinCommand,
    ) -> Result<CommandReport, CallsError> {
        let dev = &snapshot.devices[index];
        let action = command.name();
        let timeouts = self.cfg.timeouts;

        // ---- Phase 1: everything that can refuse, before any mutating port call. ----
        enum Plan {
            Dial {
                line: LineId,
                number: String,
                route: bool,
            },
            Op {
                target: CallRef,
                op: CallOp,
            },
            Tones {
                target: CallRef,
                tones: Vec<DtmfTone>,
            },
        }
        let plan = match command {
            WinCommand::Dial {
                number,
                route_to_local,
            } => {
                CallCommand::Dial {
                    number: number.clone(),
                }
                .validate()
                .map_err(|_| Refusal::InvalidNumber)?;
                if let Some(blocker) = dev.readiness.dial_blocker() {
                    return Err(Refusal::Blocked { blocker }.into());
                }
                let line = dev
                    .line
                    .as_ref()
                    .map(|l| l.id.clone())
                    .ok_or(Refusal::Blocked {
                        blocker: Blocker::NoLine,
                    })?;
                if *route_to_local && dev.readiness.audio == AudioReadiness::Blocked {
                    return Err(Refusal::AudioRouteBlocked.into());
                }
                Plan::Dial {
                    line,
                    number: number.expose().to_owned(),
                    route: *route_to_local,
                }
            }
            other => {
                if let Some(blocker) = dev.readiness.control_blocker() {
                    return Err(Refusal::Blocked { blocker }.into());
                }
                let (reference, op) = match other {
                    WinCommand::Answer(r) => (r, CallOp::Accept),
                    WinCommand::Reject(r) => (r, CallOp::Reject),
                    WinCommand::End(r) => (r, CallOp::End),
                    WinCommand::Hold(r) => (r, CallOp::Hold),
                    WinCommand::Resume(r) => (r, CallOp::Resume),
                    WinCommand::SetMute { call, muted } => {
                        (call, if *muted { CallOp::Mute } else { CallOp::Unmute })
                    }
                    WinCommand::RouteAudio { call, to } => (call, CallOp::Route(*to)),
                    WinCommand::SendTones { call, .. } => (call, CallOp::Dtmf(DtmfTone::Star)),
                    WinCommand::Dial { .. } => unreachable!("handled above"),
                };
                let call = self.live_call(dev, reference)?;
                let status = call.record.status;
                let state_ok = match op {
                    CallOp::Accept | CallOp::Reject => status == NativeCallStatus::Incoming,
                    CallOp::End => status != NativeCallStatus::Incoming,
                    CallOp::Hold | CallOp::Mute | CallOp::Unmute | CallOp::Dtmf(_) => {
                        status == NativeCallStatus::Talking
                    }
                    CallOp::Resume => status == NativeCallStatus::Held,
                    CallOp::Route(AudioDevice::Unknown) => false,
                    CallOp::Route(_) => true,
                };
                if !state_ok {
                    return Err(Refusal::InvalidState { status }.into());
                }
                if matches!(op, CallOp::Hold | CallOp::Resume)
                    && call.record.hold_supported == Some(false)
                {
                    return Err(Refusal::HoldUnsupported.into());
                }
                if let CallOp::Route(AudioDevice::Local) = op
                    && dev.readiness.audio == AudioReadiness::Blocked
                {
                    return Err(Refusal::AudioRouteBlocked.into());
                }
                if let WinCommand::SendTones { tones, .. } = other {
                    let text = tones.expose();
                    CallCommand::SendTones {
                        tones: tones.clone(),
                    }
                    .validate()
                    .map_err(|_| Refusal::InvalidTones)?;
                    let mut keys = Vec::new();
                    for c in text.chars() {
                        keys.push(DtmfTone::from_char(c).ok_or(Refusal::UnsupportedTone)?);
                    }
                    Plan::Tones {
                        target: reference.clone(),
                        tones: keys,
                    }
                } else {
                    Plan::Op {
                        target: reference.clone(),
                        op,
                    }
                }
            }
        };

        // ---- Phase 2: dispatch. A mutating port method is called at most once per step. ----
        let report = match plan {
            Plan::Dial {
                line,
                number,
                route,
            } => {
                let deadline = Deadline::after(timeouts.command);
                match self.platform.dial(&line, &number, deadline) {
                    Err(e) => CommandReport {
                        action,
                        outcome: Outcome::classify(Err(e)),
                        steps_done: 0,
                        dialed: None,
                        route: None,
                        after: None,
                    },
                    Ok(result) => {
                        let outcome = Outcome::classify(Ok(result.status));
                        let dialed = result.call.as_ref().map(|c| CallRef {
                            session: self.cfg.session,
                            device: dev.id.clone(),
                            epoch: dev.epoch,
                            call: c.id.clone(),
                        });
                        let route_outcome = match (&dialed, outcome.is_success(), route) {
                            (Some(r), true, true) => {
                                let d = Deadline::after(timeouts.command);
                                Some(Outcome::classify(self.platform.call_op(
                                    &r.call,
                                    CallOp::Route(AudioDevice::Local),
                                    d,
                                )))
                            }
                            _ => None,
                        };
                        CommandReport {
                            action,
                            outcome,
                            steps_done: 0,
                            dialed,
                            route: route_outcome,
                            after: None,
                        }
                    }
                }
            }
            Plan::Op { target, op } => {
                let deadline = Deadline::after(timeouts.command);
                let outcome = Outcome::classify(self.platform.call_op(&target.call, op, deadline));
                CommandReport {
                    action,
                    outcome,
                    steps_done: 0,
                    dialed: None,
                    route: None,
                    after: None,
                }
            }
            Plan::Tones { target, tones } => {
                let deadline = Deadline::after(timeouts.command);
                let mut done = 0;
                let mut outcome = Outcome::Succeeded;
                for tone in &tones {
                    outcome = Outcome::classify(self.platform.call_op(
                        &target.call,
                        CallOp::Dtmf(*tone),
                        deadline.capped(timeouts.step),
                    ));
                    if !outcome.is_success() {
                        break;
                    }
                    done += 1;
                }
                CommandReport {
                    action,
                    outcome,
                    steps_done: done,
                    dialed: None,
                    route: None,
                    after: None,
                }
            }
        };

        // ---- Phase 3: re-read so the result is judged by what Windows now shows. ----
        let after = self.refresh();
        let target = match command {
            WinCommand::Answer(r)
            | WinCommand::Reject(r)
            | WinCommand::End(r)
            | WinCommand::Hold(r)
            | WinCommand::Resume(r)
            | WinCommand::SetMute { call: r, .. }
            | WinCommand::SendTones { call: r, .. }
            | WinCommand::RouteAudio { call: r, .. } => Some(r.clone()),
            WinCommand::Dial { .. } => report.dialed.clone(),
        };
        let after_view = target.and_then(|t| {
            after
                .devices
                .iter()
                .find(|d| d.id == t.device)
                .and_then(|d| d.call(&t).cloned())
        });
        Ok(CommandReport {
            after: after_view,
            ..report
        })
    }
}
