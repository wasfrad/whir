//! Local wall-clock time and locale formatting (the user's own date/time style).

use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{
    DATE_MONTHDAY, DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::core::PCWSTR;

/// A local calendar time, minute precision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

impl LocalTime {
    fn from_system(st: &SYSTEMTIME) -> Self {
        Self {
            year: st.wYear as i32,
            month: st.wMonth as u32,
            day: st.wDay as u32,
            hour: st.wHour as u32,
            minute: st.wMinute as u32,
            weekday: st.wDayOfWeek as u32,
        }
    }

    fn to_system(self) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: self.year as u16,
            wMonth: self.month as u16,
            wDayOfWeek: self.weekday as u16,
            wDay: self.day as u16,
            wHour: self.hour as u16,
            wMinute: self.minute as u16,
            wSecond: 0,
            wMilliseconds: 0,
        }
    }

    /// Days since 1970-01-01 of this calendar date.
    pub fn day_number(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }
}

pub fn now_local() -> LocalTime {
    LocalTime::from_system(&unsafe { GetLocalTime() })
}

const DAY: u32 = 24 * 60 * 60;

/// A local wall-clock moment to the second: how time entries are kept. Wall
/// clock, not UTC, so a day's entries are that day's however the clocks
/// change (an entry across a daylight-saving change is off by that hour).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    /// Days since 1970-01-01 of the local date.
    pub day: i64,
    /// Seconds since that day's midnight, `0..86400`.
    pub secs: u32,
}

impl Stamp {
    pub fn now() -> Stamp {
        let st = unsafe { GetLocalTime() };
        Stamp {
            day: days_from_civil(st.wYear as i32, st.wMonth as u32, st.wDay as u32),
            secs: st.wHour as u32 * 3600 + st.wMinute as u32 * 60 + st.wSecond as u32,
        }
    }

    pub fn at(day: i64, hour: u32, minute: u32, second: u32) -> Stamp {
        Stamp::from_seconds(day * DAY as i64 + (hour * 3600 + minute * 60 + second) as i64)
    }

    /// Midnight at the start of `day`.
    pub fn midnight(day: i64) -> Stamp {
        Stamp { day, secs: 0 }
    }

    /// Seconds since 1970-01-01 00:00 (wall clock): for differences.
    pub fn seconds(self) -> i64 {
        self.day * DAY as i64 + self.secs as i64
    }

    pub fn from_seconds(s: i64) -> Stamp {
        Stamp {
            day: s.div_euclid(DAY as i64),
            secs: s.rem_euclid(DAY as i64) as u32,
        }
    }

    pub fn plus(self, seconds: i64) -> Stamp {
        Stamp::from_seconds(self.seconds() + seconds)
    }

    /// Seconds from `earlier` to this.
    pub fn since(self, earlier: Stamp) -> i64 {
        self.seconds() - earlier.seconds()
    }

    pub fn hour(self) -> u32 {
        self.secs / 3600
    }

    pub fn minute(self) -> u32 {
        self.secs / 60 % 60
    }

    /// For the locale's formats ([`format_hm`], [`format_date`]).
    pub fn local(self) -> LocalTime {
        let (year, month, day) = civil_from_days(self.day);
        LocalTime {
            year,
            month,
            day,
            hour: self.hour(),
            minute: self.minute(),
            weekday: weekday(self.day),
        }
    }

    /// `2026-10-26T09:02:11`.
    pub fn format(self) -> String {
        let (y, m, d) = civil_from_days(self.day);
        let s = self.secs;
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
            s / 3600,
            s / 60 % 60,
            s % 60
        )
    }

    /// What [`Stamp::format`] writes.
    pub fn parse(text: &str) -> Option<Stamp> {
        let (date, time) = text.split_once('T')?;
        let day = parse_date(date)?;
        let mut parts = time.split(':').map(|p| p.parse::<u32>().ok());
        let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
        if parts.next().is_some() || h > 23 || m > 59 || s > 59 {
            return None;
        }
        Some(Stamp::at(day, h, m, s))
    }
}

/// `2026-10-26` as a day number.
pub fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.split('-').map(|p| p.parse::<u32>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y as i32, m)
    {
        return None;
    }
    Some(days_from_civil(y as i32, m, d))
}

/// `2026-10-26`.
pub fn format_iso_date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The local date of a day number (the inverse of [`days_from_civil`]).
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

/// 0 = Sunday (1970-01-01 was a Thursday).
pub fn weekday(day: i64) -> u32 {
    (day + 4).rem_euclid(7) as u32
}

/// The Monday of the week `day` is in: weeks run Monday to Sunday.
pub fn monday_of(day: i64) -> i64 {
    day - (weekday(day) as i64 + 6) % 7
}

/// "1:05" (hours and minutes) for a number of seconds; negative is "0:00".
pub fn format_duration(seconds: i64) -> String {
    let minutes = seconds.max(0) / 60;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// "1:02:33" (a stopwatch: hours, minutes and seconds); negative is "0:00:00".
pub fn format_clock(seconds: i64) -> String {
    let s = seconds.max(0);
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Convert a file timestamp to local time (daylight saving rules of that date).
pub fn to_local(t: SystemTime) -> Option<LocalTime> {
    // FILETIME: 100 ns ticks since 1601-01-01.
    const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
    let d = t.duration_since(UNIX_EPOCH).ok()?;
    let ticks = (d.as_secs() + EPOCH_DIFF_SECS) * 10_000_000 + d.subsec_nanos() as u64 / 100;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    unsafe {
        let mut utc = SYSTEMTIME::default();
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        let mut local = SYSTEMTIME::default();
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
        Some(LocalTime::from_system(&local))
    }
}

/// Days since 1970-01-01 (proleptic Gregorian), H. Hinnant's algorithm.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn wide_to_string(buf: &[u16], n: i32) -> Option<String> {
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

/// "4:12 PM" (or "16:12") in the user's locale.
pub fn format_hm(t: &LocalTime) -> String {
    let st = t.to_system();
    let mut buf = [0u16; 64];
    let n = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&st),
            PCWSTR::null(),
            Some(&mut buf),
        )
    };
    wide_to_string(&buf, n).unwrap_or_else(|| format!("{}:{:02}", t.hour, t.minute))
}

