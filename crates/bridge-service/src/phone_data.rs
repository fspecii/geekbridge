// SPDX-License-Identifier: GPL-3.0-or-later
//! Contacts, calendar and SMS from a paired phone, held in memory for the Agenda and Messages
//! windows. The wire contract is `docs/phone-data-link.md`.
//!
//! * **Consent first.** Per device and feature (`Feature::Contacts`, `Calendar`, `Sms`; default
//!   off). With consent off nothing is requested, packets that still arrive are dropped unread and
//!   the stored data of that feature is gone. Unpairing forgets everything.
//! * **Memory only.** No file is written. A restart of the service empties the hub.
//! * **Bounded.** Every inbound packet is size-checked by the parsers, counts of threads, messages,
//!   contacts and events are capped, and inbound traffic is rate limited per device.
//! * **Honest send state.** A send is `HandedToPhone` until the phone itself says more
//!   (`geekbridge.sms.status`, GeekBridge phones only). It is never resent automatically.
use crate::calendar_edit::EditLedger;
use bridge_core::{
    agenda::{Agenda, CalendarEvent, Contact, Date, Record, SourceId, Store, When},
    ipc::{ErrorBody, ErrorCode, Feature},
    messages::{
        Address, Conversation, Direction, MergeOutcome, OutgoingTextIntent, ResendConsent,
        SendEvent, SendId, SendLedger, SendState, SimSelection, SubmissionId, Submit,
        SubscriptionId, ThreadId,
    },
    phone_link::{self as wire, SmsPhase},
    phone_view::{
        AgendaView, CalendarEditAction, CalendarItem, ConversationItem, DataDevice, DataState,
        EventDraft, EventItem, MAX_CONVERSATIONS_SHOWN, MAX_EVENTS_SHOWN, MAX_MESSAGES_LIMIT,
        MAX_MESSAGES_PER_PHONE, MAX_MESSAGES_PER_THREAD, MAX_MESSAGES_SHOWN, MAX_PEOPLE_LIMIT,
        MAX_PEOPLE_SHOWN, MAX_SENDS_SHOWN, MAX_VIEW_DAYS, MessageItem, MessagesView, Person,
        SelectedThread, SendItem, SendPhase, SimItem,
    },
};
use bridge_kde::sms;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

/// A packet the hub wants written to a phone's link.
pub struct Out {
    pub kind: &'static str,
    pub body: Map<String, Value>,
}

/// A newly arrived text, as far as a notifier may know it. `body` is `Some` only when the user
/// chose to show message content in notifications (`PhoneData::set_notify_content`); by default
/// message bodies never reach a notifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingText {
    pub device_name: String,
    /// Contact name, else the number.
    pub sender: String,
    /// The phone number or address, for the reply action.
    pub number: String,
    pub body: Option<String>,
}

/// Desktop notification for a newly arrived text.
pub trait SmsNotifier: Send + Sync {
    fn received(&self, device_name: &str, sender: &str);
    /// Richer form with the number (reply action) and, if allowed, the text. Default: [`Self::received`].
    fn incoming(&self, n: &IncomingText) {
        self.received(&n.device_name, &n.sender);
    }
    /// An event from the shared calendar starts in `minutes` (0 = now). Default: no notification.
    fn event_soon(&self, _device_name: &str, _title: &str, _minutes: u32) {}
}

/// What the link negotiated, for one phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub contacts: bool,
    pub calendar: bool,
    /// The phone accepts `geekbridge.calendar.write` and answers with a result.
    pub calendar_write: bool,
    pub sms_read: bool,
    pub sms_send: bool,
    pub sms_status: bool,
    /// The phone accepts `kdeconnect.battery.request`.
    pub battery_request: bool,
    /// The phone accepts `kdeconnect.notification.request` (dismiss, send-all).
    pub notification_request: bool,
    /// The phone accepts `kdeconnect.notification.action` (press a button).
    pub notification_action: bool,
    /// The phone accepts `geekbridge.photos.request` and answers with a list and thumbnails (GeekBridge phones only).
    pub photos: bool,
}
impl Caps {
    pub fn from_negotiated(outgoing: &[String], incoming: &[String]) -> Self {
        let out = |k: &str| outgoing.iter().any(|x| x == k);
        let inc = |k: &str| incoming.iter().any(|x| x == k);
        Self {
            contacts: out(wire::CONTACTS_REQUEST_UIDS)
                && out(wire::CONTACTS_REQUEST_VCARDS)
                && inc(wire::CONTACTS_UIDS)
                && inc(wire::CONTACTS_VCARDS),
            calendar: out(wire::CALENDAR_REQUEST) && inc(wire::CALENDAR_SYNC),
            calendar_write: out(wire::CALENDAR_WRITE) && inc(wire::CALENDAR_WRITE_RESULT),
            sms_read: out(sms::REQUEST_CONVERSATIONS)
                && out(sms::REQUEST_CONVERSATION)
                && inc(sms::MESSAGES),
            sms_send: out(sms::REQUEST),
            sms_status: inc(wire::SMS_STATUS),
            battery_request: out(bridge_core::battery_link::REQUEST),
            notification_request: out(bridge_core::notification_link::REQUEST),
            notification_action: out(bridge_core::notification_link::ACTION),
            photos: out(bridge_core::photos_link::REQUEST)
                && inc(bridge_core::photos_link::LIST)
                && inc(bridge_core::photos_link::THUMB),
        }
    }
}

/// Verdict for one inbound packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Accepted,
    /// Well-formed but not wanted (consent off, not requested, rate limit): dropped silently.
    Ignored,
    /// Malformed or contradictory: counts against the link's misbehaviour budget.
    Invalid,
}

const MAX_THREADS: usize = 300;
const MAX_MESSAGES_TOTAL: usize = MAX_MESSAGES_PER_PHONE;
const MAX_SEARCH_CHARS: usize = 64;
const MAX_PENDING_VCARDS: usize = 5000;
const VCARD_RETRY_MS: u64 = 15_000;
const REFRESH_AFTER_MS: u64 = 5 * 60 * 1000;
const ACK_TIMEOUT_MS: u64 = 60_000;
const SENDS_PER_MINUTE: usize = 10;
/// SMS parts per minute (a long or emoji text is several, each billed by the carrier).
const SEND_PARTS_PER_MINUTE: usize = 40;
/// Texts remembered per phone (settled ones beyond this are forgotten, oldest first), so a link that stays
/// up for months does not grow the send book or the status snapshot without bound.
const SEND_HISTORY: usize = 200;
/// Shown where a picture message has no text: MMS is read on the phone only, but never silently missing.
pub const MMS_PLACEHOLDER: &str = "MMS message \u{2014} open on phone";
const NOTIFY_MAX_AGE_MS: u64 = 10 * 60 * 1000;
const NOTIFY_MIN_GAP_MS: u64 = 2_000;
/// Most distinct senders announced for one packet; the rest are still in the Messages window.
const NOTIFY_MAX_SENDERS: usize = 3;
const NOTIFY_TRACKED_SENDERS: usize = 64;
const BUCKET_CAPACITY: f64 = 600.0;
const BUCKET_REFILL_PER_SEC: f64 = 60.0;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn err(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
    }
}

