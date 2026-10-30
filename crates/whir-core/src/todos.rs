//! Todos: notes flagged as things to do. A minimal tracker: a note is open
//! (flagged), done (with when), or neither. Open ones come first in the notes
//! list, and the capture dock keeps them in sight.
//!
//! Kept beside the notes, in `.whir\todos.txt` (the library skips dot-names),
//! by file name, so the notes themselves stay plain Markdown:
//!
//! ```text
//! whir-todos 1
//! todo "Fix Acme invoice.md" 2026-10-29T14:02:00
//! done "Call Bob.md" 2026-10-20T09:00:00 2026-10-22T10:00:00
//! ```
//!
//! Both whir's window and the dock change it: under a named mutex, from a
//! fresh read, written atomically (like the timesheet).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::w;

use crate::file::{hide, write_atomic};
use crate::marks::{quote, unquote};
use crate::time::Stamp;

const HEADER: &str = "whir-todos";
const FOLDER: &str = ".whir";
const FILE: &str = "todos.txt";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Flagged, since then.
    Open { since: Stamp },
    /// Flagged then, and done since.
    Done { since: Stamp, done: Stamp },
}

impl State {
    pub fn is_open(self) -> bool {
        matches!(self, State::Open { .. })
    }

    fn since(self) -> Stamp {
        match self {
            State::Open { since } | State::Done { since, .. } => since,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    /// The note's file name, in the notes folder.
    pub name: String,
    pub state: State,
}

impl Todo {
    /// The note's title (its file name follows it).
    pub fn title(&self) -> &str {
        Path::new(&self.name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Todos {
    pub items: Vec<Todo>,
    /// Lines this build doesn't understand, kept as they are.
    pub other: Vec<String>,
}

impl Todos {
    pub fn get(&self, name: &str) -> Option<State> {
        self.items
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .map(|t| t.state)
    }

    /// The open ones, the most recently flagged first.
    pub fn open(&self) -> Vec<&Todo> {
        let mut open: Vec<&Todo> = self.items.iter().filter(|t| t.state.is_open()).collect();
        open.sort_by_key(|t| std::cmp::Reverse(t.state.since()));
        open
    }

    /// Flag `name` (open), mark it done, or forget it (`None`).
    pub fn set(&mut self, name: &str, state: Option<State>) {
        self.items.retain(|t| !t.name.eq_ignore_ascii_case(name));
        if let Some(state) = state {
            self.items.push(Todo {
                name: name.to_owned(),
                state,
            });
        }
    }

    /// The note was renamed: its flag goes with it.
    pub fn rename(&mut self, from: &str, to: &str) {
        for t in &mut self.items {
            if t.name.eq_ignore_ascii_case(from) {
                t.name = to.to_owned();
            }
        }
    }
}

pub fn parse(text: &str) -> Option<Todos> {
    let mut lines = text
        .lines()
        .map(|l| l.trim().trim_start_matches('\u{feff}'))
        .filter(|l| !l.is_empty());
    if lines.next()?.split_whitespace().next() != Some(HEADER) {
        return None;
    }
    let mut todos = Todos::default();
    for line in lines {
        match parse_line(line) {
            Some(t) => todos.items.push(t),
            None => todos.other.push(line.to_owned()),
        }
    }
    Some(todos)
}

/// `todo "<name>" <since>` or `done "<name>" <since> <done>`.
fn parse_line(line: &str) -> Option<Todo> {
    let (kind, rest) = line.split_once(' ')?;
    let rest = rest.trim();
    let close = rest.rfind('"')?;
    let name = unquote(&rest[..=close])?;
    let mut times = rest[close + 1..].split_whitespace().map(Stamp::parse);
    let state = match kind {
        "todo" => State::Open {
            since: times.next()??,
        },
        "done" => State::Done {
            since: times.next()??,
            done: times.next()??,
        },
        _ => return None,
    };
    if times.next().is_some() || name.is_empty() {
        return None;
    }
    Some(Todo { name, state })
}

pub fn serialize(todos: &Todos) -> String {
    let mut out = format!("{HEADER} 1\n");
    for t in &todos.items {
        let line = match t.state {
            State::Open { since } => format!("todo {} {}\n", quote(&t.name), since.format()),
            State::Done { since, done } => format!(
                "done {} {} {}\n",
                quote(&t.name),
                since.format(),
                done.format()
            ),
        };
        out.push_str(&line);
    }
    for line in &todos.other {
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn path(notes_dir: &Path) -> PathBuf {
    notes_dir.join(FOLDER).join(FILE)
}

/// The todos of the notes in `notes_dir` (none if there is no file, or it
/// can't be read).
pub fn load(notes_dir: &Path) -> Todos {
    std::fs::read_to_string(path(notes_dir))
        .ok()
        .and_then(|t| parse(&t))
        .unwrap_or_default()
}

static NOTIFY: OnceLock<fn()> = OnceLock::new();

/// Call `notify` after every change written (to tell the other process).
pub fn on_change(notify: fn()) {
    let _ = NOTIFY.set(notify);
}

/// Change the todos of `notes_dir`, from what is on disk now.
pub fn change(notes_dir: &Path, change: impl FnOnce(&mut Todos)) -> Result<(), String> {
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\whir.todos.v1")) }.ok();
    let locked = mutex.is_some_and(|m| {
        let r = unsafe { WaitForSingleObject(m, 3000) };
        r == WAIT_OBJECT_0 || r == WAIT_ABANDONED
    });
    let result = (|| {
        let file = path(notes_dir);
        let mut todos = match std::fs::read_to_string(&file) {
            Ok(text) => parse(&text).ok_or("The todo list isn't one whir can read.")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Todos::default(),
            Err(e) => return Err(e.to_string()),
        };
        let before = todos.clone();
        change(&mut todos);
        if todos == before {
            return Ok(false);
        }
        let dir = notes_dir.join(FOLDER);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        hide(&dir);
        let text = serialize(&todos);
        write_atomic(&file, |f| f.write_all(text.as_bytes())).map_err(|e| e.message(FILE))?;
        Ok(true)
    })();
    if let Some(m) = mutex {
        unsafe {
            if locked {
                let _ = ReleaseMutex(m);
            }
            let _ = CloseHandle(m);
        }
    }
    if result.as_ref().is_ok_and(|&wrote| wrote)
        && let Some(notify) = NOTIFY.get()
    {
        notify();
    }
    result.map(|_| ())
}

/// The note at `path` (in its notes folder): open, done, or neither.
pub fn set(path: &Path, state: Option<State>) -> Result<(), String> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Ok(());
    };
    change(dir, |t| t.set(name, state))
}

/// The note at `from` is now at `to` (same folder): its flag follows it.
pub fn renamed(from: &Path, to: &Path) {
    let names = (
        from.file_name().and_then(|n| n.to_str()),
        to.file_name().and_then(|n| n.to_str()),
    );
    if let (Some(dir), (Some(from), Some(to))) = (from.parent(), names)
        && load(dir).get(from).is_some()
    {
        let _ = change(dir, |t| t.rename(from, to));
    }
}

/// The note at `path` is gone: so is its flag.
pub fn removed(path: &Path) {
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        && load(dir).get(name).is_some()
    {
        let _ = change(dir, |t| t.set(name, None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> Stamp {
        Stamp::at(20_000, 9, 0, 0).plus(minute * 60)
    }

    #[test]
    fn todos_round_trip_and_keep_strangers() {
        let mut t = Todos::default();
        t.set(
            "Fix \"Acme\" invoice.md",
            Some(State::Open { since: at(0) }),
        );
        t.set(
            "Call Bob.md",
            Some(State::Done {
                since: at(1),
                done: at(90),
            }),
        );
        t.other.push("future 1 2 3".into());
        let back = parse(&serialize(&t)).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.items[0].title(), "Fix \"Acme\" invoice");
        assert_eq!(parse("whir-time 1\n"), None);
        let odd =
            parse("whir-todos 1\ntodo \"a.md\"\ndone \"b.md\" 2026-10-20T09:00:00\n").unwrap();
        assert!(odd.items.is_empty());
        assert_eq!(odd.other.len(), 2);
    }

    #[test]
    fn open_ones_are_newest_first_and_flags_follow_renames() {
        let mut t = Todos::default();
        t.set("a.md", Some(State::Open { since: at(0) }));
        t.set("b.md", Some(State::Open { since: at(5) }));
        t.set(
            "c.md",
            Some(State::Done {
                since: at(1),
                done: at(2),
            }),
        );
        let open: Vec<&str> = t.open().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(open, ["b.md", "a.md"]);
        t.rename("A.md", "Alpha.md");
        assert!(t.get("alpha.md").is_some_and(State::is_open));
        assert_eq!(t.get("a.md"), None);
        // Flagged again: one entry, the new state.
        t.set("b.md", None);
        t.set("b.md", Some(State::Open { since: at(9) }));
        assert_eq!(t.items.iter().filter(|x| x.name == "b.md").count(), 1);
    }

    #[test]
    fn changes_are_written_and_seen_from_the_file() {
        let dir = std::env::temp_dir().join(format!("whir-todos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("Fix it.md");
        set(&note, Some(State::Open { since: at(0) })).unwrap();
        assert!(load(&dir).get("Fix it.md").is_some_and(State::is_open));
        renamed(&note, &dir.join("Fixed it.md"));
        assert!(load(&dir).get("Fixed it.md").is_some());
        removed(&dir.join("Fixed it.md"));
        assert!(load(&dir).items.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