/// "September 12" (without year) or the short date ("9/12/2025").
pub fn format_date(t: &LocalTime, with_year: bool) -> String {
    let st = t.to_system();
    let mut buf = [0u16; 96];
    let flags = if with_year {
        DATE_SHORTDATE
    } else {
        DATE_MONTHDAY
    };
    let n = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            flags,
            Some(&st),
            PCWSTR::null(),
            Some(&mut buf),
            PCWSTR::null(),
        )
    };
    wide_to_string(&buf, n).unwrap_or_else(|| {
        if with_year {
            format!("{}-{:02}-{:02}", t.year, t.month, t.day)
        } else {
            format!("{}/{}", t.month, t.day)
        }
    })
}

/// Abbreviated weekday name ("Mon") in the user's locale.
pub fn format_weekday(t: &LocalTime) -> String {
    date_picture(t, "ddd").unwrap_or_else(|| {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.weekday as usize % 7].to_owned()
    })
}

/// "Thursday, October 29": a day, spelled out (names in the user's language).
pub fn format_long_day(t: &LocalTime) -> String {
    date_picture(t, "dddd, MMMM d").unwrap_or_else(|| format_date(t, false))
}

/// `t` as a `GetDateFormatEx` picture ("ddd", "MMMM d").
fn date_picture(t: &LocalTime, picture: &str) -> Option<String> {
    let st = t.to_system();
    let mut buf = [0u16; 96];
    let fmt: Vec<u16> = picture.encode_utf16().chain(Some(0)).collect();
    let n = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            Default::default(),
            Some(&st),
            PCWSTR(fmt.as_ptr()),
            Some(&mut buf),
            PCWSTR::null(),
        )
    };
    wide_to_string(&buf, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(
            days_from_civil(2026, 9, 23) - days_from_civil(2026, 9, 22),
            1
        );
        // Across a year and a leap day.
        assert_eq!(
            days_from_civil(2025, 1, 1) - days_from_civil(2024, 12, 31),
            1
        );
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 28),
            2
        );
    }

    #[test]
    fn civil_days_round_trip() {
        for day in [-800_000, -1, 0, 1, 11_017, 20_000, 20_754, 2_000_000] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m, d), day);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
    }

    #[test]
    fn weeks_start_on_monday() {
        let day = |y, m, d| days_from_civil(y, m, d);
        assert_eq!(weekday(day(2026, 10, 29)), 4); // a Thursday
        assert_eq!(monday_of(day(2026, 10, 29)), day(2026, 10, 26));
        assert_eq!(monday_of(day(2026, 10, 26)), day(2026, 10, 26));
        // Sunday belongs to the week before.
        assert_eq!(monday_of(day(2026, 11, 1)), day(2026, 10, 26));
        // Across a year.
        assert_eq!(monday_of(day(2027, 1, 1)), day(2026, 12, 28));
        assert_eq!(monday_of(day(1970, 1, 1)), day(1969, 12, 29));
    }

    #[test]
    fn stamps_print_parse_and_count() {
        let day = days_from_civil(2026, 10, 26);
        let s = Stamp::at(day, 9, 2, 11);
        assert_eq!(s.format(), "2026-10-26T09:02:11");
        assert_eq!(Stamp::parse("2026-10-26T09:02:11"), Some(s));
        for bad in [
            "",
            "2026-10-26",
            "2026-10-26T9:02",
            "2026-13-01T00:00:00",
            "2026-02-29T00:00:00",
            "2026-10-26T24:00:00",
            "2026-10-26T09:02:11:00",
            "x-10-26T09:02:11",
        ] {
            assert_eq!(Stamp::parse(bad), None, "{bad}");
        }
        assert_eq!(Stamp::parse("2024-02-29T23:59:59").unwrap().secs, 86_399);
        // Past midnight is the next day.
        assert_eq!(s.plus(15 * 3600), Stamp::at(day + 1, 0, 2, 11));
        assert_eq!(Stamp::at(day + 1, 0, 2, 11).since(s), 15 * 3600);
        assert!(Stamp::midnight(day + 1) > Stamp::at(day, 23, 59, 59));
        assert_eq!(s.local().weekday, 1);
        assert_eq!((s.local().hour, s.local().minute), (9, 2));
    }

    #[test]
    fn durations_are_hours_and_minutes() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:00");
        assert_eq!(format_duration(65 * 60), "1:05");
        assert_eq!(format_duration(12 * 3600 + 30 * 60), "12:30");
        assert_eq!(format_duration(-5), "0:00");
        assert_eq!(format_clock(0), "0:00:00");
        assert_eq!(format_clock(59), "0:00:59");
        assert_eq!(format_clock(3600 + 2 * 60 + 33), "1:02:33");
        assert_eq!(format_clock(36 * 3600), "36:00:00");
        assert_eq!(format_clock(-5), "0:00:00");
    }

    #[test]
    fn local_conversion_roundtrips_the_date() {
        let now = SystemTime::now();
        let a = to_local(now).unwrap();
        let b = now_local();
        assert!((a.day_number() - b.day_number()).abs() <= 1);
        assert!(!format_hm(&a).is_empty());
        assert!(!format_date(&a, true).is_empty());
        assert!(!format_weekday(&a).is_empty());
    }
}
