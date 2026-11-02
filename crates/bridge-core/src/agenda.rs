// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only contacts and calendar domain for the Agenda app.
//!
//! Pure data and validation: no I/O, no pairing, no phone access, no new dependencies. Records
//! come from a phone-side source (KDE contacts today; the calendar side has no existing protocol,
//! see `docs/phone-data-link.md`) and are never written back.
//!
//! Conventions:
//! - [`SourceId`] and [`Revision`] are opaque. Never use an id as a file name or compare
//!   revisions with `<`; revisions only support equality (KDE uses `CONTACT_LAST_UPDATED_TIMESTAMP`
//!   that way, and Android calendar rows have no modified timestamp at all).
//! - All-day events are floating calendar dates with an exclusive end (RFC 5545 `VALUE=DATE`,
//!   Android `ALL_DAY=1`). Timed events are UTC instants plus an IANA zone name used only for display.
//! - Recurrence is carried as raw RFC 5545 metadata and is **not** expanded here. Day views skip
//!   series masters; the phone should supply expanded occurrences (Android `Instances`) as events.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub const MAX_RECORDS: usize = 100_000;
pub const MAX_VCARD_BYTES: usize = 512 * 1024;
const DAY_MS: i64 = 86_400_000;
const MIN_EPOCH_DAY: i64 = -719_162; // 0001-01-01
const MAX_EPOCH_DAY: i64 = 2_932_896; // 9999-12-31

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgendaError {
    InvalidDate,
    InvalidRange(&'static str),
    InvalidField(&'static str),
    DuplicateId(SourceId),
    /// `base` is the revision the sender believed was stored; `current` is what actually is.
    Conflict {
        id: SourceId,
        base: Option<Revision>,
        current: Option<Revision>,
    },
    /// Same revision token as the stored record but different content.
    StaleRevision(SourceId),
    LimitExceeded,
}
impl fmt::Display for AgendaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDate => f.write_str("invalid calendar date"),
            Self::InvalidRange(why) => write!(f, "invalid time range: {why}"),
            Self::InvalidField(name) => write!(f, "invalid field: {name}"),
            Self::DuplicateId(id) => write!(f, "duplicate id {}/{}", id.source, id.id),
            Self::Conflict { id, .. } => write!(f, "revision conflict for {}/{}", id.source, id.id),
            Self::StaleRevision(id) => write!(
                f,
                "content changed without a new revision for {}/{}",
                id.source, id.id
            ),
            Self::LimitExceeded => f.write_str("too many agenda records"),
        }
    }
}
impl std::error::Error for AgendaError {}
type Result<T> = std::result::Result<T, AgendaError>;

pub trait Validate {
    fn validate(&self) -> Result<()>;
}

fn text(field: &'static str, s: &str, max: usize, multiline: bool, required: bool) -> Result<()> {
    let bad = (required && s.trim().is_empty())
        || s.len() > max
        || s.chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')));
    if bad {
        Err(AgendaError::InvalidField(field))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------- dates

pub fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
pub fn days_in_month(year: i32, month: u8) -> Option<u8> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => return None,
    })
}

/// Proleptic Gregorian date, years 1..=9999. Serialized as `YYYY-MM-DD`; impossible dates are
/// rejected on deserialization too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}
impl Date {
    pub fn new(year: i32, month: u8, day: u8) -> Result<Self> {
        match days_in_month(year, month) {
            Some(max) if (1..=9999).contains(&year) && (1..=max).contains(&day) => {
                Ok(Self { year, month, day })
            }
            _ => Err(AgendaError::InvalidDate),
        }
    }
    pub fn year(self) -> i32 {
        self.year
    }
    pub fn month(self) -> u8 {
        self.month
    }
    pub fn day(self) -> u8 {
        self.day
    }
    /// Days since 1970-01-01 (negative before).
    pub fn epoch_day(self) -> i64 {
        let (y, m, d) = (
            self.year as i64 - (self.month <= 2) as i64,
            self.month as i64,
            self.day as i64,
        );
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
    }
    pub fn from_epoch_day(days: i64) -> Result<Self> {
        if !(MIN_EPOCH_DAY..=MAX_EPOCH_DAY).contains(&days) {
            return Err(AgendaError::InvalidDate);
        }
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        Self::new(
            (yoe + era * 400 + (m <= 2) as i64) as i32,
            m as u8,
            (doy - (153 * mp + 2) / 5 + 1) as u8,
        )
    }
}
impl std::str::FromStr for Date {
    type Err = AgendaError;
    fn from_str(s: &str) -> Result<Self> {
        let b = s.as_bytes();
        let digits = |r: std::ops::Range<usize>| {
            b[r].iter().try_fold(0i32, |n, c| {
                c.is_ascii_digit().then(|| n * 10 + (c - b'0') as i32)
            })
        };
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return Err(AgendaError::InvalidDate);
        }
        match (digits(0..4), digits(5..7), digits(8..10)) {
            (Some(y), Some(m), Some(d)) => Self::new(y, m as u8, d as u8),
            _ => Err(AgendaError::InvalidDate),
        }
    }
}
impl TryFrom<String> for Date {
    type Error = AgendaError;
    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}
impl From<Date> for String {
    fn from(d: Date) -> String {
        d.to_string()
    }
}
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

// ---------------------------------------------------------------- identity and revisions

/// `source` names the provider scope (for example `android:<device-id>:contacts`); `id` is the
/// provider's own key (contact `LOOKUP_KEY`, calendar `Events._ID`). Both are opaque.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId {
    pub source: String,
    pub id: String,
}
impl SourceId {
    pub fn new(source: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            id: id.into(),
        }
    }
}
impl Validate for SourceId {
    fn validate(&self) -> Result<()> {
        text("source", &self.source, 128, false, true)?;
        text("id", &self.id, 512, false, true)
    }
}

