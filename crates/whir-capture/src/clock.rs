//! The dock's side of time tracking: the timesheet store, the stopwatch on
//! the bar, picking a milestone, and noticing time away. The dock draws it and
//! passes clicks on; this is what they do.

use whir_core::settings::{IniFile, app_data_file};
use whir_core::time::{
    Stamp, format_clock, format_duration, format_hm, format_long_day, monday_of,
};
use whir_core::timesheet::{self, Grid, Running, State, Store};
use whir_platform::ipc::{self, Changed};

use crate::away::{self, Away, Watch};
use crate::panel::{Confirm, DayRow, Group, MilestoneRow, Model, View};

/// How often the dock writes down that it was running (in `dock.ini`): a
/// milestone left running while whir wasn't is asked about from there.
const SEEN_EVERY_SECS: i64 = 5 * 60;

/// What the bar shows of the time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Strip {
    /// The milestone ▶ plays (the running one, or the last): project and
    /// milestone names.
    pub milestone: Option<(String, String)>,
    pub playing: bool,
    /// Today's time on it, "1:02:33".
    pub time: String,
    /// Said where the milestone goes when there isn't one.
    pub hint: &'static str,
}

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

    /// A milestone that ran on while the dock didn't (whir quit, the PC off):
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

    /// The milestone ▶ plays: the running one, else the last one that ran
    /// (unless it has been archived or deleted since).
    pub fn selected(&self) -> Option<u32> {
        let state = self.state()?;
        if let Some(r) = &state.running {
            return Some(r.milestone);
        }
        state
            .milestone(state.last?)
            .filter(|t| !t.archived)
            .map(|t| t.id)
    }

    /// There are milestones to pick from.
    pub fn has_milestones(&self) -> bool {
        self.state()
            .is_some_and(|s| s.milestones.iter().any(|t| !t.archived))
    }

    /// Seconds away that count, if asking is on.
    fn threshold(&self) -> Option<i64> {
        let minutes = self.state()?.options.away_minutes as i64;
        (minutes > 0).then_some(minutes * 60)
    }

    /// The dock stays out while a milestone runs.
    pub fn keeps_out(&self) -> bool {
        self.running().is_some() && self.state().is_some_and(|s| s.options.keep_out)
    }

    /// This week's seconds per milestone per day, the running time included;
    /// and which day of it today is.
    fn today(&mut self, now: Stamp) -> Option<(Grid, usize)> {
        let store = self.store.as_mut()?;
        let monday = monday_of(now.day);
        let week = store.week(monday).clone();
        let grid = Grid::of(&week, store.state().running.as_ref(), now);
        Some((grid, (now.day - monday) as usize))
    }

    /// Today's seconds on `milestone`.
    fn today_on(&mut self, milestone: u32, now: Stamp) -> i64 {
        self.today(now)
            .map_or(0, |(grid, today)| grid.cell(milestone, today))
    }

    /// What the bar shows.
    pub fn strip(&mut self, now: Stamp) -> Strip {
        let hint = if self.store.is_none() {
            "No Documents folder"
        } else if self.has_milestones() {
            "Choose a milestone"
        } else {
            "Add a milestone…"
        };
        let Some(milestone) = self.selected() else {
            return Strip {
                milestone: None,
                playing: false,
                time: String::new(),
                hint,
            };
        };
        let seconds = self.today_on(milestone, now);
        let state = self.state().expect("a milestone was selected");
        let (project, milestone) = state.names(milestone);
        Strip {
            milestone: Some((project.to_owned(), milestone.to_owned())),
            playing: state.running.is_some(),
            time: format_clock(seconds),
            hint,
        }
    }

    /// What the tray icon's tooltip says (to the minute: it is set only when
    /// this changes).
    pub fn tip(&mut self, now: Stamp) -> String {
        let Some(milestone) = self.running().map(|r| r.milestone) else {
            return "whir".to_owned();
        };
        let seconds = self.today_on(milestone, now);
        match self.state() {
            Some(s) => format!("whir — {} {}", s.label(milestone), format_duration(seconds)),
            None => "whir".to_owned(),
        }
    }

    /// What the panel shows (todos aside): `view` open, and `confirm` the
    /// milestone the user picked while another runs.
    pub fn model(&mut self, now: Stamp, view: Option<View>, confirm: Option<u32>) -> Model {
        let mut model = Model {
            view,
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
        let Some((grid, today)) = self.today(now) else {
            return model;
        };
        model.available = true;
        let state = self.state().expect("a store");
        let running = state.running.as_ref().map(|r| r.milestone);
        if let (Some(to), Some(from)) = (confirm, running) {
            model.confirm = Some(Confirm {
                to,
                to_label: state.label(to),
                from_label: state.label(from),
                from_time: format_clock(grid.cell(from, today)),
            });
        }
        match view {
            Some(View::Today) => {
                let mut rows: Vec<DayRow> = grid
                    .rows
                    .iter()
                    .filter(|(_, days)| days[today] > 0)
                    .map(|&(t, days)| {
                        let (project, milestone) = state.names(t);
                        DayRow {
                            project: project.to_owned(),
                            milestone: milestone.to_owned(),
                            time: format_clock(days[today]),
                            running: running == Some(t),
                        }
                    })
                    .collect();
                rows.sort_by_key(|r| (r.project.to_lowercase(), r.milestone.to_lowercase()));
                model.today = rows;
                model.date = format_long_day(&now.local());
                model.total = format_clock(grid.day_total(today));
            }
            Some(View::Picker) => {
                model.groups = state
                    .by_project(false, false)
                    .into_iter()
                    .map(|(c, milestones)| Group {
                        project: c.name.clone(),
                        milestones: milestones
                            .into_iter()
                            .map(|t| {
                                let secs = grid.cell(t.id, today);
                                MilestoneRow {
                                    id: t.id,
                                    name: t.name.clone(),
                                    today: if secs > 0 {
                                        format_clock(secs)
                                    } else {
                                        String::new()
                                    },
                                    running: running == Some(t.id),
                                }
                            })
                            .collect(),
                    })
                    .collect();
            }
            None => {}
        }
        model
    }

    fn change(&mut self, change: impl FnOnce(&mut timesheet::Txn)) {
        let Some(store) = &mut self.store else { return };
        if let Err(e) = store.update(change) {
            crate::report(&e);
        }
    }

    /// ▶ / ⏸: pause, or resume the last milestone.
    pub fn toggle(&mut self) {
        let now = Stamp::now();
        self.change(|tx| tx.toggle(now));
        self.started_or_stopped();
    }

    /// Time `milestone` from now (stopping whatever runs).
    pub fn start(&mut self, milestone: u32) {
        let now = Stamp::now();
        self.change(|tx| tx.start(milestone, now));
        self.started_or_stopped();
    }

    /// `milestone` was picked: started if nothing runs. True if another milestone
    /// is running, and the user is to be asked first.
    pub fn pick(&mut self, milestone: u32) -> bool {
        match self.running().map(|r| r.milestone) {
            Some(running) => running != milestone,
            None => {
                self.start(milestone);
                false
            }
        }
    }

    fn started_or_stopped(&mut self) {
        // A question about the time before doesn't apply any more.
        self.away = None;
        self.watch.reset();
        self.seen_saved = None;
    }

    /// The away time counts.
    pub fn keep_away(&mut self) {
        self.away = None;
        self.watch.reset();
    }

    /// It doesn't: the milestone stops when the user left, and runs again from
    /// when they came back.
    pub fn drop_away(&mut self) {
        if let Some(a) = self.away.take() {
            self.change(|tx| tx.drop_away(a.since, a.until));
        }
        self.watch.reset();
    }

    /// Every second, as long as a milestone runs: notice time away, and write
    /// down (now and then) that the dock is running. True if there is
    /// something new to ask.
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
        // Only the part of it the milestone was running for.
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
