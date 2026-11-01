//! Pure calendar logic for the Calendar window: which local day an event belongs to, month/week
//! grids, overlap columns, the event editor's text fields and their conversion to UTC. No drawing
//! and no I/O; the computer's UTC offset is passed in so tests cover daylight-saving edges.
use crate::localtime::Date;
use bridge_core::phone_view::{CalendarItem, EventDraft, EventItem};

pub const DAY_MS: i64 = 86_400_000;

/// UTC offset in minutes of the viewer's computer on a local date.
pub type Offset<'a> = &'a dyn Fn(Date) -> i32;

/// First local day (as `Date::to_days`) and last local day (inclusive) an event touches.
pub fn local_days(e: &EventItem, offset: Offset) -> (i64, i64) {
    if e.all_day {
        // Android stores all-day events on UTC midnights: the nominal date, in every zone.
        let first = e.start_ms.div_euclid(DAY_MS);
        let last = (e.end_ms.div_euclid(DAY_MS) - 1).max(first);
        return (first, last);
    }
    let start_utc_day = Date::from_days(e.start_ms.div_euclid(DAY_MS));
    let off_s = i64::from(offset(start_utc_day)) * 60;
    let start = (e.start_ms.div_euclid(1000) + off_s).div_euclid(86_400);
    let end_utc_day = Date::from_days(e.end_ms.div_euclid(DAY_MS));
    let off_e = i64::from(offset(end_utc_day)) * 60;
    // End is exclusive: an event ending exactly at midnight does not touch the next day.
    let last = ((e.end_ms.div_euclid(1000) + off_e - 1).div_euclid(86_400)).max(start);
    (start, last)
}

pub fn touches(e: &EventItem, day: Date, offset: Offset) -> bool {
    let (a, b) = local_days(e, offset);
    (a..=b).contains(&day.to_days())
}

