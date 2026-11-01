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

pub const MONTH_NAMES: [&str; 12] = [
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

impl Weekday {
    pub fn name(self) -> &'static str {
        match self {
            Self::Monday => "Monday",
            Self::Tuesday => "Tuesday",
            Self::Wednesday => "Wednesday",
            Self::Thursday => "Thursday",
            Self::Friday => "Friday",
            Self::Saturday => "Saturday",
            Self::Sunday => "Sunday",
        }
    }
    pub fn short(self) -> &'static str {
        &self.name()[..3]
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
        MONTH_NAMES[(self.month - 1) as usize]
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
    fn splits_unix_time_with_offset() {
        // 2026-11-01 23:30:00 UTC is already the 2nd at UTC+1.
        let unix = d(2026, 11, 1).to_days() * 86_400 + 23 * 3600 + 1800;
        assert_eq!(split_unix(unix, 0), (d(2026, 11, 1), 23 * 3600 + 1800));
        assert_eq!(split_unix(unix, 3600), (d(2026, 11, 2), 1800));
        assert_eq!(split_unix(unix, -86_400 * 2).0, d(2026, 10, 30));
    }

    #[test]
    fn now_is_plausible() {
        let n = now();
        assert!(n.date.year >= 2024 && n.seconds_into_day < 86_400);
    }
}
