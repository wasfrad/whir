//! "2 min ago", "Yesterday 4:12 PM", "Mon 4:12 PM", "Sep 12" — how the drawer
//! shows when a note was saved.

use whir_core::time::{self, LocalTime};

/// Locale formatting, injectable for tests.
pub trait Formats {
    fn hm(&self, t: &LocalTime) -> String;
    fn weekday(&self, t: &LocalTime) -> String;
    fn date(&self, t: &LocalTime, with_year: bool) -> String;
}

/// The user's Windows locale.
pub struct SystemFormats;

impl Formats for SystemFormats {
    fn hm(&self, t: &LocalTime) -> String {
        time::format_hm(t)
    }
    fn weekday(&self, t: &LocalTime) -> String {
        time::format_weekday(t)
    }
    fn date(&self, t: &LocalTime, with_year: bool) -> String {
        time::format_date(t, with_year)
    }
}

/// Label for `then`, seen at `now`; `elapsed_s` is the exact age in seconds.
pub fn label(now: &LocalTime, then: &LocalTime, elapsed_s: i64, f: &impl Formats) -> String {
    let days = now.day_number() - then.day_number();
    if (-300..60).contains(&elapsed_s) {
        return "Just now".into();
    }
    if (0..3600).contains(&elapsed_s) {
        return format!("{} min ago", elapsed_s / 60);
    }
    match days {
        0 => f.hm(then),
        1 => format!("Yesterday {}", f.hm(then)),
        2..=6 => format!("{} {}", f.weekday(then), f.hm(then)),
        _ => f.date(then, then.year != now.year),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct En;
    impl Formats for En {
        fn hm(&self, t: &LocalTime) -> String {
            let h = match t.hour % 12 {
                0 => 12,
                h => h,
            };
            format!(
                "{h}:{:02} {}",
                t.minute,
                if t.hour < 12 { "AM" } else { "PM" }
            )
        }
        fn weekday(&self, t: &LocalTime) -> String {
            ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][t.weekday as usize].into()
        }
        fn date(&self, t: &LocalTime, with_year: bool) -> String {
            let m = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ][t.month as usize - 1];
            if with_year {
                format!("{m} {}, {}", t.day, t.year)
            } else {
                format!("{m} {}", t.day)
            }
        }
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> LocalTime {
        // 1970-01-01 was a Thursday.
        let wd = (time::days_from_civil(year, month, day) + 4).rem_euclid(7) as u32;
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            weekday: wd,
        }
    }

    #[test]
    fn buckets() {
        let now = at(2026, 9, 23, 0, 30);
        assert_eq!(label(&now, &at(2026, 9, 23, 0, 30), 20, &En), "Just now");
        assert_eq!(label(&now, &at(2026, 9, 23, 0, 25), 300, &En), "5 min ago");
        // 00:30 vs 23:59 the day before: "Yesterday", not "31 min ago" rules
        // (under an hour wins: it *is* 31 minutes ago).
        assert_eq!(
            label(&now, &at(2026, 9, 22, 23, 59), 31 * 60, &En),
            "31 min ago"
        );
        assert_eq!(
            label(&now, &at(2026, 9, 22, 22, 0), 9000, &En),
            "Yesterday 10:00 PM"
        );
        let noon = at(2026, 9, 23, 12, 0);
        assert_eq!(
            label(&noon, &at(2026, 9, 23, 8, 5), 4 * 3600, &En),
            "8:05 AM"
        );
        assert_eq!(
            label(&noon, &at(2026, 9, 21, 16, 12), 2 * 86400, &En),
            "Mon 4:12 PM"
        );
        assert_eq!(
            label(&noon, &at(2026, 9, 16, 9, 0), 7 * 86400, &En),
            "Sep 16"
        );
        assert_eq!(
            label(&noon, &at(2025, 12, 31, 9, 0), 300 * 86400, &En),
            "Dec 31, 2025"
        );
        // Clock skew: a few minutes in the future is still "Just now".
        assert_eq!(label(&noon, &at(2026, 9, 23, 12, 2), -120, &En), "Just now");
    }

    #[test]
    fn new_year_edge() {
        let now = at(2027, 1, 1, 9, 0);
        assert_eq!(
            label(&now, &at(2026, 12, 31, 18, 0), 15 * 3600, &En),
            "Yesterday 6:00 PM"
        );
        assert_eq!(
            label(&now, &at(2026, 12, 20, 18, 0), 12 * 86400, &En),
            "Dec 20, 2026"
        );
    }
}
