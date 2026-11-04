// SPDX-License-Identifier: GPL-3.0-or-later
//! The real hub (feature `callwifi`). See the parent module.
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use bridge_callwifi::codec::{Decoder, Encoder};
use bridge_callwifi::gate::{Action, Gate, Side};
use bridge_callwifi::jitter::Out as JitterOut;
use bridge_callwifi::selftest::Verdict;
use bridge_callwifi::wire::{self, Dir, Frame, Msg, Ready, Uplink};
use bridge_kde::Packet;
use serde_json::{Value, json};

use super::super::{Core, Ctl};
use crate::dispatch::{ApiError, err};

/// The per-computer phone grant that must be on (`GrantFeature.CallAudio`).
pub(in crate::transport) const GRANT: &str = "callaudio";
const MAX_EVENTS: usize = 16;
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Decoded microphone frames waiting for the helper (speak back). 5 frames = 100 ms; older ones are dropped.
const MAX_UPLINK: usize = 5;
const PENDING_MS: u64 = 10_000;

pub(in crate::transport) fn incoming_types() -> Vec<String> {
    wire::PHONE_INCOMING
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}
pub(in crate::transport) fn outgoing_types() -> Vec<String> {
    // What a computer accepts: offer, frame, stats, stop, ready, speak.
    wire::COMPUTER_INCOMING
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}
pub(in crate::transport) fn is_kind(kind: &str) -> bool {
    wire::is_callaudio_kind(kind)
}

struct Inner {
    enabled: bool,
    gate: Gate,
    ready: Ready,
    /// What was last announced to the computers (switch, ready); an unchanged config sends nothing.
    announced: Option<(bool, Ready)>,
    /// The connection the live (or pending) session runs on, and that computer's name.
    conn: Option<(u64, String)>,
    seq: u64,
    events: VecDeque<(u64, Value)>,
    delivered: u64,
    stopped: bool,
    enc: Option<Encoder>,
    dec: Option<Decoder>,
    uplink: VecDeque<Vec<i16>>,
    down_seq: u32,
    /// When the pending offer was made (hub clock, ms); an offer nobody answers is dropped.
    offered_at: u64,
    sent: u64,
    got_up: u64,
    /// The uplink of the live session, for the receiver report to the computer: the last sequence number, frames
    /// missing in the sequence, frames dropped because the helper did not take them in time, the last report.
    up_last_seq: Option<u32>,
    up_lost: u64,
    up_dropped: u64,
    up_reported_at: u64,
}

pub(in crate::transport) struct Hub {
    st: Mutex<Inner>,
    cv: Condvar,
    epoch: Instant,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            st: Mutex::new(Inner {
                enabled: false,
                gate: Gate::new(Side::Phone),
                ready: Ready {
                    ready: false,
                    listen: false,
                    speak: false,
                    test: Verdict::NotRun,
                },
                announced: None,
                conn: None,
                seq: 0,
                events: VecDeque::new(),
                delivered: 0,
                stopped: false,
                enc: None,
                dec: None,
                uplink: VecDeque::new(),
                down_seq: 0,
                offered_at: 0,
                sent: 0,
                got_up: 0,
                up_last_seq: None,
                up_lost: 0,
                up_dropped: 0,
                up_reported_at: 0,
            }),
            cv: Condvar::new(),
            epoch: Instant::now(),
        }
    }
}

/// Packets the hub wants sent, to the connection `conn`.
type Sends = Vec<Msg>;

impl Inner {
    fn event(&mut self, body: Value) {
        self.seq += 1;
        let mut body = body;
        if let Some(m) = body.as_object_mut() {
            m.insert("seq".into(), json!(self.seq));
        }
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back((self.seq, body));
    }

