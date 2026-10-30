//! The timesheet on disk, shared by two processes: the dock starts and stops
//! timers, the window edits entries and closes weeks. Each change is made
//! under a named mutex, from a fresh read of the files it touches, and
//! written atomically; readers read a file again whenever it has changed.
//!
//! ```text
//! <dir>\.whir\timers.txt       State: customers, timers, options, what runs
//! <dir>\.whir\2026-10-26.txt   that week's finished entries (named for its Monday)
//! <dir>\2026-10-26 *.csv       a closed week's timesheet (see `export`)
//! ```

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use super::format::{parse_state, parse_week, serialize_state, serialize_week};
use super::model::{Entry, Running, State, Week, split_days, week_of};
use crate::file::{FileIdentity, hide, open_shared, write_atomic};
use crate::time::{Stamp, format_iso_date, parse_date};

const DATA_FOLDER: &str = ".whir";
const STATE_FILE: &str = "timers.txt";
/// How long a change waits for the other process to finish one of its own.
const LOCK_WAIT_MS: u32 = 3000;
/// A timer stopped (or switched) within this many seconds of starting leaves
/// nothing behind: a slip of the mouse, not work.
const MIN_ENTRY_SECS: i64 = 30;

/// A file as last read, and which version of it that was.
struct Cached<T> {
    value: T,
    /// `None`: there was no file.
    identity: Option<FileIdentity>,
}

pub struct Store {
    dir: PathBuf,
    state: Cached<State>,
    weeks: HashMap<i64, Cached<Week>>,
    mutex: HANDLE,
    /// Called after a change is written (to tell the other process).
    notify: Option<Box<dyn Fn()>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        if !self.mutex.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.mutex);
            }
        }
    }
}

