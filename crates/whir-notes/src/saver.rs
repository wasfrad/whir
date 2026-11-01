//! One background thread writes notes, so a slow disk (or OneDrive) never
//! stalls typing. Jobs run in order; each ends with a reply and a wake.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

use whir_core::file::{FileIdentity, WriteError, write_atomic};
use whir_core::library::{self, Library};
use whir_core::{Waker, file};

pub enum Target {
    /// A new library note: claim a free name for `stem` in `dir`.
    LibraryNew { dir: PathBuf, stem: String },
    /// An existing library note, renamed to `rename_to` first if it doesn't match.
    Library {
        path: PathBuf,
        rename_to: Option<String>,
    },
    /// Any other file, written in place.
    Path(PathBuf),
    /// A copy elsewhere (Save As of a library note); nothing is tracked.
    Export(PathBuf),
    /// The note was emptied: its file goes to the Recycle Bin, unless someone
    /// else has changed it.
    Emptied(PathBuf),
}

pub struct Job {
    pub target: Target,
    pub bytes: Vec<u8>,
    /// The file must still be this version (else: someone else edited it).
    pub expect: Option<FileIdentity>,
    pub cancel: Arc<AtomicBool>,
    /// Content hash the reply is about.
    pub hash: u64,
    pub reply: Sender<Reply>,
    pub waker: Waker,
    pub library: Arc<Library>,
}

#[derive(Debug)]
pub enum SaveErr {
    /// Changed on disk by another program since we loaded/wrote it.
    Conflict,
    Io(String),
}

#[derive(Debug)]
pub struct Reply {
    pub hash: u64,
    /// Where the note lives now (`None` after a delete; unchanged for exports).
    pub path: Option<PathBuf>,
    pub identity: Option<FileIdentity>,
    pub result: Result<(), SaveErr>,
    pub export: bool,
}

struct Saver {
    tx: Mutex<Sender<Job>>,
    pending: Mutex<usize>,
    idle: Condvar,
}

fn saver() -> &'static Saver {
    static SAVER: OnceLock<Saver> = OnceLock::new();
    SAVER.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        std::thread::Builder::new()
            .name("whir-notes-saver".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let waker = job.waker.clone();
                    let reply_tx = job.reply.clone();
                    let reply = run(job);
                    let _ = reply_tx.send(reply);
                    waker.wake();
                    let s = saver();
                    if let Ok(mut p) = s.pending.lock() {
                        *p = p.saturating_sub(1);
                        if *p == 0 {
                            s.idle.notify_all();
                        }
                    }
                }
            })
            .expect("spawn notes saver");
        Saver {
            tx: Mutex::new(tx),
            pending: Mutex::new(0),
            idle: Condvar::new(),
        }
    })
}

/// Queue a write.
pub fn submit(job: Job) {
    let _ = SAVER_STARTED.set(());
    let s = saver();
    if let Ok(mut p) = s.pending.lock() {
        *p += 1;
    }
    if let Ok(tx) = s.tx.lock()
        && let Err(e) = tx.send(job)
    {
        // The thread is gone (never happens in practice): write inline.
        let job = e.0;
        let reply_tx = job.reply.clone();
        let _ = reply_tx.send(run(job));
        if let Ok(mut p) = s.pending.lock() {
            *p = p.saturating_sub(1);
        }
    }
}

/// Wait until every queued write has finished, or `deadline`. True if idle.
pub fn wait_idle(deadline: Instant) -> bool {
    let Some(s) = SAVER_STARTED.get().map(|_| saver()) else {
        return true;
    };
    let Ok(mut p) = s.pending.lock() else {
        return false;
    };
    while *p > 0 {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        match s.idle.wait_timeout(p, deadline - now) {
            Ok((guard, _)) => p = guard,
            Err(_) => return false,
        }
    }
    true
}

/// Set once the first job is submitted, so `wait_idle` doesn't start a thread.
static SAVER_STARTED: OnceLock<()> = OnceLock::new();