    /// Turn gate actions into outgoing packets and events for Kotlin.
    fn apply(&mut self, actions: Vec<Action>, now_ms: u64) -> Sends {
        let mut out = Vec::new();
        let computer = self.conn.as_ref().map(|c| c.1.clone()).unwrap_or_default();
        for a in actions {
            match a {
                Action::Send(m) => {
                    if let Msg::Offer(o) = &m {
                        let session = o.session;
                        self.down_seq = 0;
                        self.offered_at = now_ms;
                        self.uplink.clear();
                        self.up_last_seq = None;
                        self.up_lost = 0;
                        self.up_dropped = 0;
                        self.got_up = 0;
                        self.event(
                            json!({"ev": "start", "session": session, "computer": computer}),
                        );
                    }
                    out.push(m);
                }
                Action::IndicatorOn => {
                    self.event(json!({"ev": "indicator", "on": true, "computer": computer}));
                }
                Action::IndicatorOff => {
                    self.uplink.clear();
                    self.event(json!({"ev": "stop"}));
                    self.conn = None;
                }
                Action::SpeakState(on) => {
                    if !on {
                        self.uplink.clear();
                    }
                    self.event(json!({"ev": "speak", "on": on}));
                }
                Action::Accept(f) => {
                    self.got_up += 1;
                    if let Some(last) = self.up_last_seq {
                        let gap = f.seq.wrapping_sub(last).wrapping_sub(1);
                        // A huge jump is a restart of the computer's microphone, not a loss.
                        if gap < 500 {
                            self.up_lost += u64::from(gap);
                        }
                    }
                    self.up_last_seq = Some(f.seq);
                    if self.dec.is_none() {
                        self.dec = Decoder::new().ok();
                    }
                    if let Some(d) = self.dec.as_mut() {
                        let pcm = d.render(&JitterOut::Packet(f.opus)).to_vec();
                        if self.uplink.len() >= MAX_UPLINK {
                            self.uplink.pop_front();
                            self.up_dropped += 1;
                        }
                        self.uplink.push_back(pcm);
                    }
                }
                Action::Drop(_) | Action::PeerReady(_) | Action::PeerStats(_) => {}
            }
        }
        out
    }
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    pub(in crate::transport) fn shutdown(&self) {
        let mut g = self.lock();
        g.stopped = true;
        let acts = g.gate.link_lost();
        g.apply(acts, self.now_ms());
        g.events.clear();
        drop(g);
        self.cv.notify_all();
    }

    /// The link carrying the session is gone.
    pub(in crate::transport) fn link_lost(&self, connection: u64) {
        let mut g = self.lock();
        if g.conn.as_ref().is_some_and(|c| c.0 == connection) {
            let acts = g.gate.link_lost();
            g.apply(acts, self.now_ms());
            drop(g);
            self.cv.notify_all();
        }
    }

    /// Block until an event newer than `since` exists, the host stops, or `timeout` passes. Also the gate's clock.
    pub(in crate::transport) fn wait(
        &self,
        since: u64,
        timeout: Duration,
    ) -> (Value, Sends, Option<u64>) {
        let deadline = Instant::now() + timeout.min(MAX_WAIT);
        let mut g = self.lock();
        let mut sends = Vec::new();
        let mut conn = None;
        loop {
            let floor = since.max(g.delivered);
            g.events.retain(|e| e.0 > floor);
            if !g.events.is_empty() || g.stopped {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // Wake at least once a second so an idle session times out on schedule.
            let step = left.min(Duration::from_secs(1));
            g = self
                .cv
                .wait_timeout(g, step)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            let now = self.now_ms();
            let mut acts = g.gate.tick(now);
            // An offer the computer never answers is withdrawn after 10 s, so the helper is not left capturing.
            if g.gate.session().is_some_and(|s| !s.live)
                && now.saturating_sub(g.offered_at) > PENDING_MS
            {
                acts.extend(g.gate.stop());
            }
            if !acts.is_empty() {
                conn = g.conn.as_ref().map(|c| c.0);
                sends.extend(g.apply(acts, now));
            }
        }
        if let Some(last) = g.events.back() {
            g.delivered = g.delivered.max(last.0);
        }
        let body = json!({
            "events": g.events.iter().map(|e| e.1.clone()).collect::<Vec<_>>(),
            "last_seq": g.seq,
            "stopped": g.stopped,
            "enabled": g.enabled,
        });
        (body, sends, conn)
    }
}

fn packet_of(m: &Msg) -> Packet {
    Packet::new(m.kind(), wire::body(m))
}

impl Core {
    fn callwifi_send(&self, connection: u64, msgs: Sends) {
        for m in msgs {
            let kind = m.kind();
            // A stop is always delivered (it is how a revoked grant reaches the computer); everything else
            // needs the grant.
            let ctls = if matches!(m, Msg::Stop { .. }) {
                self.accepting_ctls(kind, connection)
            } else {
                self.permitted_ctls(GRANT, kind, Some(connection))
            };
            for ctl in ctls {
                let _ = ctl.try_send(Ctl::Packet(Box::new(packet_of(&m))));
            }
        }
    }

