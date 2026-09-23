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
    let st = t.to_system();
    let mut buf = [0u16; 32];
    let fmt: Vec<u16> = "ddd".encode_utf16().chain(Some(0)).collect();
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
    wide_to_string(&buf, n).unwrap_or_else(|| {
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.weekday as usize % 7].to_owned()
    })
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
