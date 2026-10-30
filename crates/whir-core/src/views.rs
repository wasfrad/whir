//! Where each file was left: the view a document had when it was last put
//! away (a PDF's zoom and place), in `%LOCALAPPDATA%\whir\views.txt`. The
//! newest first, at most [`MAX`] files. An entry only counts for the same
//! file at the same version, so a different file that later lands on the
//! same path (a mail attachment's temp copy, say) opens afresh. The view
//! itself is the document kind's own one-line string.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::file::FileIdentity;
use crate::settings::{app_data_file, save_to, spawn_write};

/// Files remembered; the oldest beyond this are forgotten.
pub const MAX: usize = 500;
const HEADER: &str = "whir-views 1";

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    path: String,
    identity: FileIdentity,
    view: String,
}

struct Store {
    /// `None`: in memory only (tests, or no `%LOCALAPPDATA%`).
    file: Option<PathBuf>,
    /// Newest first.
    entries: Vec<Entry>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> MutexGuard<'static, Store> {
    STORE
        .get_or_init(|| {
            let file = app_data_file("views.txt");
            let entries = file
                .as_deref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|s| parse(&s))
                .unwrap_or_default();
            Mutex::new(Store { file, entries })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Forget the file: start empty and never write (tests).
pub fn memory_only() {
    let mut s = STORE
        .get_or_init(|| {
            Mutex::new(Store {
                file: None,
                entries: Vec::new(),
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    s.file = None;
    s.entries.clear();
}

fn same_path(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.to_lowercase() == b.to_lowercase()
}

/// The view `path` was left with, if it's still the same file.
pub fn lookup(path: &Path, identity: &FileIdentity) -> Option<String> {
    let path = path.to_string_lossy();
    store()
        .entries
        .iter()
        .find(|e| same_path(&e.path, &path))
        .filter(|e| e.identity == *identity)
        .map(|e| e.view.clone())
}

/// `path` (this version of it) was left showing `view`, a single line.
pub fn remember(path: &Path, identity: &FileIdentity, view: &str) {
    let entry = Entry {
        path: path.to_string_lossy().into_owned(),
        identity: *identity,
        view: view.to_owned(),
    };
    let mut s = store();
    if s.remember(entry) && s.file.is_some() {
        drop(s);
        save_soon();
    }
}

impl Store {
    /// To the front, replacing its path's old entry. False if nothing changed.
    fn remember(&mut self, entry: Entry) -> bool {
        if self.entries.first() == Some(&entry) {
            return false;
        }
        self.entries.retain(|e| !same_path(&e.path, &entry.path));
        self.entries.insert(0, entry);
        self.entries.truncate(MAX);
        true
    }
}

/// How many files are remembered.
pub fn len() -> usize {
    store().entries.len()
}

/// Forget every file.
pub fn forget_all() {
    let mut s = store();
    if s.entries.is_empty() {
        return;
    }
    s.entries.clear();
    if s.file.is_some() {
        drop(s);
        save_soon();
    }
}

fn save_soon() {
    spawn_write("views", || {
        static WRITE: Mutex<()> = Mutex::new(());
        let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
        let snapshot = {
            let s = store();
            s.file.clone().map(|p| (p, serialize(&s.entries)))
        };
        if let Some((path, text)) = snapshot {
            let _ = save_to(&path, &text);
        }
    });
}

/// `volume index size mtime <tab> view <tab> path` lines under a header.
/// Lines that don't parse are skipped; so is a file with another header.
fn parse(text: &str) -> Vec<Entry> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some(HEADER) {
        return Vec::new();
    }
    let mut out: Vec<Entry> = Vec::new();
    for line in lines {
        let mut cols = line.splitn(3, '\t');
        let (Some(id), Some(view), Some(path)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let n: Vec<u64> = id
            .split(' ')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .unwrap_or_default();
        let &[volume, index, size, mtime] = n.as_slice() else {
            continue;
        };
        if path.is_empty()
            || volume > u32::MAX as u64
            || out.iter().any(|e| same_path(&e.path, path))
        {
            continue;
        }
        out.push(Entry {
            path: path.to_owned(),
            identity: FileIdentity {
                volume: volume as u32,
                index,
                size,
                mtime,
            },
            view: view.to_owned(),
        });
        if out.len() == MAX {
            break;
        }
    }
    out
}

fn serialize(entries: &[Entry]) -> String {
    let mut out = Vec::new();
    let _ = writeln!(out, "{HEADER}");
    for e in entries {
        let i = &e.identity;
        let _ = writeln!(
            out,
            "{} {} {} {}\t{}\t{}",
            i.volume, i.index, i.size, i.mtime, e.view, e.path
        );
    }
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> FileIdentity {
        FileIdentity {
            volume: 7,
            index: n,
            size: 1000 + n,
            mtime: 133_000_000_000_000_000 + n,
        }
    }

    fn entry(path: &str, n: u64, view: &str) -> Entry {
        Entry {
            path: path.to_owned(),
            identity: id(n),
            view: view.to_owned(),
        }
    }

    #[test]
    fn parse_and_serialize() {
        let e = vec![
            entry(r"C:\Users\me\Invoice 1.pdf", 1, "fit-page 0 0"),
            entry(r"D:\x\y.pdf", 2, "150% 812.5 40"),
        ];
        let text = serialize(&e);
        assert!(text.starts_with("whir-views 1\n"));
        assert_eq!(parse(&text), e);
    }

    #[test]
    fn garbled_lines_and_other_files_are_skipped() {
        let good = serialize(&[entry(r"C:\a.pdf", 1, "fit-width 0 0")]);
        let text = format!(
            "{good}nonsense\n1 2 3\tview\n1 2 x 4\tv\tC:\\b.pdf\n1 2 3 4\tv\t\n\
             1 2 3 4\tdup\tc:\\A.PDF\n"
        );
        assert_eq!(parse(&text), parse(&good));
        assert!(parse("something else\n1 2 3 4\tv\tC:\\a.pdf\n").is_empty());
        assert!(parse("").is_empty());
    }

    #[test]
    fn remember_lookup_and_forget() {
        memory_only();
        let a = Path::new(r"C:\Mail\Invoice.pdf");
        assert_eq!(lookup(a, &id(1)), None);
        remember(a, &id(1), "fit-page 0 0");
        assert_eq!(lookup(a, &id(1)).as_deref(), Some("fit-page 0 0"));
        // Paths match whatever their case.
        assert_eq!(
            lookup(Path::new(r"c:\mail\INVOICE.pdf"), &id(1)).as_deref(),
            Some("fit-page 0 0")
        );
        // Another file at the same path: nothing to restore.
        assert_eq!(lookup(a, &id(2)), None);
        // Remembering again replaces it.
        remember(a, &id(2), "fit-width 300 0");
        assert_eq!(len(), 1);
        assert_eq!(lookup(a, &id(2)).as_deref(), Some("fit-width 300 0"));
        forget_all();
        assert_eq!(len(), 0);
    }

    #[test]
    fn newest_first_and_capped() {
        let mut s = Store {
            file: None,
            entries: Vec::new(),
        };
        let name = |n: u64| format!(r"C:\f{n}.pdf");
        for n in 0..(MAX as u64 + 20) {
            assert!(s.remember(entry(&name(n), n, "fit-page 0 0")));
        }
        // The same again changes nothing; an older one comes back to the front.
        let newest = s.entries[0].clone();
        assert!(!s.remember(newest));
        assert!(s.remember(entry(r"C:\F30.PDF", 30, "fit-width 0 0")));
        assert_eq!(s.entries[0].path, r"C:\F30.PDF");
        let last = MAX as u64 + 19;
        assert!(s.remember(entry(&name(last), last, "fit-page 0 0")));
        let back = parse(&serialize(&s.entries));
        assert_eq!(back.len(), MAX);
        assert_eq!(back[0].path, name(last));
        assert_eq!(back[1].path, r"C:\F30.PDF");
        // The oldest 20 are gone.
        assert_eq!(back[MAX - 1].path, name(20));
    }
}