struct Bucket {
    tokens: f64,
    at_ms: u64,
}
impl Bucket {
    fn take(&mut self, now: u64) -> bool {
        let dt = now.saturating_sub(self.at_ms) as f64 / 1000.0;
        self.tokens = (self.tokens + dt * BUCKET_REFILL_PER_SEC).min(BUCKET_CAPACITY);
        self.at_ms = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Thread {
    conv: Conversation,
    exhausted: bool,
    asked_older: bool,
    /// Marked read on this computer only: incoming messages up to this phone timestamp count as read.
    read_through_ms: u64,
    /// The per-thread cap dropped older messages, so more history cannot be held.
    capped: bool,
    /// Unread count the phone reported with the conversation list (GeekBridge phones only).
    phone_unread: Option<u32>,
    /// What `phone_unread` was when the user marked the conversation read here.
    unread_at_mark: u32,
    /// Newest message held when the phone last reported its unread counts.
    phone_seen_ms: u64,
}
impl Thread {
    fn new(tid: ThreadId) -> Self {
        Self {
            conv: Conversation::new(tid),
            exhausted: false,
            asked_older: false,
            read_through_ms: 0,
            capped: false,
            phone_unread: None,
            unread_at_mark: 0,
            phone_seen_ms: 0,
        }
    }
    /// Unread here: what is loaded and unread after the local mark, or what the phone counted
    /// beyond what it counted when the user marked it read (the phone cannot be told).
    fn unread(&self) -> u32 {
        let loaded: u32 = self
            .conv
            .iter()
            .filter(|m| {
                !m.read
                    && m.direction() == Direction::Incoming
                    && m.date_ms > self.read_through_ms
                    // Once the phone reports counts, older loaded rows (whose read flag was
                    // never refreshed) are covered by that count; only newer ones add to it.
                    && (self.phone_unread.is_none() || m.date_ms > self.phone_seen_ms)
            })
            .count()
            .try_into()
            .unwrap_or(u32::MAX);
        let phone = self
            .phone_unread
            .unwrap_or(0)
            .saturating_sub(self.unread_at_mark);
        // `loaded` only holds rows newer than the phone's report once it has reported, so the two add.
        loaded.saturating_add(phone)
    }
}

struct Dev {
    name: String,
    generation: u64,
    tx: Option<mpsc::Sender<Out>>,
    caps: Caps,
    consent: [bool; 3], // contacts, calendar, sms
    bucket: Bucket,

    agenda: Agenda,
    contacts_requested: bool,
    listing: HashMap<String, String>,
    pending: VecDeque<String>,
    inflight: HashSet<String>,
    /// When the vCard batch in flight was asked for; a batch nobody answered is asked again.
    inflight_ms: u64,
    contacts_synced: Option<u64>,
    contacts_asked_ms: u64,
    /// The phone listed its maximum number of contacts: more may exist than were shared.
    contacts_capped: bool,
    /// Threads the phone reported holding (GeekBridge phones add `threadsTotal` to the conversation list).
    threads_on_phone: Option<usize>,

    calendar_requested: bool,
    cal_token: Option<String>,
    cal_names: Vec<String>,
    cal_truncated: bool,
    cal_synced: Option<u64>,
    cal_asked_ms: u64,
    cal_request_seq: u64,
    cal_info: Vec<wire::CalendarInfo>,
    /// Separate consents: changing the phone's calendar, and notifying before events.
    edit_consent: bool,
    alert_consent: bool,
    edits: EditLedger,
    alerted: HashSet<String>,

    threads: BTreeMap<ThreadId, Thread>,
    /// SIMs the phone reported with its messages (GeekBridge phones only).
    sims: Vec<SimItem>,
    sms_requested: bool,
    sms_listed: bool,
    sms_asked_ms: u64,
    ledger: SendLedger,
    requests: HashMap<String, SendId>,
    send_reasons: HashMap<SendId, String>,
    /// When each recent text left and how many SMS parts it was, for the per-minute limits.
    send_times: VecDeque<(u64, usize)>,
    /// When each sender last caused a desktop notification (bounded), so one chatty sender cannot hide another.
    notified_at: HashMap<String, u64>,
}

const CONTACTS: usize = 0;
const CALENDAR: usize = 1;
const SMS: usize = 2;

fn slot(f: Feature) -> Option<usize> {
    match f {
        Feature::Contacts => Some(CONTACTS),
        Feature::Calendar => Some(CALENDAR),
        Feature::Sms => Some(SMS),
        _ => None,
    }
}

impl Dev {
    fn new(name: &str, now: u64) -> Self {
        Self {
            name: name.chars().filter(|c| !c.is_control()).take(64).collect(),
            generation: 0,
            tx: None,
            caps: Caps::default(),
            consent: [false; 3],
            bucket: Bucket {
                tokens: BUCKET_CAPACITY,
                at_ms: now,
            },
            agenda: Agenda::default(),
            contacts_requested: false,
            listing: HashMap::new(),
            pending: VecDeque::new(),
            inflight: HashSet::new(),
            inflight_ms: 0,
            contacts_synced: None,
            contacts_asked_ms: 0,
            contacts_capped: false,
            threads_on_phone: None,
            calendar_requested: false,
            cal_token: None,
            cal_names: Vec::new(),
            cal_truncated: false,
            cal_synced: None,
            cal_asked_ms: 0,
            cal_request_seq: 0,
            cal_info: Vec::new(),
            edit_consent: false,
            alert_consent: false,
            edits: EditLedger::default(),
            alerted: HashSet::new(),
            threads: BTreeMap::new(),
            sims: Vec::new(),
            sms_requested: false,
            sms_listed: false,
            sms_asked_ms: 0,
            ledger: SendLedger::new(),
            requests: HashMap::new(),
            send_reasons: HashMap::new(),
            send_times: VecDeque::new(),
            notified_at: HashMap::new(),
        }
    }

    fn connected(&self) -> bool {
        self.tx.is_some()
    }

    fn send(&self, kind: &'static str, body: Map<String, Value>) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.try_send(Out { kind, body }).is_ok())
    }

    fn source(&self, id: &str, what: &str) -> String {
        format!("android:{id}:{what}")
    }

    fn state_of(&self, i: usize) -> DataState {
        let offered = [self.caps.contacts, self.caps.calendar, self.caps.sms_read][i];
        let have = match i {
            CONTACTS => self.contacts_synced.is_some(),
            CALENDAR => self.cal_synced.is_some(),
            _ => self.sms_listed,
        };
        if !self.consent[i] {
            DataState::ConsentOff
        } else if !self.connected() {
            if have {
                DataState::Ready
            } else {
                DataState::Offline
            }
        } else if !offered {
            DataState::NotOffered
        } else if have {
            DataState::Ready
        } else {
            DataState::Waiting
        }
    }

    fn clear(&mut self, i: usize) {
        match i {
            CONTACTS => {
                self.agenda.contacts = Store::default();
                self.listing.clear();
                self.pending.clear();
                self.inflight.clear();
                self.contacts_requested = false;
                self.contacts_synced = None;
                self.contacts_capped = false;
            }
            CALENDAR => {
                self.agenda.events = Store::default();
                self.calendar_requested = false;
                self.cal_token = None;
                self.cal_names.clear();
                self.cal_info.clear();
                self.alerted.clear();
                self.cal_truncated = false;
                self.cal_synced = None;
            }
            _ => {
                self.threads.clear();
                self.threads_on_phone = None;
                self.sims.clear();
                self.sms_requested = false;
                self.sms_listed = false;
                self.ledger = SendLedger::new();
                self.requests.clear();
                self.send_reasons.clear();
            }
        }
    }

    // ------------------------------------------------------------ requests

    fn ask_contacts(&mut self, now: u64) {
        if self.consent[CONTACTS]
            && self.caps.contacts
            && self.send(wire::CONTACTS_REQUEST_UIDS, Map::new())
        {
            self.contacts_requested = true;
            self.contacts_asked_ms = now;
            // Anything still queued from an earlier listing is superseded by the new answer.
            self.pending.clear();
            self.inflight.clear();
        }
    }

    fn ask_calendar(&mut self, now: u64, full: bool) {
        if self.consent[CALENDAR] && self.caps.calendar {
            self.cal_request_seq += 1;
            let rid = format!("cal{}-{}", self.cal_request_seq, now);
            let since = if full {
                None
            } else {
                self.cal_token.as_deref()
            };
            if self.send(
                wire::CALENDAR_REQUEST,
                wire::calendar_request_body(&rid, since),
            ) {
                self.calendar_requested = true;
                self.cal_asked_ms = now;
            }
        }
    }

    fn ask_sms(&mut self, now: u64) {
        if self.consent[SMS]
            && self.caps.sms_read
            && self.send(
                sms::REQUEST_CONVERSATIONS,
                sms::request_conversations_body(),
            )
        {
            self.sms_requested = true;
            self.sms_asked_ms = now;
        }
    }

    /// A vCard batch the phone never answered (dropped answer, busy phone) is asked for again, so a
    /// long contact sync cannot stall for good on one lost packet.
    fn retry_stalled_vcards(&mut self, now: u64) {
        if !self.inflight.is_empty()
            && self.connected()
            && now.saturating_sub(self.inflight_ms) > VCARD_RETRY_MS
        {
            let mut again: Vec<String> = self.inflight.drain().collect();
            again.sort();
            for uid in again.into_iter().rev() {
                self.pending.push_front(uid);
            }
            self.next_vcard_batch();
        }
    }

    fn next_vcard_batch(&mut self) {
        if !self.inflight.is_empty() {
            return;
        }
        let batch: Vec<String> = (0..wire::VCARD_BATCH)
            .map_while(|_| self.pending.pop_front())
            .collect();
        if batch.is_empty() {
            self.contacts_synced = Some(now_ms());
            return;
        }
        if self.send(
            wire::CONTACTS_REQUEST_VCARDS,
            wire::request_vcards_body(&batch),
        ) {
            self.inflight = batch.into_iter().collect();
            self.inflight_ms = now_ms();
        } else {
            self.pending.clear();
        }
    }

    // ------------------------------------------------------------ inbound

    fn on_uids(&mut self, id: &str, body: &Map<String, Value>) -> Disposition {
        if !self.consent[CONTACTS] || !self.contacts_requested {
            return Disposition::Ignored;
        }
        let Ok(remote) = wire::parse_uids(body) else {
            return Disposition::Invalid;
        };
        let source = self.source(id, "contacts");
        let refs: Vec<(SourceId, _)> = remote
            .iter()
            .map(|(u, t)| {
                (
                    SourceId::new(source.clone(), u.clone()),
                    bridge_core::agenda::Revision(t.clone()),
                )
            })
            .collect();
        let Ok(plan) = self.agenda.contacts.plan_sync(&refs) else {
            return Disposition::Invalid;
        };
        // An empty listing is ambiguous (permission revoked, provider not ready): keep what we have.
        if remote.is_empty() && !plan.absent.is_empty() {
            self.contacts_synced = Some(now_ms());
            return Disposition::Accepted;
        }
        let now = now_ms();
        for gone in &plan.absent {
            let _ = self.agenda.contacts.mark_deleted(gone, now);
        }
        self.contacts_capped = remote.len() >= wire::MAX_CONTACT_UIDS;
        self.listing = remote.into_iter().collect();
        self.pending = plan
            .fetch
            .into_iter()
            .map(|s| s.id)
            .take(MAX_PENDING_VCARDS)
            .collect();
        self.inflight.clear();
        self.next_vcard_batch();
        Disposition::Accepted
    }

    fn on_vcards(&mut self, id: &str, body: &Map<String, Value>) -> Disposition {
        if !self.consent[CONTACTS] || self.inflight.is_empty() {
            return Disposition::Ignored;
        }
        let Ok(cards) = wire::parse_vcards(body) else {
            return Disposition::Invalid;
        };
        let source = self.source(id, "contacts");
        for (uid, vcard) in cards {
            if !self.inflight.remove(&uid) {
                continue; // never asked for it
            }
            let Ok(mut contact) = wire::contact_from_vcard(&vcard) else {
                continue;
            };
            // Nothing displays the raw card, and 5,000 of them held up to 64 KiB each. Keep the parsed fields only.
            contact.vcard = None;
            let rev = wire::contact_revision(self.listing.get(&uid).map(String::as_str), &vcard);
            let sid = SourceId::new(source.clone(), uid);
            let base = self.agenda.contacts.get(&sid).map(|r| r.revision.clone());
            let _ = self
                .agenda
                .contacts
                .apply(Record::live(sid, rev, contact), base.as_ref());
        }
        // The phone omits contacts it could not read; they will not come again this round.
        self.inflight.clear();
        self.next_vcard_batch();
        Disposition::Accepted
    }

    fn on_calendar(&mut self, id: &str, body: &Map<String, Value>, now: u64) -> Disposition {
        if !self.consent[CALENDAR] || !self.calendar_requested {
            return Disposition::Ignored;
        }
        let Ok(sync) = wire::parse_calendar_sync(body) else {
            return Disposition::Invalid;
        };
        let source = self.source(id, "calendar");
        match wire::apply_calendar(
            &mut self.agenda.events,
            &source,
            self.cal_token.as_deref(),
            &sync,
            now,
        ) {
            Ok(_) => {
                self.cal_token = Some(sync.token.clone());
                self.cal_names = sync.calendars.iter().map(|c| c.name.clone()).collect();
                self.cal_info = sync.calendars.clone();
                self.cal_truncated = sync.truncated;
                self.cal_synced = Some(now);
            }
            // We missed a step (restart on either side): ask for the whole window again.
            Err(wire::CalendarMismatch::BaseMismatch) => self.ask_calendar(now, true),
        }
        Disposition::Accepted
    }

    fn on_messages(
        &mut self,
        body: &Map<String, Value>,
        now: u64,
    ) -> (Disposition, Vec<(String, String)>) {
        if !self.consent[SMS] || !self.sms_requested {
            return (Disposition::Ignored, Vec::new());
        }
        let Ok(parsed) = sms::parse_messages(body) else {
            return (Disposition::Invalid, Vec::new());
        };
        if let Some(sims) = sms::parse_sims(body) {
            self.sims = sims;
        }
        let phone_unread = sms::parse_unread(body);
        let has_unread_map = sms::has_unread_map(body);
        let packet_threads: HashSet<ThreadId> =
            parsed.messages.iter().map(|m| m.key.thread_id).collect();
        if let Some(n) = body.get("threadsTotal").and_then(Value::as_u64) {
            self.threads_on_phone = Some(usize::try_from(n.min(1_000_000)).unwrap_or(0));
        }
        let total: usize = self.threads.values().map(|t| t.conv.len()).sum();
        let mut room = MAX_MESSAGES_TOTAL.saturating_sub(total);
        let mut notify = Vec::new();
        let mut fresh = Vec::new();
        let mut touched = HashSet::new();
        for m in parsed.messages {
            let tid = m.key.thread_id;
            if !self.threads.contains_key(&tid)
                && self.threads.len() >= MAX_THREADS
                && !self.drop_stalest_thread_before(m.date_ms)
            {
                continue;
            }
            // Only a message that would add a row needs room; a changed one replaces in place.
            let adds = self
                .threads
                .get(&tid)
                .is_none_or(|t| t.conv.get(&m.key).is_none());
            if adds && room == 0 {
                // Memory for this phone is full: make room by dropping the oldest history of the
                // biggest thread instead of refusing what just arrived.
                let freed = self.evict_for_room();
                if freed == 0 {
                    break;
                }
                room = freed;
            }
            // The thread exists only once its first message is going in (no empty ghost on a refusal).
            let t = self.threads.entry(tid).or_insert_with(|| Thread::new(tid));
            let incoming = m.direction() == Direction::Incoming;
            let unread = !m.read;
            let recent = now.saturating_sub(m.date_ms) <= NOTIFY_MAX_AGE_MS;
            match t.conv.merge(m.clone()) {
                Ok(MergeOutcome::Inserted) => {
                    room -= 1;
                    touched.insert(tid);
                    if incoming
                        && unread
                        && recent
                        && self.sms_listed
                        && let Some(s) = m.addresses.first()
                    {
                        notify.push((s.as_str().to_owned(), m.body.to_string()));
                    }
                    fresh.push(m);
                }
                Ok(MergeOutcome::Updated) => {
                    touched.insert(tid);
                    fresh.push(m);
                }
                _ => {}
            }
        }
        // History pages that add nothing mean the phone has no more.
        for tid in touched {
            if let Some(t) = self.threads.get_mut(&tid) {
                t.asked_older = false;
                if t.conv.trim_oldest(MAX_MESSAGES_PER_THREAD) > 0 {
                    t.capped = true;
                }
            }
        }
        for (tid, t) in &mut self.threads {
            // Only a packet that answers this thread (nothing in it from any other thread) can
            // say the phone has no more; unrelated traffic leaves the request pending.
            if t.asked_older && packet_threads.iter().all(|p| p == tid) {
                t.exhausted = true;
                t.asked_older = false;
            }
        }
        if has_unread_map {
            // A thread missing from the phone's map has nothing unread there.
            let counts: HashMap<ThreadId, u32> = phone_unread.into_iter().collect();
            for (tid, t) in &mut self.threads {
                let n = counts.get(tid).copied().unwrap_or(0);
                t.phone_unread = Some(n);
                t.phone_seen_ms = t.conv.newest_date_ms().unwrap_or(0);
                // Read on the phone since the mark: the baseline can only fall with the count.
                t.unread_at_mark = t.unread_at_mark.min(n);
            }
        }
        self.correlate(&fresh, now);
        self.sms_listed = true;
        let notify = self.pick_notifications(notify, now);
        (Disposition::Accepted, notify)
    }

    /// The list is full: forget the conversation whose newest message is oldest, but only if a message
    /// from a new conversation is newer than it (a text from the 301st person must not vanish).
    fn drop_stalest_thread_before(&mut self, date_ms: u64) -> bool {
        let stalest = self
            .threads
            .iter()
            .map(|(k, t)| (t.conv.newest_date_ms().unwrap_or(0), *k))
            .min();
        match stalest {
            Some((d, k)) if d < date_ms => self.threads.remove(&k).is_some(),
            _ => false,
        }
    }

    /// One notification per sender per gap, at most [`NOTIFY_MAX_SENDERS`] senders per packet, so a flood
    /// of texts cannot flood the desktop but a second person's text is not swallowed by the first's.
    fn pick_notifications(
        &mut self,
        candidates: Vec<(String, String)>,
        now: u64,
    ) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for (sender, text) in candidates {
            if out.len() >= NOTIFY_MAX_SENDERS
                || out
                    .iter()
                    .any(|(s, _)| sender_key(s) == sender_key(&sender))
            {
                continue;
            }
            let last = self
                .notified_at
                .get(&sender_key(&sender))
                .copied()
                .unwrap_or(0);
            if now.saturating_sub(last) < NOTIFY_MIN_GAP_MS {
                continue;
            }
            out.push((sender, text));
        }
        if self.notified_at.len() + out.len() > NOTIFY_TRACKED_SENDERS {
            self.notified_at
                .retain(|_, t| now.saturating_sub(*t) < NOTIFY_MIN_GAP_MS);
        }
        for (sender, _) in &out {
            if self.notified_at.len() < NOTIFY_TRACKED_SENDERS {
                self.notified_at.insert(sender_key(sender), now);
            }
        }
        out
    }

    /// Frees room by dropping the oldest messages on the phone (a batch of up to 100, taken one at a time
    /// from whichever thread holds the oldest one, never emptying a thread); returns how many slots opened.
    fn evict_for_room(&mut self) -> usize {
        let mut freed = 0;
        while freed < 100 {
            let Some(t) = self
                .threads
                .values_mut()
                .filter(|t| t.conv.len() > 1)
                .min_by_key(|t| t.conv.oldest_date_ms().unwrap_or(u64::MAX))
            else {
                break;
            };
            let keep = t.conv.len() - 1;
            let n = t.conv.trim_oldest(keep);
            if n == 0 {
                break;
            }
            t.capped = true;
            freed += n;
        }
        freed
    }

    /// Stock phones send no status packets; a newly seen outgoing message that matches exactly one
    /// of our dispatched sends is the only evidence available. Ambiguity is never resolved by guessing.
    fn correlate(&mut self, fresh: &[bridge_core::messages::Message], now: u64) {
        if fresh.is_empty() {
            return;
        }
        let ids: Vec<SendId> = self.requests.values().copied().collect();
        for sid in ids {
            let Some(rec) = self.ledger.get(sid) else {
                continue;
            };
            if !matches!(
                rec.state(),
                SendState::Queued | SendState::AcceptedByPhone | SendState::Uncertain { .. }
            ) || !rec.was_dispatched()
            {
                continue;
            }
            let not_before = rec.created_ms.saturating_sub(2 * 60 * 1000);
            if let bridge_core::messages::Correlation::Unique(key) =
                rec.correlate(fresh, not_before)
                && let Some(m) = fresh.iter().find(|m| m.key == key)
                && let Some(ev) = m.message_box.send_evidence()
            {
                let _ = self.ledger.apply(sid, ev, now);
            }
        }
    }

    fn on_sms_status(&mut self, body: &Map<String, Value>, now: u64) -> Disposition {
        if !self.consent[SMS] {
            return Disposition::Ignored;
        }
        let Ok(st) = wire::parse_sms_status(body) else {
            return Disposition::Invalid;
        };
        let Some(&sid) = self.requests.get(&st.request_id) else {
            return Disposition::Ignored;
        };
        let ev = match st.phase {
            SmsPhase::Accepted => SendEvent::PhoneAccepted,
            SmsPhase::Sent => SendEvent::PhoneSent,
            SmsPhase::Delivered => SendEvent::PhoneDelivered,
            SmsPhase::Failed => SendEvent::PhoneFailed,
        };
        let _ = self.ledger.apply(sid, ev, now);
        if let Some(r) = st.reason {
            self.send_reasons.insert(sid, r);
        }
        Disposition::Accepted
    }

    fn tick_sends(&mut self, now: u64) {
        if !self.caps.sms_status {
            return;
        }
        let stale: Vec<SendId> = self
            .requests
            .values()
            .copied()
            .filter(|sid| {
                self.ledger.get(*sid).is_some_and(|r| {
                    r.state() == SendState::Queued
                        && r.was_dispatched()
                        && now.saturating_sub(r.updated_ms) > ACK_TIMEOUT_MS
                })
            })
            .collect();
        for sid in stale {
            let _ = self.ledger.apply(sid, SendEvent::AckTimedOut, now);
        }
    }
}

impl Dev {
    fn edit_state(&self) -> DataState {
        if !self.edit_consent || !self.consent[CALENDAR] {
            DataState::ConsentOff
        } else if !self.connected() {
            DataState::Offline
        } else if !self.caps.calendar_write {
            DataState::NotOffered
        } else {
            DataState::Ready
        }
    }

    fn on_write_result(&mut self, body: &Map<String, Value>, now: u64) -> Disposition {
        if !self.edit_consent {
            return Disposition::Ignored;
        }
        let Ok(r) = wire::parse_write_result(body) else {
            return Disposition::Invalid;
        };
        if !self.edits.on_result(&r, now) {
            return Disposition::Ignored;
        }
        if r.applied || r.reason.as_deref() == Some("changed") {
            // Show the change (or, after a refused stale edit, the phone's newer version) right away instead of
            // waiting for the phone's own push.
            self.ask_calendar(now, false);
        }
        Disposition::Accepted
    }
}

/// Minutes before an event at which it is announced: its largest reminder, else 10, at most 60.
fn alert_lead_minutes(e: &CalendarEvent) -> u32 {
    e.meta
        .reminders_min
        .iter()
        .copied()
        .max()
        .unwrap_or(10)
        .clamp(1, 60)
}

#[derive(Default)]
struct Inner {
    devices: HashMap<String, Dev>,
    next_generation: u64,
}

pub struct PhoneData {
    inner: Mutex<Inner>,
    notifier: Mutex<Option<Arc<dyn SmsNotifier>>>,
    /// Show message text in new-message notifications. Off unless the user chose it.
    notify_content: AtomicBool,
}

impl Default for PhoneData {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            notifier: Mutex::new(None),
            notify_content: AtomicBool::new(false),
        }
    }
}

impl PhoneData {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `true` lets a notification carry the message text; the default hides it.
    pub fn set_notify_content(&self, show: bool) {
        self.notify_content.store(show, Ordering::Relaxed);
    }

    pub fn set_notifier(&self, n: Arc<dyn SmsNotifier>) {
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = Some(n);
    }

    /// A trusted link came up. `consents` are (contacts, calendar, sms). Returns the channel the
    /// link loop drains and a generation for [`Self::link_down`].
    pub fn link_up(
        &self,
        id: &str,
        name: &str,
        caps: Caps,
        consents: (bool, bool, bool),
    ) -> (mpsc::Receiver<Out>, u64) {
        let (tx, rx) = mpsc::channel(64);
        let now = now_ms();
        let mut g = self.lock();
        g.next_generation += 1;
        let generation = g.next_generation;
        let d = g
            .devices
            .entry(id.to_owned())
            .or_insert_with(|| Dev::new(name, now));
        d.name = name.chars().filter(|c| !c.is_control()).take(64).collect();
        d.generation = generation;
        d.tx = Some(tx);
        d.caps = caps;
        d.consent = [consents.0, consents.1, consents.2];
        // The phone no longer offers a feature (its owner switched it off, or took the permission away): what was
        // held from the last session goes now, not at the next restart. The desktop's own consent is separate.
        if !caps.contacts {
            d.clear(CONTACTS);
        }
        if !caps.calendar {
            d.clear(CALENDAR);
        }
        if !caps.sms_read {
            d.clear(SMS);
        }
        // A new link is a new session: ask again from scratch.
        d.contacts_requested = false;
        d.calendar_requested = false;
        d.sms_requested = false;
        d.sms_listed = false;
        d.ask_contacts(now);
        d.ask_calendar(now, false);
        d.ask_sms(now);
        (rx, generation)
    }

