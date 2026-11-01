//! Small persistent preferences in `%LOCALAPPDATA%\whir\settings.ini`: plain
//! `key = value` lines (`#` starts a comment). Loaded on first use; a change
//! is written back atomically on a background thread. Keys this build doesn't
//! know survive the round trip, so an older whir never drops a newer one's
//! settings. (One whir window per session, so `settings.ini` has one writer;
//! the capture dock is a separate process and keeps its own [`IniFile`].)

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Instant;

use crate::file::{WriteError, write_atomic};

/// One `key = value` file, held in memory.
pub struct IniFile {
    /// `None`: in memory only (tests, or no `%LOCALAPPDATA%`).
    path: Option<PathBuf>,
    entries: Vec<(String, String)>,
}

impl IniFile {
    /// A missing or unreadable file is just empty.
    pub fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| parse(&s))
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Write it back now (atomically), creating the folder if needed.
    pub fn save(&self) -> Result<(), WriteError> {
        match &self.path {
            Some(path) => save_to(path, &serialize(&self.entries)),
            None => Ok(()),
        }
    }

    /// Returns true if the value changed.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) if v == value => false,
            Some((_, v)) => {
                *v = value.to_owned();
                true
            }
            None => {
                self.entries.push((key.to_owned(), value.to_owned()));
                true
            }
        }
    }
}

static STORE: OnceLock<Mutex<IniFile>> = OnceLock::new();

fn store() -> MutexGuard<'static, IniFile> {
    STORE
        .get_or_init(|| Mutex::new(IniFile::load(app_data_file("settings.ini"))))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// `%LOCALAPPDATA%\whir\<name>`.
pub fn app_data_file(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&dir).join("whir").join(name))
}

/// Forget the settings file: start from defaults and never write (tests).
pub fn memory_only() {
    let mut s = STORE
        .get_or_init(|| Mutex::new(IniFile::load(None)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    s.path = None;
    s.entries.clear();
}

pub fn get_bool(key: &str, default: bool) -> bool {
    match store().get(key).map(str::to_ascii_lowercase).as_deref() {
        Some("true" | "1" | "yes" | "on") => true,
        Some("false" | "0" | "no" | "off") => false,
        _ => default,
    }
}

pub fn set_bool(key: &str, value: bool) {
    set_str(key, if value { "true" } else { "false" });
}

pub fn get_str(key: &str) -> Option<String> {
    store().get(key).map(str::to_owned)
}

/// `value` must be a single line.
pub fn set_str(key: &str, value: &str) {
    let mut s = store();
    if s.set(key, value) && s.path.is_some() {
        drop(s);
        save_soon();
    }
}

/// [`set_str`], for a value another process wants to read: `after` runs once
/// it's on disk.
pub fn set_str_then(key: &str, value: &str, after: impl FnOnce() + Send + 'static) {
    let mut s = store();
    let write = s.set(key, value) && s.path.is_some();
    drop(s);
    if write {
        spawn_write("settings", move || {
            write_latest();
            after();
        });
    } else {
        after();
    }
}

/// Write the current settings on a short-lived thread (writes are rare, but
/// flushing to disk can take a while on a busy drive).
fn save_soon() {
    spawn_write("settings", write_latest);
}

/// One write at a time, each of the latest values.
fn write_latest() {
    static WRITE: Mutex<()> = Mutex::new(());
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let snapshot = {
        let s = store();
        s.path.clone().map(|p| (p, serialize(&s.entries)))
    };
    if let Some((path, text)) = snapshot {
        let _ = save_to(&path, &text);
    }
}

/// Writes of files in `%LOCALAPPDATA%\whir` still running, for [`wait_idle`].
static IN_FLIGHT: Mutex<usize> = Mutex::new(0);
static IDLE: Condvar = Condvar::new();

/// Run `write` on a short-lived thread that [`wait_idle`] waits for.
pub(crate) fn spawn_write(name: &str, write: impl FnOnce() + Send + 'static) {
    let done = || {
        let mut n = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        if *n == 0 {
            IDLE.notify_all();
        }
    };
    *IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            write();
            done();
        });
    if spawned.is_err() {
        done();
    }
}

/// Wait until the settings (and the views of [`crate::views`]) are on disk,
/// or `deadline`: before exiting, which would cut a write off. True if idle.
pub fn wait_idle(deadline: Instant) -> bool {
    let mut n = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
    while *n > 0 {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        n = match IDLE.wait_timeout(n, deadline - now) {
            Ok((guard, _)) => guard,
            Err(e) => e.into_inner().0,
        };
    }
    true
}

pub(crate) fn save_to(path: &Path, text: &str) -> Result<(), WriteError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(path, |f| f.write_all(text.as_bytes())).map(|_| ())
}

/// `key = value` lines; comments, blank and malformed lines are skipped, and
/// a repeated key keeps its last value.
fn parse(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(key, _)| key == k) {
            Some((_, old)) => *old = v.to_owned(),
            None => out.push((k.to_owned(), v.to_owned())),
        }
    }
    out
}

fn serialize(entries: &[(String, String)]) -> String {
    let mut out = String::from("# whir settings\n");
    for (k, v) in entries {
        out.push_str(&format!("{k} = {v}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_serialize() {
        let text = "# comment\n\n pdf.minimap = false \nno equals sign\n= orphan\n\
                    future.key=some value\npdf.minimap = true\n";
        let e = parse(text);
        assert_eq!(
            e,
            vec![
                ("pdf.minimap".to_owned(), "true".to_owned()),
                ("future.key".to_owned(), "some value".to_owned()),
            ]
        );
        assert_eq!(parse(&serialize(&e)), e);
    }

    #[test]
    fn set_keeps_unknown_keys() {
        let mut s = IniFile {
            path: None,
            entries: parse("other = 1\na = false\n"),
        };
        assert!(s.set("a", "true"));
        assert!(!s.set("a", "true"));
        assert!(s.set("b", "x"));
        assert_eq!(
            serialize(&s.entries),
            "# whir settings\nother = 1\na = true\nb = x\n"
        );
    }

    #[test]
    fn save_and_reload() {
        let dir = std::env::temp_dir().join(format!("whir-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("settings.ini");
        save_to(&path, "# whir settings\npdf.minimap = false\n").unwrap();
        let mut s = IniFile::load(Some(path.clone()));
        assert_eq!(s.get("pdf.minimap"), Some("false"));
        // Overwrites in place.
        s.set("pdf.minimap", "true");
        s.set("dock.edge", "left");
        s.save().unwrap();
        let s = IniFile::load(Some(path));
        assert_eq!(s.get("pdf.minimap"), Some("true"));
        assert_eq!(s.get("dock.edge"), Some("left"));
        // A missing file is just empty.
        assert!(
            IniFile::load(Some(dir.join("missing.ini")))
                .entries
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
