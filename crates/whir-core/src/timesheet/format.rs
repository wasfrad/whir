//! The files, as text: one record per line, a version header first, names
//! and notes quoted. Lines this build doesn't understand are kept as they
//! are, so an older whir never drops a newer one's data.
//!
//! ```text
//! whir-time 1                              whir-week 1
//! option rounding 0.25                     closed 2026-11-02T09:14:00
//! option away 10                           entry 7 2026-10-26T09:02:11 2026-10-26T10:15:40 "Kickoff"
//! option keep-out on
//! customer 3 "Acme Corp"
//! timer 7 3 "Onboarding" "PRJ-00123"
//! timer 9 3 "Support" "" archived
//! last 7
//! running 7 2026-10-26T10:15:40 "Notes so far"
//! ```

use super::model::{Customer, Entry, Rounding, Running, State, Timer, Week};
use crate::marks::{quote, unquote};
use crate::time::Stamp;

const STATE_HEADER: &str = "whir-time";
const WEEK_HEADER: &str = "whir-week";

/// A line's words: bare ones end at whitespace; quoted ones (`"…"`, escaped
/// the way [`quote`] writes them) may hold anything. `None` if a quote is
/// left open.
fn words(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let end = if rest.starts_with('"') {
            // The closing quote: the first one not escaped.
            let mut escaped = false;
            let close = rest.bytes().enumerate().skip(1).find_map(|(i, b)| {
                match (escaped, b) {
                    (true, _) => escaped = false,
                    (false, b'\\') => escaped = true,
                    (false, b'"') => return Some(i),
                    _ => {}
                }
                None
            })?;
            out.push(unquote(&rest[..=close])?);
            close + 1
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            out.push(rest[..end].to_owned());
            end
        };
        rest = rest[end..].trim_start();
    }
    Some(out)
}

fn body<'a>(text: &'a str, header: &str) -> Option<impl Iterator<Item = &'a str>> {
    let mut lines = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty());
    let mut first = lines.next()?.split_whitespace();
    (first.next() == Some(header)).then_some(lines)
}

/// `None` if `text` isn't a `timers.txt`.
pub fn parse_state(text: &str) -> Option<State> {
    let mut state = State::default();
    for line in body(text, STATE_HEADER)? {
        if !state_line(&mut state, line) {
            state.other.push(line.to_owned());
        }
    }
    Some(state)
}

fn state_line(state: &mut State, line: &str) -> bool {
    let Some(w) = words(line) else { return false };
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    let id = |i: usize| w.get(i)?.parse::<u32>().ok();
    match w.as_slice() {
        ["option", "rounding", r] => match Rounding::from_name(r) {
            Some(r) => state.options.rounding = r,
            None => return false,
        },
        ["option", "away", m] => match m.parse() {
            Ok(m) => state.options.away_minutes = m,
            Err(_) => return false,
        },
        ["option", "keep-out", v @ ("on" | "off")] => state.options.keep_out = *v == "on",
        ["customer", _, name] => match id(1) {
            Some(id) => state.customers.push(Customer {
                id,
                name: (*name).to_owned(),
            }),
            None => return false,
        },
        ["timer", _, _, name, code, flags @ ..] => {
            let (Some(id), Some(customer)) = (id(1), id(2)) else {
                return false;
            };
            let archived = match flags {
                [] => false,
                ["archived"] => true,
                _ => return false,
            };
            state.timers.push(Timer {
                id,
                customer,
                name: (*name).to_owned(),
                code: (*code).to_owned(),
                archived,
            });
        }
        ["last", _] => match id(1) {
            Some(t) => state.last = Some(t),
            None => return false,
        },
        ["running", _, start, note] => {
            let (Some(timer), Some(start)) = (id(1), Stamp::parse(start)) else {
                return false;
            };
            state.running = Some(Running {
                timer,
                start,
                note: (*note).to_owned(),
            });
        }
        _ => return false,
    }
    true
}

