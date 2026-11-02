// SPDX-License-Identifier: GPL-3.0-or-later
//! Calendar dates and the computer's local time zone, with no extra dependencies.
//!
//! Only the system clock and the time zone configured on this computer are used; nothing
//! here comes from a phone. Zone lookup uses the host C library on Unix and Windows
//! dynamic time-zone conversion APIs on Windows. Failed lookups explicitly fall back to UTC.
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32, // 1..=12
    pub day: u32,   // 1..=31
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

pub const MONTH_NAMES: [&str; 12] = month_table::EN;

/// Languages with built-in month and weekday names. Anything else shows English.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    De,
    Fr,
    Es,
    Pt,
    It,
}

/// How dates and clocks are shown on this computer, read once from `LC_ALL` / `LC_TIME` /
/// `LANG` (Unix). Windows and macOS GUI apps carry no such variables, so they show English with a
/// 24 h clock; `GEEKBRIDGE_CLOCK=12|24` overrides the clock everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Locale {
    pub lang: Lang,
    pub hour12: bool,
}

impl Locale {
    /// Parses a POSIX locale name such as `de_DE.UTF-8@euro`; unknown or `C` gives English / 24 h.
    pub fn parse(name: &str) -> Self {
        let base = name.split(['.', '@']).next().unwrap_or("");
        let (lang_code, territory) = base.split_once('_').unwrap_or((base, ""));
        let lang = match lang_code.to_ascii_lowercase().as_str() {
            "de" => Lang::De,
            "fr" => Lang::Fr,
            "es" => Lang::Es,
            "pt" => Lang::Pt,
            "it" => Lang::It,
            _ => Lang::En,
        };
        let hour12 = matches!(lang_code.to_ascii_lowercase().as_str(), "hi" | "ar" | "ko")
            || matches!(
                territory.to_ascii_uppercase().as_str(),
                "US" | "CA" | "AU" | "NZ" | "IN" | "PH" | "MX" | "CO" | "EG" | "SA" | "PK" | "BD"
            );
        Self { lang, hour12 }
    }

    /// From the environment values, in POSIX precedence; `clock` is `GEEKBRIDGE_CLOCK`.
    pub fn from_env_values(
        all: Option<&str>,
        time: Option<&str>,
        lang: Option<&str>,
        clock: Option<&str>,
    ) -> Self {
        let name = [all, time, lang]
            .into_iter()
            .flatten()
            .find(|v| !v.is_empty())
            .unwrap_or("");
        let mut locale = Self::parse(name);
        match clock {
            Some("12") => locale.hour12 = true,
            Some("24") => locale.hour12 = false,
            _ => {}
        }
        locale
    }

    /// The computer's locale. Tests always see English / 24 h so expectations stay fixed.
    pub fn current() -> Self {
        #[cfg(test)]
        {
            Self::default()
        }
        #[cfg(not(test))]
        {
            static CURRENT: std::sync::OnceLock<Locale> = std::sync::OnceLock::new();
            *CURRENT.get_or_init(|| {
                let var = |k: &str| std::env::var(k).ok();
                Self::from_env_values(
                    var("LC_ALL").as_deref(),
                    var("LC_TIME").as_deref(),
                    var("LANG").as_deref(),
                    var("GEEKBRIDGE_CLOCK").as_deref(),
                )
            })
        }
    }

    /// "14:05" or, with a 12 h clock, "2:05 PM".
    pub fn clock(self, seconds_into_day: u32) -> String {
        let (h, m) = (seconds_into_day / 3600 % 24, seconds_into_day / 60 % 60);
        if self.hour12 {
            let suffix = if h < 12 { "AM" } else { "PM" };
            format!("{}:{m:02} {suffix}", if h % 12 == 0 { 12 } else { h % 12 })
        } else {
            format!("{h:02}:{m:02}")
        }
    }

    pub fn month_name(self, month: u32) -> &'static str {
        month_table::months(self.lang)[(month.clamp(1, 12) - 1) as usize]
    }

    /// Hour-grid label: "14:00" or "2 PM".
    pub fn hour_label(self, hour: u32) -> String {
        if self.hour12 {
            let h = hour % 24;
            format!(
                "{} {}",
                if h.is_multiple_of(12) { 12 } else { h % 12 },
                if h < 12 { "AM" } else { "PM" }
            )
        } else {
            format!("{:02}:00", hour % 24)
        }
    }
}