    /// Kotlin: the switch, the self-test result and the helper's uplink route. Sends `ready` to the allowed computers.
    pub(in crate::transport) fn callwifi_config(
        &self,
        enabled: bool,
        helper_up: bool,
        verdict: Verdict,
        speak_allowed: bool,
        uplink: Uplink,
    ) -> Value {
        let (ready, sends, conn, changed) = {
            let mut g = self.callwifi.lock();
            g.enabled = enabled;
            g.gate.uplink_route = uplink;
            let mut acts = g.gate.set_consent(false, enabled);
            acts.extend(g.gate.set_speak_allowed(enabled && speak_allowed));
            let conn = g.conn.as_ref().map(|c| c.0);
            let sends = g.apply(acts, self.callwifi.now_ms());
            let listen = enabled && helper_up && verdict.can_listen();
            let speak = listen && speak_allowed && uplink != Uplink::None;
            g.ready = Ready {
                ready: enabled && helper_up,
                listen,
                speak,
                test: verdict,
            };
            let now = (enabled, g.ready.clone());
            let changed = g.announced.as_ref() != Some(&now);
            g.announced = Some(now);
            (g.ready.clone(), sends, conn, changed)
        };
        self.callwifi.cv.notify_all();
        if let Some(c) = conn {
            self.callwifi_send(c, sends);
        }
        let ids: Vec<u64> = if changed {
            self.trusted_ctls_ids()
        } else {
            Vec::new()
        };
        for id in ids {
            self.callwifi_send(id, vec![Msg::Ready(ready.clone())]);
        }
        json!({"ready": ready.ready, "listen": ready.listen, "speak": ready.speak})
    }

    fn accepting_ctls(&self, kind: &str, connection: u64) -> Vec<tokio::sync::mpsc::Sender<Ctl>> {
        use super::super::Phase;
        self.lock()
            .conns
            .values()
            .filter(|c| c.id == connection && c.phase == Phase::Trusted)
            .filter(|c| c.peer_accepts.iter().any(|a| a == kind))
            .map(|c| c.ctl.clone())
            .collect()
    }

    fn trusted_ctls_ids(&self) -> Vec<u64> {
        use super::super::Phase;
        self.lock()
            .conns
            .values()
            .filter(|c| c.phase == Phase::Trusted)
            .map(|c| c.id)
            .collect()
    }

    /// A computer just became trusted: tell it where the helper stands (only if the switch is on).
    pub(in crate::transport) fn callwifi_connected(&self, id: u64) {
        let ready = {
            let g = self.callwifi.lock();
            g.enabled.then(|| g.ready.clone())
        };
        if let Some(r) = ready {
            self.callwifi_send(id, vec![Msg::Ready(r)]);
        }
    }

    /// Kotlin: a phone call is live or not.
    pub(in crate::transport) fn callwifi_call(&self, active: bool) -> Value {
        let (sends, conn) = {
            let mut g = self.callwifi.lock();
            let acts = g.gate.set_call_active(active);
            let conn = g.conn.as_ref().map(|c| c.0);
            (g.apply(acts, self.callwifi.now_ms()), conn)
        };
        self.callwifi.cv.notify_all();
        if let Some(c) = conn {
            self.callwifi_send(c, sends);
        }
        json!({"ok": true})
    }