/// Run a job on the calling thread (session end: no time for queues).
pub fn run_now(job: Job) -> Reply {
    run(job)
}

fn io(e: WriteError, path: &Path) -> SaveErr {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match e {
        WriteError::ChangedOnDisk => SaveErr::Conflict,
        e => SaveErr::Io(e.message(&name)),
    }
}

/// An emptied note's file goes to the Recycle Bin, so that clearing a note is
/// never the one keystroke with no way back. (Tests would fill the real bin.)
fn discard(path: &Path) -> Result<(), String> {
    if !path.exists() {
        Ok(())
    } else if cfg!(test) {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    } else {
        file::recycle(None, path)
    }
}

fn run(job: Job) -> Reply {
    let hash = job.hash;
    let fail = |path: Option<PathBuf>, e: SaveErr| Reply {
        hash,
        path,
        identity: None,
        result: Err(e),
        export: false,
    };
    if job.cancel.load(Ordering::Relaxed) {
        return fail(None, SaveErr::Io("cancelled".into()));
    }
    let changed_on_disk = |path: &Path| -> bool {
        match (job.expect, FileIdentity::of_path(path)) {
            (Some(expect), Ok(now)) => expect != now,
            // Deleted outside whir: writing it back is what the user wants.
            _ => false,
        }
    };
    let write = |path: &Path| write_atomic(path, |f| std::io::Write::write_all(f, &job.bytes));

    let (reply, touched) = match job.target {
        Target::LibraryNew { dir, stem } => {
            match library::place_new(&dir, &stem, "md", |f| {
                std::io::Write::write_all(f, &job.bytes)
            }) {
                Ok((path, id)) => (
                    Reply {
                        hash,
                        path: Some(path.clone()),
                        identity: Some(id),
                        result: Ok(()),
                        export: false,
                    },
                    vec![path],
                ),
                Err(e) => (
                    fail(None, SaveErr::Io(format!("Couldn't save the note: {e}"))),
                    vec![],
                ),
            }
        }
        Target::Library { path, rename_to } => {
            if changed_on_disk(&path) {
                return fail(Some(path), SaveErr::Conflict);
            }
            let mut current = path.clone();
            if let Some(stem) = rename_to
                && let Ok(p) = library::rename_to_stem(&path, &stem)
            {
                current = p;
            }
            match write(&current) {
                Ok(id) => (
                    Reply {
                        hash,
                        path: Some(current.clone()),
                        identity: Some(id),
                        result: Ok(()),
                        export: false,
                    },
                    vec![path, current],
                ),
                Err(e) => {
                    let err = io(e, &current);
                    (fail(Some(current.clone()), err), vec![path, current])
                }
            }
        }
        Target::Path(path) => {
            if changed_on_disk(&path) {
                return fail(Some(path), SaveErr::Conflict);
            }
            match write(&path) {
                Ok(id) => (
                    Reply {
                        hash,
                        path: Some(path.clone()),
                        identity: Some(id),
                        result: Ok(()),
                        export: false,
                    },
                    vec![path],
                ),
                Err(e) => {
                    let err = io(e, &path);
                    (fail(Some(path.clone()), err), vec![path])
                }
            }
        }
        Target::Export(path) => match write(&path) {
            Ok(_) => (
                Reply {
                    hash,
                    path: None,
                    identity: None,
                    result: Ok(()),
                    export: true,
                },
                vec![path],
            ),
            Err(e) => {
                let err = io(e, &path);
                (
                    Reply {
                        export: true,
                        ..fail(None, err)
                    },
                    vec![],
                )
            }
        },
        Target::Emptied(path) => {
            if changed_on_disk(&path) {
                return fail(Some(path), SaveErr::Conflict);
            }
            match discard(&path) {
                Ok(()) => (
                    Reply {
                        hash,
                        path: None,
                        identity: None,
                        result: Ok(()),
                        export: false,
                    },
                    vec![path],
                ),
                Err(e) => (fail(Some(path.clone()), SaveErr::Io(e)), vec![path]),
            }
        }
    };
    job.library.refresh(touched);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("whir-saver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn job(target: Target, bytes: &[u8], lib: &Arc<Library>) -> (Job, Receiver<Reply>) {
        let (tx, rx) = channel();
        (
            Job {
                target,
                bytes: bytes.to_vec(),
                expect: None,
                cancel: Arc::new(AtomicBool::new(false)),
                hash: 7,
                reply: tx,
                waker: Waker::noop(),
                library: lib.clone(),
            },
            rx,
        )
    }

    #[test]
    fn new_rename_conflict_delete() {
        let d = dir("flow");
        let lib = Library::at(d.clone(), Waker::noop());
        let (j, _) = job(
            Target::LibraryNew {
                dir: d.clone(),
                stem: "Plan".into(),
            },
            b"# Plan\n",
            &lib,
        );
        let r = run_now(j);
        let path = r.path.unwrap();
        assert_eq!(path.file_name().unwrap(), "Plan.md");
        let id = r.identity.unwrap();

        // Rename with the title, keeping content in sync.
        let (mut j, _) = job(
            Target::Library {
                path: path.clone(),
                rename_to: Some("Roadmap".into()),
            },
            b"# Roadmap\n",
            &lib,
        );
        j.expect = Some(id);
        let r = run_now(j);
        assert!(r.result.is_ok(), "{r:?}");
        let path2 = r.path.unwrap();
        assert_eq!(path2.file_name().unwrap(), "Roadmap.md");
        assert!(!path.exists());
        assert_eq!(std::fs::read(&path2).unwrap(), b"# Roadmap\n");

        // Someone else edits it: a stale identity is a conflict, nothing written.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path2, b"# Roadmap\n\ntheirs, longer\n").unwrap();
        let (mut j, _) = job(
            Target::Library {
                path: path2.clone(),
                rename_to: None,
            },
            b"mine",
            &lib,
        );
        j.expect = r.identity;
        let r = run_now(j);
        assert!(matches!(r.result, Err(SaveErr::Conflict)));
        assert_eq!(
            std::fs::read(&path2).unwrap(),
            b"# Roadmap\n\ntheirs, longer\n"
        );

        // An emptied note leaves no file, unless someone else changed it since.
        let (mut j, _) = job(Target::Emptied(path2.clone()), b"", &lib);
        j.expect = Some(id);
        assert!(matches!(run_now(j).result, Err(SaveErr::Conflict)));
        assert!(path2.exists());
        let (mut j, _) = job(Target::Emptied(path2.clone()), b"", &lib);
        j.expect = FileIdentity::of_path(&path2).ok();
        let r = run_now(j);
        assert!(r.result.is_ok(), "{r:?}");
        assert!(r.path.is_none());
        assert!(!path2.exists());
        // Already gone is as good.
        let (j, _) = job(Target::Emptied(path2.clone()), b"", &lib);
        assert!(run_now(j).result.is_ok());

        // Cancelled jobs write nothing.
        let (j, _) = job(
            Target::LibraryNew {
                dir: d.clone(),
                stem: "Never".into(),
            },
            b"x",
            &lib,
        );
        j.cancel.store(true, Ordering::Relaxed);
        assert!(run_now(j).result.is_err());
        assert!(!d.join("Never.md").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn queued_jobs_reply_and_drain() {
        let d = dir("queue");
        let lib = Library::at(d.clone(), Waker::noop());
        let (j, rx) = job(Target::Path(d.join("a.txt")), b"hello", &lib);
        submit(j);
        let r = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(r.result.is_ok());
        assert!(wait_idle(
            Instant::now() + std::time::Duration::from_secs(5)
        ));
        assert_eq!(std::fs::read(d.join("a.txt")).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&d);
    }
}
