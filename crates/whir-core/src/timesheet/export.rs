//! Closing out a week: its timesheet written as CSV files next to the data,
//! for Excel, for whir's own table view, and for keeping.
//!
//! - `2026-10-26 timesheet.csv`: a row per project, a column per day, hours
//!   rounded as the options say (what goes on the timecard), with the notes.
//! - `2026-10-26 entries.csv`: every entry, to the second.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::model::{Grid, Running, State, Week};
use super::store::Txn;
use crate::file::write_atomic;
use crate::time::{Stamp, format_iso_date};

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Where a week's CSVs go.
pub fn csv_paths(dir: &Path, monday: i64) -> (PathBuf, PathBuf) {
    let date = format_iso_date(monday);
    (
        dir.join(format!("{date} timesheet.csv")),
        dir.join(format!("{date} entries.csv")),
    )
}

/// One CSV field: quoted when it has to be, quotes doubled.
fn field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) || s.starts_with(' ') || s.ends_with(' ') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// The rows of the timesheet, separated by `sep` (`,` for the file, a tab for
/// the clipboard).
fn timesheet(state: &State, grid: &Grid, week: &Week, sep: &str) -> String {
    let rounding = state.options.rounding;
    let cell = |s: &str| {
        if sep == "," {
            field(s)
        } else {
            s.replace(['\t', '\r', '\n'], " ")
        }
    };
    let mut out = String::new();
    let mut line = |cells: Vec<String>| {
        let cells: Vec<String> = cells.iter().map(|c| cell(c)).collect();
        out.push_str(&cells.join(sep));
        out.push_str("\r\n");
    };
    let mut header = vec!["Customer".into(), "Project".into(), "Code".into()];
    for (d, name) in DAYS.iter().enumerate() {
        header.push(format!(
            "{name} {}",
            format_iso_date(grid.monday + d as i64)
        ));
    }
    header.extend(["Total".into(), "Notes".into()]);
    line(header);

    // Customer, then project, by name.
    let mut rows: Vec<u32> = grid.rows.iter().map(|(t, _)| *t).collect();
    rows.sort_by_key(|&t| {
        let (c, p) = state.names(t);
        (c.to_lowercase(), p.to_lowercase())
    });
    for t in rows {
        let (customer, project) = state.names(t);
        let code = state.timer(t).map_or("", |t| t.code.as_str());
        let mut cells = vec![customer.to_owned(), project.to_owned(), code.to_owned()];
        for d in 0..7 {
            cells.push(rounding.format(grid.cell(t, d)));
        }
        cells.push(hours(grid.rounded_row(t, rounding)));
        cells.push(notes(week, t));
        line(cells);
    }
    let mut total = vec!["Total".into(), String::new(), String::new()];
    for d in 0..7 {
        total.push(hours(grid.rounded_day(d, rounding)));
    }
    total.push(hours(grid.rounded_total(rounding)));
    total.push(String::new());
    line(total);
    out
}

