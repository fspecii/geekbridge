//! Phone data relay: contacts, calendar and SMS packets between the trusted link and Kotlin.
//!
//! Contract: `docs/phone-data-link.md`. Rust does not interpret any of this data. It
//!
//! * declares the capabilities of the features the app enabled at start (`data` start argument),
//! * turns desktop requests of an enabled feature into *events* Kotlin collects with a blocking
//!   `transport_data_wait` (a disabled feature's packets are dropped, never queued),
//! * sends what Kotlin hands to `transport_data_send`, but only allow-listed outgoing kinds of an
//!   enabled feature, bounded in size and rate.
//!
//! Content (contacts, event text, message bodies) is never logged or kept beyond the bounded,
//! short-lived event queue. Packets with a payload are refused.
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use bridge_kde::Packet;
use serde_json::{Map, Value, json};

use super::{Core, Ctl, Phase};
use crate::dispatch::{ApiError, err};

const MAX_EVENTS: usize = 64;
const MAX_WAIT: Duration = Duration::from_secs(25);
/// A request nobody collected within this long is dropped, so a restarted waiter cannot act on a
/// stale one (an old SMS send must never be replayed).
const EVENT_TTL: Duration = Duration::from_secs(60);
/// Largest serialized body `transport_data_send` accepts.
pub(in crate::transport) const MAX_SEND_BODY_BYTES: usize = 1024 * 1024;
const RATE_BURST: f64 = 20.0;
/// 20 packets per 10 s sustained.
const RATE_PER_SEC: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::transport) enum Feature {
    Contacts,
    Calendar,
    Sms,
}

impl Feature {
    const ALL: [Feature; 3] = [Feature::Contacts, Feature::Calendar, Feature::Sms];

    fn incoming(self) -> &'static [&'static str] {
        match self {
            Feature::Contacts => &[
                "kdeconnect.contacts.request_all_uids_timestamps",
                "kdeconnect.contacts.request_vcards_by_uid",
            ],
            Feature::Calendar => &["geekbridge.calendar.request"],
            Feature::Sms => &[
                "kdeconnect.sms.request_conversations",
                "kdeconnect.sms.request_conversation",
                "kdeconnect.sms.request",
            ],
        }
    }

    fn outgoing(self) -> &'static [&'static str] {
        match self {
            Feature::Contacts => &[
                "kdeconnect.contacts.response_uids_timestamps",
                "kdeconnect.contacts.response_vcards",
            ],
            Feature::Calendar => &["geekbridge.calendar.sync"],
            Feature::Sms => &["kdeconnect.sms.messages", "geekbridge.sms.status"],
        }
    }

    fn of_incoming(kind: &str) -> Option<Feature> {
        Self::ALL.into_iter().find(|f| f.incoming().contains(&kind))
    }

    fn of_outgoing(kind: &str) -> Option<Feature> {
        Self::ALL.into_iter().find(|f| f.outgoing().contains(&kind))
    }
}

/// Which features the app enabled, as parsed from the `data` start argument / config request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::transport) struct Flags {
    pub contacts: bool,
    pub calendar: bool,
    pub sms: bool,
}

impl Flags {
    fn get(self, f: Feature) -> bool {
        match f {
            Feature::Contacts => self.contacts,
            Feature::Calendar => self.calendar,
            Feature::Sms => self.sms,
        }
    }
}

pub(in crate::transport) fn incoming_types(flags: Flags) -> Vec<String> {
    types(flags, Feature::incoming)
}
pub(in crate::transport) fn outgoing_types(flags: Flags) -> Vec<String> {
    types(flags, Feature::outgoing)
}
fn types(flags: Flags, of: fn(Feature) -> &'static [&'static str]) -> Vec<String> {
    Feature::ALL
        .into_iter()
        .filter(|f| flags.get(*f))
        .flat_map(|f| of(f).iter().map(|s| (*s).to_owned()))
        .collect()
}

pub(in crate::transport) fn is_data_kind(kind: &str) -> bool {
    Feature::of_incoming(kind).is_some() || Feature::of_outgoing(kind).is_some()
}

