// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit, connection-bound pairing.
//!
//! A [`PendingLink`] is the only place pairing happens. The state machine
//! below is pure (time is passed in) so every rule is unit-testable; the driver
//! at the bottom wires it to a link, a local approver and the trust store.
//!
//! Rules enforced here:
//! * Trust is written only after a *local* approval decision and only for the
//!   certificate presented on this exact connection.
//! * A peer's `{pair:true}` carrying a timestamp while we have an outstanding
//!   request is treated as a crossed request, **not** as an accept. Stock apps
//!   read it as an accept, which would pair with no local decision.
//! * Duplicate or unsolicited pair packets never cause a second accept to be emitted. A small
//!   budget of them is tolerated before the link is dropped.
//! * A repeated *request* from the peer while its prompt is pending (stock Android derives a new
//!   key from every request's timestamp) **replaces** the prompt: the old one is cancelled, the
//!   user is shown the new key, and every decision is tagged with the prompt generation it was
//!   made for, so an approval of an older key is refused. At most [`MAX_REPLACEMENTS`] requests
//!   per [`REPLACEMENT_WINDOW`] are honoured, the rest are ignored.
//! * Non-pair packets are dropped, never delivered.
//! * Pairing state dies with the connection.

use std::future::Future;
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until};

use crate::cert::{CertInfo, hex, unix_now};
use crate::error::{KdeError, PairingFailure, Result};
use crate::link::{PendingLink, TrustedLink};
use crate::packet::Packet;
use crate::store::TrustedPeer;

pub const PAIR_TYPE: &str = "kdeconnect.pair";

/// Peer requests per window that may (re)start a prompt; further ones are ignored.
pub const MAX_REPLACEMENTS: usize = 5;
/// A prompt series (the first request plus its replacements) lives at most this many response
/// windows, however many replacements arrive.
pub const PROMPT_CAP_FACTOR: u32 = 3;
/// Tries at the trust-store write that completes a pairing when its file lock times out.
const PERSIST_ATTEMPTS: usize = 3;

pub const REPLACEMENT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct PairingConfig {
    /// How long we wait for the peer once *we* asked, and for a peer request
    /// in [`PairMode::AwaitPeerRequest`].
    pub request_timeout: Duration,
    /// How long the local user has to answer a request made by the peer.
    pub response_timeout: Duration,
    /// Accepted distance between a request timestamp and our clock.
    pub max_skew_secs: i64,
    /// Unexpected/duplicate packets tolerated while unpaired.
    pub max_unexpected: u32,
    /// Wall clock in unix seconds (injectable for tests).
    pub clock: fn() -> i64,
    /// One-time QR token to put in our own pair request ([`crate::qrpair`]). The caller has
    /// already checked that the peer's certificate is the one the QR code pinned.
    pub token: Option<String>,
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            response_timeout: Duration::from_secs(25),
            max_skew_secs: 1800,
            max_unexpected: 8,
            clock: unix_now,
            token: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairMode {
    /// Send a pair request now and wait for the peer to accept.
    Request,
    /// Send nothing; wait for the peer to ask.
    AwaitPeerRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// We asked the peer.
    Outgoing,
    /// The peer asked us.
    Incoming,
}

/// Everything a human needs to decide. Shown by the [`Approver`].
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    pub direction: Direction,
    pub peer_id: String,
    pub peer_name: String,
    pub peer_cert_sha256: String,
    pub own_cert_sha256: String,
    /// 8 hex characters derived from both public keys and the request timestamp.
    /// The peer's screen shows the same value.
    pub verification_key: String,
    pub timestamp: i64,
    pub expires_in: Duration,
    /// One-time token the peer put in its pair request (QR pairing). It is unverified here: the
    /// approver redeems it against its own [`crate::qrpair::TokenBook`].
    pub presented_token: Option<String>,
    /// We sent a QR token ourselves, i.e. the peer's certificate was pinned from the QR code
    /// before this prompt, so the code comparison is not needed.
    pub qr_pinned: bool,
}

