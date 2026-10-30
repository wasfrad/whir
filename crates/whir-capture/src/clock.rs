//! The dock's side of time tracking: the timesheet store, the running timer,
//! and noticing time away. The dock draws it and passes clicks on; this is
//! what they do.

use whir_core::settings::{IniFile, app_data_file};
use whir_core::time::{Stamp, format_duration, format_hm, monday_of};
use whir_core::timesheet::{self, Grid, Running, State, Store};
use whir_platform::ipc::{self, Changed};

use crate::away::{self, Away, Watch};
use crate::panel::{Group, Model, RunningRow, TimerRow};

/// How often the dock writes down that it was running (in `dock.ini`): a
/// timer left running while whir wasn't is asked about from there.
const SEEN_EVERY_SECS: i64 = 5 * 60;

pub struct Clock {
    /// `None`: no Documents folder to keep time in.
    store: Option<Store>,
    watch: Watch,
    /// Time away, waiting for "keep" or "drop".
    pub away: Option<Away>,
    seen_saved: Option<Stamp>,
}

impl Clock {
    pub fn new() -> Clock {
        let store = timesheet::resolve_dir()
            .map(|dir| Store::open(dir).on_change(|| ipc::notify_changed(Changed::Time)));
        let mut clock = Clock {
            store,
            watch: Watch::default(),
            away: None,
            seen_saved: None,
        };
        clock.refresh();
        clock.away_while_not_running(Stamp::now());
        clock
    }

    /// A timer that ran on while the dock didn't (whir quit, the PC off):
    /// away from when the dock was last seen running.
    fn away_while_not_running(&mut self, now: Stamp) {
        let (Some(r), Some(threshold)) = (self.running(), self.threshold()) else {
            return;
        };
        let seen = IniFile::load(app_data_file("dock.ini"))
            .get("seen")
            .and_then(Stamp::parse);
        if let Some(seen) = seen
            && seen > r.start
            && now.since(seen) >= threshold
        {
            self.away = Some(Away {
                since: seen,
                until: now,
            });
        }
    }

    /// Read again what changed on disk. True if anything did.
    pub fn refresh(&mut self) -> bool {
        let changed = self.store.as_mut().is_some_and(Store::refresh);
        if self.running().is_none() {
            self.away = None;
            self.watch.reset();
        }
        changed
    }

    pub fn state(&self) -> Option<&State> {
        self.store.as_ref().map(Store::state)
    }

    pub fn running(&self) -> Option<&Running> {
        self.state()?.running.as_ref()
    }

    /// Seconds away that count, if asking is on.
    fn threshold(&self) -> Option<i64> {
        let minutes = self.state()?.options.away_minutes as i64;
        (minutes > 0).then_some(minutes * 60)
    }

    /// The dock stays out while a timer runs.
    pub fn keeps_out(&self) -> bool {
        self.running().is_some() && self.state().is_some_and(|s| s.options.keep_out)
    }

    /// A timer to resume (one has run before).
    pub fn can_resume(&self) -> bool {
        self.state()
            .and_then(|s| s.timer(s.last?))
            .is_some_and(|t| !t.archived)
    }

    /// "1:23" while a timer runs.
    pub fn elapsed(&self, now: Stamp) -> Option<String> {
        Some(format_duration(now.since(self.running()?.start)))
    }

    /// What the tray icon's tooltip says.
    pub fn tip(&self, now: Stamp) -> String {
        match (self.state(), self.running()) {
            (Some(s), Some(r)) => format!(
                "whir — {} {}",
                s.label(r.timer),
                format_duration(now.since(r.start))
            ),
            _ => "whir".to_owned(),
        }
    }