mod month_table {
    use super::Lang;
    pub const EN: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const DE: [&str; 12] = [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ];
    const FR: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    const ES: [&str; 12] = [
        "enero",
        "febrero",
        "marzo",
        "abril",
        "mayo",
        "junio",
        "julio",
        "agosto",
        "septiembre",
        "octubre",
        "noviembre",
        "diciembre",
    ];
    const PT: [&str; 12] = [
        "janeiro",
        "fevereiro",
        "março",
        "abril",
        "maio",
        "junho",
        "julho",
        "agosto",
        "setembro",
        "outubro",
        "novembro",
        "dezembro",
    ];
    const IT: [&str; 12] = [
        "gennaio",
        "febbraio",
        "marzo",
        "aprile",
        "maggio",
        "giugno",
        "luglio",
        "agosto",
        "settembre",
        "ottobre",
        "novembre",
        "dicembre",
    ];
    pub fn months(lang: Lang) -> &'static [&'static str; 12] {
        match lang {
            Lang::En => &EN,
            Lang::De => &DE,
            Lang::Fr => &FR,
            Lang::Es => &ES,
            Lang::Pt => &PT,
            Lang::It => &IT,
        }
    }

    // Monday first. (full, short)
    type Days = [(&'static str, &'static str); 7];
    const D_EN: Days = [
        ("Monday", "Mon"),
        ("Tuesday", "Tue"),
        ("Wednesday", "Wed"),
        ("Thursday", "Thu"),
        ("Friday", "Fri"),
        ("Saturday", "Sat"),
        ("Sunday", "Sun"),
    ];
    const D_DE: Days = [
        ("Montag", "Mo"),
        ("Dienstag", "Di"),
        ("Mittwoch", "Mi"),
        ("Donnerstag", "Do"),
        ("Freitag", "Fr"),
        ("Samstag", "Sa"),
        ("Sonntag", "So"),
    ];
    const D_FR: Days = [
        ("lundi", "lun."),
        ("mardi", "mar."),
        ("mercredi", "mer."),
        ("jeudi", "jeu."),
        ("vendredi", "ven."),
        ("samedi", "sam."),
        ("dimanche", "dim."),
    ];
    const D_ES: Days = [
        ("lunes", "lun"),
        ("martes", "mar"),
        ("miércoles", "mié"),
        ("jueves", "jue"),
        ("viernes", "vie"),
        ("sábado", "sáb"),
        ("domingo", "dom"),
    ];
    const D_PT: Days = [
        ("segunda-feira", "seg"),
        ("terça-feira", "ter"),
        ("quarta-feira", "qua"),
        ("quinta-feira", "qui"),
        ("sexta-feira", "sex"),
        ("sábado", "sáb"),
        ("domingo", "dom"),
    ];
    const D_IT: Days = [
        ("lunedì", "lun"),
        ("martedì", "mar"),
        ("mercoledì", "mer"),
        ("giovedì", "gio"),
        ("venerdì", "ven"),
        ("sabato", "sab"),
        ("domenica", "dom"),
    ];
    pub fn days(lang: Lang) -> &'static Days {
        match lang {
            Lang::En => &D_EN,
            Lang::De => &D_DE,
            Lang::Fr => &D_FR,
            Lang::Es => &D_ES,
            Lang::Pt => &D_PT,
            Lang::It => &D_IT,
        }
    }
}

impl Weekday {
    /// Full name in the computer's language.
    pub fn name(self) -> &'static str {
        self.name_in(Locale::current().lang)
    }
    /// Abbreviation in the computer's language.
    pub fn short(self) -> &'static str {
        self.short_in(Locale::current().lang)
    }
    pub fn name_in(self, lang: Lang) -> &'static str {
        month_table::days(lang)[self as usize].0
    }
    pub fn short_in(self, lang: Lang) -> &'static str {
        month_table::days(lang)[self as usize].1
    }
}

impl Date {
    /// Days since 1970-01-01 (proleptic Gregorian; Hinnant's `days_from_civil`).
    pub fn to_days(self) -> i64 {
        let y = i64::from(self.year) - i64::from(self.month <= 2);
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = i64::from(self.month);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.day) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Self { year, month, day }
    }

    pub fn add_days(self, delta: i64) -> Self {
        Self::from_days(self.to_days() + delta)
    }

    pub fn weekday(self) -> Weekday {
        // 1970-01-01 was a Thursday.
        match (self.to_days() + 3).rem_euclid(7) {
            0 => Weekday::Monday,
            1 => Weekday::Tuesday,
            2 => Weekday::Wednesday,
            3 => Weekday::Thursday,
            4 => Weekday::Friday,
            5 => Weekday::Saturday,
            _ => Weekday::Sunday,
        }
    }

    /// The Monday on or before this date.
    pub fn week_start(self) -> Self {
        self.add_days(-(self.weekday() as i64))
    }

    pub fn month_name(self) -> &'static str {
        Locale::current().month_name(self.month)
    }
}

