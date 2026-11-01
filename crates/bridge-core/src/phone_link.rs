//! Wire helpers for phone data over the paired link: contacts, calendar and SMS status.
//!
//! The contract is `docs/phone-data-link.md`. Everything parsed here is untrusted phone input:
//! bounded, strict about types, and it never panics. Contacts use the stock
//! `kdeconnect.contacts.*` packets; the calendar and the SMS status are GeekBridge extensions
//! (`geekbridge.calendar.*`, `geekbridge.sms.status`). SMS messages themselves are parsed by
//! `bridge_kde::sms` with [`crate::messages`].
use crate::agenda::{
    AgendaError, CalendarEvent, Contact, Record, Revision, SourceId, State, Store, Validate, When,
    vcard_rev,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

pub const CONTACTS_REQUEST_UIDS: &str = "kdeconnect.contacts.request_all_uids_timestamps";
pub const CONTACTS_UIDS: &str = "kdeconnect.contacts.response_uids_timestamps";
pub const CONTACTS_REQUEST_VCARDS: &str = "kdeconnect.contacts.request_vcards_by_uid";
pub const CONTACTS_VCARDS: &str = "kdeconnect.contacts.response_vcards";
pub const CALENDAR_REQUEST: &str = "geekbridge.calendar.request";
pub const CALENDAR_SYNC: &str = "geekbridge.calendar.sync";
pub const SMS_STATUS: &str = "geekbridge.sms.status";

pub const MAX_CONTACT_UIDS: usize = 5000;
pub const VCARD_BATCH: usize = 25;
pub const MAX_CALENDAR_EVENTS: usize = 1000;
pub const MAX_CALENDARS: usize = 32;
pub const MAX_DELETED: usize = 500;
pub const MAX_REQUEST_ID_BYTES: usize = 64;
const MAX_TOKEN_BYTES: usize = 128;
static EMPTY: Vec<Value> = Vec::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// A required field is missing or has the wrong type.
    Malformed,
    /// A count or length is over its bound.
    TooLarge,
}

pub fn is_contacts_kind(kind: &str) -> bool {
    matches!(
        kind,
        CONTACTS_REQUEST_UIDS | CONTACTS_UIDS | CONTACTS_REQUEST_VCARDS | CONTACTS_VCARDS
    )
}
pub fn is_calendar_kind(kind: &str) -> bool {
    matches!(kind, CALENDAR_REQUEST | CALENDAR_SYNC)
}

pub fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REQUEST_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(if c.is_control() && !matches!(c, '\n' | '\t') {
            ' '
        } else {
            c
        });
    }
    out
}

// ---------------------------------------------------------------- contacts

/// `uid -> revision` pairs from `response_uids_timestamps`. Timestamps may be strings or numbers.
pub fn parse_uids(body: &Map<String, Value>) -> Result<Vec<(String, String)>, WireError> {
    let uids = body
        .get("uids")
        .and_then(Value::as_array)
        .ok_or(WireError::Malformed)?;
    if uids.len() > MAX_CONTACT_UIDS {
        return Err(WireError::TooLarge);
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(uids.len());
    for uid in uids {
        let uid = uid.as_str().ok_or(WireError::Malformed)?;
        if uid.is_empty() || uid.len() > 512 || uid == "uids" || !seen.insert(uid) {
            return Err(WireError::Malformed);
        }
        let ts = match body.get(uid) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err(WireError::Malformed),
        };
        out.push((uid.to_owned(), ts));
    }
    Ok(out)
}

