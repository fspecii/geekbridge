// SPDX-License-Identifier: GPL-3.0-or-later
//! Call control on the phone transport (`geekbridge.calls.*`, see `bridge_core::calls_link`).
//!
//! Rust owns the wire; Kotlin owns Android's telephony. Nothing here touches a call:
//!
//! * **Phone -> computer**: Kotlin reports the coarse call state with `transport_calls_report`
//!   (only after the user opted in). The latest report is kept and sent to every trusted
//!   connection, and again to a connection that appears later.
//! * **Computer -> phone**: a `geekbridge.calls.request` becomes an *event* that Kotlin collects with
//!   a blocking `transport_calls_wait`. Kotlin decides whether it may act (permission, ringing call),
//!   does it through `TelecomManager`, and answers with `transport_calls_result`.
//!
//! Requests received while the feature is off are answered with a failure at once and never reach
//! Kotlin. Events are bounded; nothing is logged; no number or name is kept after the next report.
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bridge_core::calls_link::{
    self as wire, LinkCallReport, LinkFailure, PhoneState, REQUEST, RESULT, STATE,
};
use bridge_kde::Packet;
use serde_json::{Map, Value, json};

use super::{Core, Ctl, Phase};
use crate::dispatch::{ApiError, err};

const MAX_EVENTS: usize = 8;
const MAX_WAIT: Duration = Duration::from_secs(55);
/// A computer's request is meant to act now; one nobody collected within this long is dropped, so a
/// waiter that restarts later cannot answer a different, newer call with it.
const REQUEST_TTL: Duration = Duration::from_secs(20);

pub(in crate::transport) fn incoming_types() -> Vec<String> {
    vec![REQUEST.to_owned()]
}
pub(in crate::transport) fn outgoing_types() -> Vec<String> {
    vec![STATE.to_owned(), RESULT.to_owned()]
}

struct Event {
    seq: u64,
    body: Value,
    at: std::time::Instant,
}

struct State {
    enabled: bool,
    seq: u64,
    events: VecDeque<Event>,
    /// Highest sequence already handed to the (single) waiter; never handed out twice.
    delivered: u64,
    stopped: bool,
    last: Option<Map<String, Value>>,
}

pub(in crate::transport) struct Hub {
    st: Mutex<State>,
    cv: Condvar,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            st: Mutex::new(State {
                enabled: false,
                seq: 0,
                events: VecDeque::new(),
                delivered: 0,
                stopped: false,
                last: None,
            }),
            cv: Condvar::new(),
        }
    }
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(in crate::transport) fn configure(&self, enabled: bool) {
        let mut g = self.lock();
        g.enabled = enabled;
        if !enabled {
            g.events.clear();
            g.last = None;
        }
        drop(g);
        // A parked waiter re-reads `enabled` and ends instead of lingering for its full timeout.
        self.cv.notify_all();
    }

    pub(in crate::transport) fn enabled(&self) -> bool {
        self.lock().enabled
    }

    pub(in crate::transport) fn shutdown(&self) {
        let mut g = self.lock();
        g.stopped = true;
        g.events.clear();
        g.last = None;
        drop(g);
        self.cv.notify_all();
    }

    /// The report to greet a new connection with.
    pub(in crate::transport) fn current(&self) -> Option<Map<String, Value>> {
        let g = self.lock();
        if g.enabled { g.last.clone() } else { None }
    }

    fn push(&self, mut body: Value) {
        let mut g = self.lock();
        if !g.enabled || g.stopped {
            return;
        }
        g.seq += 1;
        let seq = g.seq;
        if let Some(map) = body.as_object_mut() {
            map.insert("seq".to_owned(), json!(seq));
        }
        if g.events.len() == MAX_EVENTS {
            g.events.pop_front();
        }
        g.events.push_back(Event {
            seq,
            body,
            at: std::time::Instant::now(),
        });
        drop(g);
        self.cv.notify_all();
    }

    /// Keep only events newer than `since`, not yet delivered, and not expired.
    fn prune(g: &mut State, since: u64) {
        let floor = since.max(g.delivered);
        let now = std::time::Instant::now();
        g.events
            .retain(|e| e.seq > floor && now.saturating_duration_since(e.at) <= REQUEST_TTL);
    }

    #[cfg(test)]
    fn age_events(&self, by: Duration) {
        for e in self.lock().events.iter_mut() {
            e.at = e.at.checked_sub(by).unwrap();
        }
    }

    /// Block until an event newer than `since` exists, the transport stops, or `timeout` passes.
    pub(in crate::transport) fn wait(&self, since: u64, timeout: Duration) -> Value {
        let deadline = std::time::Instant::now() + timeout.min(MAX_WAIT);
        let mut g = self.lock();
        Self::prune(&mut g, since);
        while g.events.is_empty() && !g.stopped {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
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
            "last_seq": g.seq,
            "stopped": g.stopped,
            "enabled": g.enabled,
        })
    }
}