/// Events of one local day: all-day first, then by start time, then title.
pub fn on_day<'a>(events: &'a [&'a EventItem], day: Date, offset: Offset) -> Vec<&'a EventItem> {
    let mut v: Vec<&EventItem> = events
        .iter()
        .copied()
        .filter(|e| touches(e, day, offset))
        .collect();
    v.sort_by(|a, b| {
        (!a.all_day, a.start_ms, a.title.to_lowercase()).cmp(&(
            !b.all_day,
            b.start_ms,
            b.title.to_lowercase(),
        ))
    });
    v
}

/// Seconds from local midnight of `day` for `utc_ms`, clamped to the day (for timeline blocks).
pub fn seconds_into(utc_ms: i64, day: Date, offset_minutes: i32) -> i64 {
    let local = utc_ms.div_euclid(1000) + i64::from(offset_minutes) * 60;
    (local - day.to_days() * 86_400).clamp(0, 86_400)
}

/// First day of the month containing `d`.
pub fn month_start(d: Date) -> Date {
    Date { day: 1, ..d }
}

pub fn add_months(d: Date, delta: i32) -> Date {
    let m0 = d.year * 12 + (d.month as i32 - 1) + delta;
    Date {
        year: m0.div_euclid(12),
        month: (m0.rem_euclid(12) + 1) as u32,
        day: 1,
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    bridge_core::agenda::days_in_month(year, month as u8).map_or(30, u32::from)
}

/// The six Monday-first weeks that cover the month of `d`.
pub fn month_grid(d: Date) -> [[Date; 7]; 6] {
    let first = month_start(d).week_start();
    std::array::from_fn(|w| std::array::from_fn(|i| first.add_days((w * 7 + i) as i64)))
}

/// Greedy columns for overlapping timed events: (event index, column), and the column count.
/// `spans` are (start, end) in any consistent unit, sorted by start by the caller.
pub fn columns(spans: &[(i64, i64)]) -> (Vec<usize>, usize) {
    let mut ends: Vec<i64> = Vec::new();
    let cols = spans
        .iter()
        .map(|(s, e)| {
            let c = ends.iter().position(|end| *end <= *s).unwrap_or_else(|| {
                ends.push(0);
                ends.len() - 1
            });
            ends[c] = *e;
            c
        })
        .collect();
    (cols, ends.len().max(1))
}

// ------------------------------------------------------------------ colours and labels

/// Distinct fallback colours (ARGB) for calendars the phone gave none.
const FALLBACK: [u32; 6] = [
    0xFF1E88E5, 0xFF43A047, 0xFFF4511E, 0xFF8E24AA, 0xFFF9A825, 0xFF00897B,
];

pub fn calendar_color(cal: Option<&CalendarItem>, index: usize) -> [u8; 3] {
    let argb = cal
        .and_then(|c| c.color)
        .unwrap_or(FALLBACK[index % FALLBACK.len()]);
    [(argb >> 16) as u8, (argb >> 8) as u8, argb as u8]
}

/// "Personal (me@gmail.com)" - the calendar name with its account, so two calendars named
/// "Calendar" from different accounts can be told apart.
pub fn calendar_label(c: &CalendarItem) -> String {
    match c.account.as_deref().filter(|a| *a != c.name) {
        Some(a) => format!("{} ({a})", c.name),
        None => c.name.clone(),
    }
}

pub fn is_google(c: &CalendarItem) -> bool {
    c.account_type.as_deref() == Some("com.google")
}

pub fn reminder_label(min: u32) -> String {
    match min {
        0 => "At the start".into(),
        m if m % 10_080 == 0 => format!("{} week{} before", m / 10_080, plural(m / 10_080)),
        m if m % 1440 == 0 => format!("{} day{} before", m / 1440, plural(m / 1440)),
        m if m % 60 == 0 => format!("{} hour{} before", m / 60, plural(m / 60)),
        m => format!("{m} minutes before"),
    }
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ------------------------------------------------------------------ editor

pub const REMINDER_CHOICES: [Option<u32>; 7] = [
    None,
    Some(0),
    Some(5),
    Some(10),
    Some(30),
    Some(60),
    Some(1440),
];

/// The event editor's fields exactly as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub title: String,
    pub all_day: bool,
    pub start_date: String,
    pub start_time: String,
    /// All-day events end on this day (inclusive in the form, exclusive on the wire).
    pub end_date: String,
    pub end_time: String,
    pub location: String,
    pub notes: String,
    pub reminder: Option<u32>,
}

impl Fields {
    pub fn new_on(day: Date, hour: u32) -> Self {
        let h = hour.min(22);
        Self {
            title: String::new(),
            all_day: false,
            start_date: fmt_date(day),
            start_time: format!("{h:02}:00"),
            end_date: fmt_date(day),
            end_time: format!("{:02}:00", h + 1),
            location: String::new(),
            notes: String::new(),
            reminder: Some(10),
        }
    }

    /// Prefills from an existing event, shown in the viewer's zone.
    pub fn from_event(e: &EventItem, offset: Offset) -> Self {
        let (first, last) = local_days(e, offset);
        let (sd, ed) = (Date::from_days(first), Date::from_days(last));
        let clock = |ms: i64, d: Date| {
            let s = seconds_into(ms, d, offset(d));
            format!("{:02}:{:02}", s / 3600, s / 60 % 60)
        };
        // An event ending at midnight shows as 24:00 of the previous day; keep it editable as 00:00 next day.
        let end_instant_day = Date::from_days(
            (e.end_ms.div_euclid(1000)
                + i64::from(offset(Date::from_days(e.end_ms.div_euclid(DAY_MS)))) * 60)
                .div_euclid(86_400),
        );
        Self {
            title: e.title.clone(),
            all_day: e.all_day,
            start_date: fmt_date(sd),
            start_time: if e.all_day {
                "09:00".into()
            } else {
                clock(e.start_ms, sd)
            },
            end_date: fmt_date(if e.all_day { ed } else { end_instant_day }),
            end_time: if e.all_day {
                "10:00".into()
            } else {
                clock(e.end_ms, end_instant_day)
            },
            location: e.location.clone().unwrap_or_default(),
            notes: e.description.clone().unwrap_or_default(),
            reminder: e.reminders_min.first().copied(),
        }
    }

    /// Validates the fields and converts them to what goes to the phone. `zone` is the IANA name
    /// of this computer's time zone (UTC when it could not be read).
    pub fn to_draft(&self, zone: &str, offset: Offset) -> Result<EventDraft, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Give the event a title.".into());
        }
        let sd = parse_date(&self.start_date).ok_or("The start date must look like 2026-11-24.")?;
        let ed = parse_date(&self.end_date).ok_or("The end date must look like 2026-11-24.")?;
        let (start_ms, end_ms) = if self.all_day {
            if ed.to_days() < sd.to_days() {
                return Err("The event cannot end before it starts.".into());
            }
            (sd.to_days() * DAY_MS, (ed.to_days() + 1) * DAY_MS)
        } else {
            let st = parse_time(&self.start_time).ok_or("The start time must look like 14:30.")?;
            let et = parse_time(&self.end_time).ok_or("The end time must look like 15:30.")?;
            let utc = |d: Date, secs: u32| {
                (d.to_days() * 86_400 + i64::from(secs) - i64::from(offset(d)) * 60) * 1000
            };
            let (s, e) = (utc(sd, st), utc(ed, et));
            if e <= s {
                return Err("The event must end after it starts.".into());
            }
            (s, e)
        };
        let draft = EventDraft {
            title: title.to_owned(),
            all_day: self.all_day,
            start_ms,
            end_ms,
            tz: if self.all_day {
                "UTC".into()
            } else {
                zone.to_owned()
            },
            location: Some(self.location.trim().to_owned()).filter(|s| !s.is_empty()),
            description: Some(self.notes.trim().to_owned()).filter(|s| !s.is_empty()),
            reminder_min: self.reminder,
        };
        bridge_core::phone_link::validate_draft(&draft).map_err(|e| e.message().to_owned())?;
        Ok(draft)
    }
}