    /// Kotlin: end the session now (Stop in the notification, the helper died, the user turned a switch off).
    pub(in crate::transport) fn callwifi_stop(&self) -> Value {
        let (sends, conn) = {
            let mut g = self.callwifi.lock();
            let conn = g.conn.as_ref().map(|c| c.0);
            let acts = g.gate.stop();
            (g.apply(acts, self.callwifi.now_ms()), conn)
        };
        self.callwifi.cv.notify_all();
        if let Some(c) = conn {
            self.callwifi_send(c, sends);
        }
        json!({"ok": true})
    }

    /// Kotlin: one 20 ms frame (320 samples, s16le, base64) of the far party. Dropped unless the gate says it may flow.
    pub(in crate::transport) fn callwifi_frame(
        &self,
        pcm_b64: &str,
        cap: u32,
    ) -> Result<Value, ApiError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(pcm_b64)
            .map_err(|_| err("invalid_field", "a field has the wrong type or value"))?;
        if bytes.len() != wire::FRAME_SAMPLES * 2 {
            return Err(err("invalid_field", "a field has the wrong type or value"));
        }
        let pcm: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let (frame, conn) = {
            let mut g = self.callwifi.lock();
            if !g.gate.may_send(Dir::Down) {
                return Ok(json!({"sent": false}));
            }
            g.gate.note_sent(self.callwifi.now_ms());
            let Some(session) = g.gate.session().map(|s| s.id) else {
                return Ok(json!({"sent": false}));
            };
            if g.enc.is_none() {
                g.enc = Encoder::new().ok();
            }
            let seq = g.down_seq;
            let opus = match g.enc.as_mut().map(|e| e.encode(&pcm)) {
                Some(Ok(o)) => o,
                _ => return Ok(json!({"sent": false})),
            };
            g.down_seq = seq.wrapping_add(1);
            g.sent += 1;
            let conn = g.conn.as_ref().map(|c| c.0);
            (
                Msg::Frame(Frame {
                    session,
                    dir: Dir::Down,
                    seq,
                    ts: seq.wrapping_mul(wire::FRAME_SAMPLES as u32),
                    cap,
                    opus,
                }),
                conn,
            )
        };
        if let Some(c) = conn {
            self.callwifi_send(c, vec![frame]);
        }
        Ok(json!({"sent": true}))
    }

    /// Kotlin: events (`start`, `indicator`, `stop`, `speak`), blocking up to `timeout`.
    pub(in crate::transport) fn callwifi_wait(&self, since: u64, timeout: Duration) -> Value {
        let (body, sends, conn) = self.callwifi.wait(since, timeout);
        if let Some(c) = conn {
            self.callwifi_send(c, sends);
        }
        body
    }

    /// Kotlin: decoded microphone frames for the helper's injection route (speak back), as base64 s16le. While
    /// speaking, about once a second it also sends the computer a receiver report for the uplink (numbers only).
    pub(in crate::transport) fn callwifi_pull(&self) -> Value {
        let now = self.callwifi.now_ms();
        let mut g = self.callwifi.lock();
        let report = match g.gate.session() {
            Some(s) if s.live && s.up && now.saturating_sub(g.up_reported_at) >= 1000 => {
                Some(Msg::Stats(wire::Stats {
                    session: s.id,
                    dir: Dir::Up,
                    got: g.got_up,
                    lost: g.up_lost,
                    late: g.up_dropped,
                    depth_ms: (g.uplink.len() * 20) as u32,
                }))
            }
            _ => None,
        };
        let report_conn = report.as_ref().and_then(|_| g.conn.as_ref().map(|c| c.0));
        if report_conn.is_some() {
            g.up_reported_at = now;
        }
        let frames: Vec<String> = g
            .uplink
            .drain(..)
            .map(|f| {
                let mut b = Vec::with_capacity(f.len() * 2);
                for s in f {
                    b.extend_from_slice(&s.to_le_bytes());
                }
                base64::engine::general_purpose::STANDARD.encode(b)
            })
            .collect();
        drop(g);
        if let (Some(r), Some(c)) = (report, report_conn) {
            self.callwifi_send(c, vec![r]);
        }
        json!({"frames": frames})
    }