/// `uid -> vCard text` pairs from `response_vcards`. Entries without a vCard are skipped (the
/// phone omits contacts it could not read).
pub fn parse_vcards(body: &Map<String, Value>) -> Result<Vec<(String, String)>, WireError> {
    let uids = body
        .get("uids")
        .and_then(Value::as_array)
        .ok_or(WireError::Malformed)?;
    if uids.len() > VCARD_BATCH * 4 {
        return Err(WireError::TooLarge);
    }
    let mut out = Vec::new();
    for uid in uids {
        let uid = uid.as_str().ok_or(WireError::Malformed)?;
        if uid.is_empty() || uid.len() > 512 || uid == "uids" {
            return Err(WireError::Malformed);
        }
        if let Some(Value::String(v)) = body.get(uid) {
            out.push((uid.to_owned(), v.clone()));
        }
    }
    Ok(out)
}

pub fn request_vcards_body(uids: &[String]) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("uids".into(), json!(uids));
    m
}

fn unfold(vcard: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut qp_open = false;
    for raw in vcard.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if qp_open {
            // vCard 2.1 quoted-printable soft break: the previous line ended with `=`.
            if let Some(last) = lines.last_mut() {
                last.pop();
                last.push_str(raw);
                qp_open = last.ends_with('=');
                continue;
            }
        }
        if raw.starts_with([' ', '\t']) && !lines.is_empty() {
            if let Some(last) = lines.last_mut() {
                last.push_str(&raw[1..]);
            }
            continue;
        }
        qp_open = raw.ends_with('=') && raw.to_ascii_uppercase().contains("QUOTED-PRINTABLE");
        lines.push(raw.to_owned());
    }
    lines
}

