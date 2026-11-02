// SPDX-License-Identifier: GPL-3.0-or-later
//! Honest bookkeeping for calendar edits sent to a phone (`geekbridge.calendar.write`).
//!
//! Nothing here touches the link. The ledger records what the user asked for and how far it got:
//! `HandedToPhone` until the phone itself answers, `Applied` / `Failed` from its answer, and
//! `Uncertain` when the link dropped or the phone stayed silent. A retry reuses the same operation
//! id, which the phone answers once (it never writes an id twice), so retrying is always safe.
use bridge_core::{
    phone_link::WriteResult,
    phone_view::{CalendarEditAction, EditItem, EditPhase, MAX_EDITS_SHOWN},
};
use serde_json::{Map, Value};
use std::collections::VecDeque;

/// Most operations remembered per phone; the oldest finished one is dropped first.
const KEEP: usize = 50;
/// No answer within this long after dispatch -> `Uncertain`.
pub const ANSWER_TIMEOUT_MS: u64 = 45_000;
/// Edits per minute per phone.
pub const EDITS_PER_MINUTE: usize = 20;

struct Entry {
    item: EditItem,
    /// Exactly what was sent, so a retry repeats it byte for byte.
    body: Map<String, Value>,
}

#[derive(Default)]
pub struct EditLedger {
    entries: VecDeque<Entry>,
    times: VecDeque<u64>,
}

impl EditLedger {
    pub fn contains(&self, op_id: &str) -> bool {
        self.entries.iter().any(|e| e.item.op_id == op_id)
    }

    /// Records a new operation as handed to the link. False for a repeated id (use [`Self::retry`]).
    pub fn begin(
        &mut self,
        device_id: &str,
        op_id: &str,
        action: CalendarEditAction,
        title: &str,
        body: Map<String, Value>,
        now: u64,
    ) -> bool {
        if self.contains(op_id) {
            return false;
        }
        self.times.push_back(now);
        while self.entries.len() >= KEEP {
            // Prefer dropping a finished one; an in-doubt one must stay visible.
            let at = self
                .entries
                .iter()
                .position(|e| matches!(e.item.phase, EditPhase::Applied | EditPhase::Failed))
                .unwrap_or(0);
            self.entries.remove(at);
        }
        self.entries.push_back(Entry {
            item: EditItem {
                op_id: op_id.to_owned(),
                device_id: device_id.to_owned(),
                action,
                title: title.chars().filter(|c| !c.is_control()).take(80).collect(),
                phase: EditPhase::HandedToPhone,
                reason: None,
                created_unix_ms: now,
                updated_unix_ms: now,
            },
            body,
        });
        true
    }

    /// The link refused the packet: forget the operation, nothing left this computer.
    pub fn abort(&mut self, op_id: &str) {
        self.entries.retain(|e| e.item.op_id != op_id);
    }

    pub fn rate_ok(&mut self, now: u64) -> bool {
        while self
            .times
            .front()
            .is_some_and(|t| now.saturating_sub(*t) > 60_000)
        {
            self.times.pop_front();
        }
        self.times.len() < EDITS_PER_MINUTE
    }