/// Wall-clock "now" in the zone the date was computed for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub date: Date,
    pub seconds_into_day: u32,
    /// False when the computer's zone could not be read and UTC is shown instead.
    pub local_zone_known: bool,
}

pub fn split_unix(unix_seconds: i64, offset_seconds: i32) -> (Date, u32) {
    let local = unix_seconds + i64::from(offset_seconds);
    (
        Date::from_days(local.div_euclid(86_400)),
        local.rem_euclid(86_400) as u32,
    )
}

pub fn now() -> Now {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let offset = utc_offset_seconds(unix);
    let (date, seconds_into_day) = split_unix(unix, offset.unwrap_or(0));
    Now {
        date,
        seconds_into_day,
        local_zone_known: offset.is_some(),
    }
}

/// The computer's UTC offset in minutes around noon of `date` (0 when the zone cannot be read).
pub fn utc_offset_minutes(date: Date) -> i32 {
    utc_offset_seconds(date.to_days() * 86_400 + 43_200).map_or(0, |s| s / 60)
}

/// The time of day of a UTC millisecond instant shown at `offset_minutes`: `"HH:MM"`, or
/// `"h:MM AM"` when the computer's locale uses a 12 h clock.
pub fn clock_label(utc_ms: i64, offset_minutes: i32) -> String {
    clock_label_in(Locale::current(), utc_ms, offset_minutes)
}

/// The UTC offset in minutes that applied at the instant `utc_ms` (0 when the zone cannot be
/// read). Unlike `utc_offset_minutes`, this is right for a past or future instant on the other
/// side of a daylight-saving change.
pub fn utc_offset_minutes_at(utc_ms: i64) -> i32 {
    utc_offset_seconds(utc_ms.div_euclid(1000)).map_or(0, |s| s / 60)
}

/// `clock_label` at the offset that applied at `utc_ms` itself.
pub fn clock_label_local(utc_ms: i64) -> String {
    clock_label(utc_ms, utc_offset_minutes_at(utc_ms))
}

pub fn clock_label_in(locale: Locale, utc_ms: i64, offset_minutes: i32) -> String {
    let secs = (utc_ms / 1000 + i64::from(offset_minutes) * 60).rem_euclid(86_400);
    locale.clock(secs as u32)
}

#[cfg(all(unix, target_pointer_width = "64"))]
fn utc_offset_seconds(unix_seconds: i64) -> Option<i32> {
    use std::ffi::{c_char, c_int, c_long};

    // Layout shared by glibc, musl and the BSD/macOS C libraries on 64-bit targets.
    #[repr(C)]
    struct Tm {
        tm_sec: c_int,
        tm_min: c_int,
        tm_hour: c_int,
        tm_mday: c_int,
        tm_mon: c_int,
        tm_year: c_int,
        tm_wday: c_int,
        tm_yday: c_int,
        tm_isdst: c_int,
        tm_gmtoff: c_long,
        tm_zone: *const c_char,
    }
    unsafe extern "C" {
        fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
    }

    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    // SAFETY: both pointers refer to live, properly aligned locals; `localtime_r` is
    // re-entrant and only writes into `tm`.
    let ok = unsafe { !localtime_r(&unix_seconds, &mut tm).is_null() };
    (ok && tm.tm_gmtoff.abs() < 24 * 3600).then_some(tm.tm_gmtoff as i32)
}

#[cfg(any(windows, test))]
fn windows_filetime_ticks(unix_seconds: i64) -> Option<u64> {
    // FILETIME counts 100ns units since 1601-01-01, 11,644,473,600 seconds before Unix.
    let seconds = u64::try_from(unix_seconds.checked_add(11_644_473_600)?).ok()?;
    seconds.checked_mul(10_000_000)
}

