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
use bridge_core::{
    agenda::{Agenda, CalendarEvent, Contact, Date, Record, SourceId, Store, When},
    ipc::{ErrorBody, ErrorCode, Feature},
    messages::{
        Address, Conversation, Direction, MergeOutcome, OutgoingTextIntent, ResendConsent,
        SendEvent, SendId, SendLedger, SendState, SimSelection, SubmissionId, Submit, ThreadId,
    },
    phone_link::{self as wire, SmsPhase},
    phone_view::{
        AgendaView, ConversationItem, DataDevice, DataState, EventItem, MAX_CONVERSATIONS_SHOWN,
        MAX_EVENTS_SHOWN, MAX_MESSAGES_SHOWN, MAX_PEOPLE_SHOWN, MAX_SENDS_SHOWN, MessageItem,
        MessagesView, Person, SelectedThread, SendItem, SendPhase,
    },
};
use bridge_kde::sms;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

/// A packet the hub wants written to a phone's link.
pub struct Out {
    pub kind: &'static str,
    pub body: Map<String, Value>,
}

/// Desktop notification for a newly arrived text. Only the sender is passed on: message bodies
/// never reach a notifier.
pub trait SmsNotifier: Send + Sync {
    fn received(&self, device_name: &str, sender: &str);
}

/// What the link negotiated, for one phone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    pub contacts: bool,
    pub calendar: bool,
    pub sms_read: bool,
    pub sms_send: bool,
    pub sms_status: bool,
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
            sms_read: out(sms::REQUEST_CONVERSATIONS)
                && out(sms::REQUEST_CONVERSATION)
                && inc(sms::MESSAGES),
            sms_send: out(sms::REQUEST),
            sms_status: inc(wire::SMS_STATUS),
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
const MAX_MESSAGES_TOTAL: usize = 20_000;
const MAX_PENDING_VCARDS: usize = 5000;
const REFRESH_AFTER_MS: u64 = 5 * 60 * 1000;
const ACK_TIMEOUT_MS: u64 = 60_000;
const SENDS_PER_MINUTE: usize = 10;
const NOTIFY_MAX_AGE_MS: u64 = 10 * 60 * 1000;
const NOTIFY_MIN_GAP_MS: u64 = 2_000;
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
    contacts_synced: Option<u64>,
    contacts_asked_ms: u64,

    calendar_requested: bool,
    cal_token: Option<String>,
    cal_names: Vec<String>,
    cal_truncated: bool,
    cal_synced: Option<u64>,
    cal_asked_ms: u64,
    cal_request_seq: u64,

    threads: BTreeMap<ThreadId, Thread>,
    sms_requested: bool,
    sms_listed: bool,
    sms_asked_ms: u64,
    ledger: SendLedger,
    requests: HashMap<String, SendId>,
    send_reasons: HashMap<SendId, String>,
    send_times: VecDeque<u64>,
    last_notify_ms: u64,
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
            contacts_synced: None,
            contacts_asked_ms: 0,
            calendar_requested: false,
            cal_token: None,
            cal_names: Vec::new(),
            cal_truncated: false,
            cal_synced: None,
            cal_asked_ms: 0,
            cal_request_seq: 0,
            threads: BTreeMap::new(),
            sms_requested: false,
            sms_listed: false,
            sms_asked_ms: 0,
            ledger: SendLedger::new(),
            requests: HashMap::new(),
            send_reasons: HashMap::new(),
            send_times: VecDeque::new(),
            last_notify_ms: 0,
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
            }
            CALENDAR => {
                self.agenda.events = Store::default();
                self.calendar_requested = false;
                self.cal_token = None;
                self.cal_names.clear();
                self.cal_truncated = false;
                self.cal_synced = None;
            }
            _ => {
                self.threads.clear();
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
            let Ok(contact) = wire::contact_from_vcard(&vcard) else {
                continue;
            };
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
                self.cal_truncated = sync.truncated;
                self.cal_synced = Some(now);
            }
            // We missed a step (restart on either side): ask for the whole window again.
            Err(wire::CalendarMismatch::BaseMismatch) => self.ask_calendar(now, true),
        }
        Disposition::Accepted
    }

    fn on_messages(&mut self, body: &Map<String, Value>, now: u64) -> (Disposition, Vec<String>) {
        if !self.consent[SMS] || !self.sms_requested {
            return (Disposition::Ignored, Vec::new());
        }
        let Ok(parsed) = sms::parse_messages(body) else {
            return (Disposition::Invalid, Vec::new());
        };
        let total: usize = self.threads.values().map(|t| t.conv.len()).sum();
        let mut room = MAX_MESSAGES_TOTAL.saturating_sub(total);
        let mut notify = Vec::new();
        let mut fresh = Vec::new();
        let mut touched = HashSet::new();
        for m in parsed.messages {
            let tid = m.key.thread_id;
            if !self.threads.contains_key(&tid) {
                if self.threads.len() >= MAX_THREADS {
                    continue;
                }
                self.threads.insert(
                    tid,
                    Thread {
                        conv: Conversation::new(tid),
                        exhausted: false,
                        asked_older: false,
                    },
                );
            }
            if room == 0 {
                break;
            }
            let t = self.threads.get_mut(&tid).expect("inserted above");
            let incoming = m.direction() == Direction::Incoming;
            let unread = !m.read;
            let recent = now.saturating_sub(m.date_ms) <= NOTIFY_MAX_AGE_MS;
            let sender = m.addresses.first().map(|a| a.as_str().to_owned());
            match t.conv.merge(m.clone()) {
                Ok(MergeOutcome::Inserted) => {
                    room -= 1;
                    touched.insert(tid);
                    fresh.push(m);
                    if incoming
                        && unread
                        && recent
                        && self.sms_listed
                        && let Some(s) = sender
                    {
                        notify.push(s);
                    }
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
            }
        }
        for t in self.threads.values_mut() {
            if t.asked_older {
                t.exhausted = true;
                t.asked_older = false;
            }
        }
        self.correlate(&fresh, now);
        self.sms_listed = true;
        // One notification per burst per gap, so a flood of texts cannot flood the desktop.
        if now.saturating_sub(self.last_notify_ms) < NOTIFY_MIN_GAP_MS {
            notify.clear();
        } else if !notify.is_empty() {
            self.last_notify_ms = now;
            notify.truncate(1);
        }
        (Disposition::Accepted, notify)
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

#[derive(Default)]
struct Inner {
    devices: HashMap<String, Dev>,
    next_generation: u64,
}

pub struct PhoneData {
    inner: Mutex<Inner>,
    notifier: Mutex<Option<Arc<dyn SmsNotifier>>>,
}

impl Default for PhoneData {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            notifier: Mutex::new(None),
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

    pub fn link_down(&self, id: &str, generation: u64) {
        let mut g = self.lock();
        if let Some(d) = g.devices.get_mut(id)
            && d.generation == generation
        {
            d.tx = None;
            d.ledger.connection_lost(now_ms());
        }
    }

    /// The user changed a consent. Turning one off erases that feature's data at once.
    pub fn set_enabled(&self, id: &str, feature: Feature, on: bool) {
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

    /// Unpaired: drop everything held for the device.
    pub fn forget(&self, id: &str) {
        self.lock().devices.remove(id);
    }

    /// An inbound packet of a phone-data kind. See [`Disposition`].
    pub fn on_packet(&self, id: &str, kind: &str, body: &Map<String, Value>) -> Disposition {
        let now = now_ms();
        let mut notice = None;
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
                sms::MESSAGES => {
                    let (v, notify) = d.on_messages(body, now);
                    if let Some(sender) = notify.into_iter().next() {
                        let who = self.display_name(d, &sender);
                        notice = Some((d.name.clone(), who));
                    }
                    v
                }
                wire::SMS_STATUS => d.on_sms_status(body, now),
                _ => Disposition::Invalid,
            }
        };
        if let Some((device, sender)) = notice
            && let Some(n) = self
                .notifier
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        {
            n.received(&device, &sender);
        }
        verdict
    }

    fn display_name(&self, d: &Dev, address: &str) -> String {
        let names = name_index(d);
        names.get(&suffix(address)).cloned().unwrap_or_else(|| {
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

    pub fn send_text(
        &self,
        device: &str,
        recipient: &str,
        text: &str,
        submission: &str,
    ) -> Result<u64, ErrorBody> {
        let intent = OutgoingTextIntent::new(&[recipient], text, SimSelection::PhoneDefault)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        let submission =
            SubmissionId::new(submission).map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        d.check_can_send(now)?;
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

    pub fn resend(&self, device: &str, send_id: u64, submission: &str) -> Result<u64, ErrorBody> {
        let submission =
            SubmissionId::new(submission).map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        let now = now_ms();
        let mut g = self.lock();
        let d = Self::target(&mut g, device)?;
        d.check_can_send(now)?;
        let intent = d
            .ledger
            .get(SendId(send_id))
            .ok_or_else(|| err(ErrorCode::NotFound, "unknown message"))?
            .intent
            .clone();
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
                sms: d.state_of(SMS),
                contacts_count: d.agenda.contacts.live().count(),
                events_count: d.agenda.events.live().count(),
                calendar_names: d.cal_names.clone(),
                calendar_truncated: d.cal_truncated,
                contacts_synced_unix_ms: d.contacts_synced,
                calendar_synced_unix_ms: d.cal_synced,
                sms_status_reports: d.caps.sms_status,
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
        let date = parse_day(day)?;
        let query: String = query.chars().take(64).collect();
        let now = now_ms();
        let mut g = self.lock();
        // Opening the window refreshes at most every five minutes; no background timer exists.
        for d in g.devices.values_mut() {
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
        let mut events = Vec::new();
        let mut ids: Vec<&String> = g.devices.keys().collect();
        ids.sort();
        for id in ids {
            let d = &g.devices[id];
            if d.consent[CONTACTS] {
                let found = d.agenda.people(&query);
                people_total += found.len();
                for rec in found {
                    if people.len() >= MAX_PEOPLE_SHOWN {
                        break;
                    }
                    if let Some(c) = rec.item() {
                        people.push(person(id, rec, c));
                    }
                }
            }
            if d.consent[CALENDAR] {
                for rec in d.agenda.day(date, utc_offset_minutes) {
                    if events.len() >= MAX_EVENTS_SHOWN {
                        break;
                    }
                    if let Some(e) = rec.item() {
                        events.push(event_item(id, rec, e));
                    }
                }
            }
        }
        events.sort_by_key(|e: &EventItem| (!e.all_day, e.start_ms));
        Ok(AgendaView {
            devices,
            people,
            people_total,
            events,
        })
    }

    pub fn messages_view(
        &self,
        device: Option<&str>,
        thread: Option<i64>,
    ) -> Result<MessagesView, ErrorBody> {
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
                let ordered = t.conv.ordered();
                let Some(last) = ordered.last() else { continue };
                let addrs: Vec<String> = last
                    .addresses
                    .iter()
                    .map(|a| a.as_str().to_owned())
                    .collect();
                let title = addrs
                    .iter()
                    .map(|a| names.get(&suffix(a)).cloned().unwrap_or_else(|| a.clone()))
                    .collect::<Vec<_>>()
                    .join(", ");
                conversations.push(ConversationItem {
                    device_id: id.clone(),
                    thread_id: t.conv.thread_id().0,
                    title: if title.is_empty() {
                        "Unknown sender".into()
                    } else {
                        title
                    },
                    addresses: addrs,
                    last_body: truncate(&last.body, 160),
                    last_date_ms: last.date_ms,
                    last_outgoing: last.direction() == Direction::Outgoing,
                    unread: ordered
                        .iter()
                        .filter(|m| !m.read && m.direction() == Direction::Incoming)
                        .count()
                        .try_into()
                        .unwrap_or(u32::MAX),
                    message_count: ordered.len(),
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
        conversations.truncate(MAX_CONVERSATIONS_SHOWN);
        sends.sort_by_key(|s| std::cmp::Reverse(s.created_unix_ms));
        sends.truncate(MAX_SENDS_SHOWN);
        let selected = match (device, thread) {
            (Some(dev), Some(t)) => g.devices.get(dev).filter(|d| d.consent[SMS]).and_then(|d| {
                let th = d.threads.get(&ThreadId(t))?;
                let all = th.conv.ordered();
                let skip = all.len().saturating_sub(MAX_MESSAGES_SHOWN);
                Some(SelectedThread {
                    device_id: dev.to_owned(),
                    thread_id: t,
                    messages: all[skip..]
                        .iter()
                        .map(|m| MessageItem {
                            uid: m.key.uid,
                            mms: m.key.transport == bridge_core::messages::Transport::Mms,
                            outgoing: m.direction() == Direction::Outgoing,
                            body: truncate(&m.body, 4000),
                            date_ms: m.date_ms,
                            attachments: m.attachment_count,
                        })
                        .collect(),
                    may_have_older: !th.exhausted,
                })
            }),
            _ => None,
        };
        Ok(MessagesView {
            devices,
            conversations,
            selected,
            sends,
        })
    }
}

impl Dev {
    fn check_can_send(&mut self, now: u64) -> Result<(), ErrorBody> {
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
            .is_some_and(|t| now.saturating_sub(*t) > 60_000)
        {
            self.send_times.pop_front();
        }
        if self.send_times.len() >= SENDS_PER_MINUTE {
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
        self.send_times.push_back(now);
        Ok(())
    }
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

fn truncate(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Last seven digits: matches `+1 555 123 4567` with `5551234567` without a phone-number library.
fn suffix(s: &str) -> String {
    let d = digits(s);
    d[d.len().saturating_sub(7)..].to_owned()
}

fn name_index(d: &Dev) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if d.consent[CONTACTS] {
        for (_, c) in d.agenda.contacts.live() {
            for p in &c.phones {
                let s = suffix(p);
                if s.len() >= 5 && !c.display_name.trim().is_empty() {
                    m.entry(s).or_insert_with(|| c.display_name.clone());
                }
            }
        }
    }
    m
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

fn event_item(device: &str, rec: &Record<CalendarEvent>, e: &CalendarEvent) -> EventItem {
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
        description: e.description.as_ref().map(|d| truncate(d, 600)),
    }
}

/// Best-effort freedesktop notification through `notify-send`: "New message from <sender>". The
/// message text is never part of it.
pub struct NotifySendSms;

impl NotifySendSms {
    pub fn args(device: &str, sender: &str) -> Vec<String> {
        let clean = |s: &str| -> String {
            s.chars()
                .filter(|c| !c.is_control())
                .take(48)
                .collect::<String>()
        };
        vec![
            "--app-name=GeekBridge".into(),
            "--expire-time=8000".into(),
            "--".into(),
            format!("New message from {}", clean(sender)),
            format!("On {}", clean(device)),
        ]
    }
}

impl SmsNotifier for NotifySendSms {
    fn received(&self, device_name: &str, sender: &str) {
        let args = Self::args(device_name, sender);
        std::thread::spawn(move || {
            let _ = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }
    fn caps_all() -> Caps {
        Caps {
            contacts: true,
            calendar: true,
            sms_read: true,
            sms_send: true,
            sms_status: true,
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
        let v = h.messages_view(Some(ID), Some(4)).unwrap();
        assert_eq!(v.conversations.len(), 1);
        assert_eq!(v.conversations[0].unread, 2);
        assert_eq!(v.selected.as_ref().unwrap().messages.len(), 2);

        // Sending: handed to phone, then the phone's own reports move it forward.
        let id = h
            .send_text(ID, "+15551230000", "on my way", "sub-1")
            .unwrap();
        let out = rx.try_recv().unwrap();
        assert_eq!(out.kind, sms::REQUEST);
        let rid = out.body["requestId"].as_str().unwrap().to_owned();
        assert_eq!(
            h.send_text(ID, "+15551230000", "on my way", "sub-1")
                .unwrap(),
            id
        );
        assert!(
            rx.try_recv().is_err(),
            "same submission must not send twice"
        );
        let phase = |h: &PhoneData| h.messages_view(None, None).unwrap().sends[0].phase;
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
    fn dropped_link_after_dispatch_is_uncertain_and_never_auto_resent() {
        let h = PhoneData::new();
        let (mut rx, generation) = h.link_up(ID, "Pixel", caps_all(), (false, false, true));
        drain(&mut rx);
        h.send_text(ID, "5551230000", "hi", "s1").unwrap();
        h.link_down(ID, generation);
        let v = h.messages_view(None, None).unwrap();
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
    fn send_refusals() {
        let h = PhoneData::new();
        let (mut rx, g) = h.link_up(ID, "Pixel", caps_all(), (false, false, false));
        drain(&mut rx);
        assert_eq!(
            h.send_text(ID, "1", "x", "a").unwrap_err().code,
            ErrorCode::Unsupported
        );
        h.set_enabled(ID, Feature::Sms, true);
        drain(&mut rx);
        assert_eq!(
            h.send_text(ID, "", "x", "a").unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            h.send_text(ID, "1", "  ", "a").unwrap_err().code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            h.send_text(ID, "1", "x", "bad id").unwrap_err().code,
            ErrorCode::BadRequest
        );
        for i in 0..SENDS_PER_MINUTE {
            h.send_text(ID, "5551230000", "x", &format!("s{i}"))
                .unwrap();
        }
        assert_eq!(
            h.send_text(ID, "5551230000", "x", "late").unwrap_err().code,
            ErrorCode::TooMany
        );
        h.link_down(ID, g);
        assert_eq!(
            h.send_text(ID, "5551230000", "x", "off").unwrap_err().code,
            ErrorCode::NotConnected
        );
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
}