    /// What the panel shows (todos aside).
    pub fn model(&mut self, now: Stamp, expanded: bool) -> Model {
        let mut model = Model {
            expanded,
            away: self.away.map(|a| {
                format!(
                    "Away {} ({} – {})",
                    format_duration(a.seconds()),
                    format_hm(&a.since.local()),
                    format_hm(&a.until.local())
                )
            }),
            ..Model::default()
        };
        let Some(store) = &mut self.store else {
            return model;
        };
        model.available = true;
        if !expanded {
            return model;
        }
        let monday = monday_of(now.day);
        let week = store.week(monday).clone();
        let state = store.state();
        let grid = Grid::of(&week, state.running.as_ref(), now);
        let today = (now.day - monday) as usize;
        let running = state.running.as_ref().map(|r| r.timer);
        model.groups = state
            .by_customer(false, false)
            .into_iter()
            .map(|(c, timers)| Group {
                customer: c.name.clone(),
                timers: timers
                    .into_iter()
                    .map(|t| {
                        let secs = grid.cell(t.id, today);
                        TimerRow {
                            id: t.id,
                            name: t.name.clone(),
                            today: if secs > 0 {
                                format_duration(secs)
                            } else {
                                String::new()
                            },
                            running: running == Some(t.id),
                        }
                    })
                    .collect(),
            })
            .collect();
        model.running = state.running.as_ref().map(|r| RunningRow {
            label: state.label(r.timer),
            elapsed: format_duration(now.since(r.start)),
            note: r.note.clone(),
        });
        model.totals = format!(
            "Today {} · Week {}",
            format_duration(grid.day_total(today)),
            format_duration(grid.total())
        );
        model
    }

    fn change(&mut self, change: impl FnOnce(&mut timesheet::Txn)) {
        let Some(store) = &mut self.store else { return };
        if let Err(e) = store.update(change) {
            crate::report(&e);
        }
    }

    /// The timer button: pause, or resume the last timer.
    pub fn toggle(&mut self) {
        let now = Stamp::now();
        self.change(|tx| tx.toggle(now));
        self.started_or_stopped();
    }

    pub fn start(&mut self, timer: u32) {
        let now = Stamp::now();
        self.change(|tx| tx.start(timer, now));
        self.started_or_stopped();
    }

    pub fn stop(&mut self) {
        let now = Stamp::now();
        self.change(|tx| tx.stop(now));
        self.started_or_stopped();
    }

    fn started_or_stopped(&mut self) {
        // A question about the time before doesn't apply any more.
        self.away = None;
        self.watch.reset();
        self.seen_saved = None;
    }

    pub fn note(&self) -> String {
        self.running().map(|r| r.note.clone()).unwrap_or_default()
    }

    pub fn set_note(&mut self, note: &str) {
        self.change(|tx| tx.set_note(note));
    }

    /// The away time counts.
    pub fn keep_away(&mut self) {
        self.away = None;
        self.watch.reset();
    }

    /// It doesn't: the timer stops when the user left, and runs again from
    /// when they came back.
    pub fn drop_away(&mut self) {
        if let Some(a) = self.away.take() {
            self.change(|tx| tx.drop_away(a.since, a.until));
        }
        self.watch.reset();
    }

    /// Every little while, as long as a timer runs: notice time away, and
    /// write down that the dock is running. True if there is something new
    /// to ask.
    pub fn tick(&mut self, now: Stamp) -> bool {
        let Some(start) = self.running().map(|r| r.start) else {
            self.watch.reset();
            return false;
        };
        if self
            .seen_saved
            .is_none_or(|s| now.since(s) >= SEEN_EVERY_SECS)
        {
            let mut ini = IniFile::load(app_data_file("dock.ini"));
            ini.set("seen", &now.format());
            let _ = ini.save();
            self.seen_saved = Some(now);
        }
        let Some(threshold) = self.threshold() else {
            return false;
        };
        let Some(away) = self.watch.tick(now, away::idle_seconds(), threshold) else {
            return false;
        };
        // Only the part of it the timer was running for.
        let since = away.since.max(start);
        if away.until.since(since) < threshold {
            return false;
        }
        self.away = Some(match self.away {
            Some(a) => Away {
                since: a.since.min(since),
                until: a.until.max(away.until),
            },
            None => Away {
                since,
                until: away.until,
            },
        });
        true
    }
}