/// Opaque change token, compared for equality only.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Revision(pub String);
impl Validate for Revision {
    fn validate(&self) -> Result<()> {
        text("revision", &self.0, 128, false, true)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State<T> {
    Live(T),
    /// `deleted_unix_ms` is `None` when the source did not say when (Android's
    /// `DeletedContacts` keeps it for 30 days; absence-based deletion has none).
    Tombstone {
        deleted_unix_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record<T> {
    pub id: SourceId,
    pub revision: Revision,
    pub state: State<T>,
}
impl<T> Record<T> {
    pub fn live(id: SourceId, revision: Revision, item: T) -> Self {
        Self {
            id,
            revision,
            state: State::Live(item),
        }
    }
    pub fn tombstone(id: SourceId, revision: Revision, deleted_unix_ms: Option<u64>) -> Self {
        Self {
            id,
            revision,
            state: State::Tombstone { deleted_unix_ms },
        }
    }
    pub fn item(&self) -> Option<&T> {
        match &self.state {
            State::Live(t) => Some(t),
            State::Tombstone { .. } => None,
        }
    }
}
impl<T: Validate> Validate for Record<T> {
    fn validate(&self) -> Result<()> {
        self.id.validate()?;
        self.revision.validate()?;
        self.item().map_or(Ok(()), T::validate)
    }
}

// ---------------------------------------------------------------- contacts

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub display_name: String,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub organization: Option<String>,
    /// Raw vCard as sent by the phone, kept for lossless display. Strip photos before storing.
    #[serde(default)]
    pub vcard: Option<String>,
}
impl Validate for Contact {
    fn validate(&self) -> Result<()> {
        text("display_name", &self.display_name, 256, false, false)?;
        if self.phones.len() > 64 || self.emails.len() > 64 {
            return Err(AgendaError::InvalidField("contact points"));
        }
        for p in &self.phones {
            text("phone", p, 64, false, true)?;
        }
        for e in &self.emails {
            text("email", e, 320, false, true)?;
        }
        if let Some(o) = &self.organization {
            text("organization", o, 256, false, false)?;
        }
        if self.display_name.trim().is_empty() && self.phones.is_empty() && self.emails.is_empty() {
            return Err(AgendaError::InvalidField(
                "contact needs a name, phone or email",
            ));
        }
        match &self.vcard {
            Some(v)
                if v.len() > MAX_VCARD_BYTES
                    || !v.trim_start().starts_with("BEGIN:VCARD")
                    || !v.contains("END:VCARD") =>
            {
                Err(AgendaError::InvalidField("vcard"))
            }
            _ => Ok(()),
        }
    }
}

/// The phone-side revision stamp in a KDE Connect vCard: the last `REV:` line. Android appends
/// `REV:<CONTACT_LAST_UPDATED_TIMESTAMP>` just before `END:VCARD`; it does not emit the
/// `X-KDECONNECT-TIMESTAMP` line that kdeconnect-kde's desktop plugin looks for.
pub fn vcard_rev(vcard: &str) -> Option<&str> {
    vcard.lines().rev().find_map(|l| {
        let l = l.trim_end();
        (l.len() > 4 && l.is_char_boundary(4) && l[..4].eq_ignore_ascii_case("REV:"))
            .then(|| l[4..].trim())
    })
}

// ---------------------------------------------------------------- calendar

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum When {
    /// `end_exclusive` is the first day *not* covered (a one-day event ends the next day).
    AllDay { start: Date, end_exclusive: Date },
    /// UTC instants in milliseconds; `end_utc_ms` is exclusive. `time_zone` is display metadata.
    Timed {
        start_utc_ms: i64,
        end_utc_ms: i64,
        time_zone: String,
    },
}
impl When {
    /// Maps Android `Events.DTSTART/DTEND/ALL_DAY/EVENT_TIMEZONE`. All-day rows must sit on UTC
    /// midnights (Android's documented write rule); `time_zone` is ignored for them.
    pub fn from_android(
        dtstart_ms: i64,
        dtend_ms: i64,
        all_day: bool,
        time_zone: &str,
    ) -> Result<Self> {
        let when = if all_day {
            if dtstart_ms.rem_euclid(DAY_MS) != 0 || dtend_ms.rem_euclid(DAY_MS) != 0 {
                return Err(AgendaError::InvalidRange("all-day bounds are not midnight"));
            }
            Self::AllDay {
                start: Date::from_epoch_day(dtstart_ms / DAY_MS)?,
                end_exclusive: Date::from_epoch_day(dtend_ms / DAY_MS)?,
            }
        } else {
            Self::Timed {
                start_utc_ms: dtstart_ms,
                end_utc_ms: dtend_ms,
                time_zone: time_zone.to_owned(),
            }
        };
        when.validate()?;
        Ok(when)
    }
    /// Sort key in UTC milliseconds (all-day events use their nominal UTC midnight).
    pub fn start_ms(&self) -> i64 {
        match self {
            Self::AllDay { start, .. } => start.epoch_day() * DAY_MS,
            Self::Timed { start_utc_ms, .. } => *start_utc_ms,
        }
    }
    /// Whether the event touches any day of `[first, first + days)` as seen at `utc_offset_minutes`.
    pub fn overlaps_days(&self, first: Date, days: u32, utc_offset_minutes: i32) -> bool {
        let from = first.epoch_day();
        let to = from + i64::from(days.max(1));
        match self {
            Self::AllDay {
                start,
                end_exclusive,
            } => start.epoch_day() < to && end_exclusive.epoch_day() > from,
            Self::Timed {
                start_utc_ms,
                end_utc_ms,
                ..
            } => {
                let shift = i64::from(utc_offset_minutes) * 60_000;
                start_utc_ms + shift < to * DAY_MS && end_utc_ms + shift > from * DAY_MS
            }
        }
    }
    /// Whether the event touches `day` as seen at `utc_offset_minutes` (the viewer's offset on
    /// that day; this crate has no tz database). All-day events ignore the offset.
    pub fn covers_day(&self, day: Date, utc_offset_minutes: i32) -> bool {
        match self {
            Self::AllDay {
                start,
                end_exclusive,
            } => *start <= day && day < *end_exclusive,
            Self::Timed {
                start_utc_ms,
                end_utc_ms,
                ..
            } => {
                let shift = utc_offset_minutes as i64 * 60_000;
                let day_start = day.epoch_day() * DAY_MS;
                start_utc_ms + shift < day_start + DAY_MS && end_utc_ms + shift > day_start
            }
        }
    }
}
impl Validate for When {
    fn validate(&self) -> Result<()> {
        match self {
            Self::AllDay {
                start,
                end_exclusive,
            } if end_exclusive <= start => {
                Err(AgendaError::InvalidRange("all-day end must be after start"))
            }
            Self::AllDay { .. } => Ok(()),
            Self::Timed {
                start_utc_ms,
                end_utc_ms,
                time_zone,
            } => {
                let (lo, hi) = (MIN_EPOCH_DAY * DAY_MS, (MAX_EPOCH_DAY + 1) * DAY_MS);
                if end_utc_ms <= start_utc_ms {
                    return Err(AgendaError::InvalidRange("timed end must be after start"));
                }
                if *start_utc_ms < lo || *end_utc_ms > hi {
                    return Err(AgendaError::InvalidRange("outside years 1-9999"));
                }
                // Shape only: this crate cannot know whether the zone exists in the viewer's tzdb.
                let shape_ok = time_zone.len() <= 64
                    && !time_zone.is_empty()
                    && !time_zone.contains("//")
                    && !time_zone.contains("..")
                    && !time_zone.starts_with('/')
                    && !time_zone.ends_with('/')
                    && time_zone
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_+-/:".contains(c));
                if shape_ok {
                    Ok(())
                } else {
                    Err(AgendaError::InvalidField("time_zone"))
                }
            }
        }
    }
}

/// Raw RFC 5545 recurrence metadata (Android `RRULE`/`RDATE`/`EXDATE`, without property names).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recurrence {
    Series {
        rrule: Option<String>,
        #[serde(default)]
        rdates: Vec<String>,
        #[serde(default)]
        exdates: Vec<String>,
    },
    /// A single changed occurrence; like Android, exceptions cannot recur themselves.
    Exception {
        master_id: String,
        original_start_utc_ms: i64,
    },
}
impl Validate for Recurrence {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Series {
                rrule,
                rdates,
                exdates,
            } => {
                if rrule.is_none() && rdates.is_empty() {
                    return Err(AgendaError::InvalidField("series needs rrule or rdate"));
                }
                if let Some(r) = rrule {
                    text("rrule", r, 512, false, true)?;
                    if !r.to_ascii_uppercase().contains("FREQ=") {
                        return Err(AgendaError::InvalidField("rrule FREQ"));
                    }
                }
                if rdates.len() > 256 || exdates.len() > 256 {
                    return Err(AgendaError::InvalidField("rdate/exdate count"));
                }
                rdates
                    .iter()
                    .chain(exdates)
                    .try_for_each(|d| text("rdate/exdate", d, 128, false, true))
            }
            Self::Exception { master_id, .. } => text("master_id", master_id, 512, false, true),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub title: String,
    pub when: When,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub recurrence: Option<Recurrence>,
    /// Calendar, reminder and attendee details of a phone event (all optional on the wire).
    #[serde(default)]
    pub meta: EventMeta,
}

/// Extra, display-only details of one calendar instance. All bounded by [`Validate`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMeta {
    /// The phone's calendar this instance belongs to (`Calendars._ID`).
    #[serde(default)]
    pub calendar_id: Option<String>,
    /// The phone's `Events._ID`, the handle an edit or delete refers to.
    #[serde(default)]
    pub event_id: Option<String>,
    /// The instance belongs to a recurring series (it cannot be edited from the desktop).
    #[serde(default)]
    pub recurring: bool,
    /// Reminders in minutes before the start.
    #[serde(default)]
    pub reminders_min: Vec<u32>,
    #[serde(default)]
    pub attendee_count: u32,
    /// Names (or addresses) of the first few attendees.
    #[serde(default)]
    pub attendees: Vec<String>,
}
pub const MAX_REMINDERS: usize = 5;
pub const MAX_ATTENDEE_NAMES: usize = 6;
impl Validate for EventMeta {
    fn validate(&self) -> Result<()> {
        for id in [&self.calendar_id, &self.event_id].into_iter().flatten() {
            text("id", id, 64, false, true)?;
        }
        if self.reminders_min.len() > MAX_REMINDERS || self.attendees.len() > MAX_ATTENDEE_NAMES {
            return Err(AgendaError::InvalidField("meta count"));
        }
        self.attendees
            .iter()
            .try_for_each(|a| text("attendee", a, 128, false, false))
    }
}
impl Validate for CalendarEvent {
    fn validate(&self) -> Result<()> {
        text("title", &self.title, 512, false, false)?;
        if let Some(l) = &self.location {
            text("location", l, 512, false, false)?;
        }
        if let Some(d) = &self.description {
            text("description", d, 16 * 1024, true, false)?;
        }
        self.when.validate()?;
        self.meta.validate()?;
        self.recurrence
            .as_ref()
            .map_or(Ok(()), Recurrence::validate)
    }
}

// ---------------------------------------------------------------- store

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Inserted,
    Updated,
    Deleted,
    Unchanged,
}

