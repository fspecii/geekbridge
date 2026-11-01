//! Bounded, content-bearing views of phone data that the service hands to the Agenda and Messages
//! windows over the authenticated control channel (`Request::AgendaView` / `Request::MessagesView`).
//!
//! These carry personal data (names, numbers, event text, message bodies) and exist only in memory
//! on both sides. Each list is capped so a reply always fits `ipc::MAX_RESPONSE_BYTES`.
use serde::{Deserialize, Serialize};

pub const MAX_PEOPLE_SHOWN: usize = 300;
pub const MAX_EVENTS_SHOWN: usize = 400;
pub const MAX_PEOPLE_LIMIT: usize = 1000;
pub const MAX_VIEW_DAYS: u32 = 56;
pub const MAX_EDITS_SHOWN: usize = 20;
pub const MAX_CONVERSATIONS_SHOWN: usize = 100;
pub const MAX_MESSAGES_SHOWN: usize = 100;
pub const MAX_SENDS_SHOWN: usize = 20;

/// Where one phone feature stands for one paired device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataState {
    /// The per-device consent is off (the default). Nothing is requested.
    #[default]
    ConsentOff,
    /// The phone is not connected.
    Offline,
    /// Connected and consent is on, but the phone does not offer this feature (not enabled in the
    /// phone app, permission missing, or a build without it).
    NotOffered,
    /// Requested; waiting for the phone's first answer.
    Waiting,
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDevice {
    pub device_id: String,
    pub name: String,
    pub contacts: DataState,
    pub calendar: DataState,
    pub sms: DataState,
    /// Whether this phone may be asked to change its calendar (own consent, own phone capability).
    #[serde(default)]
    pub calendar_edit: DataState,
    pub contacts_count: usize,
    pub events_count: usize,
    pub calendar_names: Vec<String>,
    /// The phone only creates events in calendars it lists as writable; false = phone is read-only.
    #[serde(default)]
    pub calendar_writable: bool,
    pub calendar_truncated: bool,
    pub contacts_synced_unix_ms: Option<u64>,
    pub calendar_synced_unix_ms: Option<u64>,
    /// The phone sends `geekbridge.sms.status`, so sent/delivered are real signals. Stock KDE
    /// Connect phones never do.
    pub sms_status_reports: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub device_id: String,
    pub id: String,
    pub name: String,
    pub phones: Vec<String>,
    pub emails: Vec<String>,
    pub organization: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventItem {
    pub device_id: String,
    pub id: String,
    pub title: String,
    pub all_day: bool,
    /// UTC milliseconds. All-day events carry their nominal UTC midnights (end exclusive).
    pub start_ms: i64,
    pub end_ms: i64,
    pub location: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub calendar_id: Option<String>,
    /// The phone's `Events._ID`; present when the event can be named in an edit.
    #[serde(default)]
    pub event_id: Option<String>,
    /// Belongs to a recurring series: shown, but edits are not offered from the desktop.
    #[serde(default)]
    pub recurring: bool,
    /// Event time zone name as the phone stored it (display only).
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub reminders_min: Vec<u32>,
    #[serde(default)]
    pub attendee_count: u32,
    #[serde(default)]
    pub attendees: Vec<String>,
}

/// One calendar the phone exposes (Google accounts that sync on the phone appear here).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarItem {
    pub device_id: String,
    pub id: String,
    pub name: String,
    pub account: Option<String>,
    pub account_type: Option<String>,
    /// ARGB.
    pub color: Option<u32>,
    pub writable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarEditAction {
    Create,
    Update,
    Delete,
}

/// What the user typed in the event editor. All-day events use UTC midnights (end exclusive), the
/// way Android stores them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDraft {
    pub title: String,
    pub all_day: bool,
    pub start_ms: i64,
    pub end_ms: i64,
    /// IANA zone the times were entered in (ignored for all-day events).
    pub tz: String,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// One reminder, minutes before the start.
    #[serde(default)]
    pub reminder_min: Option<u32>,
}

/// Progress of one edit. The phone's own answer is the only thing that reaches `Applied`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditPhase {
    /// Held here: the phone is not connected.
    Queued,
    /// Written to the link; no answer from the phone yet.
    HandedToPhone,
    /// The phone wrote it to its calendar provider; Google sync on the phone does the rest.
    Applied,
    Failed,
    /// The link dropped or the phone did not answer; it may or may not have been applied.
    Uncertain,
}

/// Plain-language text for a phone-reported failure code.
pub fn edit_reason_text(code: &str) -> &'static str {
    match code {
        "disabled" => "Editing is switched off in the GeekBridge app on the phone.",
        "no_permission" => "Android has not granted calendar write access to GeekBridge.",
        "not_writable" => "That calendar is read-only on the phone.",
        "not_found" => "The event no longer exists on the phone.",
        "recurring" => "Repeating events cannot be changed from the computer.",
        "invalid" => "The phone rejected the event details.",
        "calendar_missing" => "That calendar is not available on the phone.",
        _ => "The phone could not write the change.",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditItem {
    pub op_id: String,
    pub device_id: String,
    pub action: CalendarEditAction,
    pub title: String,
    pub phase: EditPhase,
    pub reason: Option<String>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgendaView {
    pub devices: Vec<DataDevice>,
    pub people: Vec<Person>,
    pub people_total: usize,
    pub events: Vec<EventItem>,
    #[serde(default)]
    pub calendars: Vec<CalendarItem>,
    #[serde(default)]
    pub edits: Vec<EditItem>,
    /// More events matched than fit this reply.
    #[serde(default)]
    pub events_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationItem {
    pub device_id: String,
    pub thread_id: i64,
    pub addresses: Vec<String>,
    /// Contact name when the phone's contacts are shared and one matches, else the first address.
    pub title: String,
    pub last_body: String,
    pub last_date_ms: u64,
    pub last_outgoing: bool,
    pub unread: u32,
    pub message_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageItem {
    pub uid: i64,
    pub mms: bool,
    pub outgoing: bool,
    pub body: String,
    pub date_ms: u64,
    pub attachments: u32,
}

/// Send progress. The phone's own signal is the only thing that can raise it above `HandedToPhone`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendPhase {
    /// Held here; nothing has left this computer (phone offline or still starting).
    Queued,
    /// The request was written to the link. No confirmation from the phone yet.
    HandedToPhone,
    /// The phone confirmed it took the message and is sending it.
    AcceptedByPhone,
    /// Android reports the message left the phone. Not proof the recipient has it.
    Sent,
    /// The carrier returned a delivery report.
    Delivered,
    Failed,
    /// The link dropped after the request left; it may or may not have been sent.
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendItem {
    pub send_id: u64,
    pub device_id: String,
    pub recipient: String,
    pub text: String,
    pub phase: SendPhase,
    /// Short phone-reported code for `Failed`.
    pub reason: Option<String>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagesView {
    pub devices: Vec<DataDevice>,
    pub conversations: Vec<ConversationItem>,
    pub selected: Option<SelectedThread>,
    pub sends: Vec<SendItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedThread {
    pub device_id: String,
    pub thread_id: i64,
    /// Newest last.
    pub messages: Vec<MessageItem>,
    /// Older history may exist on the phone.
    pub may_have_older: bool,
}