/// `1.25`, `0` for none (in totals).
fn hours(h: f64) -> String {
    let text = format!("{h:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A project's notes for the week: "Mon: kickoff, review; Wed: fixes".
fn notes(week: &Week, timer: u32) -> String {
    let mut days: Vec<String> = Vec::new();
    for (d, name) in DAYS.iter().enumerate() {
        let mut seen: Vec<&str> = Vec::new();
        for e in &week.entries {
            let note = e.note.trim();
            if e.timer == timer
                && e.day() == week.monday + d as i64
                && !note.is_empty()
                && !seen.contains(&note)
            {
                seen.push(note);
            }
        }
        if !seen.is_empty() {
            days.push(format!("{name}: {}", seen.join(", ")));
        }
    }
    days.join("; ").replace(['\r', '\n'], " ")
}

/// The timesheet CSV (with a byte-order mark, so Excel reads it as UTF-8).
pub fn timesheet_csv(state: &State, week: &Week) -> String {
    let grid = Grid::of(week, None, Stamp::default());
    format!("\u{feff}{}", timesheet(state, &grid, week, ","))
}

/// The timesheet as tab-separated text, for pasting into Excel or an email;
/// the running timer's time so far included.
pub fn timesheet_tsv(state: &State, week: &Week, running: Option<&Running>, now: Stamp) -> String {
    let grid = Grid::of(week, running, now);
    timesheet(state, &grid, week, "\t")
}

/// Every entry of the week.
pub fn entries_csv(state: &State, week: &Week) -> String {
    let mut out = String::from("\u{feff}Date,Start,End,Hours,Customer,Project,Code,Note\r\n");
    let time = |s: Stamp| {
        let full = s.format();
        full.split_once('T')
            .map_or(full.clone(), |(_, t)| t.to_owned())
    };
    for e in &week.entries {
        let (customer, project) = state.names(e.timer);
        let code = state.timer(e.timer).map_or("", |t| t.code.as_str());
        let end = if e.end.day > e.start.day {
            "24:00:00".to_owned()
        } else {
            time(e.end)
        };
        let cells = [
            format_iso_date(e.day()),
            time(e.start),
            end,
            format!("{:.2}", e.seconds() as f64 / 3600.0),
            customer.to_owned(),
            project.to_owned(),
            code.to_owned(),
            e.note.clone(),
        ];
        let cells: Vec<String> = cells.iter().map(|c| field(c)).collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    }
    out
}

impl Txn<'_> {
    /// Close out the week starting `monday`: write its CSVs and mark it
    /// closed. A timer running in it keeps running; its time so far is
    /// written as an entry first, so the timesheet has it. Returns the
    /// timesheet CSV's path.
    pub fn close_week(&mut self, monday: i64, now: Stamp) -> Result<PathBuf, String> {
        if let Some(r) = self.state.running.clone()
            && r.start < Stamp::midnight(monday + 7)
        {
            let note = r.note.clone();
            self.stop(now);
            self.start(r.timer, now);
            self.set_note(&note);
        }
        let state = self.state.clone();
        let week = self.week(monday).clone();
        let (sheet, entries) = csv_paths(self.dir(), monday);
        std::fs::create_dir_all(self.dir()).map_err(|e| e.to_string())?;
        for (path, text) in [
            (&sheet, timesheet_csv(&state, &week)),
            (&entries, entries_csv(&state, &week)),
        ] {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            write_atomic(path, |f| f.write_all(text.as_bytes())).map_err(|e| e.message(&name))?;
        }
        self.week_mut(monday).closed = Some(now);
        Ok(sheet)
    }

    /// Open a closed week again (its CSVs stay until it is closed again).
    pub fn reopen_week(&mut self, monday: i64) {
        self.week_mut(monday).closed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;
    use crate::timesheet::{Entry, Store};

    fn sample() -> (State, Week) {
        let m = days_from_civil(2026, 10, 26);
        let mut s = State::default();
        let acme = s.add_customer("Acme, Inc.");
        let globex = s.add_customer("Globex");
        let onboard = s.add_timer(acme, "Onboarding", "PRJ-1");
        let migrate = s.add_timer(globex, "Migration \"v2\"", "");
        let mut w = Week::new(m);
        for (t, d, from, to, note) in [
            (migrate, 0, (9, 0), (9, 50), "plan"),
            (onboard, 0, (10, 0), (11, 20), "kickoff"),
            (onboard, 0, (13, 0), (13, 30), "kickoff"),
            (onboard, 2, (9, 0), (9, 7), ""),
            (onboard, 2, (10, 0), (10, 30), "review"),
        ] {
            w.insert(Entry {
                timer: t,
                start: Stamp::at(m + d, from.0, from.1, 0),
                end: Stamp::at(m + d, to.0, to.1, 0),
                note: note.into(),
            });
        }
        (s, w)
    }

    #[test]
    fn the_timesheet_has_a_row_per_project_and_rounded_days() {
        let (s, w) = sample();
        let csv = timesheet_csv(&s, &w);
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').lines().collect();
        assert_eq!(
            lines,
            [
                "Customer,Project,Code,Mon 2026-10-26,Tue 2026-10-27,Wed 2026-10-28,\
                 Thu 2026-10-29,Fri 2026-10-30,Sat 2026-10-31,Sun 2026-11-01,Total,Notes",
                "\"Acme, Inc.\",Onboarding,PRJ-1,1.75,,0.5,,,,,2.25,Mon: kickoff; Wed: review",
                "Globex,\"Migration \"\"v2\"\"\",,0.75,,,,,,,0.75,Mon: plan",
                "Total,,,2.5,0,0.5,0,0,0,0,3,",
            ]
        );
    }

    #[test]
    fn entries_are_listed_to_the_second() {
        let (s, w) = sample();
        let csv = entries_csv(&s, &w);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(
            lines[2],
            "2026-10-26,10:00:00,11:20:00,1.33,\"Acme, Inc.\",Onboarding,PRJ-1,kickoff"
        );
    }

    #[test]
    fn closing_writes_both_files_and_keeps_the_timer_running() {
        let dir = std::env::temp_dir().join(format!("whir-time-close-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(dir.clone());
        let m = days_from_civil(2026, 10, 26);
        let friday_noon = Stamp::at(m + 4, 12, 0, 0);
        let sheet = store
            .update(|tx| {
                let c = tx.state.add_customer("Acme");
                let t = tx.state.add_timer(c, "Onboarding", "");
                tx.start(t, Stamp::at(m + 4, 9, 0, 0));
                tx.set_note("all morning");
                tx.close_week(m, friday_noon)
            })
            .unwrap()
            .unwrap();
        assert!(sheet.exists());
        assert!(csv_paths(&dir, m).1.exists());
        assert!(std::fs::read_to_string(&sheet).unwrap().contains(",3,"));
        assert_eq!(store.week(m).closed, Some(friday_noon));
        let r = store.state().running.clone().unwrap();
        assert_eq!((r.start, r.note.as_str()), (friday_noon, "all morning"));
        store.update(|tx| tx.reopen_week(m)).unwrap();
        assert_eq!(store.week(m).closed, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