    /// The link of `generation` ended. True if it was the current one (a newer link may already be up).
    pub fn link_down(&self, id: &str, generation: u64) -> bool {
        let mut g = self.lock();
        if let Some(d) = g.devices.get_mut(id)
            && d.generation == generation
        {
            d.tx = None;
            d.ledger.connection_lost(now_ms());
            d.edits.connection_lost(now_ms());
            return true;
        }
        false
    }

    /// The user changed a consent. Turning one off erases that feature's data at once.
    pub fn set_enabled(&self, id: &str, feature: Feature, on: bool) {
        if matches!(feature, Feature::CalendarEdit | Feature::CalendarAlerts) {
            let mut g = self.lock();
            let d = g
                .devices
                .entry(id.to_owned())
                .or_insert_with(|| Dev::new("", now_ms()));
            if feature == Feature::CalendarEdit {
                d.edit_consent = on;
                if !on {
                    d.edits.clear();
                }
            } else {
                d.alert_consent = on;
                d.alerted.clear();
            }
            return;
        }
        let Some(i) = slot(feature) else { return };
        let now = now_ms();
        let mut g = self.lock();
        let d = g
            .devices
            .entry(id.to_owned())
            .or_insert_with(|| Dev::new("", now));
        if d.consent[i] == on {
            return;
        }
        d.consent[i] = on;
        if !on {
            d.clear(i);
        } else if d.connected() {
            match i {
                CONTACTS => d.ask_contacts(now),
                CALENDAR => d.ask_calendar(now, true),
                _ => d.ask_sms(now),
            }
        }
    }

    /// Ask a connected phone for its battery state now (it answers with a `kdeconnect.battery` packet).
    /// Only if the phone declared the request; the caller checked the `battery` consent.
    pub fn request_battery(&self, id: &str) {
        let g = self.lock();
        if let Some(d) = g.devices.get(id)
            && d.caps.battery_request
        {
            d.send(
                bridge_core::battery_link::REQUEST,
                bridge_core::battery_link::request_body(),
            );
        }
    }

    /// What the connected phone's link negotiated; all false when it is not connected.
    pub fn caps(&self, id: &str) -> Caps {
        self.lock()
            .devices
            .get(id)
            .filter(|d| d.connected())
            .map(|d| d.caps)
            .unwrap_or_default()
    }

    /// Queue one stock packet (no data-feature bookkeeping) on a connected phone's link. The caller checked
    /// the consent and the negotiated capability. False if the phone is not connected or the queue is full.
    pub fn send_stock(&self, id: &str, kind: &'static str, body: Map<String, Value>) -> bool {
        self.lock()
            .devices
            .get(id)
            .is_some_and(|d| d.send(kind, body))
    }

    /// Unpaired: drop everything held for the device.
    pub fn forget(&self, id: &str) {
        self.lock().devices.remove(id);
    }

    /// An inbound packet of a phone-data kind. See [`Disposition`].
    pub fn on_packet(&self, id: &str, kind: &str, body: &Map<String, Value>) -> Disposition {
        let now = now_ms();
        let mut notices = Vec::new();
        let verdict = {
            let mut g = self.lock();
            let Some(d) = g.devices.get_mut(id) else {
                return Disposition::Ignored;
            };
            if !d.bucket.take(now) {
                return Disposition::Ignored;
            }
            match kind {
                wire::CONTACTS_UIDS => d.on_uids(id, body),
                wire::CONTACTS_VCARDS => d.on_vcards(id, body),
                wire::CALENDAR_SYNC => d.on_calendar(id, body, now),
                wire::CALENDAR_WRITE_RESULT => d.on_write_result(body, now),
                sms::MESSAGES => {
                    let (v, notify) = d.on_messages(body, now);
                    for (number, text) in notify {
                        notices.push(IncomingText {
                            device_name: d.name.clone(),
                            sender: self.display_name(d, &number),
                            number,
                            body: self.notify_content.load(Ordering::Relaxed).then_some(text),
                        });
                    }
                    crate::tray::poke(); // new or changed messages move the tray's unread count
                    v
                }
                wire::SMS_STATUS => d.on_sms_status(body, now),
                _ => Disposition::Invalid,
            }
        };
        if !notices.is_empty()
            && let Some(n) = self
                .notifier
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        {
            for text in &notices {
                n.incoming(text);
            }
        }
        verdict
    }

    fn display_name(&self, d: &Dev, address: &str) -> String {
        let names = name_index(d);
        lookup_name(&names, address).unwrap_or_else(|| {
            address
                .chars()
                .filter(|c| !c.is_control())
                .take(40)
                .collect()
        })
    }

    // ------------------------------------------------------------ requests from the windows

