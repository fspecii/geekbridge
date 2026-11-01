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
//! * Duplicate or unsolicited pair packets never change state, never restart a
//!   timer and never cause a second accept to be emitted. A small budget of
//!   them is tolerated before the link is dropped.
//! * Non-pair packets are dropped, never delivered.
//! * Pairing state dies with the connection.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::time::{Instant, sleep_until};

use crate::cert::{CertInfo, hex, unix_now};
use crate::error::{KdeError, PairingFailure, Result};
use crate::link::{PendingLink, TrustedLink};
use crate::packet::Packet;
use crate::store::TrustedPeer;

pub const PAIR_TYPE: &str = "kdeconnect.pair";

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
}

impl Default for PairingConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            response_timeout: Duration::from_secs(25),
            max_skew_secs: 1800,
            max_unexpected: 8,
            clock: unix_now,
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
        Ok(Self { pair, timestamp })
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
    Prompt {
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
                vec![
                    Action::Send {
                        pair: true,
                        timestamp: Some(ts),
                    },
                    Action::Prompt {
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
                let expires_in = self.cfg.response_timeout;
                self.state = State::RequestedByPeer {
                    deadline: now.mono + expires_in,
                };
                vec![Action::Prompt {
                    direction: Direction::Incoming,
                    timestamp: ts,
                    verification_key: self.key(ts),
                    expires_in,
                }]
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

            (State::RequestedByPeer { .. }, true, _) => self.note_unexpected().unwrap_or_default(),
            (State::RequestedByPeer { .. }, false, _) => {
                self.fail(PairingFailure::RejectedByPeer, false)
            }

            (State::Finished, _, _) => vec![],
        }
    }

    pub(crate) fn on_local_decision(&mut self, approve: bool, now: Now) -> Vec<Action> {
        if let Some(a) = self.expired(now) {
            return a;
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

type Approval<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

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
        let mut approver = Some(approver);
        let mut approval: Option<Approval<'_>> = None;

        loop {
            for action in std::mem::take(&mut actions) {
                match action {
                    Action::Send { pair, timestamp } => {
                        self.conn.send(&pair_packet(pair, timestamp)).await?
                    }
                    Action::Prompt {
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
                        };
                        // The machine prompts exactly once per attempt.
                        let ap = approver
                            .take()
                            .ok_or(PairingFailure::Malformed("second approval prompt"))?;
                        approval = Some(Box::pin(ap.approve(req)));
                    }
                    Action::Fail(f) => return Err(f.into()),
                    Action::Complete => return self.complete(conn_id, bound_fp, clock).await,
                }
            }

            let deadline = machine
                .deadline()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                biased;
                _ = sleep_until(deadline) => actions = machine.on_tick(now()),
                decision = async {
                    match approval.as_mut() {
                        Some(f) => f.as_mut().await,
                        None => std::future::pending().await,
                    }
                } => {
                    approval = None;
                    actions = machine.on_local_decision(decision, now());
                }
                line = self.conn.reader.read_line(self.conn.limits.packet_line) => {
                    let line = line.map_err(|e| match e {
                        KdeError::UnexpectedEof => KdeError::Pairing(PairingFailure::LinkClosed),
                        other => other,
                    })?;
                    let packet = Packet::from_line(&line)?;
                    actions = if packet.kind == PAIR_TYPE {
                        machine.on_pair(PairBody::parse(&packet)?, now())
                    } else {
                        machine.on_other_packet(now())
                    };
                }
            }
        }
    }

    async fn complete(
        mut self,
        conn_id: crate::link::ConnectionId,
        fp: [u8; 32],
        clock: fn() -> i64,
    ) -> Result<TrustedLink> {
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
        if let Err(e) = self.store.insert(peer) {
            // The peer may already consider us paired; tell it we are not.
            let _ = self.conn.send(&pair_packet(false, None)).await;
            return Err(e);
        }
        Ok(TrustedLink::new(self.conn, self.store))
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
        }
    }
    fn request(ts: i64) -> PairBody {
        PairBody {
            pair: true,
            timestamp: Some(ts),
        }
    }
    fn cancel() -> PairBody {
        PairBody {
            pair: false,
            timestamp: None,
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
                assert_eq!(m.on_local_decision(true, now), vec![Action::Complete]);
            } else {
                assert!(
                    m.on_local_decision(true, now).is_empty(),
                    "local approval alone must not complete"
                );
                assert_eq!(m.on_pair(accept(), now), vec![Action::Complete]);
            }
            // Finished: nothing else does anything.
            assert!(m.on_pair(accept(), now).is_empty());
            assert!(m.on_local_decision(true, now).is_empty());
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
            m.on_local_decision(true, now).is_empty(),
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
        // Duplicate request with a different timestamp: ignored, key and timer untouched.
        assert!(m.on_pair(request(5001), at(t0, 5000)).is_empty());
        assert!(m.on_pair(accept(), at(t0, 5000)).is_empty());
        let a = m.on_local_decision(true, at(t0, 5000));
        assert_eq!(a, vec![send(true, None), Action::Complete]);
        assert!(m.on_local_decision(true, at(t0, 5000)).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn approval_after_timeout_never_completes() {
        let t0 = Instant::now();
        let mut m = machine();
        m.start(PairMode::AwaitPeerRequest, at(t0, 5000));
        m.on_pair(request(5000), at(t0, 5000));
        let late = at(t0 + Duration::from_secs(26), 5026);
        let a = m.on_local_decision(true, late);
        assert_eq!(
            a,
            vec![send(false, None), Action::Fail(PairingFailure::Timeout)]
        );
        assert!(!a.contains(&Action::Complete));
        assert!(m.on_local_decision(true, late).is_empty());
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
        assert!(m.on_local_decision(true, at(t0, 5000)).is_empty());
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
            m.on_local_decision(false, at(t0, 1)),
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
    /// commands are recorded in `docs/architecture.md`.
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