/// Source of the local decision. Returning `true` is the explicit approval; a
/// pairing never completes without one.
pub trait Approver: Send {
    fn approve(&mut self, request: ApprovalRequest) -> impl Future<Output = bool> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairBody {
    pub pair: bool,
    pub timestamp: Option<i64>,
    pub token: Option<String>,
}

impl PairBody {
    pub fn parse(packet: &Packet) -> std::result::Result<Self, PairingFailure> {
        let pair = match packet.body.get("pair") {
            Some(Value::Bool(b)) => *b,
            _ => return Err(PairingFailure::Malformed("pair is not a boolean")),
        };
        let timestamp = match packet.body.get("timestamp") {
            None => None,
            Some(Value::Number(n)) => Some(
                n.as_i64()
                    .ok_or(PairingFailure::Malformed("timestamp is not an integer"))?,
            ),
            Some(_) => return Err(PairingFailure::Malformed("timestamp is not an integer")),
        };
        let token = match packet.body.get("gbToken") {
            None => None,
            Some(Value::String(t)) if crate::qrpair::token_shape_ok(t) => Some(t.clone()),
            Some(_) => return Err(PairingFailure::Malformed("gbToken is not a token")),
        };
        Ok(Self {
            pair,
            timestamp,
            token,
        })
    }
}

pub fn pair_packet(pair: bool, timestamp: Option<i64>) -> Packet {
    let mut body = Map::new();
    body.insert("pair".into(), Value::Bool(pair));
    if let Some(t) = timestamp {
        body.insert("timestamp".into(), Value::from(t));
    }
    Packet::new(PAIR_TYPE, body)
}

/// Protocol 8 verification key: SHA-256 over the two SubjectPublicKeyInfo DER
/// blobs (unsigned-lexicographically larger first) followed by the decimal
/// timestamp; the first 4 bytes as upper-case hex. Identity strings and device
/// ids do not take part.
pub fn verification_key(own_spki: &[u8], peer_spki: &[u8], timestamp: i64) -> String {
    let (first, second) = if own_spki < peer_spki {
        (peer_spki, own_spki)
    } else {
        (own_spki, peer_spki)
    };
    let mut h = Sha256::new();
    h.update(first);
    h.update(second);
    h.update(timestamp.to_string().as_bytes());
    hex(&h.finalize()[..4]).to_uppercase()
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) struct Now {
    pub mono: Instant,
    pub unix: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Send {
        pair: bool,
        timestamp: Option<i64>,
    },
    /// Show this prompt. Any earlier prompt is cancelled first; `generation` identifies it.
    Prompt {
        generation: u64,
        direction: Direction,
        timestamp: i64,
        verification_key: String,
        expires_in: Duration,
    },
    /// Both sides have agreed; the caller persists trust.
    Complete,
    Fail(PairingFailure),
}

#[derive(Debug)]
enum State {
    Waiting {
        deadline: Instant,
    },
    Requested {
        deadline: Instant,
        peer_accepted: bool,
        local_approved: bool,
    },
    RequestedByPeer {
        deadline: Instant,
        timestamp: i64,
    },
    Finished,
}

#[derive(Debug)]
pub(crate) struct Machine {
    state: State,
    cfg: PairingConfig,
    own_spki: Vec<u8>,
    peer_spki: Vec<u8>,
    unexpected: u32,
    hint_sent: bool,
    generation: u64,
    /// Monotonic times of the peer requests that opened or replaced a prompt.
    requests: Vec<Instant>,
    /// When the first prompt of the current series opened; replacements never run past a fixed
    /// multiple of the response window counted from here.
    prompt_started: Option<Instant>,
}

impl Machine {
    pub(crate) fn new(cfg: PairingConfig, own_spki: Vec<u8>, peer_spki: Vec<u8>) -> Self {
        Self {
            state: State::Finished,
            cfg,
            own_spki,
            peer_spki,
            unexpected: 0,
            hint_sent: false,
            generation: 0,
            requests: Vec::new(),
            prompt_started: None,
        }
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        match self.state {
            State::Waiting { deadline }
            | State::Requested { deadline, .. }
            | State::RequestedByPeer { deadline, .. } => Some(deadline),
            State::Finished => None,
        }
    }

    fn key(&self, ts: i64) -> String {
        verification_key(&self.own_spki, &self.peer_spki, ts)
    }

    pub(crate) fn start(&mut self, mode: PairMode, now: Now) -> Vec<Action> {
        let deadline = now.mono + self.cfg.request_timeout;
        match mode {
            PairMode::AwaitPeerRequest => {
                self.state = State::Waiting { deadline };
                vec![]
            }
            PairMode::Request => {
                let ts = now.unix;
                self.state = State::Requested {
                    deadline,
                    peer_accepted: false,
                    local_approved: false,
                };
                self.generation += 1;
                vec![
                    Action::Send {
                        pair: true,
                        timestamp: Some(ts),
                    },
                    Action::Prompt {
                        generation: self.generation,
                        direction: Direction::Outgoing,
                        timestamp: ts,
                        verification_key: self.key(ts),
                        expires_in: self.cfg.request_timeout,
                    },
                ]
            }
        }
    }

    fn fail(&mut self, f: PairingFailure, notify_peer: bool) -> Vec<Action> {
        self.state = State::Finished;
        let mut out = Vec::new();
        if notify_peer {
            out.push(Action::Send {
                pair: false,
                timestamp: None,
            });
        }
        out.push(Action::Fail(f));
        out
    }

