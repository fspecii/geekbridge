// SPDX-License-Identifier: GPL-3.0-or-later
//! Live transport state and the tasks that drive connections on the transport runtime.
//!
//! Everything here is built on `bridge-kde`: it performs the TLS handshake, pins certificates,
//! runs pairing and owns the trust store. This module only decides *when* those run (never before
//! `transport_start`, never on the JNI thread), keeps one connection per device, and holds the
//! prompts a local user must answer. No lock is held across an `.await`.
//!
//! Trust rules enforced here, on top of `bridge-kde`'s own:
//! * a pairing completes only through `bridge-kde`'s `Approver` hook, which resolves only from
//!   [`Core::decide`] (an explicit local decision naming the exact request, connection, certificate
//!   fingerprint and verification code), or as a rejection when the state is torn down;
//! * feature packets are only read from a `TrustedLink`; a pending link is exposed only as a
//!   connection that can pair or be closed;
//! * state is per [`Core`]; `transport_stop` drops it and the runtime, so nothing survives a restart.
use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bridge_kde::tiebreak::{CROSS_DIAL_WINDOW, prefer_new};
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceId, Direction, Endpoint, FileTrustStore,
    Handshake, KdeError, LinkEvent, LinkInfo, Packet, PairMode, PairingFailure, PendingLink, Role,
    TrustStore, TrustedLink,
};
use serde_json::{Map, Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep, timeout};

use bridge_core::heartbeat::{EVERY as HEARTBEAT_EVERY, Heartbeat};
use bridge_kde::share::{SHARE_REQUEST, SHARE_REQUEST_UPDATE};

mod calls;
mod clipboard;
mod data;
mod permits;
mod redial;
mod share;

pub(super) use clipboard::SendArgs as ClipboardSend;

pub(super) use calls::ReportArgs as CallsReport;

pub(super) use data::Flags as DataFlags;
pub(super) use permits::{FEATURES as PERMIT_FEATURES, PermitMap};
pub(crate) use redial::unsuppress;

pub(super) fn data_incoming_types(flags: DataFlags) -> Vec<String> {
    data::incoming_types(flags)
}
pub(super) fn data_outgoing_types(flags: DataFlags) -> Vec<String> {
    data::outgoing_types(flags)
}
pub(super) fn calls_incoming_types() -> Vec<String> {
    calls::incoming_types()
}
pub(super) fn calls_outgoing_types() -> Vec<String> {
    calls::outgoing_types()
}
pub(super) fn clipboard_incoming_types() -> Vec<String> {
    clipboard::incoming_types()
}
pub(super) fn clipboard_outgoing_types() -> Vec<String> {
    clipboard::outgoing_types()
}

use super::bindhealth;
use super::discovery::{self, Discovered};
use crate::dispatch::{ApiError, err};

/// The only feature packet type this build implements, in both directions.
pub(super) const PING: &str = "kdeconnect.ping";

pub(super) const MAX_CONNECTIONS: usize = 16;
pub(super) const MAX_ATTEMPTS: usize = 4;
pub(super) const MAX_UNPAIRED: usize = 8;
const MAX_INBOUND_HANDSHAKES: usize = 8;
const MAX_PROMPTS: usize = 4;
const MAX_EVENTS: usize = 64;
/// `transport_wait` blocks without any timer in between, so its bound can be long: each expiry is a wakeup.
const MAX_EVENT_WAIT: Duration = Duration::from_secs(600);
const MAX_PAIRED_LISTED: usize = 64;
/// Wrong verification codes or certificate echoes tolerated per prompt before it is rejected.
const MAX_WRONG_ATTEMPTS: u8 = 3;
/// A stock peer sends nothing larger than a few hundred bytes for the only packet type we
/// advertise. Longer lines close the connection (`bridge-kde` never resynchronises) instead of
/// letting a peer make the phone buffer the protocol's 32 MiB maximum.
pub(super) const MAX_PACKET_LINE: usize = 64 * 1024;
/// Whole-handshake ceiling on top of `bridge-kde`'s per-stage timeouts.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(20);
/// How long an outbound unpaired connection waits for `transport_pair_request`.
const PARK_TTL: Duration = Duration::from_secs(60);
const CTL_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    /// Outbound, TLS up, peer not trusted. Nothing is read until the UI picks a pairing mode.
    Parked,
    /// We sent a pair request and wait for the peer.
    PairingRequest,
    /// Waiting for the peer to send a pair request.
    PairingAwait,
    /// Peer's exact certificate is pinned; feature packets flow.
    Trusted,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Parked => "parked",
            Phase::PairingRequest => "pairing_request",
            Phase::PairingAwait => "pairing_await",
            Phase::Trusted => "trusted",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Inbound,
    Outbound,
}

/// What a scanned QR code pinned: the computer's certificate and the one-time token to present.
pub(super) struct QrPin {
    pub token: String,
    pub cert_sha256: String,
}

enum Ctl {
    Pair(PairMode, Option<String>),
    Ping,
    Unpair,
    SendFile(Box<share::SendJob>),
    SendText(share::TextJob),
    Clip(Box<clipboard::Out>),
    Calls(calls::Out),
    Data(data::Out),
}

struct ConnEntry {
    id: u64,
    role: Role,
    phase: Phase,
    peer_id: DeviceId,
    peer_name: String,
    peer_type: &'static str,
    peer_cert_sha256: String,
    peer_ip: IpAddr,
    /// The computer declared it accepts `geekbridge.debug.log` (see `data::Feature::Debug`).
    peer_debug: bool,
    /// What the computer declared it accepts. A packet type outside this list is never sent to it: a stock
    /// KDE Connect computer would at best ignore it, and it must not receive a caller number or a calendar
    /// it cannot use.
    peer_accepts: Vec<String>,
    ctl: mpsc::Sender<Ctl>,
    close: Arc<Notify>,
    since: Instant,
    pings_received: u64,
    ignored_packets: u64,
}

struct Attempt {
    id: u64,
    device: DeviceId,
    since: Instant,
}

struct Prompt {
    request_id: u64,
    connection_id: u64,
    direction: Direction,
    peer_id: String,
    peer_name: String,
    peer_cert_sha256: String,
    own_cert_sha256: String,
    code: String,
    timestamp: i64,
    /// The peer's certificate was pinned from a scanned QR code before this prompt.
    qr_pinned: bool,
    deadline: Instant,
    wrong_attempts: u8,
    tx: oneshot::Sender<bool>,
}

struct Event {
    seq: u64,
    kind: &'static str,
    connection_id: Option<u64>,
    device_id: Option<String>,
    code: Option<&'static str>,
    /// Milliseconds since anything was last heard from the computer, on a `link_closed` event.
    quiet_ms: Option<u64>,
}

#[derive(Default)]
struct Counters {
    inbound_dropped: u64,
    inbound_failed: u64,
    rejected_duplicates: u64,
}

struct State {
    /// False once torn down: late tasks can no longer register anything.
    open: bool,
    conns: BTreeMap<u64, ConnEntry>,
    attempts: Vec<Attempt>,
    next_attempt: u64,
    prompts: Vec<Prompt>,
    events: VecDeque<Event>,
    next_seq: u64,
    /// Wakes `transport_wait` when an event is recorded or the transport stops.
    changed: Arc<Condvar>,
    discovered: Vec<Discovered>,
    counters: Counters,
}

