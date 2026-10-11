//! Screenshots taken with the capture dock: `Documents\whir\Screenshots`.
//!
//! The folder is the truth: plain image files, listed newest first for the
//! drawer's Library. A capture's highlights live beside it, never in it, in a
//! small text file at `Screenshots\.whir\<file name>.marks`, so the image is
//! exactly what was captured and the highlights can be changed any time.
//!
//! One worker thread does all the reading and writing, in order.

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::Waker;
use crate::file::{self, write_atomic};
use crate::library::paths_equal;
use crate::marks::{self, MarkSet};

/// The captures folder's name inside the notes folder.
pub const FOLDER: &str = "Screenshots";
/// Where marks files are kept, inside the captures folder.
const MARKS_FOLDER: &str = ".whir";
const MARKS_EXT: &str = "marks";
/// What the Library lists (what the dock writes, and what a copy may be saved as).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff"];
/// Marks whose image has been gone this long are swept away, as are our own
/// temp files (crash leftovers). The wait covers a sync client delivering a
/// marks file before its image.
const STALE: Duration = Duration::from_secs(60 * 60);

/// Where captures are kept: `WHIR_CAPTURES_DIR`, else `Screenshots` inside the
/// notes folder (`WHIR_NOTES_DIR`, else `Documents\whir`). `None` if the
/// Documents folder can't be resolved.
pub fn resolve_dir() -> Option<PathBuf> {
    resolve(
        std::env::var_os("WHIR_CAPTURES_DIR").map(PathBuf::from),
        std::env::var_os("WHIR_NOTES_DIR").map(PathBuf::from),
        crate::library::documents_dir,
    )
}

fn resolve(
    captures: Option<PathBuf>,
    notes: Option<PathBuf>,
    documents: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    captures.or_else(|| {
        notes
            .or_else(|| documents().map(|d| d.join("whir")))
            .map(|n| n.join(FOLDER))
    })
}

/// The marks file for `image`.
pub fn marks_path(image: &Path) -> Option<PathBuf> {
    let mut name = image.file_name()?.to_owned();
    name.push(".");
    name.push(MARKS_EXT);
    Some(image.parent()?.join(MARKS_FOLDER).join(name))
}

#[derive(Clone, Debug)]
pub struct CaptureEntry {
    pub path: PathBuf,
    /// The file's name without its extension.
    pub name: String,
    /// When it was captured.
    pub created: SystemTime,
    pub modified: SystemTime,
    pub size: u64,
    /// A OneDrive placeholder: never read (that would download it).
    pub cloud_only: bool,
    /// Its highlights, for the thumbnail.
    pub marks: Option<Arc<MarkSet>>,
    /// The marks file's time and size when `marks` was read.
    marks_stamp: Option<(Option<SystemTime>, u64)>,
}

#[derive(Clone, Default)]
pub struct CapturesSnapshot {
    /// Bumped on every change.
    pub generation: u64,
    /// Newest capture first.
    pub entries: Arc<[CaptureEntry]>,
    pub scanning: bool,
    /// At least one scan has completed.
    pub scanned: bool,
    pub error: Option<String>,
    /// When the last full scan finished.
    pub scanned_at: Option<Instant>,
}

type Reply = Box<dyn FnOnce(Result<(), String>) + Send>;

enum Work {
    Scan,
    Refresh(Vec<PathBuf>),
    SaveMarks(PathBuf, MarkSet, Reply),
    Forget(PathBuf),
}

type Shared = Arc<Mutex<CapturesSnapshot>>;

pub struct Captures {
    waker: Waker,
    override_dir: Option<PathBuf>,
    dir: OnceLock<Option<PathBuf>>,
    shared: Shared,
    worker: Mutex<Option<Sender<Work>>>,
}

impl Captures {
    /// No I/O: the folder is resolved on first use.
    pub fn new(waker: Waker) -> Arc<Self> {
        Self::with_override(None, waker)
    }

    /// A store rooted at `dir` (tests).
    pub fn at(dir: PathBuf, waker: Waker) -> Arc<Self> {
        Self::with_override(Some(dir), waker)
    }

    fn with_override(override_dir: Option<PathBuf>, waker: Waker) -> Arc<Self> {
        Arc::new(Self {
            waker,
            override_dir,
            dir: OnceLock::new(),
            shared: Arc::new(Mutex::new(CapturesSnapshot::default())),
            worker: Mutex::new(None),
        })
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir
            .get_or_init(|| self.override_dir.clone().or_else(resolve_dir))
            .as_deref()
    }

