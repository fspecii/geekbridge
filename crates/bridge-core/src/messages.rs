// SPDX-License-Identifier: GPL-3.0-or-later
//! SMS domain model: conversations, messages, SIM selection, validated outgoing text and a
//! send lifecycle that never resends automatically once a send might have reached the phone.
//!
//! Std + serde only, no I/O and no clock: callers pass `now_ms`. Nothing here talks to a phone.
//! The pinned KDE Android SMS plugin has no send acknowledgement at all (see `docs/phone-data-link.md`),
//! so `AcceptedByPhone`/`Sent`/`Delivered` are states of this model, not things KDE can report today.
//! MMS attachments and RCS are out of scope: MMS messages parse (text part only, `attachment_count`
//! kept) but nothing here sends them.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, fmt};

/// Application bound in Unicode scalar values (not a carrier limit; the phone segments long text).
pub const MAX_TEXT_CHARS: usize = 2000;
pub const MAX_ADDRESS_CHARS: usize = 64;
pub const MAX_ADDRESSES_PER_MESSAGE: usize = 64;
pub const MAX_SUBMISSION_ID_CHARS: usize = 64;
/// Upper bound for a body received from the phone (bytes).
pub const MAX_INCOMING_BODY_BYTES: usize = 256 * 1024;
pub const MAX_PAGE_SIZE: u32 = 200;
pub const DEFAULT_PAGE_SIZE: u32 = 50;
pub const MAX_CONVERSATION_MESSAGES: usize = 100_000;
pub const MAX_LEDGER_RECORDS: usize = 10_000;
/// SMS v1 targets exactly one recipient: KDE Android sends multi-recipient messages as MMS by default.
pub const SMS_MAX_RECIPIENTS: usize = 1;
/// `SubscriptionManager.DEFAULT_SUBSCRIPTION_ID` (AOSP): "the default subscription", not a real SIM.
const ANDROID_DEFAULT_SUBSCRIPTION_ID: i32 = i32::MAX;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    EmptyText,
    BlankText,
    TextTooLong { chars: usize },
    ForbiddenControlChar,
    EmptyAddress,
    AddressTooLong,
    AddressControlChar,
    NoRecipient,
    GroupNeedsMms { recipients: usize },
    InvalidSubscription(i32),
    InvalidSubmissionId,
    NegativeField(&'static str),
    InvalidField(&'static str),
    UnsupportedMessageBox(i64),
    BodyTooLarge,
    TooManyAddresses,
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => write!(f, "message text is empty"),
            Self::BlankText => write!(f, "message text is only whitespace"),
            Self::TextTooLong { chars } => write!(
                f,
                "message text has {chars} characters, limit is {MAX_TEXT_CHARS}"
            ),
            Self::ForbiddenControlChar => {
                write!(f, "message text contains a forbidden control character")
            }
            Self::EmptyAddress => write!(f, "address is empty"),
            Self::AddressTooLong => write!(f, "address longer than {MAX_ADDRESS_CHARS} characters"),
            Self::AddressControlChar => write!(f, "address contains a control character"),
            Self::NoRecipient => write!(f, "no recipient"),
            Self::GroupNeedsMms { recipients } => write!(
                f,
                "{recipients} recipients would be a group (MMS) message; unsupported"
            ),
            Self::InvalidSubscription(v) => {
                write!(f, "{v} is not a selectable SIM subscription id")
            }
            Self::InvalidSubmissionId => write!(
                f,
                "submission id must be 1..={MAX_SUBMISSION_ID_CHARS} ASCII [A-Za-z0-9_-]"
            ),
            Self::NegativeField(n) => write!(f, "field {n} is negative"),
            Self::InvalidField(n) => write!(f, "field {n} is missing or not a number"),
            Self::UnsupportedMessageBox(v) => write!(f, "unsupported Android message type {v}"),
            Self::BodyTooLarge => write!(f, "message body exceeds {MAX_INCOMING_BODY_BYTES} bytes"),
            Self::TooManyAddresses => write!(f, "more than {MAX_ADDRESSES_PER_MESSAGE} addresses"),
        }
    }
}
impl Error for ValidationError {}

// ---------------------------------------------------------------------------------------------
// Identifiers and addresses
// ---------------------------------------------------------------------------------------------

/// Android `thread_id`. Only meaningful on one phone; never compare across devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(pub i64);