impl State {
    fn event(
        &mut self,
        kind: &'static str,
        connection_id: Option<u64>,
        device: Option<&DeviceId>,
        code: Option<&'static str>,
    ) {
        self.event_quiet(kind, connection_id, device, code, None);
    }

    fn event_quiet(
        &mut self,
        kind: &'static str,
        connection_id: Option<u64>,
        device: Option<&DeviceId>,
        code: Option<&'static str>,
        quiet_ms: Option<u64>,
    ) {
        if !self.open {
            return;
        }
        self.next_seq += 1;
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            seq: self.next_seq,
            kind,
            connection_id,
            device_id: device.map(|d| d.to_string()),
            code,
            quiet_ms,
        });
        self.changed.notify_all();
    }

    fn drop_connection(&mut self, id: u64) {
        self.conns.remove(&id);
        self.prompts.retain(|p| p.connection_id != id);
    }
}

/// What `transport_start` set up; immutable afterwards.
pub(super) struct Info {
    pub device_name: String,
    pub cert_sha256: String,
    pub listen: Option<SocketAddr>,
    pub discovery: discovery::Status,
}

pub(super) struct Core {
    pub endpoint: Endpoint,
    store: Arc<FileTrustStore>,
    pub policy: AddressPolicy,
    pub info: Info,
    /// Shared by every run of the host, so a request id never repeats across a restart.
    requests: Arc<AtomicU64>,
    state: Mutex<State>,
    shares: Mutex<share::Book>,
    clip: clipboard::Hub,
    calls: calls::Hub,
    data: data::Hub,
    permits: permits::Permits,
    redial: Mutex<redial::Book>,
    /// Rung when an unpaired computer announces itself, so the paused announcements answer it at once.
    announce_wake: tokio::sync::Notify,
}

pub(super) struct Decision {
    pub request_id: u64,
    pub connection_id: u64,
    pub approve: bool,
    pub verification_code: Option<String>,
    pub peer_cert_sha256: Option<String>,
}

impl Core {
    pub(super) fn new(
        endpoint: Endpoint,
        store: Arc<FileTrustStore>,
        policy: AddressPolicy,
        info: Info,
        requests: Arc<AtomicU64>,
    ) -> Self {
        Self {
            endpoint,
            store,
            policy,
            info,
            requests,
            state: Mutex::new(State {
                open: true,
                conns: BTreeMap::new(),
                attempts: Vec::new(),
                next_attempt: 1,
                prompts: Vec::new(),
                events: VecDeque::new(),
                next_seq: 0,
                changed: Arc::new(Condvar::new()),
                discovered: Vec::new(),
                counters: Counters::default(),
            }),
            shares: Mutex::new(share::Book::default()),
            clip: clipboard::Hub::default(),
            calls: calls::Hub::default(),
            data: data::Hub::default(),
            permits: permits::Permits::default(),
            announce_wake: tokio::sync::Notify::new(),
            redial: Mutex::new({
                let mut book = redial::Book::default();
                book.carry_over();
                book
            }),
        }
    }

    fn lock_redial(&self) -> MutexGuard<'_, redial::Book> {
        self.redial.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A panic while the lock was held only ever interrupted a bookkeeping update; keep serving.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn local_id(&self) -> &DeviceId {
        self.endpoint.credentials().device_id()
    }

    /// Reject everything pending and stop accepting registrations. Dropping the prompts makes any
    /// waiting approver resolve to a rejection; the runtime shutdown that follows ends the tasks.
    pub(super) fn shutdown(&self) {
        self.clip.shutdown();
        self.calls.shutdown();
        self.data.shutdown();
        self.shares
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel_all();
        let mut st = self.lock();
        st.open = false;
        for c in st.conns.values() {
            c.close.notify_one();
        }
        st.conns.clear();
        st.prompts.clear();
        st.attempts.clear();
        st.discovered.clear();
        st.events.clear();
        st.changed.notify_all();
    }