pub(in crate::transport) enum Out {
    State(Map<String, Value>),
    Result(Map<String, Value>),
}

/// Write one queued call packet.
pub(in crate::transport) async fn send(link: &mut bridge_kde::TrustedLink, out: Out) -> bool {
    let packet = match out {
        Out::State(body) => Packet::new(STATE, body),
        Out::Result(body) => Packet::new(RESULT, body),
    };
    link.send(&packet).await.is_ok()
}

/// What Kotlin reports about the phone's call state.
pub(in crate::transport) struct ReportArgs {
    pub state: String,
    pub number: Option<String>,
    pub name: Option<String>,
    pub can_answer: bool,
    pub can_end: bool,
    pub can_dial: bool,
}

impl Core {
    pub(in crate::transport) fn calls(&self) -> &Hub {
        &self.calls
    }

    fn trusted_ctls(&self, only: Option<u64>) -> Vec<tokio::sync::mpsc::Sender<Ctl>> {
        let st = self.lock();
        st.conns
            .values()
            .filter(|c| c.phase == Phase::Trusted && only.is_none_or(|id| id == c.id))
            .map(|c| c.ctl.clone())
            .collect()
    }

    /// Validate a state report from Kotlin, keep it, and send it to every trusted computer.
    pub(in crate::transport) fn calls_report(&self, a: ReportArgs) -> Result<Value, ApiError> {
        if !self.calls.enabled() {
            return Err(err("calls_disabled", "call control is off in the app"));
        }
        let state = PhoneState::parse(&a.state)
            .ok_or_else(|| err("invalid_field", "a field has the wrong type or value"))?;
        let mut body = Map::new();
        body.insert("state".into(), json!(state.as_str()));
        if let Some(n) = a.number {
            body.insert("number".into(), json!(n));
        }
        if let Some(n) = a.name {
            body.insert("name".into(), json!(n));
        }
        body.insert("canAnswer".into(), json!(a.can_answer));
        body.insert("canEnd".into(), json!(a.can_end));
        body.insert("canDial".into(), json!(a.can_dial));
        // The same parser the computer uses, so what leaves is what it accepts (an idle report
        // drops number and name).
        let report = LinkCallReport::parse(&body)
            .map_err(|_| err("invalid_field", "a field has the wrong type or value"))?;
        let body = report.body();
        self.calls.lock().last = Some(body.clone());
        let mut queued = 0;
        for ctl in self.permitted_ctls("calls", None) {
            if ctl.try_send(Ctl::Calls(Out::State(body.clone()))).is_ok() {
                queued += 1;
            }
        }
        Ok(json!({ "queued": queued }))
    }

    /// Tell the computer that asked how its request went.
    pub(in crate::transport) fn calls_result(
        &self,
        connection_id: u64,
        request_id: &str,
        failure: Option<&str>,
    ) -> Result<Value, ApiError> {
        let outcome = match failure {
            None => Ok(()),
            Some(f) => Err(LinkFailure::parse(f)
                .ok_or_else(|| err("invalid_field", "a field has the wrong type or value"))?),
        };
        if !wire::valid_request_id(request_id) {
            return Err(err("invalid_field", "a field has the wrong type or value"));
        }
        let ctls = self.trusted_ctls(Some(connection_id));
        let ctl = ctls
            .first()
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        ctl.try_send(Ctl::Calls(Out::Result(wire::result_body(
            request_id, outcome,
        ))))
        .map_err(|_| err("busy", "the connection is busy"))?;
        Ok(json!({ "queued": true }))
    }