    /// Returns `Some(actions)` if the deadline has passed.
    fn expired(&mut self, now: Now) -> Option<Vec<Action>> {
        let deadline = self.deadline()?;
        if now.mono < deadline {
            return None;
        }
        let notify = !matches!(self.state, State::Waiting { .. });
        Some(self.fail(PairingFailure::Timeout, notify))
    }

    pub(crate) fn on_tick(&mut self, now: Now) -> Vec<Action> {
        self.expired(now).unwrap_or_default()
    }

    fn note_unexpected(&mut self) -> Option<Vec<Action>> {
        self.unexpected += 1;
        (self.unexpected > self.cfg.max_unexpected)
            .then(|| self.fail(PairingFailure::TooManyUnexpectedPackets, false))
    }

    /// A packet that is neither pair nor identity: dropped, counted, and at
    /// most one `{pair:false}` hint is sent while nothing is pending.
    pub(crate) fn on_other_packet(&mut self, now: Now) -> Vec<Action> {
        if matches!(self.state, State::Finished) {
            return vec![];
        }
        if let Some(a) = self.expired(now) {
            return a;
        }
        if let Some(a) = self.note_unexpected() {
            return a;
        }
        if matches!(self.state, State::Waiting { .. }) && !self.hint_sent {
            self.hint_sent = true;
            return vec![Action::Send {
                pair: false,
                timestamp: None,
            }];
        }
        vec![]
    }

    pub(crate) fn on_pair(&mut self, body: PairBody, now: Now) -> Vec<Action> {
        if matches!(self.state, State::Finished) {
            return vec![];
        }
        if let Some(a) = self.expired(now) {
            return a;
        }
        match (&mut self.state, body.pair, body.timestamp) {
            (State::Waiting { .. }, true, Some(ts)) => {
                if (ts - now.unix).abs() > self.cfg.max_skew_secs {
                    return self.fail(PairingFailure::ClockSkew, true);
                }
                self.requests.push(now.mono);
                self.prompt_peer_request(ts, now)
            }
            // Peer cancels something we never saw, or an "accept" nobody asked for.
            (State::Waiting { .. }, _, _) => self.note_unexpected().unwrap_or_default(),

            (
                State::Requested {
                    peer_accepted,
                    local_approved,
                    ..
                },
                true,
                None,
            ) => {
                if *peer_accepted {
                    return self.note_unexpected().unwrap_or_default();
                }
                *peer_accepted = true;
                if *local_approved {
                    self.state = State::Finished;
                    vec![Action::Complete]
                } else {
                    vec![]
                }
            }
            // Crossed request: ignored on purpose, see module docs.
            (State::Requested { .. }, true, Some(_)) => self.note_unexpected().unwrap_or_default(),
            (State::Requested { .. }, false, _) => self.fail(PairingFailure::RejectedByPeer, false),

            // A repeated request replaces the pending prompt (new timestamp, new key). Same
            // timestamp, clock skew, or an exhausted budget: ignored, nothing changes.
            (State::RequestedByPeer { timestamp, .. }, true, Some(ts)) => {
                let same = ts == *timestamp;
                let skewed = (ts - now.unix).abs() > self.cfg.max_skew_secs;
                self.requests
                    .retain(|t| now.mono.saturating_duration_since(*t) < REPLACEMENT_WINDOW);
                if same || skewed || self.requests.len() >= MAX_REPLACEMENTS {
                    return self.note_unexpected().unwrap_or_default();
                }
                self.requests.push(now.mono);
                self.prompt_peer_request(ts, now)
            }
            // A bare accept while the peer's own request is pending is nothing.
            (State::RequestedByPeer { .. }, true, None) => {
                self.note_unexpected().unwrap_or_default()
            }
            (State::RequestedByPeer { .. }, false, _) => {
                self.fail(PairingFailure::RejectedByPeer, false)
            }

            (State::Finished, _, _) => vec![],
        }
    }

    /// (Re)open the prompt for a peer request: a fresh generation, a fresh response window.
    fn prompt_peer_request(&mut self, ts: i64, now: Now) -> Vec<Action> {
        let started = match (&self.state, self.prompt_started) {
            (State::RequestedByPeer { .. }, Some(t)) => t,
            _ => now.mono,
        };
        self.prompt_started = Some(started);
        let cap = started + self.cfg.response_timeout * PROMPT_CAP_FACTOR;
        let expires_in = self
            .cfg
            .response_timeout
            .min(cap.saturating_duration_since(now.mono));
        self.generation += 1;
        self.state = State::RequestedByPeer {
            deadline: now.mono + expires_in,
            timestamp: ts,
        };
        vec![Action::Prompt {
            generation: self.generation,
            direction: Direction::Incoming,
            timestamp: ts,
            verification_key: self.key(ts),
            expires_in,
        }]
    }

