// SPDX-License-Identifier: GPL-3.0-or-later
//! The calls that ended since the service started, for the Calls window's Recents list and the
//! missed-call notification. Memory only: nothing is written to disk and the list is gone with the
//! process. It is built from what the merged call view shows, so a call seen by Bluetooth and by
//! the phone link is one entry.
//!
//! A call whose source went away (link dropped, phone out of range) is forgotten rather than
//! recorded, because nobody knows how it ended; a flap must not turn into a "missed call".
use bridge_core::{
    calls::{CallDirection, CallPhase, Sensitive},
    calls_view::{CallsView, RecentCall, RecentKind},
};
use std::{
    collections::{BTreeSet, VecDeque},
    time::Instant,
};

/// How many finished calls are kept.
pub const KEEP: usize = 30;

struct Open {
    /// `source|call` of every part that has seen this call.
    parts: BTreeSet<String>,
    phone: String,
    number: Option<Sensitive>,
    name: Option<Sensitive>,
    direction: CallDirection,
    answered_at: Option<Instant>,
    declined: bool,
}

struct Done {
    call: RecentCall,
    at: Instant,
}

#[derive(Default)]
pub struct History {
    open: Vec<Open>,
    done: VecDeque<Done>,
}

fn parts_of(r: &bridge_core::calls_view::CallRow) -> Vec<String> {
    if r.parts.is_empty() {
        vec![r.key.clone()]
    } else {
        r.parts.clone()
    }
}

impl History {
    /// Fold the current view in. `live_sources` are the keys of the sources that exist now. Returns
    /// the calls that ended unanswered since the last call, oldest first (the missed ones).
    pub fn observe(
        &mut self,
        view: &CallsView,
        live_sources: &BTreeSet<String>,
        now: Instant,
    ) -> Vec<RecentCall> {
        let mut seen = vec![false; self.open.len()];
        for r in &view.rows {
            let parts = parts_of(r);
            let at = self
                .open
                .iter()
                .position(|o| parts.iter().any(|p| o.parts.contains(p)));
            let i = at.unwrap_or_else(|| {
                self.open.push(Open {
                    parts: BTreeSet::new(),
                    phone: r.phone.clone(),
                    number: None,
                    name: None,
                    direction: CallDirection::Unknown,
                    answered_at: None,
                    declined: false,
                });
                seen.push(true);
                self.open.len() - 1
            });
            seen[i] = true;
            let o = &mut self.open[i];
            o.parts.extend(parts);
            if r.number.is_some() {
                o.number.clone_from(&r.number);
            }
            if r.name.is_some() {
                o.name.clone_from(&r.name);
            }
            let dir = match r.phase {
                CallPhase::Incoming | CallPhase::Waiting => CallDirection::Incoming,
                CallPhase::Dialing | CallPhase::Alerting => CallDirection::Outgoing,
                CallPhase::Active | CallPhase::Held => r.direction,
            };
            if o.direction == CallDirection::Unknown {
                o.direction = dir;
            }
            if !r.ringing() && !matches!(r.phase, CallPhase::Dialing | CallPhase::Alerting) {
                o.answered_at.get_or_insert(now);
            }
        }
        let mut missed = Vec::new();
        let mut keep = Vec::new();
        for (o, seen) in std::mem::take(&mut self.open).into_iter().zip(seen) {
            if seen {
                keep.push(o);
                continue;
            }
            let source_alive = o.parts.iter().all(|p| {
                let source = p.split_once('|').map_or(p.as_str(), |(s, _)| s);
                live_sources.contains(source)
            });
            if !source_alive {
                continue;
            }
            if let Some(call) = Self::finish(&o, now) {
                if call.kind == RecentKind::Missed {
                    missed.push(call.clone());
                }
                if self.done.len() >= KEEP {
                    self.done.pop_front();
                }
                self.done.push_back(Done { call, at: now });
            }
        }
        self.open = keep;
        missed
    }

    fn finish(o: &Open, now: Instant) -> Option<RecentCall> {
        let talk = o
            .answered_at
            .map(|t| now.saturating_duration_since(t).as_secs());
        let kind = match (o.direction, o.answered_at.is_some()) {
            (CallDirection::Incoming, true) => RecentKind::Incoming,
            (CallDirection::Incoming, false) if o.declined => RecentKind::Declined,
            (CallDirection::Incoming, false) => RecentKind::Missed,
            (CallDirection::Outgoing, true) => RecentKind::Outgoing,
            (CallDirection::Outgoing, false) => RecentKind::OutgoingUnanswered,
            // Seen only after it was already live (the service started mid-call, or the phone
            // link reports no direction): it happened, but who called whom is unknown.
            (CallDirection::Unknown, true) => RecentKind::Connected,
            (CallDirection::Unknown, false) => return None,
        };
        Some(RecentCall {
            phone: o.phone.clone(),
            kind,
            number: o.number.clone(),
            name: o.name.clone(),
            ended_secs_ago: 0,
            talk_secs: talk,
        })
    }

    /// The user declined this ringing call from here.
    pub fn declined(&mut self, parts: &[String]) {
        for o in &mut self.open {
            if parts.iter().any(|p| o.parts.contains(p)) {
                o.declined = true;
            }
        }
    }

