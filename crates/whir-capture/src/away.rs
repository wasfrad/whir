//! Noticing that the user was away while a timer ran: no keyboard or mouse
//! for a while (a locked screen counts), or the PC asleep (the clock jumps
//! between ticks). Once they're back, the dock asks whether that time counts.

use whir_core::time::Stamp;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// A stretch the user was away for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Away {
    pub since: Stamp,
    pub until: Stamp,
}

impl Away {
    pub fn seconds(&self) -> i64 {
        self.until.since(self.since)
    }
}

/// Seconds since the last keyboard or mouse input in this session.
pub fn idle_seconds() -> i64 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        // Both wrap every 49.7 days; the difference doesn't care.
        GetTickCount().wrapping_sub(info.dwTime) as i64 / 1000
    }
}

/// Watches, tick by tick, while a timer runs.
#[derive(Debug, Default)]
pub struct Watch {
    /// The previous tick.
    last: Option<Stamp>,
    /// Away since then (not back yet).
    since: Option<Stamp>,
}

impl Watch {
    /// Start over (a timer started, or the question was answered).
    pub fn reset(&mut self) {
        *self = Watch::default();
    }

    /// One look, `idle` seconds after the last input. `threshold` seconds
    /// away is what counts (0: never). Returns the stretch once the user is
    /// back from it.
    pub fn tick(&mut self, now: Stamp, idle: i64, threshold: i64) -> Option<Away> {
        let last = self.last.replace(now);
        if threshold <= 0 {
            self.since = None;
            return None;
        }
        // Still gone (asleep counts: the tick count goes on while it sleeps).
        if idle >= threshold {
            let from = now.plus(-idle);
            self.since = Some(self.since.map_or(from, |s| s.min(from)));
            return None;
        }
        let back = now.plus(-idle);
        // Woken, and busy again before this tick: the clock jumped.
        if let Some(last) = last
            && now.since(last) >= threshold
        {
            let since = self.since.take().map_or(last, |s| s.min(last));
            return Some(Away { since, until: back });
        }
        let since = self.since.take()?;
        Some(Away { since, until: back })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60;

    fn at(minute: i64) -> Stamp {
        Stamp::at(20_000, 9, 0, 0).plus(minute * MIN)
    }

    #[test]
    fn short_breaks_are_not_away() {
        let mut w = Watch::default();
        for m in 0..30 {
            assert_eq!(w.tick(at(m), 4 * MIN, 10 * MIN), None);
        }
    }

    #[test]
    fn idle_long_enough_is_reported_once_back() {
        let mut w = Watch::default();
        assert_eq!(w.tick(at(0), 0, 10 * MIN), None);
        // Left at minute 1; still gone at 12 and 20.
        assert_eq!(w.tick(at(12), 11 * MIN, 10 * MIN), None);
        assert_eq!(w.tick(at(20), 19 * MIN, 10 * MIN), None);
        // Back a moment ago.
        let away = w.tick(at(21), 0, 10 * MIN).unwrap();
        assert_eq!(
            away,
            Away {
                since: at(1),
                until: at(21)
            }
        );
        assert_eq!(away.seconds(), 20 * MIN);
        // Asked once.
        assert_eq!(w.tick(at(22), 0, 10 * MIN), None);
    }

    #[test]
    fn sleeping_is_away_from_the_last_tick() {
        let mut w = Watch::default();
        w.tick(at(0), 0, 10 * MIN);
        // Woken an hour later by a key press, before the next tick.
        let away = w.tick(at(60), 0, 10 * MIN).unwrap();
        assert_eq!(
            away,
            Away {
                since: at(0),
                until: at(60)
            }
        );

        // Woken (by the PC itself) and not touched yet: asked once back.
        let mut w = Watch::default();
        w.tick(at(0), 2 * MIN, 10 * MIN);
        assert_eq!(w.tick(at(60), 62 * MIN, 10 * MIN), None);
        let away = w.tick(at(61), 0, 10 * MIN).unwrap();
        assert_eq!(
            away,
            Away {
                since: at(-2),
                until: at(61)
            }
        );
    }

    #[test]
    fn switched_off_never_asks() {
        let mut w = Watch::default();
        w.tick(at(0), 0, 0);
        assert_eq!(w.tick(at(30), 30 * MIN, 0), None);
        assert_eq!(w.tick(at(31), 0, 0), None);
    }
}