    /// `generation` is the prompt the decision was made for. A decision for any other prompt
    /// (an older key that has since been replaced) is dropped: it is not a decision on what
    /// the user is looking at now.
    pub(crate) fn on_local_decision(
        &mut self,
        generation: u64,
        approve: bool,
        now: Now,
    ) -> Vec<Action> {
        if let Some(a) = self.expired(now) {
            return a;
        }
        if generation != self.generation {
            return vec![];
        }
        match &mut self.state {
            State::Requested { .. } | State::RequestedByPeer { .. } if !approve => {
                self.fail(PairingFailure::RejectedLocally, true)
            }
            State::Requested {
                peer_accepted,
                local_approved,
                ..
            } => {
                *local_approved = true;
                if *peer_accepted {
                    self.state = State::Finished;
                    vec![Action::Complete]
                } else {
                    vec![]
                }
            }
            State::RequestedByPeer { .. } => {
                // Finished first, so a duplicate decision can never send a second accept.
                self.state = State::Finished;
                vec![
                    Action::Send {
                        pair: true,
                        timestamp: None,
                    },
                    Action::Complete,
                ]
            }
            State::Waiting { .. } | State::Finished => vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Serves prompts to the approver one at a time. A newer prompt drops the pending `approve`
/// future (cancelling the old prompt in the UI) before the approver is asked again, and each
/// decision is returned tagged with the generation it answers.
async fn serve_approvals<A: Approver>(
    approver: &mut A,
    mut prompts: mpsc::UnboundedReceiver<(u64, ApprovalRequest)>,
    decisions: mpsc::UnboundedSender<(u64, bool)>,
) {
    let mut next = prompts.recv().await;
    while let Some((generation, request)) = next {
        tokio::select! {
            decision = approver.approve(request) => {
                let _ = decisions.send((generation, decision));
                next = prompts.recv().await;
            }
            newer = prompts.recv() => next = newer,
        }
    }
    std::future::pending::<()>().await;
}

impl PendingLink {
    /// Run one pairing attempt on this link. Consumes the link: on failure the
    /// connection is dropped and nothing is stored; on success the returned
    /// [`TrustedLink`] is bound to the same connection and certificate.
    pub async fn pair<A: Approver>(
        mut self,
        mode: PairMode,
        approver: &mut A,
    ) -> Result<TrustedLink> {
        let clock = self.pairing.clock;
        let conn_id = self.conn.info.connection_id;
        let bound_fp = self.conn.info.peer_cert.fingerprint();
        let own_fp = self.own_cert.fingerprint_hex();
        let mut machine = Machine::new(
            self.pairing.clone(),
            self.own_cert.spki_der().to_vec(),
            self.conn.info.peer_cert.spki_der().to_vec(),
        );
        let now = || Now {
            mono: Instant::now(),
            unix: clock(),
        };
        let mut actions = machine.start(mode, now());
        let mut peer_token: Option<String> = None;
        if let (PairMode::AwaitPeerRequest, Some(request)) = (mode, self.seed.take()) {
            // The request was read by the trusted link this one came from.
            peer_token = request.token.clone();
            actions = machine.on_pair(request, now());
        }
        let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
        let (decision_tx, mut decision_rx) = mpsc::unbounded_channel();
        let approvals = serve_approvals(approver, prompt_rx, decision_tx);
        tokio::pin!(approvals);

        loop {
            let batch = std::mem::take(&mut actions);
            // Our accept and the completion come in one batch: the trust record must be durable and
            // visible before the accept goes out, or the peer can finish first and observe us unpaired.
            let completing = batch.iter().any(|a| matches!(a, Action::Complete));
            let mut stored = false;
            for action in batch {
                match action {
                    Action::Send { pair, timestamp } => {
                        if completing && pair && timestamp.is_none() && !stored {
                            self.persist(conn_id, bound_fp, clock).await?;
                            stored = true;
                        }
                        let mut packet = pair_packet(pair, timestamp);
                        // Only our own request carries the QR token, never an accept or a cancel.
                        if let (true, Some(_), Some(token)) = (pair, timestamp, &self.pairing.token)
                        {
                            packet
                                .body
                                .insert("gbToken".into(), Value::from(token.clone()));
                        }
                        self.conn.send(&packet).await?
                    }
                    Action::Prompt {
                        generation,
                        direction,
                        timestamp,
                        verification_key,
                        expires_in,
                    } => {
                        let req = ApprovalRequest {
                            direction,
                            peer_id: self.conn.info.peer.device_id.to_string(),
                            peer_name: self.conn.info.peer.device_name.clone(),
                            peer_cert_sha256: self.conn.info.peer_cert.fingerprint_hex(),
                            own_cert_sha256: own_fp.clone(),
                            verification_key,
                            timestamp,
                            expires_in,
                            presented_token: peer_token.clone(),
                            qr_pinned: self.pairing.token.is_some()
                                && direction == Direction::Outgoing,
                        };
                        // Supersedes any pending prompt; its approval can no longer be given.
                        let _ = prompt_tx.send((generation, req));
                    }
                    Action::Fail(f) => return Err(f.into()),
                    Action::Complete => {
                        if !stored {
                            self.persist(conn_id, bound_fp, clock).await?;
                        }
                        return Ok(self.into_trusted());
                    }
                }
            }

            let deadline = machine
                .deadline()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                biased;
                _ = sleep_until(deadline) => actions = machine.on_tick(now()),
                _ = &mut approvals => {}
                Some((generation, decision)) = decision_rx.recv() => {
                    actions = machine.on_local_decision(generation, decision, now());
                }
                line = self.conn.reader.read_line(self.conn.limits.packet_line) => {
                    let line = line.map_err(|e| match e {
                        KdeError::UnexpectedEof => KdeError::Pairing(PairingFailure::LinkClosed),
                        other => other,
                    })?;
                    if let Some(tap) = &self.conn.tap {
                        tap(false, &line);
                    }
                    let packet = Packet::from_line(&line)?;
                    actions = if packet.kind == PAIR_TYPE {
                        let body = PairBody::parse(&packet)?;
                        if body.pair && body.timestamp.is_some() && peer_token.is_none() {
                            peer_token = body.token.clone();
                        }
                        machine.on_pair(body, now())
                    } else {
                        machine.on_other_packet(now())
                    };
                }
            }
        }
    }

    async fn persist(
        &mut self,
        conn_id: crate::link::ConnectionId,
        fp: [u8; 32],
        clock: fn() -> i64,
    ) -> Result<()> {
        // The certificate we verified and showed to the user must be the one on
        // this connection, and it is the one we pin.
        if self.conn.info.connection_id != conn_id || self.conn.info.peer_cert.fingerprint() != fp {
            return Err(PairingFailure::BindingChanged.into());
        }
        let info = &self.conn.info;
        let peer = TrustedPeer {
            device_id: info.peer.device_id.clone(),
            name: info.peer.device_name.clone(),
            device_type: info.peer.device_type.as_str().to_owned(),
            protocol_version: info.peer.protocol_version,
            cert: CertInfo::parse(info.peer_cert.der())?,
            paired_at_unix: clock(),
        };
        // File lock + fsync: never on an async worker (a slow disk would freeze every other task).
        // The write is durable before the accept goes out (a crash must never leave the phone paired
        // with us and us not with it). A lock held by another process for longer than the lock
        // timeout (a stalled disk) is retried: the handshake is over and the peer is still waiting.
        let mut stored = Err(KdeError::Store("trust store write interrupted".into()));
        for _ in 0..PERSIST_ATTEMPTS {
            let (store, peer) = (self.store.clone(), peer.clone());
            stored = tokio::task::spawn_blocking(move || store.insert(peer))
                .await
                .unwrap_or_else(|_| Err(KdeError::Store("trust store write interrupted".into())));
            if !matches!(&stored, Err(KdeError::Store(m)) if m.starts_with("timed out")) {
                break;
            }
        }
        if let Err(e) = stored {
            // The peer may already consider us paired; tell it we are not.
            let _ = self.conn.send(&pair_packet(false, None)).await;
            return Err(e);
        }
        Ok(())
    }

    fn into_trusted(self) -> TrustedLink {
        TrustedLink::new(self.conn, self.store, self.own_cert, self.pairing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PairingConfig {
        PairingConfig::default()
    }

    fn machine() -> Machine {
        Machine::new(cfg(), vec![1, 2, 3], vec![9, 9])
    }

    fn at(mono: Instant, unix: i64) -> Now {
        Now { mono, unix }
    }

    fn send(pair: bool, ts: Option<i64>) -> Action {
        Action::Send {
            pair,
            timestamp: ts,
        }
    }

    fn accept() -> PairBody {
        PairBody {
            pair: true,
            timestamp: None,
            token: None,
        }
    }
    fn request(ts: i64) -> PairBody {
        PairBody {
            pair: true,
            timestamp: Some(ts),
            token: None,
        }
    }
    fn cancel() -> PairBody {
        PairBody {
            pair: false,
            timestamp: None,
            token: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn outgoing_needs_peer_accept_and_local_approval_in_either_order() {
        for peer_first in [false, true] {
            let t0 = Instant::now();
            let mut m = machine();
            let a = m.start(PairMode::Request, at(t0, 1000));
            assert_eq!(a[0], send(true, Some(1000)));
            assert!(matches!(
                a[1],
                Action::Prompt {
                    direction: Direction::Outgoing,
                    timestamp: 1000,
                    ..
                }
            ));
            let now = at(t0 + Duration::from_secs(1), 1001);
            if peer_first {
                assert!(
                    m.on_pair(accept(), now).is_empty(),
                    "peer accept alone must not complete"
                );
                assert_eq!(m.on_local_decision(1, true, now), vec![Action::Complete]);
            } else {
                assert!(
                    m.on_local_decision(1, true, now).is_empty(),
                    "local approval alone must not complete"
                );
                assert_eq!(m.on_pair(accept(), now), vec![Action::Complete]);
            }
            // Finished: nothing else does anything.
            assert!(m.on_pair(accept(), now).is_empty());
            assert!(m.on_local_decision(1, true, now).is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn crossed_request_is_not_an_accept() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::Request, at(t0, 1000));
        let now = at(t0, 1000);
        assert!(m.on_pair(request(1000), now).is_empty());
        assert!(
            m.on_local_decision(1, true, now).is_empty(),
            "still waiting for a genuine accept"
        );
        // Times out, notifying the peer.
        let late = at(t0 + Duration::from_secs(31), 1031);
        assert_eq!(
            m.on_tick(late),
            vec![send(false, None), Action::Fail(PairingFailure::Timeout)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn incoming_accept_is_sent_exactly_once() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        let a = m.on_pair(request(5000), at(t0, 5000));
        let Action::Prompt {
            direction: Direction::Incoming,
            verification_key: key,
            ..
        } = &a[0]
        else {
            panic!("{a:?}")
        };
        assert_eq!(*key, verification_key(&[1, 2, 3], &[9, 9], 5000));
        // Same timestamp again, and a bare accept: ignored.
        assert!(m.on_pair(request(5000), at(t0, 5000)).is_empty());
        assert!(m.on_pair(accept(), at(t0, 5000)).is_empty());
        let a = m.on_local_decision(1, true, at(t0, 5000));
        assert_eq!(a, vec![send(true, None), Action::Complete]);
        assert!(m.on_local_decision(1, true, at(t0, 5000)).is_empty());
    }

    fn prompt_of(a: &[Action]) -> (u64, String) {
        match a {
            [
                Action::Prompt {
                    generation,
                    verification_key,
                    ..
                },
            ] => (*generation, verification_key.clone()),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_request_replaces_the_prompt_and_stale_approval_is_refused() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        let (g1, k1) = prompt_of(&m.on_pair(request(5000), at(t0, 5000)));
        let later = at(t0 + Duration::from_secs(10), 5010);
        let (g2, k2) = prompt_of(&m.on_pair(request(5010), later));
        assert_ne!(g1, g2);
        assert_ne!(k1, k2, "the new key is derived from the new timestamp");
        assert_eq!(k2, verification_key(&[1, 2, 3], &[9, 9], 5010));
        // The user approved the old key: refused, nothing sent, still pending.
        assert!(m.on_local_decision(g1, true, later).is_empty());
        assert!(m.deadline().is_some());
        // The response window restarted for the new prompt.
        assert_eq!(m.deadline(), Some(later.mono + cfg().response_timeout));
        // Approving what is shown now completes, and the accept goes out exactly once.
        assert_eq!(
            m.on_local_decision(g2, true, later),
            vec![send(true, None), Action::Complete]
        );
        assert!(m.on_local_decision(g2, true, later).is_empty());
        assert!(m.on_pair(request(5011), later).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn replacements_cannot_extend_the_prompt_past_its_hard_cap() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        m.on_pair(request(5000), at(t0, 5000));
        let cap = cfg().response_timeout * PROMPT_CAP_FACTOR;
        // A new request every 20 s, inside the 25 s window, 4 per minute.
        for i in 1..=3i64 {
            let t = at(t0 + Duration::from_secs(20 * i as u64), 5000 + 20 * i);
            m.on_pair(request(5000 + 20 * i), t);
            assert!(m.deadline().unwrap() <= t0 + cap);
        }
        assert_eq!(m.deadline(), Some(t0 + cap));
        let t = at(t0 + cap, 5000 + 75);
        assert!(matches!(
            m.on_pair(request(5100), t).last(),
            Some(Action::Fail(PairingFailure::Timeout))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn stale_rejection_does_not_cancel_the_new_prompt() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        let (g1, _) = prompt_of(&m.on_pair(request(5000), at(t0, 5000)));
        let (g2, _) = prompt_of(&m.on_pair(request(5001), at(t0, 5001)));
        assert!(m.on_local_decision(g1, false, at(t0, 5001)).is_empty());
        assert_eq!(
            m.on_local_decision(g2, false, at(t0, 5001)),
            vec![
                send(false, None),
                Action::Fail(PairingFailure::RejectedLocally)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn replacement_requests_are_rate_limited() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        let mut last = 0;
        for i in 0..MAX_REPLACEMENTS as i64 {
            let a = m.on_pair(request(5000 + i), at(t0, 5000 + i));
            last = prompt_of(&a).0;
        }
        // Sixth request inside the minute: ignored, the fifth prompt stays.
        assert!(m.on_pair(request(5100), at(t0, 5001)).is_empty());
        assert!(m.on_local_decision(last, true, at(t0, 5001)).len() == 2);

        // After the window the budget is back.
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        for i in 0..MAX_REPLACEMENTS as i64 {
            m.on_pair(request(5000 + i), at(t0, 5000));
        }
        let late = at(t0 + REPLACEMENT_WINDOW, 5000);
        // (prompt itself expired meanwhile: the request now fails with a timeout notice)
        assert!(matches!(
            m.on_pair(request(5200), late).last(),
            Some(Action::Fail(PairingFailure::Timeout))
        ));
    }

    /// The request captured from stock Android 1.37.0, replayed twice the way a double tap
    /// produces it (second timestamp later), with the clock set to the capture time.
    #[tokio::test(start_paused = true)]
    async fn captured_android_request_replayed_twice_replaces_the_prompt() {
        let raw =
            include_str!("../tests/fixtures/kdeconnect-android-real/android-pair-request.json");
        let first =
            PairBody::parse(&Packet::from_line(raw.trim_end().as_bytes()).unwrap()).unwrap();
        let ts = first.timestamp.unwrap();
        let second = PairBody {
            timestamp: Some(ts + 7),
            ..first.clone()
        };
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, ts));
        let (g1, k1) = prompt_of(&m.on_pair(first.clone(), at(t0, ts)));
        // A literal duplicate is ignored; the re-timestamped one replaces.
        assert!(m.on_pair(first, at(t0, ts)).is_empty());
        let (g2, k2) = prompt_of(&m.on_pair(second, at(t0, ts + 7)));
        assert_ne!(k1, k2);
        assert!(m.on_local_decision(g1, true, at(t0, ts + 7)).is_empty());
        assert_eq!(
            m.on_local_decision(g2, true, at(t0, ts + 7)),
            vec![send(true, None), Action::Complete]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn replacement_with_bad_clock_or_same_timestamp_changes_nothing() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        let (g1, _) = prompt_of(&m.on_pair(request(5000), at(t0, 5000)));
        assert!(m.on_pair(request(5000), at(t0, 5000)).is_empty());
        assert!(m.on_pair(request(5000 + 1801), at(t0, 5000)).is_empty());
        assert_eq!(m.on_local_decision(g1, true, at(t0, 5000)).len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn approval_after_timeout_never_completes() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        m.on_pair(request(5000), at(t0, 5000));
        let late = at(t0 + Duration::from_secs(26), 5026);
        let a = m.on_local_decision(1, true, late);
        assert_eq!(
            a,
            vec![send(false, None), Action::Fail(PairingFailure::Timeout)]
        );
        assert!(!a.contains(&Action::Complete));
        assert!(m.on_local_decision(1, true, late).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn timestamp_window() {
        for (ts, ok) in [
            (5000 - 1800, true),
            (5000 + 1800, true),
            (5000 - 1801, false),
            (5000 + 1801, false),
            (-1, false),
            (0, false),
        ] {
            let t0 = Instant::now();
            let mut m = machine();
            m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
            let a = m.on_pair(request(ts), at(t0, 5000));
            assert_eq!(matches!(a[0], Action::Prompt { .. }), ok, "ts {ts}");
            if !ok {
                assert_eq!(
                    a,
                    vec![send(false, None), Action::Fail(PairingFailure::ClockSkew)]
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn accept_without_request_is_ignored() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        assert!(m.on_pair(accept(), at(t0, 5000)).is_empty());
        assert!(m.on_local_decision(1, true, at(t0, 5000)).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn rejections_and_cancellations() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::Request, at(t0, 1));
        assert_eq!(
            m.on_pair(cancel(), at(t0, 1)),
            vec![Action::Fail(PairingFailure::RejectedByPeer)]
        );

        let mut m = machine();
        m.start(PairMode::Request, at(t0, 1));
        assert_eq!(
            m.on_local_decision(1, false, at(t0, 1)),
            vec![
                send(false, None),
                Action::Fail(PairingFailure::RejectedLocally)
            ]
        );

        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 1));
        m.on_pair(request(1), at(t0, 1));
        assert_eq!(
            m.on_pair(cancel(), at(t0, 1)),
            vec![Action::Fail(PairingFailure::RejectedByPeer)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unexpected_budget_and_single_hint() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 1));
        assert_eq!(m.on_other_packet(at(t0, 1)), vec![send(false, None)]);
        for _ in 0..7 {
            assert!(
                m.on_other_packet(at(t0, 1)).is_empty(),
                "hint is sent at most once"
            );
        }
        assert_eq!(
            m.on_other_packet(at(t0, 1)),
            vec![Action::Fail(PairingFailure::TooManyUnexpectedPackets)]
        );
    }

    #[test]
    fn pair_body_parsing() {
        let p = |v: Value| {
            let mut pk = pair_packet(true, None);
            pk.body = v.as_object().unwrap().clone();
            PairBody::parse(&pk)
        };
        assert_eq!(p(serde_json::json!({"pair": true})).unwrap(), accept());
        assert_eq!(
            p(serde_json::json!({"pair": true, "timestamp": 7})).unwrap(),
            request(7)
        );
        assert!(p(serde_json::json!({"pair": "true"})).is_err());
        assert!(p(serde_json::json!({})).is_err());
        assert!(p(serde_json::json!({"pair": true, "timestamp": "7"})).is_err());
        assert!(p(serde_json::json!({"pair": true, "timestamp": 7.5})).is_err());
        assert!(p(serde_json::json!({"pair": true, "timestamp": null})).is_err());
    }

    #[test]
    fn verification_key_is_order_independent_and_timestamp_bound() {
        let a = [1u8, 2, 3, 4];
        let b = [9u8, 8, 7];
        let k = verification_key(&a, &b, 1737228658);
        assert_eq!(k, verification_key(&b, &a, 1737228658));
        assert_ne!(k, verification_key(&a, &b, 1737228659));
        assert_eq!(k.len(), 8);
        assert!(
            k.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
        // Larger-first ordering is part of the algorithm: prefix compares smaller.
        let (long, short) = ([1u8, 2, 3], [1u8, 2]);
        let mut h = Sha256::new();
        h.update(long);
        h.update(short);
        h.update(b"5");
        assert_eq!(
            verification_key(&short, &long, 5),
            hex(&h.finalize()[..4]).to_uppercase()
        );
    }

    fn fixture(pem: &str) -> CertInfo {
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        let der = CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
        CertInfo::parse(der.as_ref()).unwrap()
    }

    /// Expected values were computed independently with `openssl pkey -pubin
    /// -outform DER` + `sha256sum` over freshly generated fixtures (extension-less,
    /// v3, serial 1, ecdsa-with-SHA512, like stock Android certificates). The fixture
    /// commands are recorded in docs/kdeconnect-interop.md.
    #[test]
    fn independent_verification_key_vectors() {
        let a = fixture(include_str!("../tests/fixtures/a.crt.pem"));
        let b = fixture(include_str!("../tests/fixtures/b.crt.pem"));
        assert_eq!(a.common_name(), "0a1b2c3d4e5f60718293a4b5c6d7e8f9");
        for (x, y) in [(&a, &b), (&b, &a)] {
            assert_eq!(
                verification_key(x.spki_der(), y.spki_der(), 1737228658),
                "DDB9D41A"
            );
            assert_eq!(
                verification_key(x.spki_der(), y.spki_der(), 2737228658),
                "2DE0EE73"
            );
            assert_eq!(
                verification_key(x.spki_der(), y.spki_der(), 1737228659),
                "A93C13E7"
            );
        }
    }

    /// Cross-check against certificates produced by the stock Android app. They are
    /// not vendored (they live inside GPL test sources); point the variable at a
    /// directory holding `a.pem` and `b.pem` extracted from the pinned checkout.
    /// Expected values are the numbers asserted by the upstream test at the pinned commit.
    #[test]
    #[ignore = "needs GEEKBRIDGE_STOCK_CERT_DIR with a.pem and b.pem from the pinned kdeconnect-android test"]
    fn stock_android_certificates_and_vectors() {
        let dir = std::env::var("GEEKBRIDGE_STOCK_CERT_DIR").expect("GEEKBRIDGE_STOCK_CERT_DIR");
        let read = |n: &str| fixture(&std::fs::read_to_string(format!("{dir}/{n}.pem")).unwrap());
        let (a, b) = (read("a"), read("b"));
        assert_eq!(a.common_name(), "ee061a75_e403_4ecc_9261_5ffe2722f698");
        assert!(crate::identity::DeviceId::parse(a.common_name()).is_ok());
        for (x, y) in [(&a, &b), (&b, &a)] {
            assert_eq!(
                verification_key(x.spki_der(), y.spki_der(), 1737228658),
                "54DC916E"
            );
            assert_eq!(
                verification_key(x.spki_der(), y.spki_der(), 2737228658),
                "8C07153A"
            );
        }
    }
}