    /// A `geekbridge.calls.*` packet from a trusted peer. True if it was one of ours.
    pub(in crate::transport) fn incoming_calls(&self, connection_id: u64, p: &Packet) -> bool {
        if p.kind != REQUEST {
            return false;
        }
        if p.payload_size.is_some() || p.payload_transfer_info.is_some() {
            return false;
        }
        let Ok((request_id, action)) = wire::parse_request(&p.body) else {
            return true;
        };
        if self.calls.enabled() && self.permitted("calls", connection_id) {
            let mut event = json!({
                "kind": "request",
                "connection_id": connection_id,
                "request_id": request_id,
                "action": action.as_str(),
            });
            if action == wire::LinkAction::Dial {
                // parse_request already proved the number is present and plain digits.
                if let (Ok(n), Some(o)) = (wire::parse_dial_number(&p.body), event.as_object_mut())
                {
                    o.insert("number".into(), json!(n.expose()));
                }
            }
            self.calls.push(event);
        } else {
            // Off in the app, or not allowed for this computer: refuse at once; Kotlin never hears about it.
            for ctl in self.trusted_ctls(Some(connection_id)) {
                let _ = ctl.try_send(Ctl::Calls(Out::Result(wire::result_body(
                    &request_id,
                    Err(LinkFailure::PermissionMissing),
                ))));
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_hub_keeps_nothing_and_waits_return_promptly() {
        let hub = Hub::default();
        hub.push(json!({"kind": "request"}));
        let r = hub.wait(0, Duration::from_millis(5));
        assert_eq!(r["events"].as_array().unwrap().len(), 0);
        assert_eq!(r["enabled"], false);
        assert!(hub.current().is_none());
    }

    #[test]
    fn events_are_bounded_numbered_and_released_by_since() {
        let hub = Hub::default();
        hub.configure(true);
        for i in 0..(MAX_EVENTS + 3) {
            hub.push(json!({"kind": "request", "n": i}));
        }
        let r = hub.wait(0, Duration::from_millis(5));
        let events = r["events"].as_array().unwrap();
        assert_eq!(events.len(), MAX_EVENTS);
        let last = r["last_seq"].as_u64().unwrap();
        assert_eq!(events.last().unwrap()["seq"].as_u64().unwrap(), last);
        let r = hub.wait(last, Duration::from_millis(5));
        assert!(r["events"].as_array().unwrap().is_empty());
        hub.configure(false);
        assert!(
            hub.wait(0, Duration::ZERO)["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn delivered_requests_are_not_replayed_to_a_restarted_waiter() {
        // The Kotlin waiter restarts with `since = 0` when it dies; a request it already acted on
        // must not come back, or a stale "answer" would pick up the next, unrelated call.
        let hub = Hub::default();
        hub.configure(true);
        hub.push(json!({"kind": "request", "action": "answer"}));
        let first = hub.wait(0, Duration::from_millis(5));
        assert_eq!(first["events"].as_array().unwrap().len(), 1);
        let again = hub.wait(0, Duration::from_millis(5));
        assert!(again["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn requests_expire_if_nobody_collects_them() {
        let hub = Hub::default();
        hub.configure(true);
        hub.push(json!({"kind": "request", "action": "answer"}));
        hub.age_events(REQUEST_TTL + Duration::from_secs(1));
        let r = hub.wait(0, Duration::from_millis(5));
        assert!(r["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn shutdown_wakes_a_waiter() {
        let hub = std::sync::Arc::new(Hub::default());
        hub.configure(true);
        let h = hub.clone();
        let t = std::thread::spawn(move || h.wait(0, Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(30));
        hub.shutdown();
        assert_eq!(t.join().unwrap()["stopped"], true);
    }
}