    fn target<'a>(g: &'a mut Inner, device: &str) -> Result<&'a mut Dev, ErrorBody> {
        g.devices
            .get_mut(device)
            .ok_or_else(|| err(ErrorCode::NotFound, "no data from that phone"))
    }

    pub fn refresh_agenda(&self, device: Option<&str>) -> Result<(), ErrorBody> {
        let now = now_ms();
        let mut g = self.lock();
        let ids: Vec<String> = match device {
            Some(d) => vec![d.to_owned()],
            None => g.devices.keys().cloned().collect(),
        };
        let mut any = false;
        for id in ids {
            let d = Self::target(&mut g, &id)?;
            if d.connected() {
                d.ask_contacts(now);
                d.ask_calendar(now, false);
                any = true;
            }
        }
        if any || device.is_none() {
            Ok(())
        } else {
            Err(err(ErrorCode::NotConnected, "the phone is not connected"))
        }
    }

    pub fn refresh_messages(&self, device: &str, thread: Option<i64>) -> Result<(), ErrorBody> {
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        if !d.connected() {
            return Err(err(ErrorCode::NotConnected, "the phone is not connected"));
        }
        if !d.consent[SMS] || !d.caps.sms_read {
            return Err(err(
                ErrorCode::Unsupported,
                "messages are not shared by this phone",
            ));
        }
        match thread {
            None => d.ask_sms(now),
            Some(t) => {
                let tid = ThreadId(t);
                let th = d
                    .threads
                    .get_mut(&tid)
                    .ok_or_else(|| err(ErrorCode::NotFound, "unknown conversation"))?;
                if th.exhausted {
                    return Ok(());
                }
                let start = th.conv.oldest_date_ms();
                th.asked_older = true;
                let body = sms::request_conversation_body(tid, start, sms::PAGE_SIZE);
                if !d.send(sms::REQUEST_CONVERSATION, body) {
                    return Err(err(ErrorCode::Busy, "the link is busy, try again"));
                }
            }
        }
        Ok(())
    }

    /// Marks a conversation read on this computer only. The KDE protocol has no way to tell the
    /// phone, so the phone's own unread state is untouched and the mark is forgotten with the data.
    pub fn mark_read(&self, device: &str, thread: i64) -> Result<(), ErrorBody> {
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        let th = d
            .threads
            .get_mut(&ThreadId(thread))
            .ok_or_else(|| err(ErrorCode::NotFound, "unknown conversation"))?;
        th.read_through_ms = th.conv.newest_date_ms().unwrap_or(0);
        th.unread_at_mark = th.phone_unread.unwrap_or(0);
        drop(g);
        crate::tray::poke(); // the tray's unread count follows the mark
        Ok(())
    }

    /// Unread messages over every phone whose Messages consent is on (the tray tooltip and the
    /// waybar `unread` field). A count only: no text, no names.
    pub fn unread_total(&self) -> u32 {
        let g = self.lock();
        g.devices
            .values()
            .filter(|d| d.consent[SMS])
            .flat_map(|d| d.threads.values())
            .fold(0_u32, |n, t| n.saturating_add(t.unread()))
    }

    pub fn send_text(
        &self,
        device: &str,
        recipient: &str,
        text: &str,
        submission: &str,
        sub_id: Option<i32>,
    ) -> Result<u64, ErrorBody> {
        let submission =
            SubmissionId::new(submission).map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        // Only a SIM the phone itself listed can be chosen; anything else would fall back silently.
        let sim = match sub_id {
            None => SimSelection::PhoneDefault,
            Some(id) if d.sims.iter().any(|s| s.id == id) => SimSelection::Subscription(
                SubscriptionId::new(id).map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?,
            ),
            Some(_) => {
                return Err(err(
                    ErrorCode::BadRequest,
                    "that SIM is not one the phone reported",
                ));
            }
        };
        let intent = OutgoingTextIntent::new(&[recipient], text, sim)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        d.check_can_send(now, parts_of(intent.text()))?;
        let id = match d
            .ledger
            .submit(submission, intent.clone(), now)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?
        {
            Submit::Existing(id) => return Ok(id.0), // same click again: never a second text
            Submit::Created(id) => id,
        };
        d.dispatch(id, &intent, now)?;
        Ok(id.0)
    }

    /// Whether a text could be sent to `device` right now: Messages is allowed for it on this
    /// computer, the phone is connected and takes texts to send (its own switch is on), and the
    /// per-minute limit has room. The same checks `send_text` makes, without sending anything.
    pub fn can_send_text(&self, device: &str) -> bool {
        let now = now_ms();
        let mut g = self.lock();
        Self::target(&mut g, device).is_ok_and(|d| d.check_can_send(now, 1).is_ok())
    }

    pub fn resend(&self, device: &str, send_id: u64, submission: &str) -> Result<u64, ErrorBody> {
        let submission =
            SubmissionId::new(submission).map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        let intent = d
            .ledger
            .get(SendId(send_id))
            .ok_or_else(|| err(ErrorCode::NotFound, "unknown message"))?
            .intent
            .clone();
        d.check_can_send(now, parts_of(intent.text()))?;
        let id = match d
            .ledger
            .resend(
                SendId(send_id),
                submission,
                ResendConsent::UserAcceptsDuplicateRisk,
                now,
            )
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?
        {
            Submit::Existing(id) => return Ok(id.0),
            Submit::Created(id) => id,
        };
        d.dispatch(id, &intent, now)?;
        Ok(id.0)
    }

    // ------------------------------------------------------------ calendar edits

    /// Sends one create / update / delete to the phone. Refused (nothing leaves this computer)
    /// unless the user allowed editing for this phone, the phone offers it, and the request is
    /// complete: creating needs an explicit writable calendar, changing needs a known one-off event.
    pub fn calendar_edit(
        &self,
        device: &str,
        op_id: &str,
        action: CalendarEditAction,
        calendar_id: Option<&str>,
        event_id: Option<&str>,
        draft: Option<&EventDraft>,
    ) -> Result<(), ErrorBody> {
        let bad = |m: &str| err(ErrorCode::BadRequest, m);
        if !wire::valid_request_id(op_id) {
            return Err(bad("operation id must be 1-64 letters, digits, - or _"));
        }
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        if !d.edit_consent || !d.consent[CALENDAR] {
            return Err(err(
                ErrorCode::Unsupported,
                "Editing the calendar is not allowed for this phone",
            ));
        }
        if !d.connected() {
            return Err(err(
                ErrorCode::NotConnected,
                "the phone is not connected, nothing was changed",
            ));
        }
        if !d.caps.calendar_write {
            return Err(err(
                ErrorCode::Unsupported,
                "this phone does not offer calendar editing (turn it on in the GeekBridge app)",
            ));
        }
        if d.edits.contains(op_id) {
            return Err(bad("that operation was already sent"));
        }
        if !d.edits.rate_ok(now) {
            return Err(err(ErrorCode::TooMany, "too many changes in a minute"));
        }
        let title: String;
        let mut base_rev: Option<String> = None;
        match action {
            CalendarEditAction::Create => {
                let cal = calendar_id.ok_or_else(|| bad("choose a calendar"))?;
                let info = d
                    .cal_info
                    .iter()
                    .find(|c| c.id == cal)
                    .ok_or_else(|| bad("that calendar is not shared by the phone"))?;
                if !info.writable {
                    return Err(bad("that calendar is read-only on the phone"));
                }
                let dr = draft.ok_or_else(|| bad("missing event details"))?;
                wire::validate_draft(dr).map_err(|e| bad(e.message()))?;
                title = dr.title.trim().to_owned();
            }
            CalendarEditAction::Update | CalendarEditAction::Delete => {
                let eid = event_id.ok_or_else(|| bad("missing event"))?;
                let (record, known) = d
                    .agenda
                    .events
                    .live()
                    .find(|(_, e)| e.meta.event_id.as_deref() == Some(eid))
                    .ok_or_else(|| bad("that event is not on the phone any more"))?;
                // The version the user was looking at: the phone refuses when its copy has moved on since.
                base_rev = Some(record.revision.0.clone());
                if known.meta.recurring || known.recurrence.is_some() {
                    return Err(bad("repeating events cannot be changed from the computer"));
                }
                let writable = known
                    .meta
                    .calendar_id
                    .as_deref()
                    .is_some_and(|cid| d.cal_info.iter().any(|c| c.id == cid && c.writable));
                if !writable {
                    return Err(bad("that calendar is read-only on the phone"));
                }
                title = known.title.clone();
                if action == CalendarEditAction::Update {
                    let dr = draft.ok_or_else(|| bad("missing event details"))?;
                    wire::validate_draft(dr).map_err(|e| bad(e.message()))?;
                }
            }
        }
        let body = wire::calendar_write_body(
            op_id,
            action,
            calendar_id,
            event_id,
            draft,
            base_rev.as_deref(),
        );
        if !d
            .edits
            .begin(device, op_id, action, &title, body.clone(), now)
        {
            return Err(bad("that operation was already sent"));
        }
        if !d.send(wire::CALENDAR_WRITE, body) {
            d.edits.abort(op_id);
            return Err(err(
                ErrorCode::Busy,
                "the link is busy, nothing was changed",
            ));
        }
        Ok(())
    }

    /// Sends an in-doubt (or failed) edit again with its original operation id.
    pub fn calendar_edit_retry(&self, device: &str, op_id: &str) -> Result<(), ErrorBody> {
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        if !d.edit_consent || !d.connected() || !d.caps.calendar_write {
            return Err(err(
                ErrorCode::NotConnected,
                "the phone is not ready for calendar changes",
            ));
        }
        let body = d
            .edits
            .retry_body(op_id)
            .cloned()
            .ok_or_else(|| err(ErrorCode::NotFound, "nothing to send again"))?;
        if !d.send(wire::CALENDAR_WRITE, body) {
            return Err(err(ErrorCode::Busy, "the link is busy, nothing was sent"));
        }
        d.edits.mark_resent(op_id, now);
        Ok(())
    }

    /// Announces events that are about to start for phones with the alerts consent. Called about
    /// every 30 seconds; each event instance is announced once.
    pub fn tick_alerts(&self) {
        let now = now_ms();
        let mut due: Vec<(String, String, u32)> = Vec::new();
        {
            let mut g = self.lock();
            for d in g.devices.values_mut() {
                d.edits.tick(now);
                if !d.alert_consent || !d.consent[CALENDAR] {
                    continue;
                }
                let mut live = HashSet::new();
                for (rec, e) in d.agenda.events.live() {
                    let When::Timed { start_utc_ms, .. } = &e.when else {
                        continue;
                    };
                    let key = format!("{}@{}", rec.id.id, rec.revision.0);
                    live.insert(key.clone());
                    let start = u64::try_from(*start_utc_ms).unwrap_or(0);
                    let lead = u64::from(alert_lead_minutes(e)) * 60_000;
                    // Never announce something that began more than a minute ago (late link, restart).
                    if now + lead >= start && now < start + 60_000 && d.alerted.insert(key) {
                        let minutes =
                            u32::try_from(start.saturating_sub(now).div_ceil(60_000)).unwrap_or(0);
                        due.push((d.name.clone(), e.title.clone(), minutes));
                    }
                }
                d.alerted.retain(|k| live.contains(k));
            }
        }
        if due.is_empty() {
            return;
        }
        let notifier = self
            .notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(n) = notifier {
            for (device, title, minutes) in due {
                n.event_soon(&device, &title, minutes);
            }
        }
    }

    // ------------------------------------------------------------ views

    fn devices(g: &Inner) -> Vec<DataDevice> {
        let mut v: Vec<DataDevice> = g
            .devices
            .iter()
            .filter(|(_, d)| d.consent.iter().any(|c| *c))
            .map(|(id, d)| DataDevice {
                device_id: id.clone(),
                name: d.name.clone(),
                contacts: d.state_of(CONTACTS),
                calendar: d.state_of(CALENDAR),
                calendar_edit: d.edit_state(),
                calendar_writable: d.cal_info.iter().any(|c| c.writable),
                sms: d.state_of(SMS),
                contacts_count: d.agenda.contacts.live().count(),
                events_count: d.agenda.events.live().count(),
                calendar_names: d.cal_names.clone(),
                calendar_truncated: d.cal_truncated,
                contacts_capped: d.contacts_capped,
                threads_on_phone: d.threads_on_phone,
                contacts_synced_unix_ms: d.contacts_synced,
                calendar_synced_unix_ms: d.cal_synced,
                sms_status_reports: d.caps.sms_status,
                sms_send: d.caps.sms_send,
                sims: d.sims.clone(),
            })
            .collect();
        v.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        v
    }

    pub fn agenda_view(
        &self,
        query: &str,
        day: &str,
        utc_offset_minutes: i32,
    ) -> Result<AgendaView, ErrorBody> {
        self.agenda_view_range(query, day, utc_offset_minutes, 1, 0)
    }

    /// Contacts matching `query` (alphabetical, at most `people_limit`) and the events touching
    /// `days` days from `day`, both within a byte budget so the reply always fits one response.
    pub fn agenda_view_range(
        &self,
        query: &str,
        day: &str,
        utc_offset_minutes: i32,
        days: u32,
        people_limit: usize,
    ) -> Result<AgendaView, ErrorBody> {
        let date = parse_day(day)?;
        let days = days.clamp(1, MAX_VIEW_DAYS);
        let people_limit = match people_limit {
            0 => MAX_PEOPLE_SHOWN,
            n => n.min(MAX_PEOPLE_LIMIT),
        };
        let query: String = query.chars().take(64).collect();
        let now = now_ms();
        let mut g = self.lock();
        // Opening the window refreshes at most every five minutes; no background timer exists.
        for d in g.devices.values_mut() {
            d.retry_stalled_vcards(now);
            if d.connected() {
                if now.saturating_sub(d.contacts_asked_ms) > REFRESH_AFTER_MS {
                    d.ask_contacts(now);
                }
                if now.saturating_sub(d.cal_asked_ms) > REFRESH_AFTER_MS {
                    d.ask_calendar(now, false);
                }
            }
        }
        let devices = Self::devices(&g);
        let mut people = Vec::new();
        let mut people_total = 0;
        let mut people_bytes = 0;
        let mut events = Vec::new();
        let mut events_bytes = 0;
        let mut events_truncated = false;
        let mut calendars = Vec::new();
        let mut edits = Vec::new();
        let mut ids: Vec<&String> = g.devices.keys().collect();
        ids.sort();
        for id in ids {
            let d = &g.devices[id];
            if d.consent[CONTACTS] {
                let mut found = d.agenda.people(&query);
                rank_prefix_first(&mut found, &query);
                people_total += found.len();
                for rec in found {
                    if people.len() >= people_limit || people_bytes > VIEW_PEOPLE_BYTES {
                        break;
                    }
                    if let Some(c) = rec.item() {
                        let p = person(id, rec, c);
                        people_bytes += approx_person_bytes(&p);
                        people.push(p);
                    }
                }
            }
            if d.consent[CALENDAR] {
                calendars.extend(d.cal_info.iter().map(|c| calendar_item(id, c)));
                for rec in d.agenda.range(date, days, utc_offset_minutes) {
                    if events.len() >= MAX_EVENTS_SHOWN || events_bytes > VIEW_EVENTS_BYTES {
                        events_truncated = true;
                        break;
                    }
                    if let Some(e) = rec.item() {
                        let item = event_item(id, rec, e, days > 1);
                        events_bytes += approx_event_bytes(&item);
                        events.push(item);
                    }
                }
            }
            if d.edit_consent {
                edits.extend(d.edits.items());
            }
        }
        events.sort_by_key(|e: &EventItem| (e.start_ms, !e.all_day));
        edits.sort_by_key(|e| std::cmp::Reverse(e.created_unix_ms));
        edits.truncate(bridge_core::phone_view::MAX_EDITS_SHOWN);
        Ok(AgendaView {
            devices,
            people,
            people_total,
            events,
            calendars,
            edits,
            events_truncated,
        })
    }

    pub fn messages_view(
        &self,
        device: Option<&str>,
        thread: Option<i64>,
        query: Option<&str>,
    ) -> Result<MessagesView, ErrorBody> {
        self.messages_view_window(device, thread, query, 0)
    }

    /// Like [`Self::messages_view`], with the newest `message_limit` messages of the selected thread
    /// (0 = `MAX_MESSAGES_SHOWN`), cut to a byte budget so the reply always fits one response.
    pub fn messages_view_window(
        &self,
        device: Option<&str>,
        thread: Option<i64>,
        query: Option<&str>,
        message_limit: usize,
    ) -> Result<MessagesView, ErrorBody> {
        let message_limit = match message_limit {
            0 => MAX_MESSAGES_SHOWN,
            n => n.min(MAX_MESSAGES_LIMIT),
        };
        let needle: Option<String> = query
            .map(|q| q.trim().chars().take(MAX_SEARCH_CHARS).collect::<String>())
            .filter(|q| !q.is_empty())
            .map(|q| q.to_lowercase());
        let now = now_ms();
        let mut g = self.lock();
        for d in g.devices.values_mut() {
            d.tick_sends(now);
            if d.connected()
                && d.consent[SMS]
                && now.saturating_sub(d.sms_asked_ms) > REFRESH_AFTER_MS
            {
                d.ask_sms(now);
            }
        }
        let devices = Self::devices(&g);
        let mut conversations = Vec::new();
        let mut sends = Vec::new();
        let mut ids: Vec<&String> = g.devices.keys().collect();
        ids.sort();
        for id in ids {
            let d = &g.devices[id];
            if !d.consent[SMS] {
                continue;
            }
            let names = name_index(d);
            for t in d.threads.values() {
                let Some(last) = t.conv.newest() else {
                    continue;
                };
                let addrs: Vec<String> = last
                    .addresses
                    .iter()
                    .map(|a| a.as_str().to_owned())
                    .collect();
                let title = addrs
                    .iter()
                    .map(|a| lookup_name(&names, a).unwrap_or_else(|| a.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut matched = None;
                if let Some(q) = &needle {
                    let in_title = title.to_lowercase().contains(q.as_str())
                        || addrs.iter().any(|a| a.to_lowercase().contains(q.as_str()));
                    if !in_title {
                        // Newest matching message gives the snippet.
                        let hit = t
                            .conv
                            .ordered()
                            .into_iter()
                            .rev()
                            .find(|m| m.body.to_lowercase().contains(q.as_str()));
                        match hit {
                            Some(m) => matched = Some(snippet(&m.body, q, 60)),
                            None => continue,
                        }
                    }
                }
                conversations.push(ConversationItem {
                    matched,
                    device_id: id.clone(),
                    thread_id: t.conv.thread_id().0,
                    title: if title.is_empty() {
                        "Unknown sender".into()
                    } else {
                        title
                    },
                    addresses: addrs,
                    last_body: if last.body.trim().is_empty()
                        && (last.key.transport == bridge_core::messages::Transport::Mms
                            || last.attachment_count > 0)
                    {
                        MMS_PLACEHOLDER.to_owned()
                    } else {
                        truncate(&last.body, 160)
                    },
                    last_date_ms: last.date_ms,
                    last_outgoing: last.direction() == Direction::Outgoing,
                    unread: t.unread(),
                    message_count: t.conv.len(),
                });
            }
            for r in d.requests.values().filter_map(|s| d.ledger.get(*s)) {
                sends.push(SendItem {
                    send_id: r.id.0,
                    device_id: id.clone(),
                    recipient: r
                        .intent
                        .recipients()
                        .first()
                        .map(|a: &Address| a.as_str().to_owned())
                        .unwrap_or_default(),
                    text: truncate(r.intent.text(), 160),
                    phase: phase_of(r.state(), r.was_dispatched()),
                    reason: d.send_reasons.get(&r.id).cloned(),
                    created_unix_ms: r.created_ms,
                    updated_unix_ms: r.updated_ms,
                });
            }
        }
        conversations.sort_by_key(|c| std::cmp::Reverse(c.last_date_ms));
        let conversations_total = conversations.len();
        conversations.truncate(MAX_CONVERSATIONS_SHOWN);
        sends.sort_by_key(|s| std::cmp::Reverse(s.created_unix_ms));
        sends.truncate(MAX_SENDS_SHOWN);
        let selected = match (device, thread) {
            (Some(dev), Some(t)) => g.devices.get(dev).filter(|d| d.consent[SMS]).and_then(|d| {
                let th = d.threads.get(&ThreadId(t))?;
                let all = th.conv.ordered();
                let skip = all.len().saturating_sub(message_limit);
                let mut bytes = 0;
                let mut messages: Vec<MessageItem> = all[skip..]
                    .iter()
                    .rev()
                    .map_while(|m| {
                        let item = MessageItem {
                            uid: m.key.uid,
                            mms: m.key.transport == bridge_core::messages::Transport::Mms,
                            outgoing: m.direction() == Direction::Outgoing,
                            body: truncate(&m.body, 4000),
                            date_ms: m.date_ms,
                            attachments: m.attachment_count,
                            sub_id: m.subscription.map(SubscriptionId::get),
                        };
                        bytes += 120 + item.body.len();
                        // The newest message is always included, whatever its size.
                        (bytes <= VIEW_MESSAGES_BYTES || m.key == all[all.len() - 1].key)
                            .then_some(item)
                    })
                    .collect();
                messages.reverse();
                Some(SelectedThread {
                    device_id: dev.to_owned(),
                    thread_id: t,
                    messages,
                    held: all.len(),
                    capped: th.capped,
                    may_have_older: !th.exhausted && !th.capped,
                })
            }),
            _ => None,
        };
        Ok(MessagesView {
            devices,
            conversations,
            conversations_total,
            selected,
            sends,
        })
    }
}

impl Dev {
    fn check_can_send(&mut self, now: u64, parts: usize) -> Result<(), ErrorBody> {
        if !self.consent[SMS] {
            return Err(err(
                ErrorCode::Unsupported,
                "Messages is not allowed for this phone",
            ));
        }
        if !self.connected() {
            return Err(err(
                ErrorCode::NotConnected,
                "the phone is not connected, nothing was sent",
            ));
        }
        if !self.caps.sms_send {
            return Err(err(
                ErrorCode::Unsupported,
                "this phone does not accept texts to send",
            ));
        }
        while self
            .send_times
            .front()
            .is_some_and(|(t, _)| now.saturating_sub(*t) > 60_000)
        {
            self.send_times.pop_front();
        }
        let used: usize = self.send_times.iter().map(|(_, p)| p).sum();
        if self.send_times.len() >= SENDS_PER_MINUTE
            || (used > 0 && used + parts > SEND_PARTS_PER_MINUTE)
        {
            return Err(err(
                ErrorCode::TooMany,
                "too many messages in a minute, wait a moment",
            ));
        }
        Ok(())
    }

    /// Writes the request to the link, then records the irreversible `Dispatched`.
    fn dispatch(
        &mut self,
        id: SendId,
        intent: &OutgoingTextIntent,
        now: u64,
    ) -> Result<(), ErrorBody> {
        let request_id = format!("gb{}-{}", id.0, now);
        if !self.send(sms::REQUEST, sms::send_request_body(intent, &request_id)) {
            let _ = self.ledger.apply(id, SendEvent::LocalAbort, now);
            return Err(err(ErrorCode::Busy, "the link is busy, nothing was sent"));
        }
        let _ = self.ledger.apply(id, SendEvent::Dispatched, now);
        self.requests.insert(request_id, id);
        self.send_times.push_back((now, parts_of(intent.text())));
        let gone = self.ledger.prune_settled(SEND_HISTORY);
        if !gone.is_empty() {
            self.requests.retain(|_, sid| !gone.contains(sid));
            for sid in &gone {
                self.send_reasons.remove(sid);
            }
        }
        Ok(())
    }
}

/// SMS parts a text will most likely use (at least one).
fn parts_of(text: &str) -> usize {
    bridge_core::messages::sms_segments(text).segments.max(1)
}

fn phase_of(state: SendState, dispatched: bool) -> SendPhase {
    match state {
        SendState::Queued if dispatched => SendPhase::HandedToPhone,
        SendState::Queued => SendPhase::Queued,
        SendState::AcceptedByPhone => SendPhase::AcceptedByPhone,
        SendState::Sent => SendPhase::Sent,
        SendState::Delivered => SendPhase::Delivered,
        SendState::Failed { .. } => SendPhase::Failed,
        SendState::Uncertain { .. } => SendPhase::Uncertain,
    }
}

fn parse_day(day: &str) -> Result<Date, ErrorBody> {
    let bad = || err(ErrorCode::BadRequest, "day must be YYYY-MM-DD");
    let mut it = day.split('-');
    let (y, m, d) = (it.next(), it.next(), it.next());
    if it.next().is_some() {
        return Err(bad());
    }
    Date::new(
        y.and_then(|v| v.parse().ok()).ok_or_else(bad)?,
        m.and_then(|v| v.parse().ok()).ok_or_else(bad)?,
        d.and_then(|v| v.parse().ok()).ok_or_else(bad)?,
    )
    .map_err(|_| bad())
}

/// About `max` characters of `body` around the first case-insensitive occurrence of `needle`
/// (already lower case). Falls back to the start of the text.
fn snippet(body: &str, needle: &str, max: usize) -> String {
    let flat: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let lower = flat.to_lowercase();
    let at = lower
        .find(needle)
        .map(|b| lower[..b].chars().count())
        .unwrap_or(0);
    let start = at.saturating_sub(max / 4);
    let text: String = flat.chars().skip(start).take(max).collect();
    format!("{}{text}", if start > 0 { "…" } else { "" })
}

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Digits that identify a number: the last ten, so `+49 151 2345678` and `0151 2345678` are the same
/// number while numbers from different countries that only share a short tail are not.
const KEY_DIGITS: usize = 10;
/// Shorter tail used only to find a contact for a number stored without its country code.
const LOOSE_DIGITS: usize = 7;

/// Whether `s` is written as a phone number (digits and the usual separators), not a sender name.
fn number_like(s: &str) -> bool {
    let t = s.trim();
    t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')' | '+' | '/'))
}

/// Who a text is from, for "one banner per sender": the number's last ten digits; a sender with no real
/// number (a shop's name, `Bank12345`, a short code) stays as written.
fn sender_key(s: &str) -> String {
    let d = digits(s);
    if number_like(s) && d.len() >= 5 {
        tail(&d, KEY_DIGITS)
    } else {
        s.to_owned()
    }
}

fn tail(d: &str, n: usize) -> String {
    d[d.len().saturating_sub(n)..].to_owned()
}

/// Contact names by number: exact (last ten digits) entries, plus a seven-digit entry for contacts stored
/// without a `+` country code. A seven-digit entry shared by two different names is blank (ambiguous).
fn name_index(d: &Dev) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if d.consent[CONTACTS] {
        for (_, c) in d.agenda.contacts.live() {
            if c.display_name.trim().is_empty() {
                continue;
            }
            for p in &c.phones {
                let dg = digits(p);
                if dg.len() < 5 {
                    continue;
                }
                m.entry(tail(&dg, KEY_DIGITS))
                    .or_insert_with(|| c.display_name.clone());
                if dg.len() >= LOOSE_DIGITS && !p.trim_start().starts_with('+') {
                    m.entry(format!("~{}", tail(&dg, LOOSE_DIGITS)))
                        .and_modify(|e| {
                            if *e != c.display_name {
                                e.clear();
                            }
                        })
                        .or_insert_with(|| c.display_name.clone());
                }
            }
        }
    }
    m
}

/// The contact for `address`: an exact number match, else a unique contact stored without a country code
/// whose last seven digits agree. Not for names or codes (`Bank12345`).
fn lookup_name(names: &HashMap<String, String>, address: &str) -> Option<String> {
    let dg = digits(address);
    if !number_like(address) || dg.len() < 5 {
        return None;
    }
    if let Some(n) = names.get(&tail(&dg, KEY_DIGITS)) {
        return Some(n.clone());
    }
    if dg.len() >= LOOSE_DIGITS {
        return names
            .get(&format!("~{}", tail(&dg, LOOSE_DIGITS)))
            .filter(|n| !n.is_empty())
            .cloned();
    }
    None
}