#[cfg(windows)]
fn utc_offset_seconds(unix_seconds: i64) -> Option<i32> {
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{
            DYNAMIC_TIME_ZONE_INFORMATION, FileTimeToSystemTime, GetDynamicTimeZoneInformation,
            SystemTimeToTzSpecificLocalTimeEx,
        },
    };
    let ticks = windows_filetime_ticks(unix_seconds)?;
    let filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY: all three Win32 output structures contain only integer fields and fixed arrays;
    // zero is a valid initial representation and the APIs fill them before they are read.
    let (mut utc, mut local, mut zone): (SYSTEMTIME, SYSTEMTIME, DYNAMIC_TIME_ZONE_INFORMATION) =
        unsafe { std::mem::zeroed() };
    // SAFETY: pointers reference valid, aligned structures for the duration of each call.
    // The system APIs are read-only; no machine time-zone settings are changed.
    let success = unsafe {
        FileTimeToSystemTime(&filetime, &mut utc) != 0
            && GetDynamicTimeZoneInformation(&mut zone) != u32::MAX
            && SystemTimeToTzSpecificLocalTimeEx(&zone, &utc, &mut local) != 0
    };
    if !success {
        return None;
    }
    let date = Date {
        year: i32::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
    };
    let seconds = date.to_days().checked_mul(86_400)?.checked_add(
        i64::from(local.wHour) * 3600 + i64::from(local.wMinute) * 60 + i64::from(local.wSecond),
    )?;
    let offset = i32::try_from(seconds.checked_sub(unix_seconds)?).ok()?;
    (offset.unsigned_abs() < 86_400).then_some(offset)
}