    pub(in crate::transport) fn callwifi_status(&self) -> Value {
        let g = self.callwifi.lock();
        json!({
            "enabled": g.enabled,
            "session": g.gate.session().map(|s| json!({"id": s.id, "live": s.live, "speaking": s.up})),
            "sent": g.sent,
            "got_up": g.got_up,
            "up_lost": g.up_lost,
            "up_dropped": g.up_dropped,
            "dropped": g.gate.dropped,
        })
    }

    /// Grants or connections changed: end a session whose computer may no longer use it. Within the next call.
    pub(in crate::transport) fn callwifi_revalidate(&self) {
        let conn = self.callwifi.lock().conn.as_ref().map(|c| c.0);
        if let Some(c) = conn
            && !self.permitted(GRANT, c)
        {
            let (sends, conn) = {
                let mut g = self.callwifi.lock();
                let acts = g.gate.set_consent(false, false);
                let conn = g.conn.as_ref().map(|c| c.0);
                // set_consent(false,false) also cleared the switch; restore the real one without a session.
                let enabled = g.enabled;
                g.gate.set_consent(false, enabled);
                (g.apply(acts, self.callwifi.now_ms()), conn)
            };
            self.callwifi.cv.notify_all();
            if let Some(c) = conn {
                self.callwifi_send(c, sends);
            }
        }
    }

    /// A `geekbridge.callaudio.*` packet from a trusted computer. True if it was acted on or rightly refused.
    pub(in crate::transport) fn incoming_callwifi(&self, connection_id: u64, p: &Packet) -> bool {
        if p.payload_size.is_some() || p.payload_transfer_info.is_some() {
            return false;
        }
        let Ok(msg) = wire::parse(&p.kind, &p.body) else {
            return false;
        };
        let name = self
            .lock()
            .conns
            .get(&connection_id)
            .map(|c| c.peer_name.clone())
            .unwrap_or_default();
        let permitted = self.permitted(GRANT, connection_id);
        let now = self.callwifi.now_ms();
        let (sends, conn) = {
            let mut g = self.callwifi.lock();
            if !g.enabled || !permitted {
                // Refuse without a trace: a request gets a stop so the computer's button resets; the rest is dropped.
                drop(g);
                if let Msg::Request { session } = msg {
                    self.callwifi_send(
                        connection_id,
                        vec![Msg::Stop {
                            session,
                            reason: Some("not_available".into()),
                        }],
                    );
                }
                return true;
            }
            // A session belongs to one connection; nobody else may touch it.
            if let Some((c, _)) = g.conn.as_ref()
                && *c != connection_id
            {
                if let Msg::Request { session } = msg {
                    drop(g);
                    self.callwifi_send(
                        connection_id,
                        vec![Msg::Stop {
                            session,
                            reason: Some("busy".into()),
                        }],
                    );
                }
                return true;
            }
            if let Msg::Request { session } = msg {
                // Offer only when the helper is up and the self-test found the far party's audio.
                if !g.ready.listen {
                    drop(g);
                    self.callwifi_send(
                        connection_id,
                        vec![Msg::Stop {
                            session,
                            reason: Some("not_ready".into()),
                        }],
                    );
                    return true;
                }
                g.conn = Some((connection_id, name));
            }
            let acts = g.gate.on_msg(msg, now);
            if g.gate.session().is_none() {
                g.conn = None;
            }
            let conn = g.conn.as_ref().map(|c| c.0).or(Some(connection_id));
            (g.apply(acts, now), conn)
        };
        self.callwifi.cv.notify_all();
        if let Some(c) = conn {
            self.callwifi_send(c, sends);
        }
        true
    }
}
