//! The notes library: the folder where whir keeps the notes and tables it creates
//! (`Documents\whir`, or `WHIR_NOTES_DIR`), file naming, and the list the drawer shows.
//!
//! Nothing here touches the disk until it's needed: the folder is resolved on
//! first use and scanned by a background worker started after the first frame.

use std::collections::HashMap;
use std::io::{self, Read as _};
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::Waker;
use crate::file::{self, FileIdentity};
use crate::time::LocalTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Note,
    Table,
}

#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub title: String,
    pub preview: String,
    pub modified: SystemTime,
    pub size: u64,
    /// A OneDrive placeholder: never read by the scan (that would download it).
    pub cloud_only: bool,
}

#[derive(Clone, Default)]
pub struct LibrarySnapshot {
    /// Bumped on every change.
    pub generation: u64,
    /// Newest first.
    pub entries: Arc<[LibraryEntry]>,
    pub scanning: bool,
    /// At least one scan has completed.
    pub scanned: bool,
    pub error: Option<String>,
    /// When the last full scan finished.
    pub scanned_at: Option<Instant>,
}

enum Work {
    Scan,
    Refresh(Vec<PathBuf>),
}

type Shared = Arc<Mutex<LibrarySnapshot>>;

pub struct Library {
    waker: Waker,
    override_dir: Option<PathBuf>,
    dir: OnceLock<Option<PathBuf>>,
    shared: Shared,
    worker: Mutex<Option<Sender<Work>>>,
}

pub const NOTE_EXTS: &[&str] = &["md", "markdown"];
pub const TABLE_EXTS: &[&str] = &["csv", "tsv"];
/// Bytes read from each file for its title and preview.
const HEAD_BYTES: usize = 2048;
/// Our own temp files older than this are leftovers from a crash.
const STALE_TEMP: Duration = Duration::from_secs(60 * 60);

impl Library {
    /// No I/O: the folder is resolved on first use.
    pub fn new(waker: Waker) -> Arc<Self> {
        let override_dir = std::env::var_os("WHIR_NOTES_DIR").map(PathBuf::from);
        Self::with_override(override_dir, waker)
    }

    /// A library rooted at `dir` (tests).
    pub fn at(dir: PathBuf, waker: Waker) -> Arc<Self> {
        Self::with_override(Some(dir), waker)
    }

    fn with_override(override_dir: Option<PathBuf>, waker: Waker) -> Arc<Self> {
        Arc::new(Self {
            waker,
            override_dir,
            dir: OnceLock::new(),
            shared: Arc::new(Mutex::new(LibrarySnapshot::default())),
            worker: Mutex::new(None),
        })
    }

    /// `Documents\whir` (follows OneDrive folder redirection). `None` if the
    /// Documents folder can't be resolved.
    pub fn dir(&self) -> Option<&Path> {
        self.dir
            .get_or_init(|| {
                self.override_dir
                    .clone()
                    .or_else(|| documents_dir().map(|d| d.join("whir")))
            })
            .as_deref()
    }

    /// The folder, created if needed (first save).
    pub fn ensure_dir(&self) -> io::Result<&Path> {
        let dir = self
            .dir()
            .ok_or_else(|| io::Error::other("the Documents folder isn't available"))?;
        std::fs::create_dir_all(dir)?;
        Ok(dir)
    }

    /// `path` is directly inside the library folder.
    pub fn contains(&self, path: &Path) -> bool {
        match (self.dir(), path.parent()) {
            (Some(dir), Some(parent)) => paths_equal(dir, parent),
            _ => false,
        }
    }

    pub fn snapshot(&self) -> LibrarySnapshot {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Rescan the whole folder in the background.
    pub fn scan(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.scanning = true;
        }
        self.send(Work::Scan);
    }