    /// `path` is directly inside the captures folder.
    pub fn contains(&self, path: &Path) -> bool {
        match (self.dir(), path.parent()) {
            (Some(dir), Some(parent)) => paths_equal(dir, parent),
            _ => false,
        }
    }

    pub fn snapshot(&self) -> CapturesSnapshot {
        self.shared.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Rescan the whole folder in the background.
    pub fn scan(&self) {
        if let Ok(mut s) = self.shared.lock() {
            s.scanning = true;
        }
        self.send(Work::Scan);
    }

    /// Re-read (or drop) these files in the background after one was written
    /// or deleted. Does nothing before the first scan (which will see them
    /// anyway): opening a capture must not start the store.
    pub fn refresh(&self, paths: Vec<PathBuf>) {
        let scanned = self.shared.lock().is_ok_and(|s| s.scanned || s.scanning);
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| self.contains(p)).collect();
        if scanned && !paths.is_empty() {
            self.send(Work::Refresh(paths));
        }
    }

    /// The marks saved for `image`, read here and now (call from a worker).
    pub fn load_marks(&self, image: &Path) -> Option<MarkSet> {
        if !self.contains(image) {
            return None;
        }
        read_marks(&marks_path(image)?)
    }

    /// Save `set` as `image`'s marks in the background (an empty set removes
    /// the file). `done` is called on the worker with the result.
    pub fn save_marks(
        &self,
        image: PathBuf,
        set: MarkSet,
        done: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        if self.contains(&image) {
            self.send(Work::SaveMarks(image, set, Box::new(done)));
        } else {
            done(Ok(()));
        }
    }

    /// The image was deleted: its marks go too.
    pub fn forget(&self, image: PathBuf) {
        if self.contains(&image) {
            self.send(Work::Forget(image));
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
                .name("whir-captures".into())
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

fn read_marks(path: &Path) -> Option<MarkSet> {
    let meta = std::fs::metadata(path).ok()?;
    if file::is_cloud_only(meta.file_attributes()) {
        return None;
    }
    marks::parse(&std::fs::read_to_string(path).ok()?)
}

/// Write `set` for `image`, or remove the file when there is nothing in it or
/// the image itself is gone (a tab still holding marks must not bring a
/// deleted capture's file back).
fn write_marks(image: &Path, set: &MarkSet) -> Result<(), String> {
    let Some(path) = marks_path(image) else {
        return Ok(());
    };
    let nothing = set.marks.is_empty() && set.other.is_empty();
    if nothing || !image.exists() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    let name = "the highlights";
    let dir = path.parent().unwrap_or(Path::new("."));
    if !dir.is_dir() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        file::hide(dir);
    }
    let text = marks::serialize(set);
    write_atomic(&path, |f| f.write_all(text.as_bytes()))
        .map(|_| ())
        .map_err(|e| e.message(name))
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
            Work::SaveMarks(image, set, done) => {
                let result = write_marks(&image, &set);
                done(result);
                self.refresh(&[image]);
            }
            Work::Forget(image) => {
                if let Some(marks) = marks_path(&image) {
                    let _ = std::fs::remove_file(marks);
                }
                self.refresh(&[image]);
            }
        }
        self.waker.wake();
    }

    fn previous(&self) -> Arc<[CaptureEntry]> {
        self.shared
            .lock()
            .map(|s| s.entries.clone())
            .unwrap_or_default()
    }

    fn publish(&self, mut entries: Vec<CaptureEntry>, full_scan: bool, error: Option<String>) {
        entries.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.name.cmp(&a.name)));
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
            let error = Some("The Documents folder isn't available.".into());
            self.publish(Vec::new(), true, error);
            return;
        };
        let read = match std::fs::read_dir(dir) {
            Ok(r) => r,
            // Not created yet: nothing captured so far.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.publish(Vec::new(), true, None);
                return;
            }
            Err(e) => {
                self.publish(Vec::new(), true, Some(e.to_string()));
                return;
            }
        };
        let previous: HashMap<PathBuf, CaptureEntry> = self
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
                if is_stale(&meta) {
                    let _ = std::fs::remove_file(&path);
                }
                continue;
            }
            if !meta.is_file() || name.starts_with('.') || !is_image(&path) {
                continue;
            }
            entries.push(read_entry(&path, &meta, previous.get(&path)));
        }
        sweep_orphans(dir, &entries);
        self.publish(entries, true, None);
    }

    fn refresh(&self, paths: &[PathBuf]) {
        let mut entries: Vec<CaptureEntry> = self.previous().to_vec();
        for path in paths {
            entries.retain(|e| !paths_equal(&e.path, path));
            if is_image(path)
                && let Ok(meta) = std::fs::metadata(path)
                && meta.is_file()
            {
                entries.push(read_entry(path, &meta, None));
            }
        }
        self.publish(entries, false, None);
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn is_stale(meta: &std::fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age > STALE)
}

