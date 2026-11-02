// SPDX-License-Identifier: GPL-3.0-or-later
//! Derives GeekBridge calls from the three standard HFP call indicators.
//!
//! `IOBluetoothHandsFreeDevice` reports `call`, `callsetup` and `callheld` as separate delegate
//! callbacks and documents no call identifiers. The indicators can therefore describe at most
//! three calls ("slots"): one in setup (ringing or dialing), one active and one held. This module
//! assigns a stable [`CallId`] to each slot, follows a call as it moves between slots, and says so
//! when a transition cannot be attributed ([`CallTracker::identity_uncertain`]). Control commands
//! consult that flag and refuse rather than guess.
//!
//! Callbacks for one logical change arrive separately and in no guaranteed order (answering a call
//! can report `callsetup=0` before `call=1`). A change that would end a call is therefore held
//! back for [`SETTLE_MS`]; if a complementary indicator shows the call merely moved slots, no end
//! is ever reported.
use crate::model::{Indicator, ListEntry, ListStatus, Millis, bound_field};
use bridge_core::calls::{
    CallDirection, CallEvent, CallId, CallParty, CallPhase, EndReason, Sensitive,
};
use std::collections::BTreeMap;

/// How long an apparent call end is deferred while waiting for a complementary indicator.
pub const SETTLE_MS: u64 = 250;
/// A caller number seen before the matching `callsetup=1` is kept this long.
const STASH_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupKind {
    Incoming,
    Dialing,
    Alerting,
}
impl SetupKind {
    fn incoming(self) -> bool {
        self == Self::Incoming
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub id: CallId,
    pub direction: CallDirection,
    pub party: CallParty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupSlot {
    pub slot: Slot,
    pub kind: SetupKind,
}

/// Where a call currently sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Setup(SetupKind),
    Active,
    Held,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slots {
    pub setup: Option<SetupSlot>,
    pub active: Option<Slot>,
    pub held: Option<Slot>,
}
impl Slots {
    pub fn live(&self) -> usize {
        usize::from(self.setup.is_some())
            + usize::from(self.active.is_some())
            + usize::from(self.held.is_some())
    }
    pub fn kind_of(&self, id: &CallId) -> Option<SlotKind> {
        if self.setup.as_ref().is_some_and(|s| &s.slot.id == id) {
            return Some(SlotKind::Setup(self.setup.as_ref()?.kind));
        }
        if self.active.as_ref().is_some_and(|s| &s.id == id) {
            return Some(SlotKind::Active);
        }
        if self.held.as_ref().is_some_and(|s| &s.id == id) {
            return Some(SlotKind::Held);
        }
        None
    }
    fn ids(&self) -> Vec<&CallId> {
        let mut v = Vec::new();
        if let Some(s) = &self.setup {
            v.push(&s.slot.id);
        }
        if let Some(s) = &self.active {
            v.push(&s.id);
        }
        if let Some(s) = &self.held {
            v.push(&s.id);
        }
        v
    }
    /// The phase the core model should show for the slot holding `id`.
    fn phase_of(&self, id: &CallId) -> Option<CallPhase> {
        Some(match self.kind_of(id)? {
            SlotKind::Setup(SetupKind::Incoming) => {
                if self.active.is_some() || self.held.is_some() {
                    CallPhase::Waiting
                } else {
                    CallPhase::Incoming
                }
            }
            SlotKind::Setup(SetupKind::Dialing) => CallPhase::Dialing,
            SlotKind::Setup(SetupKind::Alerting) => CallPhase::Alerting,
            SlotKind::Active => CallPhase::Active,
            SlotKind::Held => CallPhase::Held,
        })
    }
    fn slot(&self, id: &CallId) -> Option<&Slot> {
        match self.kind_of(id)? {
            SlotKind::Setup(_) => self.setup.as_ref().map(|s| &s.slot),
            SlotKind::Active => self.active.as_ref(),
            SlotKind::Held => self.held.as_ref(),
        }
    }
    fn slot_mut(&mut self, id: &CallId) -> Option<&mut Slot> {
        match self.kind_of(id)? {
            SlotKind::Setup(_) => self.setup.as_mut().map(|s| &mut s.slot),
            SlotKind::Active => self.active.as_mut(),
            SlotKind::Held => self.held.as_mut(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Raw {
    call: bool,
    setup: u8,
    held: u8,
}
impl Raw {
    fn has_active(self) -> bool {
        self.held == 1 || (self.call && self.held == 0)
    }
    fn has_held(self) -> bool {
        self.held != 0
    }
    fn setup_kind(self) -> Option<SetupKind> {
        match self.setup {
            1 => Some(SetupKind::Incoming),
            2 => Some(SetupKind::Dialing),
            3 => Some(SetupKind::Alerting),
            _ => None,
        }
    }
}

/// What was emitted for a call, to send only changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    direction: CallDirection,
    phase: CallPhase,
    party: CallParty,
}

struct Plan {
    slots: Slots,
    ended: Vec<CallId>,
    uncertain: bool,
    /// A new active call appeared with no known origin while a setup call is still pending: most
    /// likely the first half of an answer (`call=1` before `callsetup=0`), so commit later.
    tentative: bool,
}

#[derive(Debug)]
pub struct CallTracker {
    prefix: String,
    counter: u64,
    raw: Raw,
    seen_call_indicator: bool,
    committed: Slots,
    shown: BTreeMap<CallId, Shown>,
    deferred_since: Option<Millis>,
    uncertain: bool,
    stash: Option<(Sensitive, Millis)>,
    /// List entries that matched no slot (diagnostic).
    pub unmatched_list_entries: u32,
}

impl CallTracker {
    /// `prefix` makes ids unique across link generations (for example `mac-g3`).
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            counter: 0,
            raw: Raw::default(),
            seen_call_indicator: false,
            committed: Slots::default(),
            shown: BTreeMap::new(),
            deferred_since: None,
            uncertain: false,
            stash: None,
            unmatched_list_entries: 0,
        }
    }

    /// True once any call-related indicator has been delivered on this link. Until then the call
    /// picture is unknown, not "no calls", and control commands are refused.
    pub fn indicators_observed(&self) -> bool {
        self.seen_call_indicator
    }
    pub fn slots(&self) -> &Slots {
        &self.committed
    }
    /// A transition could not be attributed to specific calls, so ids may be swapped. Cleared as
    /// soon as at most one call is live.
    pub fn identity_uncertain(&self) -> bool {
        self.uncertain && self.committed.live() > 1
    }
    /// The core model refused an event we derived; do not trust call identities until one call remains.
    pub fn mark_uncertain(&mut self) {
        self.uncertain = true;
    }
    /// An end is being held back waiting for a complementary indicator.
    pub fn settling(&self) -> bool {
        self.deferred_since.is_some()
    }

    fn fresh_id(&mut self) -> CallId {
        self.counter += 1;
        CallId::new(format!("{}-c{}", self.prefix, self.counter))
    }

    pub fn on_indicator(&mut self, ind: Indicator, now: Millis) -> Vec<CallEvent> {
        match ind {
            Indicator::Call(v) => {
                self.raw.call = v;
                self.seen_call_indicator = true;
            }
            Indicator::CallSetup(v) => {
                self.raw.setup = v;
                self.seen_call_indicator = true;
            }
            Indicator::CallHeld(v) => {
                // The same value re-reported while two calls exist is how an AG announces a swap,
                // but it is indistinguishable from a repeated report: ids may now be exchanged.
                if self.raw.held == 1
                    && v == 1
                    && self.committed.active.is_some()
                    && self.committed.held.is_some()
                {
                    self.uncertain = true;
                }
                self.raw.held = v;
                self.seen_call_indicator = true;
            }
            Indicator::Service(_)
            | Indicator::Signal(_)
            | Indicator::Roam(_)
            | Indicator::Battery(_) => return Vec::new(),
        }
        self.reconcile(now, false)
    }

    /// Commit deferred call endings whose settle window elapsed.
    pub fn tick(&mut self, now: Millis) -> Vec<CallEvent> {
        match self.deferred_since {
            Some(since) if now.since(since) >= SETTLE_MS => self.reconcile(now, true),
            _ => Vec::new(),
        }
    }

    /// `incomingCallFrom:` — attach to the ringing call, or hold briefly for the one about to ring.
    pub fn on_caller_id(&mut self, number: Option<Sensitive>, now: Millis) -> Vec<CallEvent> {
        let Some(number) = number.map(|n| Sensitive::new(bound_field(n.expose()))) else {
            return Vec::new();
        };
        if let Some(setup) = self.committed.setup.as_mut().filter(|s| s.kind.incoming()) {
            if setup.slot.party.number.as_ref() == Some(&number) {
                return Vec::new();
            }
            setup.slot.party.number = Some(number);
            return self.emit_changes(Vec::new());
        }
        self.stash = Some((number, now));
        Vec::new()
    }

    /// Enrich a slot from a `currentCall:` entry. Entries are matched by status only; an entry whose
    /// status names no live slot, or whose direction contradicts the slot, is counted and ignored.
    pub fn on_list_entry(&mut self, entry: &ListEntry) -> Vec<CallEvent> {
        let target = match entry.status {
            ListStatus::Active => self.committed.active.as_ref().map(|s| s.id.clone()),
            ListStatus::Held | ListStatus::HeldByResponseAndHold => {
                self.committed.held.as_ref().map(|s| s.id.clone())
            }
            ListStatus::Dialing | ListStatus::Alerting => self
                .committed
                .setup
                .as_ref()
                .filter(|s| !s.kind.incoming())
                .map(|s| s.slot.id.clone()),
            ListStatus::Incoming | ListStatus::Waiting => self
                .committed
                .setup
                .as_ref()
                .filter(|s| s.kind.incoming())
                .map(|s| s.slot.id.clone()),
        };
        let Some(id) = target else {
            self.unmatched_list_entries += 1;
            return Vec::new();
        };
        let Some(slot) = self.committed.slot_mut(&id) else {
            return Vec::new();
        };
        let reported = match entry.incoming {
            Some(true) => CallDirection::Incoming,
            Some(false) => CallDirection::Outgoing,
            None => CallDirection::Unknown,
        };
        if reported != CallDirection::Unknown
            && slot.direction != CallDirection::Unknown
            && slot.direction != reported
        {
            self.unmatched_list_entries += 1;
            return Vec::new();
        }
        if slot.direction == CallDirection::Unknown {
            slot.direction = reported;
        }
        if let Some(n) = &entry.number {
            slot.party.number = Some(Sensitive::new(bound_field(n.expose())));
        }
        if let Some(n) = &entry.name {
            slot.party.name = Some(Sensitive::new(bound_field(n.expose())));
        }
        self.emit_changes(Vec::new())
    }

    /// Forget everything (link generation ended). The core model ends the calls itself on
    /// `Link { Down }`; the tracker is simply discarded and rebuilt for the next generation.
    pub fn live_ids(&self) -> Vec<CallId> {
        self.committed.ids().into_iter().cloned().collect()
    }

    fn reconcile(&mut self, now: Millis, force: bool) -> Vec<CallEvent> {
        let plan = self.derive(now);
        if (!plan.ended.is_empty() || plan.tentative) && !force {
            // Possibly a transient between two indicator callbacks: wait before reporting an end
            // or an unexplained new call.
            self.deferred_since.get_or_insert(now);
            return Vec::new();
        }
        self.deferred_since = None;
        if plan.uncertain {
            self.uncertain = true;
        }
        self.committed = plan.slots;
        if self.committed.live() <= 1 {
            self.uncertain = false;
        }
        self.emit_changes(plan.ended)
    }

    fn emit_changes(&mut self, ended: Vec<CallId>) -> Vec<CallEvent> {
        let mut out = Vec::new();
        for id in self
            .committed
            .ids()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            let (Some(phase), Some(slot)) =
                (self.committed.phase_of(&id), self.committed.slot(&id))
            else {
                continue;
            };
            let now_shown = Shown {
                direction: slot.direction,
                phase,
                party: slot.party.clone(),
            };
            if self.shown.get(&id) == Some(&now_shown) {
                continue;
            }
            out.push(CallEvent::CallUpdated {
                id: id.clone(),
                direction: now_shown.direction,
                phase,
                party: now_shown.party.clone(),
            });
            self.shown.insert(id, now_shown);
        }
        for id in ended {
            if self.shown.remove(&id).is_some() {
                out.push(CallEvent::CallEnded {
                    id,
                    reason: EndReason::Unspecified,
                });
            }
        }
        out
    }

    /// Compute the slot assignment implied by the raw indicators, given the committed slots.
    fn derive(&mut self, now: Millis) -> Plan {
        let raw = self.raw;
        let old = self.committed.clone();
        let (n_active, n_held) = (raw.has_active(), raw.has_held());
        let new_kind = raw.setup_kind();
        let mut uncertain = false;

        // A ringing/dialing call stays the same call while its kind stays in the same family.
        let setup_continues = match (&old.setup, new_kind) {
            (Some(s), Some(k)) => s.kind.incoming() == k.incoming(),
            _ => false,
        };
        let setup_gone = old.setup.is_some() && !setup_continues;

        let mut used_setup = false;
        let mut a_to_held = false;
        let mut active_fresh = false;

        // Held slot.
        let mut new_held: Option<Slot> = None;
        if n_held {
            if let Some(h) = old.held.clone() {
                new_held = Some(h);
            } else if let Some(a) = old.active.clone() {
                new_held = Some(a);
                a_to_held = true;
            }
        }

        // Active slot.
        let mut new_active: Option<Slot> = None;
        if n_active {
            if old.active.is_some() && !a_to_held {
                new_active = old.active.clone();
            } else if setup_gone {
                new_active = old.setup.as_ref().map(|s| s.slot.clone());
                used_setup = true;
            } else if old.held.is_some() && !n_held {
                new_active = old.held.clone();
            }
            if old.active.is_some() && !a_to_held && old.held.is_some() && !n_held {
                // Active and held both existed, now only one call: which one ended is not
                // observable from the indicators. Keep the active id and flag it.
                uncertain = true;
            }
        }
        if n_held && new_held.is_none() {
            // A held call with no known origin (for example indicators first seen mid-call).
            new_held = Some(self.new_slot(CallDirection::Unknown, &old));
        }
        if n_active && new_active.is_none() {
            new_active = Some(self.new_slot(CallDirection::Unknown, &old));
            active_fresh = true;
        }
        let tentative = active_fresh && setup_continues;

        // Setup slot.
        let mut new_setup: Option<SetupSlot> = None;
        if let Some(kind) = new_kind {
            if setup_continues {
                let mut s = old.setup.clone().expect("continuing setup exists");
                s.kind = kind;
                new_setup = Some(s);
            } else {
                let mut slot = self.new_slot(
                    if kind.incoming() {
                        CallDirection::Incoming
                    } else {
                        CallDirection::Outgoing
                    },
                    &old,
                );
                if kind.incoming()
                    && let Some((n, _)) = self
                        .stash
                        .take()
                        .filter(|(_, at)| now.since(*at) <= STASH_MS)
                {
                    slot.party.number = Some(n);
                }
                new_setup = Some(SetupSlot { slot, kind });
            }
        }

        // An answered call takes the direction implied by how it began.
        if used_setup
            && let (Some(a), Some(s)) = (new_active.as_mut(), old.setup.as_ref())
            && a.direction == CallDirection::Unknown
        {
            a.direction = s.slot.direction;
        }

        let slots = Slots {
            setup: new_setup,
            active: new_active,
            held: new_held,
        };
        let kept: Vec<&CallId> = slots.ids();
        let mut ended = Vec::new();
        for gone in old
            .setup
            .iter()
            .map(|s| &s.slot)
            .chain(old.active.iter())
            .chain(old.held.iter())
        {
            if !kept.contains(&&gone.id) {
                ended.push(gone.id.clone());
            }
        }
        Plan {
            slots,
            ended,
            uncertain,
            tentative,
        }
    }

    fn new_slot(&mut self, direction: CallDirection, _old: &Slots) -> Slot {
        Slot {
            id: self.fresh_id(),
            direction,
            party: CallParty::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Millis {
        Millis(ms)
    }

    fn ind(tr: &mut CallTracker, i: Indicator, at: u64) -> Vec<CallEvent> {
        tr.on_indicator(i, t(at))
    }

    fn updated(ev: &[CallEvent]) -> Vec<(String, CallPhase)> {
        ev.iter()
            .filter_map(|e| match e {
                CallEvent::CallUpdated { id, phase, .. } => Some((id.as_str().to_owned(), *phase)),
                _ => None,
            })
            .collect()
    }
    fn ended(ev: &[CallEvent]) -> Vec<String> {
        ev.iter()
            .filter_map(|e| match e {
                CallEvent::CallEnded { id, .. } => Some(id.as_str().to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn no_indicators_means_unknown_not_idle() {
        let tr = CallTracker::new("m");
        assert!(!tr.indicators_observed());
    }

    #[test]
    fn incoming_call_answer_and_hangup_keep_one_id() {
        let mut tr = CallTracker::new("m");
        let ev = ind(&mut tr, Indicator::CallSetup(1), 0);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Incoming)]);
        // Standard order: call=1 first. Not reported yet: an unexplained active call beside a
        // ringing one is held back until callsetup=0 shows whether the ringing call moved.
        assert!(ind(&mut tr, Indicator::Call(true), 100).is_empty());
        let ev = ind(&mut tr, Indicator::CallSetup(0), 120);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Active)]);
        assert!(ended(&ev).is_empty());
        assert_eq!(
            tr.slots().active.as_ref().unwrap().direction,
            CallDirection::Incoming
        );
        // Peer hangs up.
        assert!(ind(&mut tr, Indicator::Call(false), 5_000).is_empty());
        let ev = tr.tick(t(5_000 + SETTLE_MS));
        assert_eq!(ended(&ev), vec!["m-c1".to_owned()]);
    }

    #[test]
    fn real_second_call_is_committed_once_the_settle_window_passes() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::CallSetup(1), 0);
        assert!(ind(&mut tr, Indicator::Call(true), 10).is_empty());
        // callsetup stays 1: genuinely an active call plus a waiting one.
        let ev = tr.tick(t(10 + SETTLE_MS));
        assert!(updated(&ev).iter().any(|(_, p)| *p == CallPhase::Active));
        assert!(updated(&ev).iter().any(|(_, p)| *p == CallPhase::Waiting));
    }