    /// Re-read (or drop) these files in the background after we wrote, renamed
    /// or deleted them. Paths outside the library are ignored.
    pub fn refresh(&self, paths: Vec<PathBuf>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| self.contains(p)).collect();
        if !paths.is_empty() {
            self.send(Work::Refresh(paths));
        }
    }

    fn send(&self, work: Work) {
        let Ok(mut guard) = self.worker.lock() else {
            return;
        };
        if guard.is_none() {
            let (tx, rx) = channel::<Work>();
            let worker = Worker {
                dir: self.dir().map(Path::to_path_buf),
                shared: self.shared.clone(),
                waker: self.waker.clone(),
            };
            let spawned = std::thread::Builder::new()
                .name("whir-library".into())
                .spawn(move || {
                    while let Ok(w) = rx.recv() {
                        worker.run(w);
                    }
                });
            if spawned.is_err() {
                return;
            }
            *guard = Some(tx);
        }
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(work);
        }
    }
}

struct Worker {
    dir: Option<PathBuf>,
    shared: Shared,
    waker: Waker,
}

impl Worker {
    fn run(&self, work: Work) {
        match work {
            Work::Scan => self.scan(),
            Work::Refresh(paths) => self.refresh(&paths),
        }
        self.waker.wake();
    }

    fn previous(&self) -> Arc<[LibraryEntry]> {
        self.shared
            .lock()
            .map(|s| s.entries.clone())
            .unwrap_or_default()
    }

    fn publish(&self, entries: Vec<LibraryEntry>, full_scan: bool, error: Option<String>) {
        let mut entries = entries;
        entries.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.title.cmp(&b.title))
        });
        if let Ok(mut s) = self.shared.lock() {
            s.generation += 1;
            s.entries = entries.into();
            if full_scan {
                s.scanning = false;
                s.scanned = true;
                s.error = error;
                s.scanned_at = Some(Instant::now());
            }
        }
    }

    fn scan(&self) {
        let Some(dir) = &self.dir else {
            self.publish(
                Vec::new(),
                true,
                Some("The Documents folder isn't available.".into()),
            );
            return;
        };
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            // Not created yet: nothing saved so far.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.publish(Vec::new(), true, None);
                return;
            }
            Err(e) => {
                self.publish(Vec::new(), true, Some(e.to_string()));
                return;
            }
        };
        let previous: HashMap<PathBuf, LibraryEntry> = self
            .previous()
            .iter()
            .map(|e| (e.path.clone(), e.clone()))
            .collect();
        let mut entries = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if name.starts_with(file::TEMP_PREFIX) {
                let old = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > STALE_TEMP);
                if old {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            if !meta.is_file() || name.starts_with("~$") || name.starts_with('.') {
                continue;
            }
            let Some(kind) = entry_kind(&path) else {
                continue;
            };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            // Unchanged since the last scan: keep what we read then.
            if let Some(p) = previous.get(&path)
                && p.modified == modified
                && p.size == meta.len()
            {
                entries.push(p.clone());
                continue;
            }
            entries.push(read_entry(
                &path,
                kind,
                modified,
                meta.len(),
                meta.file_attributes(),
            ));
        }
        self.publish(entries, true, None);
    }

    fn refresh(&self, paths: &[PathBuf]) {
        let mut entries: Vec<LibraryEntry> = self.previous().to_vec();
        for path in paths {
            entries.retain(|e| !paths_equal(&e.path, path));
            let Some(kind) = entry_kind(path) else {
                continue;
            };
            if let Ok(meta) = std::fs::metadata(path)
                && meta.is_file()
            {
                entries.push(read_entry(
                    path,
                    kind,
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                    meta.file_attributes(),
                ));
            }
        }
        self.publish(entries, false, None);
    }
}

fn entry_kind(path: &Path) -> Option<EntryKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if NOTE_EXTS.contains(&ext.as_str()) {
        Some(EntryKind::Note)
    } else if TABLE_EXTS.contains(&ext.as_str()) {
        Some(EntryKind::Table)
    } else {
        None
    }
}