/// Puts contacts whose name starts with the query first (stable, so alphabetical within each group), so a
/// short view limit cannot cut off the best match.
fn rank_prefix_first(found: &mut [&Record<Contact>], query: &str) {
    let q = bridge_core::collate::fold(query.trim());
    if q.is_empty() {
        return;
    }
    // A stable partition, not a sort: a keyed sort instantiates a whole quicksort for this one call (~9 kB).
    let (mut first, rest): (Vec<_>, Vec<_>) = found.iter().partition(|r| {
        r.item()
            .is_some_and(|c| bridge_core::collate::fold(&c.display_name).starts_with(&q))
    });
    first.extend(rest);
    found.copy_from_slice(&first);
}

fn person(device: &str, rec: &Record<Contact>, c: &Contact) -> Person {
    Person {
        device_id: device.to_owned(),
        id: rec.id.id.clone(),
        name: if c.display_name.trim().is_empty() {
            c.phones
                .first()
                .or(c.emails.first())
                .cloned()
                .unwrap_or_default()
        } else {
            c.display_name.clone()
        },
        phones: c.phones.iter().take(8).cloned().collect(),
        emails: c.emails.iter().take(8).cloned().collect(),
        organization: c.organization.clone(),
    }
}

/// Budgets that keep every reply under `ipc::MAX_RESPONSE_BYTES` (256 KiB) with room to spare.
const VIEW_PEOPLE_BYTES: usize = 90 * 1024;
const VIEW_EVENTS_BYTES: usize = 110 * 1024;
const VIEW_MESSAGES_BYTES: usize = 150 * 1024;

fn approx_person_bytes(p: &Person) -> usize {
    160 + p.name.len()
        + p.id.len()
        + p.phones.iter().map(|s| s.len() + 4).sum::<usize>()
        + p.emails.iter().map(|s| s.len() + 4).sum::<usize>()
        + p.organization.as_ref().map_or(0, String::len)
}

fn approx_event_bytes(e: &EventItem) -> usize {
    330 + e.title.len()
        + e.id.len()
        + e.location.as_ref().map_or(0, String::len)
        + e.description.as_ref().map_or(0, String::len)
        + e.attendees.iter().map(|s| s.len() + 4).sum::<usize>()
}

fn calendar_item(device: &str, c: &wire::CalendarInfo) -> CalendarItem {
    CalendarItem {
        device_id: device.to_owned(),
        id: c.id.clone(),
        name: c.name.clone(),
        account: c.account.clone(),
        account_type: c.account_type.clone(),
        color: c.color,
        writable: c.writable,
    }
}

fn event_item(
    device: &str,
    rec: &Record<CalendarEvent>,
    e: &CalendarEvent,
    compact: bool,
) -> EventItem {
    let (all_day, start_ms, end_ms) = match &e.when {
        When::AllDay {
            start,
            end_exclusive,
        } => (
            true,
            start.epoch_day() * 86_400_000,
            end_exclusive.epoch_day() * 86_400_000,
        ),
        When::Timed {
            start_utc_ms,
            end_utc_ms,
            ..
        } => (false, *start_utc_ms, *end_utc_ms),
    };
    EventItem {
        device_id: device.to_owned(),
        id: rec.id.id.clone(),
        title: e.title.clone(),
        all_day,
        start_ms,
        end_ms,
        location: e.location.clone(),
        description: e
            .description
            .as_ref()
            .map(|d| truncate(d, if compact { 200 } else { 600 })),
        calendar_id: e.meta.calendar_id.clone(),
        event_id: e.meta.event_id.clone(),
        recurring: e.meta.recurring || e.recurrence.is_some(),
        tz: match &e.when {
            When::Timed { time_zone, .. } => Some(time_zone.clone()),
            When::AllDay { .. } => None,
        },
        reminders_min: e.meta.reminders_min.clone(),
        attendee_count: e.meta.attendee_count,
        attendees: e.meta.attendees.clone(),
    }
}

/// Best-effort freedesktop notification through `notify-send`: "New message from <sender>" with a
/// Reply action (libnotify >= 0.8; a click on the body does the same). The text is part of it only
/// when the notification carries a body, which `PhoneData` allows only if the user chose to show
/// message content.
pub struct NotifySendSms;

impl NotifySendSms {
    pub fn args(device: &str, sender: &str) -> Vec<String> {
        Self::text_args(&IncomingText {
            device_name: device.to_owned(),
            sender: sender.to_owned(),
            number: String::new(),
            body: None,
        })
    }

    pub fn text_args(n: &IncomingText) -> Vec<String> {
        let clean = |s: &str, max: usize| crate::notify_text::plain(s, max);
        let mut args = vec![
            "--app-name=GeekBridge".to_owned(),
            "--icon=dev.geekbridge.Messages".to_owned(),
            "--expire-time=8000".into(),
        ];
        if !n.number.is_empty() {
            // Same sender, same slot: a new text replaces the earlier banner instead of stacking a pile.
            let tag = sender_key(&n.number)
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(20)
                .collect::<String>();
            args.push("--category=im.received".into());
            if !tag.is_empty() {
                args.push(format!(
                    "--hint=string:x-canonical-private-synchronous:geekbridge-sms-{tag}"
                ));
            }
            args.push("--wait".into());
            args.push("--action=reply=Reply".into());
            args.push("--action=default=Reply".into());
        }
        args.push("--".into());
        args.push(format!("New message from {}", clean(&n.sender, 48)));
        args.push(match &n.body {
            Some(b) => clean(&b.replace('\n', " "), 160),
            None => format!("On {}", clean(&n.device_name, 48)),
        });
        args
    }
}

impl NotifySendSms {
    pub fn event_args(device: &str, title: &str, minutes: u32) -> Vec<String> {
        let clean = |s: &str, n: usize| crate::notify_text::plain(s, n);
        vec![
            "--app-name=GeekBridge".into(),
            "--icon=dev.geekbridge.Agenda".into(),
            "--expire-time=15000".into(),
            "--".into(),
            if minutes == 0 {
                format!("Now: {}", clean(title, 80))
            } else {
                format!("In {minutes} min: {}", clean(title, 80))
            },
            format!("Calendar on {}", clean(device, 48)),
        ]
    }
}