/// Read a file and which version it is; `Ok(None)` if there is none.
fn read(path: &Path) -> io::Result<Option<(String, FileIdentity)>> {
    let mut file = match open_shared(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let identity = FileIdentity::of(&file)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(Some((text, identity)))
}

fn unreadable(name: &str) -> String {
    format!("“{name}” in the timesheet folder isn't a timesheet file whir can read.")
}

impl Store {
    /// The timesheet kept in `dir` (nothing is read until it is asked for).
    pub fn open(dir: PathBuf) -> Store {
        let mutex =
            unsafe { CreateMutexW(None, false, w!("Local\\whir.time.v1")) }.unwrap_or_default();
        Store {
            dir,
            state: Cached {
                value: State::default(),
                identity: None,
            },
            weeks: HashMap::new(),
            mutex,
            notify: None,
        }
    }

    /// Call `notify` after each change this store writes.
    pub fn on_change(mut self, notify: impl Fn() + 'static) -> Store {
        self.notify = Some(Box::new(notify));
        self
    }

    /// Where the closed weeks' CSVs go.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn data_dir(&self) -> PathBuf {
        self.dir.join(DATA_FOLDER)
    }

    fn state_path(&self) -> PathBuf {
        self.data_dir().join(STATE_FILE)
    }

    fn week_path(&self, monday: i64) -> PathBuf {
        self.data_dir()
            .join(format!("{}.txt", format_iso_date(monday)))
    }

    /// Read again whatever changed on disk since it was last read. True if
    /// anything did.
    pub fn refresh(&mut self) -> bool {
        let mut changed = self.refresh_state();
        let mondays: Vec<i64> = self.weeks.keys().copied().collect();
        for monday in mondays {
            changed |= self.refresh_week(monday);
        }
        changed
    }

    fn refresh_state(&mut self) -> bool {
        let path = self.state_path();
        if FileIdentity::of_path(&path).ok() == self.state.identity && self.state.identity.is_some()
        {
            return false;
        }
        let (value, identity) = match read(&path) {
            Ok(Some((text, id))) => (parse_state(&text).unwrap_or_default(), Some(id)),
            // Gone, or unreadable for now: as if empty.
            _ => (State::default(), None),
        };
        let changed = value != self.state.value;
        self.state = Cached { value, identity };
        changed
    }

    fn refresh_week(&mut self, monday: i64) -> bool {
        let path = self.week_path(monday);
        let now = FileIdentity::of_path(&path).ok();
        if let Some(cached) = self.weeks.get(&monday)
            && cached.identity == now
        {
            return false;
        }
        let (value, identity) = match read(&path) {
            Ok(Some((text, id))) => (
                parse_week(&text, monday).unwrap_or_else(|| Week::new(monday)),
                Some(id),
            ),
            _ => (Week::new(monday), None),
        };
        let changed = self.weeks.get(&monday).is_none_or(|c| c.value != value);
        self.weeks.insert(monday, Cached { value, identity });
        changed
    }

    /// The timers, the options and what is running, as last read.
    pub fn state(&self) -> &State {
        &self.state.value
    }

    /// The week starting `monday` (read now if it wasn't yet).
    pub fn week(&mut self, monday: i64) -> &Week {
        if !self.weeks.contains_key(&monday) {
            self.refresh_week(monday);
        }
        &self.weeks[&monday].value
    }

    /// The Mondays of the weeks that have a file, oldest first.
    pub fn mondays(&self) -> Vec<i64> {
        let Ok(dir) = std::fs::read_dir(self.data_dir()) else {
            return Vec::new();
        };
        let mut mondays: Vec<i64> = dir
            .filter_map(|e| {
                let name = e.ok()?.file_name();
                let stem = name.to_str()?.strip_suffix(".txt")?;
                parse_date(stem)
            })
            .collect();
        mondays.sort_unstable();
        mondays
    }

    /// Make a change: `change` gets the timesheet as it is on disk now (the
    /// other process may have changed it), and what it changes is written
    /// before the other process can make a change of its own.
    pub fn update<R>(&mut self, change: impl FnOnce(&mut Txn) -> R) -> Result<R, String> {
        let _lock = Lock::take(self.mutex)?;
        let path = self.state_path();
        let state = match read(&path).map_err(|e| e.to_string())? {
            Some((text, _)) => parse_state(&text).ok_or_else(|| unreadable(STATE_FILE))?,
            None => State::default(),
        };
        let mut txn = Txn {
            store: self,
            original: state.clone(),
            state,
            weeks: BTreeMap::new(),
            originals: BTreeMap::new(),
            error: None,
        };
        let result = change(&mut txn);
        let Txn {
            state,
            original,
            weeks,
            originals,
            error,
            ..
        } = txn;
        if let Some(e) = error {
            return Err(e);
        }

        std::fs::create_dir_all(self.data_dir()).map_err(|e| e.to_string())?;
        hide(&self.data_dir());
        let mut wrote = false;
        for (monday, week) in weeks {
            if originals.get(&monday) == Some(&week) {
                continue;
            }
            let path = self.week_path(monday);
            let text = serialize_week(&week);
            let identity = write_atomic(&path, |f| f.write_all(text.as_bytes()))
                .map_err(|e| e.message(&format_iso_date(monday)))?;
            self.weeks.insert(
                monday,
                Cached {
                    value: week,
                    identity: Some(identity),
                },
            );
            wrote = true;
        }
        if state != original {
            let text = serialize_state(&state);
            let identity = write_atomic(&path, |f| f.write_all(text.as_bytes()))
                .map_err(|e| e.message(STATE_FILE))?;
            self.state = Cached {
                value: state,
                identity: Some(identity),
            };
            wrote = true;
        } else {
            self.state.value = state;
        }
        if wrote && let Some(notify) = &self.notify {
            notify();
        }
        Ok(result)
    }
}

/// The named mutex, held for one change.
struct Lock(HANDLE);

impl Lock {
    fn take(mutex: HANDLE) -> Result<Lock, String> {
        if mutex.is_invalid() {
            // Couldn't create it: carry on uncoordinated rather than not at all.
            return Ok(Lock(mutex));
        }
        let r = unsafe { WaitForSingleObject(mutex, LOCK_WAIT_MS) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            Ok(Lock(mutex))
        } else {
            Err("The timesheet is busy (the other part of whir is saving it). Try again.".into())
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = ReleaseMutex(self.0);
            }
        }
    }
}

/// One change to the timesheet, from what is on disk now.
pub struct Txn<'a> {
    store: &'a Store,
    pub state: State,
    original: State,
    weeks: BTreeMap<i64, Week>,
    originals: BTreeMap<i64, Week>,
    /// A week file that couldn't be read: nothing is written.
    error: Option<String>,
}