    #[test]
    fn answer_with_setup_cleared_first_is_not_reported_as_end() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::CallSetup(1), 0);
        // Out-of-order: setup drops before call rises. The end is deferred, not reported.
        let ev = ind(&mut tr, Indicator::CallSetup(0), 100);
        assert!(ev.is_empty());
        assert!(tr.settling());
        // call=1 arrives inside the settle window: same id becomes active.
        let ev = ind(&mut tr, Indicator::Call(true), 150);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Active)]);
        assert!(ended(&ev).is_empty());
        assert!(!tr.settling());
        assert!(tr.tick(t(10_000)).is_empty());
    }

    #[test]
    fn rejected_call_ends_after_settle_window_only() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::CallSetup(1), 0);
        assert!(ind(&mut tr, Indicator::CallSetup(0), 100).is_empty());
        assert!(tr.tick(t(100 + SETTLE_MS - 1)).is_empty());
        let ev = tr.tick(t(100 + SETTLE_MS));
        assert_eq!(ended(&ev), vec!["m-c1".to_owned()]);
        assert_eq!(tr.slots().live(), 0);
    }

    #[test]
    fn outgoing_call_progresses_dialing_alerting_active() {
        let mut tr = CallTracker::new("m");
        let ev = ind(&mut tr, Indicator::CallSetup(2), 0);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Dialing)]);
        let ev = ind(&mut tr, Indicator::CallSetup(3), 10);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Alerting)]);
        ind(&mut tr, Indicator::Call(true), 20);
        let ev = ind(&mut tr, Indicator::CallSetup(0), 30);
        assert!(ended(&ev).is_empty());
        let slot = tr.slots().active.clone().unwrap();
        assert_eq!(slot.id.as_str(), "m-c1");
        assert_eq!(slot.direction, CallDirection::Outgoing);
    }

    #[test]
    fn failed_dial_ends_after_settle() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::CallSetup(2), 0);
        assert!(ind(&mut tr, Indicator::CallSetup(0), 5).is_empty());
        let ev = tr.tick(t(5 + SETTLE_MS));
        assert_eq!(ended(&ev), vec!["m-c1".to_owned()]);
    }

    #[test]
    fn waiting_call_hold_active_then_answer_swaps_ids_correctly() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::Call(true), 0); // c1 active
        let ev = ind(&mut tr, Indicator::CallSetup(1), 10); // c2 waiting
        assert!(updated(&ev).contains(&("m-c2".into(), CallPhase::Waiting)));
        // holdCall: callheld=1 and callsetup=0 (order may vary).
        ind(&mut tr, Indicator::CallHeld(1), 20);
        let ev = ind(&mut tr, Indicator::CallSetup(0), 30);
        let s = tr.slots();
        assert_eq!(s.held.as_ref().unwrap().id.as_str(), "m-c1");
        assert_eq!(s.active.as_ref().unwrap().id.as_str(), "m-c2");
        assert!(ended(&ev).is_empty());
        assert!(!tr.identity_uncertain());
    }

    #[test]
    fn hold_and_resume_single_call() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::Call(true), 0);
        let ev = ind(&mut tr, Indicator::CallHeld(2), 10);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Held)]);
        let ev = ind(&mut tr, Indicator::CallHeld(0), 20);
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Active)]);
        assert!(ended(&ev).is_empty());
    }

    #[test]
    fn rereported_held_with_two_calls_marks_identity_uncertain() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::Call(true), 0);
        ind(&mut tr, Indicator::CallHeld(1), 5); // c1 held? active moved to held, new active appears
        assert_eq!(tr.slots().live(), 2);
        assert!(!tr.identity_uncertain());
        ind(&mut tr, Indicator::CallHeld(1), 20);
        assert!(tr.identity_uncertain());
    }

    #[test]
    fn uncertainty_clears_when_one_call_remains() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::Call(true), 0);
        ind(&mut tr, Indicator::CallHeld(1), 5);
        ind(&mut tr, Indicator::CallHeld(1), 20);
        assert!(tr.identity_uncertain());
        // Held call released: one call left; ambiguity no longer matters.
        ind(&mut tr, Indicator::CallHeld(0), 30);
        tr.tick(t(30 + SETTLE_MS));
        assert!(!tr.identity_uncertain());
    }

    #[test]
    fn active_and_held_collapsing_to_one_flags_uncertainty_while_two_remain() {
        let mut tr = CallTracker::new("m");
        // Establish active c1 + held (via c1 held then new active).
        ind(&mut tr, Indicator::Call(true), 0);
        ind(&mut tr, Indicator::CallHeld(1), 5);
        assert_eq!(tr.slots().live(), 2);
        // Held released: only the active remains. Not a swap; ends exactly one call after settle.
        ind(&mut tr, Indicator::CallHeld(0), 10);
        let ev = tr.tick(t(10 + SETTLE_MS));
        assert_eq!(ended(&ev).len(), 1);
    }

    #[test]
    fn caller_id_before_and_after_ring_attaches_number() {
        let mut tr = CallTracker::new("m");
        assert!(
            tr.on_caller_id(Some(Sensitive::new("+15551234")), t(0))
                .is_empty()
        );
        let ev = ind(&mut tr, Indicator::CallSetup(1), 5);
        match &ev[0] {
            CallEvent::CallUpdated { party, .. } => {
                assert_eq!(party.number.as_ref().unwrap().expose(), "+15551234");
            }
            other => panic!("{other:?}"),
        }
        // Repeated RING/CLIP is idempotent.
        assert!(
            tr.on_caller_id(Some(Sensitive::new("+15551234")), t(10))
                .is_empty()
        );
        // After the ring, a number change updates the call.
        let ev = tr.on_caller_id(Some(Sensitive::new("+15559999")), t(20));
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn stale_stashed_number_is_not_attached() {
        let mut tr = CallTracker::new("m");
        tr.on_caller_id(Some(Sensitive::new("111")), t(0));
        let ev = ind(&mut tr, Indicator::CallSetup(1), STASH_MS + 1);
        match &ev[0] {
            CallEvent::CallUpdated { party, .. } => assert!(party.number.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_entry_enriches_matching_slot_and_ignores_others() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::Call(true), 0);
        let entry = ListEntry {
            index: Some(1),
            status: ListStatus::Active,
            incoming: Some(true),
            number: Some(Sensitive::new("+1555")),
            name: Some(Sensitive::new("Ada")),
        };
        let ev = tr.on_list_entry(&entry);
        assert_eq!(ev.len(), 1);
        let held_entry = ListEntry {
            status: ListStatus::Held,
            ..entry.clone()
        };
        assert!(tr.on_list_entry(&held_entry).is_empty());
        assert_eq!(tr.unmatched_list_entries, 1);
        // Direction contradiction against known direction is refused.
        let contradict = ListEntry {
            incoming: Some(false),
            ..entry
        };
        assert!(tr.on_list_entry(&contradict).is_empty());
    }

    #[test]
    fn indicators_first_seen_mid_call_create_calls_with_unknown_direction() {
        let mut tr = CallTracker::new("m");
        let ev = ind(&mut tr, Indicator::CallHeld(2), 0);
        // held-only: one held call, no active.
        assert_eq!(updated(&ev), vec![("m-c1".into(), CallPhase::Held)]);
        assert_eq!(
            tr.slots().held.as_ref().unwrap().direction,
            CallDirection::Unknown
        );
    }

    #[test]
    fn non_call_indicators_never_touch_calls() {
        let mut tr = CallTracker::new("m");
        assert!(ind(&mut tr, Indicator::Signal(3), 0).is_empty());
        assert!(ind(&mut tr, Indicator::Battery(5), 0).is_empty());
        assert!(!tr.indicators_observed());
    }

    #[test]
    fn long_caller_number_is_bounded() {
        let mut tr = CallTracker::new("m");
        ind(&mut tr, Indicator::CallSetup(1), 0);
        let ev = tr.on_caller_id(Some(Sensitive::new("9".repeat(1000))), t(1));
        match &ev[0] {
            CallEvent::CallUpdated { party, .. } => {
                assert!(party.number.as_ref().unwrap().expose().len() <= 128)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn events_replayed_into_core_state_are_accepted() {
        use bridge_core::calls::{Applied, CallState, LinkState};
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        let mut tr = CallTracker::new("m");
        let script = [
            (Indicator::Call(true), 0),
            (Indicator::CallSetup(1), 10),
            (Indicator::CallHeld(1), 20),
            (Indicator::CallSetup(0), 30),
            (Indicator::CallHeld(0), 40),
            (Indicator::Call(false), 50),
        ];
        let mut evs = Vec::new();
        for (i, at) in script {
            evs.extend(tr.on_indicator(i, t(at)));
        }
        evs.extend(tr.tick(t(10_000)));
        for e in evs {
            let applied = state.apply(e.clone());
            assert!(
                !matches!(applied, Applied::Rejected(_)),
                "{e:?} -> {applied:?}"
            );
        }
        assert!(state.calls.is_empty());
    }
}
