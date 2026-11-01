//! What is tracked: projects, their milestones (the milestones), and the time
//! spent on them. Pure data and arithmetic; the files are in `format`, the
//! locking and writing in `store`.

use crate::time::{Stamp, monday_of};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: u32,
    pub name: String,
}

/// A milestone of a project's: what time is tracked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Milestone {
    pub id: u32,
    pub project: u32,
    pub name: String,
    /// The milestone's code elsewhere (Certinia's milestone or assignment ID),
    /// carried into the timesheet; may be empty.
    pub code: String,
    /// Kept for the weeks it has time in, but no longer offered.
    pub archived: bool,
}

/// A stretch of time on one milestone. Kept within one day: [`split_days`] cuts
/// a longer one at midnight, so every day's total is the sum of its entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub milestone: u32,
    pub start: Stamp,
    /// After `start`, and no later than the next midnight.
    pub end: Stamp,
    pub note: String,
}

impl Entry {
    pub fn seconds(&self) -> i64 {
        self.end.since(self.start)
    }

    /// The day the entry is on.
    pub fn day(&self) -> i64 {
        self.start.day
    }
}

/// The milestone that is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Running {
    pub milestone: u32,
    pub start: Stamp,
    pub note: String,
}

/// How a day's time on a milestone is rounded for the timesheet. The entries
/// themselves always keep the exact seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rounding {
    Exact,
    /// To 0.1 h (6 minutes).
    Tenth,
    /// To 0.25 h (15 minutes).
    #[default]
    Quarter,
}

impl Rounding {
    pub const ALL: [Rounding; 3] = [Rounding::Exact, Rounding::Tenth, Rounding::Quarter];

    /// As `timers.txt` has it.
    pub fn name(self) -> &'static str {
        match self {
            Rounding::Exact => "exact",
            Rounding::Tenth => "0.1",
            Rounding::Quarter => "0.25",
        }
    }

    pub fn from_name(name: &str) -> Option<Rounding> {
        Rounding::ALL.into_iter().find(|r| r.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Rounding::Exact => "Exact (to the minute)",
            Rounding::Tenth => "0.1 h (6 minutes)",
            Rounding::Quarter => "0.25 h (15 minutes)",
        }
    }

    fn step(self) -> i64 {
        match self {
            Rounding::Exact => 60,
            Rounding::Tenth => 360,
            Rounding::Quarter => 900,
        }
    }

    /// `seconds` in hours, to the nearest step (halfway rounds up).
    pub fn hours(self, seconds: i64) -> f64 {
        let step = self.step();
        let steps = (seconds.max(0) + step / 2) / step;
        (steps * step) as f64 / 3600.0
    }

    /// [`Rounding::hours`] as the timesheet shows it: "1.25", "0.1", "" for none.
    pub fn format(self, seconds: i64) -> String {
        let h = self.hours(seconds);
        if h == 0.0 {
            return String::new();
        }
        let text = format!("{h:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub rounding: Rounding,
    /// Minutes without input (or asleep) before the dock asks whether that
    /// time counts; 0 = never ask.
    pub away_minutes: u32,
    /// The dock stays out (not slid into the edge) while a milestone runs.
    pub keep_out: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rounding: Rounding::Quarter,
            away_minutes: 10,
            keep_out: true,
        }
    }
}

/// Everything but the finished entries: the milestones, the options and what is
/// running (`timers.txt`). Small, so the dock can read it every time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub projects: Vec<Project>,
    pub milestones: Vec<Milestone>,
    pub options: Options,
    pub running: Option<Running>,
    /// The milestone that ran last: what "resume" starts again.
    pub last: Option<u32>,
    /// Lines this build doesn't understand (a newer whir's), kept as they are.
    pub other: Vec<String>,
}

impl State {
    pub fn milestone(&self, id: u32) -> Option<&Milestone> {
        self.milestones.iter().find(|t| t.id == id)
    }

    pub fn milestone_mut(&mut self, id: u32) -> Option<&mut Milestone> {
        self.milestones.iter_mut().find(|t| t.id == id)
    }

    pub fn project(&self, id: u32) -> Option<&Project> {
        self.projects.iter().find(|c| c.id == id)
    }

    pub fn project_mut(&mut self, id: u32) -> Option<&mut Project> {
        self.projects.iter_mut().find(|c| c.id == id)
    }

    /// The project's name and the milestone's: what a milestone is called.
    pub fn names(&self, milestone: u32) -> (&str, &str) {
        match self.milestone(milestone) {
            Some(t) => (
                self.project(t.project).map_or("", |c| c.name.as_str()),
                t.name.as_str(),
            ),
            None => ("", "(deleted milestone)"),
        }
    }