/// What to do after comparing a phone's id/revision list with the store (KDE's
/// `response_uids_timestamps` handshake).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// Ids that are new or whose revision differs: request their full records.
    pub fetch: Vec<SourceId>,
    /// Live local ids the phone no longer lists: call [`Store::mark_deleted`].
    pub absent: Vec<SourceId>,
}

/// In-memory, validated, optimistic-concurrency store of one record kind.
#[derive(Clone, Debug)]
pub struct Store<T> {
    records: BTreeMap<SourceId, Record<T>>,
}
impl<T> Default for Store<T> {
    fn default() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }
}
impl<T: Validate + PartialEq + Clone> Store<T> {
    pub fn get(&self, id: &SourceId) -> Option<&Record<T>> {
        self.records.get(id)
    }
    /// Live records only, in id order.
    pub fn live(&self) -> impl Iterator<Item = (&Record<T>, &T)> {
        self.records
            .values()
            .filter_map(|r| r.item().map(|t| (r, t)))
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Inserts or replaces a record. `base` is the revision the sender last saw for this id
    /// (`None` for "never seen"); it must match what is stored, else [`AgendaError::Conflict`].
    /// Replaying the identical record is a no-op, and a changed record must carry a new revision.
    pub fn apply(&mut self, record: Record<T>, base: Option<&Revision>) -> Result<Applied> {
        record.validate()?;
        let current = self.records.get(&record.id);
        if current == Some(&record) {
            return Ok(Applied::Unchanged);
        }
        if current.map(|c| &c.revision) != base {
            return Err(AgendaError::Conflict {
                id: record.id,
                base: base.cloned(),
                current: current.map(|c| c.revision.clone()),
            });
        }
        if current.is_some_and(|c| c.revision == record.revision) {
            return Err(AgendaError::StaleRevision(record.id));
        }
        if current.is_none() && self.records.len() >= MAX_RECORDS {
            return Err(AgendaError::LimitExceeded);
        }
        let outcome = match (&record.state, current) {
            (State::Tombstone { .. }, _) => Applied::Deleted,
            (_, None) => Applied::Inserted,
            _ => Applied::Updated,
        };
        self.records.insert(record.id.clone(), record);
        Ok(outcome)
    }

    /// Compares the phone's `(id, revision)` list with the store.
    pub fn plan_sync(&self, remote: &[(SourceId, Revision)]) -> Result<SyncPlan> {
        if remote.len() > MAX_RECORDS {
            return Err(AgendaError::LimitExceeded);
        }
        let mut seen = BTreeSet::new();
        let mut plan = SyncPlan::default();
        for (id, rev) in remote {
            id.validate()?;
            rev.validate()?;
            if !seen.insert(id) {
                return Err(AgendaError::DuplicateId(id.clone()));
            }
            if self.records.get(id).map(|r| &r.revision) != Some(rev) {
                plan.fetch.push(id.clone());
            }
        }
        plan.absent = self
            .live()
            .map(|(r, _)| &r.id)
            .filter(|id| !seen.contains(id))
            .cloned()
            .collect();
        Ok(plan)
    }

    /// Locally tombstones a live record (the phone stopped listing it). Absent or already
    /// deleted ids are left alone.
    pub fn mark_deleted(&mut self, id: &SourceId, now_unix_ms: u64) -> Result<Applied> {
        match self.records.get(id) {
            Some(r) if r.item().is_some() => {
                let base = r.revision.clone();
                self.apply(
                    Record::tombstone(
                        id.clone(),
                        Revision(format!("gb-deleted:{now_unix_ms}")),
                        Some(now_unix_ms),
                    ),
                    Some(&base),
                )
            }
            _ => Ok(Applied::Unchanged),
        }
    }

    /// Drops tombstones deleted before `cutoff_unix_ms`; tombstones without a time are kept.
    pub fn prune_tombstones(&mut self, cutoff_unix_ms: u64) -> usize {
        let before = self.records.len();
        self.records.retain(|_, r| !matches!(r.state, State::Tombstone { deleted_unix_ms: Some(t) } if t < cutoff_unix_ms));
        before - self.records.len()
    }
}

/// Backing data for the Agenda app's People and Day views.
#[derive(Clone, Debug, Default)]
pub struct Agenda {
    pub contacts: Store<Contact>,
    pub events: Store<CalendarEvent>,
}
impl Agenda {
    /// People view: live contacts matching `query` (name, organization, email substring, or three
    /// or more phone digits), sorted by case-insensitive name. Empty query lists everyone.
    pub fn people(&self, query: &str) -> Vec<&Record<Contact>> {
        let (q, q_digits) = (
            crate::collate::fold(query.trim()),
            query
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>(),
        );
        let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
        let mut out: Vec<_> = self
            .contacts
            .live()
            .filter(|(_, c)| {
                q.is_empty()
                    || [Some(&c.display_name), c.organization.as_ref()]
                        .into_iter()
                        .flatten()
                        .chain(&c.emails)
                        .any(|s| crate::collate::fold(s).contains(&q))
                    || (q_digits.len() >= 3
                        && c.phones.iter().any(|p| digits(p).contains(&q_digits)))
            })
            .collect();
        out.sort_by_cached_key(|(r, c)| (crate::collate::key(&c.display_name), r.id.clone()));
        out.into_iter().map(|(r, _)| r).collect()
    }