fn read_entry(
    path: &Path,
    kind: EntryKind,
    modified: SystemTime,
    size: u64,
    attributes: u32,
) -> LibraryEntry {
    let cloud_only = file::is_cloud_only(attributes);
    let head = if cloud_only {
        String::new()
    } else {
        read_head(path).unwrap_or_default()
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let is_tsv = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
    let (title, preview) = entry_text(&stem, kind, &head, is_tsv);
    LibraryEntry {
        path: path.to_path_buf(),
        kind,
        title,
        preview,
        modified,
        size,
        cloud_only,
    }
}

/// The first few KB as text (UTF-8, or UTF-16 with a BOM).
fn read_head(path: &Path) -> io::Result<String> {
    let mut f = file::open_shared(path)?;
    let mut buf = vec![0u8; HEAD_BYTES];
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    buf.truncate(n);
    Ok(decode_text(&buf))
}

/// Decode a file prefix: UTF-16 with BOM, else UTF-8 (lossy, BOM dropped).
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn documents_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Documents, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Case-insensitive path equality (ASCII folding, like the rest of whir).
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.as_os_str()
            .to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_owned()
    };
    norm(a).eq_ignore_ascii_case(&norm(b))
}

// --- naming (pure) -----------------------------------------------------------------

/// Longest file stem we generate from a title (characters).
const MAX_STEM_CHARS: usize = 80;

/// A title as a safe Windows file stem, or `None` if nothing usable is left.
pub fn sanitize_stem(title: &str) -> Option<String> {
    let mut out = String::with_capacity(title.len());
    let mut last_space = false;
    for c in title.chars() {
        let c = match c {
            ':' | '/' | '\\' | '|' => '-',
            '<' | '>' | '"' | '?' | '*' => continue,
            c if c.is_control() => ' ',
            c => c,
        };
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    let trimmed: String = out
        .trim_matches(|c: char| c == ' ' || c == '.')
        .chars()
        .take(MAX_STEM_CHARS)
        .collect();
    // Cutting may expose trailing spaces/dots again.
    let mut stem = trimmed.trim_end_matches([' ', '.']).to_owned();
    if stem.is_empty() {
        return None;
    }
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&stem)) {
        stem.push('_');
    }
    Some(stem)
}

/// "Untitled 2026-09-23 1412" / "Untitled table 2026-09-23 1412".
pub fn untitled_stem(prefix: &str, t: &LocalTime) -> String {
    format!(
        "{prefix} {:04}-{:02}-{:02} {:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    )
}

/// A stem made by [`untitled_stem`] (possibly with a " (n)" suffix).
pub fn is_untitled_stem(stem: &str) -> bool {
    let base = strip_counter(stem);
    let Some(rest) = base
        .strip_prefix("Untitled table ")
        .or_else(|| base.strip_prefix("Untitled "))
    else {
        return false;
    };
    let b = rest.as_bytes();
    b.len() == 15
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && b.iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10) || c.is_ascii_digit())
}

/// "Foo (3)" → "Foo".
fn strip_counter(stem: &str) -> &str {
    if let Some(open) = stem.rfind(" (")
        && stem.ends_with(')')
        && stem[open + 2..stem.len() - 1]
            .bytes()
            .all(|b| b.is_ascii_digit())
        && stem.len() > open + 3
    {
        return &stem[..open];
    }
    stem
}

/// `existing` already is `desired` or one of its " (n)" variants (case-insensitive).
pub fn stem_matches(existing: &str, desired: &str) -> bool {
    existing.eq_ignore_ascii_case(desired) || strip_counter(existing).eq_ignore_ascii_case(desired)
}

/// File names to try, in order: "stem.ext", "stem (2).ext" … "stem (99).ext",
/// then a timestamped name that practically can't collide.
pub fn candidates(stem: &str, ext: &str) -> Vec<String> {
    let mut v = Vec::with_capacity(100);
    v.push(format!("{stem}.{ext}"));
    for n in 2..=99 {
        v.push(format!("{stem} ({n}).{ext}"));
    }
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    v.push(format!("{stem} {nanos:x}.{ext}"));
    v
}