pub fn serialize_state(state: &State) -> String {
    let o = &state.options;
    let mut out = format!(
        "{STATE_HEADER} 1\noption rounding {}\noption away {}\noption keep-out {}\n",
        o.rounding.name(),
        o.away_minutes,
        if o.keep_out { "on" } else { "off" }
    );
    for c in &state.customers {
        out.push_str(&format!("customer {} {}\n", c.id, quote(&c.name)));
    }
    for t in &state.timers {
        out.push_str(&format!(
            "timer {} {} {} {}{}\n",
            t.id,
            t.customer,
            quote(&t.name),
            quote(&t.code),
            if t.archived { " archived" } else { "" }
        ));
    }
    if let Some(t) = state.last {
        out.push_str(&format!("last {t}\n"));
    }
    if let Some(r) = &state.running {
        out.push_str(&format!(
            "running {} {} {}\n",
            r.timer,
            r.start.format(),
            quote(&r.note)
        ));
    }
    for line in &state.other {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The week starting on `monday`; `None` if `text` isn't a week file.
pub fn parse_week(text: &str, monday: i64) -> Option<Week> {
    let mut week = Week::new(monday);
    for line in body(text, WEEK_HEADER)? {
        if !week_line(&mut week, line) {
            week.other.push(line.to_owned());
        }
    }
    Some(week)
}

fn week_line(week: &mut Week, line: &str) -> bool {
    let Some(w) = words(line) else { return false };
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["closed", at] => match Stamp::parse(at) {
            Some(at) => week.closed = Some(at),
            None => return false,
        },
        ["entry", timer, start, end, note] => {
            let (Ok(timer), Some(start), Some(end)) =
                (timer.parse(), Stamp::parse(start), Stamp::parse(end))
            else {
                return false;
            };
            // Within one of this week's days, or it isn't one of ours.
            let within_a_day = start < end && end <= Stamp::midnight(start.day + 1);
            if !within_a_day || !week.contains(start.day) {
                return false;
            }
            week.insert(Entry {
                timer,
                start,
                end,
                note: (*note).to_owned(),
            });
        }
        _ => return false,
    }
    true
}

pub fn serialize_week(week: &Week) -> String {
    let mut out = format!("{WEEK_HEADER} 1\n");
    if let Some(at) = week.closed {
        out.push_str(&format!("closed {}\n", at.format()));
    }
    for e in &week.entries {
        out.push_str(&format!(
            "entry {} {} {} {}\n",
            e.timer,
            e.start.format(),
            e.end.format(),
            quote(&e.note)
        ));
    }
    for line in &week.other {
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;
    use crate::timesheet::model::Options;

    #[test]
    fn words_are_split_and_quotes_kept_together() {
        assert_eq!(
            words(r#"timer 7 3 "Acme \"West\" Corp" "" archived"#).unwrap(),
            ["timer", "7", "3", "Acme \"West\" Corp", "", "archived"]
        );
        assert_eq!(words(r#"a "b\\" c"#).unwrap(), ["a", "b\\", "c"]);
        assert_eq!(words("  ").unwrap(), Vec::<String>::new());
        assert_eq!(words(r#"a "open"#), None);
    }

    #[test]
    fn state_round_trips() {
        let mut s = State::default();
        let acme = s.add_customer("Acme \"West\"");
        let t = s.add_timer(acme, "Onboarding", "PRJ-00123");
        let u = s.add_timer(acme, "Support", "");
        s.timer_mut(u).unwrap().archived = true;
        s.options.rounding = Rounding::Tenth;
        s.options.away_minutes = 15;
        s.options.keep_out = false;
        s.last = Some(t);
        s.running = Some(Running {
            timer: t,
            start: Stamp::at(days_from_civil(2026, 10, 26), 10, 15, 40),
            note: "line one\nline two".to_owned(),
        });
        s.other.push("future thing 1".to_owned());
        let text = serialize_state(&s);
        assert_eq!(parse_state(&text), Some(s));
    }

    #[test]
    fn a_new_state_has_the_default_options() {
        let s = parse_state("whir-time 1\n").unwrap();
        assert_eq!(s.options, Options::default());
        assert_eq!(parse_state("whir-week 1\n"), None);
        assert_eq!(parse_state(""), None);
        // A byte-order mark (Notepad) and blank lines are fine.
        assert!(parse_state("\u{feff}whir-time 1\n\n").is_some());
    }

    #[test]
    fn nonsense_lines_are_kept_not_lost() {
        let text = "whir-time 1\noption rounding 0.3\ntimer x 1 \"a\" \"\"\ncustomer 1 \"ok\"\n";
        let s = parse_state(text).unwrap();
        assert_eq!(s.customers.len(), 1);
        assert_eq!(s.options.rounding, Rounding::Quarter);
        assert_eq!(s.other, ["option rounding 0.3", "timer x 1 \"a\" \"\""]);
        assert!(serialize_state(&s).contains("timer x 1"));
    }

    #[test]
    fn weeks_round_trip_and_refuse_strays() {
        let m = days_from_civil(2026, 10, 26);
        let mut w = Week::new(m);
        w.closed = Some(Stamp::at(m + 7, 9, 14, 0));
        w.insert(Entry {
            timer: 7,
            start: Stamp::at(m, 9, 2, 11),
            end: Stamp::at(m, 10, 15, 40),
            note: "Kickoff call".to_owned(),
        });
        w.insert(Entry {
            timer: 9,
            start: Stamp::at(m + 6, 23, 0, 0),
            end: Stamp::midnight(m + 7),
            note: String::new(),
        });
        let text = serialize_week(&w);
        assert_eq!(parse_week(&text, m), Some(w));

        let strays = "whir-week 1\n\
            entry 1 2026-10-19T09:00:00 2026-10-19T10:00:00 \"last week\"\n\
            entry 1 2026-10-26T10:00:00 2026-10-26T09:00:00 \"backwards\"\n\
            entry 1 2026-10-26T23:00:00 2026-10-27T01:00:00 \"two days\"\n";
        let w = parse_week(strays, m).unwrap();
        assert!(w.entries.is_empty());
        assert_eq!(w.other.len(), 3);
    }
}