struct Event {
    seq: u64,
    body: Value,
    at: Instant,
}

struct State {
    flags: Flags,
    seq: u64,
    events: VecDeque<Event>,
    delivered: u64,
    stopped: bool,
    tokens: f64,
    refilled: Instant,
}

pub(in crate::transport) struct Hub {
    st: Mutex<State>,
    cv: Condvar,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            st: Mutex::new(State {
                flags: Flags::default(),
                seq: 0,
                events: VecDeque::new(),
                delivered: 0,
                stopped: false,
                tokens: RATE_BURST,
                refilled: Instant::now(),
            }),
            cv: Condvar::new(),
        }
    }
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::transport) fn set_flags(&self, flags: Flags) {
        let mut g = self.lock();
        g.flags = flags;
        // Requests of a feature that was switched off must not reach Kotlin afterwards.
        let keep: VecDeque<Event> = std::mem::take(&mut g.events)
            .into_iter()
            .filter(|e| {
                e.body
                    .get("kind")
                    .and_then(Value::as_str)
                    .and_then(Feature::of_incoming)
                    .is_some_and(|f| flags.get(f))
            })
            .collect();
        g.events = keep;
        drop(g);
        self.cv.notify_all();
    }

    pub(in crate::transport) fn flags(&self) -> Flags {
        self.lock().flags
    }

    pub(in crate::transport) fn shutdown(&self) {
        let mut g = self.lock();
        g.stopped = true;
        g.events.clear();
        drop(g);
        self.cv.notify_all();
    }

    fn push(&self, feature: Feature, connection_id: u64, kind: &str, body: Map<String, Value>) {
        let mut g = self.lock();
        if g.stopped || !g.flags.get(feature) {
            return;
        }
        g.seq += 1;
        let seq = g.seq;
        if g.events.len() == MAX_EVENTS {
            g.events.pop_front();
        }
        g.events.push_back(Event {
            seq,
            body: json!({
                "seq": seq,
                "connection_id": connection_id,
                "kind": kind,
                "body": Value::Object(body),
            }),
            at: Instant::now(),
        });
        drop(g);
        self.cv.notify_all();
    }

    fn prune(g: &mut State, since: u64) {
        let floor = since.max(g.delivered);
        let now = Instant::now();
        g.events
            .retain(|e| e.seq > floor && now.saturating_duration_since(e.at) <= EVENT_TTL);
    }

    /// Block until an event newer than `since` exists, the transport stops, or `timeout` passes.
    pub(in crate::transport) fn wait(&self, since: u64, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout.min(MAX_WAIT);
        let mut g = self.lock();
        Self::prune(&mut g, since);
        while g.events.is_empty() && !g.stopped {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        Self::prune(&mut g, since);
        if let Some(last) = g.events.back() {
            g.delivered = g.delivered.max(last.seq);
        }
        json!({
            "events": g.events.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            "seq": g.seq,
            "stopped": g.stopped,
            "contacts": g.flags.contacts,
            "calendar": g.flags.calendar,
            "sms": g.flags.sms,
        })
    }

    /// Token bucket: 20 burst, 2 per second sustained.
    fn take_token(&self) -> bool {
        let mut g = self.lock();
        let now = Instant::now();
        let gained = now.saturating_duration_since(g.refilled).as_secs_f64() * RATE_PER_SEC;
        g.refilled = now;
        g.tokens = (g.tokens + gained).min(RATE_BURST);
        if g.tokens >= 1.0 {
            g.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub(in crate::transport) struct Out {
    kind: String,
    body: Map<String, Value>,
}

/// Write one queued packet.
pub(in crate::transport) async fn send(link: &mut bridge_kde::TrustedLink, out: Out) -> bool {
    link.send(&Packet::new(&out.kind, out.body)).await.is_ok()
}

impl Core {
    pub(in crate::transport) fn data(&self) -> &Hub {
        &self.data
    }

    /// Relay one packet from Kotlin to the trusted computer(s), or just the named connection.
    pub(in crate::transport) fn data_send(
        &self,
        kind: &str,
        body: Map<String, Value>,
        only: Option<u64>,
    ) -> Result<Value, ApiError> {
        let feature = Feature::of_outgoing(kind)
            .ok_or_else(|| err("kind_not_allowed", "this packet type may not be sent"))?;
        if !self.data.flags().get(feature) {
            return Err(err("data_disabled", "this feature is off in the app"));
        }
        let size = serde_json::to_vec(&body).map_or(usize::MAX, |v| v.len());
        if size > MAX_SEND_BODY_BYTES {
            return Err(err("too_large", "the packet body is over the size limit"));
        }
        let ctls: Vec<_> = {
            let st = self.lock();
            st.conns
                .values()
                .filter(|c| c.phase == Phase::Trusted && only.is_none_or(|id| id == c.id))
                .map(|c| c.ctl.clone())
                .collect()
        };
        if ctls.is_empty() {
            return Err(err("not_connected", "no trusted computer is connected"));
        }
        if !self.data.take_token() {
            return Err(err("rate_limited", "too many packets; try again shortly"));
        }
        let mut queued = 0;
        for ctl in ctls {
            let out = Out {
                kind: kind.to_owned(),
                body: body.clone(),
            };
            if ctl.try_send(Ctl::Data(out)).is_ok() {
                queued += 1;
            }
        }
        if queued == 0 {
            return Err(err("busy", "the connection is busy"));
        }
        Ok(json!({ "queued": queued }))
    }

    /// A data packet from a trusted peer. True if it was handled (acted on or deliberately dropped).
    pub(in crate::transport) fn incoming_data(&self, connection_id: u64, p: &Packet) -> bool {
        if p.payload_size.is_some() || p.payload_transfer_info.is_some() {
            return false;
        }
        // Only desktop -> phone request kinds are accepted; a response kind from the peer is noise.
        let Some(feature) = Feature::of_incoming(&p.kind) else {
            return false;
        };
        self.data
            .push(feature, connection_id, &p.kind, p.body.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Flags {
        Flags {
            contacts: true,
            calendar: true,
            sms: true,
        }
    }

    #[test]
    fn capabilities_follow_the_flags() {
        assert!(incoming_types(Flags::default()).is_empty());
        let only_cal = Flags {
            calendar: true,
            ..Flags::default()
        };
        assert_eq!(
            incoming_types(only_cal),
            vec!["geekbridge.calendar.request"]
        );
        assert_eq!(outgoing_types(only_cal), vec!["geekbridge.calendar.sync"]);
        assert_eq!(incoming_types(on()).len(), 6);
        assert_eq!(outgoing_types(on()).len(), 5);
    }

    #[test]
    fn disabled_features_queue_nothing_and_events_are_bounded_and_not_replayed() {
        let hub = Hub::default();
        hub.push(Feature::Sms, 1, "kdeconnect.sms.request", Map::new());
        assert!(
            hub.wait(0, Duration::ZERO)["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        hub.set_flags(on());
        for _ in 0..(MAX_EVENTS + 5) {
            hub.push(Feature::Sms, 1, "kdeconnect.sms.request", Map::new());
        }
        let r = hub.wait(0, Duration::ZERO);
        assert_eq!(r["events"].as_array().unwrap().len(), MAX_EVENTS);
        assert!(
            hub.wait(0, Duration::ZERO)["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn switching_a_feature_off_discards_its_queued_requests() {
        let hub = Hub::default();
        hub.set_flags(on());
        hub.push(Feature::Sms, 1, "kdeconnect.sms.request", Map::new());
        hub.push(
            Feature::Calendar,
            1,
            "geekbridge.calendar.request",
            Map::new(),
        );
        hub.set_flags(Flags {
            calendar: true,
            ..Flags::default()
        });
        let r = hub.wait(0, Duration::ZERO);
        let ev = r["events"].as_array().unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["kind"], "geekbridge.calendar.request");
    }

    #[test]
    fn rate_limit_allows_a_burst_of_twenty() {
        let hub = Hub::default();
        let granted = (0..40).filter(|_| hub.take_token()).count();
        assert!((20..=21).contains(&granted), "{granted}");
    }
}