/// The image's own data needs no reading; its marks file is read again only
/// if it changed since `previous`.
fn read_entry(
    path: &Path,
    meta: &std::fs::Metadata,
    previous: Option<&CaptureEntry>,
) -> CaptureEntry {
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let marks_stamp = marks_path(path)
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| (m.modified().ok(), m.len()));
    let marks = match (previous, marks_stamp) {
        (_, None) => None,
        (Some(p), stamp) if p.marks_stamp == stamp => p.marks.clone(),
        _ => marks_path(path)
            .and_then(|p| read_marks(&p))
            .filter(|set| !set.marks.is_empty())
            .map(Arc::new),
    };
    CaptureEntry {
        path: path.to_owned(),
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        created: meta.created().unwrap_or(modified),
        modified,
        size: meta.len(),
        cloud_only: file::is_cloud_only(meta.file_attributes()),
        marks,
        marks_stamp,
    }
}

/// Remove marks files whose image is no longer listed, once they are old.
fn sweep_orphans(dir: &Path, entries: &[CaptureEntry]) {
    let Ok(read) = std::fs::read_dir(dir.join(MARKS_FOLDER)) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let Some(image) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(&format!(".{MARKS_EXT}")))
        else {
            continue;
        };
        let kept = entries.iter().any(|e| {
            e.path
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case(image))
        });
        if !kept && entry.metadata().is_ok_and(|m| is_stale(&m)) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::marks::{Ink, Mark};

    #[test]
    fn override_then_notes_folder_then_documents() {
        let p = |s: &str| Some(PathBuf::from(s));
        let docs = || p(r"C:\Users\me\Documents");
        assert_eq!(
            resolve(p(r"D:\shots"), p(r"D:\notes"), docs),
            p(r"D:\shots")
        );
        assert_eq!(
            resolve(None, p(r"D:\notes"), docs),
            p(r"D:\notes\Screenshots")
        );
        assert_eq!(
            resolve(None, None, docs),
            p(r"C:\Users\me\Documents\whir\Screenshots")
        );
        assert_eq!(resolve(None, None, || None), None);
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whir-captures-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait(store: &Captures, done: impl Fn(&CapturesSnapshot) -> bool) -> CapturesSnapshot {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = store.snapshot();
            if done(&s) {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn scanned(store: &Captures) -> CapturesSnapshot {
        let before = store.snapshot().scanned_at;
        store.scan();
        wait(store, |s| {
            s.scanned && !s.scanning && s.scanned_at != before
        })
    }

    fn set(n: u32) -> MarkSet {
        MarkSet {
            size: [800, 600],
            marks: (0..n)
                .map(|i| Mark {
                    x: i * 10,
                    y: 5,
                    w: 100,
                    h: 20,
                    ink: Ink::Yellow,
                })
                .collect(),
            other: Vec::new(),
        }
    }

    /// Save and wait for the write.
    fn save(store: &Captures, image: &Path, set: MarkSet) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        store.save_marks(image.to_owned(), set, move |r| {
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    #[test]
    fn the_scan_lists_images_newest_first_and_nothing_else() {
        let dir = test_dir("scan");
        for name in ["a.png", "b.PNG", "c.jpg"] {
            std::fs::write(dir.join(name), b"x").unwrap();
            // Creation times a file system can tell apart.
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(dir.join("notes.md"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        std::fs::write(dir.join(format!("{}1.png", file::TEMP_PREFIX)), b"x").unwrap();
        std::fs::create_dir(dir.join("folder.png")).unwrap();
        std::fs::create_dir(dir.join(MARKS_FOLDER)).unwrap();

        let store = Captures::at(dir.clone(), Waker::noop());
        assert!(store.contains(&dir.join("a.png")));
        assert!(!store.contains(&dir.join(MARKS_FOLDER).join("a.png.marks")));
        let s = scanned(&store);
        let names: Vec<&str> = s.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["c", "b", "a"]);
        assert!(s.entries.iter().all(|e| e.marks.is_none() && !e.cloud_only));
        assert_eq!(s.error, None);

        // A folder that doesn't exist yet is just empty.
        let none = Captures::at(dir.join("missing"), Waker::noop());
        assert!(scanned(&none).entries.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn marks_are_kept_beside_the_image_and_removed_when_empty() {
        let dir = test_dir("marks");
        let image = dir.join("shot.png");
        std::fs::write(&image, b"png").unwrap();
        let store = Captures::at(dir.clone(), Waker::noop());
        let sidecar = marks_path(&image).unwrap();
        assert_eq!(sidecar, dir.join(".whir").join("shot.png.marks"));
        assert_eq!(store.load_marks(&image), None);

        save(&store, &image, set(2)).unwrap();
        assert_eq!(store.load_marks(&image), Some(set(2)));
        assert_eq!(
            std::fs::read(&image).unwrap(),
            b"png",
            "the image is never touched"
        );
        // The Library shows them.
        let s = scanned(&store);
        assert_eq!(s.entries[0].marks.as_deref(), Some(&set(2)));

        save(&store, &image, set(1)).unwrap();
        assert_eq!(store.load_marks(&image), Some(set(1)));
        let s = wait(&store, |s| {
            s.entries.first().and_then(|e| e.marks.clone()).as_deref() == Some(&set(1))
        });
        assert_eq!(s.entries.len(), 1);

        save(&store, &image, set(0)).unwrap();
        assert!(!sidecar.exists());
        assert_eq!(store.load_marks(&image), None);
        // Removing what isn't there is fine too.
        save(&store, &image, set(0)).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_is_written_outside_the_folder_or_for_a_deleted_image() {
        let dir = test_dir("outside");
        let elsewhere = test_dir("outside-elsewhere");
        let store = Captures::at(dir.clone(), Waker::noop());

        let other = elsewhere.join("photo.png");
        std::fs::write(&other, b"x").unwrap();
        save(&store, &other, set(1)).unwrap();
        assert!(!elsewhere.join(MARKS_FOLDER).exists());
        assert_eq!(store.load_marks(&other), None);

        // Deleted while a tab still had marks to save: nothing comes back.
        let gone = dir.join("gone.png");
        std::fs::write(&gone, b"x").unwrap();
        save(&store, &gone, set(1)).unwrap();
        std::fs::remove_file(&gone).unwrap();
        save(&store, &gone, set(2)).unwrap();
        assert!(!marks_path(&gone).unwrap().exists());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(elsewhere);
    }

    #[test]
    fn forgetting_an_image_removes_its_marks_and_its_entry() {
        let dir = test_dir("forget");
        let image = dir.join("shot.png");
        std::fs::write(&image, b"x").unwrap();
        let store = Captures::at(dir.clone(), Waker::noop());
        save(&store, &image, set(1)).unwrap();
        assert_eq!(scanned(&store).entries.len(), 1);

        std::fs::remove_file(&image).unwrap();
        store.forget(image.clone());
        wait(&store, |s| s.entries.is_empty());
        assert!(!marks_path(&image).unwrap().exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refresh_waits_for_the_first_scan() {
        let dir = test_dir("refresh");
        let image = dir.join("shot.png");
        std::fs::write(&image, b"x").unwrap();
        let store = Captures::at(dir.clone(), Waker::noop());
        // Opening a capture mustn't start the store.
        store.refresh(vec![image.clone()]);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(store.snapshot().generation, 0);

        assert_eq!(scanned(&store).entries.len(), 1);
        let second = dir.join("second.png");
        std::fs::write(&second, b"x").unwrap();
        store.refresh(vec![second]);
        wait(&store, |s| s.entries.len() == 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn orphaned_marks_are_swept_only_once_they_are_old() {
        let dir = test_dir("orphans");
        let image = dir.join("kept.png");
        std::fs::write(&image, b"x").unwrap();
        let store = Captures::at(dir.clone(), Waker::noop());
        save(&store, &image, set(1)).unwrap();
        let marks = dir.join(MARKS_FOLDER);
        let fresh = marks.join("recent.png.marks");
        let old = marks.join("long-gone.png.marks");
        std::fs::write(&fresh, marks::serialize(&set(1))).unwrap();
        std::fs::write(&old, marks::serialize(&set(1))).unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();

        scanned(&store);
        assert!(marks_path(&image).unwrap().exists(), "its image is there");
        assert!(fresh.exists(), "its image may still be on the way");
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