impl SmsNotifier for NotifySendSms {
    fn event_soon(&self, device_name: &str, title: &str, minutes: u32) {
        let args = Self::event_args(device_name, title, minutes);
        std::thread::spawn(move || {
            let _ = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }

    fn received(&self, device_name: &str, sender: &str) {
        self.incoming(&IncomingText {
            device_name: device_name.to_owned(),
            sender: sender.to_owned(),
            number: String::new(),
            body: None,
        });
    }

    fn incoming(&self, n: &IncomingText) {
        let args = Self::text_args(n);
        let number = n.number.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            // "Reply" opens the Messages window on that conversation; the number travels in the
            // environment of the new process, never on a command line.
            if let Ok(out) = out
                && !number.is_empty()
                && matches!(
                    String::from_utf8_lossy(&out.stdout).trim(),
                    "reply" | "default"
                )
            {
                crate::desktop_notify::open_app_with_env(
                    "geekbridge-messages",
                    "GEEKBRIDGE_MESSAGE_TO",
                    &number,
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::phone_view::EditPhase;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }
    fn caps_all() -> Caps {
        Caps {
            contacts: true,
            calendar: true,
            calendar_write: true,
            sms_read: true,
            sms_send: true,
            sms_status: true,
            battery_request: true,
            notification_request: true,
            notification_action: true,
            photos: true,
        }
    }
    const ID: &str = "pppppppppppppppppppppppppppppppp";

    fn up(h: &PhoneData, on: (bool, bool, bool)) -> mpsc::Receiver<Out> {
        h.link_up(ID, "Pixel", caps_all(), on).0
    }
    fn drain(rx: &mut mpsc::Receiver<Out>) -> Vec<&'static str> {
        let mut v = Vec::new();
        while let Ok(o) = rx.try_recv() {
            v.push(o.kind);
        }
        v
    }

    #[test]
    fn nothing_is_requested_without_consent() {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, false, false));
        assert!(drain(&mut rx).is_empty());
        // Packets for a feature that is off are dropped unread, not counted as misbehaviour.
        let m = obj(json!({"version": 2, "messages": []}));
        assert_eq!(h.on_packet(ID, sms::MESSAGES, &m), Disposition::Ignored);
        h.set_enabled(ID, Feature::Sms, true);
        assert_eq!(drain(&mut rx), vec![sms::REQUEST_CONVERSATIONS]);
        h.set_enabled(ID, Feature::Contacts, true);
        h.set_enabled(ID, Feature::Calendar, true);
        assert_eq!(
            drain(&mut rx),
            vec![wire::CONTACTS_REQUEST_UIDS, wire::CALENDAR_REQUEST]
        );
    }

    #[test]
    fn phone_without_the_capability_is_never_asked() {
        let h = PhoneData::new();
        let (mut rx, _) = h.link_up(ID, "Stock", Caps::default(), (true, true, true));
        assert!(drain(&mut rx).is_empty());
        let v = h.agenda_view("", "2026-11-01", 0).unwrap();
        assert_eq!(v.devices[0].contacts, DataState::NotOffered);
        assert_eq!(v.devices[0].calendar, DataState::NotOffered);
    }

    #[test]
    fn contacts_flow_fetches_changes_and_tombstones() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, false, false));
        assert_eq!(drain(&mut rx), vec![wire::CONTACTS_REQUEST_UIDS]);
        let uids = obj(json!({"uids": ["a", "b"], "a": "1", "b": "1"}));
        assert_eq!(
            h.on_packet(ID, wire::CONTACTS_UIDS, &uids),
            Disposition::Accepted
        );
        assert_eq!(drain(&mut rx), vec![wire::CONTACTS_REQUEST_VCARDS]);
        let cards = obj(json!({"uids": ["a", "b"],
            "a": "BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nTEL:+1 555 000 1111\nEND:VCARD\n",
            "b": "BEGIN:VCARD\nVERSION:3.0\nFN:Bob\nEND:VCARD\n"}));
        assert_eq!(
            h.on_packet(ID, wire::CONTACTS_VCARDS, &cards),
            Disposition::Accepted
        );
        let v = h.agenda_view("", "2026-11-01", 0).unwrap();
        assert_eq!(
            v.people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["Ada", "Bob"]
        );
        // vCards nobody asked for are not stored.
        let rogue = obj(json!({"uids": ["zz"], "zz": "BEGIN:VCARD\nFN:Eve\nEND:VCARD\n"}));
        h.on_packet(ID, wire::CONTACTS_VCARDS, &rogue);
        assert_eq!(h.agenda_view("", "2026-11-01", 0).unwrap().people_total, 2);
        // Next listing: a changed (re-fetched), b gone (tombstoned).
        h.refresh_agenda(Some(ID)).unwrap();
        drain(&mut rx);
        h.on_packet(
            ID,
            wire::CONTACTS_UIDS,
            &obj(json!({"uids": ["a"], "a": "2"})),
        );
        assert_eq!(drain(&mut rx), vec![wire::CONTACTS_REQUEST_VCARDS]);
        let v = h.agenda_view("", "2026-11-01", 0).unwrap();
        assert_eq!(v.people_total, 1);
        // An empty listing must not wipe the store.
        h.refresh_agenda(Some(ID)).unwrap();
        h.on_packet(ID, wire::CONTACTS_UIDS, &obj(json!({"uids": []})));
        assert_eq!(h.agenda_view("", "2026-11-01", 0).unwrap().people_total, 1);
        // Consent off erases it.
        h.set_enabled(ID, Feature::Contacts, false);
        assert!(
            h.agenda_view("", "2026-11-01", 0)
                .unwrap()
                .people
                .is_empty()
        );
    }

    #[test]
    fn the_raw_vcard_is_not_kept_in_memory_and_withdrawing_consent_leaves_nothing() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, false, false));
        drain(&mut rx);
        h.on_packet(
            ID,
            wire::CONTACTS_UIDS,
            &obj(json!({"uids": ["a"], "a": "1"})),
        );
        let note = "n".repeat(50_000);
        let card = format!("BEGIN:VCARD\nVERSION:3.0\nFN:Ada\nNOTE:{note}\nEND:VCARD\n");
        h.on_packet(
            ID,
            wire::CONTACTS_VCARDS,
            &obj(json!({"uids": ["a"], "a": card})),
        );
        {
            let g = h.lock();
            let dev = g.devices.get(ID).unwrap();
            let held: Vec<_> = dev.agenda.contacts.live().collect();
            assert_eq!(held.len(), 1);
            assert!(
                held[0].1.vcard.is_none(),
                "5,000 raw cards of up to 64 KiB would be held for nothing"
            );
        }
        h.set_enabled(ID, Feature::Contacts, false);
        let g = h.lock();
        let dev = g.devices.get(ID).unwrap();
        assert!(dev.agenda.contacts.is_empty() && dev.listing.is_empty() && dev.pending.is_empty());
    }

    #[test]
    fn a_phone_that_stops_offering_contacts_and_calendar_takes_its_data_with_it() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, true, false));
        drain(&mut rx);
        h.on_packet(
            ID,
            wire::CONTACTS_UIDS,
            &obj(json!({"uids": ["a"], "a": "1"})),
        );
        h.on_packet(
            ID,
            wire::CONTACTS_VCARDS,
            &obj(json!({"uids": ["a"], "a": "BEGIN:VCARD\nFN:Ada\nEND:VCARD\n"})),
        );
        h.on_packet(
            ID,
            wire::CALENDAR_SYNC,
            &cal_sync(
                json!([cal_ev("5@1", "5", "1", 1_800_000_000_000, false)]),
                "t:1",
                true,
                None,
            ),
        );
        let v = h.agenda_view_range("", "2027-01-15", 0, 42, 0).unwrap();
        assert_eq!((v.people_total, v.events.len()), (1, 1));
        // The phone comes back without the capabilities: the owner turned both off there.
        let (_, generation) = h.link_up(ID, "Pixel", Caps::default(), (true, true, false));
        let v = h.agenda_view_range("", "2027-01-15", 0, 42, 0).unwrap();
        assert_eq!(
            (v.people_total, v.events.len()),
            (0, 0),
            "nothing is shown from a phone that withdrew"
        );
        assert_eq!(v.devices[0].contacts, DataState::NotOffered);
        h.link_down(ID, generation);
        // ...and it is really gone, not just hidden: offering again starts from an empty store.
        let g = h.lock();
        let dev = g.devices.get(ID).unwrap();
        assert!(
            dev.agenda.contacts.is_empty()
                && dev.agenda.events.is_empty()
                && dev.cal_info.is_empty()
        );
    }

    #[test]
    fn an_unanswered_vcard_batch_is_asked_for_again() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, false, false));
        drain(&mut rx);
        let uids = obj(json!({"uids": ["a", "b"], "a": "1", "b": "1"}));
        h.on_packet(ID, wire::CONTACTS_UIDS, &uids);
        assert_eq!(drain(&mut rx), vec![wire::CONTACTS_REQUEST_VCARDS]);
        // Nothing answers; a poll before the timeout does nothing, one after it re-sends the same batch.
        h.lock()
            .devices
            .get_mut(ID)
            .unwrap()
            .retry_stalled_vcards(now_ms());
        assert!(drain(&mut rx).is_empty());
        h.lock()
            .devices
            .get_mut(ID)
            .unwrap()
            .retry_stalled_vcards(now_ms() + VCARD_RETRY_MS + 1);
        let again = rx.try_recv().expect("batch asked again");
        assert_eq!(again.kind, wire::CONTACTS_REQUEST_VCARDS);
        assert_eq!(again.body["uids"], json!(["a", "b"]));
    }

    #[test]
    fn calendar_delta_with_wrong_base_asks_for_a_full_sync() {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, true, false));
        drain(&mut rx);
        let ev = |id: &str| {
            json!({"id": id, "rev": "r", "title": "Standup", "allDay": false,
            "startMs": 1_793_523_600_000_i64, "endMs": 1_793_525_400_000_i64, "tz": "UTC"})
        };
        let full = obj(
            json!({"version": 1, "full": true, "base": null, "token": "t:1",
            "calendars": [{"id": "1", "name": "Work"}], "events": [ev("1@1")], "deleted": []}),
        );
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_SYNC, &full),
            Disposition::Accepted
        );
        let v = h.agenda_view("", "2026-11-01", 0).unwrap();
        assert_eq!(v.devices[0].events_count, 1);
        assert_eq!(v.devices[0].calendar_names, vec!["Work"]);
        let bad = obj(
            json!({"version": 1, "full": false, "base": "t:9", "token": "t:10",
            "calendars": [], "events": [ev("2@1")], "deleted": []}),
        );
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_SYNC, &bad),
            Disposition::Accepted
        );
        let sent = rx.try_recv().expect("full request");
        assert_eq!(sent.kind, wire::CALENDAR_REQUEST);
        assert!(!sent.body.contains_key("since"));
        assert_eq!(
            h.agenda_view("", "2026-11-01", 0).unwrap().devices[0].events_count,
            1
        );
        // Malformed packets count as misbehaviour.
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_SYNC, &obj(json!({"version": 1}))),
            Disposition::Invalid
        );
    }

    fn msg(thread: i64, id: i64, ty: i64, body: &str, date: u64) -> Value {
        json!({"addresses": [{"address": "+15551230000"}], "body": body, "date": date,
               "type": ty, "read": 0, "thread_id": thread, "_id": id, "sub_id": 1})
    }

    #[test]
    fn sms_conversations_send_states_and_notification() {
        struct N(Mutex<Vec<(String, String)>>);
        impl SmsNotifier for N {
            fn received(&self, d: &str, s: &str) {
                self.0.lock().unwrap().push((d.into(), s.into()));
            }
        }
        let n = Arc::new(N(Mutex::new(Vec::new())));
        let h = PhoneData::new();
        h.set_notifier(n.clone());
        let mut rx = up(&h, (false, false, true));
        assert_eq!(drain(&mut rx), vec![sms::REQUEST_CONVERSATIONS]);
        let now = now_ms();
        let first =
            obj(json!({"version": 2, "messages": [msg(4, 1, 1, "old news", now - 3_600_000)]}));
        assert_eq!(
            h.on_packet(ID, sms::MESSAGES, &first),
            Disposition::Accepted
        );
        assert!(
            n.0.lock().unwrap().is_empty(),
            "the first listing never notifies"
        );
        // A new unread inbox message after the listing notifies once, with the sender only.
        let live = obj(json!({"version": 2, "messages": [msg(4, 2, 1, "secret body", now)]}));
        h.on_packet(ID, sms::MESSAGES, &live);
        assert_eq!(
            n.0.lock().unwrap().as_slice(),
            &[("Pixel".to_string(), "+15551230000".to_string())]
        );
        let v = h.messages_view(Some(ID), Some(4), None).unwrap();
        assert_eq!(v.conversations.len(), 1);
        assert_eq!(v.conversations[0].unread, 2);
        assert_eq!(v.selected.as_ref().unwrap().messages.len(), 2);

        // Sending: handed to phone, then the phone's own reports move it forward.
        let id = h
            .send_text(ID, "+15551230000", "on my way", "sub-1", None)
            .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.kind, sms::REQUEST);
        let rid = out.body["requestId"].as_str().unwrap().to_owned();
        assert_eq!(
            h.send_text(ID, "+15551230000", "on my way", "sub-1", None)
                .unwrap(),
            id
        );
        assert!(
            rx.try_recv().is_err(),
            "same submission must not send twice"
        );
        let phase = |h: &PhoneData| h.messages_view(None, None, None).unwrap().sends[0].phase;
        assert_eq!(phase(&h), SendPhase::HandedToPhone);
        for (st, want) in [
            ("accepted", SendPhase::AcceptedByPhone),
            ("sent", SendPhase::Sent),
            ("delivered", SendPhase::Delivered),
        ] {
            let b = obj(json!({"requestId": rid, "state": st}));
            assert_eq!(h.on_packet(ID, wire::SMS_STATUS, &b), Disposition::Accepted);
            assert_eq!(phase(&h), want);
        }
        // A status for a request we never made is dropped.
        let b = obj(json!({"requestId": "nope", "state": "sent"}));
        assert_eq!(h.on_packet(ID, wire::SMS_STATUS, &b), Disposition::Ignored);
    }

    #[test]
    fn a_thread_read_on_the_phone_stops_counting_as_unread() {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, false, true));
        drain(&mut rx);
        let now = now_ms();
        let first = obj(
            json!({"version": 2, "unread": {"4": 2, "5": 1}, "messages": [
            msg(4, 1, 1, "a", now - 3000), msg(4, 2, 1, "b", now - 2000),
            msg(5, 3, 1, "c", now - 1000)]}),
        );
        h.on_packet(ID, sms::MESSAGES, &first);
        assert_eq!(h.unread_total(), 3);
        // The phone read thread 4: it drops out of the map, the older rows stay flagged unread here.
        let second = obj(json!({"version": 2, "unread": {"5": 1}, "messages": [
            msg(5, 3, 1, "c", now - 1000)]}));
        h.on_packet(ID, sms::MESSAGES, &second);
        assert_eq!(h.unread_total(), 1);
        // A push without a map (stock phones, single messages) still counts what is new.
        let push = obj(json!({"version": 2, "messages": [msg(5, 9, 1, "d", now)]}));
        h.on_packet(ID, sms::MESSAGES, &push);
        assert_eq!(h.unread_total(), 2);
    }

    #[test]
    fn unrelated_traffic_does_not_exhaust_load_older() {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, false, true));
        drain(&mut rx);
        let now = now_ms();
        let first = obj(json!({"version": 2, "messages": [msg(4, 1, 1, "a", now - 5000)]}));
        h.on_packet(ID, sms::MESSAGES, &first);
        h.refresh_messages(ID, Some(4)).unwrap();
        assert_eq!(drain(&mut rx), vec![sms::REQUEST_CONVERSATION]);
        // A push for another thread arrives before the page does.
        let other = obj(json!({"version": 2, "messages": [msg(7, 2, 1, "x", now)]}));
        h.on_packet(ID, sms::MESSAGES, &other);
        h.refresh_messages(ID, Some(4)).unwrap();
        assert_eq!(
            drain(&mut rx),
            vec![sms::REQUEST_CONVERSATION],
            "still not exhausted"
        );
        // An empty answer for the thread is the phone saying there is no more.
        let empty = obj(json!({"version": 2, "messages": []}));
        h.on_packet(ID, sms::MESSAGES, &empty);
        h.refresh_messages(ID, Some(4)).unwrap();
        assert!(drain(&mut rx).is_empty());
    }

    #[test]
    fn dropped_link_after_dispatch_is_uncertain_and_never_auto_resent() {
        let h = PhoneData::new();
        let (mut rx, generation) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        drain(&mut rx);
        h.send_text(ID, "5551230000", "hi", "s1", None).unwrap();
        h.link_down(ID, generation);
        let v = h.messages_view(None, None, None).unwrap();
        assert_eq!(v.sends[0].phase, SendPhase::Uncertain);
        // Reconnecting sends nothing by itself, and sending while offline is refused.
        let (mut rx2, _) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        assert_eq!(drain(&mut rx2), vec![sms::REQUEST_CONVERSATIONS]);
        // Explicit resend creates a new record and a new request.
        let new = h.resend(ID, v.sends[0].send_id, "s2").unwrap();
        assert_ne!(new, v.sends[0].send_id);
        assert_eq!(drain(&mut rx2), vec![sms::REQUEST]);
    }

    #[test]
    fn send_book_stays_bounded_over_a_long_lived_link() {
        let h = PhoneData::new();
        let (mut rx, _g) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        drain(&mut rx);
        for i in 0..(SEND_HISTORY * 3) {
            let id = h
                .send_text(ID, "5551230000", "x", &format!("s{i}"), None)
                .unwrap();
            let mut g = h.lock();
            let d = g.devices.get_mut(ID).unwrap();
            d.send_times.clear(); // the per-minute limit is not what is under test
            d.send_reasons.insert(SendId(id), "r".into());
            let _ = d.ledger.apply(SendId(id), SendEvent::PhoneAccepted, 1);
            let _ = d.ledger.apply(SendId(id), SendEvent::PhoneDelivered, 2);
            drop(g);
            drain(&mut rx);
        }
        let g = h.lock();
        let d = g.devices.get(ID).unwrap();
        assert!(d.ledger.len() <= SEND_HISTORY, "ledger {}", d.ledger.len());
        assert!(d.requests.len() <= SEND_HISTORY);
        assert!(d.send_reasons.len() <= SEND_HISTORY);
        drop(g);
        let v = h.messages_view(None, None, None).unwrap();
        assert!(v.sends.len() <= SEND_HISTORY);
    }

    #[test]
    fn send_refusals() {
        let h = PhoneData::new();
        let (mut rx, g) = h.link_up(ID, "Pixel", caps_all(), (false, false, false));
        drain(&mut rx);
        assert_eq!(
            h.send_text(ID, "1", "x", "a", None).unwrap_err().code,
            ErrorCode::Unsupported
        );
        h.set_enabled(ID, Feature::Sms, true);
        drain(&mut rx);
        assert_eq!(
            h.send_text(ID, "", "x", "a", None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            h.send_text(ID, "1", "  ", "a", None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            h.send_text(ID, "1", "x", "bad id", None).unwrap_err().code,
            ErrorCode::BadRequest
        );
        for i in 0..SENDS_PER_MINUTE {
            h.send_text(ID, "5551230000", "x", &format!("s{i}"), None)
                .unwrap();
        }
        assert_eq!(
            h.send_text(ID, "5551230000", "x", "late", None)
                .unwrap_err()
                .code,
            ErrorCode::TooMany
        );
        h.link_down(ID, g);
        assert_eq!(
            h.send_text(ID, "5551230000", "x", "off", None)
                .unwrap_err()
                .code,
            ErrorCode::NotConnected
        );
    }

    #[test]
    fn can_send_text_follows_the_same_gates_as_sending() {
        let h = PhoneData::new();
        assert!(!h.can_send_text(ID), "unknown phone");
        let (mut rx, g) = h.link_up(ID, "Pixel", caps_all(), (false, false, false));
        drain(&mut rx);
        assert!(!h.can_send_text(ID), "Messages not allowed here");
        h.set_enabled(ID, Feature::Sms, true);
        drain(&mut rx);
        assert!(h.can_send_text(ID));
        for i in 0..SENDS_PER_MINUTE {
            h.send_text(ID, "5551230000", "x", &format!("s{i}"), None)
                .unwrap();
        }
        assert!(!h.can_send_text(ID), "send limit reached");
        h.link_down(ID, g);
        assert!(!h.can_send_text(ID), "phone gone");
        // A phone whose Messages switch is off offers no send request.
        let h = PhoneData::new();
        let mut caps = caps_all();
        caps.sms_send = false;
        let (mut rx, _g) = h.link_up(ID, "Pixel", caps, (false, false, true));
        drain(&mut rx);
        assert!(!h.can_send_text(ID), "phone does not take texts");
    }

    #[test]
    fn unpair_forgets_everything() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, true, true));
        drain(&mut rx);
        h.forget(ID);
        assert!(
            h.agenda_view("", "2026-11-01", 0)
                .unwrap()
                .devices
                .is_empty()
        );
        let m = obj(json!({"version": 2, "messages": []}));
        assert_eq!(h.on_packet(ID, sms::MESSAGES, &m), Disposition::Ignored);
    }

    #[test]
    fn day_parsing_is_strict() {
        assert!(parse_day("2026-11-01").is_ok());
        for bad in [
            "",
            "2026-13-01",
            "2026-11",
            "x-1-1",
            "2026-11-01-01",
            "2026-02-30",
        ] {
            assert!(parse_day(bad).is_err(), "{bad}");
        }
    }

    // ------------------------------------------------------------ calendar edits and alerts

    fn cal_sync(events: Value, token: &str, full: bool, base: Option<&str>) -> Map<String, Value> {
        obj(
            json!({"version": 1, "full": full, "base": base, "token": token,
            "calendars": [
                {"id": "1", "name": "Personal", "account": "me@gmail.com", "accountType": "com.google",
                 "color": -16711936, "writable": true},
                {"id": "2", "name": "Holidays", "account": "me@gmail.com", "color": 255, "writable": false}],
            "events": events, "deleted": []}),
        )
    }
    fn cal_ev(id: &str, event_id: &str, cal: &str, start: i64, recurring: bool) -> Value {
        json!({"id": id, "rev": "r1", "title": "Dentist", "allDay": false,
            "startMs": start, "endMs": start + 3_600_000, "tz": "Europe/Berlin",
            "calendarId": cal, "eventId": event_id, "recurring": recurring,
            "reminders": [30, 10], "attendees": {"count": 2, "names": ["Ann", "Bob"]}})
    }
    fn editing_hub(events: Value) -> (Arc<PhoneData>, mpsc::Receiver<Out>) {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, true, false));
        h.set_enabled(ID, Feature::CalendarEdit, true);
        h.refresh_agenda(Some(ID)).unwrap();
        drain(&mut rx);
        assert_eq!(
            h.on_packet(
                ID,
                wire::CALENDAR_SYNC,
                &cal_sync(events, "t:1", true, None)
            ),
            Disposition::Accepted
        );
        (h, rx)
    }
    fn draft() -> EventDraft {
        EventDraft {
            title: "Lunch".into(),
            all_day: false,
            start_ms: 1_800_000_000_000,
            end_ms: 1_800_003_600_000,
            tz: "Europe/Berlin".into(),
            location: None,
            description: None,
            reminder_min: Some(10),
        }
    }

    #[test]
    fn calendar_view_carries_account_colour_reminders_and_attendees() {
        let (h, _rx) = editing_hub(json!([cal_ev("5@1", "5", "1", 1_800_000_000_000, false)]));
        let v = h.agenda_view_range("", "2027-01-15", 0, 42, 0).unwrap();
        assert_eq!(v.calendars.len(), 2);
        assert_eq!(v.calendars[0].account.as_deref(), Some("me@gmail.com"));
        assert_eq!(v.calendars[0].account_type.as_deref(), Some("com.google"));
        assert_eq!(v.calendars[0].color, Some(0xFF00FF00));
        assert!(v.calendars[0].writable && !v.calendars[1].writable);
        let e = &v.events[0];
        assert_eq!(e.calendar_id.as_deref(), Some("1"));
        assert_eq!(e.reminders_min, vec![10, 30]);
        assert_eq!((e.attendee_count, e.attendees.len()), (2, 2));
        assert_eq!(e.tz.as_deref(), Some("Europe/Berlin"));
        assert_eq!(v.devices[0].calendar_edit, DataState::Ready);
    }

    #[test]
    fn nothing_is_written_without_consent_capability_and_a_writable_calendar() {
        // No edit consent at all.
        let h = PhoneData::new();
        let mut rx = up(&h, (false, true, false));
        drain(&mut rx);
        h.on_packet(
            ID,
            wire::CALENDAR_SYNC,
            &cal_sync(json!([]), "t:1", true, None),
        );
        let create = |h: &PhoneData, cal: Option<&str>| {
            h.calendar_edit(
                ID,
                "op1",
                CalendarEditAction::Create,
                cal,
                None,
                Some(&draft()),
            )
        };
        assert_eq!(
            create(&h, Some("1")).unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert!(drain(&mut rx).is_empty());

        // Consent, but no explicit calendar / read-only calendar / unknown calendar.
        h.set_enabled(ID, Feature::CalendarEdit, true);
        assert_eq!(create(&h, None).unwrap_err().code, ErrorCode::BadRequest);
        assert_eq!(
            create(&h, Some("2")).unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            create(&h, Some("9")).unwrap_err().code,
            ErrorCode::BadRequest
        );
        let mut bad = draft();
        bad.end_ms = bad.start_ms;
        assert!(
            h.calendar_edit(
                ID,
                "op2",
                CalendarEditAction::Create,
                Some("1"),
                None,
                Some(&bad)
            )
            .is_err()
        );
        assert!(
            drain(&mut rx).is_empty(),
            "refusals must not touch the link"
        );

        // A phone that does not declare writing is refused too.
        let h2 = PhoneData::new();
        let caps = Caps {
            calendar_write: false,
            ..caps_all()
        };
        let mut rx2 = h2.link_up(ID, "Pixel", caps, (false, true, false)).0;
        h2.set_enabled(ID, Feature::CalendarEdit, true);
        h2.on_packet(
            ID,
            wire::CALENDAR_SYNC,
            &cal_sync(json!([]), "t:1", true, None),
        );
        drain(&mut rx2);
        assert_eq!(
            h2.calendar_edit(
                ID,
                "op",
                CalendarEditAction::Create,
                Some("1"),
                None,
                Some(&draft())
            )
            .unwrap_err()
            .code,
            ErrorCode::Unsupported
        );
        assert!(drain(&mut rx2).is_empty());
    }

    #[test]
    fn create_goes_out_once_and_only_the_phones_answer_applies_it() {
        let (h, mut rx) = editing_hub(json!([]));
        h.calendar_edit(
            ID,
            "op-1",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(&draft()),
        )
        .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.kind, wire::CALENDAR_WRITE);
        assert_eq!(out.body["opId"], "op-1");
        assert_eq!(out.body["calendarId"], "1");
        assert_eq!(out.body["event"]["title"], "Lunch");
        // Same operation id again: refused, not sent twice.
        assert!(
            h.calendar_edit(
                ID,
                "op-1",
                CalendarEditAction::Create,
                Some("1"),
                None,
                Some(&draft())
            )
            .is_err()
        );
        assert!(drain(&mut rx).is_empty());
        let phase = |h: &PhoneData| h.agenda_view("", "2027-01-15", 0).unwrap().edits[0].phase;
        assert_eq!(phase(&h), EditPhase::HandedToPhone);
        // Unknown ids are ignored; the real answer applies it and refreshes the events.
        let stranger = obj(json!({"opId": "nope", "state": "applied"}));
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_WRITE_RESULT, &stranger),
            Disposition::Ignored
        );
        let ok = obj(json!({"opId": "op-1", "state": "applied", "eventId": "77"}));
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_WRITE_RESULT, &ok),
            Disposition::Accepted
        );
        assert_eq!(phase(&h), EditPhase::Applied);
        assert_eq!(drain(&mut rx), vec![wire::CALENDAR_REQUEST]);
    }

    #[test]
    fn a_dropped_link_makes_an_edit_uncertain_and_retry_reuses_the_operation_id() {
        let (h, mut rx) = editing_hub(json!([]));
        h.calendar_edit(
            ID,
            "op-9",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(&draft()),
        )
        .unwrap();
        let first = rx.try_recv().unwrap();
        let (_, gen_) = {
            // Reconnect: the old generation's link_down must not matter; simulate the drop.
            let g = h.lock();
            (0, g.devices[ID].generation)
        };
        h.link_down(ID, gen_);
        let view = h.agenda_view("", "2027-01-15", 0).unwrap();
        assert_eq!(view.edits[0].phase, EditPhase::Uncertain);
        let mut rx2 = h.link_up(ID, "Pixel", caps_all(), (false, true, false)).0;
        h.set_enabled(ID, Feature::CalendarEdit, true);
        drain(&mut rx2);
        h.on_packet(
            ID,
            wire::CALENDAR_SYNC,
            &cal_sync(json!([]), "t:2", true, None),
        );
        h.calendar_edit_retry(ID, "op-9").unwrap();
        let again = rx2.try_recv().unwrap();
        assert_eq!(again.body, first.body, "a retry repeats the exact packet");
        assert!(
            h.calendar_edit_retry(ID, "op-9").is_err(),
            "no double retry while in flight"
        );
    }

    #[test]
    fn repeating_and_read_only_events_cannot_be_edited_or_deleted() {
        let (h, mut rx) = editing_hub(json!([
            cal_ev("5@1", "5", "1", 1_800_000_000_000, true),
            cal_ev("6@1", "6", "2", 1_800_000_000_000, false),
            cal_ev("7@1", "7", "1", 1_800_000_000_000, false),
        ]));
        for ev in ["5", "6", "404"] {
            let r = h.calendar_edit(
                ID,
                &format!("d{ev}"),
                CalendarEditAction::Delete,
                None,
                Some(ev),
                None,
            );
            assert_eq!(r.unwrap_err().code, ErrorCode::BadRequest, "{ev}");
        }
        assert!(drain(&mut rx).is_empty());
        h.calendar_edit(ID, "d7", CalendarEditAction::Delete, None, Some("7"), None)
            .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.body["action"], "delete");
        assert_eq!(out.body["eventId"], "7");
        assert!(out.body.get("event").is_none());
    }

    #[test]
    fn an_edit_names_the_version_it_was_made_from_and_a_refusal_refreshes_the_view() {
        let (h, mut rx) = editing_hub(json!([cal_ev("7@1", "7", "1", 1_800_000_000_000, false)]));
        drain(&mut rx);
        h.calendar_edit(
            ID,
            "u7",
            CalendarEditAction::Update,
            None,
            Some("7"),
            Some(&draft()),
        )
        .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.body["baseRev"], "r1", "the revision the desktop holds");
        h.calendar_edit(ID, "d7", CalendarEditAction::Delete, None, Some("7"), None)
            .unwrap();
        assert_eq!(rx.try_recv().unwrap().body["baseRev"], "r1");
        h.calendar_edit(
            ID,
            "c1",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(&draft()),
        )
        .unwrap();
        assert!(rx.try_recv().unwrap().body.get("baseRev").is_none());
        // The phone says its copy changed: the edit failed with that reason, and a delta is requested.
        drain(&mut rx);
        let res = obj(json!({"opId": "u7", "state": "failed", "reason": "changed"}));
        assert_eq!(
            h.on_packet(ID, wire::CALENDAR_WRITE_RESULT, &res),
            Disposition::Accepted
        );
        assert_eq!(drain(&mut rx), vec![wire::CALENDAR_REQUEST]);
        let v = h.agenda_view("", "2027-01-15", 0).unwrap();
        let e = v.edits.iter().find(|e| e.op_id == "u7").unwrap();
        assert_eq!(e.reason.as_deref(), Some("changed"));
        assert_eq!(e.phase, bridge_core::phone_view::EditPhase::Failed);
    }

    #[test]
    fn turning_edit_consent_off_forgets_the_ledger() {
        let (h, _rx) = editing_hub(json!([]));
        h.calendar_edit(
            ID,
            "op",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(&draft()),
        )
        .unwrap();
        h.set_enabled(ID, Feature::CalendarEdit, false);
        assert!(h.agenda_view("", "2027-01-15", 0).unwrap().edits.is_empty());
        assert_eq!(
            h.calendar_edit(
                ID,
                "o2",
                CalendarEditAction::Create,
                Some("1"),
                None,
                Some(&draft())
            )
            .unwrap_err()
            .code,
            ErrorCode::Unsupported
        );
    }

    #[test]
    fn upcoming_events_are_announced_once_and_only_with_alert_consent() {
        struct Rec(Mutex<Vec<(String, u32)>>);
        impl SmsNotifier for Rec {
            fn received(&self, _: &str, _: &str) {}
            fn event_soon(&self, _: &str, title: &str, minutes: u32) {
                self.0.lock().unwrap().push((title.into(), minutes));
            }
        }
        let rec = Arc::new(Rec(Mutex::new(Vec::new())));
        let soon = now_ms() as i64 + 20 * 60_000; // inside the 30 minute reminder, outside 10
        let (h, _rx) = editing_hub(json!([
            cal_ev("5@1", "5", "1", soon, false),
            cal_ev("6@1", "6", "1", now_ms() as i64 + 5 * 3_600_000, false),
        ]));
        h.set_notifier(rec.clone());
        h.tick_alerts();
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "alerts are off by default"
        );
        h.set_enabled(ID, Feature::CalendarAlerts, true);
        h.tick_alerts();
        h.tick_alerts();
        let got = rec.0.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "Dentist");
        assert!((19..=20).contains(&got[0].1));
    }

    #[test]
    fn notification_text_has_no_markup_or_direction_overrides() {
        // Sender and title are chosen by the other side (S2-3 in docs/reports/security-review-2.md).
        let a = NotifySendSms::args("Pixel", "<a href=\"http://evil\">Bank</a>\u{202E}");
        let e = NotifySendSms::event_args("Pi<b>xel", "<img src=x>Dentist", 5);
        for s in a.iter().chain(e.iter()).filter(|s| !s.starts_with("--")) {
            assert!(!s.contains(['<', '>', '&', '\u{202E}']), "{s}");
        }
    }

    #[test]
    fn event_notification_text_has_no_control_characters() {
        let a = NotifySendSms::event_args("Pi\nxel", "Den\ttist", 5);
        assert!(a.iter().all(|s| !s.contains('\n') && !s.contains('\t')));
        assert!(a[4].starts_with("In 5 min"));
        assert!(NotifySendSms::event_args("p", "t", 0)[4].starts_with("Now"));
    }

    // ------------------------------------------------------------ Messages experience

    fn sms_hub() -> (Arc<PhoneData>, mpsc::Receiver<Out>) {
        let h = PhoneData::new();
        let mut rx = up(&h, (false, false, true));
        drain(&mut rx);
        (h, rx)
    }

    fn feed(h: &PhoneData, list: Vec<Value>) {
        let b = obj(json!({"version": 2, "messages": list}));
        assert_eq!(h.on_packet(ID, sms::MESSAGES, &b), Disposition::Accepted);
    }

    #[test]
    fn marking_read_is_local_and_new_messages_stay_unread() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        feed(
            &h,
            vec![msg(4, 1, 1, "a", now - 5000), msg(4, 2, 1, "b", now - 4000)],
        );
        let unread =
            |h: &PhoneData| h.messages_view(None, None, None).unwrap().conversations[0].unread;
        assert_eq!(unread(&h), 2);
        h.mark_read(ID, 4).unwrap();
        assert_eq!(unread(&h), 0);
        feed(&h, vec![msg(4, 3, 1, "c", now)]);
        assert_eq!(unread(&h), 1, "only what arrived after the mark");
        assert_eq!(h.mark_read(ID, 99).unwrap_err().code, ErrorCode::NotFound);
    }

    #[test]
    fn search_filters_by_number_and_body_and_gives_a_snippet() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        let other = json!({"addresses": [{"address": "+4915112345678"}], "body": "Lunch at noon?",
            "date": now - 1000, "type": 1, "read": 1, "thread_id": 5, "_id": 1});
        feed(
            &h,
            vec![
                msg(4, 1, 1, "your parcel arrives TOMORROW", now - 2000),
                other,
            ],
        );
        let find = |q: &str| h.messages_view(None, None, Some(q)).unwrap().conversations;
        assert_eq!(find("").len(), 2);
        let by_body = find("tomorrow");
        assert_eq!(by_body.len(), 1);
        assert_eq!(by_body[0].thread_id, 4);
        assert!(by_body[0].matched.as_deref().unwrap().contains("TOMORROW"));
        let by_number = find("4915");
        assert_eq!((by_number.len(), by_number[0].thread_id), (1, 5));
        assert!(
            by_number[0].matched.is_none(),
            "title match needs no snippet"
        );
        assert!(find("zzz").is_empty());
    }

    #[test]
    fn a_picture_message_is_shown_as_a_placeholder_not_as_an_empty_line() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        let mms = json!({"addresses": [{"address": "+4915112345678"}], "body": "",
            "date": now - 1000, "type": 1, "read": 0, "thread_id": 5, "_id": 7, "attachments": [{}]});
        // The same _id in the SMS table is a different message: both are kept.
        let sms7 = json!({"addresses": [{"address": "+4915112345678"}], "body": "earlier",
            "date": now - 5000, "type": 1, "read": 1, "thread_id": 5, "_id": 7});
        feed(&h, vec![sms7, mms]);
        let v = h.messages_view(Some(ID), Some(5), None).unwrap();
        assert_eq!(v.conversations[0].last_body, MMS_PLACEHOLDER);
        assert_eq!(v.conversations[0].message_count, 2);
        let sel = v.selected.unwrap();
        assert_eq!(sel.messages.len(), 2);
        assert!(sel.messages[1].mms && sel.messages[1].body.is_empty());
        // A picture with a caption keeps the caption in the list.
        let cap = json!({"addresses": [{"address": "+4915112345678"}], "body": "look!",
            "date": now, "type": 1, "read": 0, "thread_id": 5, "_id": 8, "attachments": [{}]});
        feed(&h, vec![cap]);
        let v = h.messages_view(None, None, None).unwrap();
        assert_eq!(v.conversations[0].last_body, "look!");
    }

    #[test]
    fn a_text_from_a_new_person_is_not_lost_when_the_conversation_list_is_full() {
        let (h, _rx) = sms_hub();
        let base = now_ms() - 10_000_000;
        let first: Vec<Value> = (0..MAX_THREADS as i64)
            .map(|t| msg(t + 1, 1, 1, "hi", base + t as u64))
            .collect();
        for chunk in first.chunks(100) {
            feed(&h, chunk.to_vec());
        }
        assert_eq!(
            h.messages_view(None, None, None)
                .unwrap()
                .conversations_total,
            MAX_THREADS
        );
        // Older than everything held: still left out. Newer: takes the place of the stalest conversation.
        feed(&h, vec![msg(9000, 1, 1, "ancient", base - 5)]);
        feed(&h, vec![msg(9001, 1, 1, "fresh", base + 5_000_000)]);
        let v = h.messages_view(None, None, None).unwrap();
        assert_eq!(v.conversations_total, MAX_THREADS);
        assert!(v.conversations.iter().any(|c| c.thread_id == 9001));
        assert!(!v.conversations.iter().any(|c| c.thread_id == 9000));
        assert!(
            !v.conversations.iter().any(|c| c.thread_id == 1),
            "the stalest one made room"
        );
    }

    #[test]
    fn a_long_thread_is_capped_and_keeps_the_newest_messages() {
        let (h, _rx) = sms_hub();
        let base = 1_700_000_000_000_u64;
        let total = MAX_MESSAGES_PER_THREAD as u64 + 150;
        for chunk in (0..total).collect::<Vec<_>>().chunks(400) {
            feed(
                &h,
                chunk
                    .iter()
                    .map(|i| msg(4, *i as i64 + 1, 1, "x", base + i))
                    .collect(),
            );
        }
        let v = h.messages_view(Some(ID), Some(4), None).unwrap();
        let sel = v.selected.unwrap();
        assert_eq!(sel.held, MAX_MESSAGES_PER_THREAD);
        assert!(sel.capped);
        assert!(
            !sel.may_have_older,
            "no point asking for what cannot be kept"
        );
        assert_eq!(sel.messages.len(), MAX_MESSAGES_SHOWN);
        assert_eq!(sel.messages.last().unwrap().uid, total as i64);
        assert_eq!(v.conversations[0].message_count, MAX_MESSAGES_PER_THREAD);
    }

    #[test]
    fn out_of_order_pages_and_repeats_leave_one_ordered_copy() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        feed(
            &h,
            vec![
                msg(4, 3, 1, "third", now - 1000),
                msg(4, 1, 1, "first", now - 3000),
            ],
        );
        feed(
            &h,
            vec![
                msg(4, 2, 1, "second", now - 2000),
                msg(4, 3, 1, "third", now - 1000),
            ],
        );
        let sel = h
            .messages_view(Some(ID), Some(4), None)
            .unwrap()
            .selected
            .unwrap();
        let bodies: Vec<_> = sel.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, ["first", "second", "third"]);
    }

    #[test]
    fn only_a_sim_the_phone_reported_can_be_chosen() {
        let (h, mut rx) = sms_hub();
        let now = now_ms();
        let b = obj(json!({"version": 2, "messages": [msg(4, 1, 1, "hi", now)],
            "sims": [{"id": 3, "slot": 1, "label": "Home"}, {"id": 5, "slot": 2, "label": "Work"}]}));
        h.on_packet(ID, sms::MESSAGES, &b);
        let v = h.messages_view(None, None, None).unwrap();
        assert_eq!(v.devices[0].sims.len(), 2);
        assert!(v.devices[0].sms_send);
        assert_eq!(
            h.send_text(ID, "5551230000", "x", "a", Some(9))
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        assert!(rx.try_recv().is_err(), "nothing leaves for an unknown SIM");
        h.send_text(ID, "5551230000", "x", "b", Some(5)).unwrap();
        assert_eq!(rx.try_recv().unwrap().body["subID"], 5);
        h.send_text(ID, "5551230000", "y", "c", None).unwrap();
        assert!(!rx.try_recv().unwrap().body.contains_key("subID"));
    }

    #[test]
    fn notification_hides_the_text_unless_the_user_chose_to_show_it() {
        struct N(Mutex<Vec<IncomingText>>);
        impl SmsNotifier for N {
            fn received(&self, _: &str, _: &str) {}
            fn incoming(&self, n: &IncomingText) {
                self.0.lock().unwrap().push(n.clone());
            }
        }
        for show in [false, true] {
            let n = Arc::new(N(Mutex::new(Vec::new())));
            let h = PhoneData::new();
            h.set_notifier(n.clone());
            h.set_notify_content(show);
            let mut rx = up(&h, (false, false, true));
            drain(&mut rx);
            let now = now_ms();
            feed(&h, vec![msg(4, 1, 1, "old", now - 3_600_000)]);
            feed(&h, vec![msg(4, 2, 1, "my pin is 1234", now)]);
            let got = n.0.lock().unwrap().clone();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].number, "+15551230000");
            assert_eq!(got[0].body.as_deref(), show.then_some("my pin is 1234"));
        }
        let args = NotifySendSms::text_args(&IncomingText {
            device_name: "Pixel".into(),
            sender: "Ann".into(),
            number: "+1555".into(),
            body: None,
        });
        assert!(args.iter().any(|a| a == "--action=reply=Reply"));
        assert!(!args.join(" ").contains("pin"));
    }

    #[test]
    fn a_second_sender_in_the_same_burst_is_still_announced() {
        struct N(Mutex<Vec<String>>);
        impl SmsNotifier for N {
            fn received(&self, _: &str, _: &str) {}
            fn incoming(&self, n: &IncomingText) {
                self.0.lock().unwrap().push(n.number.clone());
            }
        }
        let n = Arc::new(N(Mutex::new(Vec::new())));
        let h = PhoneData::new();
        h.set_notifier(n.clone());
        let mut rx = up(&h, (false, false, true));
        drain(&mut rx);
        let now = now_ms();
        feed(&h, vec![msg(4, 1, 1, "old", now - 3_600_000)]);
        let from = |thread: i64, id: i64, addr: &str| {
            let mut m = msg(thread, id, 1, "hi", now);
            m["addresses"] = json!([{"address": addr}]);
            m
        };
        feed(
            &h,
            vec![
                from(4, 2, "+15551230000"),
                from(4, 3, "+15551230000"),
                from(5, 4, "+15559990000"),
            ],
        );
        assert_eq!(
            n.0.lock().unwrap().as_slice(),
            &["+15551230000".to_string(), "+15559990000".to_string()],
            "one banner per sender, none lost"
        );
        // Both are inside their gap now: a repeat is quiet.
        feed(
            &h,
            vec![from(4, 5, "+15551230000"), from(5, 6, "+15559990000")],
        );
        assert_eq!(n.0.lock().unwrap().len(), 2);
    }

    #[test]
    fn one_person_written_two_ways_is_one_banner_and_text_senders_stay_apart() {
        struct N(Mutex<Vec<String>>);
        impl SmsNotifier for N {
            fn received(&self, _: &str, _: &str) {}
            fn incoming(&self, n: &IncomingText) {
                self.0.lock().unwrap().push(n.number.clone());
            }
        }
        let n = Arc::new(N(Mutex::new(Vec::new())));
        let h = PhoneData::new();
        h.set_notifier(n.clone());
        let mut rx = up(&h, (false, false, true));
        drain(&mut rx);
        let now = now_ms();
        feed(&h, vec![msg(4, 1, 1, "old", now - 3_600_000)]);
        let from = |thread: i64, id: i64, addr: &str| {
            let mut m = msg(thread, id, 1, "hi", now);
            m["addresses"] = json!([{"address": addr}]);
            m
        };
        feed(
            &h,
            vec![
                from(4, 2, "+49 151 2345678"),
                from(4, 3, "0151 2345678"),
                from(5, 4, "AMAZON"),
                from(6, 5, "DHL"),
            ],
        );
        assert_eq!(
            n.0.lock().unwrap().as_slice(),
            &[
                "+49 151 2345678".to_string(),
                "AMAZON".to_string(),
                "DHL".to_string()
            ],
            "the same number in two notations is one sender; named senders are not merged"
        );
        let slot = |number: &str| {
            NotifySendSms::text_args(&IncomingText {
                device_name: "Pixel".into(),
                sender: "Ann".into(),
                number: number.into(),
                body: None,
            })
            .into_iter()
            .find(|a| a.contains("synchronous"))
        };
        assert_eq!(
            slot("+49 151 2345678"),
            slot("0151 2345678"),
            "one banner slot per person"
        );
        assert_ne!(slot("+49 151 2345678"), slot("+49 151 2345679"));
    }

    #[test]
    fn texts_the_phone_lists_again_after_a_reconnect_are_not_announced_twice() {
        struct N(Mutex<Vec<String>>);
        impl SmsNotifier for N {
            fn received(&self, _: &str, _: &str) {}
            fn incoming(&self, n: &IncomingText) {
                self.0.lock().unwrap().push(n.number.clone());
            }
        }
        let n = Arc::new(N(Mutex::new(Vec::new())));
        let h = PhoneData::new();
        h.set_notifier(n.clone());
        let (mut rx, generation) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        drain(&mut rx);
        let now = now_ms();
        feed(&h, vec![msg(4, 1, 1, "old", now - 3_600_000)]);
        let fresh = msg(4, 2, 1, "new", now);
        feed(&h, vec![fresh.clone()]);
        assert_eq!(n.0.lock().unwrap().len(), 1);
        h.link_down(ID, generation);
        let (mut rx2, _) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        drain(&mut rx2);
        // The phone's whole list again, the same unread text in it.
        feed(&h, vec![msg(4, 1, 1, "old", now - 3_600_000), fresh]);
        assert_eq!(n.0.lock().unwrap().len(), 1, "same text, same banner");
    }

    #[test]
    fn banner_replaces_the_previous_one_from_the_same_sender() {
        let a = NotifySendSms::text_args(&IncomingText {
            device_name: "Pixel".into(),
            sender: "Ann".into(),
            number: "+1 (555) 123-0000".into(),
            body: None,
        });
        let hint = "--hint=string:x-canonical-private-synchronous:geekbridge-sms-5551230000";
        assert!(a.iter().any(|x| x == hint), "{a:?}");
        assert!(a.iter().any(|x| x == "--category=im.received"));
        // Options stay before the `--` separator, so sender text can never be read as one.
        let sep = a.iter().position(|x| x == "--").unwrap();
        assert!(a.iter().position(|x| x == hint).unwrap() < sep);
        // No number, no reply, no slot.
        let b = NotifySendSms::args("Pixel", "Ann");
        assert!(!b.iter().any(|x| x.contains("synchronous")));
    }

    #[test]
    fn the_phones_unread_count_beats_what_is_loaded_and_a_local_mark_sticks() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        // The list carries one message per thread, but the phone says 5 are unread.
        let b = obj(
            json!({"version": 2, "messages": [msg(4, 9, 1, "newest", now - 1000)],
            "unread": {"4": 5}}),
        );
        h.on_packet(ID, sms::MESSAGES, &b);
        let unread =
            |h: &PhoneData| h.messages_view(None, None, None).unwrap().conversations[0].unread;
        assert_eq!(unread(&h), 5);
        h.mark_read(ID, 4).unwrap();
        assert_eq!(unread(&h), 0);
        // The next refresh repeats the phone's (unchanged) count: still read here.
        h.on_packet(ID, sms::MESSAGES, &b);
        assert_eq!(unread(&h), 0);
        // Two more arrive on the phone: only those two show.
        let b2 = obj(
            json!({"version": 2, "messages": [msg(4, 11, 1, "again", now)], "unread": {"4": 7}}),
        );
        h.on_packet(ID, sms::MESSAGES, &b2);
        assert_eq!(unread(&h), 2);
    }

    #[test]
    fn reading_on_the_phone_lowers_the_mark_so_later_texts_are_still_counted() {
        let (h, _rx) = sms_hub();
        let now = now_ms();
        let list =
            |m: Value, n: u32| obj(json!({"version": 2, "messages": [m], "unread": {"4": n}}));
        let unread =
            |h: &PhoneData| h.messages_view(None, None, None).unwrap().conversations[0].unread;
        h.on_packet(ID, sms::MESSAGES, &list(msg(4, 9, 1, "a", now - 3000), 5));
        h.mark_read(ID, 4).unwrap();
        // The user then reads the thread on the phone: its count falls to 0 ...
        h.on_packet(ID, sms::MESSAGES, &list(msg(4, 9, 1, "a", now - 3000), 0));
        // ... and two new texts arrive (the list carries only the newest of the thread).
        h.on_packet(ID, sms::MESSAGES, &list(msg(4, 11, 1, "b", now), 2));
        assert_eq!(unread(&h), 2, "the stale mark of 5 must not hide them");
    }

    // ------------------------------------------------------------ Messages review 2 follow-up

    fn give_contacts(h: &PhoneData, cards: &[(&str, &str)]) {
        let uids: Vec<&str> = cards.iter().map(|c| c.0).collect();
        let mut listing = serde_json::Map::new();
        listing.insert("uids".into(), json!(uids));
        let mut vcards = serde_json::Map::new();
        vcards.insert("uids".into(), json!(uids));
        for (uid, card) in cards {
            listing.insert((*uid).into(), json!("1"));
            vcards.insert((*uid).into(), json!(card));
        }
        h.on_packet(ID, wire::CONTACTS_UIDS, &listing);
        h.on_packet(ID, wire::CONTACTS_VCARDS, &vcards);
    }
    fn card(name: &str, tel: &str) -> String {
        format!("BEGIN:VCARD\nVERSION:3.0\nFN:{name}\nTEL:{tel}\nEND:VCARD\n")
    }

    #[test]
    fn senders_with_a_shared_short_tail_are_different_senders() {
        assert_eq!(sender_key("+49 151 2345678"), sender_key("0151 2345678"));
        assert_eq!(sender_key("+1 (555) 123-4567"), sender_key("5551234567"));
        assert_ne!(
            sender_key("+44 20 7946 0001"),
            sender_key("+1 555 946 0001"),
            "same last seven digits, different countries"
        );
        assert_ne!(sender_key("Bank12345"), sender_key("12345"));
        assert_eq!(sender_key("Bank12345"), "Bank12345");
    }

    #[test]
    fn contact_names_need_an_exact_number_or_one_unambiguous_local_match() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, false, false));
        drain(&mut rx);
        give_contacts(
            &h,
            &[
                ("a", &card("Ann", "+44 20 7946 0001")),
                ("b", &card("Local Lou", "946 7777")),
                ("c", &card("Dup One", "555 1234")),
                ("d", &card("Dup Two", "(777) 555 1234")),
            ],
        );
        let g = h.lock();
        let names = name_index(g.devices.get(ID).unwrap());
        assert_eq!(lookup_name(&names, "020 7946 0001").as_deref(), Some("Ann"));
        assert_eq!(
            lookup_name(&names, "+1 555 946 0001"),
            None,
            "other country"
        );
        assert_eq!(
            lookup_name(&names, "Bank9460001"),
            None,
            "a name, not a number"
        );
        assert_eq!(
            lookup_name(&names, "+1 555 946 7777").as_deref(),
            Some("Local Lou"),
            "a contact stored without a country code, one candidate"
        );
        assert_eq!(
            lookup_name(&names, "+1 000 555 1234"),
            None,
            "two candidates"
        );
    }

    #[test]
    fn a_prefix_match_is_not_cut_off_by_the_view_limit() {
        let h = PhoneData::new();
        let mut rx = up(&h, (true, false, false));
        drain(&mut rx);
        let mut cards: Vec<(String, String)> = (0..20)
            .map(|i| (format!("a{i}"), card(&format!("Alicia{i:02}"), "555 0100")))
            .collect();
        cards.push(("z".into(), card("Liam", "555 0199")));
        let refs: Vec<(&str, &str)> = cards
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        give_contacts(&h, &refs);
        let v = h.agenda_view_range("li", "2026-11-05", 0, 1, 3).unwrap();
        let names: Vec<_> = v.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "Liam", "{names:?}");
    }

    #[test]
    fn the_send_limit_counts_sms_parts() {
        let (h, _rx) = sms_hub();
        let long = "\u{1F600}".repeat(1000); // 2000 UTF-16 units: about 30 parts
        assert!((25..=40).contains(&parts_of(&long)), "{}", parts_of(&long));
        assert_eq!(parts_of("x"), 1);
        h.send_text(ID, "5551230000", &long, "l1", None).unwrap();
        assert_eq!(
            h.send_text(ID, "5551230000", &long, "l2", None)
                .unwrap_err()
                .code,
            ErrorCode::TooMany,
            "a second 30-part text in the minute"
        );
        h.send_text(ID, "5551230000", "short", "s1", None).unwrap();
    }

    #[test]
    fn losing_the_sms_permission_on_the_phone_takes_the_conversations_with_it() {
        let (h, _rx) = sms_hub();
        feed(&h, vec![msg(4, 1, 1, "hello", now_ms() - 1000)]);
        assert_eq!(h.unread_total(), 1);
        let mut caps = caps_all();
        caps.sms_read = false;
        h.link_up(ID, "Pixel", caps, (false, false, true));
        assert_eq!(h.unread_total(), 0);
        assert!(h.lock().devices.get(ID).unwrap().threads.is_empty());
    }

    #[test]
    fn eviction_drops_the_oldest_messages_not_the_biggest_threads() {
        let (h, _rx) = sms_hub();
        let base = now_ms() - 1_000_000;
        // Thread 1 is small and old; thread 2 is big and newer.
        for chunk in 0..3 {
            let list: Vec<Value> = (0..50)
                .map(|i| {
                    let n = chunk * 50 + i;
                    msg(2, 1000 + n, 1, "big", base + 10_000 + n as u64)
                })
                .collect();
            feed(&h, list);
        }
        feed(
            &h,
            (0..60)
                .map(|i| msg(1, i, 1, "old", base + i as u64))
                .collect(),
        );
        let freed = h.lock().devices.get_mut(ID).unwrap().evict_for_room();
        assert_eq!(freed, 100, "a batch of 100");
        let g = h.lock();
        let d = g.devices.get(ID).unwrap();
        let len = |t: i64| d.threads.get(&ThreadId(t)).map_or(0, |t| t.conv.len());
        assert_eq!(
            len(1),
            1,
            "the old thread is trimmed first and never emptied"
        );
        assert_eq!(len(2), 150 - 41, "then the oldest of the big one");
    }

    /// Fills the phone to the memory cap straight into the store (packets are rate-limited).
    fn fill_to_cap(h: &PhoneData, base: u64) {
        let mut g = h.lock();
        let d = g.devices.get_mut(ID).unwrap();
        let per = MAX_MESSAGES_TOTAL / 10;
        for t in 0..10_i64 {
            let th = d
                .threads
                .entry(ThreadId(10 + t))
                .or_insert_with(|| Thread::new(ThreadId(10 + t)));
            for i in 0..per as i64 {
                let one = msg(10 + t, t * 100_000 + i, 1, "x", base + i as u64);
                let body = obj(json!({"version": 2, "messages": [one]}));
                for m in sms::parse_messages(&body).unwrap().messages {
                    th.conv.merge(m).unwrap();
                }
            }
        }
    }

    #[test]
    fn a_changed_message_at_the_memory_cap_evicts_nothing_and_a_new_thread_leaves_no_ghost() {
        let (h, _rx) = sms_hub();
        let base = now_ms() - 10_000_000;
        fill_to_cap(&h, base);
        let held = |h: &PhoneData| -> usize {
            let g = h.lock();
            g.devices
                .get(ID)
                .unwrap()
                .threads
                .values()
                .map(|t| t.conv.len())
                .sum()
        };
        assert_eq!(held(&h), MAX_MESSAGES_TOTAL);
        // The phone re-sends an existing message with changed text: replaced in place.
        feed(&h, vec![msg(10, 0, 1, "edited", base)]);
        assert_eq!(held(&h), MAX_MESSAGES_TOTAL, "an update needs no room");
        // A new message in a new conversation makes room by dropping history, and the thread has a row.
        feed(&h, vec![msg(999, 7, 1, "hello", now_ms() - 1000)]);
        let g = h.lock();
        let d = g.devices.get(ID).unwrap();
        assert!(
            d.threads.values().all(|t| !t.conv.is_empty()),
            "no empty thread"
        );
        assert_eq!(d.threads.get(&ThreadId(999)).map(|t| t.conv.len()), Some(1));
        assert!(d.threads.values().map(|t| t.conv.len()).sum::<usize>() <= MAX_MESSAGES_TOTAL);
    }
}