pub fn fmt_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year, d.month, d.day)
}

pub fn parse_date(s: &str) -> Option<Date> {
    let mut it = s.trim().split('-');
    let (y, m, d) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || y.len() != 4 || m.len() > 2 || d.len() > 2 {
        return None;
    }
    let (y, m, d) = (
        y.parse::<i32>().ok()?,
        m.parse::<u32>().ok()?,
        d.parse::<u32>().ok()?,
    );
    ((1970..=2200).contains(&y) && (1..=12).contains(&m) && (1..=days_in_month(y, m)).contains(&d))
        .then_some(Date {
            year: y,
            month: m,
            day: d,
        })
}

/// `"H:MM"` / `"HH:MM"` to seconds into the day.
pub fn parse_time(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let (h, m) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
    (h < 24 && m < 60).then_some(h * 3600 + m * 60)
}

/// IANA name of this computer's time zone, if it can be read (`TZ`, then `/etc/localtime`).
pub fn local_zone_name() -> Option<String> {
    let ok = |z: &str| {
        !z.is_empty()
            && z.len() <= 64
            && !z.contains("..")
            && z.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_+-/".contains(c))
            && z.contains(|c: char| c.is_ascii_alphabetic())
    };
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if ok(tz) {
            return Some(tz.to_owned());
        }
    }
    let link = std::fs::read_link("/etc/localtime").ok()?;
    let s = link.to_string_lossy();
    let zone = s.split_once("zoneinfo/")?.1;
    ok(zone).then(|| zone.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date {
            year: y,
            month: m,
            day,
        }
    }
    fn ev(all_day: bool, start: i64, end: i64) -> EventItem {
        EventItem {
            device_id: "d".into(),
            id: "1@1".into(),
            title: "T".into(),
            all_day,
            start_ms: start,
            end_ms: end,
            location: None,
            description: None,
            calendar_id: None,
            event_id: Some("1".into()),
            recurring: false,
            tz: None,
            reminders_min: vec![],
            attendee_count: 0,
            attendees: vec![],
        }
    }
    fn utc(_: Date) -> i32 {
        0
    }
    fn berlin_winter(_: Date) -> i32 {
        60
    }
    fn la(_: Date) -> i32 {
        -480
    }

    #[test]
    fn all_day_events_keep_their_nominal_date_in_every_zone() {
        let day = d(2026, 11, 24);
        let e = ev(true, day.to_days() * DAY_MS, (day.to_days() + 1) * DAY_MS);
        for off in [utc as fn(Date) -> i32, berlin_winter, la] {
            assert_eq!(local_days(&e, &off), (day.to_days(), day.to_days()));
        }
        // A three day trip covers three days and not the exclusive end.
        let trip = ev(true, day.to_days() * DAY_MS, (day.to_days() + 3) * DAY_MS);
        assert_eq!(local_days(&trip, &la), (day.to_days(), day.to_days() + 2));
    }

    #[test]
    fn timed_events_move_with_the_viewers_zone() {
        // 23:30 UTC on the 24th is 00:30 on the 25th in Berlin and 15:30 on the 24th in Los Angeles.
        let start = (d(2026, 11, 24).to_days() * 86_400 + 23 * 3600 + 1800) * 1000;
        let e = ev(false, start, start + 3_600_000);
        assert_eq!(local_days(&e, &berlin_winter).0, d(2026, 11, 25).to_days());
        assert_eq!(local_days(&e, &la).0, d(2026, 11, 24).to_days());
        assert_eq!(local_days(&e, &utc).0, d(2026, 11, 24).to_days());
    }

    #[test]
    fn midnight_end_does_not_touch_the_next_day_and_overnight_spans_two() {
        let mid = d(2026, 11, 25).to_days() * DAY_MS;
        let e = ev(false, mid - 3_600_000, mid);
        assert_eq!(
            local_days(&e, &utc),
            (d(2026, 11, 24).to_days(), d(2026, 11, 24).to_days())
        );
        let night = ev(false, mid - 3_600_000, mid + 3_600_000);
        assert_eq!(
            local_days(&night, &utc),
            (d(2026, 11, 24).to_days(), d(2026, 11, 25).to_days())
        );
        // A zero-length event still has a day.
        let z = ev(false, mid, mid);
        assert_eq!(local_days(&z, &utc), (mid / DAY_MS, mid / DAY_MS));
    }

    #[test]
    fn day_lists_put_all_day_first() {
        let day = d(2026, 11, 24);
        let base = day.to_days() * DAY_MS;
        let a = ev(false, base + 9 * 3_600_000, base + 10 * 3_600_000);
        let b = ev(true, base, base + DAY_MS);
        let c = ev(false, base + 8 * 3_600_000, base + 9 * 3_600_000);
        let all = [&a, &b, &c];
        let got = on_day(&all, day, &utc);
        assert_eq!(
            got.iter().map(|e| e.start_ms).collect::<Vec<_>>(),
            vec![b.start_ms, c.start_ms, a.start_ms]
        );
        assert!(on_day(&all, day.add_days(1), &utc).is_empty());
    }

    #[test]
    fn month_grid_is_monday_first_and_covers_the_month() {
        let g = month_grid(d(2026, 11, 15)); // 1 Nov 2026 is a Sunday
        assert_eq!(g[0][0], d(2026, 10, 26));
        assert_eq!(g[0][6], d(2026, 11, 1));
        assert!(g.iter().flatten().any(|x| *x == d(2026, 11, 30)));
        assert_eq!(g[5][6], d(2026, 12, 6));
        assert_eq!(add_months(d(2026, 11, 30), 2), d(2027, 1, 1));
        assert_eq!(add_months(d(2026, 1, 31), -1), d(2025, 12, 1));
        assert_eq!(days_in_month(2028, 2), 29);
    }

    #[test]
    fn overlapping_events_get_separate_columns() {
        let (cols, n) = columns(&[(0, 10), (5, 15), (10, 20), (30, 40)]);
        assert_eq!(cols, vec![0, 1, 0, 0]);
        assert_eq!(n, 2);
        assert_eq!(columns(&[]).1, 1);
    }

    #[test]
    fn editor_converts_local_times_to_utc_and_all_day_to_midnights() {
        let mut f = Fields::new_on(d(2026, 11, 24), 14);
        f.title = " Dentist ".into();
        let draft = f.to_draft("Europe/Berlin", &berlin_winter).unwrap();
        assert_eq!(draft.title, "Dentist");
        // 14:00 Berlin (UTC+1) = 13:00 UTC.
        assert_eq!(
            draft.start_ms,
            (d(2026, 11, 24).to_days() * 86_400 + 13 * 3600) * 1000
        );
        assert_eq!(draft.end_ms - draft.start_ms, 3_600_000);
        assert_eq!(draft.tz, "Europe/Berlin");
        assert_eq!(draft.reminder_min, Some(10));

        f.all_day = true;
        f.end_date = "2026-11-26".into();
        let a = f.to_draft("Europe/Berlin", &berlin_winter).unwrap();
        assert_eq!(a.start_ms % DAY_MS, 0);
        assert_eq!(a.end_ms - a.start_ms, 3 * DAY_MS);
        assert_eq!(a.tz, "UTC");
    }

    #[test]
    fn editor_rejects_bad_input_with_a_plain_sentence() {
        let ok = {
            let mut f = Fields::new_on(d(2026, 11, 24), 9);
            f.title = "x".into();
            f
        };
        let err = |f: &Fields| f.to_draft("UTC", &utc).unwrap_err();
        assert!(
            err(&Fields {
                title: "  ".into(),
                ..ok.clone()
            })
            .contains("title")
        );
        assert!(
            err(&Fields {
                start_date: "24.11.2026".into(),
                ..ok.clone()
            })
            .contains("start date")
        );
        assert!(
            err(&Fields {
                start_date: "2026-02-30".into(),
                ..ok.clone()
            })
            .contains("start date")
        );
        assert!(
            err(&Fields {
                start_time: "25:00".into(),
                ..ok.clone()
            })
            .contains("start time")
        );
        assert!(
            err(&Fields {
                end_time: "08:00".into(),
                ..ok.clone()
            })
            .contains("end after")
        );
        assert!(
            err(&Fields {
                all_day: true,
                end_date: "2026-11-23".into(),
                ..ok
            })
            .contains("before")
        );
    }

    #[test]
    fn editing_prefills_in_local_time_and_round_trips() {
        let start = (d(2026, 11, 24).to_days() * 86_400 + 13 * 3600) * 1000; // 14:00 Berlin
        let mut e = ev(false, start, start + 5_400_000);
        e.title = "Dentist".into();
        e.reminders_min = vec![30];
        let f = Fields::from_event(&e, &berlin_winter);
        assert_eq!(
            (f.start_date.as_str(), f.start_time.as_str()),
            ("2026-11-24", "14:00")
        );
        assert_eq!(f.end_time, "15:30");
        assert_eq!(f.reminder, Some(30));
        let draft = f.to_draft("Europe/Berlin", &berlin_winter).unwrap();
        assert_eq!((draft.start_ms, draft.end_ms), (e.start_ms, e.end_ms));
    }

    #[test]
    fn dates_and_times_parse_strictly() {
        assert_eq!(parse_date("2028-02-29"), Some(d(2028, 2, 29)));
        assert_eq!(parse_date("2027-02-29"), None);
        assert_eq!(parse_date("2026-1-5"), Some(d(2026, 1, 5)));
        assert_eq!(parse_date("26-01-05"), None);
        assert_eq!(parse_date("2026-01-05-1"), None);
        assert_eq!(parse_time("9:05"), Some(9 * 3600 + 300));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("09:5"), None);
    }

    #[test]
    fn labels_name_the_account_and_reminders_read_naturally() {
        let c = CalendarItem {
            device_id: "d".into(),
            id: "1".into(),
            name: "Work".into(),
            account: Some("me@gmail.com".into()),
            account_type: Some("com.google".into()),
            color: Some(0xFF112233),
            writable: true,
        };
        assert_eq!(calendar_label(&c), "Work (me@gmail.com)");
        assert!(is_google(&c));
        assert_eq!(calendar_color(Some(&c), 0), [0x11, 0x22, 0x33]);
        assert_eq!(calendar_color(None, 1), [0x43, 0xA0, 0x47]);
        assert_eq!(reminder_label(0), "At the start");
        assert_eq!(reminder_label(10), "10 minutes before");
        assert_eq!(reminder_label(60), "1 hour before");
        assert_eq!(reminder_label(2880), "2 days before");
    }
}