    /// "Acme Corp · Onboarding".
    pub fn label(&self, milestone: u32) -> String {
        match self.names(milestone) {
            ("", milestone) => milestone.to_owned(),
            (project, milestone) => format!("{project} · {milestone}"),
        }
    }

    pub fn add_project(&mut self, name: &str) -> u32 {
        let id = self.projects.iter().map(|c| c.id).max().unwrap_or(0) + 1;
        self.projects.push(Project {
            id,
            name: one_line(name),
        });
        id
    }

    pub fn add_milestone(&mut self, project: u32, name: &str, code: &str) -> u32 {
        let id = self.milestones.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        self.milestones.push(Milestone {
            id,
            project,
            name: one_line(name),
            code: one_line(code),
            archived: false,
        });
        id
    }

    /// Projects by name, each with its milestones by name; archived milestones only
    /// if `archived`, and projects without milestones to show only if `empty`.
    pub fn by_project(&self, archived: bool, empty: bool) -> Vec<(&Project, Vec<&Milestone>)> {
        let mut projects: Vec<&Project> = self.projects.iter().collect();
        projects.sort_by_key(|c| c.name.to_lowercase());
        projects
            .into_iter()
            .filter_map(|c| {
                let mut milestones: Vec<&Milestone> = self
                    .milestones
                    .iter()
                    .filter(|t| t.project == c.id && (archived || !t.archived))
                    .collect();
                milestones.sort_by_key(|t| t.name.to_lowercase());
                (empty || !milestones.is_empty()).then_some((c, milestones))
            })
            .collect()
    }
}

/// Names live on one line in `timers.txt` and in the timesheet's cells.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One week's finished entries, Monday to Sunday (`2026-10-26.txt`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Week {
    pub monday: i64,
    /// When the week was closed out (its CSVs written); `None` while open.
    pub closed: Option<Stamp>,
    /// In order of start.
    pub entries: Vec<Entry>,
    pub other: Vec<String>,
}

impl Week {
    pub fn new(monday: i64) -> Week {
        Week {
            monday,
            closed: None,
            entries: Vec::new(),
            other: Vec::new(),
        }
    }

    pub fn contains(&self, day: i64) -> bool {
        (self.monday..self.monday + 7).contains(&day)
    }

    /// Add an entry (on one of this week's days), keeping the order.
    pub fn insert(&mut self, entry: Entry) {
        let at = self.entries.partition_point(|e| e.start <= entry.start);
        self.entries.insert(at, entry);
    }

    /// Entries ending after the week was closed: not in its CSVs yet.
    pub fn changed_since_closed(&self) -> usize {
        match self.closed {
            Some(at) => self.entries.iter().filter(|e| e.end > at).count(),
            None => 0,
        }
    }
}

/// `start..end` on `milestone`, cut at each midnight in between.
pub fn split_days(milestone: u32, start: Stamp, end: Stamp, note: &str) -> Vec<Entry> {
    let mut pieces = Vec::new();
    let mut from = start;
    while from < end {
        let to = end.min(Stamp::midnight(from.day + 1));
        pieces.push(Entry {
            milestone,
            start: from,
            end: to,
            note: note.to_owned(),
        });
        from = to;
    }
    pieces
}

/// Seconds per milestone per day of one week (Monday first), the running milestone's
/// time so far included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grid {
    pub monday: i64,
    /// In order of first appearance.
    pub rows: Vec<(u32, [i64; 7])>,
}

impl Grid {
    pub fn of(week: &Week, running: Option<&Running>, now: Stamp) -> Grid {
        let mut grid = Grid {
            monday: week.monday,
            rows: Vec::new(),
        };
        for e in &week.entries {
            grid.add(e);
        }
        if let Some(r) = running {
            for piece in split_days(r.milestone, r.start, now, "") {
                grid.add(&piece);
            }
        }
        grid
    }

    fn add(&mut self, e: &Entry) {
        let d = e.day() - self.monday;
        if !(0..7).contains(&d) {
            return;
        }
        let row = match self.rows.iter().position(|(t, _)| *t == e.milestone) {
            Some(i) => i,
            None => {
                self.rows.push((e.milestone, [0; 7]));
                self.rows.len() - 1
            }
        };
        self.rows[row].1[d as usize] += e.seconds();
    }

    pub fn cell(&self, milestone: u32, day: usize) -> i64 {
        self.rows
            .iter()
            .find(|(t, _)| *t == milestone)
            .map_or(0, |(_, days)| days[day])
    }

    pub fn day_total(&self, day: usize) -> i64 {
        self.rows.iter().map(|(_, days)| days[day]).sum()
    }

    pub fn total(&self) -> i64 {
        (0..7).map(|d| self.day_total(d)).sum()
    }