    /// Newest first, with the time since each ended.
    pub fn recent(&self, now: Instant) -> Vec<RecentCall> {
        self.done
            .iter()
            .rev()
            .map(|d| RecentCall {
                ended_secs_ago: now.saturating_duration_since(d.at).as_secs(),
                ..d.call.clone()
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty() && self.done.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::calls::{CallState, Sensitive};
    use bridge_core::calls_link::{LinkCallReport, PhoneState};
    use bridge_core::calls_view::{SourceSnapshot, build};
    use std::time::Duration;

    fn view(state: PhoneState, number: Option<&str>) -> CallsView {
        let report = LinkCallReport {
            state,
            number: number.map(Sensitive::new),
            name: None,
            can_answer: true,
            can_end: true,
            can_dial: false,
        };
        let mut s = CallState::default();
        report.apply(&mut s);
        build(&[SourceSnapshot {
            key: "link:p".into(),
            label: "Pixel".into(),
            caps: report.capabilities(),
            state: s,
        }])
    }
    fn alive() -> BTreeSet<String> {
        ["link:p".to_owned()].into()
    }

    #[test]
    fn an_unanswered_ring_is_missed_once_and_listed() {
        let mut h = History::default();
        let t = Instant::now();
        assert!(
            h.observe(&view(PhoneState::Ringing, Some("+1555")), &alive(), t)
                .is_empty()
        );
        // The same ring seen again changes nothing.
        assert!(
            h.observe(&view(PhoneState::Ringing, Some("+1555")), &alive(), t)
                .is_empty()
        );
        let missed = h.observe(&view(PhoneState::Idle, None), &alive(), t);
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].kind, RecentKind::Missed);
        assert_eq!(missed[0].callback_number().as_deref(), Some("+1555"));
        assert!(
            h.observe(&view(PhoneState::Idle, None), &alive(), t)
                .is_empty()
        );
        assert_eq!(h.recent(t).len(), 1);
    }

    #[test]
    fn an_answered_call_keeps_its_number_and_talk_time_and_is_not_missed() {
        let mut h = History::default();
        let t = Instant::now();
        h.observe(&view(PhoneState::Ringing, Some("+1555")), &alive(), t);
        let t2 = t + Duration::from_secs(2);
        h.observe(&view(PhoneState::OffHook, None), &alive(), t2);
        let t3 = t2 + Duration::from_secs(65);
        let missed = h.observe(&view(PhoneState::Idle, None), &alive(), t3);
        assert!(missed.is_empty());
        let r = h.recent(t3 + Duration::from_secs(120));
        assert_eq!(r[0].kind, RecentKind::Incoming);
        assert_eq!(r[0].talk_secs, Some(65));
        assert_eq!(r[0].ended_secs_ago, 120);
        assert_eq!(r[0].callback_number().as_deref(), Some("+1555"));
    }

    #[test]
    fn a_ring_declined_here_is_declined_not_missed() {
        let mut h = History::default();
        let t = Instant::now();
        let v = view(PhoneState::Ringing, Some("+1555"));
        h.observe(&v, &alive(), t);
        h.declined(&v.rows[0].parts);
        assert!(
            h.observe(&view(PhoneState::Idle, None), &alive(), t)
                .is_empty()
        );
        assert_eq!(h.recent(t)[0].kind, RecentKind::Declined);
    }

    #[test]
    fn a_vanished_source_records_nothing() {
        let mut h = History::default();
        let t = Instant::now();
        h.observe(&view(PhoneState::Ringing, Some("+1555")), &alive(), t);
        // Phone link dropped: no row, no source. Not a missed call.
        let v = CallsView::empty();
        assert!(h.observe(&v, &BTreeSet::new(), t).is_empty());
        assert!(h.recent(t).is_empty());
        assert!(h.is_empty());
    }

    #[test]
    fn a_call_first_seen_live_is_recorded_without_a_direction_and_the_list_is_bounded() {
        let mut h = History::default();
        let t = Instant::now();
        for _ in 0..(KEEP + 5) {
            h.observe(&view(PhoneState::OffHook, None), &alive(), t);
            h.observe(&view(PhoneState::Idle, None), &alive(), t);
        }
        let r = h.recent(t);
        assert_eq!(r.len(), KEEP);
        assert_eq!(r[0].kind, RecentKind::Connected);
    }

    #[test]
    fn callback_numbers_are_plain_or_absent() {
        let mk = |n: &str| RecentCall {
            phone: String::new(),
            kind: RecentKind::Missed,
            number: Some(Sensitive::new(n)),
            name: None,
            ended_secs_ago: 0,
            talk_secs: None,
        };
        assert_eq!(
            mk("+44 (20) 7946-0001").callback_number().as_deref(),
            Some("+442079460001")
        );
        assert_eq!(mk("0151 234").callback_number().as_deref(), Some("0151234"));
        assert_eq!(mk("*123#").callback_number(), None);
        assert_eq!(mk("1+2").callback_number(), None);
        assert_eq!(mk("+".repeat(2).as_str()).callback_number(), None);
        assert_eq!(mk("Unknown").callback_number(), None);
        assert_eq!(mk(&"1".repeat(21)).callback_number(), None);
    }
}