    /// Block until the newest event is not `since` any more, the transport stops, or `timeout`
    /// passes. This is how the app learns about a change (the peer unpaired) while nothing polls.
    pub(super) fn wait_events(&self, since: u64, timeout: Duration) -> Value {
        let deadline = std::time::Instant::now() + timeout.min(MAX_EVENT_WAIT);
        let mut st = self.lock();
        let changed = st.changed.clone();
        while st.open && st.next_seq == since {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            st = changed
                .wait_timeout(st, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        json!({ "last_event_seq": st.next_seq, "stopped": !st.open })
    }

    // ------------------------------------------------------------------------- commands

    /// At least one computer is paired and every paired computer has a trusted connection right now.
    pub(super) fn all_paired_linked(&self) -> bool {
        let Ok(paired) = self.store.list() else {
            return false;
        };
        let st = self.lock();
        !paired.is_empty()
            && paired.iter().all(|p| {
                st.conns
                    .values()
                    .any(|c| c.phase == Phase::Trusted && c.peer_id == p.device_id)
            })
    }

    /// Reserve an outbound attempt. The caller spawns [`outbound`] with the returned id.
    pub(super) fn begin_attempt(&self, device: &DeviceId) -> Result<u64, ApiError> {
        let mut st = self.lock();
        if !st.open {
            return Err(err("not_running", "the transport is not running"));
        }
        if st.conns.values().any(|c| c.peer_id == *device)
            || st.attempts.iter().any(|a| a.device == *device)
        {
            return Err(err(
                "already_connected",
                "a connection to this device already exists or is in progress",
            ));
        }
        if st.attempts.len() >= MAX_ATTEMPTS
            || st.conns.len() + st.attempts.len() >= MAX_CONNECTIONS
        {
            return Err(err(
                "too_many_connections",
                "too many connections or attempts are in flight",
            ));
        }
        let id = st.next_attempt;
        st.next_attempt += 1;
        st.attempts.push(Attempt {
            id,
            device: device.clone(),
            since: Instant::now(),
        });
        Ok(id)
    }

    pub(super) fn pair_request(
        &self,
        id: u64,
        mode: PairMode,
        qr: Option<QrPin>,
    ) -> Result<Value, ApiError> {
        let mut st = self.lock();
        let c = st
            .conns
            .get_mut(&id)
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        if c.phase != Phase::Parked {
            return Err(err(
                "wrong_state",
                "the connection is not waiting for a pairing mode",
            ));
        }
        // The token is a secret that only the computer in the QR code may see: check the
        // certificate on this very connection against the pinned one *before* it is sent.
        let token = match qr {
            Some(pin) if mode == PairMode::Request => {
                if !c.peer_cert_sha256.eq_ignore_ascii_case(&pin.cert_sha256) {
                    c.close.notify_one();
                    st.drop_connection(id);
                    return Err(err(
                        "qr_certificate_mismatch",
                        "this is not the computer in the QR code",
                    ));
                }
                Some(pin.token)
            }
            Some(_) => return Err(err("invalid_field", "a field has an invalid value")),
            None => None,
        };
        let c = st
            .conns
            .get_mut(&id)
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        c.ctl
            .try_send(Ctl::Pair(mode, token))
            .map_err(|_| err("busy", "the connection is busy"))?;
        c.phase = match mode {
            PairMode::Request => Phase::PairingRequest,
            PairMode::AwaitPeerRequest => Phase::PairingAwait,
        };
        Ok(json!({ "connection_id": id, "phase": c.phase.as_str() }))
    }

    /// Apply an explicit local decision to exactly one live prompt.
    pub(super) fn decide(&self, d: Decision) -> Result<Value, ApiError> {
        let mut st = self.lock();
        let unknown = || err("unknown_request", "no such pairing request");
        let index = st
            .prompts
            .iter()
            .position(|p| p.request_id == d.request_id && p.connection_id == d.connection_id)
            .ok_or_else(unknown)?;
        let live = st.prompts.get(index).is_some_and(|p| {
            st.conns
                .get(&p.connection_id)
                .is_some_and(|c| c.peer_cert_sha256 == p.peer_cert_sha256)
        });
        if !live {
            st.prompts.remove(index);
            return Err(unknown());
        }
        let Some(prompt) = st.prompts.get_mut(index) else {
            return Err(unknown());
        };
        if Instant::now() >= prompt.deadline {
            st.prompts.remove(index);
            return Err(err("request_expired", "the pairing request has expired"));
        }
        if !d.approve {
            let p = st.prompts.remove(index);
            let _ = p.tx.send(false);
            return Ok(json!({ "decision": "rejected" }));
        }
        let cert_ok = d
            .peer_cert_sha256
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(&prompt.peer_cert_sha256));
        let code_ok = d
            .verification_code
            .as_deref()
            .is_some_and(|c| c.trim().eq_ignore_ascii_case(&prompt.code));
        if !(cert_ok && code_ok) {
            prompt.wrong_attempts += 1;
            if prompt.wrong_attempts >= MAX_WRONG_ATTEMPTS {
                let p = st.prompts.remove(index);
                let _ = p.tx.send(false);
                return Err(err(
                    "too_many_attempts",
                    "too many wrong confirmations; the pairing request was rejected",
                ));
            }
            return Err(if cert_ok {
                err(
                    "verification_mismatch",
                    "the verification code does not match",
                )
            } else {
                err(
                    "certificate_mismatch",
                    "the certificate fingerprint does not match the request",
                )
            });
        }
        let p = st.prompts.remove(index);
        if p.tx.send(true).is_err() {
            return Err(err("request_expired", "the pairing request has expired"));
        }
        // Approval is only the local half; the peer must still accept. Never claim "paired" here.
        Ok(json!({ "decision": "approved" }))
    }

    pub(super) fn ping(&self, id: u64) -> Result<Value, ApiError> {
        let st = self.lock();
        let c = st
            .conns
            .get(&id)
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        if c.phase != Phase::Trusted {
            return Err(err("not_trusted", "the peer is not trusted"));
        }
        c.ctl
            .try_send(Ctl::Ping)
            .map_err(|_| err("busy", "the connection is busy"))?;
        Ok(json!({ "queued": true }))
    }

    /// Close a connection without touching trust.
    /// The computer said goodbye on this connection. Returns the close reason for the event log. A deliberate
    /// leave keeps this phone off the computer for a while; a shutdown leaves the normal back-off alone.
    fn peer_goodbye(
        &self,
        id: u64,
        why: bridge_core::goodbye::Reason,
    ) -> (&'static str, Option<&'static str>) {
        use bridge_core::goodbye::Reason;
        let device = self.lock().conns.get(&id).map(|c| c.peer_id.clone());
        if why.suppresses_redial()
            && let Some(device) = device
        {
            redial::suppress(
                &device,
                std::time::Instant::now(),
                bridge_core::goodbye::SUPPRESS_FOR,
            );
        }
        match why {
            Reason::Shutdown => ("link_closed", Some("peer_shutdown")),
            Reason::UserDisconnect | Reason::Unpair => ("link_closed", Some("peer_goodbye")),
        }
    }

    pub(super) fn disconnect(&self, id: u64) -> Result<Value, ApiError> {
        let mut st = self.lock();
        let c = st
            .conns
            .get(&id)
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        c.close.notify_one();
        // The user's own disconnect: no redial and no nudge-driven dial to this computer for a while (as after the
        // computer's goodbye); the user's Connect or the computer's own dial ends it.
        if c.phase == Phase::Trusted {
            redial::suppress(
                &c.peer_id,
                std::time::Instant::now(),
                bridge_core::goodbye::SUPPRESS_FOR,
            );
        }
        st.drop_connection(id);
        Ok(json!({ "disconnected": true }))
    }

    /// Change the name announced from the next connection and discovery announcement on.
    pub(super) fn set_device_name(&self, raw: &str) -> Result<Value, ApiError> {
        let name = self
            .endpoint
            .set_device_name(raw)
            .map_err(|_| err("invalid_device_name", "device name is not acceptable"))?;
        Ok(json!({ "device_name": name }))
    }

    /// Forget a device: delete its trust record first, then close any connection (a trusted
    /// link tells the peer on a best-effort basis).
    pub(super) fn unpair(&self, device: &DeviceId) -> Result<Value, ApiError> {
        let was_paired = self
            .store
            .remove(device)
            .map_err(|_| err("store_error", "the trust store could not be updated"))?;
        self.cancel_transfers_of(device.as_str());
        self.lock_redial().forget(device);
        let mut st = self.lock();
        let found = st
            .conns
            .values()
            .find(|c| c.peer_id == *device)
            .map(|c| (c.id, c.phase == Phase::Trusted));
        if let Some((id, trusted)) = found {
            let notified = trusted
                && st
                    .conns
                    .get(&id)
                    .is_some_and(|c| c.ctl.try_send(Ctl::Unpair).is_ok());
            if !notified && let Some(c) = st.conns.get(&id) {
                c.close.notify_one();
            }
            st.drop_connection(id);
        }
        st.event("unpaired", found.map(|f| f.0), Some(device), None);
        Ok(json!({ "was_paired": was_paired, "disconnected": found.is_some() }))
    }

    // ------------------------------------------------------------------------- tasks' side

    fn attempt_failed(&self, attempt: u64, device: &DeviceId, code: &'static str) {
        let mut st = self.lock();
        st.attempts.retain(|a| a.id != attempt);
        st.event("connect_failed", None, Some(device), Some(code));
    }

    /// Register a connection after its handshake, enforcing one connection per device.
    fn admit(
        &self,
        info: &LinkInfo,
        phase: Phase,
        ctl: mpsc::Sender<Ctl>,
        close: Arc<Notify>,
        attempt: Option<u64>,
    ) -> Result<(), &'static str> {
        let mut st = self.lock();
        if let Some(a) = attempt {
            st.attempts.retain(|x| x.id != a);
        }
        if !st.open {
            return Err("stopped");
        }
        let existing = st
            .conns
            .values()
            .find(|c| c.peer_id == info.peer.device_id)
            .map(|c| (c.id, c.phase, c.role, c.since));
        if let Some((old_id, old_phase, old_role, old_since)) = existing {
            let replace = match (phase == Phase::Trusted, old_phase == Phase::Trusted) {
                (true, false) => true,
                (false, _) => false,
                // A cross-dial only while the held connection is brand new; an older one meeting a new trusted
                // connection is a zombie (the computer gave up on it and its close never arrived): newest wins, as
                // on the computer (`bridge_service::collision`).
                (true, true) => {
                    old_since.elapsed() >= CROSS_DIAL_WINDOW
                        || prefer_new(self.local_id(), &info.peer.device_id, old_role, info.role)
                }
            };
            if !replace {
                st.counters.rejected_duplicates += 1;
                return Err("duplicate_connection");
            }
            if let Some(old) = st.conns.get(&old_id) {
                old.close.notify_one();
            }
            st.drop_connection(old_id);
        } else if st.conns.len() >= MAX_CONNECTIONS {
            return Err("too_many_connections");
        }
        let id = info.connection_id.0;
        st.conns.insert(
            id,
            ConnEntry {
                id,
                role: info.role,
                phase,
                peer_id: info.peer.device_id.clone(),
                peer_name: info.peer.device_name.clone(),
                peer_type: info.peer.device_type.as_str(),
                peer_cert_sha256: info.peer_cert.fingerprint_hex(),
                peer_ip: info.peer_ip,
                peer_debug: info
                    .peer
                    .incoming_capabilities
                    .iter()
                    .any(|c| c == bridge_core::debug_link::LOG),
                peer_accepts: info.peer.incoming_capabilities.clone(),
                ctl,
                close,
                since: Instant::now(),
                pings_received: 0,
                ignored_packets: 0,
            },
        );
        Ok(())
    }