    /// The phone's answer. False when the id is unknown (ignored).
    pub fn on_result(&mut self, r: &WriteResult, now: u64) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.item.op_id == r.op_id) else {
            return false;
        };
        e.item.phase = if r.applied {
            EditPhase::Applied
        } else if r.unknown {
            EditPhase::Uncertain
        } else {
            EditPhase::Failed
        };
        e.item.reason = if r.applied { None } else { r.reason.clone() };
        e.item.updated_unix_ms = now;
        true
    }

    /// The link dropped: whatever was in flight may or may not have been applied.
    pub fn connection_lost(&mut self, now: u64) {
        for e in &mut self.entries {
            if e.item.phase == EditPhase::HandedToPhone {
                e.item.phase = EditPhase::Uncertain;
                e.item.updated_unix_ms = now;
            }
        }
    }

    pub fn tick(&mut self, now: u64) {
        for e in &mut self.entries {
            if e.item.phase == EditPhase::HandedToPhone
                && now.saturating_sub(e.item.updated_unix_ms) > ANSWER_TIMEOUT_MS
            {
                e.item.phase = EditPhase::Uncertain;
                e.item.updated_unix_ms = now;
            }
        }
    }

    /// The packet of an operation that is in doubt or failed, for sending again with the same id.
    pub fn retry_body(&self, op_id: &str) -> Option<&Map<String, Value>> {
        self.entries
            .iter()
            .find(|e| {
                e.item.op_id == op_id
                    && matches!(e.item.phase, EditPhase::Uncertain | EditPhase::Failed)
            })
            .map(|e| &e.body)
    }

    pub fn mark_resent(&mut self, op_id: &str, now: u64) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.item.op_id == op_id) {
            e.item.phase = EditPhase::HandedToPhone;
            e.item.reason = None;
            e.item.updated_unix_ms = now;
        }
    }

    pub fn items(&self) -> Vec<EditItem> {
        self.entries
            .iter()
            .rev()
            .take(MAX_EDITS_SHOWN)
            .map(|e| e.item.clone())
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.times.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(op: &str, applied: bool, reason: Option<&str>) -> WriteResult {
        WriteResult {
            op_id: op.into(),
            applied,
            unknown: false,
            reason: reason.map(str::to_owned),
            event_id: None,
            duplicate: false,
        }
    }

    #[test]
    fn lifecycle_applied_failed_and_uncertain() {
        let mut l = EditLedger::default();
        assert!(l.begin(
            "d",
            "a",
            CalendarEditAction::Create,
            "Lunch",
            Map::new(),
            10
        ));
        assert!(!l.begin(
            "d",
            "a",
            CalendarEditAction::Create,
            "Lunch",
            Map::new(),
            11
        ));
        assert_eq!(l.items()[0].phase, EditPhase::HandedToPhone);
        assert!(l.on_result(&res("a", true, None), 20));
        assert_eq!(l.items()[0].phase, EditPhase::Applied);
        assert!(!l.on_result(&res("zzz", true, None), 20));

        assert!(l.begin("d", "b", CalendarEditAction::Delete, "x", Map::new(), 30));
        l.connection_lost(40);
        assert_eq!(l.items()[0].phase, EditPhase::Uncertain);
        assert!(l.retry_body("b").is_some());
        assert!(
            l.retry_body("a").is_none(),
            "applied edits are never resent"
        );
        // A late answer after the drop still settles the matter.
        assert!(l.on_result(&res("b", false, Some("not_found")), 50));
        assert_eq!(l.items()[0].phase, EditPhase::Failed);
        assert_eq!(l.items()[0].reason.as_deref(), Some("not_found"));
        l.mark_resent("b", 60);
        assert_eq!(l.items()[0].phase, EditPhase::HandedToPhone);
        // The phone itself may say it does not know (interrupted mid-write).
        let mut unsure = res("b", false, Some("interrupted"));
        unsure.unknown = true;
        assert!(l.on_result(&unsure, 70));
        assert_eq!(l.items()[0].phase, EditPhase::Uncertain);
    }

    #[test]
    fn silence_becomes_uncertain_and_rate_is_bounded() {
        let mut l = EditLedger::default();
        l.begin("d", "a", CalendarEditAction::Update, "t", Map::new(), 0);
        l.tick(ANSWER_TIMEOUT_MS);
        assert_eq!(l.items()[0].phase, EditPhase::HandedToPhone);
        l.tick(ANSWER_TIMEOUT_MS + 1);
        assert_eq!(l.items()[0].phase, EditPhase::Uncertain);

        let mut l = EditLedger::default();
        for i in 0..EDITS_PER_MINUTE {
            assert!(l.rate_ok(0));
            l.begin(
                "d",
                &format!("o{i}"),
                CalendarEditAction::Create,
                "t",
                Map::new(),
                0,
            );
        }
        assert!(!l.rate_ok(1000));
        assert!(l.rate_ok(61_000));
    }

    #[test]
    fn memory_is_bounded_and_doubtful_edits_survive() {
        let mut l = EditLedger::default();
        l.begin("d", "keep", CalendarEditAction::Delete, "t", Map::new(), 0);
        l.connection_lost(1);
        for i in 0..KEEP * 2 {
            l.begin(
                "d",
                &format!("o{i}"),
                CalendarEditAction::Create,
                "t",
                Map::new(),
                2,
            );
            l.on_result(&res(&format!("o{i}"), true, None), 3);
        }
        assert!(l.entries.len() <= KEEP);
        assert!(l.contains("keep"));
        assert!(l.items().len() <= MAX_EDITS_SHOWN);
    }
}