fn qp_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'='
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unescape(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Removes `PHOTO`/`LOGO`/`SOUND` properties (large base64 blobs) from a vCard, keeping the rest
/// verbatim. Folded continuation lines of a removed property go with it.
pub fn strip_vcard_blobs(vcard: &str) -> String {
    let mut out = String::with_capacity(vcard.len().min(8 * 1024));
    let mut skipping = false;
    for line in vcard.split_inclusive('\n') {
        let t = line.trim_end_matches(['\r', '\n']);
        if t.starts_with([' ', '\t']) {
            if skipping {
                continue;
            }
        } else {
            let name = t
                .split([';', ':'])
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            skipping = matches!(name.as_str(), "PHOTO" | "LOGO" | "SOUND");
            if skipping {
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// Builds a [`Contact`] from a phone vCard (2.1 or 3.0, quoted-printable aware). The stored raw
/// vCard has photos removed. Fails if the card has no name, phone or email.
pub fn contact_from_vcard(vcard: &str) -> Result<Contact, AgendaError> {
    if vcard.len() > crate::agenda::MAX_VCARD_BYTES {
        return Err(AgendaError::InvalidField("vcard"));
    }
    let mut c = Contact::default();
    let (mut fn_name, mut n_name) = (None::<String>, None::<String>);
    for line in unfold(&strip_vcard_blobs(vcard)) {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = head.split(';');
        let name = parts.next().unwrap_or("").to_ascii_uppercase();
        let params: Vec<String> = parts.map(str::to_ascii_uppercase).collect();
        let value = if params.iter().any(|p| p.contains("QUOTED-PRINTABLE")) {
            qp_decode(value)
        } else {
            value.to_owned()
        };
        // Strip a group prefix such as `item1.TEL`.
        let name = name.rsplit('.').next().unwrap_or("").to_owned();
        match name.as_str() {
            "FN" => fn_name = Some(unescape(&value)),
            "N" => {
                let f: Vec<&str> = value.split(';').collect();
                let joined = [f.get(3), f.get(1), f.get(2), f.first()]
                    .into_iter()
                    .flatten()
                    .map(|s| unescape(s))
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                n_name = Some(joined);
            }
            "TEL" => {
                let v = unescape(value.trim());
                if !v.is_empty() && c.phones.len() < 64 && !c.phones.contains(&v) {
                    c.phones.push(clean(&v, 64));
                }
            }
            "EMAIL" => {
                let v = unescape(value.trim());
                if !v.is_empty() && c.emails.len() < 64 && !c.emails.contains(&v) {
                    c.emails.push(clean(&v, 320));
                }
            }
            "ORG" => {
                let v = value
                    .split(';')
                    .map(unescape)
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                if !v.is_empty() && c.organization.is_none() {
                    c.organization = Some(clean(&v, 256));
                }
            }
            _ => {}
        }
    }
    c.display_name = clean(
        fn_name
            .filter(|s| !s.trim().is_empty())
            .or(n_name.filter(|s| !s.trim().is_empty()))
            .unwrap_or_default()
            .trim(),
        256,
    );
    c.vcard = Some(strip_vcard_blobs(vcard));
    c.validate()?;
    Ok(c)
}

/// Revision for a fetched contact: the phone's listing timestamp, else the vCard `REV:` line.
pub fn contact_revision(listing_ts: Option<&str>, vcard: &str) -> Revision {
    Revision(
        listing_ts
            .map(str::to_owned)
            .or_else(|| vcard_rev(vcard).map(str::to_owned))
            .unwrap_or_else(|| "0".into()),
    )
}

// ---------------------------------------------------------------- calendar

pub fn calendar_request_body(request_id: &str, since: Option<&str>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("requestId".into(), json!(request_id));
    if let Some(s) = since {
        m.insert("since".into(), json!(s));
    }
    m
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalendarSync {
    pub full: bool,
    pub base: Option<String>,
    pub token: String,
    pub truncated: bool,
    pub calendars: Vec<CalendarInfo>,
    pub events: Vec<(String, Revision, CalendarEvent)>,
    pub deleted: Vec<String>,
    /// Events dropped because they failed validation.
    pub skipped: usize,
}

fn str_field<'a>(o: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    o.get(k).and_then(Value::as_str)
}

pub fn parse_calendar_sync(body: &Map<String, Value>) -> Result<CalendarSync, WireError> {
    if body.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(WireError::Malformed);
    }
    let full = body
        .get("full")
        .and_then(Value::as_bool)
        .ok_or(WireError::Malformed)?;
    let token = str_field(body, "token").ok_or(WireError::Malformed)?;
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(WireError::Malformed);
    }
    let base = match body.get("base") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() && s.len() <= MAX_TOKEN_BYTES => Some(s.clone()),
        _ => return Err(WireError::Malformed),
    };
    if full == base.is_some() {
        return Err(WireError::Malformed); // full has no base; a delta must name one
    }
    let arr = |k: &str, max: usize| -> Result<&Vec<Value>, WireError> {
        match body.get(k) {
            None => Ok(&EMPTY),
            Some(Value::Array(a)) if a.len() <= max => Ok(a),
            Some(Value::Array(_)) => Err(WireError::TooLarge),
            _ => Err(WireError::Malformed),
        }
    };
    let mut calendars = Vec::new();
    for c in arr("calendars", MAX_CALENDARS)? {
        let o = c.as_object().ok_or(WireError::Malformed)?;
        let id = str_field(o, "id").ok_or(WireError::Malformed)?;
        if id.is_empty() || id.len() > 64 {
            return Err(WireError::Malformed);
        }
        calendars.push(CalendarInfo {
            id: id.to_owned(),
            name: clean(str_field(o, "name").unwrap_or(""), 128),
        });
    }
    let mut deleted = Vec::new();
    for d in arr("deleted", MAX_DELETED)? {
        let id = d
            .as_object()
            .and_then(|o| str_field(o, "id"))
            .ok_or(WireError::Malformed)?;
        deleted.push(id.to_owned());
    }
    let mut events = Vec::new();
    let mut skipped = 0;
    let mut seen = BTreeSet::new();
    for e in arr("events", MAX_CALENDAR_EVENTS)? {
        match parse_event(e) {
            Some((id, rev, ev)) if seen.insert(id.clone()) => events.push((id, rev, ev)),
            _ => skipped += 1,
        }
    }
    Ok(CalendarSync {
        full,
        base,
        token: token.to_owned(),
        truncated: body
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        calendars,
        events,
        deleted,
        skipped,
    })
}

fn parse_event(v: &Value) -> Option<(String, Revision, CalendarEvent)> {
    let o = v.as_object()?;
    let id = str_field(o, "id")?.to_owned();
    let rev = Revision(str_field(o, "rev")?.to_owned());
    let all_day = o.get("allDay").and_then(Value::as_bool)?;
    let start = o.get("startMs").and_then(Value::as_i64)?;
    let mut end = o.get("endMs").and_then(Value::as_i64)?;
    if !all_day && end <= start {
        end = start.checked_add(60_000)?; // instantaneous reminder-style event
    }
    let tz = str_field(o, "tz")
        .filter(|s| !s.is_empty())
        .unwrap_or("UTC");
    let when = When::from_android(start, end, all_day, tz).ok()?;
    let opt = |k: &str, max: usize| {
        str_field(o, k)
            .map(|s| clean(s, max))
            .filter(|s| !s.trim().is_empty())
    };
    let ev = CalendarEvent {
        title: clean(str_field(o, "title").unwrap_or(""), 512),
        when,
        location: opt("location", 512),
        description: opt("description", 16 * 1024),
        recurrence: None,
    };
    ev.validate().ok()?;
    rev.validate().ok()?;
    SourceId::new("x", id.clone()).validate().ok()?;
    Some((id, rev, ev))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CalendarApplied {
    pub inserted: usize,
    pub updated: usize,
    pub deleted: usize,
    pub rejected: usize,
}

/// Why a delta cannot be applied and a full sync must be asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarMismatch {
    /// The delta builds on a token other than the one the desktop applied last.
    BaseMismatch,
}

/// Applies a sync to `store`. `have_token` is the token of the last applied sync. A delta whose
/// `base` differs is refused (the caller asks for a full sync). A full sync tombstones every live
/// record not listed.
pub fn apply_calendar(
    store: &mut Store<CalendarEvent>,
    source: &str,
    have_token: Option<&str>,
    sync: &CalendarSync,
    now_unix_ms: u64,
) -> Result<CalendarApplied, CalendarMismatch> {
    if !sync.full && sync.base.as_deref() != have_token {
        return Err(CalendarMismatch::BaseMismatch);
    }
    let mut out = CalendarApplied {
        rejected: sync.skipped,
        ..Default::default()
    };
    for (id, rev, ev) in &sync.events {
        let sid = SourceId::new(source, id.clone());
        let base = store.get(&sid).map(|r| r.revision.clone());
        match store.apply(Record::live(sid, rev.clone(), ev.clone()), base.as_ref()) {
            Ok(crate::agenda::Applied::Inserted) => out.inserted += 1,
            Ok(crate::agenda::Applied::Updated) => out.updated += 1,
            Ok(_) => {}
            Err(_) => out.rejected += 1,
        }
    }
    let mut gone: Vec<SourceId> = sync
        .deleted
        .iter()
        .map(|d| SourceId::new(source, d.clone()))
        .collect();
    if sync.full {
        let listed: BTreeSet<&String> = sync.events.iter().map(|(i, _, _)| i).collect();
        gone.extend(
            store
                .live()
                .filter(|(r, _)| !listed.contains(&r.id.id))
                .map(|(r, _)| r.id.clone()),
        );
    }
    for id in gone {
        if matches!(
            store.mark_deleted(&id, now_unix_ms),
            Ok(crate::agenda::Applied::Deleted)
        ) {
            out.deleted += 1;
        }
    }
    // Tombstones only need to outlive the next sync.
    store.prune_tombstones(now_unix_ms.saturating_sub(24 * 3600 * 1000));
    Ok(out)
}

/// True when a record is a live item (helper for views).
pub fn is_live<T>(r: &Record<T>) -> bool {
    matches!(r.state, State::Live(_))
}

// ---------------------------------------------------------------- sms status

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmsPhase {
    Accepted,
    Sent,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsStatus {
    pub request_id: String,
    pub phase: SmsPhase,
    pub reason: Option<String>,
}

pub fn parse_sms_status(body: &Map<String, Value>) -> Result<SmsStatus, WireError> {
    let request_id = str_field(body, "requestId").ok_or(WireError::Malformed)?;
    if !valid_request_id(request_id) {
        return Err(WireError::Malformed);
    }
    let phase = match str_field(body, "state").ok_or(WireError::Malformed)? {
        "accepted" => SmsPhase::Accepted,
        "sent" => SmsPhase::Sent,
        "delivered" => SmsPhase::Delivered,
        "failed" => SmsPhase::Failed,
        _ => return Err(WireError::Malformed),
    };
    let reason = str_field(body, "reason")
        .map(|r| {
            r.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(32)
                .collect::<String>()
        })
        .filter(|r| !r.is_empty());
    Ok(SmsStatus {
        request_id: request_id.to_owned(),
        phase,
        reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn uids_listing_checks_types_and_bounds() {
        let ok = obj(json!({"uids": ["a", "b"], "a": "100", "b": 200}));
        assert_eq!(
            parse_uids(&ok).unwrap(),
            vec![("a".into(), "100".into()), ("b".into(), "200".into())]
        );
        assert!(parse_uids(&obj(json!({"uids": ["a"]}))).is_err()); // no timestamp
        assert!(parse_uids(&obj(json!({"uids": ["a", "a"], "a": "1"}))).is_err());
        assert!(parse_uids(&obj(json!({"uids": ["uids"], "uids": "1"}))).is_err());
        assert!(parse_uids(&obj(json!({"uids": [1]}))).is_err());
        let many: Vec<String> = (0..MAX_CONTACT_UIDS + 1).map(|i| i.to_string()).collect();
        assert_eq!(
            parse_uids(&obj(json!({ "uids": many }))),
            Err(WireError::TooLarge)
        );
    }

    #[test]
    fn vcard_30_and_21_quoted_printable() {
        let v3 = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nTEL;TYPE=CELL:+44 20 7946 0000\r\nEMAIL:ada@example.org\r\nORG:Analytical Engines\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\n BBBB\r\nREV:1700\r\nEND:VCARD\r\n";
        let c = contact_from_vcard(v3).unwrap();
        assert_eq!(c.display_name, "Ada Lovelace");
        assert_eq!(c.phones, vec!["+44 20 7946 0000"]);
        assert_eq!(c.emails, vec!["ada@example.org"]);
        assert_eq!(c.organization.as_deref(), Some("Analytical Engines"));
        assert!(!c.vcard.unwrap().contains("PHOTO"));

        let v21 = "BEGIN:VCARD\nVERSION:2.1\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:M=C3=BCller;J=C3=BCr=\n=67en;;;\nTEL;CELL:555 0100\nEND:VCARD\n";
        let c = contact_from_vcard(v21).unwrap();
        assert_eq!(c.display_name, "Jürgen Müller");
        assert_eq!(c.phones, vec!["555 0100"]);
    }

    #[test]
    fn vcard_without_identity_is_rejected() {
        assert!(contact_from_vcard("BEGIN:VCARD\nVERSION:3.0\nEND:VCARD\n").is_err());
        assert!(contact_from_vcard("hello").is_err());
    }

    #[test]
    fn contact_revision_prefers_listing() {
        assert_eq!(contact_revision(Some("5"), "REV:9\n").0, "5");
        assert_eq!(contact_revision(None, "X\nREV:9\nEND:VCARD").0, "9");
    }

    fn sync_body(
        full: bool,
        base: Option<&str>,
        token: &str,
        events: Value,
        deleted: Value,
    ) -> Map<String, Value> {
        obj(json!({
            "version": 1, "full": full, "base": base, "token": token,
            "calendars": [{"id": "1", "name": "Personal"}],
            "events": events, "deleted": deleted
        }))
    }
    fn ev(id: &str, rev: &str, title: &str) -> Value {
        json!({"id": id, "rev": rev, "title": title, "allDay": false,
               "startMs": 1_730_500_000_000_i64, "endMs": 1_730_503_600_000_i64, "tz": "Europe/Berlin"})
    }

    #[test]
    fn calendar_full_then_delta_with_tombstones() {
        let mut store = Store::default();
        let full = parse_calendar_sync(&sync_body(
            true,
            None,
            "b:1",
            json!([ev("1@1", "r1", "A"), ev("2@1", "r1", "B")]),
            json!([]),
        ))
        .unwrap();
        let r = apply_calendar(&mut store, "android:p:cal", None, &full, 1000).unwrap();
        assert_eq!((r.inserted, r.deleted), (2, 0));

        // A delta for the wrong base is refused.
        let delta = parse_calendar_sync(&sync_body(
            false,
            Some("b:1"),
            "b:2",
            json!([ev("1@1", "r2", "A2")]),
            json!([{"id": "2@1"}]),
        ))
        .unwrap();
        assert_eq!(
            apply_calendar(&mut store, "android:p:cal", Some("zzz"), &delta, 2000),
            Err(CalendarMismatch::BaseMismatch)
        );
        let r = apply_calendar(&mut store, "android:p:cal", Some("b:1"), &delta, 2000).unwrap();
        assert_eq!((r.updated, r.deleted), (1, 1));
        assert_eq!(store.live().count(), 1);

        // A full snapshot tombstones what it does not list.
        let full2 = parse_calendar_sync(&sync_body(
            true,
            None,
            "b:3",
            json!([ev("9@1", "r1", "Z")]),
            json!([]),
        ))
        .unwrap();
        let r = apply_calendar(&mut store, "android:p:cal", Some("b:2"), &full2, 3000).unwrap();
        assert_eq!((r.inserted, r.deleted), (1, 1));
        assert_eq!(
            store
                .live()
                .map(|(_, e)| e.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Z"]
        );
    }

    #[test]
    fn calendar_parse_is_strict_and_bounded() {
        let mut bad = sync_body(true, Some("x"), "t", json!([]), json!([]));
        assert!(parse_calendar_sync(&bad).is_err()); // full with base
        bad = sync_body(false, None, "t", json!([]), json!([]));
        assert!(parse_calendar_sync(&bad).is_err()); // delta without base
        let many: Vec<Value> = (0..MAX_CALENDAR_EVENTS + 1)
            .map(|i| ev(&i.to_string(), "r", "x"))
            .collect();
        assert_eq!(
            parse_calendar_sync(&sync_body(true, None, "t", json!(many), json!([]))),
            Err(WireError::TooLarge)
        );
        // Invalid events are skipped, not fatal; instantaneous timed events get a minute.
        let s = parse_calendar_sync(&sync_body(
            true,
            None,
            "t",
            json!([
                {"id": "a", "rev": "r", "title": "t", "allDay": true, "startMs": 5, "endMs": 7},
                {"id": "b", "rev": "r", "title": "t", "allDay": false, "startMs": 1_730_500_000_000_i64, "endMs": 1_730_500_000_000_i64, "tz": ""},
            ]),
            json!([]),
        ))
        .unwrap();
        assert_eq!((s.events.len(), s.skipped), (1, 1));
    }

    #[test]
    fn sms_status_parses_known_states() {
        let s = parse_sms_status(&obj(
            json!({"requestId": "r-1", "state": "failed", "reason": "no service!"}),
        ))
        .unwrap();
        assert_eq!(
            (s.phase, s.reason.as_deref()),
            (SmsPhase::Failed, Some("noservice"))
        );
        assert!(parse_sms_status(&obj(json!({"requestId": "r 1", "state": "sent"}))).is_err());
        assert!(parse_sms_status(&obj(json!({"requestId": "r1", "state": "queued"}))).is_err());
    }
}