    /// The bound path failed for `src`: remember it (unbound dials for a while) and tell the app, reason code only.
    fn bind_fallback(&self, src: Ipv4Addr, reason: &'static str, device: &DeviceId) {
        bindhealth::mark_unhealthy(src);
        self.lock()
            .event("wifi_bind_fallback", None, Some(device), Some(reason));
    }

    fn remove_conn(&self, id: u64, event: Option<(&'static str, Option<&'static str>)>) {
        self.remove_conn_quiet(id, event, None);
    }

    fn remove_conn_quiet(
        &self,
        id: u64,
        event: Option<(&'static str, Option<&'static str>)>,
        quiet_ms: Option<u64>,
    ) {
        let mut st = self.lock();
        let device = st.conns.get(&id).map(|c| c.peer_id.clone());
        let present = device.is_some();
        st.drop_connection(id);
        if present && let Some((kind, code)) = event {
            st.event_quiet(kind, Some(id), device.as_ref(), code, quiet_ms);
        }
    }

    fn with_conn(&self, id: u64, f: impl FnOnce(&mut ConnEntry)) {
        if let Some(c) = self.lock().conns.get_mut(&id) {
            f(c);
        }
    }

    fn clear_prompts(&self, id: u64) {
        self.lock().prompts.retain(|p| p.connection_id != id);
    }

    /// Register the prompt a local user must answer. `None` rejects at once (torn down, or too
    /// many prompts outstanding).
    fn add_prompt(
        &self,
        connection_id: u64,
        r: ApprovalRequest,
    ) -> Option<oneshot::Receiver<bool>> {
        let mut st = self.lock();
        if !st.open || st.prompts.len() >= MAX_PROMPTS {
            return None;
        }
        // A newer prompt on the same connection replaces the older one (the peer repeated its
        // request with a new key); the old request id can no longer be approved.
        st.prompts.retain(|p| p.connection_id != connection_id);
        let request_id = self.requests.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let device = DeviceId::parse(&r.peer_id).ok();
        st.prompts.push(Prompt {
            request_id,
            connection_id,
            direction: r.direction,
            peer_id: r.peer_id,
            peer_name: r.peer_name,
            peer_cert_sha256: r.peer_cert_sha256,
            own_cert_sha256: r.own_cert_sha256,
            code: r.verification_key,
            timestamp: r.timestamp,
            qr_pinned: r.qr_pinned,
            deadline: Instant::now() + r.expires_in,
            wrong_attempts: 0,
            tx,
        });
        st.event("pairing_prompt", Some(connection_id), device.as_ref(), None);
        Some(rx)
    }

    pub(super) fn record_discovery(&self, d: Discovered) {
        let mut st = self.lock();
        if !st.open {
            return;
        }
        // A stock computer that is not paired yet lists this phone only from its announcement. While every
        // paired computer is linked the announcements are paused, so answer a newcomer immediately.
        let newcomer = self
            .store
            .list()
            .is_ok_and(|paired| !paired.iter().any(|p| p.device_id == d.device_id));
        discovery::upsert(&mut st.discovered, d);
        drop(st);
        if newcomer {
            self.announce_wake.notify_one();
        }
    }

    /// Resolves when an unpaired computer has announced itself since the last time, or when the app nudged the core
    /// (screen on, Doze window, network back) while some paired computer is not linked: the 30 s announcement timer
    /// runs on the monotonic clock, which stands still while the phone sleeps, and the computer only redials a phone it
    /// has heard recently.
    pub(super) async fn newcomer_announced(&self) {
        let mut nudge = super::heartbeat_now_receiver();
        nudge.borrow_and_update();
        loop {
            tokio::select! {
                () = self.announce_wake.notified() => return,
                changed = nudge.changed() => {
                    if changed.is_err() {
                        return std::future::pending().await;
                    }
                    if !self.all_paired_linked() {
                        return;
                    }
                }
            }
        }
    }

    fn count(&self, f: impl FnOnce(&mut Counters)) {
        f(&mut self.lock().counters);
    }

    // ------------------------------------------------------------------------- snapshot