/// Phone number, short code or e-mail address. Trimmed; compared as an exact string.
/// No phone-number normalisation: KDE's phone-side `isSamePhoneNumber` is locale-aware and not reproduced.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Address(String);
impl Address {
    pub fn new(raw: &str) -> Result<Self, ValidationError> {
        let s = raw.trim();
        if s.is_empty() {
            return Err(ValidationError::EmptyAddress);
        }
        if s.chars().count() > MAX_ADDRESS_CHARS {
            return Err(ValidationError::AddressTooLong);
        }
        if s.chars().any(char::is_control) {
            return Err(ValidationError::AddressControlChar);
        }
        Ok(Self(s.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for Address {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(&s).map_err(serde::de::Error::custom)
    }
}

/// Android subscription id of a real SIM. `0` is rejected: KDE's phone side emits `0` when the
/// `sub_id` column is missing, so `0` cannot be told apart from "unknown". Negative values are
/// Android placeholders/invalid and `i32::MAX` is Android's "default" sentinel. This is
/// GeekBridge's conservative policy, not an Android guarantee about valid ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SubscriptionId(i32);
impl SubscriptionId {
    pub fn new(v: i32) -> Result<Self, ValidationError> {
        if v < 1 || v == ANDROID_DEFAULT_SUBSCRIPTION_ID {
            Err(ValidationError::InvalidSubscription(v))
        } else {
            Ok(Self(v))
        }
    }
    pub fn get(self) -> i32 {
        self.0
    }
}
impl<'de> Deserialize<'de> for SubscriptionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(i32::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "subscription_id")]
pub enum SimSelection {
    /// Let the phone choose (KDE: omit `subID`; Android falls back to its default).
    #[default]
    PhoneDefault,
    Subscription(SubscriptionId),
}
impl SimSelection {
    /// Value for the KDE `subID` request field; `None` means omit it.
    pub fn kde_sub_id(self) -> Option<i64> {
        match self {
            Self::PhoneDefault => None,
            Self::Subscription(s) => Some(i64::from(s.get())),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    Outgoing,
}

/// `Telephony.TextBasedSmsColumns.MESSAGE_TYPE_*` (AOSP `Telephony.java`): ALL=0 is a query filter,
/// not a stored state, and is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageBox {
    Inbox,
    Sent,
    Draft,
    Outbox,
    Failed,
    Queued,
}
impl MessageBox {
    pub fn from_android(v: i64) -> Option<Self> {
        Some(match v {
            1 => Self::Inbox,
            2 => Self::Sent,
            3 => Self::Draft,
            4 => Self::Outbox,
            5 => Self::Failed,
            6 => Self::Queued,
            _ => return None,
        })
    }
    pub fn direction(self) -> Direction {
        if self == Self::Inbox {
            Direction::Incoming
        } else {
            Direction::Outgoing
        }
    }
    /// What this phone-side box tells us about a send. `Sent` means Android handed it onward, not
    /// that the recipient got it: KDE's SMS columns do not include the delivery `status` column.
    pub fn send_evidence(self) -> Option<SendEvent> {
        match self {
            Self::Outbox | Self::Queued => Some(SendEvent::PhoneAccepted),
            Self::Sent => Some(SendEvent::PhoneSent),
            Self::Failed => Some(SendEvent::PhoneFailed),
            Self::Inbox | Self::Draft => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Sms,
    Mms,
}

/// Phone-local identity of a message. Android's SMS and MMS tables number rows independently, so
/// `_id` alone is not unique within a thread (KDE desktop's `m_known_messages` set keys on
/// thread + `_id` only and can conflate them); the transport is part of the key here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageKey {
    pub thread_id: ThreadId,
    pub transport: Transport,
    pub uid: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub key: MessageKey,
    pub message_box: MessageBox,
    pub body: String,
    /// Milliseconds since the epoch on the *phone* clock (MMS seconds are scaled by the phone).
    pub date_ms: u64,
    pub read: bool,
    /// Remote party first for incoming multi-target messages; recipients (self excluded) for outgoing.
    pub addresses: Vec<Address>,
    pub subscription: Option<SubscriptionId>,
    /// MMS parts reported by the phone. Content is not modelled yet.
    pub attachment_count: u32,
}
impl Message {
    pub fn direction(&self) -> Direction {
        self.message_box.direction()
    }
}

/// Lenient JSON number as emitted by KDE: the code writes integers, doc comments show strings/bools.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Num {
    I(i64),
    B(bool),
    S(String),
}
impl Num {
    fn int(&self) -> Option<i64> {
        match self {
            Self::I(v) => Some(*v),
            Self::B(b) => Some(i64::from(*b)),
            Self::S(s) => s.trim().parse().ok(),
        }
    }
}
fn req(n: &Option<Num>, name: &'static str) -> Result<i64, ValidationError> {
    n.as_ref()
        .and_then(Num::int)
        .ok_or(ValidationError::InvalidField(name))
}
fn nonneg(n: &Option<Num>, name: &'static str) -> Result<i64, ValidationError> {
    let v = req(n, name)?;
    if v < 0 {
        Err(ValidationError::NegativeField(name))
    } else {
        Ok(v)
    }
}

#[derive(Clone, Debug, Deserialize)]
struct WireAddress {
    address: String,
}

/// One element of the `messages` array of a `kdeconnect.sms.messages` packet. Untrusted input:
/// convert with [`Message::from_wire`]. Unknown fields (`event`) are ignored; MMS is recognised by
/// the presence of the `attachments` array, which the phone writes only for MMS rows.
#[derive(Clone, Debug, Deserialize)]
pub struct WireMessage {
    #[serde(default)]
    addresses: Vec<WireAddress>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    date: Option<Num>,
    #[serde(default, rename = "type")]
    kind: Option<Num>,
    #[serde(default)]
    read: Option<Num>,
    #[serde(default)]
    thread_id: Option<Num>,
    #[serde(default, rename = "_id")]
    uid: Option<Num>,
    #[serde(default)]
    sub_id: Option<Num>,
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
}

impl Message {
    pub fn from_wire(w: WireMessage) -> Result<Self, ValidationError> {
        let body = w.body.unwrap_or_default();
        if body.len() > MAX_INCOMING_BODY_BYTES {
            return Err(ValidationError::BodyTooLarge);
        }
        if w.addresses.len() > MAX_ADDRESSES_PER_MESSAGE {
            return Err(ValidationError::TooManyAddresses);
        }
        let raw_box = req(&w.kind, "type")?;
        let message_box = MessageBox::from_android(raw_box)
            .ok_or(ValidationError::UnsupportedMessageBox(raw_box))?;
        let mut addresses: Vec<Address> = Vec::with_capacity(w.addresses.len());
        for a in &w.addresses {
            let a = Address::new(&a.address)?;
            if !addresses.contains(&a) {
                addresses.push(a);
            }
        }
        // Missing/0/invalid `sub_id` means "unknown" (KDE writes 0 when the column is absent).
        let subscription = w
            .sub_id
            .as_ref()
            .and_then(Num::int)
            .and_then(|v| i32::try_from(v).ok())
            .and_then(|v| SubscriptionId::new(v).ok());
        let transport = if w.attachments.is_some() {
            Transport::Mms
        } else {
            Transport::Sms
        };
        Ok(Self {
            key: MessageKey {
                thread_id: ThreadId(nonneg(&w.thread_id, "thread_id")?),
                transport,
                uid: nonneg(&w.uid, "_id")?,
            },
            message_box,
            body,
            addresses,
            subscription,
            date_ms: u64::try_from(nonneg(&w.date, "date")?)
                .map_err(|_| ValidationError::NegativeField("date"))?,
            read: req(&w.read, "read").map(|v| v != 0).unwrap_or(false),
            attachment_count: w
                .attachments
                .map_or(0, |a| u32::try_from(a.len()).unwrap_or(u32::MAX)),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Conversations: deduplication and ordering
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Inserted,
    /// Same key, different content (e.g. outbox -> sent, unread -> read). The phone is authoritative.
    Updated,
    /// Same key, identical content: dropped silently. Paging re-requests overlap by design.
    Duplicate,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MergeSummary {
    pub inserted: usize,
    pub updated: usize,
    pub duplicates: usize,
    pub rejected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    WrongThread,
    ConversationFull,
    LedgerFull,
    SubmissionConflict(SendId),
    UnknownSend(SendId),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongThread => write!(f, "message belongs to another thread"),
            Self::ConversationFull => write!(
                f,
                "conversation holds {MAX_CONVERSATION_MESSAGES} messages already"
            ),
            Self::LedgerFull => write!(f, "send ledger holds {MAX_LEDGER_RECORDS} records already"),
            Self::SubmissionConflict(id) => write!(
                f,
                "submission id already used for a different message (send {})",
                id.0
            ),
            Self::UnknownSend(id) => write!(f, "unknown send {}", id.0),
        }
    }
}
impl Error for StoreError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    thread_id: ThreadId,
    messages: BTreeMap<MessageKey, Message>,
}
impl Conversation {
    pub fn new(thread_id: ThreadId) -> Self {
        Self {
            thread_id,
            messages: BTreeMap::new(),
        }
    }
    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }
    pub fn len(&self) -> usize {
        self.messages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
    pub fn get(&self, key: &MessageKey) -> Option<&Message> {
        self.messages.get(key)
    }

    pub fn merge(&mut self, m: Message) -> Result<MergeOutcome, StoreError> {
        if m.key.thread_id != self.thread_id {
            return Err(StoreError::WrongThread);
        }
        let full = self.messages.len() >= MAX_CONVERSATION_MESSAGES;
        match self.messages.get_mut(&m.key) {
            Some(old) if *old == m => Ok(MergeOutcome::Duplicate),
            Some(old) => {
                *old = m;
                Ok(MergeOutcome::Updated)
            }
            None if full => Err(StoreError::ConversationFull),
            None => {
                self.messages.insert(m.key, m);
                Ok(MergeOutcome::Inserted)
            }
        }
    }
    /// Merges a batch; messages that fail (wrong thread, capacity) are counted in `rejected`.
    pub fn merge_all(&mut self, batch: impl IntoIterator<Item = Message>) -> MergeSummary {
        let mut s = MergeSummary::default();
        for m in batch {
            match self.merge(m) {
                Ok(MergeOutcome::Inserted) => s.inserted += 1,
                Ok(MergeOutcome::Updated) => s.updated += 1,
                Ok(MergeOutcome::Duplicate) => s.duplicates += 1,
                Err(_) => s.rejected += 1,
            }
        }
        s
    }
    /// Oldest first; ties on the phone timestamp are broken by key so the order is deterministic.
    pub fn ordered(&self) -> Vec<&Message> {
        let mut v: Vec<&Message> = self.messages.values().collect();
        v.sort_by_key(|m| (m.date_ms, m.key));
        v
    }
    /// The latest message (same order as [`Self::ordered`]) without sorting the thread: O(n), no allocation.
    pub fn newest(&self) -> Option<&Message> {
        self.messages.values().max_by_key(|m| (m.date_ms, m.key))
    }
    pub fn oldest_date_ms(&self) -> Option<u64> {
        self.messages.values().map(|m| m.date_ms).min()
    }
    /// Newest phone timestamp held.
    pub fn newest_date_ms(&self) -> Option<u64> {
        self.messages.values().map(|m| m.date_ms).max()
    }
    /// Drops the oldest messages until at most `keep` remain; returns how many went. This bounds
    /// memory for a long thread: the newest history is what the window shows.
    pub fn trim_oldest(&mut self, keep: usize) -> usize {
        let excess = self.messages.len().saturating_sub(keep);
        if excess == 0 {
            return 0;
        }
        let mut order: Vec<(u64, MessageKey)> =
            self.messages.values().map(|m| (m.date_ms, m.key)).collect();
        order.sort_unstable();
        for (_, key) in order.into_iter().take(excess) {
            self.messages.remove(&key);
        }
        excess
    }
    /// Iterates in key order (not time order); for searches that do not care about order.
    pub fn iter(&self) -> impl Iterator<Item = &Message> {
        self.messages.values()
    }
}

// ---------------------------------------------------------------------------------------------
// Segment counting
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmsEncoding {
    /// GSM 03.38 default alphabet (7 bit); a few characters cost two septets.
    Gsm7,
    /// UTF-16 code units (any character outside GSM, including every emoji).
    Ucs2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentInfo {
    pub encoding: SmsEncoding,
    /// Septets (GSM) or UTF-16 code units (UCS-2) the text costs.
    pub units: usize,
    /// Number of SMS the phone will most likely send; 0 for empty text.
    pub segments: usize,
    /// Capacity of one segment at the current length (single or concatenated).
    pub per_segment: usize,
    /// Units left in the last segment.
    pub remaining: usize,
}

const GSM_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
const GSM_EXTENDED: &str = "^{}\\[~]|€\u{c}";

fn gsm_cost(c: char) -> Option<usize> {
    if GSM_BASIC.contains(c) {
        Some(1)
    } else if GSM_EXTENDED.contains(c) {
        Some(2)
    } else {
        None
    }
}

/// Estimates how many SMS segments `text` needs: 160/153 septets for GSM text, 70/67 UTF-16 units
/// as soon as one character is outside the GSM alphabet (an emoji turns the whole message into
/// UCS-2). An escape pair or a surrogate pair is never split across segments. This is the standard
/// rule, not a promise: the phone and carrier have the last word (`SmsManager.divideMessage`).
pub fn sms_segments(text: &str) -> SegmentInfo {
    let gsm = text.chars().all(|c| gsm_cost(c).is_some());
    let (encoding, single, multi) = if gsm {
        (SmsEncoding::Gsm7, 160, 153)
    } else {
        (SmsEncoding::Ucs2, 70, 67)
    };
    let cost = |c: char| {
        if gsm {
            gsm_cost(c).unwrap_or(1)
        } else {
            c.len_utf16()
        }
    };
    let units: usize = text.chars().map(cost).sum();
    if units == 0 {
        return SegmentInfo {
            encoding,
            units,
            segments: 0,
            per_segment: single,
            remaining: single,
        };
    }
    if units <= single {
        return SegmentInfo {
            encoding,
            units,
            segments: 1,
            per_segment: single,
            remaining: single - units,
        };
    }
    let (mut segments, mut used) = (1, 0);
    for c in text.chars() {
        let n = cost(c);
        if used + n > multi {
            segments += 1;
            used = 0;
        }
        used += n;
    }
    SegmentInfo {
        encoding,
        units,
        segments,
        per_segment: multi,
        remaining: multi - used,
    }
}

// ---------------------------------------------------------------------------------------------
// History paging (kdeconnect.sms.request_conversation)
// ---------------------------------------------------------------------------------------------

/// Parameters for one `kdeconnect.sms.request_conversation`. `range_start_timestamp: None` asks
/// for the newest messages (KDE: field omitted/negative); `Some(t)` asks for messages with
/// `date <= t` (inclusive, newest first), so consecutive pages overlap at the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub thread_id: ThreadId,
    pub range_start_timestamp: Option<u64>,
    pub number_to_request: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOutcome {
    /// More history may exist; request [`HistoryPager::request`] again.
    Advanced { new: usize },
    /// The phone returned nothing new and fewer messages than asked for: start of history reached.
    Exhausted,
    /// A full page contained only known messages: more than `page_size` messages share the
    /// boundary timestamp. Call [`HistoryPager::grow_page_size`] and retry, or give up.
    Stalled,
}
#[derive(Clone, Debug)]
pub struct HistoryPager {
    thread_id: ThreadId,
    page_size: u32,
    cursor_ms: Option<u64>,
    done: bool,
}
impl HistoryPager {
    pub fn new(thread_id: ThreadId, page_size: u32) -> Self {
        Self {
            thread_id,
            page_size: page_size.clamp(1, MAX_PAGE_SIZE),
            cursor_ms: None,
            done: false,
        }
    }
    pub fn is_done(&self) -> bool {
        self.done
    }
    pub fn page_size(&self) -> u32 {
        self.page_size
    }
    pub fn request(&self) -> Option<PageRequest> {
        (!self.done).then_some(PageRequest {
            thread_id: self.thread_id,
            range_start_timestamp: self.cursor_ms,
            number_to_request: self.page_size,
        })
    }
    /// Doubles the page size (capped). Returns false if it cannot grow, i.e. the stall is final.
    pub fn grow_page_size(&mut self) -> bool {
        let next = self.page_size.saturating_mul(2).min(MAX_PAGE_SIZE);
        let grew = next > self.page_size;
        self.page_size = next;
        grew
    }
    /// Merges one response. The cursor is the oldest timestamp seen (not minus one): ties at the
    /// boundary would otherwise be skipped; the inclusive overlap is removed by deduplication.
    pub fn absorb(&mut self, conv: &mut Conversation, page: Vec<Message>) -> PageOutcome {
        let returned = page.len();
        let oldest = page
            .iter()
            .filter(|m| m.key.thread_id == self.thread_id)
            .map(|m| m.date_ms)
            .min();
        let s = conv.merge_all(page);
        let full = returned >= self.page_size as usize;
        if s.inserted == 0 && !full {
            self.done = true;
            return PageOutcome::Exhausted;
        }
        if s.inserted == 0 {
            return PageOutcome::Stalled;
        }
        if let Some(o) = oldest {
            self.cursor_ms = Some(o);
        }
        if !full {
            self.done = true;
            return PageOutcome::Exhausted;
        }
        PageOutcome::Advanced { new: s.inserted }
    }
}

// ---------------------------------------------------------------------------------------------
// Outgoing text
// ---------------------------------------------------------------------------------------------

/// Text is stored exactly as given: no trimming, no Unicode normalisation. Rejected: empty,
/// whitespace-only, over [`MAX_TEXT_CHARS`] scalar values, and control characters other than
/// `\n`, `\r`, `\t` (so NUL and escape sequences cannot be smuggled to the phone). Zero-width and
/// other format characters are not detected as "blank"; std offers no Unicode category test.
fn validate_text(text: &str) -> Result<(), ValidationError> {
    if text.is_empty() {
        return Err(ValidationError::EmptyText);
    }
    if text.trim().is_empty() {
        return Err(ValidationError::BlankText);
    }
    let chars = text.chars().count();
    if chars > MAX_TEXT_CHARS {
        return Err(ValidationError::TextTooLong { chars });
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(ValidationError::ForbiddenControlChar);
    }
    Ok(())
}

#[derive(Deserialize)]
struct OutgoingTextIntentRaw {
    recipients: Vec<String>,
    text: String,
    #[serde(default)]
    sim: SimSelection,
}

/// A validated request to send one SMS. Deserialisation re-validates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "OutgoingTextIntentRaw")]
pub struct OutgoingTextIntent {
    recipients: Vec<Address>,
    text: String,
    sim: SimSelection,
}
impl TryFrom<OutgoingTextIntentRaw> for OutgoingTextIntent {
    type Error = ValidationError;
    fn try_from(r: OutgoingTextIntentRaw) -> Result<Self, Self::Error> {
        let refs: Vec<&str> = r.recipients.iter().map(String::as_str).collect();
        Self::new(&refs, r.text, r.sim)
    }
}
impl OutgoingTextIntent {
    /// Recipients are trimmed and exact duplicates collapse, then exactly one must remain.
    pub fn new(
        recipients: &[&str],
        text: impl Into<String>,
        sim: SimSelection,
    ) -> Result<Self, ValidationError> {
        let text = text.into();
        validate_text(&text)?;
        let mut rs: Vec<Address> = Vec::new();
        for r in recipients {
            let a = Address::new(r)?;
            if !rs.contains(&a) {
                rs.push(a);
            }
        }
        match rs.len() {
            0 => return Err(ValidationError::NoRecipient),
            n if n > SMS_MAX_RECIPIENTS => {
                return Err(ValidationError::GroupNeedsMms { recipients: n });
            }
            _ => {}
        }
        Ok(Self {
            recipients: rs,
            text,
            sim,
        })
    }
    pub fn recipients(&self) -> &[Address] {
        &self.recipients
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn sim(&self) -> SimSelection {
        self.sim
    }
}

/// Caller-generated token for one compose action (e.g. one press of "Send"). This is GeekBridge's
/// own local idempotency key; KDE's protocol has no client id and does not echo one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SubmissionId(String);
impl SubmissionId {
    pub fn new(s: &str) -> Result<Self, ValidationError> {
        let ok = !s.is_empty()
            && s.len() <= MAX_SUBMISSION_ID_CHARS
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ValidationError::InvalidSubmissionId)
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for SubmissionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------------------------
// Send lifecycle
// ---------------------------------------------------------------------------------------------

/// Local identifier of a send record. Never sent to the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SendId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    PhoneReported,
    LocalAbort,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertainCause {
    ConnectionLost,
    AckTimeout,
}

/// `Queued` = held locally; with `dispatched == true` the request may already be on the wire.
/// `Uncertain` = request may or may not have been acted on; automatic resend is forbidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum SendState {
    Queued,
    AcceptedByPhone,
    Sent,
    Delivered,
    Failed { reason: FailureReason },
    Uncertain { cause: UncertainCause },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendEvent {
    /// The request is about to be (or has begun to be) written to the transport. Irreversible.
    Dispatched,
    ConnectionLost,
    AckTimedOut,
    PhoneAccepted,
    PhoneSent,
    PhoneDelivered,
    PhoneFailed,
    /// Cancelled before anything was dispatched.
    LocalAbort,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Changed,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionError {
    AlreadyDispatched,
    /// The phone cannot have acted on a request that was never dispatched.
    NeverDispatched,
    NotAbortable,
    /// Contradictory terminal evidence (e.g. delivered after failed). State is left unchanged.
    Conflict {
        from: SendState,
        event: SendEvent,
    },
    ResendNotAllowed(SendState),
}
impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyDispatched => write!(f, "send was already dispatched"),
            Self::NeverDispatched => {
                write!(f, "phone evidence for a send that was never dispatched")
            }
            Self::NotAbortable => write!(f, "only a never-dispatched send can be aborted"),
            Self::Conflict { from, event } => {
                write!(f, "event {event:?} contradicts state {from:?}")
            }
            Self::ResendNotAllowed(s) => write!(
                f,
                "resend is only possible from failed or uncertain, not {s:?}"
            ),
        }
    }
}
impl Error for TransitionError {}

/// Required to resend from `Failed` or `Uncertain`. For `Uncertain` the first copy may still go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResendConsent {
    UserAcceptsDuplicateRisk,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRecord {
    pub id: SendId,
    pub submission: SubmissionId,
    pub intent: OutgoingTextIntent,
    state: SendState,
    dispatched: bool,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// Set when this record is a user-approved resend of an earlier failed/uncertain one.
    pub resend_of: Option<SendId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Correlation {
    None,
    Unique(MessageKey),
    Ambiguous(usize),
}

impl SendRecord {
    pub fn new(
        id: SendId,
        submission: SubmissionId,
        intent: OutgoingTextIntent,
        now_ms: u64,
    ) -> Self {
        Self {
            id,
            submission,
            intent,
            state: SendState::Queued,
            dispatched: false,
            created_ms: now_ms,
            updated_ms: now_ms,
            resend_of: None,
        }
    }
    pub fn state(&self) -> SendState {
        self.state
    }
    pub fn was_dispatched(&self) -> bool {
        self.dispatched
    }
    /// The only case where sending again without asking the user is safe: nothing ever left.
    pub fn can_auto_send(&self) -> bool {
        self.state == SendState::Queued && !self.dispatched
    }

    pub fn apply(&mut self, event: SendEvent, now_ms: u64) -> Result<Applied, TransitionError> {
        use SendEvent as E;
        use SendState as S;
        let from = self.state;
        let conflict = TransitionError::Conflict { from, event };
        let queued_undispatched = from == S::Queued && !self.dispatched;
        let next = match event {
            E::Dispatched => {
                if self.dispatched {
                    return Err(TransitionError::AlreadyDispatched);
                }
                if from != S::Queued {
                    return Err(TransitionError::Conflict { from, event });
                }
                self.dispatched = true;
                self.updated_ms = now_ms;
                return Ok(Applied::Changed);
            }
            E::ConnectionLost | E::AckTimedOut => match from {
                S::Queued if self.dispatched => S::Uncertain {
                    cause: if event == E::ConnectionLost {
                        UncertainCause::ConnectionLost
                    } else {
                        UncertainCause::AckTimeout
                    },
                },
                // Never dispatched, or the phone already acknowledged / the outcome is settled.
                _ => from,
            },
            E::LocalAbort => {
                if !queued_undispatched {
                    return Err(TransitionError::NotAbortable);
                }
                S::Failed {
                    reason: FailureReason::LocalAbort,
                }
            }
            E::PhoneAccepted | E::PhoneSent | E::PhoneDelivered | E::PhoneFailed => {
                if queued_undispatched {
                    return Err(TransitionError::NeverDispatched);
                }
                match (from, event) {
                    // Settled outcomes: lower evidence is stale, contradictory evidence is an error.
                    (S::Delivered, E::PhoneFailed) => return Err(conflict),
                    (S::Delivered, _) => from,
                    (S::Failed { .. }, E::PhoneFailed | E::PhoneAccepted) => from,
                    (S::Failed { .. }, _) => return Err(conflict),
                    (S::Sent, E::PhoneAccepted | E::PhoneSent) => from,
                    (S::AcceptedByPhone, E::PhoneAccepted) => from,
                    (_, E::PhoneAccepted) => S::AcceptedByPhone,
                    (_, E::PhoneSent) => S::Sent,
                    (_, E::PhoneDelivered) => S::Delivered,
                    (_, _) => S::Failed {
                        reason: FailureReason::PhoneReported,
                    },
                }
            }
        };
        if next == from {
            return Ok(Applied::Unchanged);
        }
        self.state = next;
        self.updated_ms = now_ms;
        Ok(Applied::Changed)
    }

    /// Feeds phone-side evidence from a synced message that [`correlate`](Self::correlate) matched.
    pub fn apply_message_evidence(
        &mut self,
        m: &Message,
        now_ms: u64,
    ) -> Result<Applied, TransitionError> {
        match m.message_box.send_evidence() {
            Some(e) => self.apply(e, now_ms),
            None => Ok(Applied::Unchanged),
        }
    }

    /// Creates a new record for the same content. Only from `Failed` or `Uncertain`, and only with
    /// explicit consent. The old record is left as it is; if it was uncertain it can still resolve.
    pub fn resend(
        &self,
        id: SendId,
        submission: SubmissionId,
        _consent: ResendConsent,
        now_ms: u64,
    ) -> Result<Self, TransitionError> {
        match self.state {
            SendState::Failed { .. } | SendState::Uncertain { .. } => {
                let mut r = Self::new(id, submission, self.intent.clone(), now_ms);
                r.resend_of = Some(self.id);
                Ok(r)
            }
            s => Err(TransitionError::ResendNotAllowed(s)),
        }
    }

    /// Heuristic only. KDE has no send id, so a sent message is recognised by outgoing direction,
    /// same recipient, identical body and `date_ms >= not_before_ms` (phone clock; the caller
    /// chooses the bound and must allow for clock skew). Identical texts to the same recipient
    /// cannot be told apart and yield `Ambiguous`, which callers must not resolve by guessing.
    pub fn correlate<'a>(
        &self,
        candidates: impl IntoIterator<Item = &'a Message>,
        not_before_ms: u64,
    ) -> Correlation {
        let mut hits = candidates.into_iter().filter(|m| {
            m.direction() == Direction::Outgoing
                && m.message_box != MessageBox::Draft
                && m.date_ms >= not_before_ms
                && m.body == self.intent.text
                && self
                    .intent
                    .recipients
                    .iter()
                    .all(|r| m.addresses.contains(r))
        });
        match (hits.next(), hits.next()) {
            (None, _) => Correlation::None,
            (Some(m), None) => Correlation::Unique(m.key),
            (Some(_), Some(_)) => Correlation::Ambiguous(2 + hits.count()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    Created(SendId),
    Existing(SendId),
}

/// In-memory send book-keeping. Idempotent per [`SubmissionId`]; the same *content* submitted under
/// a new id is a new send (sending "ok" twice is legitimate).
#[derive(Clone, Debug, Default)]
pub struct SendLedger {
    records: BTreeMap<SendId, SendRecord>,
    by_submission: BTreeMap<SubmissionId, SendId>,
    next_id: u64,
}
impl SendLedger {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, id: SendId) -> Option<&SendRecord> {
        self.records.get(&id)
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn alloc(&mut self) -> Result<SendId, StoreError> {
        if self.records.len() >= MAX_LEDGER_RECORDS {
            return Err(StoreError::LedgerFull);
        }
        self.next_id += 1;
        Ok(SendId(self.next_id))
    }
    pub fn submit(
        &mut self,
        submission: SubmissionId,
        intent: OutgoingTextIntent,
        now_ms: u64,
    ) -> Result<Submit, StoreError> {
        if let Some(&id) = self.by_submission.get(&submission) {
            return if self.records[&id].intent == intent {
                Ok(Submit::Existing(id))
            } else {
                Err(StoreError::SubmissionConflict(id))
            };
        }
        let id = self.alloc()?;
        self.by_submission.insert(submission.clone(), id);
        self.records
            .insert(id, SendRecord::new(id, submission, intent, now_ms));
        Ok(Submit::Created(id))
    }
    pub fn apply(
        &mut self,
        id: SendId,
        event: SendEvent,
        now_ms: u64,
    ) -> Result<Applied, ApplyError> {
        self.records
            .get_mut(&id)
            .ok_or(ApplyError::Store(StoreError::UnknownSend(id)))?
            .apply(event, now_ms)
            .map_err(ApplyError::Transition)
    }
    /// Oldest record that has never left the machine; the only kind a sender loop may pick up alone.
    pub fn next_auto_sendable(&self) -> Option<SendId> {
        self.records
            .values()
            .find(|r| r.can_auto_send())
            .map(|r| r.id)
    }
    /// Transport dropped: every dispatched-but-unacknowledged send becomes `Uncertain`. Returns how many.
    pub fn connection_lost(&mut self, now_ms: u64) -> usize {
        self.records
            .values_mut()
            .filter_map(|r| r.apply(SendEvent::ConnectionLost, now_ms).ok())
            .filter(|a| *a == Applied::Changed)
            .count()
    }
    pub fn resend(
        &mut self,
        id: SendId,
        submission: SubmissionId,
        consent: ResendConsent,
        now_ms: u64,
    ) -> Result<Submit, ApplyError> {
        if let Some(&existing) = self.by_submission.get(&submission) {
            let same = self
                .records
                .get(&existing)
                .is_some_and(|r| r.resend_of == Some(id));
            return if same {
                Ok(Submit::Existing(existing))
            } else {
                Err(ApplyError::Store(StoreError::SubmissionConflict(existing)))
            };
        }
        let old = self
            .records
            .get(&id)
            .ok_or(ApplyError::Store(StoreError::UnknownSend(id)))?;
        let new_id = SendId(self.next_id + 1);
        let rec = old
            .resend(new_id, submission.clone(), consent, now_ms)
            .map_err(ApplyError::Transition)?;
        self.alloc().map_err(ApplyError::Store)?;
        self.by_submission.insert(submission, new_id);
        self.records.insert(new_id, rec);
        Ok(Submit::Created(new_id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    Store(StoreError),
    Transition(TransitionError),
}
impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(e) => e.fmt(f),
            Self::Transition(e) => e.fmt(f),
        }
    }
}
impl Error for ApplyError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn intent(text: &str) -> OutgoingTextIntent {
        OutgoingTextIntent::new(&["+15551234567"], text, SimSelection::PhoneDefault).unwrap()
    }
    fn sub(s: &str) -> SubmissionId {
        SubmissionId::new(s).unwrap()
    }
    fn record() -> SendRecord {
        SendRecord::new(SendId(1), sub("a"), intent("hi"), 10)
    }
    fn msg(
        thread: i64,
        transport: Transport,
        uid: i64,
        b: MessageBox,
        date: u64,
        body: &str,
    ) -> Message {
        Message {
            key: MessageKey {
                thread_id: ThreadId(thread),
                transport,
                uid,
            },
            message_box: b,
            body: body.into(),
            date_ms: date,
            read: true,
            addresses: vec![Address::new("+15551234567").unwrap()],
            subscription: None,
            attachment_count: 0,
        }
    }
    fn sms(uid: i64, date: u64) -> Message {
        msg(7, Transport::Sms, uid, MessageBox::Inbox, date, "x")
    }

    // --- outgoing text bounds -------------------------------------------------------------------

    #[test]
    fn text_must_be_nonempty_and_not_blank() {
        assert_eq!(
            OutgoingTextIntent::new(&["1"], "", SimSelection::PhoneDefault),
            Err(ValidationError::EmptyText)
        );
        for blank in [" ", "\n\t ", "\u{3000}", "\u{a0}"] {
            assert_eq!(
                OutgoingTextIntent::new(&["1"], blank, SimSelection::PhoneDefault),
                Err(ValidationError::BlankText),
                "{blank:?}"
            );
        }
    }
    #[test]
    fn text_bound_counts_unicode_scalars_not_bytes() {
        let ok = "😀".repeat(MAX_TEXT_CHARS);
        assert!(ok.len() > MAX_TEXT_CHARS);
        assert!(OutgoingTextIntent::new(&["1"], ok, SimSelection::PhoneDefault).is_ok());
        let long = "é".repeat(MAX_TEXT_CHARS + 1);
        assert_eq!(
            OutgoingTextIntent::new(&["1"], long, SimSelection::PhoneDefault),
            Err(ValidationError::TextTooLong {
                chars: MAX_TEXT_CHARS + 1
            })
        );
        assert!(
            OutgoingTextIntent::new(
                &["1"],
                "a".repeat(MAX_TEXT_CHARS),
                SimSelection::PhoneDefault
            )
            .is_ok()
        );
    }
    #[test]
    fn text_is_kept_verbatim_and_odd_unicode_is_accepted() {
        // Leading/trailing spaces, decomposed accents, ZWJ emoji, RTL and a line break all survive untouched.
        let t = "  e\u{301} 👩‍👩‍👧 שלום\nok  ";
        assert_eq!(intent(t).text(), t);
    }
    #[test]
    fn text_rejects_control_characters_except_whitespace_ones() {
        for bad in ["a\0b", "a\u{1b}[31m", "a\u{7f}", "a\u{85}"] {
            assert_eq!(
                OutgoingTextIntent::new(&["1"], bad, SimSelection::PhoneDefault),
                Err(ValidationError::ForbiddenControlChar),
                "{bad:?}"
            );
        }
        assert!(OutgoingTextIntent::new(&["1"], "a\tb\r\nc", SimSelection::PhoneDefault).is_ok());
    }
    #[test]
    fn recipients_are_trimmed_deduplicated_and_limited_to_one() {
        let i = OutgoingTextIntent::new(&["+1555", " +1555\n"], "hi", SimSelection::PhoneDefault)
            .unwrap();
        assert_eq!(i.recipients().len(), 1);
        assert_eq!(i.recipients()[0].as_str(), "+1555");
        assert_eq!(
            OutgoingTextIntent::new(&[], "hi", SimSelection::PhoneDefault),
            Err(ValidationError::NoRecipient)
        );
        assert_eq!(
            OutgoingTextIntent::new(&["1", "2"], "hi", SimSelection::PhoneDefault),
            Err(ValidationError::GroupNeedsMms { recipients: 2 })
        );
        assert_eq!(
            OutgoingTextIntent::new(&["  "], "hi", SimSelection::PhoneDefault),
            Err(ValidationError::EmptyAddress)
        );
        assert_eq!(
            OutgoingTextIntent::new(&["1\n2"], "hi", SimSelection::PhoneDefault),
            Err(ValidationError::AddressControlChar)
        );
        assert_eq!(
            OutgoingTextIntent::new(
                &[&"9".repeat(MAX_ADDRESS_CHARS + 1)],
                "hi",
                SimSelection::PhoneDefault
            ),
            Err(ValidationError::AddressTooLong)
        );
    }
    #[test]
    fn intent_deserialization_revalidates() {
        let good: OutgoingTextIntent =
            serde_json::from_value(json!({"recipients": [" +1 "], "text": "hi"})).unwrap();
        assert_eq!(good.sim(), SimSelection::PhoneDefault);
        assert_eq!(good.recipients()[0].as_str(), "+1");
        assert!(
            serde_json::from_value::<OutgoingTextIntent>(json!({"recipients": ["+1"], "text": ""}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<OutgoingTextIntent>(json!({"recipients": [], "text": "x"}))
                .is_err()
        );
        let back: OutgoingTextIntent =
            serde_json::from_str(&serde_json::to_string(&good).unwrap()).unwrap();
        assert_eq!(back, good);
    }

    // --- SIM selection --------------------------------------------------------------------------

    #[test]
    fn sim_selection_rules() {
        assert_eq!(SimSelection::default().kde_sub_id(), None);
        assert_eq!(
            SimSelection::Subscription(SubscriptionId::new(3).unwrap()).kde_sub_id(),
            Some(3)
        );
        for bad in [0, -1, -2, i32::MAX] {
            assert_eq!(
                SubscriptionId::new(bad),
                Err(ValidationError::InvalidSubscription(bad))
            );
        }
        assert!(
            serde_json::from_value::<SimSelection>(
                json!({"kind": "subscription", "subscription_id": 0})
            )
            .is_err()
        );
        let s: SimSelection =
            serde_json::from_value(json!({"kind": "subscription", "subscription_id": 2})).unwrap();
        assert_eq!(s.kde_sub_id(), Some(2));
        assert_eq!(
            serde_json::from_value::<SimSelection>(json!({"kind": "phone_default"})).unwrap(),
            SimSelection::PhoneDefault
        );
    }
    #[test]
    fn submission_ids_are_bounded_ascii() {
        for bad in ["", "a b", "é", &"a".repeat(MAX_SUBMISSION_ID_CHARS + 1)] {
            assert!(SubmissionId::new(bad).is_err(), "{bad:?}");
        }
        assert!(SubmissionId::new(&"a".repeat(MAX_SUBMISSION_ID_CHARS)).is_ok());
        assert!(SubmissionId::new("Ab-9_x").is_ok());
    }

    // --- wire parsing ---------------------------------------------------------------------------

    fn wire(v: serde_json::Value) -> Result<Message, ValidationError> {
        Message::from_wire(serde_json::from_value(v).unwrap())
    }

    #[test]
    fn android_message_type_mapping() {
        use MessageBox::*;
        for (n, b, d) in [
            (1, Inbox, Direction::Incoming),
            (2, Sent, Direction::Outgoing),
            (3, Draft, Direction::Outgoing),
            (4, Outbox, Direction::Outgoing),
            (5, Failed, Direction::Outgoing),
            (6, Queued, Direction::Outgoing),
        ] {
            assert_eq!(MessageBox::from_android(n), Some(b));
            assert_eq!(b.direction(), d);
        }
        assert_eq!(MessageBox::from_android(0), None);
        assert_eq!(MessageBox::from_android(7), None);
        assert_eq!(Outbox.send_evidence(), Some(SendEvent::PhoneAccepted));
        assert_eq!(Sent.send_evidence(), Some(SendEvent::PhoneSent));
        assert_eq!(Failed.send_evidence(), Some(SendEvent::PhoneFailed));
        assert_eq!(Draft.send_evidence(), None);
    }
    #[test]
    fn parses_kde_sms_and_mms_packets() {
        let m = wire(json!({"addresses": [{"address": "+15551234567"}, {"address": " +15551234567 "}], "body": "héllo 😀", "date": 1518846484880u64,
            "type": 1, "read": 0, "thread_id": 132, "_id": 9, "sub_id": 3, "event": 1})).unwrap();
        assert_eq!(
            m.key,
            MessageKey {
                thread_id: ThreadId(132),
                transport: Transport::Sms,
                uid: 9
            }
        );
        assert_eq!(
            (m.direction(), m.read, m.date_ms, m.addresses.len()),
            (Direction::Incoming, false, 1518846484880, 1)
        );
        assert_eq!(m.subscription.map(SubscriptionId::get), Some(3));
        let mms = wire(json!({"addresses": [], "body": "", "date": 1518846484000u64, "type": 2, "read": 1, "thread_id": 132, "_id": 9, "sub_id": 0,
            "event": 0, "attachments": [{"part_id": 1, "mime_type": "image/jpeg", "unique_identifier": "a.jpg"}]})).unwrap();
        assert_eq!(
            (mms.key.transport, mms.attachment_count, mms.subscription),
            (Transport::Mms, 1, None)
        );
        assert_ne!(
            m.key, mms.key,
            "SMS and MMS rows with the same _id are different messages"
        );
    }
    #[test]
    fn tolerates_documented_string_and_bool_encodings() {
        let m = wire(
            json!({"date": "1518846484880", "type": "2", "read": true, "thread_id": "5", "_id": 1}),
        )
        .unwrap();
        assert_eq!(
            (m.date_ms, m.message_box, m.read, m.key.thread_id),
            (1518846484880, MessageBox::Sent, true, ThreadId(5))
        );
    }
    #[test]
    fn rejects_malformed_wire_messages() {
        let base = json!({"date": 1, "type": 1, "thread_id": 1, "_id": 1});
        assert!(wire(base.clone()).is_ok());
        let with = |k: &str, v: serde_json::Value| {
            let mut b = base.clone();
            b[k] = v;
            b
        };
        assert_eq!(
            wire(with("thread_id", json!(-1))),
            Err(ValidationError::NegativeField("thread_id"))
        );
        assert_eq!(
            wire(with("type", json!(0))),
            Err(ValidationError::UnsupportedMessageBox(0))
        );
        assert_eq!(
            wire(with("_id", json!("x"))),
            Err(ValidationError::InvalidField("_id"))
        );
        assert_eq!(
            wire(with("date", json!(-5))),
            Err(ValidationError::NegativeField("date"))
        );
        assert_eq!(
            wire(with("addresses", json!([{"address": ""}]))),
            Err(ValidationError::EmptyAddress)
        );
        assert_eq!(
            wire(with("body", json!("a".repeat(MAX_INCOMING_BODY_BYTES + 1)))),
            Err(ValidationError::BodyTooLarge)
        );
        let mut no_type = base.clone();
        no_type.as_object_mut().unwrap().remove("type");
        assert_eq!(wire(no_type), Err(ValidationError::InvalidField("type")));
    }

    // --- conversation deduplication -------------------------------------------------------------

    #[test]
    fn merge_inserts_updates_and_drops_duplicates() {
        let mut c = Conversation::new(ThreadId(7));
        let out = msg(7, Transport::Sms, 1, MessageBox::Outbox, 100, "x");
        assert_eq!(c.merge(out.clone()), Ok(MergeOutcome::Inserted));
        assert_eq!(c.merge(out.clone()), Ok(MergeOutcome::Duplicate));
        let sent = Message {
            message_box: MessageBox::Sent,
            ..out.clone()
        };
        assert_eq!(c.merge(sent.clone()), Ok(MergeOutcome::Updated));
        assert_eq!(c.get(&out.key), Some(&sent));
        assert_eq!(c.len(), 1);
    }
    #[test]
    fn sms_and_mms_with_same_row_id_do_not_collide() {
        let mut c = Conversation::new(ThreadId(7));
        let s = c.merge_all([
            sms(5, 10),
            msg(7, Transport::Mms, 5, MessageBox::Inbox, 11, "m"),
        ]);
        assert_eq!((s.inserted, s.duplicates), (2, 0));
    }
    #[test]
    fn merge_rejects_other_threads_and_counts_them() {
        let mut c = Conversation::new(ThreadId(7));
        assert_eq!(
            c.merge(msg(8, Transport::Sms, 1, MessageBox::Inbox, 1, "x")),
            Err(StoreError::WrongThread)
        );
        let s = c.merge_all([
            sms(1, 1),
            sms(1, 1),
            msg(8, Transport::Sms, 2, MessageBox::Inbox, 1, "x"),
        ]);
        assert_eq!(
            s,
            MergeSummary {
                inserted: 1,
                updated: 0,
                duplicates: 1,
                rejected: 1
            }
        );
    }
    #[test]
    fn ordering_is_by_phone_date_with_deterministic_ties() {
        let mut c = Conversation::new(ThreadId(7));
        c.merge_all([sms(3, 30), sms(2, 20), sms(1, 20), sms(4, 10)]);
        let order: Vec<i64> = c.ordered().iter().map(|m| m.key.uid).collect();
        assert_eq!(order, vec![4, 1, 2, 3]);
        assert_eq!(c.oldest_date_ms(), Some(10));
    }

    // --- paging ---------------------------------------------------------------------------------

    #[test]
    fn pager_walks_back_with_inclusive_overlap_and_ends_on_short_page() {
        let mut c = Conversation::new(ThreadId(7));
        let mut p = HistoryPager::new(ThreadId(7), 2);
        assert_eq!(
            p.request(),
            Some(PageRequest {
                thread_id: ThreadId(7),
                range_start_timestamp: None,
                number_to_request: 2
            })
        );
        assert_eq!(
            p.absorb(&mut c, vec![sms(5, 50), sms(4, 40)]),
            PageOutcome::Advanced { new: 2 }
        );
        // Next request starts at the oldest seen timestamp; the phone repeats message 4.
        assert_eq!(p.request().unwrap().range_start_timestamp, Some(40));
        assert_eq!(
            p.absorb(&mut c, vec![sms(4, 40), sms(3, 30)]),
            PageOutcome::Advanced { new: 1 }
        );
        assert_eq!(
            p.absorb(&mut c, vec![sms(3, 30), sms(2, 20)]),
            PageOutcome::Advanced { new: 1 }
        );
        assert_eq!(p.absorb(&mut c, vec![sms(2, 20)]), PageOutcome::Exhausted);
        assert!(p.is_done() && p.request().is_none());
        assert_eq!(c.len(), 4);
    }
    #[test]
    fn pager_empty_response_is_exhausted() {
        let mut c = Conversation::new(ThreadId(7));
        let mut p = HistoryPager::new(ThreadId(7), 5);
        assert_eq!(p.absorb(&mut c, vec![]), PageOutcome::Exhausted);
    }
    #[test]
    fn pager_stalls_when_ties_fill_a_whole_page_and_can_grow() {
        let mut c = Conversation::new(ThreadId(7));
        let mut p = HistoryPager::new(ThreadId(7), 2);
        assert_eq!(
            p.absorb(&mut c, vec![sms(1, 10), sms(2, 10)]),
            PageOutcome::Advanced { new: 2 }
        );
        assert_eq!(
            p.absorb(&mut c, vec![sms(1, 10), sms(2, 10)]),
            PageOutcome::Stalled
        );
        assert!(!p.is_done());
        assert!(p.grow_page_size());
        assert_eq!(p.page_size(), 4);
        assert_eq!(
            p.absorb(&mut c, vec![sms(1, 10), sms(2, 10), sms(3, 10), sms(0, 5)]),
            PageOutcome::Advanced { new: 2 }
        );
        let mut big = HistoryPager::new(ThreadId(7), MAX_PAGE_SIZE);
        assert!(!big.grow_page_size());
        assert_eq!(HistoryPager::new(ThreadId(7), 0).page_size(), 1);
        assert_eq!(
            HistoryPager::new(ThreadId(7), 10_000).page_size(),
            MAX_PAGE_SIZE
        );
    }

    // --- send lifecycle -------------------------------------------------------------------------

    #[test]
    fn happy_path_and_monotonic_progress() {
        let mut r = record();
        assert!(r.can_auto_send());
        assert_eq!(r.apply(SendEvent::Dispatched, 11), Ok(Applied::Changed));
        assert!(!r.can_auto_send());
        for (e, s) in [
            (SendEvent::PhoneAccepted, SendState::AcceptedByPhone),
            (SendEvent::PhoneSent, SendState::Sent),
            (SendEvent::PhoneDelivered, SendState::Delivered),
        ] {
            assert_eq!(r.apply(e, 12), Ok(Applied::Changed));
            assert_eq!(r.state(), s);
        }
        // Stale and repeated evidence is ignored, never a regression.
        for e in [
            SendEvent::PhoneAccepted,
            SendEvent::PhoneSent,
            SendEvent::PhoneDelivered,
            SendEvent::ConnectionLost,
        ] {
            assert_eq!(r.apply(e, 13), Ok(Applied::Unchanged));
        }
        assert_eq!(r.state(), SendState::Delivered);
        assert!(matches!(
            r.apply(SendEvent::PhoneFailed, 14),
            Err(TransitionError::Conflict { .. })
        ));
        assert_eq!(r.state(), SendState::Delivered);
    }
    #[test]
    fn disconnect_after_dispatch_is_uncertain_and_blocks_auto_resend() {
        let mut ledger = SendLedger::new();
        let Submit::Created(id) = ledger.submit(sub("s1"), intent("pay rent"), 1).unwrap() else {
            panic!()
        };
        assert_eq!(ledger.next_auto_sendable(), Some(id));
        ledger.apply(id, SendEvent::Dispatched, 2).unwrap();
        assert_eq!(ledger.next_auto_sendable(), None);
        assert_eq!(ledger.connection_lost(3), 1);
        assert_eq!(
            ledger.get(id).unwrap().state(),
            SendState::Uncertain {
                cause: UncertainCause::ConnectionLost
            }
        );
        // Reconnecting, repeating the disconnect or re-submitting the same compose action never makes it sendable again.
        assert_eq!(ledger.connection_lost(4), 0);
        assert_eq!(ledger.next_auto_sendable(), None);
        assert!(!ledger.get(id).unwrap().can_auto_send());
        assert_eq!(
            ledger.submit(sub("s1"), intent("pay rent"), 5),
            Ok(Submit::Existing(id))
        );
        assert_eq!(ledger.next_auto_sendable(), None);
        assert_eq!(ledger.len(), 1);
    }
    #[test]
    fn ack_timeout_is_also_uncertain() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 1).unwrap();
        r.apply(SendEvent::AckTimedOut, 2).unwrap();
        assert_eq!(
            r.state(),
            SendState::Uncertain {
                cause: UncertainCause::AckTimeout
            }
        );
    }
    #[test]
    fn disconnect_before_dispatch_keeps_send_queued_and_safe() {
        let mut ledger = SendLedger::new();
        let Submit::Created(id) = ledger.submit(sub("s1"), intent("hi"), 1).unwrap() else {
            panic!()
        };
        assert_eq!(ledger.connection_lost(2), 0);
        assert_eq!(ledger.get(id).unwrap().state(), SendState::Queued);
        assert_eq!(ledger.next_auto_sendable(), Some(id));
    }
    #[test]
    fn dispatch_is_single_shot() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 1).unwrap();
        assert_eq!(
            r.apply(SendEvent::Dispatched, 2),
            Err(TransitionError::AlreadyDispatched)
        );
    }
    #[test]
    fn accepted_after_disconnect_stays_accepted_and_is_not_resendable() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 1).unwrap();
        r.apply(SendEvent::PhoneAccepted, 2).unwrap();
        r.apply(SendEvent::ConnectionLost, 3).unwrap();
        assert_eq!(r.state(), SendState::AcceptedByPhone);
        assert!(!r.can_auto_send());
        assert!(matches!(
            r.resend(
                SendId(2),
                sub("b"),
                ResendConsent::UserAcceptsDuplicateRisk,
                4
            ),
            Err(TransitionError::ResendNotAllowed(_))
        ));
    }
    #[test]
    fn uncertain_resolves_from_later_phone_evidence() {
        for (e, want) in [
            (SendEvent::PhoneAccepted, SendState::AcceptedByPhone),
            (SendEvent::PhoneSent, SendState::Sent),
            (SendEvent::PhoneDelivered, SendState::Delivered),
            (
                SendEvent::PhoneFailed,
                SendState::Failed {
                    reason: FailureReason::PhoneReported,
                },
            ),
        ] {
            let mut r = record();
            r.apply(SendEvent::Dispatched, 1).unwrap();
            r.apply(SendEvent::ConnectionLost, 2).unwrap();
            r.apply(e, 3).unwrap();
            assert_eq!(r.state(), want);
        }
    }
    #[test]
    fn phone_evidence_for_never_dispatched_send_is_rejected() {
        let mut r = record();
        for e in [
            SendEvent::PhoneAccepted,
            SendEvent::PhoneSent,
            SendEvent::PhoneDelivered,
            SendEvent::PhoneFailed,
        ] {
            assert_eq!(r.apply(e, 1), Err(TransitionError::NeverDispatched));
        }
        assert_eq!(r.state(), SendState::Queued);
    }
    #[test]
    fn local_abort_only_before_dispatch() {
        let mut r = record();
        r.apply(SendEvent::LocalAbort, 1).unwrap();
        assert_eq!(
            r.state(),
            SendState::Failed {
                reason: FailureReason::LocalAbort
            }
        );
        let mut d = record();
        d.apply(SendEvent::Dispatched, 1).unwrap();
        assert_eq!(
            d.apply(SendEvent::LocalAbort, 2),
            Err(TransitionError::NotAbortable)
        );
    }
    #[test]
    fn failed_then_contradicting_success_is_a_conflict() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 1).unwrap();
        r.apply(SendEvent::PhoneFailed, 2).unwrap();
        assert_eq!(r.apply(SendEvent::PhoneAccepted, 3), Ok(Applied::Unchanged));
        assert!(matches!(
            r.apply(SendEvent::PhoneSent, 3),
            Err(TransitionError::Conflict { .. })
        ));
        assert!(matches!(
            r.apply(SendEvent::PhoneDelivered, 3),
            Err(TransitionError::Conflict { .. })
        ));
    }
    #[test]
    fn sent_can_later_fail_on_a_delivery_report() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 1).unwrap();
        r.apply(SendEvent::PhoneSent, 2).unwrap();
        r.apply(SendEvent::PhoneFailed, 3).unwrap();
        assert_eq!(
            r.state(),
            SendState::Failed {
                reason: FailureReason::PhoneReported
            }
        );
    }
    #[test]
    fn resend_needs_failed_or_uncertain_creates_new_record_and_is_idempotent() {
        let mut ledger = SendLedger::new();
        let Submit::Created(id) = ledger.submit(sub("s1"), intent("hi"), 1).unwrap() else {
            panic!()
        };
        let consent = ResendConsent::UserAcceptsDuplicateRisk;
        assert!(matches!(
            ledger.resend(id, sub("s2"), consent, 2),
            Err(ApplyError::Transition(TransitionError::ResendNotAllowed(
                SendState::Queued
            )))
        ));
        ledger.apply(id, SendEvent::Dispatched, 3).unwrap();
        ledger.connection_lost(4);
        let Submit::Created(new) = ledger.resend(id, sub("s2"), consent, 5).unwrap() else {
            panic!()
        };
        assert_ne!(new, id);
        assert_eq!(ledger.get(new).unwrap().resend_of, Some(id));
        assert_eq!(ledger.get(new).unwrap().state(), SendState::Queued);
        assert_eq!(
            ledger.get(id).unwrap().state(),
            SendState::Uncertain {
                cause: UncertainCause::ConnectionLost
            }
        );
        assert_eq!(
            ledger.resend(id, sub("s2"), consent, 6),
            Ok(Submit::Existing(new))
        );
        assert_eq!(ledger.len(), 2);
        assert!(matches!(
            ledger.resend(new, sub("s2"), consent, 7),
            Err(ApplyError::Store(StoreError::SubmissionConflict(_)))
        ));
        assert_eq!(
            ledger.apply(SendId(99), SendEvent::Dispatched, 1),
            Err(ApplyError::Store(StoreError::UnknownSend(SendId(99))))
        );
    }

    // --- submission idempotency -----------------------------------------------------------------

    #[test]
    fn submission_id_is_the_idempotency_key_not_the_content() {
        let mut ledger = SendLedger::new();
        let a = ledger.submit(sub("s1"), intent("ok"), 1).unwrap();
        assert_eq!(
            ledger.submit(sub("s1"), intent("ok"), 2).unwrap(),
            match a {
                Submit::Created(i) => Submit::Existing(i),
                _ => panic!(),
            }
        );
        // Same text under a new submission id is a deliberate second message.
        let b = ledger.submit(sub("s2"), intent("ok"), 3).unwrap();
        assert!(matches!(b, Submit::Created(_)) && b != a);
        // Same id with different content is a caller bug, not silently accepted.
        assert!(matches!(
            ledger.submit(sub("s1"), intent("different"), 4),
            Err(StoreError::SubmissionConflict(_))
        ));
        assert_eq!(ledger.len(), 2);
    }
    #[test]
    fn ledger_capacity_is_bounded() {
        let mut ledger = SendLedger::new();
        for i in 0..MAX_LEDGER_RECORDS {
            ledger
                .submit(sub(&format!("s{i}")), intent("x"), 1)
                .unwrap();
        }
        assert_eq!(
            ledger.submit(sub("over"), intent("x"), 1),
            Err(StoreError::LedgerFull)
        );
    }

    // --- correlation ----------------------------------------------------------------------------

    #[test]
    fn correlation_is_unique_ambiguous_or_none() {
        let r = SendRecord::new(SendId(1), sub("a"), intent("hello"), 0);
        let mine = msg(7, Transport::Sms, 10, MessageBox::Sent, 1000, "hello");
        let other_body = msg(7, Transport::Sms, 11, MessageBox::Sent, 1000, "bye");
        let incoming = msg(7, Transport::Sms, 12, MessageBox::Inbox, 1000, "hello");
        let old = msg(7, Transport::Sms, 13, MessageBox::Sent, 10, "hello");
        let draft = msg(7, Transport::Sms, 14, MessageBox::Draft, 1000, "hello");
        let mut stranger = msg(7, Transport::Sms, 15, MessageBox::Sent, 1000, "hello");
        stranger.addresses = vec![Address::new("+19998887777").unwrap()];
        let pool = [other_body, incoming, old, draft, stranger, mine.clone()];
        assert_eq!(r.correlate(pool.iter(), 500), Correlation::Unique(mine.key));
        assert_eq!(r.correlate(pool.iter(), 5000), Correlation::None);
        let twin = msg(7, Transport::Sms, 16, MessageBox::Outbox, 1200, "hello");
        let pool2 = [mine, twin];
        assert_eq!(r.correlate(pool2.iter(), 500), Correlation::Ambiguous(2));
    }
    #[test]
    fn message_evidence_drives_state_only_after_dispatch() {
        let mut r = record();
        let sent = msg(7, Transport::Sms, 1, MessageBox::Sent, 100, "hi");
        assert_eq!(
            r.apply_message_evidence(&sent, 1),
            Err(TransitionError::NeverDispatched)
        );
        r.apply(SendEvent::Dispatched, 2).unwrap();
        assert_eq!(r.apply_message_evidence(&sent, 3), Ok(Applied::Changed));
        assert_eq!(r.state(), SendState::Sent);
        assert_eq!(
            r.apply_message_evidence(&msg(7, Transport::Sms, 1, MessageBox::Draft, 100, "hi"), 4),
            Ok(Applied::Unchanged)
        );
    }
    #[test]
    fn send_state_serde_roundtrip() {
        for s in [
            SendState::Queued,
            SendState::AcceptedByPhone,
            SendState::Sent,
            SendState::Delivered,
            SendState::Failed {
                reason: FailureReason::LocalAbort,
            },
            SendState::Uncertain {
                cause: UncertainCause::AckTimeout,
            },
        ] {
            let back: SendState =
                serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back, s);
        }
        assert_eq!(
            serde_json::to_value(SendState::Uncertain {
                cause: UncertainCause::ConnectionLost
            })
            .unwrap(),
            json!({"state": "uncertain", "cause": "connection_lost"})
        );
    }
    #[test]
    fn send_record_roundtrips_and_rejects_invalid_intent() {
        let mut r = record();
        r.apply(SendEvent::Dispatched, 11).unwrap();
        let back: SendRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(
            serde_json::from_str::<SendRecord>(
                &serde_json::to_string(&r).unwrap().replace("\"hi\"", "\"\"")
            )
            .is_err()
        );
    }

    // --- segment counting, bounded threads, out-of-order arrival ---------------------------------

    #[test]
    fn gsm_text_counts_160_then_153_per_segment() {
        let one = sms_segments(&"a".repeat(160));
        assert_eq!(
            (one.encoding, one.segments, one.remaining),
            (SmsEncoding::Gsm7, 1, 0)
        );
        let two = sms_segments(&"a".repeat(161));
        assert_eq!(
            (two.segments, two.per_segment, two.remaining),
            (2, 153, 145)
        );
        assert_eq!(sms_segments(&"a".repeat(306)).segments, 2);
        assert_eq!(sms_segments(&"a".repeat(307)).segments, 3);
        assert_eq!(sms_segments("").segments, 0);
    }

    #[test]
    fn gsm_extension_characters_cost_two_and_never_split() {
        // 159 plain + one euro sign = 161 septets: two segments.
        let t = format!("{}€", "a".repeat(159));
        assert_eq!(sms_segments(&t).units, 161);
        assert_eq!(sms_segments(&t).segments, 2);
        // 152 plain + euro at the 153 boundary moves whole into the next segment.
        let t = format!("{}€{}", "a".repeat(152), "a".repeat(10));
        let i = sms_segments(&t);
        assert_eq!((i.encoding, i.segments), (SmsEncoding::Gsm7, 2));
        assert_eq!(i.remaining, 153 - 12);
    }

    #[test]
    fn one_emoji_makes_the_whole_message_ucs2_with_70_and_67() {
        let i = sms_segments("hi 😀");
        assert_eq!(
            (i.encoding, i.units, i.segments, i.per_segment),
            (SmsEncoding::Ucs2, 5, 1, 70)
        );
        assert_eq!(sms_segments(&"ж".repeat(70)).segments, 1);
        assert_eq!(sms_segments(&"ж".repeat(71)).segments, 2);
        assert_eq!(sms_segments(&"ж".repeat(134)).segments, 2);
        assert_eq!(sms_segments(&"ж".repeat(135)).segments, 3);
        // A surrogate pair is not split: 66 units + an emoji (2) overflows a 67 segment.
        let t = format!("{}😀", "ж".repeat(66 + 5));
        assert_eq!(sms_segments(&t).units, 71 + 2);
        let t = format!("{}😀xxx", "ж".repeat(66));
        let i = sms_segments(&t);
        assert_eq!(i.segments, 2);
        assert_eq!(i.remaining, 67 - 5);
    }

    #[test]
    fn accented_latin_in_the_gsm_set_stays_gsm_but_others_do_not() {
        assert_eq!(sms_segments("café ñandu àèé").encoding, SmsEncoding::Gsm7);
        assert_eq!(sms_segments("çà").encoding, SmsEncoding::Ucs2); // lowercase c-cedilla is not in GSM
        assert_eq!(sms_segments("naïve").encoding, SmsEncoding::Ucs2);
    }

    #[test]
    fn trim_oldest_keeps_the_newest_and_bounds_memory() {
        let mut c = Conversation::new(ThreadId(7));
        for i in 0..10 {
            c.merge(sms(i, 1000 + i as u64)).unwrap();
        }
        assert_eq!(c.trim_oldest(4), 6);
        assert_eq!(c.len(), 4);
        assert_eq!(c.oldest_date_ms(), Some(1006));
        assert_eq!(c.newest_date_ms(), Some(1009));
        assert_eq!(c.trim_oldest(100), 0);
    }

    #[test]
    fn out_of_order_and_duplicate_delivery_converge_to_the_same_thread() {
        let batch: Vec<Message> = vec![
            sms(3, 300),
            sms(1, 100),
            sms(2, 200),
            sms(2, 200),
            sms(3, 300),
        ];
        let mut a = Conversation::new(ThreadId(7));
        let s = a.merge_all(batch.clone());
        assert_eq!((s.inserted, s.duplicates), (3, 2));
        let mut b = Conversation::new(ThreadId(7));
        b.merge_all(batch.into_iter().rev());
        let dates = |c: &Conversation| c.ordered().iter().map(|m| m.date_ms).collect::<Vec<_>>();
        assert_eq!(dates(&a), [100, 200, 300]);
        assert_eq!(dates(&a), dates(&b));
        // A late status update replaces the message instead of adding a second one.
        let mut late = sms(2, 200);
        late.read = false;
        assert_eq!(a.merge(late).unwrap(), MergeOutcome::Updated);
        assert_eq!(a.len(), 3);
    }
}