/// Give the (written, closed) temp file its final name in `dir`: the first free
/// candidate for `stem`. Never overwrites anything.
pub fn claim_name(tmp: &Path, dir: &Path, stem: &str, ext: &str) -> io::Result<PathBuf> {
    for name in candidates(stem, ext) {
        let target = dir.join(&name);
        if file::move_no_replace(tmp, &target)? {
            return Ok(target);
        }
    }
    Err(io::Error::other("no free file name"))
}

/// Create a new file in `dir` named after `stem`, written by `write`, without
/// ever replacing an existing file.
pub fn place_new(
    dir: &Path,
    stem: &str,
    ext: &str,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<(PathBuf, FileIdentity)> {
    let (tmp, mut f) = file::temp_in(dir)?;
    let written = (|| {
        write(&mut f)?;
        f.sync_all()
    })();
    drop(f);
    let result = written.and_then(|()| claim_name(&tmp, dir, stem, ext));
    match result {
        Ok(path) => {
            let id = FileIdentity::of_path(&path)?;
            Ok((path, id))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Rename `current` so its stem is `stem` (or a free " (n)" variant). Returns
/// the new path (unchanged if it already matches).
pub fn rename_to_stem(current: &Path, stem: &str) -> io::Result<PathBuf> {
    let dir = current.parent().unwrap_or(Path::new("."));
    let ext = current
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let cur_stem = current
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if cur_stem == stem || (stem_matches(&cur_stem, stem) && !cur_stem.eq_ignore_ascii_case(stem)) {
        return Ok(current.to_path_buf());
    }
    if cur_stem.eq_ignore_ascii_case(stem) {
        // Case-only change: the "existing" target is this very file.
        let target = dir.join(format!("{stem}.{ext}"));
        std::fs::rename(current, &target)?;
        return Ok(target);
    }
    for name in candidates(stem, &ext) {
        let target = dir.join(&name);
        if file::move_no_replace(current, &target)? {
            return Ok(target);
        }
    }
    Err(io::Error::other("no free file name"))
}

// --- titles & previews (pure) ---------------------------------------------------------

/// `# Title` (an ATX level-1 heading) → "Title".
pub fn parse_h1(line: &str) -> Option<String> {
    let line = line.trim_end_matches(['\r', '\n']);
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = line[indent..].strip_prefix('#')?;
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut text = rest.trim();
    // Optional closing sequence: " ##".
    let trimmed = text.trim_end_matches('#');
    if trimmed.len() != text.len() && (trimmed.is_empty() || trimmed.ends_with([' ', '\t'])) {
        text = trimmed.trim_end();
    }
    let text = text.replace("\\#", "#");
    Some(text)
}

/// Markdown line → plain text for previews ("- [ ] **Buy** milk" → "Buy milk").
pub fn strip_markdown(line: &str) -> String {
    let mut s = line.trim();
    loop {
        let before = s;
        s = s.trim_start_matches('#').trim_start();
        s = s.trim_start_matches('>').trim_start();
        for m in [
            "- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "- ", "* ", "+ ",
        ] {
            if let Some(r) = s.strip_prefix(m) {
                s = r.trim_start();
            }
        }
        if let Some(pos) = s.find(['.', ')'])
            && pos > 0
            && pos <= 9
            && s[..pos].bytes().all(|b| b.is_ascii_digit())
            && s[pos + 1..].starts_with(' ')
        {
            s = s[pos + 1..].trim_start();
        }
        if s == before {
            break;
        }
    }
    s.replace(['*', '_', '`'], "")
        .replace("~~", "")
        .trim()
        .to_owned()
}

fn clip(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Drawer title and one-line preview from a file's stem and first bytes.
pub fn entry_text(stem: &str, kind: EntryKind, head: &str, tsv: bool) -> (String, String) {
    match kind {
        EntryKind::Note => {
            // Code fences and rules say nothing about a note.
            let noise = |l: &&str| {
                l.is_empty()
                    || l.starts_with("```")
                    || l.starts_with("~~~")
                    || l.chars().all(|c| matches!(c, '-' | '*' | '_' | ' '))
            };
            let mut lines = head.lines().map(str::trim).filter(|l| !noise(l));
            let first = lines.next();
            let h1 = head.lines().next().and_then(parse_h1);
            let (title, rest): (String, Vec<&str>) = match (h1, first) {
                (Some(t), _) if !t.is_empty() => (t, lines.collect()),
                _ if !is_untitled_stem(stem) => {
                    (stem.to_owned(), first.into_iter().chain(lines).collect())
                }
                (_, Some(first)) => (strip_markdown(first), lines.collect()),
                (_, None) => ("New note".to_owned(), Vec::new()),
            };
            let preview = rest
                .iter()
                .map(|l| strip_markdown(l))
                .find(|l| !l.is_empty())
                .unwrap_or_default();
            (clip(&title, 120), clip(&preview, 160))
        }
        EntryKind::Table => {
            let title = if is_untitled_stem(stem) {
                "Untitled table".to_owned()
            } else {
                stem.to_owned()
            };
            let header = head.lines().next().unwrap_or("");
            let delim = if tsv {
                '\t'
            } else {
                [',', ';', '\t', '|']
                    .into_iter()
                    .max_by_key(|d| header.matches(*d).count())
                    .unwrap_or(',')
            };
            let preview = header
                .split(delim)
                .map(|f| f.trim().trim_matches('"'))
                .filter(|f| !f.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            (title, clip(&preview, 160))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_are_safe() {
        assert_eq!(
            sanitize_stem("Q3: plan/review").as_deref(),
            Some("Q3- plan-review")
        );
        assert_eq!(
            sanitize_stem("  what?  now*  ").as_deref(),
            Some("what now")
        );
        assert_eq!(sanitize_stem("...dots..."), Some("dots".to_owned()));
        assert_eq!(sanitize_stem("con").as_deref(), Some("con_"));
        assert_eq!(sanitize_stem("a\tb\nc").as_deref(), Some("a b c"));
        assert_eq!(sanitize_stem("?*<>"), None);
        assert_eq!(sanitize_stem("   "), None);
        let long = "é".repeat(100);
        let s = sanitize_stem(&long).unwrap();
        assert_eq!(s.chars().count(), MAX_STEM_CHARS);
        // Cutting at 80 chars never leaves a trailing space.
        let spaced = format!("{} end", "a".repeat(79));
        assert!(!sanitize_stem(&spaced).unwrap().ends_with(' '));
    }

    #[test]
    fn untitled_names() {
        let t = LocalTime {
            year: 2026,
            month: 9,
            day: 3,
            hour: 7,
            minute: 5,
            weekday: 4,
        };
        let s = untitled_stem("Untitled", &t);
        assert_eq!(s, "Untitled 2026-09-03 0705");
        assert!(is_untitled_stem(&s));
        assert!(is_untitled_stem("Untitled table 2026-09-03 0705 (2)"));
        assert!(!is_untitled_stem("Untitled"));
        assert!(!is_untitled_stem("Untitled ideas"));
        assert!(!is_untitled_stem("Meeting 2026-09-03 0705"));
    }

    #[test]
    fn counters() {
        assert!(stem_matches("Plan (2)", "plan"));
        assert!(stem_matches("Plan", "Plan"));
        assert!(!stem_matches("Plan (x)", "Plan"));
        assert!(!stem_matches("Planning", "Plan"));
        let c = candidates("a", "md");
        assert_eq!(c[0], "a.md");
        assert_eq!(c[1], "a (2).md");
        assert_eq!(c.len(), 100);
    }

    #[test]
    fn h1_parsing() {
        assert_eq!(parse_h1("# Title").as_deref(), Some("Title"));
        assert_eq!(parse_h1("#   Title  ##").as_deref(), Some("Title"));
        assert_eq!(parse_h1("# C# \\#1").as_deref(), Some("C# #1"));
        assert_eq!(parse_h1("#").as_deref(), Some(""));
        assert_eq!(parse_h1("#Title"), None);
        assert_eq!(parse_h1("## Sub"), None);
        assert_eq!(parse_h1("    # code"), None);
    }

    #[test]
    fn entry_titles_and_previews() {
        let (t, p) = entry_text(
            "whatever",
            EntryKind::Note,
            "# Standup\n\n- [ ] ship **it**\n",
            false,
        );
        assert_eq!((t.as_str(), p.as_str()), ("Standup", "ship it"));
        let (t, p) = entry_text(
            "Untitled 2026-09-03 0705",
            EntryKind::Note,
            "call Bob\nabout the thing",
            false,
        );
        assert_eq!((t.as_str(), p.as_str()), ("call Bob", "about the thing"));
        let (t, p) = entry_text("Ideas", EntryKind::Note, "first line\nsecond", false);
        assert_eq!((t.as_str(), p.as_str()), ("Ideas", "first line"));
        let (t, _) = entry_text("Untitled 2026-09-03 0705", EntryKind::Note, "", false);
        assert_eq!(t, "New note");
        let (t, p) = entry_text(
            "Untitled 2026-09-03 0705",
            EntryKind::Note,
            "Code sample\n```rust\nfn main() {}\n```\n",
            false,
        );
        assert_eq!((t.as_str(), p.as_str()), ("Code sample", "fn main() {}"));
        let (t, p) = entry_text(
            "Untitled table 2026-09-03 0705",
            EntryKind::Table,
            "Name,\"Qty\",Price\r\n1,2,3",
            false,
        );
        assert_eq!(
            (t.as_str(), p.as_str()),
            ("Untitled table", "Name · Qty · Price")
        );
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whir-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn placing_and_renaming() {
        use std::io::Write as _;
        let dir = test_dir("place");
        let (a, _) = place_new(&dir, "Plan", "md", |f| f.write_all(b"# Plan\n")).unwrap();
        let (b, _) = place_new(&dir, "Plan", "md", |f| f.write_all(b"# Plan\n")).unwrap();
        assert_eq!(a.file_name().unwrap(), "Plan.md");
        assert_eq!(b.file_name().unwrap(), "Plan (2).md");
        // "Plan (2)" already satisfies "Plan": no churn.
        assert_eq!(rename_to_stem(&b, "Plan").unwrap(), b);
        // Case-only rename.
        let c = rename_to_stem(&a, "plan").unwrap();
        assert_eq!(c.file_name().unwrap(), "plan.md");
        let d = rename_to_stem(&c, "Other").unwrap();
        assert_eq!(d.file_name().unwrap(), "Other.md");
        assert!(!c.exists() || paths_equal(&c, &d));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_lists_newest_first_and_skips_temps() {
        let dir = test_dir("scan");
        std::fs::write(dir.join("old.md"), "# Old\nbody").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("new.csv"), "a,b\n1,2").unwrap();
        std::fs::write(dir.join("~$whir-1-1.tmp"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let lib = Library::at(dir.clone(), Waker::noop());
        lib.scan();
        let deadline = Instant::now() + Duration::from_secs(5);
        let snap = loop {
            let s = lib.snapshot();
            if s.scanned || Instant::now() > deadline {
                break s;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let names: Vec<_> = snap
            .entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["new.csv", "old.md"]);
        assert_eq!(snap.entries[1].title, "Old");
        assert!(lib.contains(&dir.join("x.md")));
        assert!(!lib.contains(&dir.join("sub").join("x.md")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