    /// Day view: all-day events first, then by start. Series masters are skipped (not expanded).
    pub fn day(&self, day: Date, utc_offset_minutes: i32) -> Vec<&Record<CalendarEvent>> {
        self.range(day, 1, utc_offset_minutes)
    }

    /// Events touching any of `days` days from `first` (month / week / list views), all-day first
    /// within a start. Series masters are skipped; the phone already expands recurrences.
    pub fn range(
        &self,
        first: Date,
        days: u32,
        utc_offset_minutes: i32,
    ) -> Vec<&Record<CalendarEvent>> {
        let mut out: Vec<_> = self
            .events
            .live()
            .filter(|(_, e)| {
                !matches!(e.recurrence, Some(Recurrence::Series { .. }))
                    && e.when.overlaps_days(first, days, utc_offset_minutes)
            })
            .collect();
        out.sort_by_cached_key(|(r, e)| {
            (
                matches!(e.when, When::Timed { .. }),
                e.when.start_ms(),
                e.title.to_lowercase(),
                r.id.clone(),
            )
        });
        out.into_iter().map(|(r, _)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u8, day: u8) -> Date {
        Date::new(y, m, day).unwrap()
    }
    fn sid(id: &str) -> SourceId {
        SourceId::new("android:phone:contacts", id)
    }
    fn rev(s: &str) -> Revision {
        Revision(s.into())
    }
    fn person(name: &str) -> Contact {
        Contact {
            display_name: name.into(),
            ..Default::default()
        }
    }
    fn live(id: &str, r: &str, name: &str) -> Record<Contact> {
        Record::live(sid(id), rev(r), person(name))
    }
    fn event(title: &str, when: When) -> CalendarEvent {
        CalendarEvent {
            title: title.into(),
            when,
            location: None,
            description: None,
            recurrence: None,
            meta: EventMeta::default(),
        }
    }
    fn all_day(s: Date, e: Date) -> When {
        When::AllDay {
            start: s,
            end_exclusive: e,
        }
    }
    fn timed(s: i64, e: i64) -> When {
        When::Timed {
            start_utc_ms: s,
            end_utc_ms: e,
            time_zone: "Europe/Berlin".into(),
        }
    }

    #[test]
    fn leap_years_and_month_lengths() {
        assert!([1996, 2000, 2024, 2400].iter().all(|&y| is_leap_year(y)));
        assert!([1900, 2023, 2100, 2200].iter().all(|&y| !is_leap_year(y)));
        assert_eq!(days_in_month(2024, 2), Some(29));
        assert_eq!(days_in_month(1900, 2), Some(28));
        assert_eq!(days_in_month(2024, 4), Some(30));
        assert_eq!(days_in_month(2024, 13), None);
    }

    #[test]
    fn rejects_impossible_dates() {
        assert!(Date::new(2024, 2, 29).is_ok());
        for (y, m, day) in [
            (2023, 2, 29),
            (1900, 2, 29),
            (2100, 2, 29),
            (2024, 4, 31),
            (2024, 0, 1),
            (2024, 13, 1),
            (2024, 1, 0),
            (2024, 1, 32),
            (0, 1, 1),
            (10_000, 1, 1),
            (-5, 6, 1),
        ] {
            assert_eq!(
                Date::new(y, m, day),
                Err(AgendaError::InvalidDate),
                "{y}-{m}-{day}"
            );
        }
        for s in [
            "2023-02-29",
            "2024-2-09",
            " 2024-02-29",
            "2024-02-29 ",
            "2024/02/29",
            "2024-02-3x",
            "+024-02-29",
            "２０２４-02-29",
            "",
        ] {
            assert!(s.parse::<Date>().is_err(), "{s:?}");
        }
        assert_eq!("2024-02-29".parse::<Date>().unwrap(), d(2024, 2, 29));
    }

    #[test]
    fn epoch_days_round_trip_over_the_whole_range() {
        assert_eq!(d(1970, 1, 1).epoch_day(), 0);
        assert_eq!(d(1969, 12, 31).epoch_day(), -1);
        assert_eq!(d(2000, 3, 1).epoch_day(), 11_017);
        assert_eq!(d(1, 1, 1).epoch_day(), MIN_EPOCH_DAY);
        assert_eq!(d(9999, 12, 31).epoch_day(), MAX_EPOCH_DAY);
        for day in MIN_EPOCH_DAY..=MAX_EPOCH_DAY {
            let date = Date::from_epoch_day(day).unwrap();
            assert_eq!(date.epoch_day(), day);
        }
        assert!(Date::from_epoch_day(MIN_EPOCH_DAY - 1).is_err());
        assert!(Date::from_epoch_day(MAX_EPOCH_DAY + 1).is_err());
        assert_eq!(
            Date::from_epoch_day(d(2024, 2, 28).epoch_day() + 1).unwrap(),
            d(2024, 2, 29)
        );
        assert_eq!(
            Date::from_epoch_day(d(2023, 2, 28).epoch_day() + 1).unwrap(),
            d(2023, 3, 1)
        );
    }

    #[test]
    fn date_serde_is_iso_and_validating() {
        assert_eq!(
            serde_json::to_string(&d(2024, 2, 9)).unwrap(),
            "\"2024-02-09\""
        );
        assert_eq!(
            serde_json::from_str::<Date>("\"2024-02-29\"").unwrap(),
            d(2024, 2, 29)
        );
        assert!(serde_json::from_str::<Date>("\"2023-02-29\"").is_err());
        let bad = r#"{"kind":"all_day","start":"2023-02-30","end_exclusive":"2023-03-01"}"#;
        assert!(serde_json::from_str::<When>(bad).is_err());
    }

    #[test]
    fn all_day_end_is_exclusive() {
        let one_day = all_day(d(2024, 2, 28), d(2024, 2, 29));
        assert!(one_day.validate().is_ok());
        assert!(one_day.covers_day(d(2024, 2, 28), 0));
        assert!(!one_day.covers_day(d(2024, 2, 29), 0));
        assert!(!one_day.covers_day(d(2024, 2, 27), 0));
        // The offset never moves a floating all-day event.
        assert!(
            one_day.covers_day(d(2024, 2, 28), 14 * 60)
                && one_day.covers_day(d(2024, 2, 28), -12 * 60)
        );

        let over_leap = all_day(d(2024, 2, 28), d(2024, 3, 1));
        assert!(over_leap.covers_day(d(2024, 2, 29), 0));
        assert!(!over_leap.covers_day(d(2024, 3, 1), 0));
        let no_leap = all_day(d(2023, 2, 28), d(2023, 3, 1));
        assert!(no_leap.covers_day(d(2023, 2, 28), 0) && !no_leap.covers_day(d(2023, 3, 1), 0));

        assert!(
            all_day(d(2024, 2, 28), d(2024, 2, 28)).validate().is_err(),
            "empty range"
        );
        assert!(
            all_day(d(2024, 2, 29), d(2024, 2, 28)).validate().is_err(),
            "reversed range"
        );
    }

    #[test]
    fn android_rows_map_to_all_day_or_timed() {
        let feb29 = d(2024, 2, 29).epoch_day() * DAY_MS;
        let w = When::from_android(feb29, feb29 + DAY_MS, true, "UTC").unwrap();
        assert_eq!(w, all_day(d(2024, 2, 29), d(2024, 3, 1)));
        // Pre-1970 midnights must floor correctly.
        let old = d(1969, 12, 31).epoch_day() * DAY_MS;
        assert_eq!(
            When::from_android(old, old + DAY_MS, true, "UTC").unwrap(),
            all_day(d(1969, 12, 31), d(1970, 1, 1))
        );
        assert!(
            When::from_android(feb29 + 1, feb29 + DAY_MS, true, "UTC").is_err(),
            "not midnight"
        );
        assert!(When::from_android(feb29, feb29 + DAY_MS + 3_600_000, true, "UTC").is_err());
        assert!(When::from_android(feb29, feb29, true, "UTC").is_err());
        assert!(When::from_android(1_000, 2_000, false, "Europe/Berlin").is_ok());
    }

    #[test]
    fn timed_ranges_and_zones_are_validated() {
        assert!(timed(1_000, 2_000).validate().is_ok());
        assert!(timed(2_000, 2_000).validate().is_err(), "zero length");
        assert!(timed(2_000, 1_000).validate().is_err(), "reversed");
        assert!(timed(i64::MIN, 0).validate().is_err());
        assert!(timed(0, i64::MAX).validate().is_err());
        let lo = MIN_EPOCH_DAY * DAY_MS;
        assert!(timed(lo, lo + 1).validate().is_ok() && timed(lo - 1, lo + 1).validate().is_err());
        for tz in [
            "",
            "../etc",
            "a//b",
            "/UTC",
            "UTC/",
            "Europe/Be rlin",
            "Europe/Berlin\n",
            &"x".repeat(65),
        ] {
            let w = When::Timed {
                start_utc_ms: 0,
                end_utc_ms: 1,
                time_zone: tz.into(),
            };
            assert!(w.validate().is_err(), "{tz:?}");
        }
        for tz in [
            "UTC",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+5",
            "GMT+05:30",
        ] {
            assert!(
                When::Timed {
                    start_utc_ms: 0,
                    end_utc_ms: 1,
                    time_zone: tz.into()
                }
                .validate()
                .is_ok(),
                "{tz}"
            );
        }
    }

    #[test]
    fn timed_events_use_the_viewers_offset_for_day_buckets() {
        let start = d(2024, 3, 1).epoch_day() * DAY_MS + 23 * 3_600_000 + 30 * 60_000; // 23:30Z
        let w = timed(start, start + 3_600_000); // ends 00:30Z next day
        assert!(w.covers_day(d(2024, 3, 1), 0) && w.covers_day(d(2024, 3, 2), 0));
        // At UTC+1 it sits entirely on Mar 2, 00:30-01:30.
        assert!(!w.covers_day(d(2024, 3, 1), 60) && w.covers_day(d(2024, 3, 2), 60));
        // At UTC-5 it sits entirely on Mar 1, 18:30-19:30.
        assert!(w.covers_day(d(2024, 3, 1), -300) && !w.covers_day(d(2024, 3, 2), -300));
        // Ending exactly at local midnight does not touch the next day.
        let midnight = d(2024, 3, 2).epoch_day() * DAY_MS;
        let w = timed(midnight - 3_600_000, midnight);
        assert!(w.covers_day(d(2024, 3, 1), 0) && !w.covers_day(d(2024, 3, 2), 0));
    }

    #[test]
    fn recurrence_metadata_is_validated() {
        let series = |rrule: Option<&str>, rdates: &[&str]| Recurrence::Series {
            rrule: rrule.map(Into::into),
            rdates: rdates.iter().map(|s| s.to_string()).collect(),
            exdates: vec![],
        };
        assert!(
            series(Some("FREQ=WEEKLY;COUNT=10;WKST=SU"), &[])
                .validate()
                .is_ok()
        );
        assert!(series(None, &["20240229T090000Z"]).validate().is_ok());
        assert!(series(None, &[]).validate().is_err());
        assert!(series(Some("COUNT=10"), &[]).validate().is_err());
        assert!(
            series(Some("FREQ=DAILY\r\nX-EVIL:1"), &[])
                .validate()
                .is_err()
        );
        assert!(
            Recurrence::Exception {
                master_id: "".into(),
                original_start_utc_ms: 0
            }
            .validate()
            .is_err()
        );
        let json = r#"{"kind":"series","rrule":"FREQ=YEARLY"}"#;
        assert!(
            serde_json::from_str::<Recurrence>(json)
                .unwrap()
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn contacts_need_something_to_show_and_a_sane_vcard() {
        assert!(Contact::default().validate().is_err());
        assert!(
            Contact {
                phones: vec!["+54 11 5555 0100".into()],
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            Contact {
                phones: vec!["  ".into()],
                ..person("Ada")
            }
            .validate()
            .is_err()
        );
        assert!(person("Ada\u{0}").validate().is_err());
        let vcard = |s: &str| Contact {
            vcard: Some(s.into()),
            ..person("Ada")
        };
        assert!(vcard("BEGIN:VCARD\nFN:Ada\nEND:VCARD").validate().is_ok());
        assert!(vcard("FN:Ada").validate().is_err());
        assert!(
            vcard(&format!(
                "BEGIN:VCARD\n{}END:VCARD",
                "N:x\n".repeat(MAX_VCARD_BYTES)
            ))
            .validate()
            .is_err()
        );
    }

    #[test]
    fn vcard_rev_reads_what_android_appends() {
        // Shape produced by kdeconnect-android's ContactsPlugin.addVCardMetadata.
        let v = "BEGIN:VCARD\nVERSION:2.1\nN:;Ada;;;\nREV:2001-01-01\nX-KDECONNECT-ID-DEV-abc:0r1-2A\nREV:1730419200000\nEND:VCARD";
        assert_eq!(vcard_rev(v), Some("1730419200000"));
        assert_eq!(vcard_rev("BEGIN:VCARD\nEND:VCARD"), None);
        assert_eq!(vcard_rev("rev:7\r\n"), Some("7"));
        assert!(!v.contains("X-KDECONNECT-TIMESTAMP"));
        assert_eq!(vcard_rev("é"), None);
    }

    #[test]
    fn apply_inserts_updates_and_replays_idempotently() {
        let mut s = Store::<Contact>::default();
        assert_eq!(s.apply(live("a", "1", "Ada"), None), Ok(Applied::Inserted));
        assert_eq!(s.apply(live("a", "1", "Ada"), None), Ok(Applied::Unchanged));
        assert_eq!(
            s.apply(live("a", "1", "Ada"), Some(&rev("1"))),
            Ok(Applied::Unchanged)
        );
        assert_eq!(
            s.apply(live("a", "2", "Ada L."), Some(&rev("1"))),
            Ok(Applied::Updated)
        );
        assert_eq!(
            s.get(&sid("a")).unwrap().item().unwrap().display_name,
            "Ada L."
        );
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn revision_conflicts_are_detected_and_leave_state_untouched() {
        let mut s = Store::<Contact>::default();
        s.apply(live("a", "2", "Ada"), None).unwrap();
        // The sender thinks revision 1 is stored, but 2 is.
        let err = s
            .apply(live("a", "3", "Stale edit"), Some(&rev("1")))
            .unwrap_err();
        assert_eq!(
            err,
            AgendaError::Conflict {
                id: sid("a"),
                base: Some(rev("1")),
                current: Some(rev("2"))
            }
        );
        // The sender thinks the id is new, but it exists.
        assert!(matches!(
            s.apply(live("a", "3", "Dup"), None),
            Err(AgendaError::Conflict {
                current: Some(_),
                ..
            })
        ));
        // The sender expects a record that was never stored.
        assert!(matches!(
            s.apply(live("zzz", "1", "Ghost"), Some(&rev("0"))),
            Err(AgendaError::Conflict { current: None, .. })
        ));
        // Same revision token, different content.
        assert_eq!(
            s.apply(live("a", "2", "Changed quietly"), Some(&rev("2"))),
            Err(AgendaError::StaleRevision(sid("a")))
        );
        assert_eq!(s.get(&sid("a")).unwrap().revision, rev("2"));
        assert_eq!(
            s.get(&sid("a")).unwrap().item().unwrap().display_name,
            "Ada"
        );
        assert!(s.get(&sid("zzz")).is_none());
    }

    #[test]
    fn invalid_records_are_rejected_before_touching_the_store() {
        let mut s = Store::<Contact>::default();
        assert!(s.apply(live("a", "1", ""), None).is_err());
        assert!(s.apply(live("", "1", "Ada"), None).is_err());
        assert!(s.apply(live("a", "", "Ada"), None).is_err());
        assert!(s.is_empty());
    }

    #[test]
    fn tombstones_hide_records_and_resurrection_needs_the_tombstone_revision() {
        let mut s = Store::<Contact>::default();
        s.apply(live("a", "1", "Ada"), None).unwrap();
        let gone = Record::<Contact>::tombstone(sid("a"), rev("2"), Some(1_000));
        assert_eq!(s.apply(gone.clone(), Some(&rev("1"))), Ok(Applied::Deleted));
        assert_eq!(s.live().count(), 0);
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.apply(gone, Some(&rev("1"))),
            Ok(Applied::Unchanged),
            "replay"
        );
        // Coming back from the dead as if never seen is a conflict...
        assert!(matches!(
            s.apply(live("a", "3", "Ada"), None),
            Err(AgendaError::Conflict { .. })
        ));
        // ...but a revision-aware update resurrects it.
        assert_eq!(
            s.apply(live("a", "3", "Ada"), Some(&rev("2"))),
            Ok(Applied::Updated)
        );
        assert_eq!(s.live().count(), 1);
        // A tombstone may also arrive before the record was ever seen.
        assert_eq!(
            s.apply(Record::tombstone(sid("never"), rev("9"), None), None),
            Ok(Applied::Deleted)
        );
    }

    #[test]
    fn mark_deleted_and_prune_tombstones() {
        let mut s = Store::<Contact>::default();
        s.apply(live("a", "1", "Ada"), None).unwrap();
        s.apply(live("b", "1", "Bob"), None).unwrap();
        s.apply(Record::tombstone(sid("c"), rev("1"), None), None)
            .unwrap();
        assert_eq!(s.mark_deleted(&sid("a"), 500), Ok(Applied::Deleted));
        assert_eq!(
            s.mark_deleted(&sid("a"), 600),
            Ok(Applied::Unchanged),
            "already deleted"
        );
        assert_eq!(s.mark_deleted(&sid("missing"), 600), Ok(Applied::Unchanged));
        assert_eq!(s.get(&sid("a")).unwrap().revision, rev("gb-deleted:500"));
        assert_eq!(s.prune_tombstones(500), 0, "cutoff is exclusive");
        assert_eq!(s.prune_tombstones(501), 1);
        assert!(s.get(&sid("a")).is_none());
        assert!(
            s.get(&sid("c")).is_some(),
            "tombstones without a time are kept"
        );
        assert!(s.get(&sid("b")).unwrap().item().is_some());
    }

    #[test]
    fn plan_sync_mirrors_the_uid_timestamp_handshake() {
        let mut s = Store::<Contact>::default();
        s.apply(live("same", "10", "Same"), None).unwrap();
        s.apply(live("changed", "10", "Changed"), None).unwrap();
        s.apply(live("removed", "10", "Removed"), None).unwrap();
        s.apply(Record::tombstone(sid("dead"), rev("5"), None), None)
            .unwrap();
        let remote = [
            (sid("same"), rev("10")),
            (sid("changed"), rev("11")),
            (sid("new"), rev("1")),
            (sid("dead"), rev("6")),
        ];
        let plan = s.plan_sync(&remote).unwrap();
        assert_eq!(plan.fetch, vec![sid("changed"), sid("new"), sid("dead")]);
        assert_eq!(
            plan.absent,
            vec![sid("removed")],
            "already-tombstoned ids are not reported again"
        );
        let dup = [(sid("a"), rev("1")), (sid("a"), rev("2"))];
        assert_eq!(s.plan_sync(&dup), Err(AgendaError::DuplicateId(sid("a"))));
        assert!(s.plan_sync(&[(sid("a"), rev(""))]).is_err());
        // An empty phone listing marks everything absent, which the caller must treat with care.
        assert_eq!(s.plan_sync(&[]).unwrap().absent.len(), 3);
    }

    #[test]
    fn record_json_round_trips_including_tombstones() {
        let ev = Record::live(
            SourceId::new("android:phone:calendar", "42"),
            rev("7f3a"),
            CalendarEvent {
                location: Some("Café".into()),
                recurrence: Some(Recurrence::Series {
                    rrule: Some("FREQ=WEEKLY".into()),
                    rdates: vec![],
                    exdates: vec!["20240229T090000Z".into()],
                }),
                ..event("Standup", all_day(d(2024, 2, 29), d(2024, 3, 1)))
            },
        );
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"2024-02-29\"") && json.contains("\"live\""));
        assert_eq!(
            serde_json::from_str::<Record<CalendarEvent>>(&json).unwrap(),
            ev
        );
        let t = Record::<Contact>::tombstone(sid("a"), rev("2"), Some(9));
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Record<Contact>>(&json).unwrap(), t);
    }

    #[test]
    fn people_view_searches_and_sorts() {
        let mut a = Agenda::default();
        let mut add = |id: &str, c: Contact| {
            a.contacts
                .apply(Record::live(sid(id), rev("1"), c), None)
                .unwrap()
        };
        add(
            "1",
            Contact {
                phones: vec!["+54 (11) 5555-0100".into()],
                ..person("zoe")
            },
        );
        add(
            "2",
            Contact {
                emails: vec!["ada@example.org".into()],
                organization: Some("Analytical Engines".into()),
                ..person("Ada")
            },
        );
        add("3", person("Álvaro"));
        add("4", person("Bob"));
        a.contacts.mark_deleted(&sid("4"), 1).unwrap();
        let names = |q: &str| {
            a.people(q)
                .iter()
                .map(|r| r.item().unwrap().display_name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(""), ["Ada", "Álvaro", "zoe"]);
        assert_eq!(names("alvaro"), ["Álvaro"], "search ignores accents");
        assert_eq!(names("ENGINES"), ["Ada"]);
        assert_eq!(names("example.org"), ["Ada"]);
        assert_eq!(names("555501"), ["zoe"]);
        assert_eq!(
            names("55"),
            Vec::<String>::new(),
            "short digit queries do not match phones"
        );
        assert!(names("bob").is_empty(), "tombstoned contacts are hidden");
    }

    #[test]
    fn day_view_orders_events_and_skips_unexpanded_series() {
        let mut a = Agenda::default();
        let day = d(2024, 2, 29);
        let noon = day.epoch_day() * DAY_MS + 12 * 3_600_000;
        let cal = |id: &str| SourceId::new("android:phone:calendar", id);
        let mut add = |id: &str, e: CalendarEvent| {
            a.events
                .apply(Record::live(cal(id), rev("1"), e), None)
                .unwrap()
        };
        add("lunch", event("Lunch", timed(noon, noon + 3_600_000)));
        add(
            "early",
            event("Early", timed(noon - 6 * 3_600_000, noon - 5 * 3_600_000)),
        );
        add(
            "trip",
            event("Trip", all_day(d(2024, 2, 27), d(2024, 3, 1))),
        );
        add(
            "tomorrow",
            event("Tomorrow", all_day(d(2024, 3, 1), d(2024, 3, 2))),
        );
        add(
            "series",
            CalendarEvent {
                recurrence: Some(Recurrence::Series {
                    rrule: Some("FREQ=DAILY".into()),
                    rdates: vec![],
                    exdates: vec![],
                }),
                ..event("Daily", timed(noon, noon + 1))
            },
        );
        add(
            "moved",
            CalendarEvent {
                recurrence: Some(Recurrence::Exception {
                    master_id: "series".into(),
                    original_start_utc_ms: noon - DAY_MS,
                }),
                ..event("Daily (moved)", timed(noon + 7_200_000, noon + 7_300_000))
            },
        );
        let titles = |off| {
            a.day(day, off)
                .iter()
                .map(|r| r.item().unwrap().title.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(0), ["Trip", "Early", "Lunch", "Daily (moved)"]);
        // At UTC-13, 06:00Z and 12:00Z are 17:00 and 23:00 on the previous local day; 14:00Z is 01:00 today.
        assert_eq!(titles(-13 * 60), ["Trip", "Daily (moved)"]);
        // At UTC+14, 12:00Z is 02:00 the next local day and 06:00Z is 20:00 today.
        assert_eq!(titles(14 * 60), ["Trip", "Early"]);
    }
}