    pub(super) fn snapshot(&self, since: u64) -> Value {
        let st = self.lock();
        let now = Instant::now();
        let ms = |t: Instant| {
            u64::try_from(now.saturating_duration_since(t).as_millis()).unwrap_or(u64::MAX)
        };
        let connections: Vec<Value> = st
            .conns
            .values()
            .map(|c| {
                json!({
                    "connection_id": c.id,
                    "role": match c.role { Role::Initiator => "initiator", Role::Acceptor => "acceptor" },
                    "phase": c.phase.as_str(),
                    "trusted": c.phase == Phase::Trusted,
                    "peer": {
                        "device_id": c.peer_id.as_str(),
                        "name": c.peer_name,
                        "device_type": c.peer_type,
                        "cert_sha256": c.peer_cert_sha256,
                        "ip": c.peer_ip.to_string(),
                    },
                    "age_ms": ms(c.since),
                    "pings_received": c.pings_received,
                    "ignored_packets": c.ignored_packets,
                })
            })
            .collect();
        let prompts: Vec<Value> = st
            .prompts
            .iter()
            .filter(|p| p.deadline > now)
            .map(|p| {
                json!({
                    "request_id": p.request_id,
                    "connection_id": p.connection_id,
                    "direction": match p.direction { Direction::Outgoing => "outgoing", Direction::Incoming => "incoming" },
                    "peer_device_id": p.peer_id,
                    "peer_name": p.peer_name,
                    "peer_cert_sha256": p.peer_cert_sha256,
                    "own_cert_sha256": p.own_cert_sha256,
                    "verification_code": p.code,
                    "qr_pinned": p.qr_pinned,
                    "timestamp": p.timestamp,
                    "expires_in_ms": u64::try_from(p.deadline.saturating_duration_since(now).as_millis()).unwrap_or(0),
                })
            })
            .collect();
        let attempts: Vec<Value> = st
            .attempts
            .iter()
            .map(|a| json!({ "device_id": a.device.as_str(), "age_ms": ms(a.since) }))
            .collect();
        let events: Vec<Value> = st
            .events
            .iter()
            .filter(|e| e.seq > since)
            .map(|e| {
                let mut o = Map::new();
                o.insert("seq".into(), e.seq.into());
                o.insert("kind".into(), e.kind.into());
                if let Some(c) = e.connection_id {
                    o.insert("connection_id".into(), c.into());
                }
                if let Some(d) = &e.device_id {
                    o.insert("device_id".into(), d.as_str().into());
                }
                if let Some(q) = e.quiet_ms {
                    o.insert("quiet_ms".into(), q.into());
                }
                if let Some(c) = e.code {
                    o.insert("code".into(), c.into());
                }
                Value::Object(o)
            })
            .collect();
        let discovered = st.discovered.clone();
        let last_event_seq = st.next_seq;
        let transfers = self.transfers_json();
        let counters = json!({
            "inbound_dropped": st.counters.inbound_dropped,
            "inbound_failed": st.counters.inbound_failed,
            "rejected_duplicates": st.counters.rejected_duplicates,
        });
        drop(st);

        // The trust store is read after the state it is compared with: a trust record is written
        // before the event or phase that announces it, so everything reported above is already
        // reflected here (the list may be newer than the events, never older). The store has its
        // own lock and never calls back into this state.
        let paired = self.store.list();
        let paired_ids: Vec<DeviceId> = paired
            .as_ref()
            .map(|l| l.iter().map(|p| p.device_id.clone()).collect())
            .unwrap_or_default();
        let paired_json: Vec<Value> = paired
            .as_ref()
            .map(|l| {
                l.iter()
                    .take(MAX_PAIRED_LISTED)
                    .map(|p| {
                        json!({
                            "device_id": p.device_id.as_str(),
                            "name": p.name,
                            "device_type": p.device_type,
                            "cert_sha256": p.cert.fingerprint_hex(),
                            "paired_at_unix": p.paired_at_unix,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "state": "running",
            "local": {
                "device_id": self.local_id().as_str(),
                "device_name": self.endpoint.device_name(),
                "cert_sha256": self.info.cert_sha256,
            },
            "listen": self.info.listen.map(|a| json!({ "host": a.ip().to_string(), "port": a.port() })),
            "discovery": self.info.discovery.to_json(),
            "discovered": discovery::to_json(&discovered, now, &paired_ids),
            "connections": connections,
            "prompts": prompts,
            "attempts": attempts,
            "paired": paired_json,
            "paired_error": paired.is_err(),
            "events": events,
            "transfers": transfers,
            "last_event_seq": last_event_seq,
            "counters": counters,
        })
    }
}

// ------------------------------------------------------------------------------- tasks

pub(super) async fn accept_loop(core: Arc<Core>, listener: TcpListener) {
    let slots = Arc::new(Semaphore::new(MAX_INBOUND_HANDSHAKES));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(_) => {
                // Out of descriptors or similar: back off instead of spinning.
                sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            core.count(|c| c.inbound_dropped += 1);
            continue;
        };
        tokio::spawn(inbound(core.clone(), stream, permit));
    }
}

// Kernel TCP keepalive (30 s idle, 10 s x 3, TCP_USER_TIMEOUT 60 s) is armed by `bridge-kde` on every
// accepted and dialed socket, on Android as on Linux (see `bridge-kde/src/keepalive.rs`).

async fn inbound(core: Arc<Core>, stream: TcpStream, permit: tokio::sync::OwnedSemaphorePermit) {
    // The address policy runs inside `accept` before a byte is read.
    let result = timeout(HANDSHAKE_DEADLINE, core.endpoint.accept(stream)).await;
    drop(permit);
    match result {
        Ok(Ok(hs)) => drive(core, hs, Origin::Inbound, None, None, None).await,
        _ => core.count(|c| c.inbound_failed += 1),
    }
}

pub(super) async fn outbound(core: Arc<Core>, attempt: u64, addr: SocketAddr, target: DeviceId) {
    // How a bound dial went wrong, and whether the unbound retry must succeed before the bound path is blamed.
    let mut blame: Option<(Ipv4Addr, &'static str, bool)> = None;
    if let Some(IpAddr::V4(src)) = super::dial_source(addr.ip(), core.policy.loopback) {
        // One budget over connect and handshake: a bound socket that connects and then goes quiet (E2E-14) must
        // not hold the redial for the full handshake deadline.
        match timeout(
            bindhealth::BOUND_BUDGET,
            core.endpoint.connect_bound(addr, &target, IpAddr::V4(src)),
        )
        .await
        {
            Ok(Ok(hs)) => {
                drive(
                    core,
                    hs,
                    Origin::Outbound,
                    Some(attempt),
                    Some(addr),
                    Some(src),
                )
                .await;
                return;
            }
            Ok(Err(KdeError::Io(e))) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                // The plain route would say the same.
                core.attempt_failed(attempt, &target, "conn_refused");
                return;
            }
            Ok(Err(KdeError::Timeout(_))) | Err(_) => {
                blame = Some((src, bindhealth::REASON_STALLED, false));
            }
            Ok(Err(KdeError::Io(_) | KdeError::UnexpectedEof)) => {
                blame = Some((src, bindhealth::REASON_IO, true));
            }
            // Trust and protocol errors are not the bind's fault.
            Ok(Err(e)) => {
                core.attempt_failed(attempt, &target, kde_code(&e));
                return;
            }
        }
        if let Some((src, reason, false)) = blame {
            core.bind_fallback(src, reason, &target);
        }
    }
    match timeout(HANDSHAKE_DEADLINE, core.endpoint.connect(addr, &target)).await {
        Ok(Ok(hs)) => {
            if let Some((src, reason, true)) = blame {
                // The plain route works where the bound one failed: the bind is the culprit.
                core.bind_fallback(src, reason, &target);
            }
            drive(core, hs, Origin::Outbound, Some(attempt), Some(addr), None).await;
        }
        Ok(Err(e)) => core.attempt_failed(attempt, &target, kde_code(&e)),
        Err(_) => core.attempt_failed(attempt, &target, "timeout"),
    }
}

async fn drive(
    core: Arc<Core>,
    hs: Handshake,
    origin: Origin,
    attempt: Option<u64>,
    dialed: Option<SocketAddr>,
    bound: Option<Ipv4Addr>,
) {
    let info = hs.info().clone();
    let id = info.connection_id.0;
    let (ctl_tx, ctl_rx) = mpsc::channel(CTL_DEPTH);
    let close = Arc::new(Notify::new());
    let phase = match (&hs, origin) {
        (Handshake::Trusted(_), _) => Phase::Trusted,
        (Handshake::Pending(_), Origin::Inbound) => Phase::PairingAwait,
        (Handshake::Pending(_), Origin::Outbound) => Phase::Parked,
    };
    let peer_ip = info.peer_ip;
    let device = info.peer.device_id.clone();
    if let Err(code) = core.admit(&info, phase, ctl_tx, close.clone(), attempt) {
        if origin == Origin::Outbound {
            core.lock().event(
                "connect_failed",
                None,
                Some(&info.peer.device_id),
                Some(code),
            );
        }
        return; // dropping `hs` closes the connection
    }
    match hs {
        Handshake::Trusted(link) => {
            core.lock_redial()
                .connected(&device, peer_ip, dialed, info.peer.geekbridge_port);
            run_trusted(core, id, link, ctl_rx, close, bound).await
        }
        Handshake::Pending(link) => run_pending(core, id, link, origin, ctl_rx, close).await,
    }
}

async fn run_pending(
    core: Arc<Core>,
    id: u64,
    link: PendingLink,
    origin: Origin,
    mut ctl_rx: mpsc::Receiver<Ctl>,
    close: Arc<Notify>,
) {
    let (mode, token) = match origin {
        // Inbound peers are listened to at once so a request from the peer shows up as a prompt.
        Origin::Inbound => (PairMode::AwaitPeerRequest, None),
        Origin::Outbound => match park(&mut ctl_rx, &close).await {
            Parked::Mode(mode, token) => (mode, token),
            Parked::Closed => {
                core.remove_conn(id, None);
                return;
            }
            Parked::Expired => {
                core.remove_conn(id, Some(("link_closed", Some("park_timeout"))));
                return;
            }
        },
    };
    let link = match token {
        Some(token) => link.with_pair_token(token),
        None => link,
    };
    let mut approver = UiApprover {
        core: core.clone(),
        connection: id,
    };
    let outcome = tokio::select! {
        biased;
        _ = close.notified() => None,
        r = link.pair(mode, &mut approver) => Some(r),
    };
    core.clear_prompts(id);
    match outcome {
        Some(Ok(trusted)) => {
            let device = trusted.info().peer.device_id.clone();
            let known = core.lock().conns.contains_key(&id);
            if !known {
                return; // closed locally at the last moment; trust is already recorded
            }
            core.with_conn(id, |c| c.phase = Phase::Trusted);
            core.lock().event("paired", Some(id), Some(&device), None);
            let ip = core.lock().conns.get(&id).map(|c| c.peer_ip);
            if let Some(ip) = ip {
                core.lock_redial().connected(
                    &device,
                    ip,
                    None,
                    trusted.info().peer.geekbridge_port,
                );
            }
            run_trusted(core, id, trusted, ctl_rx, close, None).await;
        }
        Some(Err(e)) => {
            let code = kde_code(&e);
            // An unauthenticated peer can open connections and walk away at will; only failures
            // that follow an actual exchange are worth an event (the ring is small).
            let noise = origin == Origin::Inbound
                && matches!(
                    code,
                    "pairing_timeout" | "link_closed" | "peer_closed" | "io_error"
                );
            core.remove_conn(id, (!noise).then_some(("pairing_failed", Some(code))));
        }
        None => core.remove_conn(id, None),
    }
}

enum Parked {
    Mode(PairMode, Option<String>),
    Closed,
    Expired,
}

/// Wait for the UI to choose how to pair. Nothing is read from the peer meanwhile.
async fn park(ctl_rx: &mut mpsc::Receiver<Ctl>, close: &Notify) -> Parked {
    let ttl = sleep(PARK_TTL);
    tokio::pin!(ttl);
    loop {
        tokio::select! {
            biased;
            _ = close.notified() => return Parked::Closed,
            _ = &mut ttl => return Parked::Expired,
            ctl = ctl_rx.recv() => match ctl {
                Some(Ctl::Pair(mode, token)) => return Parked::Mode(mode, token),
                Some(_) => {}
                None => return Parked::Closed,
            },
        }
    }
}

async fn run_trusted(
    core: Arc<Core>,
    id: u64,
    mut link: TrustedLink,
    mut ctl_rx: mpsc::Receiver<Ctl>,
    close: Arc<Notify>,
    bound: Option<Ipv4Addr>,
) {
    let started = std::time::Instant::now();
    // A computer that connects while a call is already ringing must hear about it.
    if let Some(body) = core
        .calls()
        .current()
        .filter(|_| core.permitted("calls", id))
        && !calls::send(&mut link, calls::Out::State(body)).await
    {
        core.remove_conn(id, Some(("link_closed", Some("io_error"))));
        return;
    }
    // Stock KDE Connect shows every ping as a notification: only GeekBridge peers get the heartbeat.
    let heartbeat_peer = bridge_core::heartbeat::is_geekbridge_peer(
        &link.info().peer.incoming_capabilities,
        &link.info().peer.outgoing_capabilities,
    );
    let mut beat = Heartbeat::new(std::time::Instant::now());
    let mut tick = interval_at(Instant::now() + HEARTBEAT_EVERY, HEARTBEAT_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut nudge = super::heartbeat_now_receiver();
    nudge.borrow_and_update();
    let exit: Option<(&'static str, Option<&'static str>)> = loop {
        tokio::select! {
            biased;
            _ = close.notified() => break None,
            ctl = ctl_rx.recv() => match ctl {
                Some(Ctl::Ping) => {
                    if let Err(e) = link.send(&Packet::new(PING, Map::new())).await {
                        break Some(("link_closed", Some(kde_code(&e))));
                    }
                }
                Some(Ctl::Unpair) => {
                    // Trust record first, then a best-effort notice to the peer.
                    let _ = link.unpair().await;
                    break None;
                }
                Some(Ctl::SendFile(job)) => {
                    if !share::start_send(&core, &mut link, *job).await {
                        break Some(("link_closed", Some("io_error")));
                    }
                }
                Some(Ctl::SendText(job)) => {
                    if !share::send_text(&mut link, job).await {
                        break Some(("link_closed", Some("io_error")));
                    }
                }
                Some(Ctl::Clip(out)) => {
                    if !clipboard::send(&mut link, *out).await {
                        break Some(("link_closed", Some("io_error")));
                    }
                }
                Some(Ctl::Calls(out)) => {
                    if !calls::send(&mut link, out).await {
                        break Some(("link_closed", Some("io_error")));
                    }
                }
                Some(Ctl::Data(out)) => {
                    if !data::send(&mut link, out).await {
                        break Some(("link_closed", Some("io_error")));
                    }
                }
                Some(Ctl::Pair(..)) => {}
                None => break None,
            },
            event = link.recv() => match event {
                Ok(LinkEvent::Packet(p)) => {
                    // Only the advertised types are acted on; ping content is not retained.
                    let is_ping = p.kind == PING;
                    beat.heard(std::time::Instant::now(), is_ping);
                    if bridge_core::goodbye::is_goodbye_kind(&p.kind) && heartbeat_peer {
                        // The computer is closing on purpose. A malformed or unknown reason is just ignored.
                        if let Ok(why) = bridge_core::goodbye::parse(&p.body) {
                            break Some(core.peer_goodbye(id, why));
                        }
                    }
                    let acted = if p.kind == SHARE_REQUEST || p.kind == SHARE_REQUEST_UPDATE {
                        core.incoming_share(id, link.payload_context(), &p)
                    } else if bridge_core::calls_link::is_calls_kind(&p.kind) {
                        core.incoming_calls(id, &p)
                    } else if data::is_data_kind(&p.kind) {
                        core.incoming_data(id, &p)
                    } else if bridge_core::clipboard_wire::is_clipboard_kind(&p.kind)
                        || bridge_core::clipboard_wire::is_pull_protocol_kind(&p.kind)
                    {
                        core.incoming_clipboard(id, link.payload_context(), &p)
                    } else {
                        is_ping
                    };
                    core.with_conn(id, |c| {
                        if is_ping { c.pings_received += 1 } else if !acted { c.ignored_packets += 1 }
                    });
                }
                Ok(LinkEvent::PeerUnpaired) => {
                    // bridge-kde already removed the trust record.
                    let device = core.lock().conns.get(&id).map(|c| c.peer_id.clone());
                    if let Some(device) = device {
                        core.cancel_transfers_of(device.as_str());
                    }
                    break Some(("peer_unpaired", None));
                }
                Ok(LinkEvent::PeerRequestedRePair { .. }) => {
                    {
                        let mut st = core.lock();
                        let device = st.conns.get(&id).map(|c| c.peer_id.clone());
                        st.event("peer_requested_repair", Some(id), device.as_ref(), None);
                    }
                    // The computer forgot this phone while the record here outlived it. Its
                    // request is shown as the usual prompt on this connection; nothing changes
                    // until the user accepts.
                    if let Ok(pending) = link.into_re_pairing() {
                        // Sends queued for the old trusted link will never go out on this one:
                        // fail them (and the transfers announced to this device) now.
                        let device = core.lock().conns.get(&id).map(|c| c.peer_id.clone());
                        if let Some(device) = device {
                            core.cancel_transfers_of(device.as_str());
                        }
                        while let Ok(queued) = ctl_rx.try_recv() {
                            core.drop_queued(queued);
                        }
                        core.with_conn(id, |c| c.phase = Phase::PairingAwait);
                        return Box::pin(run_pending(
                            core, id, pending, Origin::Inbound, ctl_rx, close,
                        ))
                        .await;
                    }
                    break Some(("link_closed", Some("protocol")));
                }
                Err(e) => break Some(("link_closed", Some(kde_code(&e)))),
            },
            // The phone left Doze or opened a maintenance window: the timer below may not run again for a long
            // while (its clock stops while the CPU sleeps), so ping now and let a dead link show itself.
            changed = nudge.changed() => {
                if changed.is_ok() && heartbeat_peer {
                    if let Err(e) = link.send(&Packet::new(PING, Map::new())).await {
                        break Some(("link_closed", Some(kde_code(&e))));
                    }
                    beat.sent(std::time::Instant::now());
                }
            },
            // Last in the biased order: after a long sleep, packets already queued win over this check.
            _ = tick.tick() => {
                let now = std::time::Instant::now();
                if beat.peer_silent(now) {
                    break Some(("link_closed", Some("ping_timeout")));
                }
                if heartbeat_peer && beat.ping_due_idle(now, super::screen_off()) {
                    if let Err(e) = link.send(&Packet::new(PING, Map::new())).await {
                        break Some(("link_closed", Some(kde_code(&e))));
                    }
                    beat.sent(std::time::Instant::now());
                }
            },
        }
    };
    // Files queued behind the failure will never be announced; fail them instead of leaving them waiting.
    ctl_rx.close();
    while let Ok(queued) = ctl_rx.try_recv() {
        core.drop_queued(queued);
    }
    let redial = exit.is_some_and(|(kind, _)| kind == "link_closed");
    let device = core.lock().conns.get(&id).map(|c| c.peer_id.clone());
    let quiet = u64::try_from(beat.silent_for(std::time::Instant::now()).as_millis()).ok();
    core.remove_conn_quiet(id, exit, quiet);
    // A link from the Wi-Fi-bound socket that dies young without the computer ever having been heard: the bound
    // path is suspect. The redial below then leaves unbound.
    if let Some(src) = bound
        && bindhealth::early_death(
            started.elapsed(),
            beat.silent_for(std::time::Instant::now()),
            exit.and_then(|(_, code)| code),
        )
        && let Some(device) = &device
    {
        core.bind_fallback(src, bindhealth::REASON_EARLY_DEATH, device);
    }
    if redial && let Some(device) = device {
        spawn_redial(&core, device);
    }
}

/// Dial the computer again after an unexpected loss (the user's own disconnect, an unpair or a
/// stop never get here). The computer redials too; whichever link lands first wins and the
/// cross-dial rule in [`prefer_new`] keeps exactly one.
/// After a transport restart (network change): dial the paired computers it was linked to before.
pub(super) fn redial_remembered(core: &Arc<Core>, handle: &tokio::runtime::Handle) {
    let _guard = handle.enter();
    let devices = core.lock_redial().dialable();
    for device in devices {
        if matches!(core.store.get(&device), Ok(Some(_))) {
            spawn_redial(core, device);
        }
    }
}

fn spawn_redial(core: &Arc<Core>, device: DeviceId) {
    spawn_redial_now(core, device, false);
}

/// `now`: dial at once instead of waiting out the first back-off (the app just nudged us).
fn spawn_redial_now(core: &Arc<Core>, device: DeviceId, now: bool) {
    let Some(token) = core.lock_redial().begin(&device) else {
        return;
    };
    let core = core.clone();
    tokio::spawn(async move {
        redial_loop(&core, &device, token, now).await;
        core.lock_redial().end(&device, token);
    });
}

/// The app nudged the core (screen on, Doze maintenance window or its end, see `transport_heartbeat_now`). A redial
/// already sleeping in its back-off wakes by itself; this covers the paired computers with no link and no redial
/// running, typically because the budget ran out while the phone was asleep: each nudge buys one more try. Nudges come
/// from user and OS events, not timers, so this cannot turn into a storm.
pub(super) async fn redial_on_nudge(core: Arc<Core>) {
    let mut nudge = super::heartbeat_now_receiver();
    nudge.borrow_and_update();
    while nudge.changed().await.is_ok() {
        if !core.lock().open {
            return;
        }
        let devices = core.lock_redial().dialable();
        for device in devices {
            let linked = core
                .lock()
                .conns
                .values()
                .any(|c| c.peer_id == device && c.phase == Phase::Trusted);
            if linked || !matches!(core.store.get(&device), Ok(Some(_))) {
                continue;
            }
            if !redial::suppressed_for(&device, std::time::Instant::now()).is_zero() {
                continue;
            }
            core.lock_redial().grant_one(&device);
            spawn_redial_now(&core, device, true);
        }
    }
}

/// The computer asked to be left alone (a goodbye): wait that out. Ends early, with `false`, when the transport
/// stops or a link comes up (the user's Connect, the computer's own dial), which also ends the request.
async fn wait_out_goodbye(core: &Arc<Core>, device: &DeviceId) -> bool {
    loop {
        let left = redial::suppressed_for(device, std::time::Instant::now());
        if left.is_zero() {
            return true;
        }
        sleep(left.min(Duration::from_secs(1))).await;
        let st = core.lock();
        if !st.open || st.conns.values().any(|c| c.peer_id == *device) {
            return false;
        }
    }
}

/// `token` is the claim taken when this task was spawned: once the device was forgotten (unpaired) the task is stale,
/// even if the same computer is paired again before its back-off ends, and stops without touching anything.
async fn redial_loop(core: &Arc<Core>, device: &DeviceId, token: u64, mut now: bool) {
    let mut nudge = super::heartbeat_now_receiver();
    nudge.borrow_and_update();
    loop {
        let Some(delay) = core.lock_redial().next_delay(device) else {
            return;
        };
        // The back-off runs on the monotonic clock, which stands still while the phone sleeps: a 60 s wait can last
        // hours with the screen locked. A nudge from the app (screen on, Doze window) cuts it short.
        if !std::mem::take(&mut now) {
            tokio::select! {
                () = sleep(delay) => {}
                _ = nudge.changed() => {}
            }
        }
        // A goodbye can arrive while this wait runs (the loop outlives the link that started it).
        if !wait_out_goodbye(core, device).await {
            return;
        }
        if !core.lock_redial().owns(device, token) {
            return; // unpaired (and maybe paired again) while waiting: a newer pairing owns the redial
        }
        let addr = core.lock_redial().addr(device);
        let paired = matches!(core.store.get(device), Ok(Some(_)));
        let Some(addr) = addr.filter(|a| paired && core.policy.check(a.ip()).is_ok()) else {
            core.lock_redial().forget(device);
            return;
        };
        match core.begin_attempt(device) {
            Ok(attempt) => {
                core.lock().event("redial", None, Some(device), None);
                tokio::spawn(outbound(core.clone(), attempt, addr, device.clone()));
            }
            // Connected meanwhile (the computer got there first), or stopped.
            Err(_) => return,
        }
        // The dial ends within the handshake deadline; a landed link ends the loop.
        let deadline = Instant::now() + HANDSHAKE_DEADLINE + Duration::from_secs(2);
        while Instant::now() < deadline {
            sleep(Duration::from_millis(500)).await;
            let st = core.lock();
            if !st.open || st.conns.values().any(|c| c.peer_id == *device) {
                return;
            }
            if !st.attempts.iter().any(|a| a.device == *device) {
                break;
            }
        }
    }
}

/// Bridges `bridge-kde`'s approval hook to the UI: the pairing waits on a oneshot that only
/// [`Core::decide`] (or teardown, as a rejection) resolves.
struct UiApprover {
    core: Arc<Core>,
    connection: u64,
}

impl Approver for UiApprover {
    async fn approve(&mut self, request: ApprovalRequest) -> bool {
        match self.core.add_prompt(self.connection, request) {
            Some(rx) => rx.await.unwrap_or(false),
            None => false,
        }
    }
}

/// Static, content-free code for a failure. Never includes a message, address or fingerprint.
pub(super) fn kde_code(e: &KdeError) -> &'static str {
    match e {
        KdeError::Io(e) => match e.kind() {
            std::io::ErrorKind::ConnectionReset => "conn_reset",
            std::io::ErrorKind::BrokenPipe => "broken_pipe",
            std::io::ErrorKind::ConnectionAborted => "conn_aborted",
            std::io::ErrorKind::TimedOut => "tcp_timeout",
            // A redial that fails tells apart "the computer's service is not listening" from "no route to it" (the
            // phone is on another network, or the default network is mobile data and the LAN is not routed).
            std::io::ErrorKind::ConnectionRefused => "conn_refused",
            std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkUnreachable => {
                "unreachable"
            }
            _ => "io_error",
        },
        KdeError::Timeout(_) => "timeout",
        KdeError::UnexpectedEof => "peer_closed",
        KdeError::LineTooLong { .. } | KdeError::OutgoingTooLarge { .. } => "packet_too_large",
        KdeError::MalformedPacket(_) => "malformed_packet",
        KdeError::InvalidIdentity(_) => "invalid_identity",
        KdeError::UnsupportedProtocol(_) => "unsupported_protocol",
        KdeError::TargetMismatch(_) => "target_mismatch",
        KdeError::ProtocolDowngrade { .. } => "protocol_downgrade",
        KdeError::Tls(_) => "tls_failed",
        KdeError::Certificate(_) => "certificate_invalid",
        KdeError::PinMismatch { .. } => "pin_mismatch",
        KdeError::IdentityMismatch(_) => "identity_mismatch",
        KdeError::AddressRejected(_) => "address_rejected",
        KdeError::TooManyConnections => "too_many_connections",
        KdeError::Pairing(f) => match f {
            PairingFailure::Timeout => "pairing_timeout",
            PairingFailure::RejectedByPeer => "pairing_rejected_by_peer",
            PairingFailure::RejectedLocally => "pairing_rejected_locally",
            PairingFailure::ClockSkew => "clock_skew",
            PairingFailure::Malformed(_) => "pairing_malformed",
            PairingFailure::TooManyUnexpectedPackets => "pairing_unexpected_packets",
            PairingFailure::BindingChanged => "pairing_binding_changed",
            PairingFailure::LinkClosed => "link_closed",
        },
        KdeError::Store(_) | KdeError::StoreCorrupt { .. } => "store_error",
        KdeError::StoreNewer { .. } => "state_newer",
        KdeError::InsecurePermissions { .. } => "insecure_state",
        KdeError::UnsupportedPlatform(_) => "unsupported_platform",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> DeviceId {
        DeviceId::parse(&c.to_string().repeat(32)).unwrap()
    }

    #[test]
    fn cross_dial_resolution_is_symmetric() {
        let (a, b) = (id('a'), id('b'));
        // Connection X was opened by `a` (the smaller id), Y by `b`.
        // On a: X is Initiator, Y is Acceptor. On b: X is Acceptor, Y is Initiator.
        // Each side first holds one and then sees the other arrive.
        let a_keeps_x = !prefer_new(&a, &b, Role::Initiator, Role::Acceptor); // old X, new Y
        let b_replaces_y_with_x = prefer_new(&b, &a, Role::Initiator, Role::Acceptor); // old Y, new X
        assert!(a_keeps_x && b_replaces_y_with_x);
        // And in the opposite arrival order.
        let a_replaces_y_with_x = prefer_new(&a, &b, Role::Acceptor, Role::Initiator); // old Y, new X
        let b_keeps_x = !prefer_new(&b, &a, Role::Acceptor, Role::Initiator); // old X, new Y
        assert!(a_replaces_y_with_x && b_keeps_x);
        // Same direction twice is a reconnect: newest wins.
        assert!(prefer_new(&a, &b, Role::Initiator, Role::Initiator));
        assert!(prefer_new(&a, &b, Role::Acceptor, Role::Acceptor));
    }

    #[test]
    fn io_failures_name_their_cause() {
        use std::io::{Error, ErrorKind};
        let code = |k| kde_code(&KdeError::Io(Error::from(k)));
        assert_eq!(code(ErrorKind::ConnectionReset), "conn_reset");
        assert_eq!(code(ErrorKind::BrokenPipe), "broken_pipe");
        assert_eq!(code(ErrorKind::TimedOut), "tcp_timeout");
        assert_eq!(code(ErrorKind::ConnectionRefused), "conn_refused");
        assert_eq!(code(ErrorKind::HostUnreachable), "unreachable");
        assert_eq!(code(ErrorKind::NetworkUnreachable), "unreachable");
        assert_eq!(code(ErrorKind::PermissionDenied), "io_error");
    }

    #[test]
    fn every_pairing_failure_has_a_static_code() {
        for f in [
            PairingFailure::Timeout,
            PairingFailure::RejectedByPeer,
            PairingFailure::RejectedLocally,
            PairingFailure::ClockSkew,
            PairingFailure::Malformed("x"),
            PairingFailure::TooManyUnexpectedPackets,
            PairingFailure::BindingChanged,
            PairingFailure::LinkClosed,
        ] {
            let c = kde_code(&KdeError::Pairing(f));
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
    }
}
