//! Small persistent preferences in `%LOCALAPPDATA%\whir\settings.ini`: plain
//! `key = value` lines (`#` starts a comment). Loaded on first use; a change
//! is written back atomically on a background thread. Keys this build doesn't
//! know survive the round trip, so an older whir never drops a newer one's
//! settings. (One whir process per session, so there is only one writer.)

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::file::{WriteError, write_atomic};

struct Store {
    /// `None`: in memory only (tests, or no `%LOCALAPPDATA%`).
    path: Option<PathBuf>,
    entries: Vec<(String, String)>,
}

impl Store {
    fn load(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| parse(&s))
            .unwrap_or_default();
        Self { path, entries }
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Returns true if the value changed.
    fn set(&mut self, key: &str, value: &str) -> bool {
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

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> MutexGuard<'static, Store> {
    STORE
        .get_or_init(|| Mutex::new(Store::load(default_path())))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn default_path() -> Option<PathBuf> {
    let dir = std::env::var_os("LOCALAPPDATA")?;
    Some(Path::new(&dir).join("whir").join("settings.ini"))
}

/// Forget the settings file: start from defaults and never write (tests).
pub fn memory_only() {
    let mut s = STORE
        .get_or_init(|| Mutex::new(Store::load(None)))
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
    let mut s = store();
    if s.set(key, if value { "true" } else { "false" }) && s.path.is_some() {
        drop(s);
        save_soon();
    }
}

/// Write the current settings on a short-lived thread (writes are rare, but
/// flushing to disk can take a while on a busy drive).
fn save_soon() {
    let _ = std::thread::Builder::new()
        .name("settings".into())
        .spawn(|| {
            // One write at a time, each of the latest values.
            static WRITE: Mutex<()> = Mutex::new(());
            let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
            let snapshot = {
                let s = store();
                s.path.clone().map(|p| (p, serialize(&s.entries)))
            };
            if let Some((path, text)) = snapshot {
                let _ = save_to(&path, &text);
            }
        });
}

fn save_to(path: &Path, text: &str) -> Result<(), WriteError> {
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
        let mut s = Store {
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
        let s = Store::load(Some(path.clone()));
        assert_eq!(s.get("pdf.minimap"), Some("false"));
        // Overwrites in place.
        save_to(&path, "pdf.minimap = true\n").unwrap();
        assert_eq!(Store::load(Some(path)).get("pdf.minimap"), Some("true"));
        // A missing file is just empty.
        assert!(
            Store::load(Some(dir.join("missing.ini")))
                .entries
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