impl Txn<'_> {
    /// The timesheet's folder (for its CSVs).
    pub fn dir(&self) -> &Path {
        self.store.dir()
    }

    /// The week starting `monday`, to change.
    pub fn week_mut(&mut self, monday: i64) -> &mut Week {
        if !self.weeks.contains_key(&monday) {
            let path = self.store.week_path(monday);
            let week = match read(&path) {
                Ok(Some((text, _))) => parse_week(&text, monday).unwrap_or_else(|| {
                    self.error
                        .get_or_insert_with(|| unreadable(&format_iso_date(monday)));
                    Week::new(monday)
                }),
                Ok(None) => Week::new(monday),
                Err(e) => {
                    self.error.get_or_insert_with(|| e.to_string());
                    Week::new(monday)
                }
            };
            self.originals.insert(monday, week.clone());
            self.weeks.insert(monday, week);
        }
        self.weeks.get_mut(&monday).expect("just inserted")
    }

    pub fn week(&mut self, monday: i64) -> &Week {
        self.week_mut(monday)
    }

    /// Add time on `timer` (cut at midnight, into the weeks it falls in).
    pub fn add(&mut self, timer: u32, start: Stamp, end: Stamp, note: &str) {
        for piece in split_days(timer, start, end, note) {
            self.week_mut(week_of(piece.start)).insert(piece);
        }
    }

    /// Take out the `index`th entry of the week starting `monday`.
    pub fn remove(&mut self, monday: i64, index: usize) -> Option<Entry> {
        let week = self.week_mut(monday);
        (index < week.entries.len()).then(|| week.entries.remove(index))
    }

    /// Stop the running timer at `at`, keeping its time.
    pub fn stop(&mut self, at: Stamp) {
        if let Some(r) = self.state.running.take()
            && at.since(r.start) >= MIN_ENTRY_SECS
        {
            self.add(r.timer, r.start, at, &r.note);
        }
    }

    /// Start `timer` at `at` (stopping whatever else runs).
    pub fn start(&mut self, timer: u32, at: Stamp) {
        if self
            .state
            .running
            .as_ref()
            .is_some_and(|r| r.timer == timer)
        {
            return;
        }
        self.stop(at);
        self.state.running = Some(Running {
            timer,
            start: at,
            note: String::new(),
        });
        self.state.last = Some(timer);
    }

    /// Pause, or resume the timer that ran last.
    pub fn toggle(&mut self, at: Stamp) {
        match (&self.state.running, self.state.last) {
            (Some(_), _) => self.stop(at),
            (None, Some(last)) if self.state.timer(last).is_some_and(|t| !t.archived) => {
                self.start(last, at)
            }
            _ => {}
        }
    }

    /// What the running timer's time is for.
    pub fn set_note(&mut self, note: &str) {
        if let Some(r) = &mut self.state.running {
            r.note = note.trim().to_owned();
        }
    }

    /// The time since `since` didn't count (the user was away): the running
    /// timer stops then, and runs again from `now`.
    pub fn drop_away(&mut self, since: Stamp, now: Stamp) {
        let Some(r) = self.state.running.clone() else {
            return;
        };
        if since > r.start {
            self.stop(since);
        } else {
            self.state.running = None;
        }
        self.state.running = Some(Running { start: now, ..r });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::days_from_civil;

    fn temp_store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("whir-time-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::open(dir)
    }

    fn monday() -> i64 {
        days_from_civil(2026, 10, 26)
    }

    #[test]
    fn a_timer_runs_stops_and_lands_in_its_week() {
        let mut store = temp_store("runs");
        let m = monday();
        let t = store
            .update(|tx| {
                let c = tx.state.add_customer("Acme");
                tx.state.add_timer(c, "Onboarding", "PRJ-1")
            })
            .unwrap();
        store
            .update(|tx| tx.start(t, Stamp::at(m, 9, 0, 0)))
            .unwrap();
        assert_eq!(store.state().running.as_ref().unwrap().timer, t);
        store
            .update(|tx| {
                tx.set_note(" kickoff ");
                tx.stop(Stamp::at(m, 10, 30, 0));
            })
            .unwrap();
        assert!(store.state().running.is_none());
        assert_eq!(store.state().last, Some(t));
        let week = store.week(m).clone();
        assert_eq!(week.entries.len(), 1);
        assert_eq!(week.entries[0].seconds(), 90 * 60);
        assert_eq!(week.entries[0].note, "kickoff");

        // Another store (the other process) sees the same.
        let mut other = Store::open(store.dir().to_owned());
        other.refresh();
        assert_eq!(other.state(), store.state());
        assert_eq!(other.week(m), &week);
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn starting_another_stops_the_first_and_slips_leave_nothing() {
        let mut store = temp_store("switch");
        let m = monday();
        let (a, b) = store
            .update(|tx| {
                let c = tx.state.add_customer("Acme");
                (
                    tx.state.add_timer(c, "A", ""),
                    tx.state.add_timer(c, "B", ""),
                )
            })
            .unwrap();
        store
            .update(|tx| {
                tx.start(a, Stamp::at(m, 9, 0, 0));
                // A slip: B for ten seconds.
                tx.start(b, Stamp::at(m, 10, 0, 0));
                tx.start(a, Stamp::at(m, 10, 0, 10));
                tx.toggle(Stamp::at(m, 11, 0, 0));
            })
            .unwrap();
        let week = store.week(m).clone();
        let spans: Vec<(u32, i64)> = week
            .entries
            .iter()
            .map(|e| (e.timer, e.seconds()))
            .collect();
        assert_eq!(spans, [(a, 3600), (a, 3590)]);
        // Resume starts the last one again.
        store
            .update(|tx| tx.toggle(Stamp::at(m, 12, 0, 0)))
            .unwrap();
        assert_eq!(store.state().running.as_ref().unwrap().timer, a);
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn a_timer_left_running_over_the_weekend_splits_into_both_weeks() {
        let mut store = temp_store("weekend");
        let m = monday();
        store
            .update(|tx| {
                tx.state.running = Some(Running {
                    timer: 1,
                    start: Stamp::at(m + 6, 22, 0, 0),
                    note: "late".into(),
                });
                tx.stop(Stamp::at(m + 7, 2, 0, 0));
            })
            .unwrap();
        assert_eq!(store.week(m).entries.len(), 1);
        assert_eq!(store.week(m).entries[0].seconds(), 2 * 3600);
        assert_eq!(store.week(m + 7).entries[0].seconds(), 2 * 3600);
        assert_eq!(store.mondays(), [m, m + 7]);
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn dropping_away_time_leaves_a_gap() {
        let mut store = temp_store("away");
        let m = monday();
        store
            .update(|tx| {
                tx.state.running = Some(Running {
                    timer: 1,
                    start: Stamp::at(m, 9, 0, 0),
                    note: "n".into(),
                });
                tx.drop_away(Stamp::at(m, 10, 0, 0), Stamp::at(m, 11, 0, 0));
            })
            .unwrap();
        let r = store.state().running.clone().unwrap();
        assert_eq!((r.start, r.note.as_str()), (Stamp::at(m, 11, 0, 0), "n"));
        assert_eq!(store.week(m).entries[0].end, Stamp::at(m, 10, 0, 0));
        let _ = std::fs::remove_dir_all(store.dir());
    }

    #[test]
    fn a_file_whir_cant_read_is_never_overwritten() {
        let store_dir = temp_store("foreign").dir().to_owned();
        let data = store_dir.join(DATA_FOLDER);
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join(STATE_FILE), "not a timesheet\n").unwrap();
        let mut store = Store::open(store_dir.clone());
        assert!(store.update(|tx| tx.state.add_customer("x")).is_err());
        assert_eq!(
            std::fs::read_to_string(data.join(STATE_FILE)).unwrap(),
            "not a timesheet\n"
        );
        let _ = std::fs::remove_dir_all(store_dir);
    }

    #[test]
    fn notify_is_called_only_when_something_was_written() {
        use std::cell::Cell;
        use std::rc::Rc;
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        let mut store = temp_store("notify").on_change(move || seen.set(seen.get() + 1));
        store.update(|tx| tx.state.add_customer("x")).unwrap();
        store.update(|_| ()).unwrap();
        assert_eq!(calls.get(), 1);
        let _ = std::fs::remove_dir_all(store.dir());
    }
}