    /// A row's hours, each day rounded on its own (as the timesheet has them).
    pub fn rounded_row(&self, milestone: u32, rounding: Rounding) -> f64 {
        (0..7)
            .map(|d| rounding.hours(self.cell(milestone, d)))
            .sum()
    }

    pub fn rounded_day(&self, day: usize, rounding: Rounding) -> f64 {
        self.rows
            .iter()
            .map(|(_, days)| rounding.hours(days[day]))
            .sum()
    }

    pub fn rounded_total(&self, rounding: Rounding) -> f64 {
        (0..7).map(|d| self.rounded_day(d, rounding)).sum()
    }
}

/// The week `stamp` is in.
pub fn week_of(stamp: Stamp) -> i64 {
    monday_of(stamp.day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;

    fn monday() -> i64 {
        days_from_civil(2026, 10, 26)
    }

    #[test]
    fn rounding_is_per_step_and_halfway_rounds_up() {
        let q = Rounding::Quarter;
        assert_eq!(q.hours(0), 0.0);
        assert_eq!(q.hours(7 * 60 + 29), 0.0);
        assert_eq!(q.hours(7 * 60 + 30), 0.25);
        assert_eq!(q.hours(52 * 60), 0.75);
        assert_eq!(q.hours(53 * 60), 1.0);
        assert_eq!(Rounding::Tenth.hours(9 * 60), 0.2);
        assert_eq!(Rounding::Exact.hours(90 * 60), 1.5);
        assert_eq!(q.format(0), "");
        assert_eq!(q.format(75 * 60), "1.25");
        assert_eq!(q.format(60 * 60), "1");
        assert_eq!(Rounding::Tenth.format(30 * 60), "0.5");
        for r in Rounding::ALL {
            assert_eq!(Rounding::from_name(r.name()), Some(r));
        }
    }

    #[test]
    fn long_stretches_are_cut_at_midnight() {
        let start = Stamp::at(monday(), 22, 0, 0);
        let end = Stamp::at(monday() + 2, 1, 30, 0);
        let pieces = split_days(7, start, end, "late");
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].seconds(), 2 * 3600);
        assert_eq!(pieces[1].seconds(), 24 * 3600);
        assert_eq!(pieces[2].seconds(), 90 * 60);
        assert_eq!(pieces[2].end, end);
        assert!(pieces.iter().all(|p| p.note == "late" && p.milestone == 7));
        assert!(split_days(7, end, start, "").is_empty());
    }

    #[test]
    fn the_grid_adds_up_entries_and_the_running_milestone() {
        let m = monday();
        let mut week = Week::new(m);
        for (milestone, day, from, to) in [(1, 0, 9, 10), (2, 0, 10, 12), (1, 2, 13, 14)] {
            week.insert(Entry {
                milestone,
                start: Stamp::at(m + day, from, 0, 0),
                end: Stamp::at(m + day, to, 0, 0),
                note: String::new(),
            });
        }
        let running = Running {
            milestone: 2,
            start: Stamp::at(m + 2, 23, 0, 0),
            note: String::new(),
        };
        let now = Stamp::at(m + 3, 0, 30, 0);
        let g = Grid::of(&week, Some(&running), now);
        assert_eq!(g.rows.iter().map(|r| r.0).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(g.cell(1, 0), 3600);
        assert_eq!(g.cell(2, 0), 7200);
        assert_eq!(g.cell(2, 2), 3600);
        assert_eq!(g.cell(2, 3), 1800);
        assert_eq!(g.day_total(0), 3 * 3600);
        assert_eq!(g.total(), 3600 + 7200 + 3600 + 3600 + 1800);
        assert_eq!(g.rounded_total(Rounding::Quarter), 5.5);
    }

    #[test]
    fn entries_stay_in_order_and_names_are_one_line() {
        let m = monday();
        let mut week = Week::new(m);
        for h in [11, 9, 10] {
            week.insert(Entry {
                milestone: 1,
                start: Stamp::at(m, h, 0, 0),
                end: Stamp::at(m, h, 30, 0),
                note: String::new(),
            });
        }
        let hours: Vec<u32> = week.entries.iter().map(|e| e.start.hour()).collect();
        assert_eq!(hours, [9, 10, 11]);

        let mut s = State::default();
        let acme = s.add_project("  Acme\n Corp ");
        let t = s.add_milestone(acme, "On\tboarding", "PRJ-1");
        assert_eq!(s.label(t), "Acme Corp · On boarding");
        let z = s.add_milestone(acme, "Zeta", "");
        s.milestone_mut(z).unwrap().archived = true;
        let shown = s.by_project(false, false);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].1.len(), 1);
        assert_eq!(s.by_project(true, false)[0].1.len(), 2);
    }
}