#[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
fn utc_offset_seconds(_unix_seconds: i64) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_epoch_conversion_handles_boundaries() {
        assert_eq!(windows_filetime_ticks(0), Some(116_444_736_000_000_000));
        assert_eq!(windows_filetime_ticks(-11_644_473_600), Some(0));
        assert_eq!(windows_filetime_ticks(-11_644_473_601), None);
        assert_eq!(windows_filetime_ticks(i64::MAX), None);
    }

    fn d(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn civil_round_trip_and_weekdays() {
        assert_eq!(d(1970, 1, 1).to_days(), 0);
        assert_eq!(d(1970, 1, 1).weekday(), Weekday::Thursday);
        assert_eq!(d(2026, 11, 1).weekday(), Weekday::Sunday);
        assert_eq!(d(2000, 2, 29).weekday(), Weekday::Tuesday);
        for days in (-800_000..800_000).step_by(997) {
            assert_eq!(Date::from_days(days).to_days(), days);
        }
    }

    #[test]
    fn navigation_crosses_boundaries() {
        assert_eq!(d(2026, 12, 31).add_days(1), d(2027, 1, 1));
        assert_eq!(d(2028, 3, 1).add_days(-1), d(2028, 2, 29));
        assert_eq!(d(2026, 11, 1).week_start(), d(2026, 10, 26));
        assert_eq!(d(2026, 10, 26).week_start(), d(2026, 10, 26));
        assert_eq!(d(2026, 11, 1).month_name(), "November");
    }

    #[test]
    fn locale_names_come_from_the_env_and_fall_back_to_english() {
        let l = |v: &str| Locale::from_env_values(None, None, Some(v), None);
        assert_eq!(l("de_DE.UTF-8").lang, Lang::De);
        assert_eq!(
            l("fr_CA.UTF-8"),
            Locale {
                lang: Lang::Fr,
                hour12: true
            }
        );
        assert_eq!(l("pt_BR").lang, Lang::Pt);
        assert_eq!(l("it_IT@euro").lang, Lang::It);
        assert_eq!(
            l("es_MX.UTF-8"),
            Locale {
                lang: Lang::Es,
                hour12: true
            }
        );
        assert_eq!(
            l("en_US.UTF-8"),
            Locale {
                lang: Lang::En,
                hour12: true
            }
        );
        assert_eq!(
            l("en_GB.UTF-8"),
            Locale {
                lang: Lang::En,
                hour12: false
            }
        );
        for unknown in ["C", "POSIX", "", "ja_JP.UTF-8", "xx"] {
            assert_eq!(l(unknown).lang, Lang::En, "{unknown}");
        }
        // POSIX precedence: LC_ALL > LC_TIME > LANG; empty values are skipped.
        let p = Locale::from_env_values(Some(""), Some("de_DE"), Some("fr_FR"), None);
        assert_eq!(p.lang, Lang::De);
        let all = Locale::from_env_values(Some("it_IT"), Some("de_DE"), Some("fr_FR"), None);
        assert_eq!(all.lang, Lang::It);
        // The explicit clock switch wins over the locale.
        assert!(Locale::from_env_values(None, None, Some("de_DE"), Some("12")).hour12);
        assert!(!Locale::from_env_values(None, None, Some("en_US"), Some("24")).hour12);
    }

    #[test]
    fn localised_months_and_weekdays() {
        let nov1 = d(2026, 11, 1); // a Sunday
        assert_eq!(Locale::parse("de_DE").month_name(3), "März");
        assert_eq!(Locale::parse("fr_FR").month_name(2), "février");
        assert_eq!(Locale::parse("es_ES").month_name(9), "septiembre");
        assert_eq!(Locale::parse("pt_PT").month_name(12), "dezembro");
        assert_eq!(Locale::parse("it_IT").month_name(1), "gennaio");
        assert_eq!(nov1.weekday().name_in(Lang::De), "Sonntag");
        assert_eq!(nov1.weekday().short_in(Lang::Fr), "dim.");
        assert_eq!(nov1.weekday().name_in(Lang::Es), "domingo");
        assert_eq!(nov1.weekday().short_in(Lang::Pt), "dom");
        assert_eq!(nov1.weekday().name_in(Lang::It), "domenica");
        assert_eq!(nov1.weekday().name_in(Lang::En), "Sunday");
        // Every table entry is filled and distinct per language.
        for lang in [Lang::En, Lang::De, Lang::Fr, Lang::Es, Lang::Pt, Lang::It] {
            let months: Vec<_> = (1..=12).map(|m| month_table::months(lang)[m - 1]).collect();
            let mut u = months.clone();
            u.sort();
            u.dedup();
            assert_eq!(u.len(), 12, "{lang:?}");
            for w in 0..7 {
                let day = Date::from_days(4 + w).weekday();
                assert!(!day.name_in(lang).is_empty() && !day.short_in(lang).is_empty());
            }
        }
    }

    #[test]
    fn clocks_in_24_and_12_hours() {
        let h24 = Locale::default();
        let h12 = Locale {
            lang: Lang::En,
            hour12: true,
        };
        assert_eq!(h24.clock(14 * 3600 + 5 * 60), "14:05");
        assert_eq!(h12.clock(14 * 3600 + 5 * 60), "2:05 PM");
        assert_eq!(h12.clock(0), "12:00 AM");
        assert_eq!(h12.clock(12 * 3600), "12:00 PM");
        assert_eq!(h12.clock(23 * 3600 + 59 * 60), "11:59 PM");
        assert_eq!(h24.hour_label(7), "07:00");
        assert_eq!(h12.hour_label(0), "12 AM");
        assert_eq!(h12.hour_label(13), "1 PM");
        // 2026-11-02 14:05 UTC shown at UTC+1.
        let ms = (d(2026, 11, 2).to_days() * 86_400 + 14 * 3600 + 300) * 1000;
        assert_eq!(clock_label_in(h24, ms, 60), "15:05");
        assert_eq!(clock_label_in(h12, ms, 60), "3:05 PM");
        assert_eq!(clock_label_in(h12, ms, -14 * 60 - 5), "12:00 AM");
    }

    #[test]
    fn splits_unix_time_with_offset() {
        // 2026-11-01 23:30:00 UTC is already the 2nd at UTC+1.
        let unix = d(2026, 11, 1).to_days() * 86_400 + 23 * 3600 + 1800;
        assert_eq!(split_unix(unix, 0), (d(2026, 11, 1), 23 * 3600 + 1800));
        assert_eq!(split_unix(unix, 3600), (d(2026, 11, 2), 1800));
        assert_eq!(split_unix(unix, -86_400 * 2).0, d(2026, 10, 30));
    }

    #[cfg(all(unix, target_pointer_width = "64"))]
    #[test]
    fn the_offset_of_an_instant_follows_dst_not_today() {
        unsafe extern "C" {
            fn tzset();
        }
        // POSIX TZ rule (no tzdata needed): CET, DST from the last Sunday of March to October.
        // SAFETY: this is the only test that touches TZ, and nothing else here depends on the zone.
        unsafe {
            std::env::set_var("TZ", "CET-1CEST,M3.5.0,M10.5.0/3");
            tzset();
        }
        let ms = |d: Date| (d.to_days() * 86_400 + 12 * 3600) * 1000;
        assert_eq!(utc_offset_minutes_at(ms(d(2026, 1, 15))), 60);
        assert_eq!(utc_offset_minutes_at(ms(d(2026, 7, 15))), 120);
        // A winter message shown in summer: 11:00 UTC is 12:00 in January, whatever today's offset.
        assert_eq!(clock_label_local(ms(d(2026, 1, 15)) - 3600 * 1000), "12:00");
        assert_eq!(clock_label_local(ms(d(2026, 7, 15)) - 3600 * 1000), "13:00");
    }

    #[test]
    fn now_is_plausible() {
        let n = now();
        assert!(n.date.year >= 2024 && n.seconds_into_day < 86_400);
    }
}
